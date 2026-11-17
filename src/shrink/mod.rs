//! Content-level shrink pass: the core of the gateway.
//!
//! Deduplication is request-local and stateless. Only earlier identical copies
//! outside the protected tail are replaced; the last copy stays verbatim.
//! Moving the retained copy can invalidate a provider prompt-cache prefix.

pub mod anthropic;
pub mod openai;
pub mod responses;

pub const MARKER: &str = "[shrinkgate]";

pub fn is_stub_text(s: &str) -> bool {
    s.starts_with(MARKER)
}

pub fn stub_text(hash: &str, bytes: usize) -> String {
    format!("{MARKER} duplicate omitted ({bytes} bytes; sha256:{hash}); identical content is retained later in this request.")
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ShrinkStat {
    /// Total bytes of candidate-class content before the shrink pass.
    pub est_orig_bytes: usize,
    /// The same after stubbing (what actually goes upstream).
    pub est_sent_bytes: usize,
    pub blocks_stubbed: usize,
    pub saved_bytes: usize,
}

#[cfg(test)]
mod invariants {
    use super::*;
    use crate::config::Config;
    use serde_json::json;
    #[test]
    fn small_duplicates_never_expand_requests() {
        let cfg = Config {
            min_block_bytes: 0,
            keep_last_messages: 0,
            ..Config::default()
        };
        let mut a =
            json!({"messages":[{"role":"user","content":"a"},{"role":"user","content":"a"}]});
        let original = a.clone();
        assert_eq!(openai::transform(&mut a, &cfg).blocks_stubbed, 0);
        assert_eq!(a, original);
    }
    #[test]
    fn chat_mixed_media_and_system_are_untouched() {
        let text = "content".repeat(200);
        let mut v = json!({"messages":[
            {"role":"system","content":text},
            {"role":"user","content":[{"type":"text","text":text},{"type":"image_url","image_url":{"url":"https://example.test/image.png"}}]},
            {"role":"user","content":text},{"role":"user","content":"tail"}
        ]});
        let before = v.clone();
        openai::transform(&mut v, &Config::default());
        assert_eq!(v, before);
    }
    #[test]
    fn deterministic_after_replay_and_changed_tail() {
        let text = "old file".repeat(200);
        let body = json!({"input":[
            {"type":"function_call_output","call_id":"1","output":text},
            {"type":"function_call_output","call_id":"2","output":text},
            {"type":"function_call_output","call_id":"3","output":"new file".repeat(200)}
        ]});
        let mut a = body.clone();
        let mut b = body;
        responses::transform(&mut a, &Config::default());
        responses::transform(&mut b, &Config::default());
        assert_eq!(a, b);
        let transformed = a.clone();
        responses::transform(&mut a, &Config::default());
        assert_eq!(a, transformed);
        assert_eq!(a["input"][1]["output"], text);
        assert_eq!(a["input"][2]["output"], "new file".repeat(200));
    }
}
