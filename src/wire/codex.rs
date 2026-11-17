//! Codex wiring: a managed provider block in `~/.codex/config.toml` pointing
//! the Responses API at the local engine. ChatGPT sign-in is kept
//! (`requires_openai_auth = true`), or an env key via `--env-key`.

use serde_json::json;
use toml_edit::{value, DocumentMut, Item, Table};

use crate::config::Config;
use crate::paths;
use crate::wire::{
    ensure_bin, ensure_engine, now_iso, read_json_file, wire_state_file, write_json_private,
};

pub const PROVIDER_ID: &str = "shrinkgate_local";
const START: &str = "# --- ShrinkGate local engine ---";
const END: &str = "# --- end ShrinkGate local engine ---";

fn refuse(msg: &str) -> anyhow::Error {
    anyhow::anyhow!("{msg} No settings were changed.")
}

fn config_path() -> std::path::PathBuf {
    paths::codex_dir().join("config.toml")
}

pub async fn connect(cfg: &Config, force: bool, env_key: Option<String>) -> anyhow::Result<()> {
    ensure_bin()?;
    if !ensure_engine(cfg).await? {
        return Err(refuse("the ShrinkGate engine did not start"));
    }
    let path = config_path();
    let original = std::fs::read_to_string(&path).ok();
    let text = original.clone().unwrap_or_default();
    let mut doc = text
        .parse::<DocumentMut>()
        .map_err(|_| refuse("config.toml is not valid TOML"))?;

    if let Some(mp) = doc.get("model_provider").and_then(|i| i.as_str()) {
        if mp != "openai" && mp != PROVIDER_ID && !force {
            return Err(refuse(&format!(
                "model_provider is already \"{mp}\" (another provider manages Codex). Disconnect it first, or add --force."
            )));
        }
    }
    if doc.get("openai_base_url").is_some() {
        return Err(refuse(
            "openai_base_url is set; existing proxy routing needs to be resolved first.",
        ));
    }
    if doc.contains_key("profile") || doc.contains_key("profiles") {
        return Err(refuse(
            "profiles are present; resolve profile routing first.",
        ));
    }

    doc["model_provider"] = value(PROVIDER_ID);
    if doc.get("model_providers").is_none() {
        doc["model_providers"] = Item::Table(Table::new());
    }
    let mut t = Table::new();
    t["name"] = value("ShrinkGate (local engine)");
    t["base_url"] = value(format!("{}/v1", cfg.engine_url()));
    t["wire_api"] = value("responses");
    match &env_key {
        Some(k) => {
            t["env_key"] = value(k.clone());
        }
        None => {
            t["requires_openai_auth"] = value(true);
        }
    }
    doc["model_providers"][PROVIDER_ID] = Item::Table(t);
    let written = format!("{START}\n{doc}{END}\n");

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, &written)?;
    write_json_private(
        &wire_state_file("codex"),
        &json!({
            "version": 1,
            "connected_at": now_iso(),
            "engine_url": cfg.engine_url(),
            "original": original,
            "written": written,
            "env_key": env_key,
        }),
    )?;

    println!(
        "Connected. Codex now sends its Responses-API requests through ShrinkGate ({}).",
        cfg.engine_url()
    );
    if env_key.is_none() {
        println!(
            "Your ChatGPT sign-in is unchanged; ShrinkGate forwards it to the ChatGPT backend."
        );
    } else {
        println!("Set the key environment variable before launching codex.");
    }
    println!("Start a new codex session to use the connection.");
    Ok(())
}

pub fn status() -> String {
    let path = config_path();
    let Some(state) = read_json_file(&wire_state_file("codex")) else {
        return "codex: not connected".into();
    };
    match std::fs::read_to_string(&path) {
        Ok(cur) if cur == state.get("written").and_then(|v| v.as_str()).unwrap_or("") => {
            let base = state
                .get("engine_url")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("codex: connected through the ShrinkGate engine at {base}")
        }
        Ok(_) => {
            "codex: config.toml changed since connect; run `shrinkgate uninstall codex` to restore"
                .into()
        }
        Err(_) => "codex: config.toml missing".into(),
    }
}

pub fn uninstall() -> anyhow::Result<bool> {
    let state_file = wire_state_file("codex");
    let Some(state) = read_json_file(&state_file) else {
        println!("Not connected by ShrinkGate. Nothing to restore for Codex.");
        return Ok(crate::wire::no_other_wires(None));
    };
    let path = config_path();
    let written = state.get("written").and_then(|v| v.as_str()).unwrap_or("");
    let original = state
        .get("original")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let cur = std::fs::read_to_string(&path).unwrap_or_default();
    if cur != written {
        println!("Codex routing changed since connect. Files kept; resolve the managed provider before retrying.");
        return Ok(false);
    }
    match original {
        Some(text) => std::fs::write(&path, text)?,
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
    let _ = std::fs::remove_file(&state_file);
    if crate::wire::no_other_wires(Some("codex")) {
        crate::wire::systemd::remove();
    }
    println!("Restored Codex config. Start a new session.");
    Ok(true)
}
