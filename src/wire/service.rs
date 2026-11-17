//! Background service manager abstraction for systemd (Linux) and launchd (macOS).

use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

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

    pub fn service_file_path() -> PathBuf {
        crate::paths::systemd_unit_path()
    }

    fn file_exists() -> bool {
        service_file_path().exists()
    }

    /// Writes the unit file and enables + starts it. Returns false when systemd
    /// user services are unavailable (fallback: detached process).
    pub fn install(_bin: &Path) -> bool {
        let path = service_file_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = super::write_managed(&path, UNIT) {
            eprintln!("shrinkgate: service kept: {e:#}");
            return false;
        }
        systemctl(&["daemon-reload"]) && systemctl(&["enable", "--now", "shrinkgate.service"])
    }

    /// Starts the service if installed. Returns true when the service was
    /// started (or was already running).
    pub fn start_command() -> bool {
        if !file_exists() {
            return false;
        }
        systemctl(&["start", "shrinkgate.service"])
    }

    /// Stops, disables and removes the unit file. Best-effort.
    pub fn remove_command() {
        if !file_exists() {
            return;
        }
        let _ = systemctl(&["disable", "--now", "shrinkgate.service"]);
        let _ = std::fs::remove_file(service_file_path());
        let _ = systemctl(&["daemon-reload"]);
    }

    pub fn description() -> &'static str {
        "systemd user unit (Linux)"
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    pub fn service_file_path() -> PathBuf {
        crate::paths::launchd_plist_path()
    }

    fn file_exists() -> bool {
        service_file_path().exists()
    }

    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\"', "&quot;")
            .replace('\'', "&apos;")
    }

    /// Generates launchd LaunchAgent plist and loads it with `launchctl`.
    pub fn install(bin: &Path) -> bool {
        let path = service_file_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let state_dir = crate::paths::state_dir();
        let _ = std::fs::create_dir_all(&state_dir);
        let bin_str = bin.to_string_lossy();
        let err_log = state_dir.join("launchd.err.log");
        let out_log = state_dir.join("launchd.out.log");

        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.shrinkgate.engine</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
        <string>serve</string>
        <string>--quiet</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardErrorPath</key>
    <string>{err}</string>
    <key>StandardOutPath</key>
    <string>{out}</string>
</dict>
</plist>
"#,
            bin = xml_escape(&bin_str),
            err = xml_escape(&err_log.to_string_lossy()),
            out = xml_escape(&out_log.to_string_lossy())
        );

        if let Err(e) = super::write_managed(&path, &plist) {
            eprintln!("shrinkgate: service kept: {e:#}");
            return false;
        }
        let _ = std::process::Command::new("launchctl")
            .args(["unload", "-w"])
            .arg(&path)
            .status();
        std::process::Command::new("launchctl")
            .args(["load", "-w"])
            .arg(&path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    pub fn start_command() -> bool {
        if !file_exists() {
            return false;
        }
        std::process::Command::new("launchctl")
            .args(["start", "com.shrinkgate.engine"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    pub fn remove_command() {
        if !file_exists() {
            return;
        }
        let path = service_file_path();
        let _ = std::process::Command::new("launchctl")
            .args(["unload", "-w"])
            .arg(&path)
            .status();
        let _ = std::fs::remove_file(&path);
    }

    pub fn description() -> &'static str {
        "launchd user agent (macOS)"
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::*;

    #[allow(dead_code)]
    pub fn service_file_path() -> PathBuf {
        PathBuf::new()
    }
    pub fn install(_bin: &Path) -> bool {
        false
    }
    pub fn start_command() -> bool {
        false
    }
    pub fn remove_command() {}
    #[cfg(target_os = "windows")]
    pub fn description() -> &'static str {
        "detached background process (Windows)"
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    pub fn description() -> &'static str {
        "unsupported background service"
    }
}

pub use platform::{description, service_file_path};

/// Ownership is recorded separately from agent settings. An existing service
/// without this snapshot is user-owned, even if its contents look identical.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_managed(path: &Path, text: &str) -> anyhow::Result<()> {
    let state = crate::wire::read_state("service")?;
    if let Some(ref state) = state {
        anyhow::ensure!(
            state["path"].as_str() == path.to_str(),
            "service recovery path differs; resolve it manually"
        );
    }
    if path.exists() {
        anyhow::ensure!(
            state
                .as_ref()
                .is_some_and(
                    |s| crate::wire::read_optional(path).ok().flatten().as_deref()
                        == s["written"].as_str()
                ),
            "existing or externally edited service is not owned by this connection"
        );
    }
    crate::wire::persist_connection(
        "service",
        path,
        text,
        &serde_json::json!({
            "version": 1, "path": path.to_string_lossy(), "written": text, "original": null,
        }),
    )
}

pub fn ownership_issue() -> Option<String> {
    match crate::wire::read_state("service") {
        Ok(None) => None,
        Ok(Some(state)) => {
            let path = service_file_path();
            if state["path"].as_str() == path.to_str()
                && crate::wire::read_optional(&path).ok().flatten().as_deref()
                    == state["written"].as_str()
            {
                None
            } else {
                Some("managed service changed or is missing; file/recovery data kept".into())
            }
        }
        Err(e) => Some(format!("invalid service recovery state: {e}")),
    }
}

pub fn installed() -> bool {
    crate::wire::wire_state_file("service").exists() && ownership_issue().is_none()
}

pub fn install(bin: &Path) -> bool {
    platform::install(bin)
}
pub fn start() -> bool {
    installed() && platform::start_command()
}
pub fn remove() {
    if !installed() {
        if let Some(problem) = ownership_issue() {
            eprintln!("shrinkgate: {problem}");
        }
        return;
    }
    platform::remove_command();
    if !service_file_path().exists() {
        let _ = std::fs::remove_file(crate::wire::wire_state_file("service"));
    }
}
