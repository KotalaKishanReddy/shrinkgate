//! systemd user service: the Linux equivalent of the original's Startup
//! shortcut — the engine starts at login and restarts on failure.

use std::path::Path;

const UNIT: &str = "\
[Unit]
Description=ShrinkGate local context-shrinking gateway
After=network.target

[Service]
ExecStart=%h/.local/bin/shrinkgate serve --quiet
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
";

fn systemctl(args: &[&str]) -> bool {
    std::process::Command::new("systemctl")
        .args(["--user"])
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn unit_path() -> std::path::PathBuf {
    crate::paths::systemd_unit_path()
}

pub fn installed() -> bool {
    unit_path().exists()
}

/// Writes the unit file and enables + starts it. Returns false when systemd
/// user services are unavailable (fallback: detached process).
pub fn install(_bin: &Path) -> bool {
    let path = unit_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&path, UNIT).is_err() {
        return false;
    }
    systemctl(&["daemon-reload"]) && systemctl(&["enable", "--now", "shrinkgate.service"])
}

/// Starts the service if installed. Returns true when the service was
/// started (or was already running).
pub fn start() -> bool {
    if !installed() {
        return false;
    }
    systemctl(&["start", "shrinkgate.service"])
}

/// Stops, disables and removes the unit file. Best-effort.
pub fn remove() {
    if !installed() {
        return;
    }
    let _ = systemctl(&["disable", "--now", "shrinkgate.service"]);
    let _ = std::fs::remove_file(unit_path());
    let _ = systemctl(&["daemon-reload"]);
}
