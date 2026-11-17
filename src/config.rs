//! Server configuration. Defaults are compiled in; `~/.config/shrinkgate/config.toml`
//! overrides them. CLI flags override the file.

use std::path::Path;

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub shrink: bool,
    /// Blocks smaller than this many bytes are never stubbed.
    pub min_block_bytes: usize,
    /// The N most recent messages of a conversation are never stubbed.
    pub keep_last_messages: usize,
    /// Also stub identical *assistant* text blocks (off: assistant text is cheap
    /// and carries continuity; tool results are where the bulk is).
    pub allow_assistant_text: bool,
    /// Cap on concurrent tracked sessions (LRU eviction beyond this).
    pub max_sessions: usize,
    // Upstream bases. Anthropic/Meta without a path suffix (the client path is
    // appended verbatim); OpenAI likewise (`/v1/...` comes from the request).
    pub upstream_anthropic: String,
    pub upstream_openai: String,
    pub upstream_meta: String,
    /// OpenCode Zen inference endpoint (opencode-native models).
    pub upstream_opencode: String,
    /// Codex ChatGPT-login backend (responses API). The leading `/v1` of the
    /// request path is stripped when routing here.
    pub upstream_chatgpt: String,
    /// Upstream for unrecognized paths: "anthropic" | "openai" | "meta".
    pub default_upstream: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 18793,
            shrink: true,
            min_block_bytes: 600,
            keep_last_messages: 2,
            allow_assistant_text: false,
            max_sessions: 1024,
            upstream_anthropic: "https://api.anthropic.com".into(),
            upstream_openai: "https://api.openai.com".into(),
            upstream_meta: "https://api.meta.ai".into(),
            upstream_opencode: "https://opencode.ai/inference/openai".into(),
            upstream_chatgpt: "https://chatgpt.com/backend-api/codex".into(),
            default_upstream: "openai".into(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let mut cfg = Self::default();
        let file = crate::paths::config_file();
        if let Ok(text) = std::fs::read_to_string(&file) {
            if let Ok(doc) = text.parse::<toml_edit::DocumentMut>() {
                cfg.port = doc
                    .get("port")
                    .and_then(|i| i.as_integer())
                    .and_then(|v| u16::try_from(v).ok())
                    .unwrap_or(cfg.port);
                cfg.shrink = doc
                    .get("shrink")
                    .and_then(|i| i.as_bool())
                    .unwrap_or(cfg.shrink);
                cfg.min_block_bytes = doc
                    .get("min_block_bytes")
                    .and_then(|i| i.as_integer())
                    .map(|v| v.max(0) as usize)
                    .unwrap_or(cfg.min_block_bytes);
                cfg.keep_last_messages = doc
                    .get("keep_last_messages")
                    .and_then(|i| i.as_integer())
                    .map(|v| v.max(0) as usize)
                    .unwrap_or(cfg.keep_last_messages);
                cfg.allow_assistant_text = doc
                    .get("allow_assistant_text")
                    .and_then(|i| i.as_bool())
                    .unwrap_or(cfg.allow_assistant_text);
                cfg.max_sessions = doc
                    .get("max_sessions")
                    .and_then(|i| i.as_integer())
                    .map(|v| v.max(8) as usize)
                    .unwrap_or(cfg.max_sessions);
                for (key, field) in [
                    ("upstream_anthropic", &mut cfg.upstream_anthropic),
                    ("upstream_openai", &mut cfg.upstream_openai),
                    ("upstream_meta", &mut cfg.upstream_meta),
                    ("upstream_opencode", &mut cfg.upstream_opencode),
                    ("upstream_chatgpt", &mut cfg.upstream_chatgpt),
                ] {
                    if let Some(s) = doc.get(key).and_then(|i| i.as_str()) {
                        if !s.trim().is_empty() {
                            *field = s.trim().to_string();
                        }
                    }
                }
                if let Some(s) = doc.get("default_upstream").and_then(|i| i.as_str()) {
                    if ["anthropic", "openai", "meta", "opencode"].contains(&s.trim()) {
                        cfg.default_upstream = s.trim().to_string();
                    }
                }
            }
        }
        cfg
    }

    pub fn write_default_example(dir: &Path) {
        if dir.join("config.toml").exists() {
            return;
        }
        let _ = std::fs::create_dir_all(dir);
        let text = "# ShrinkGate configuration (all keys optional)\n\
                    # port = 18793\n\
                    # shrink = true\n\
                    # min_block_bytes = 600          # never stub blocks smaller than this\n\
                    # keep_last_messages = 2         # never stub the N newest messages\n\
                    # allow_assistant_text = false\n\
                    # upstream_anthropic = \"https://api.anthropic.com\"\n\
                    # upstream_openai = \"https://api.openai.com\"\n\
                    # upstream_meta = \"https://api.meta.ai\"\n\
                    # upstream_opencode = \"https://opencode.ai/inference/openai\"\n\
                    # upstream_chatgpt = \"https://chatgpt.com/backend-api/codex\"\n\
                    # default_upstream = \"openai\"\n";
        let _ = std::fs::write(dir.join("config.toml"), text);
    }

    pub fn engine_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}
