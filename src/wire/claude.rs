//! Claude Code wiring: `~/.claude/settings.json`.
//!
//! connect writes:
//!   env.ANTHROPIC_BASE_URL        -> http://127.0.0.1:<port>  (the gateway)
//!   env.ENABLE_TOOL_SEARCH        -> "true" (only when the user has not set it)
//!   env.CLAUDE_CODE_AUTO_COMPACT_WINDOW -> "200000" (only with --lean / lean on)
//!   statusLine                    -> shrinkgate statusline (opt-in)
//!   hooks.SessionStart            -> `shrinkgate serve --ensure`
//!
//! Every key's previous value is recorded; uninstall restores it while the
//! file still holds what we wrote.

use serde_json::{json, Value};
use std::path::PathBuf;

use crate::config::Config;
use crate::wire::{
    ensure_bin, ensure_engine, lookup, lookup_mut, no_other_wires, now_iso, read_json_file,
    remove_path, restore_written, set_path, wire_state_file, write_json_private,
};

const BASE_URL_KEY: &str = "env.ANTHROPIC_BASE_URL";
const TOOL_SEARCH_KEY: &str = "env.ENABLE_TOOL_SEARCH";
const LEAN_KEY: &str = "env.CLAUDE_CODE_AUTO_COMPACT_WINDOW";
pub const LEAN_WINDOW: &str = "200000";

fn settings_path() -> PathBuf {
    crate::paths::claude_dir().join("settings.json")
}

fn is_our_url(v: &str, ours: &str) -> bool {
    v == ours || v.starts_with("http://127.0.0.1:") || v.starts_with("http://localhost:")
}

fn refuse(msg: &str) -> anyhow::Error {
    anyhow::anyhow!("{msg} Nothing was changed.")
}

pub async fn connect(
    cfg: &Config,
    force: bool,
    lean: bool,
    statusline: bool,
    no_service: bool,
) -> anyhow::Result<()> {
    let ours = cfg.engine_url();
    let bin = ensure_bin()?;
    let mut service_used = false;
    if !no_service {
        service_used = crate::wire::systemd::install(&bin);
    }
    if !ensure_engine(cfg).await? {
        return Err(refuse(
            "the ShrinkGate engine did not start (systemd user service unavailable and detached spawn failed)",
        ));
    }

    let path = settings_path();
    let mut settings = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<Value>(&text)
            .map_err(|_| refuse(&format!("{path:?} is not valid JSON")))?,
        Err(_) => json!({}),
    };

    let base_now = lookup(&settings, BASE_URL_KEY).and_then(Value::as_str).unwrap_or("");
    if !base_now.is_empty() && !is_our_url(base_now, &ours) && base_now != "https://api.anthropic.com" {
        if !force {
            return Err(refuse(
                &format!(
                    "ANTHROPIC_BASE_URL already points at {base_now}, another gateway. \
                     Remove it, or add --force to replace it (uninstall restores the old value)."
                ),
            ));
        }
    }
    if lookup(&settings, "env.ANTHROPIC_CUSTOM_HEADERS")
        .and_then(Value::as_str)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
        && !force
    {
        return Err(refuse(
            "ANTHROPIC_CUSTOM_HEADERS is already set. Remove it, or add --force. ",
        ));
    }

    // Record previous values for everything we are about to write.
    let mut written = serde_json::Map::new();
    let mut previous = serde_json::Map::new();
    {
        let mut record =
            |key: &str, value: Value| {
                previous.insert(key.to_string(), lookup(&settings, key).cloned().unwrap_or(Value::Null));
                written.insert(key.to_string(), value);
            };
        record(BASE_URL_KEY, Value::String(ours.clone()));
        if lookup(&settings, TOOL_SEARCH_KEY).is_none() {
            record(TOOL_SEARCH_KEY, Value::String("true".into()));
        }
        if lean && lookup(&settings, LEAN_KEY).is_none() {
            record(LEAN_KEY, Value::String(LEAN_WINDOW.into()));
        }
        if statusline {
            record(
                "statusLine",
                json!({"type": "command", "command": format!("{} statusline", bin.display())}),
            );
        }
    }

    // SessionStart hook.
    let hook_entry = json!({"hooks": [{"type": "command", "command": format!("{} serve --ensure", bin.display())}]});
    let hook_existed = lookup(&settings, "hooks").is_some();
    let session_start_existed = lookup(&settings, "hooks.SessionStart").is_some();
    let entry_json = serde_json::to_string(&hook_entry)?;
    let mut session_start = lookup(&settings, "hooks.SessionStart")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !session_start.iter().any(|e| serde_json::to_string(e).map(|s| s == entry_json).unwrap_or(false)) {
        session_start.push(hook_entry.clone());
    }

    // Apply.
    set_path(&mut settings, BASE_URL_KEY, Value::String(ours.clone()));
    if let Some(v) = written.get(TOOL_SEARCH_KEY) {
        set_path(&mut settings, TOOL_SEARCH_KEY, v.clone());
    }
    if let Some(v) = written.get(LEAN_KEY) {
        set_path(&mut settings, LEAN_KEY, v.clone());
    }
    if let Some(v) = written.get("statusLine") {
        set_path(&mut settings, "statusLine", v.clone());
    }
    set_path(&mut settings, "hooks.SessionStart", Value::Array(session_start));

    // Persist state first, then settings (mirrors the original's ordering).
    let state = json!({
        "version": 1,
        "connected_at": now_iso(),
        "engine_url": ours,
        "written": Value::Object(written),
        "previous": Value::Object(previous),
        "hook": {
            "existed": hook_existed,
            "session_start_existed": session_start_existed,
            "entry": hook_entry,
        },
        "lean": lean,
        "statusline": statusline,
        "service": !no_service,
        "bin": bin.display().to_string(),
    });
    write_json_private(&wire_state_file("claude"), &state)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&settings)? + "\n",
    )?;

    println!("Connected. Claude Code now sends its requests through ShrinkGate on this computer ({}).", ours);
    if !service_used && !no_service {
        println!("systemd user service unavailable; the SessionStart hook keeps the engine running instead.");
    }
    if lean {
        println!("Keep sessions lean: auto-compact window capped at {LEAN_WINDOW} tokens.");
    }
    println!("Start a new Claude Code session to use the connection.");
    Ok(())
}

pub fn status(cfg: &Config) -> String {
    let path = settings_path();
    let state = read_json_file(&wire_state_file("claude"));
    let mut lines = Vec::new();
    match std::fs::read_to_string(&path)
        .ok()
        .map(|t| serde_json::from_str::<Value>(&t).ok())
        .flatten()
    {
        Some(settings) => {
            let base = lookup(&settings, BASE_URL_KEY).and_then(Value::as_str).unwrap_or("");
            match &state {
                Some(st) if lookup(&settings, BASE_URL_KEY) == st.get("written").and_then(|w| w.get(BASE_URL_KEY)) => {
                    lines.push(format!("claude: connected through the ShrinkGate engine at {base}"));
                }
                Some(_) => {
                    lines.push("claude: settings.json no longer matches what ShrinkGate wrote. Run `shrinkgate uninstall claude` to restore.".into());
                }
                None if !base.is_empty() && is_our_url(base, &cfg.engine_url()) => {
                    lines.push(format!("claude: ANTHROPIC_BASE_URL points at {base} (not connected by ShrinkGate)"));
                }
                None if !base.is_empty() => {
                    lines.push(format!("claude: ANTHROPIC_BASE_URL points at {base} (not ShrinkGate)"));
                }
                None => lines.push("claude: not connected (no ANTHROPIC_BASE_URL)".into()),
            }
        }
        None => lines.push(format!("claude: no settings file at {path:?}")),
    }
    if let Some(st) = &state {
        if st.get("lean").and_then(Value::as_bool).unwrap_or(false) {
            lines.push(format!("claude: keep sessions lean on (auto-compact window {LEAN_WINDOW})"));
        }
        if st.get("statusline").and_then(Value::as_bool).unwrap_or(false) {
            lines.push("claude: statusline wired".into());
        }
    }
    lines.join("\n")
}

pub fn uninstall(_cfg: &Config, purge: bool) -> anyhow::Result<bool> {
    let state_file = wire_state_file("claude");
    let Some(state) = read_json_file(&state_file) else {
        println!("Not connected by ShrinkGate. Nothing to restore for Claude Code.");
        return Ok(no_other_wires(None) && purge_clean(purge));
    };
    let path = settings_path();
    let mut settings = match std::fs::read_to_string(&path)
        .ok()
        .map(|t| serde_json::from_str::<Value>(&t).ok())
        .flatten()
    {
        Some(v) => v,
        None => {
            println!("settings.json is unreadable; it was not modified.");
            let _ = std::fs::remove_file(&state_file);
            return Ok(true);
        }
    };

    let mut leftovers = restore_written(&mut settings, &state);

    // Remove our SessionStart hook entry, whatever the state says.
    if let Some(entry) = state.pointer("/hook/entry") {
        if let Some(arr) = lookup_mut(&mut settings, "hooks.SessionStart").and_then(Value::as_array_mut) {
            let before = arr.len();
            arr.retain(|e| serde_json::to_string(e).ok().as_deref() != serde_json::to_string(entry).ok().as_deref());
            if arr.len() == before {
                leftovers.push("hooks.SessionStart (entry not found)".into());
            }
            if arr.is_empty() {
                remove_path(&mut settings, "hooks.SessionStart");
            }
        }
        if lookup(&settings, "hooks")
            .and_then(Value::as_object)
            .map(|o| o.is_empty())
            .unwrap_or(false)
        {
            remove_path(&mut settings, "hooks");
        }
    }

    std::fs::write(&path, serde_json::to_string_pretty(&settings)? + "\n")?;
    let _ = std::fs::remove_file(&state_file);

    let mut kept_service = false;
    if no_other_wires(Some("claude")) {
        crate::wire::systemd::remove();
    } else {
        kept_service = true;
    }

    if purge {
        purge_clean(true);
    }

    if leftovers.is_empty() {
        println!("Restored Claude Code settings. Start a new session.");
        if kept_service {
            println!("The engine service stays (Codex/OpenCode still wired to it).");
        }
        Ok(true)
    } else {
        println!("Left alone because they changed since connect:");
        for l in leftovers {
            println!("  - {l}");
        }
        Ok(false)
    }
}

fn purge_clean(purge: bool) -> bool {
    if purge {
        let _ = std::fs::remove_file(crate::paths::bin_install_path());
        let _ = std::fs::remove_dir_all(crate::paths::state_dir());
        println!("Purged the ShrinkGate binary and all state.");
    }
    true
}

/// lean on/off/status: only the auto-compact window.
pub fn lean(action: &str, force: bool) -> anyhow::Result<i32> {
    let state_file = wire_state_file("claude");
    let path = settings_path();
    match action {
        "status" => {
            let settings = read_json_file(&path).unwrap_or(json!({}));
            let cur = lookup(&settings, LEAN_KEY).and_then(Value::as_str);
            match cur {
                Some(LEAN_WINDOW) => {
                    let wired = read_json_file(&state_file)
                        .map(|s| s.get("lean").and_then(Value::as_bool).unwrap_or(false))
                        .unwrap_or(false);
                    println!("lean: on{} ({LEAN_WINDOW} tokens)", if wired { "" } else { " (set outside ShrinkGate)" });
                }
                Some(other) => println!("lean: window set to {other:?} outside ShrinkGate (kept)"),
                None => println!("lean: off (no window set)"),
            }
            Ok(0)
        }
        "on" | "off" => {
            let mut settings = std::fs::read_to_string(&path)
                .ok()
                .map(|t| serde_json::from_str::<Value>(&t))
                .and_then(|r| r.ok())
                .unwrap_or(json!({}));
            let mut state = read_json_file(&state_file).unwrap_or(json!({
                "version": 1, "written": {}, "previous": {},
                "connected_at": now_iso(),
            }));
            if action == "on" {
                let cur = lookup(&settings, LEAN_KEY).and_then(Value::as_str).unwrap_or("");
                if !cur.is_empty() && cur != LEAN_WINDOW && !force {
                    return Err(refuse(
                        "CLAUDE_CODE_AUTO_COMPACT_WINDOW is set to your own value; add --force to replace it",
                    ));
                }
                let prev = lookup(&settings, LEAN_KEY).cloned().unwrap_or(Value::Null);
                if !state
                    .get("written")
                    .map(|w| w.get(LEAN_KEY).is_some())
                    .unwrap_or(false)
                {
                    state
                        .as_object_mut()
                        .unwrap()
                        .entry("previous")
                        .or_insert(json!({}))
                        .as_object_mut()
                        .unwrap()
                        .entry(LEAN_KEY)
                        .or_insert(prev);
                    state
                        .as_object_mut()
                        .unwrap()
                        .entry("written")
                        .or_insert(json!({}))
                        .as_object_mut()
                        .unwrap()
                        .insert(LEAN_KEY.to_string(), Value::String(LEAN_WINDOW.into()));
                }
                set_path(&mut settings, LEAN_KEY, Value::String(LEAN_WINDOW.into()));
                state
                    .as_object_mut()
                    .unwrap()
                    .insert("lean".into(), Value::Bool(true));
                write_json_private(&state_file, &state)?;
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::fs::write(&path, serde_json::to_string_pretty(&settings)? + "\n")?;
                println!("lean: on (auto-compact window {LEAN_WINDOW} tokens). Start a new Claude Code session.");
            } else {
                let mut leftovers = restore_written(&mut settings, &state);
                leftovers.retain(|k| k == LEAN_KEY);
                state
                    .as_object_mut()
                    .unwrap()
                    .insert("lean".into(), Value::Bool(false));
                if let Some(w) = state
                    .as_object_mut()
                    .unwrap()
                    .get_mut("written")
                    .and_then(Value::as_object_mut)
                {
                    w.remove(LEAN_KEY);
                }
                write_json_private(&state_file, &state)?;
                std::fs::write(&path, serde_json::to_string_pretty(&settings)? + "\n")?;
                println!("lean: off. Start a new Claude Code session.");
            }
            Ok(0)
        }
        _ => Err(anyhow::anyhow!("unknown lean action: {action} (use on|off|status)")),
    }
}
