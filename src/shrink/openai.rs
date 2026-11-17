//! Shrink pass for the OpenAI Chat Completions API (`/v1/chat/completions`).

use serde_json::Value;
use std::collections::HashMap;

use crate::config::Config;
use crate::shrink::{is_stub_text, stub_text, ShrinkStat};
use crate::store::{fingerprint, Seen, Session};

pub fn session_key(body: &Value) -> String {
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
    let user = body.get("user").and_then(Value::as_str).unwrap_or("");
    let system = body
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|msgs| {
            msgs.iter()
                .find(|m| {
                    matches!(
                        m.get("role").and_then(Value::as_str),
                        Some("system") | Some("developer")
                    )
                })
                .map(|m| serde_json::to_string(m).unwrap_or_default())
        })
        .unwrap_or_default();
    fingerprint(&[
        b"openai/chat",
        model.as_bytes(),
        user.as_bytes(),
        system.as_bytes(),
    ])
}

struct Cand {
    mi: usize,
    /// Index of the content part within `content` (array form), if array form.
    pi: Option<usize>,
    fp: String,
    bytes: usize,
}

pub fn transform(body: &mut Value, sess: &mut Session, cfg: &Config) -> ShrinkStat {
    let mut stat = ShrinkStat::default();
    if !cfg.shrink {
        return stat;
    }
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return stat;
    };
    let n = messages.len();
    if n == 0 {
        return stat;
    }
    let keep = cfg.keep_last_messages;

    let mut cands: Vec<Cand> = Vec::new();
    for (mi, msg) in messages.iter().enumerate() {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        match role {
            "system" | "developer" => continue,
            "tool" | "function" | "user" => {}
            "assistant" if cfg.allow_assistant_text => {}
            _ => continue,
        }
        match msg.get("content") {
            Some(Value::String(s)) => {
                if !is_stub_text(s) && s.len() >= cfg.min_block_bytes {
                    cands.push(Cand {
                        mi,
                        pi: None,
                        fp: fingerprint(&[format!("c\x00{s}").as_bytes()]),
                        bytes: s.len(),
                    });
                }
            }
            Some(Value::Array(parts)) => {
                for (pi, part) in parts.iter().enumerate() {
                    let Some(s) = part.get("text").and_then(Value::as_str) else {
                        continue;
                    };
                    if is_stub_text(s) || s.len() < cfg.min_block_bytes {
                        continue;
                    }
                    cands.push(Cand {
                        mi,
                        pi: Some(pi),
                        fp: fingerprint(&[format!("c\x00{s}").as_bytes()]),
                        bytes: s.len(),
                    });
                }
            }
            _ => {}
        }
    }
    if cands.is_empty() {
        return stat;
    }

    let mut count: HashMap<String, usize> = HashMap::new();
    let mut last: HashMap<String, usize> = HashMap::new();
    for (ci, c) in cands.iter().enumerate() {
        *count.entry(c.fp.clone()).or_default() += 1;
        last.insert(c.fp.clone(), ci);
    }

    for (ci, c) in cands.iter().enumerate() {
        let seen_turn = sess
            .seen
            .entry(c.fp.clone())
            .or_insert(Seen { turn: sess.turn })
            .turn;
        stat.est_orig_bytes += c.bytes;
        let is_last = last.get(&c.fp) == Some(&ci);
        let in_window = keep > 0 && c.mi >= n.saturating_sub(keep);
        let dup = count.get(&c.fp).copied().unwrap_or(0) > 1 && !is_last && !in_window;
        if dup {
            let stub = stub_text(seen_turn, c.bytes);
            let stub_len = stub.len();
            let replaced = if let Some(pi) = c.pi {
                body.pointer_mut(&format!("/messages/{}/content/{}/text", c.mi, pi))
                    .map(|t| *t = Value::String(stub.clone()))
                    .is_some()
            } else {
                body.pointer_mut(&format!("/messages/{}/content", c.mi))
                    .map(|t| *t = Value::String(stub.clone()))
                    .is_some()
            };
            if replaced {
                stat.saved_bytes += c.bytes.saturating_sub(stub_len);
                stat.blocks_stubbed += 1;
                stat.est_sent_bytes += stub_len;
            } else {
                stat.est_sent_bytes += c.bytes;
            }
        } else {
            stat.est_sent_bytes += c.bytes;
        }
    }
    sess.turn += 1;
    stat
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use serde_json::json;

    fn cfg() -> Config {
        Config::default()
    }

    fn long() -> String {
        "x-very-long-payload-".repeat(40)
    }

    #[test]
    fn system_and_developer_are_never_candidates() {
        let (c, mut s) = (cfg(), Session::default());
        let mut body = json!({"model": "m", "messages": [
            {"role": "system", "content": long()},
            {"role": "developer", "content": long()},
            {"role": "user", "content": long()},
            {"role": "user", "content": long()},
            {"role": "user", "content": "tail"},
        ]});
        let stat = transform(&mut body, &mut s, &c);
        // Only the two user copies count: earlier stubbed, system rows intact.
        assert_eq!(stat.blocks_stubbed, 1);
        assert!(body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("x-very-long"));
        assert!(body["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("x-very-long"));
        assert!(body["messages"][2]["content"]
            .as_str()
            .unwrap()
            .starts_with("[shrinkgate]"));
    }

    #[test]
    fn array_parts_stub_with_part_pointer() {
        let (c, mut s) = (cfg(), Session::default());
        let mut body = json!({"model": "m", "messages": [
            {"role": "user", "content": [{"type": "text", "text": long()}]},
            {"role": "user", "content": [{"type": "text", "text": long()}]},
            {"role": "user", "content": [{"type": "text", "text": "tail"}]},
        ]});
        let stat = transform(&mut body, &mut s, &c);
        assert_eq!(stat.blocks_stubbed, 1);
        assert!(body["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("[shrinkgate]"));
        assert!(body["messages"][1]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("x-very-long"));
    }

    #[test]
    fn assistant_text_untouched_by_default() {
        let (c, mut s) = (cfg(), Session::default());
        let mut body = json!({"model": "m", "messages": [
            {"role": "assistant", "content": long()},
            {"role": "assistant", "content": long()},
        ]});
        assert_eq!(transform(&mut body, &mut s, &c).blocks_stubbed, 0);
    }

    #[test]
    fn session_key_diverges_on_system() {
        let a = json!({"model": "m", "messages": [{"role": "system", "content": "s1"}]});
        let b = json!({"model": "m", "messages": [{"role": "system", "content": "s2"}]});
        assert_ne!(session_key(&a), session_key(&b));
        assert_eq!(session_key(&a), session_key(&a));
    }
}
