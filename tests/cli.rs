mod common;
use common::Harness;

#[test]
fn reconnect_preserves_originals_and_dry_run_has_no_effects() {
    let mut h = Harness::new();
    for (agent, path, original) in [
        ("codex", "codex/config.toml", "model = \"test-model\"\n"),
        (
            "opencode",
            "config/opencode/opencode.json",
            "{\"model\":\"existing/model\"}",
        ),
        (
            "claude",
            "claude/settings.json",
            "{\"env\":{\"CUSTOM\":\"keep\"}}",
        ),
    ] {
        let p = h.path(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, original).unwrap();
        h.ok(&["connect", agent, "--dry-run", "--no-service"]);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), original);
        assert!(!h.path("state/shrinkgate").exists(), "dry run wrote state");
    }
    h.start();
    for (agent, path) in [
        ("codex", "codex/config.toml"),
        ("opencode", "config/opencode/opencode.json"),
        ("claude", "claude/settings.json"),
    ] {
        let original = std::fs::read_to_string(h.path(path)).unwrap();
        h.ok(&["connect", agent, "--no-service"]);
        let state_path = h.path(&format!("state/shrinkgate/wire-{agent}.json"));
        let backup = std::fs::read(&state_path).unwrap();
        h.ok(&["connect", agent, "--no-service"]);
        assert_eq!(std::fs::read(&state_path).unwrap(), backup);
        h.ok(&["uninstall", agent]);
        let restored = std::fs::read_to_string(h.path(path)).unwrap();
        if agent == "claude" {
            let v: serde_json::Value = serde_json::from_str(&restored).unwrap();
            assert_eq!(v["env"]["CUSTOM"], "keep");
            assert!(v["env"].get("ANTHROPIC_BASE_URL").is_none());
        } else {
            assert_eq!(restored, original);
        }
    }
}

#[test]
fn lean_off_does_not_disconnect_claude_and_refuses_invalid_json() {
    let mut h = Harness::new();
    h.start();
    h.ok(&["connect", "claude", "--lean", "--no-service"]);
    h.ok(&["lean", "off"]);
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(h.path("claude/settings.json")).unwrap()).unwrap();
    assert_eq!(
        v["env"]["ANTHROPIC_BASE_URL"],
        format!("http://127.0.0.1:{}", h.port)
    );
    assert!(v["env"].get("CLAUDE_CODE_AUTO_COMPACT_WINDOW").is_none());
    h.ok(&["uninstall", "claude"]);
    std::fs::write(h.path("claude/settings.json"), "{invalid").unwrap();
    assert!(!h.run(&["lean", "on"]).status.success());
    assert_eq!(
        std::fs::read_to_string(h.path("claude/settings.json")).unwrap(),
        "{invalid"
    );
}

#[test]
fn malformed_configuration_and_provider_shapes_fail_safely() {
    let h = Harness::new();
    std::fs::create_dir_all(h.path("config/opencode")).unwrap();
    std::fs::write(h.path("config/opencode/opencode.json"), "{\"provider\":[]}").unwrap();
    assert!(!h
        .run(&["connect", "opencode", "--dry-run"])
        .status
        .success());
    for text in [
        "port = 0",
        "shrink = 42",
        "port =",
        "upstream_openai = 'http://example.com'",
        "typo = 1",
    ] {
        std::fs::write(h.path("config/shrinkgate/config.toml"), text).unwrap();
        assert!(!h.run(&["config"]).status.success(), "accepted {text}");
    }
}

#[test]
fn doctor_rejects_a_random_http_service_and_stats_are_json() {
    let mut h = Harness::new();
    let listener = h.socket.take().unwrap();
    let worker = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut socket, _) = listener.accept().unwrap();
        let mut b = [0; 4096];
        let _ = socket.read(&mut b);
        let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
    });
    assert!(!h.run(&["doctor"]).status.success());
    worker.join().unwrap();
    let stats: serde_json::Value =
        serde_json::from_slice(&h.ok(&["stats", "--json"]).stdout).unwrap();
    assert_eq!(stats["requests"], 0);
}

#[test]
fn changed_settings_and_corrupt_recovery_are_preserved() {
    let mut h = Harness::new();
    h.start();
    h.ok(&["connect", "codex", "--no-service"]);
    let state = h.path("state/shrinkgate/wire-codex.json");
    let backup = std::fs::read(&state).unwrap();
    std::fs::write(h.path("codex/config.toml"), "model = 'edited'\n").unwrap();
    assert!(!h
        .run(&["connect", "codex", "--no-service"])
        .status
        .success());
    assert!(!h.run(&["uninstall", "codex"]).status.success());
    assert_eq!(std::fs::read(&state).unwrap(), backup);
    std::fs::write(&state, "[]").unwrap();
    assert!(!h.run(&["uninstall", "codex"]).status.success());
    assert_eq!(std::fs::read_to_string(&state).unwrap(), "[]");
}

#[cfg(unix)]
#[test]
fn symlinked_agent_config_is_not_replaced_and_recovery_rolls_back() {
    let mut h = Harness::new();
    h.start();
    std::fs::create_dir_all(h.path("codex")).unwrap();
    std::fs::write(h.path("real.toml"), "model = 'keep'\n").unwrap();
    std::os::unix::fs::symlink(h.path("real.toml"), h.path("codex/config.toml")).unwrap();
    assert!(!h
        .run(&["connect", "codex", "--no-service"])
        .status
        .success());
    assert!(std::fs::symlink_metadata(h.path("codex/config.toml"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(!h.path("state/shrinkgate/wire-codex.json").exists());
    assert_eq!(
        std::fs::read_to_string(h.path("real.toml")).unwrap(),
        "model = 'keep'\n"
    );
}

#[test]
fn purge_keeps_shared_state_and_refuses_a_running_engine() {
    let mut h = Harness::new();
    h.start();
    h.ok(&["connect", "claude", "--no-service"]);
    h.ok(&["connect", "codex", "--no-service"]);
    assert!(!h.run(&["uninstall", "claude", "--purge"]).status.success());
    assert!(h.path("state/shrinkgate/wire-codex.json").exists());
    assert!(!h.run(&["uninstall", "codex", "--purge"]).status.success());
    assert!(
        h.path(".local/bin/shrinkgate").exists() || h.path(".local/bin/shrinkgate.exe").exists()
    );
}
