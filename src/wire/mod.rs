//! Agent wiring: point coding agents at the local gateway, record what was
//! changed, restore it on uninstall. Mirrors the stateful-restore design of
//! the original connector (previous values recorded, restored only while the
//! file still holds what we wrote).

pub mod claude;
pub mod codex;
pub mod muse;
pub mod opencode;
pub mod service;
pub mod systemd;

use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::config::Config;

pub fn wire_state_file(agent: &str) -> PathBuf {
    crate::paths::state_dir().join(format!("wire-{agent}.json"))
}

pub fn read_json_file(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Atomically replace private data, with permissions set before any bytes are written.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent"))?;
    anyhow::ensure!(
        !std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()),
        "refusing to replace symlink {}; manage this file manually",
        path.display()
    );
    std::fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn write_json_private(path: &Path, v: &Value) -> anyhow::Result<()> {
    atomic_write(path, (serde_json::to_string_pretty(v)? + "\n").as_bytes())
}

/// Serialize CLI changes across processes, including recovery state updates.
pub fn config_lock() -> anyhow::Result<std::fs::File> {
    let dir = crate::paths::state_dir();
    std::fs::create_dir_all(&dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("config.lock"))?;
    fs2::FileExt::try_lock_exclusive(&file)
        .map_err(|_| anyhow::anyhow!("another ShrinkGate configuration command is running"))?;
    Ok(file)
}

pub fn read_optional(path: &Path) -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn read_state(agent: &str) -> anyhow::Result<Option<Value>> {
    let Some(text) = read_optional(&wire_state_file(agent))? else {
        return Ok(None);
    };
    let v: Value = serde_json::from_str(&text)?;
    anyhow::ensure!(
        v.is_object(),
        "invalid {agent} recovery state: expected an object"
    );
    if agent == "claude" {
        anyhow::ensure!(
            v["written"].is_object() && v["previous"].is_object(),
            "invalid Claude recovery maps; state retained"
        );
    } else {
        anyhow::ensure!(
            v["written"].is_string() && (v["original"].is_null() || v["original"].is_string()),
            "invalid {agent} recovery snapshot; state retained"
        );
    }
    Ok(Some(v))
}

/// Keep the first recovery snapshot; never overwrite it on reconnect.
pub fn already_connected(agent: &str, path: &Path) -> anyhow::Result<bool> {
    if let Some(state) = read_state(agent)? {
        let current = read_optional(path)?;
        if current.as_deref() == state.get("written").and_then(Value::as_str) {
            println!("{agent}: already connected; original backup preserved. Uninstall before changing options.");
            return Ok(true);
        }
        anyhow::bail!("{agent}: recovery state exists but settings changed; uninstall or resolve the saved backup before reconnecting");
    }
    Ok(false)
}

pub fn persist_connection(
    agent: &str,
    path: &Path,
    written: &str,
    state: &Value,
) -> anyhow::Result<()> {
    let state_path = wire_state_file(agent);
    let previous_state = read_optional(&state_path)?;
    write_json_private(&state_path, state)?;
    if let Err(error) = atomic_write(path, written.as_bytes()) {
        match previous_state {
            Some(s) => atomic_write(&state_path, s.as_bytes())?,
            None => std::fs::remove_file(&state_path)?,
        }
        return Err(error);
    }
    Ok(())
}

pub fn preview(path: &Path, written: &str) {
    println!(
        "Dry run: proposed contents of {}\n{}",
        path.display(),
        written
    );
    println!("No configuration, service, or binary was changed.");
}

/// Copies the running binary to `~/.local/bin/shrinkgate` so hooks and the
/// systemd unit have a stable path.
pub fn ensure_bin() -> anyhow::Result<PathBuf> {
    let dst = crate::paths::bin_install_path();
    let src = std::env::current_exe()?;
    if src.canonicalize().ok() != dst.canonicalize().ok() {
        let bytes = std::fs::read(&src)?;
        if std::fs::read(&dst).ok().as_deref() != Some(&bytes) {
            atomic_write(&dst, &bytes)?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(dst)
}

pub fn command_path(path: &Path) -> String {
    #[cfg(unix)]
    {
        format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
    }
    #[cfg(not(unix))]
    {
        format!("\"{}\"", path.display())
    }
}

/// Read bounded health metadata. Configuration/version checks are explicit.
pub async fn engine_info(port: u16) -> Option<Value> {
    let probe = async {
        use http_body_util::BodyExt;
        let req = hyper::Request::builder()
            .uri(format!("http://127.0.0.1:{port}/healthz"))
            .body(crate::upstream::full(bytes::Bytes::new()))
            .ok()?;
        let resp = crate::upstream::make_client().request(req).await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let bytes = http_body_util::Limited::new(resp.into_body(), 4096)
            .collect()
            .await
            .ok()?
            .to_bytes();
        let v: Value = serde_json::from_slice(&bytes).ok()?;
        if v["engine"] == "shrinkgate" && v["ok"] == true && v["port"] == port {
            Some(v)
        } else {
            None
        }
    };
    tokio::time::timeout(std::time::Duration::from_millis(700), probe)
        .await
        .ok()
        .flatten()
}

pub fn health_matches(info: &Value, cfg: &Config) -> bool {
    info["version"] == env!("CARGO_PKG_VERSION")
        && info["health_protocol"] == 2
        && info["config_fingerprint"] == cfg.fingerprint()
}

pub async fn engine_matches(cfg: &Config) -> bool {
    engine_info(cfg.port)
        .await
        .is_some_and(|v| health_matches(&v, cfg))
}

/// Inspect managed configuration rather than parsing human-readable status text.
pub fn wiring_issues() -> Vec<String> {
    let mut issues = Vec::new();
    for (agent, path) in [
        ("claude", crate::paths::claude_dir().join("settings.json")),
        ("codex", crate::paths::codex_dir().join("config.toml")),
        ("opencode", crate::paths::opencode_config()),
    ] {
        let check = (|| -> anyhow::Result<()> {
            let state = read_state(agent)?;
            let current = read_optional(&path)?;
            if let Some(ref text) = current {
                if agent == "codex" {
                    text.parse::<toml_edit::DocumentMut>()?;
                } else {
                    anyhow::ensure!(
                        serde_json::from_str::<Value>(text)?.is_object(),
                        "settings must be an object"
                    );
                }
            }
            if let Some(state) = state {
                if agent == "claude" {
                    let doc: Value = serde_json::from_str(current.as_deref().unwrap_or("{}"))?;
                    for (key, value) in state["written"].as_object().unwrap() {
                        anyhow::ensure!(
                            lookup(&doc, key) == Some(value),
                            "managed setting {key} changed or is missing"
                        );
                    }
                    if state.pointer("/hook/added").and_then(Value::as_bool) == Some(true) {
                        anyhow::ensure!(
                            lookup(&doc, "hooks.SessionStart")
                                .and_then(Value::as_array)
                                .is_some_and(|entries| entries.contains(&state["hook"]["entry"])),
                            "managed SessionStart hook changed or is missing"
                        );
                    }
                } else {
                    anyhow::ensure!(
                        current.as_deref() == state["written"].as_str(),
                        "managed configuration changed or is missing"
                    );
                }
            }
            Ok(())
        })();
        if let Err(e) = check {
            issues.push(format!("{agent}: {e}"));
        }
    }
    issues
}

/// Starts the engine if it is not already answering. Tries the systemd user
/// unit first, then spawns a detached `serve` as a fallback.
pub async fn ensure_engine(cfg: &Config) -> anyhow::Result<bool> {
    if let Some(info) = engine_info(cfg.port).await {
        anyhow::ensure!(health_matches(&info, cfg), "an engine is running with a different configuration/version; stop and restart it before connecting");
        return Ok(true);
    }
    if !systemd::start() {
        spawn_detached_serve(cfg);
    }
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if engine_matches(cfg).await {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(unix)]
pub fn spawn_detached_serve(cfg: &Config) {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = std::process::Command::new(exe)
        .args(["serve", "--quiet", "--port", &cfg.port.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
}

#[cfg(windows)]
pub fn spawn_detached_serve(cfg: &Config) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    const DETACHED_PROCESS: u32 = 0x00000008;
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = std::process::Command::new(exe)
        .args(["serve", "--quiet", "--port", &cfg.port.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn();
}

#[cfg(not(any(unix, windows)))]
pub fn spawn_detached_serve(cfg: &Config) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = std::process::Command::new(exe)
        .args(["serve", "--quiet", "--port", &cfg.port.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// `shrinkgate serve --ensure` command line used by Claude Code's SessionStart
/// hook: exits immediately when healthy, otherwise starts the engine.
pub async fn ensure_command(cfg: &Config) -> i32 {
    match ensure_engine(cfg).await {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            eprintln!("shrinkgate ensure: {e:#}");
            1
        }
    }
}

/// Dotted-path get/set/remove on a settings JSON document.
/// `env.ANTHROPIC_BASE_URL`, `statusLine`, `hooks.SessionStart`, ...
fn split_path(key: &str) -> Vec<String> {
    key.split('.').map(|s| s.to_string()).collect()
}

pub fn lookup<'a>(doc: &'a Value, key: &str) -> Option<&'a Value> {
    let mut cur = doc;
    for part in split_path(key) {
        cur = cur.get(&part)?;
    }
    Some(cur)
}

fn lookup_mut<'a>(doc: &'a mut Value, key: &str) -> Option<&'a mut Value> {
    let mut cur = doc;
    for part in split_path(key) {
        cur = cur.get_mut(&part)?;
    }
    Some(cur)
}

pub(crate) fn set_path(doc: &mut Value, key: &str, value: Value) {
    let parts = split_path(key);
    let mut cur = doc;
    for (i, part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            if !cur.is_object() {
                *cur = Value::Object(serde_json::Map::new());
            }
            cur.as_object_mut().unwrap().insert(part.clone(), value);
            return;
        }
        if !cur.is_object() {
            *cur = Value::Object(serde_json::Map::new());
        }
        cur = cur
            .as_object_mut()
            .unwrap()
            .entry(part.clone())
            .or_insert(Value::Object(serde_json::Map::new()));
    }
}

pub(crate) fn remove_path(doc: &mut Value, key: &str) -> bool {
    let parts = split_path(key);
    let mut cur = doc;
    for (i, part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            return cur
                .as_object_mut()
                .map(|o| o.remove(part).is_some())
                .unwrap_or(false);
        }
        match cur.get_mut(part) {
            Some(next) => cur = next,
            None => return false,
        }
    }
    false
}

/// Restores every key in `state.written` that still holds our value.
/// Returns the list of leftovers (keys the user changed since connect).
pub fn restore_written(settings: &mut Value, state: &Value) -> Vec<String> {
    let mut leftovers = Vec::new();
    let Some(written) = state.get("written").and_then(Value::as_object) else {
        return leftovers;
    };
    let previous = state.get("previous").cloned().unwrap_or(Value::Null);
    for (key, our_value) in written {
        let ours_now = lookup(settings, key);
        if ours_now != Some(our_value) {
            leftovers.push(key.clone());
            continue;
        }
        match previous.get(key) {
            Some(Value::Null) | None => {
                remove_path(settings, key);
            }
            Some(old) => set_path(settings, key, old.clone()),
        }
    }
    leftovers
}

/// True when no agent wiring state files remain (safe to remove the service).
pub fn no_other_wires(except: Option<&str>) -> bool {
    if except != Some("muse") && crate::paths::config_dir().join("muse.env").exists() {
        return false;
    }
    for agent in ["claude", "codex", "opencode"] {
        if Some(agent) == except {
            continue;
        }
        if wire_state_file(agent).exists() {
            return false;
        }
    }
    true
}

pub fn now_iso() -> String {
    let (y, m, d) = crate::store::utc_ymd(crate::store::now_ms());
    format!("{y:04}-{m:02}-{d:02}T00:00:00Z")
}
