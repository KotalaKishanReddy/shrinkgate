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
    v == ours
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
    dry_run: bool,
) -> anyhow::Result<()> {
    let ours = cfg.engine_url();
    let bin = crate::paths::bin_install_path();
    let path = settings_path();
    let old_state = crate::wire::read_state("claude")?;
    let mut settings = match crate::wire::read_optional(&path)? {
        Some(text) => serde_json::from_str::<Value>(&text)
            .map_err(|_| refuse("settings.json is not valid JSON"))?,
        None => json!({}),
    };
    if !settings.is_object()
        || settings.get("env").is_some_and(|v| !v.is_object())
        || settings.get("hooks").is_some_and(|v| !v.is_object())
        || lookup(&settings, "hooks.SessionStart").is_some_and(|v| !v.is_array())
    {
        return Err(refuse(
            "settings, env and hooks must be objects; SessionStart must be an array",
        ));
    }
    if old_state
        .as_ref()
        .is_some_and(|s| s.pointer("/written/env.ANTHROPIC_BASE_URL").is_some())
    {
        let old = old_state.as_ref().unwrap();
        if old["written"]
            .as_object()
            .is_some_and(|w| w.iter().all(|(k, v)| lookup(&settings, k) == Some(v)))
        {
            println!("claude: already connected; original backup preserved. Uninstall before changing options.");
            return Ok(());
        }
        return Err(refuse(
            "saved Claude settings differ; uninstall or resolve the backup before reconnecting",
        ));
    }

    let base_now = lookup(&settings, BASE_URL_KEY)
        .and_then(Value::as_str)
        .unwrap_or("");
    if !base_now.is_empty()
        && !is_our_url(base_now, &ours)
        && base_now != "https://api.anthropic.com"
        && !force
    {
        return Err(refuse(&format!(
            "ANTHROPIC_BASE_URL already points at {base_now}, another gateway. \
                 Remove it, or add --force to replace it (uninstall restores the old value)."
        )));
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
    let mut written = old_state
        .as_ref()
        .and_then(|s| s["written"].as_object())
        .cloned()
        .unwrap_or_default();
    let mut previous = old_state
        .as_ref()
        .and_then(|s| s["previous"].as_object())
        .cloned()
        .unwrap_or_default();
    {
        let mut record = |key: &str, value: Value| {
            previous
                .entry(key.to_string())
                .or_insert_with(|| lookup(&settings, key).cloned().unwrap_or(Value::Null));
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
                json!({"type": "command", "command": format!("{} statusline", crate::wire::command_path(&bin))}),
            );
        }
    }

    // SessionStart hook.
    let hook_entry = json!({"hooks": [{"type": "command", "command": format!("{} serve --ensure", crate::wire::command_path(&bin))}]});
    let hook_existed = lookup(&settings, "hooks").is_some();
    let session_start_existed = lookup(&settings, "hooks.SessionStart").is_some();
    let entry_json = serde_json::to_string(&hook_entry)?;
    let mut session_start = lookup(&settings, "hooks.SessionStart")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let hook_added = !session_start.iter().any(|e| {
        serde_json::to_string(e)
            .map(|s| s == entry_json)
            .unwrap_or(false)
    });
    if hook_added {
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
    set_path(
        &mut settings,
        "hooks.SessionStart",
        Value::Array(session_start),
    );

    // Persist state first, then settings (mirrors the original's ordering).
    let lean_managed = written.contains_key(LEAN_KEY);
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
            "added": hook_added,
        },
        "lean": lean_managed,
        "statusline": statusline,
        "service": !no_service,
        "bin": bin.display().to_string(),
    });
    let written = serde_json::to_string_pretty(&settings)? + "\n";
    if dry_run {
        crate::wire::preview(&path, &written);
        return Ok(());
    }
    let bin = ensure_bin()?;
    let service_used = !no_service && crate::wire::systemd::install(&bin);
    if !ensure_engine(cfg).await? {
        return Err(refuse("the ShrinkGate engine did not start"));
    }
    crate::wire::persist_connection("claude", &path, &written, &state)?;

    println!(
        "Connected. Claude Code now sends its requests through ShrinkGate on this computer ({}).",
        ours
    );
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
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    {
        Some(settings) => {
            let base = lookup(&settings, BASE_URL_KEY)
                .and_then(Value::as_str)
                .unwrap_or("");
            match &state {
                Some(st)
                    if st
                        .get("written")
                        .and_then(|w| w.get(BASE_URL_KEY))
                        .is_some()
                        && lookup(&settings, BASE_URL_KEY)
                            == st.get("written").and_then(|w| w.get(BASE_URL_KEY)) =>
                {
                    lines.push(format!(
                        "claude: connected through the ShrinkGate engine at {base}"
                    ));
                }
                Some(st)
                    if st
                        .get("written")
                        .and_then(|w| w.get(BASE_URL_KEY))
                        .is_none() =>
                {
                    lines.push("claude: routing not managed (lean settings/recovery only)".into())
                }
                Some(_) => {
                    lines.push("claude: settings.json no longer matches what ShrinkGate wrote. Run `shrinkgate uninstall claude` to restore.".into());
                }
                None if !base.is_empty() && is_our_url(base, &cfg.engine_url()) => {
                    lines.push(format!(
                        "claude: ANTHROPIC_BASE_URL points at {base} (not connected by ShrinkGate)"
                    ));
                }
                None if !base.is_empty() => {
                    lines.push(format!(
                        "claude: ANTHROPIC_BASE_URL points at {base} (not ShrinkGate)"
                    ));
                }
                None => lines.push("claude: not connected (no ANTHROPIC_BASE_URL)".into()),
            }
        }
        None => lines.push(format!("claude: no settings file at {path:?}")),
    }
    if let Some(st) = &state {
        if st.get("lean").and_then(Value::as_bool).unwrap_or(false) {
            lines.push(format!(
                "claude: keep sessions lean on (auto-compact window {LEAN_WINDOW})"
            ));
        }
        if st
            .get("statusline")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            lines.push("claude: statusline wired".into());
        }
    }
    lines.join("\n")
}

pub fn uninstall(_cfg: &Config) -> anyhow::Result<bool> {
    let state_file = wire_state_file("claude");
    let Some(state) = crate::wire::read_state("claude")? else {
        println!("Not connected by ShrinkGate. Nothing to restore for Claude Code.");
        return Ok(true);
    };
    let path = settings_path();
    let mut settings = match std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    {
        Some(v) => v,
        None => {
            println!("settings.json is unreadable; it was not modified.");
            return Ok(false);
        }
    };

    let mut leftovers = restore_written(&mut settings, &state);

    // Legacy snapshots cannot prove ownership when SessionStart already existed.
    let hook_owned = state
        .pointer("/hook/added")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            state
                .pointer("/hook/session_start_existed")
                .and_then(Value::as_bool)
                == Some(false)
        });
    if state.pointer("/hook/entry").is_some()
        && state.pointer("/hook/added").is_none()
        && !hook_owned
    {
        eprintln!("shrinkgate: legacy hook ownership is uncertain; SessionStart entry kept for manual review");
    }
    if let Some(entry) = state.pointer("/hook/entry").filter(|_| hook_owned) {
        if let Some(arr) =
            lookup_mut(&mut settings, "hooks.SessionStart").and_then(Value::as_array_mut)
        {
            let before = arr.len();
            arr.retain(|e| {
                serde_json::to_string(e).ok().as_deref()
                    != serde_json::to_string(entry).ok().as_deref()
            });
            if arr.len() == before {
                leftovers.push("hooks.SessionStart (entry not found)".into());
            }
            if arr.is_empty()
                && state
                    .pointer("/hook/session_start_existed")
                    .and_then(Value::as_bool)
                    != Some(true)
            {
                remove_path(&mut settings, "hooks.SessionStart");
            }
        }
        if state.pointer("/hook/existed").and_then(Value::as_bool) != Some(true)
            && lookup(&settings, "hooks")
                .and_then(Value::as_object)
                .map(|o| o.is_empty())
                .unwrap_or(false)
        {
            remove_path(&mut settings, "hooks");
        }
    }

    crate::wire::atomic_write(
        &path,
        (serde_json::to_string_pretty(&settings)? + "\n").as_bytes(),
    )?;
    if !leftovers.is_empty() {
        let mut remaining = state.clone();
        remaining["written"]
            .as_object_mut()
            .unwrap()
            .retain(|k, _| leftovers.contains(k));
        remaining.as_object_mut().unwrap().remove("hook");
        write_json_private(&state_file, &remaining)?;
        println!(
            "Changed settings kept; recovery state retained: {}",
            leftovers.join(", ")
        );
        return Ok(false);
    }
    let _ = std::fs::remove_file(&state_file);

    let mut kept_service = false;
    if no_other_wires(Some("claude")) {
        crate::wire::systemd::remove();
    } else {
        kept_service = true;
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
                    println!(
                        "lean: on{} ({LEAN_WINDOW} tokens)",
                        if wired {
                            ""
                        } else {
                            " (set outside ShrinkGate)"
                        }
                    );
                }
                Some(other) => println!("lean: window set to {other:?} outside ShrinkGate (kept)"),
                None => println!("lean: off (no window set)"),
            }
            Ok(0)
        }
        "on" | "off" => {
            let mut settings: Value = match crate::wire::read_optional(&path)? {
                Some(text) => serde_json::from_str(&text)?,
                None => json!({}),
            };
            anyhow::ensure!(
                settings.is_object() && settings.get("env").is_none_or(Value::is_object),
                "settings and env must be JSON objects"
            );
            let mut state = crate::wire::read_state("claude")?.unwrap_or(json!({
                "version": 1, "written": {}, "previous": {},
                "connected_at": now_iso(),
            }));
            if action == "on" {
                let cur = lookup(&settings, LEAN_KEY)
                    .and_then(Value::as_str)
                    .unwrap_or("");
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
                        .insert(LEAN_KEY.to_string(), prev);
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
                crate::wire::persist_connection(
                    "claude",
                    &path,
                    &(serde_json::to_string_pretty(&settings)? + "\n"),
                    &state,
                )?;
                println!("lean: on (auto-compact window {LEAN_WINDOW} tokens). Start a new Claude Code session.");
            } else {
                let mut only_lean = state.clone();
                only_lean["written"]
                    .as_object_mut()
                    .unwrap()
                    .retain(|k, _| k == LEAN_KEY);
                let leftovers = restore_written(&mut settings, &only_lean);
                anyhow::ensure!(
                    leftovers.is_empty(),
                    "lean setting changed externally; backup retained"
                );
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
                if let Some(previous) = state.get_mut("previous").and_then(Value::as_object_mut) {
                    previous.remove(LEAN_KEY);
                }
                crate::wire::persist_connection(
                    "claude",
                    &path,
                    &(serde_json::to_string_pretty(&settings)? + "\n"),
                    &state,
                )?;
                println!("lean: off. Start a new Claude Code session.");
            }
            Ok(0)
        }
        _ => Err(anyhow::anyhow!(
            "unknown lean action: {action} (use on|off|status)"
        )),
    }
}
