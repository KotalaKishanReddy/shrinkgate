//! Content-level shrink pass: the core of the gateway.
//!
//! Design (original implementation for ShrinkGate):
//! - Every shrinkable text payload (tool results, long text blocks, chat
//!   message contents, function-call outputs) gets a stable fingerprint.
//! - Within one agent session, when the SAME payload appears again in a later
//!   request (agents resend the whole history every turn), every occurrence
//!   except the newest is replaced by a short deterministic stub.
//! - Determinism across requests keeps the upstream prompt cache useful: the
//!   rewritten prefix is byte-identical from the request after the first one.
//! - Stubs carry the first-seen request number and original size, so the model
//!   still understands that the content exists earlier in the conversation.
//! - The newest occurrence is always kept verbatim, so the model always has
//!   the current copy of any file/command output.
//! - Thinking/reasoning blocks, tool_use/tool_call definitions, images,
//!   system prompts and tool schemas are never touched.

pub mod anthropic;
pub mod openai;
pub mod responses;

pub const MARKER: &str = "[shrinkgate]";

pub fn is_stub_text(s: &str) -> bool {
    s.starts_with(MARKER)
}

pub fn stub_text(first_turn: u32, bytes: usize) -> String {
    // `first_turn` is the 0-based request ordinal; display 1-based to users.
    format!(
        "{MARKER} unchanged content — an identical copy was already sent earlier in this \
         conversation (first seen in request #{}, {bytes} bytes). This duplicate \
         was trimmed locally to save context; the original text above is still accurate.",
        first_turn + 1
    )
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
