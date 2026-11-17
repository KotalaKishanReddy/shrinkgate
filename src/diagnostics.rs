//! Local-only diagnostics; never sends provider credentials or prompts.
use crate::{config::Config, paths, store, wire};

pub fn stats(json: bool) {
    let summary = store::summary(&paths::state_dir());
    if json {
        println!("{}", summary);
    } else {
        println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    }
}

pub async fn doctor(cfg: &Config) -> i32 {
    println!("ShrinkGate {}", env!("CARGO_PKG_VERSION"));
    println!("configuration: valid ({})", paths::config_file().display());
    println!("state: {}", paths::state_dir().display());
    let info = wire::engine_info(cfg.port).await;
    let healthy = info.as_ref().is_some_and(|v| wire::health_matches(v, cfg));
    let dropped = info
        .as_ref()
        .and_then(|v| v["journal_dropped_records"].as_u64())
        .unwrap_or(0);
    if let Some(ref v) = info {
        println!(
            "active engine: shrink={}, journal dropped metadata={dropped}",
            v["shrink"]
        );
    }
    let mut issues = wire::wiring_issues();
    if let Some(problem) = wire::service::ownership_issue() {
        issues.push(problem);
    }
    if dropped > 0 {
        issues.push(format!(
            "usage metadata was lost: {dropped} journal records dropped"
        ));
    }
    if paths::config_dir().join("muse.env").exists() && cfg.default_upstream != "meta" {
        issues.push("Muse helper exists but default_upstream is not meta".into());
    }
    println!(
        "engine identity/version/configuration: {} at {}",
        if healthy { "OK" } else { "FAILED" },
        cfg.engine_url()
    );
    println!("{}", wire::claude::status(cfg));
    println!("{}", wire::codex::status());
    println!("{}", wire::opencode::status());
    println!("{}", wire::muse::status());
    println!(
        "service: {} ({})",
        if wire::service::installed() {
            "installed"
        } else {
            "not installed"
        },
        wire::service::description()
    );
    println!("upstreams: configuration validated; network/authentication not probed (no credentials sent)");
    println!("If the engine check failed: run `shrinkgate serve` in a terminal and inspect the error. Restart after changing configuration.");
    for issue in &issues {
        println!("FAILED: {issue}");
    }
    if healthy && issues.is_empty() {
        0
    } else {
        1
    }
}
