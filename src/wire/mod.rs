//! Agent wiring: point coding agents at the local gateway, record what was
//! changed, restore it on uninstall. Mirrors the stateful-restore design of
//! the original connector (previous values recorded, restored only while the
//! file still holds what we wrote).

pub mod claude;
pub mod codex;
pub mod muse;
pub mod opencode;
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

pub fn write_json_private(path: &Path, v: &Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(v)? + "\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Copies the running binary to `~/.local/bin/shrinkgate` so hooks and the
/// systemd unit have a stable path.
pub fn ensure_bin() -> anyhow::Result<PathBuf> {
    let dst = crate::paths::bin_install_path();
    let src = std::env::current_exe()?;
    if src.canonicalize().ok() != dst.canonicalize().ok() {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = dst.with_extension("tmp");
        std::fs::copy(&src, &tmp)?;
        std::fs::rename(&tmp, &dst)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755));
    }
    Ok(dst)
}

/// GET /healthz on the local engine. Short timeout.
pub async fn engine_healthy(port: u16) -> bool {
    let clt = crate::upstream::make_client();
    let url = format!("http://127.0.0.1:{port}/healthz");
    let req = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri(url)
        .body(crate::upstream::full(bytes::Bytes::new()));
    match req {
        Ok(req) => {
            let fut = clt.request(req);
            match tokio::time::timeout(std::time::Duration::from_millis(700), fut).await {
                Ok(Ok(resp)) => resp.status().is_success(),
                _ => false,
            }
        }
        Err(_) => false,
    }
}

/// Starts the engine if it is not already answering. Tries the systemd user
/// unit first, then spawns a detached `serve` as a fallback.
pub async fn ensure_engine(cfg: &Config) -> anyhow::Result<bool> {
    if engine_healthy(cfg.port).await {
        return Ok(true);
    }
    if !systemd::start() {
        spawn_detached_serve();
    }
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if engine_healthy(cfg.port).await {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn spawn_detached_serve() {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = std::process::Command::new(exe)
        .args(["serve", "--quiet"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
}

/// `shrinkgate serve --ensure` command line used by Claude Code's SessionStart
/// hook: exits immediately when healthy, otherwise starts the engine.
pub async fn ensure_command(cfg: &Config) -> i32 {
    if engine_healthy(cfg.port).await {
        return 0;
    }
    if !systemd::start() {
        spawn_detached_serve();
    }
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if engine_healthy(cfg.port).await {
            return 0;
        }
    }
    1
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
