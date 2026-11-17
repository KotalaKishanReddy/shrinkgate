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
    let healthy = wire::engine_healthy(cfg.port).await;
    println!(
        "engine identity/version: {} at {}",
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
    if healthy {
        0
    } else {
        1
    }
}
