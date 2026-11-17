//! Filesystem locations for ShrinkGate. Everything is under the user's home:
//! `~/.config/shrinkgate` for configuration, `~/.local/state/shrinkgate` for
//! journals, `~/.local/bin/shrinkgate` for the installed binary.

use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}

fn base_dir(env: &str, fallback: &str) -> PathBuf {
    match std::env::var(env) {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

pub fn config_dir() -> PathBuf {
    base_dir("XDG_CONFIG_HOME", ".config").join("shrinkgate")
}

pub fn state_dir() -> PathBuf {
    base_dir("XDG_STATE_HOME", ".local/state").join("shrinkgate")
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn usage_file(state: &Path) -> PathBuf {
    state.join("usage.jsonl")
}

/// Claude Code's config folder: `CLAUDE_CONFIG_DIR` (with `~` expanded), else `~/.claude`.
pub fn claude_dir() -> PathBuf {
    if let Ok(v) = std::env::var("CLAUDE_CONFIG_DIR") {
        let v = v.trim();
        if !v.is_empty() {
            return expand_tilde(v);
        }
    }
    home().join(".claude")
}

/// Codex's config folder: `CODEX_HOME`, else `~/.codex`.
pub fn codex_dir() -> PathBuf {
    if let Ok(v) = std::env::var("CODEX_HOME") {
        let v = v.trim();
        if !v.is_empty() {
            return expand_tilde(v);
        }
    }
    home().join(".codex")
}

/// OpenCode's global config file.
pub fn opencode_config() -> PathBuf {
    base_dir("XDG_CONFIG_HOME", ".config")
        .join("opencode")
        .join("opencode.json")
}

/// Where `connect` installs the binary that hooks and the systemd unit call.
pub fn bin_install_path() -> PathBuf {
    home().join(".local/bin/shrinkgate")
}

#[allow(dead_code)]
pub fn systemd_unit_path() -> PathBuf {
    base_dir("XDG_CONFIG_HOME", ".config").join("systemd/user/shrinkgate.service")
}

#[allow(dead_code)]
pub fn launchd_plist_path() -> PathBuf {
    home().join("Library/LaunchAgents/com.shrinkgate.engine.plist")
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if p == "~" {
        return home();
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return home().join(rest);
    }
    PathBuf::from(p)
}
