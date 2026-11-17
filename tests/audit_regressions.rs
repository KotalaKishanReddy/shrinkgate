mod common;
use common::Harness;
use serde_json::{json, Value};

#[test]
fn each_lean_management_cycle_restores_its_own_original_value() {
    let h = Harness::new();
    h.ok(&["lean", "on"]);
    h.ok(&["lean", "off"]);
    std::fs::write(
        h.path("claude/settings.json"),
        r#"{"env":{"CLAUDE_CODE_AUTO_COMPACT_WINDOW":"123456","CUSTOM":"keep"}}"#,
    )
    .unwrap();
    h.ok(&["lean", "on", "--force"]);
    h.ok(&["lean", "off"]);
    let settings: Value =
        serde_json::from_slice(&std::fs::read(h.path("claude/settings.json")).unwrap()).unwrap();
    assert_eq!(settings["env"]["CLAUDE_CODE_AUTO_COMPACT_WINDOW"], "123456");
    assert_eq!(settings["env"]["CUSTOM"], "keep");
}

#[test]
fn preexisting_identical_hook_survives_connect_and_uninstall() {
    let mut h = Harness::new();
    h.start();
    let bin = h.path(if cfg!(windows) {
        ".local/bin/shrinkgate.exe"
    } else {
        ".local/bin/shrinkgate"
    });
    let command = if cfg!(unix) {
        format!("'{}' serve --ensure", bin.display())
    } else {
        format!("\"{}\" serve --ensure", bin.display())
    };
    let original = json!({"env":{},"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":command}]}]},"custom":42});
    std::fs::create_dir_all(h.path("claude")).unwrap();
    std::fs::write(h.path("claude/settings.json"), original.to_string()).unwrap();
    h.ok(&["connect", "claude", "--no-service"]);
    h.ok(&["uninstall", "claude"]);
    let restored: Value =
        serde_json::from_slice(&std::fs::read(h.path("claude/settings.json")).unwrap()).unwrap();
    assert_eq!(restored, original);
}

#[test]
fn doctor_fails_for_changed_managed_routing_and_missing_owned_hook() {
    let mut h = Harness::new();
    h.start();
    h.ok(&["connect", "codex", "--no-service"]);
    std::fs::write(h.path("codex/config.toml"), "model_provider = 'changed'\n").unwrap();
    let check = h.run(&["doctor"]);
    assert!(!check.status.success());
    assert!(String::from_utf8_lossy(&check.stdout).contains("FAILED: codex"));
    h.ok(&["connect", "claude", "--no-service"]);
    let path = h.path("claude/settings.json");
    let mut settings: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    settings["hooks"]["SessionStart"] = json!([]);
    std::fs::write(path, settings.to_string()).unwrap();
    assert!(
        String::from_utf8_lossy(&h.run(&["doctor"]).stdout).contains("managed SessionStart hook")
    );
}

#[test]
fn disabled_engine_does_not_match_enabled_configuration_or_accept_connect() {
    let mut h = Harness::new();
    h.start_with(&["--no-shrink"]);
    assert!(!h.run(&["doctor"]).status.success());
    let status = h.run(&["status"]);
    assert!(String::from_utf8_lossy(&status.stdout).contains("mismatch"));
    assert!(!h
        .run(&["connect", "codex", "--no-service"])
        .status
        .success());
    assert!(!h.path("codex/config.toml").exists());
    assert!(!h.path("state/shrinkgate/wire-codex.json").exists());
}

#[test]
fn lean_only_state_does_not_claim_claude_is_connected() {
    let mut h = Harness::new();
    h.start();
    h.ok(&["lean", "on"]);
    let status = h.ok(&["status", "claude"]);
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(text.contains("routing not managed"));
    assert!(!text.contains("claude: connected"));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn service_path(h: &Harness) -> std::path::PathBuf {
    h.path(if cfg!(target_os = "linux") {
        "config/systemd/user/shrinkgate.service"
    } else {
        "Library/LaunchAgents/com.shrinkgate.engine.plist"
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn fake_service_manager(h: &Harness) -> std::process::Command {
    use std::os::unix::fs::PermissionsExt;
    let directory = h.path("fake-bin");
    std::fs::create_dir_all(&directory).unwrap();
    let script = directory.join(if cfg!(target_os = "linux") {
        "systemctl"
    } else {
        "launchctl"
    });
    std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut command = h.command();
    let mut directories = vec![directory];
    directories.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    command.env("PATH", std::env::join_paths(directories).unwrap());
    command
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn unmanaged_service_is_never_overwritten_or_deleted() {
    let mut h = Harness::new();
    h.start();
    let path = service_path(&h);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let original = "[Unit]\nDescription=user-owned service\n";
    std::fs::write(&path, original).unwrap();
    assert!(fake_service_manager(&h)
        .args(["connect", "claude"])
        .output()
        .unwrap()
        .status
        .success());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    h.ok(&["uninstall", "claude"]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert!(!h.path("state/shrinkgate/wire-service.json").exists());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn managed_service_drift_is_reported_and_preserved() {
    let mut h = Harness::new();
    h.start();
    assert!(fake_service_manager(&h)
        .args(["connect", "claude"])
        .output()
        .unwrap()
        .status
        .success());
    h.ok(&["doctor"]);
    let path = service_path(&h);
    assert!(h.path("state/shrinkgate/wire-service.json").exists());
    let original = "[Unit]\nDescription=edited after connect\n";
    std::fs::write(&path, original).unwrap();
    assert!(!h.run(&["doctor"]).status.success());
    h.ok(&["uninstall", "claude"]);
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    assert!(h.path("state/shrinkgate/wire-service.json").exists());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn unmodified_owned_service_is_removed_with_its_ownership_snapshot() {
    let mut h = Harness::new();
    h.start();
    assert!(fake_service_manager(&h)
        .args(["connect", "claude"])
        .output()
        .unwrap()
        .status
        .success());
    assert!(service_path(&h).exists());
    assert!(fake_service_manager(&h)
        .args(["uninstall", "claude"])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!service_path(&h).exists());
    assert!(!h.path("state/shrinkgate/wire-service.json").exists());
}
