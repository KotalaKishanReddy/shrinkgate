//! Muse Code (Meta's CLI agent) wiring — best effort.
//!
//! Muse Code is first-party paired with Meta Model API and "needs no provider
//! config" per Meta's docs. The community has established that it speaks the
//! OpenAI Responses API plus a model-catalog endpoint, and that a base-URL
//! override exists (META_API_KEY + --base-url). ShrinkGate already serves the
//! Responses API and forwards /v1/models, so the gateway is compatible when
//! Muse Code's override is used. This module wires the gateway's `meta`
//! upstream (https://api.meta.ai) and writes a helper env file; it never
//! touches Muse's own state.

use crate::config::Config;
use crate::paths;
use crate::wire::ensure_engine;

fn muse_installed() -> bool {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path_var)
        .any(|dir| dir.join("muse").exists() || dir.join("muse.exe").exists())
}

pub async fn connect(cfg: &Config) -> anyhow::Result<()> {
    if !ensure_engine(cfg).await? {
        return Err(anyhow::anyhow!(
            "the ShrinkGate engine did not start. Nothing was changed."
        ));
    }
    let env_file = paths::config_dir().join("muse.env");
    if let Some(parent) = env_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &env_file,
        format!(
            "# ShrinkGate helper for Muse Code (best-effort).\n\
             # Source this file, then start muse. Your MODEL_API_KEY is unchanged;\n\
             # the gateway forwards it to {meta}.\n\
             export SHRINKGATE_ENGINE={engine}\n\
             # Muse Code is first-party; if your muse build accepts a base-url override:\n\
             #   muse --base-url {engine}/v1\n",
            engine = cfg.engine_url(),
            meta = cfg.upstream_meta,
        ),
    )?;
    println!(
        "Wrote {}. ShrinkGate's Muse upstream is {}.",
        env_file.display(),
        cfg.upstream_meta
    );
    if muse_installed() {
        println!("muse found on PATH.");
    } else {
        println!("muse was not found on PATH; install it first (Meta ships it as a binary).");
    }
    println!(
        "Note: Muse Code has no official provider override; the --base-url route is best-effort."
    );
    Ok(())
}

pub fn status() -> String {
    let env_file = paths::config_dir().join("muse.env");
    if env_file.exists() {
        "muse: helper env written (gateway routes meta upstream); muse itself is first-party".into()
    } else {
        "muse: not connected (run `shrinkgate connect muse`)".into()
    }
}

pub fn uninstall() -> anyhow::Result<bool> {
    let env_file = paths::config_dir().join("muse.env");
    let _ = std::fs::remove_file(&env_file);
    println!("Removed the Muse helper env file (Muse's own config was never touched).");
    Ok(true)
}
