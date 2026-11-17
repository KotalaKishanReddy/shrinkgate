//! OpenCode wiring: custom providers in `~/.config/opencode/opencode.json`
//! pointing the Anthropic SDK, the Responses adapter and the Chat Completions
//! adapter at the local engine. API keys stay in env vars (`{env:...}`) or in
//! OpenCode's own credential store (`opencode auth login`).

use serde_json::{json, Value};

use crate::config::Config;
use crate::paths;
use crate::wire::{ensure_bin, ensure_engine, now_iso, read_json_file, wire_state_file};

fn refuse(msg: &str) -> anyhow::Error {
    anyhow::anyhow!("{msg} No settings were changed.")
}

fn config_path() -> std::path::PathBuf {
    paths::opencode_config()
}

fn provider_block(cfg: &Config) -> Value {
    let base = format!("{}/v1", cfg.engine_url());
    json!({
        "shrinkgate": {
            "npm": "@ai-sdk/anthropic",
            "name": "ShrinkGate (Anthropic)",
            "options": { "baseURL": base, "apiKey": "{env:ANTHROPIC_API_KEY}" },
            "models": {
                "claude-sonnet-4-5": { "name": "Claude Sonnet 4.5" },
                "claude-opus-4-1": { "name": "Claude Opus 4.1" },
                "claude-haiku-4-5": { "name": "Claude Haiku 4.5" }
            }
        },
        "shrinkgate-openai": {
            "npm": "@ai-sdk/openai",
            "name": "ShrinkGate (OpenAI Responses)",
            "options": { "baseURL": base, "apiKey": "{env:OPENAI_API_KEY}" },
            "models": {
                "gpt-5-codex": { "name": "GPT-5 Codex" },
                "gpt-5": { "name": "GPT-5" }
            }
        },
        "shrinkgate-meta": {
            "npm": "@ai-sdk/openai",
            "name": "ShrinkGate (Meta Muse)",
            "options": { "baseURL": base, "apiKey": "{env:MODEL_API_KEY}", "headers": {"x-shrinkgate-upstream": "meta"} },
            "models": {
                "muse-spark-1.3": { "name": "Muse Spark 1.3" },
                "muse-wonder-1.0": { "name": "Muse Wonder 1.0" }
            }
        },
        "shrinkgate-opencode": {
            "npm": "@ai-sdk/openai",
            "name": "ShrinkGate (OpenCode Zen)",
            "options": {
                "baseURL": base,
                "apiKey": "{env:OPENCODE_API_KEY}",
                "headers": { "x-shrinkgate-upstream": "opencode" }
            },
            "models": {
                "glm-5.3-flash-free": { "name": "GLM 5.3 Flash (via ShrinkGate)" },
                "deepseek-v4.1-flash-free": { "name": "DeepSeek V4.1 Flash Free (via ShrinkGate)" },
                "flint-40b-free": { "name": "Flint 40B Free (via ShrinkGate)" },
                "beam-free": { "name": "Beam Free (via ShrinkGate)" }
            }
        }
    })
}

pub async fn connect(
    cfg: &Config,
    force: bool,
    dry_run: bool,
    no_service: bool,
) -> anyhow::Result<()> {
    let path = config_path();
    if crate::wire::already_connected("opencode", &path)? {
        return Ok(());
    }
    let original = crate::wire::read_optional(&path)?;
    let mut doc: Value = match &original {
        Some(text) => {
            serde_json::from_str(text).map_err(|_| refuse("opencode.json is not valid JSON"))?
        }
        None => json!({"$schema": "https://opencode.ai/config.json"}),
    };
    if !doc.is_object() {
        return Err(refuse("opencode.json is not a JSON object"));
    }
    let provider_key = if doc.get("provider").is_some() {
        "provider"
    } else if doc.get("providers").is_some() {
        "providers"
    } else {
        "provider"
    };
    if let Some(providers) = doc.get(provider_key) {
        if !providers.is_object() {
            return Err(refuse("provider must be a JSON object"));
        }
        if !force
            && provider_block(cfg)
                .as_object()
                .unwrap()
                .keys()
                .any(|k| providers.get(k).is_some())
        {
            return Err(refuse(
                "a ShrinkGate provider already exists; use --force to replace",
            ));
        }
    }
    let block = provider_block(cfg);
    let providers = doc
        .as_object_mut()
        .unwrap()
        .entry(provider_key.to_string())
        .or_insert(json!({}));
    for (k, v) in block.as_object().unwrap() {
        providers
            .as_object_mut()
            .unwrap()
            .insert(k.clone(), v.clone());
    }
    let written = serde_json::to_string_pretty(&doc)? + "\n";
    if dry_run {
        crate::wire::preview(&path, &written);
        return Ok(());
    }
    let bin = ensure_bin()?;
    if !no_service {
        crate::wire::systemd::install(&bin);
    }
    if !ensure_engine(cfg).await? {
        return Err(refuse("the ShrinkGate engine did not start"));
    }
    crate::wire::persist_connection(
        "opencode",
        &path,
        &written,
        &json!({
            "version": 1,
            "connected_at": now_iso(),
            "engine_url": cfg.engine_url(),
            "original": original,
            "written": written,
        }),
    )?;

    println!(
        "Connected. OpenCode now offers ShrinkGate providers through the local engine ({}).",
        cfg.engine_url()
    );
    println!(
        "Pick a model like shrinkgate/claude-sonnet-4-5, or store keys via `opencode auth login`."
    );
    println!("Add more model IDs under models in opencode.json as needed. Restart OpenCode.");
    Ok(())
}

pub fn status() -> String {
    let Some(state) = read_json_file(&wire_state_file("opencode")) else {
        return "opencode: not connected".into();
    };
    match std::fs::read_to_string(config_path()) {
        Ok(cur) if cur == state.get("written").and_then(|v| v.as_str()).unwrap_or("") => {
            let base = state.get("engine_url").and_then(|v| v.as_str()).unwrap_or("?");
            format!("opencode: connected through the ShrinkGate engine at {base}")
        }
        Ok(_) => "opencode: opencode.json changed since connect; run `shrinkgate uninstall opencode` to restore".into(),
        Err(_) => "opencode: opencode.json missing".into(),
    }
}

pub fn uninstall() -> anyhow::Result<bool> {
    let state_file = wire_state_file("opencode");
    let Some(state) = crate::wire::read_state("opencode")? else {
        println!("Not connected by ShrinkGate. Nothing to restore for OpenCode.");
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
        println!("OpenCode config changed since connect. Files kept; retry after resolving.");
        return Ok(false);
    }
    match original {
        Some(text) => crate::wire::atomic_write(&path, text.as_bytes())?,
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
    let _ = std::fs::remove_file(&state_file);
    if crate::wire::no_other_wires(Some("opencode")) {
        crate::wire::systemd::remove();
    }
    println!("Restored OpenCode config. Restart OpenCode.");
    Ok(true)
}
