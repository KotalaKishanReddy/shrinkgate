//! Shrink pass for the Anthropic Messages API (`/v1/messages`).

use serde_json::Value;
use std::collections::HashMap;

use crate::config::Config;
use crate::shrink::{is_stub_text, stub_text, ShrinkStat};
use crate::store::fingerprint;

/// Session key: model + system prompt + Claude Code's per-session metadata id.
/// Requests sharing this key share one dedup store.
pub fn session_key(body: &Value) -> String {
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
    let system = body
        .get("system")
        .map(|s| serde_json::to_string(s).unwrap_or_default())
        .unwrap_or_default();
    let meta = body
        .pointer("/metadata/user_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    fingerprint(&[
        b"anthropic/messages",
        model.as_bytes(),
        system.as_bytes(),
        meta.as_bytes(),
    ])
}

struct Cand {
    mi: usize,
    bi: usize,
    fp: String,
    bytes: usize,
    tool_result: bool,
}

/// Rewrites `body` in place. Deterministic given (body, session state).
pub fn transform(body: &mut Value, cfg: &Config) -> ShrinkStat {
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

    // Pass 1: collect candidates (owned data, no borrows into `messages`).
    let mut cands: Vec<Cand> = Vec::new();
    for (mi, msg) in messages.iter().enumerate() {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        let assistant_ok = cfg.allow_assistant_text && role == "assistant";
        if role != "user" && !assistant_ok {
            continue;
        }
        let Some(blocks) = msg.get("content").and_then(Value::as_array) else {
            continue;
        };
        for (bi, block) in blocks.iter().enumerate() {
            let btype = block.get("type").and_then(Value::as_str).unwrap_or("");
            let (fp, bytes, tool_result) = if btype == "tool_result" {
                match block.get("content") {
                    Some(Value::String(s)) => {
                        if is_stub_text(s) {
                            continue;
                        }
                        (fingerprint(&[b"tool/text", s.as_bytes()]), s.len(), true)
                    }
                    Some(Value::Array(parts)) => {
                        // Mixed-media tool results are never stubbed; pure-text ones are.
                        if parts
                            .iter()
                            .any(|p| p.get("type").and_then(Value::as_str) != Some("text"))
                        {
                            continue;
                        }
                        if parts.iter().any(|p| {
                            p.get("text")
                                .and_then(Value::as_str)
                                .map(is_stub_text)
                                .unwrap_or(false)
                        }) {
                            continue;
                        }
                        let mut texts: Vec<&[u8]> = vec![b"tool/parts"];
                        let mut bytes = 0;
                        for part in parts {
                            let Some(text) = part.get("text").and_then(Value::as_str) else {
                                continue;
                            };
                            bytes += text.len();
                            texts.push(text.as_bytes());
                        }
                        (fingerprint(&texts), bytes, true)
                    }
                    _ => continue,
                }
            } else if btype == "text" && (cfg.allow_user_text || assistant_ok) {
                let Some(s) = block.get("text").and_then(Value::as_str) else {
                    continue;
                };
                if is_stub_text(s) {
                    continue;
                }
                (fingerprint(&[b"text", s.as_bytes()]), s.len(), false)
            } else {
                continue;
            };
            if bytes < cfg.min_block_bytes {
                continue;
            }
            cands.push(Cand {
                mi,
                bi,
                fp,
                bytes,
                tool_result,
            });
        }
    }
    if cands.is_empty() {
        return stat;
    }

    // Occurrence map: fingerprint -> (count, index of last occurrence).
    let mut last: HashMap<&str, usize> = HashMap::new();
    for (ci, c) in cands.iter().enumerate() {
        last.insert(&c.fp, ci);
    }

    // Pass 2: register first-seen, stub every non-latest duplicate outside the
    // keep-last window.
    for (ci, c) in cands.iter().enumerate() {
        stat.est_orig_bytes += c.bytes;
        let is_last = last.get(c.fp.as_str()) == Some(&ci);
        let in_window = keep > 0 && c.mi >= n.saturating_sub(keep);
        let stub = stub_text(&c.fp, c.bytes);
        let dup = !is_last && !in_window && stub.len() < c.bytes;
        if dup {
            let stub_len = stub.len();
            let block_path = format!("/messages/{}/content/{}", c.mi, c.bi);
            let replaced = if c.tool_result {
                body.pointer_mut(&block_path)
                    .map(|b| b["content"] = Value::String(stub.clone()))
                    .is_some()
            } else {
                body.pointer_mut(&block_path)
                    .map(|b| b["text"] = Value::String(stub.clone()))
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
    stat
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use serde_json::json;

    fn cfg() -> Config {
        Config {
            allow_user_text: true,
            ..Config::default()
        }
    }

    fn big(id: &str) -> Value {
        json!({"type": "tool_result", "tool_use_id": id, "content": "x-very-long-payload-".repeat(40)})
    }

    fn user_msg(blocks: Vec<Value>) -> Value {
        json!({"role": "user", "content": blocks})
    }

    #[test]
    fn single_occurrence_is_kept_verbatim() {
        let c = cfg();
        let mut body = json!({"model": "m", "messages": [user_msg(vec![big("t1")])]});
        let stat = transform(&mut body, &c);
        assert_eq!(stat.blocks_stubbed, 0);
        assert!(body["messages"][0]["content"][0]["content"]
            .as_str()
            .unwrap()
            .contains("x-very-long"));
    }

    #[test]
    fn duplicate_in_one_request_stubs_earlier_keeps_latest() {
        let c = cfg();
        let mut body = json!({"model": "m", "messages": [
            user_msg(vec![big("t1")]),
            json!({"role": "assistant", "content": [{"type": "text", "text": "ok"}]}),
            user_msg(vec![big("t1")]),
        ]});
        let stat = transform(&mut body, &c);
        assert_eq!(stat.blocks_stubbed, 1);
        assert!(stat.saved_bytes > 0);
        assert!(body["messages"][0]["content"][0]["content"]
            .as_str()
            .unwrap()
            .starts_with("[shrinkgate]"));
        // The newest occurrence stays verbatim.
        assert!(body["messages"][2]["content"][0]["content"]
            .as_str()
            .unwrap()
            .contains("x-very-long"));
    }

    #[test]
    fn stubs_are_deterministic_across_sessions() {
        let mk = || {
            let c = cfg();
            let mut body = json!({"model": "m", "messages": [
                user_msg(vec![big("t1")]),
                user_msg(vec![big("t1")]),
                json!({"role": "assistant", "content": [{"type": "text", "text": "ok"}]}),
            ]});
            transform(&mut body, &c);
            body["messages"][0]["content"][0]["content"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(mk(), mk());
        assert!(mk().contains("sha256:") && mk().contains("later in this request"));
    }

    #[test]
    fn keep_last_window_protects_recent_duplicates() {
        let mut c = cfg();
        c.keep_last_messages = 2;
        // Both occurrences sit inside the last 2 messages: nothing is stubbed.
        let mut body = json!({"model": "m", "messages": [
            json!({"role": "assistant", "content": [{"type": "text", "text": "a"}]}),
            user_msg(vec![big("t1")]),
            user_msg(vec![big("t1")]),
        ]});
        let stat = transform(&mut body, &c);
        assert_eq!(stat.blocks_stubbed, 0);
    }

    #[test]
    fn thinking_tool_use_and_media_are_never_touched() {
        let c = cfg();
        let img = json!({"type": "image", "source": {"type": "base64", "data": "AAAA"}});
        let mut body = json!({"model": "m", "messages": [
            user_msg(vec![
                json!({"type": "tool_result", "tool_use_id": "t1", "content": [
                    {"type": "text", "text": "x-very-long-payload-".repeat(40)},
                    img
                ]}),
            ]),
            json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "x-very-long-reasoning-".repeat(40), "signature": "sig"},
                {"type": "tool_use", "id": "tu1", "name": "bash", "input": {"cmd": "x-very-long-command-".repeat(40)}},
            ]}),
            user_msg(vec![big("t1")]),
        ]});
        let stat = transform(&mut body, &c);
        // The mixed-media tool result and the assistant blocks are not
        // candidates at all; the plain copy at the end has no duplicate here.
        assert_eq!(stat.blocks_stubbed, 0);
        // Nothing inside those protected shapes changed.
        let media = body["messages"][0]["content"][0]["content"]
            .as_array()
            .unwrap();
        assert!(media[1]["source"]["data"].as_str().unwrap() == "AAAA");
        let th = &body["messages"][1]["content"][0];
        assert!(th["thinking"]
            .as_str()
            .unwrap()
            .contains("x-very-long-reasoning"));
        let tu = &body["messages"][1]["content"][1];
        assert!(tu["input"]["cmd"]
            .as_str()
            .unwrap()
            .contains("x-very-long-command"));
    }

    #[test]
    fn pre_stubbed_content_is_never_double_stubbbed() {
        let mut c = cfg();
        c.min_block_bytes = 10; // force stubs past the size filter
        let stub = stub_text("test", 12345);
        let mut body = json!({"model": "m", "messages": [
            user_msg(vec![json!({"type": "tool_result", "tool_use_id": "t", "content": stub.clone()})]),
            user_msg(vec![json!({"type": "tool_result", "tool_use_id": "t", "content": stub})]),
        ]});
        let stat = transform(&mut body, &c);
        assert_eq!(stat.blocks_stubbed, 0);
        assert_eq!(stat.est_orig_bytes, 0);
    }

    #[test]
    fn small_blocks_below_threshold_never_stubbbed() {
        let c = cfg();
        let mut body = json!({"model": "m", "messages": [
            user_msg(vec![json!({"type": "tool_result", "tool_use_id": "t", "content": "short"})]),
            user_msg(vec![json!({"type": "tool_result", "tool_use_id": "t", "content": "short"})]),
        ]});
        assert_eq!(transform(&mut body, &c).blocks_stubbed, 0);
    }

    #[test]
    fn session_key_is_stable_and_diverges() {
        let a = json!({"model": "m", "system": "sys", "metadata": {"user_id": "u1"}});
        let b = json!({"model": "m", "system": "sys", "metadata": {"user_id": "u2"}});
        let d = json!({"model": "m2", "system": "sys", "metadata": {"user_id": "u1"}});
        assert_eq!(session_key(&a), session_key(&a));
        assert_ne!(session_key(&a), session_key(&b));
        assert_ne!(session_key(&a), session_key(&d));
    }
}
