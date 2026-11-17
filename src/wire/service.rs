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

    pub fn installed() -> bool {
        service_file_path().exists()
    }

    /// Writes the unit file and enables + starts it. Returns false when systemd
    /// user services are unavailable (fallback: detached process).
    pub fn install(_bin: &Path) -> bool {
        let path = service_file_path();
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

    pub fn installed() -> bool {
        service_file_path().exists()
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
            bin = bin_str,
            err = err_log.to_string_lossy(),
            out = out_log.to_string_lossy()
        );

        if std::fs::write(&path, plist).is_err() {
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

    pub fn start() -> bool {
        if !installed() {
            return false;
        }
        std::process::Command::new("launchctl")
            .args(["start", "com.shrinkgate.engine"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    pub fn remove() {
        if !installed() {
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
    pub fn installed() -> bool {
        false
    }
    pub fn install(_bin: &Path) -> bool {
        false
    }
    pub fn start() -> bool {
        false
    }
    pub fn remove() {}
    #[cfg(target_os = "windows")]
    pub fn description() -> &'static str {
        "detached background process (Windows)"
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    pub fn description() -> &'static str {
        "unsupported background service"
    }
}

pub use platform::*;
