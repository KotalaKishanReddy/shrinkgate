//! Shrink pass for the OpenAI Responses API (`/v1/responses`) — Codex and
//! Muse-style agents. Input items replayed every turn get stubbed the same
//! way; reasoning items and function definitions are never touched.

use serde_json::Value;
use std::collections::HashMap;

use crate::config::Config;
use crate::shrink::{is_stub_text, stub_text, ShrinkStat};
use crate::store::{fingerprint, Seen, Session};

pub fn session_key(body: &Value) -> String {
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
    let instructions = body
        .get("instructions")
        .and_then(Value::as_str)
        .unwrap_or("");
    let user = body
        .get("user")
        .map(|u| serde_json::to_string(u).unwrap_or_default())
        .unwrap_or_default();
    fingerprint(&[
        b"openai/responses",
        model.as_bytes(),
        instructions.as_bytes(),
        user.as_bytes(),
    ])
}

struct Cand {
    ii: usize,
    /// Content part index for message items.
    pi: Option<usize>,
    fp: String,
    bytes: usize,
}

pub fn transform(body: &mut Value, sess: &mut Session, cfg: &Config) -> ShrinkStat {
    let mut stat = ShrinkStat::default();
    if !cfg.shrink {
        return stat;
    }
    let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return stat;
    };
    let n = input.len();
    if n == 0 {
        return stat;
    }
    let keep = cfg.keep_last_messages;

    let mut cands: Vec<Cand> = Vec::new();
    for (ii, item) in input.iter().enumerate() {
        let itype = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message");
        match itype {
            "message" => {
                let role = item.get("role").and_then(Value::as_str).unwrap_or("");
                let assistant_ok = cfg.allow_assistant_text && role == "assistant";
                if role != "user" && !assistant_ok {
                    continue;
                }
                let Some(parts) = item.get("content").and_then(Value::as_array) else {
                    continue;
                };
                // Mixed media items are skipped entirely.
                if parts.iter().any(|p| {
                    matches!(
                        p.get("type").and_then(Value::as_str),
                        Some("input_image")
                            | Some("input_file")
                            | Some("input_video")
                            | Some("image_url")
                            | Some("file")
                    )
                }) {
                    continue;
                }
                for (pi, part) in parts.iter().enumerate() {
                    let Some(s) = part.get("text").and_then(Value::as_str) else {
                        continue;
                    };
                    if is_stub_text(s) || s.len() < cfg.min_block_bytes {
                        continue;
                    }
                    cands.push(Cand {
                        ii,
                        pi: Some(pi),
                        fp: fingerprint(&[format!("r\x00{s}").as_bytes()]),
                        bytes: s.len(),
                    });
                }
            }
            "function_call_output" => {
                let Some(s) = item.get("output").and_then(Value::as_str) else {
                    continue;
                };
                if is_stub_text(s) || s.len() < cfg.min_block_bytes {
                    continue;
                }
                cands.push(Cand {
                    ii,
                    pi: None,
                    fp: fingerprint(&[format!("r\x00{s}").as_bytes()]),
                    bytes: s.len(),
                });
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
        let in_window = keep > 0 && c.ii >= n.saturating_sub(keep);
        let dup = count.get(&c.fp).copied().unwrap_or(0) > 1 && !is_last && !in_window;
        if dup {
            let stub = stub_text(seen_turn, c.bytes);
            let stub_len = stub.len();
            let replaced = if let Some(pi) = c.pi {
                body.pointer_mut(&format!("/input/{}/content/{}/text", c.ii, pi))
                    .map(|t| *t = Value::String(stub.clone()))
                    .is_some()
            } else {
                body.pointer_mut(&format!("/input/{}/output", c.ii))
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
    fn function_call_outputs_stubbed() {
        let (c, mut s) = (cfg(), Session::default());
        let mut body = json!({"model": "m", "input": [
            {"type": "function_call", "call_id": "c1", "name": "read", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "c1", "output": long()},
            {"type": "function_call_output", "call_id": "c1", "output": long()},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "tail"}]},
        ]});
        let stat = transform(&mut body, &mut s, &c);
        assert_eq!(stat.blocks_stubbed, 1);
        assert!(body["input"][1]["output"]
            .as_str()
            .unwrap()
            .starts_with("[shrinkgate]"));
        assert!(body["input"][2]["output"]
            .as_str()
            .unwrap()
            .contains("x-very-long"));
    }

    #[test]
    fn message_input_parts_stubbed() {
        let (c, mut s) = (cfg(), Session::default());
        let mut body = json!({"model": "m", "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": long()}]},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": long()}]},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "tail"}]},
        ]});
        let stat = transform(&mut body, &mut s, &c);
        assert_eq!(stat.blocks_stubbed, 1);
        assert!(body["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("[shrinkgate]"));
    }

    #[test]
    fn image_items_skipped_entirely() {
        let (c, mut s) = (cfg(), Session::default());
        let mut body = json!({"model": "m", "input": [
            {"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": long()},
                {"type": "input_image", "image_url": "data:image/png;base64,AAAA"}
            ]},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": long()}]},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": long()}]},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "tail"}]},
        ]});
        let stat = transform(&mut body, &mut s, &c);
        // Mixed-media item skipped; the two pure-text copies dedupe to 1 stub.
        assert_eq!(stat.blocks_stubbed, 1);
        assert!(body["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("x-very-long"));
    }

    #[test]
    fn reasoning_items_untouched() {
        let (c, mut s) = (cfg(), Session::default());
        let mut body = json!({"model": "m", "input": [
            {"type": "reasoning", "summary": [{"type": "summary_text", "text": long()}]},
            {"type": "reasoning", "summary": [{"type": "summary_text", "text": long()}]},
        ]});
        assert_eq!(transform(&mut body, &mut s, &c).blocks_stubbed, 0);
        assert!(body["input"][0]["summary"][0]["text"]
            .as_str()
            .unwrap()
            .contains("x-very-long"));
    }

    #[test]
    fn instructions_part_of_session_key() {
        let a = json!({"model": "m", "instructions": "i1"});
        let b = json!({"model": "m", "instructions": "i2"});
        assert_ne!(session_key(&a), session_key(&b));
    }
}
