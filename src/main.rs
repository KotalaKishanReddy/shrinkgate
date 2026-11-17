//! ShrinkGate — a local context-shrinking gateway for coding agents.
//!
//! Rebuilt from scratch for Linux after studying the public (Apache-2.0)
//! connector of ContextShrink Code: a local engine on 127.0.0.1 that coding
//! agents address as their model API, trimming content they resend every turn
//! before forwarding to the real upstream with the agent's own auth.

mod benchmark;
mod config;
mod demo;
mod diagnostics;
mod paths;
mod server;
mod shrink;
mod statusline;
mod store;
mod upstream;
mod wire;

use clap::{Parser, Subcommand};
use config::Config;

#[derive(Parser)]
#[command(
    name = "shrinkgate",
    version,
    about = "Local context-shrinking gateway for coding agents (Claude Code, Codex, OpenCode, Muse Code, ...)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the gateway engine (usually as a systemd user service).
    Serve {
        #[arg(long, env = "SHRINKGATE_PORT", value_parser = clap::value_parser!(u16).range(1..))]
        port: Option<u16>,
        /// Used by the SessionStart hook: start the engine if it is not running.
        #[arg(long)]
        ensure: bool,
        #[arg(long)]
        quiet: bool,
        #[arg(long = "no-shrink")]
        no_shrink: bool,
    },
    /// Wire a coding agent to the local engine.
    Connect {
        /// claude | codex | opencode | muse
        agent: String,
        /// Preview the exact proposed agent configuration without writing files.
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
        /// Claude Code: cap the auto-compact window at 200k tokens.
        #[arg(long)]
        lean: bool,
        /// Claude Code: wire the statusline (savings per day).
        #[arg(long)]
        statusline: bool,
        #[arg(long = "no-service")]
        no_service: bool,
        /// Codex: use an env-var API key instead of the ChatGPT login.
        #[arg(long)]
        env_key: Option<String>,
    },
    /// Show connection status, engine health and savings.
    Status { agent: Option<String> },
    /// Diagnose local engine identity, configuration and agent wiring (no API keys sent).
    Doctor,
    /// Local estimated reductions and provider-reported usage (never a billing guarantee).
    Stats {
        #[arg(long)]
        json: bool,
    },
    /// Undo a connect: restore the agent's original settings.
    Uninstall {
        agent: String,
        /// Remove binary/state after every integration is disconnected and the engine is stopped.
        #[arg(long)]
        purge: bool,
    },
    /// Claude Code auto-compact window: lean on|off|status.
    Lean {
        action: String,
        #[arg(long)]
        force: bool,
    },
    /// Print shell exports for manual wiring of any other agent.
    Env,
    /// Print the resolved configuration and paths.
    Config,
    /// One line for Claude Code's statusline: savings today.
    Statusline,
    /// End-to-end demo against a mock upstream (no keys needed).
    Demo,
    /// Benchmark local transformations on synthetic fixtures or one private request JSON.
    Benchmark {
        #[arg(long)]
        input: Option<std::path::PathBuf>,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=10000))]
        iterations: u16,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let cfg = match Config::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("shrinkgate configuration: {e:#}");
            std::process::exit(1);
        }
    };
    let cfg = match &cli.cmd {
        Cmd::Serve {
            port, no_shrink, ..
        } => {
            let mut c = cfg;
            if let Some(p) = port {
                c.port = *p;
            }
            if *no_shrink {
                c.shrink = false;
            }
            c
        }
        _ => cfg,
    };
    let code = match &cli.cmd {
        Cmd::Serve { ensure, quiet, .. } => {
            if *ensure {
                wire::ensure_command(&cfg).await
            } else if let Err(e) = server::run(cfg, *quiet).await {
                eprintln!("shrinkgate: {e:#}");
                1
            } else {
                0
            }
        }
        Cmd::Connect {
            agent,
            force,
            lean,
            statusline,
            no_service,
            env_key,
            dry_run,
        } => match connect_agent(
            &cfg,
            agent,
            *force,
            (*lean, *statusline),
            *no_service,
            env_key.as_deref(),
            *dry_run,
        )
        .await
        {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("shrinkgate connect: {e:#}");
                1
            }
        },
        Cmd::Status { agent } => {
            print_status(&cfg, agent.as_deref()).await;
            0
        }
        Cmd::Doctor => diagnostics::doctor(&cfg).await,
        Cmd::Stats { json } => {
            diagnostics::stats(*json);
            0
        }
        Cmd::Uninstall { agent, purge } => match uninstall_agent(agent, *purge).await {
            Ok(ok) => {
                if ok {
                    0
                } else {
                    1
                }
            }
            Err(e) => {
                eprintln!("shrinkgate uninstall: {e:#}");
                1
            }
        },
        Cmd::Lean { action, force } => match (|| {
            let _lock = wire::config_lock()?;
            wire::claude::lean(action, *force)
        })() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("shrinkgate lean: {e:#}");
                1
            }
        },
        Cmd::Env => {
            print_env(&cfg);
            0
        }
        Cmd::Config => {
            print_config(&cfg);
            0
        }
        Cmd::Statusline => {
            println!("{}", statusline::run());
            0
        }
        Cmd::Benchmark { input, iterations } => {
            match benchmark::run(input.clone(), *iterations as usize) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("benchmark: {e:#}");
                    1
                }
            }
        }
        Cmd::Demo => match demo::run().await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("shrinkgate demo: {e:#}");
                1
            }
        },
    };
    std::process::exit(code);
}

async fn connect_agent(
    cfg: &Config,
    agent: &str,
    force: bool,
    claude_options: (bool, bool),
    no_service: bool,
    env_key: Option<&str>,
    dry_run: bool,
) -> anyhow::Result<()> {
    let (lean, statusline) = claude_options;
    let _lock = if dry_run {
        None
    } else {
        Some(wire::config_lock()?)
    };
    match agent {
        "claude" => wire::claude::connect(cfg, force, lean, statusline, no_service, dry_run).await,
        "codex" => {
            wire::codex::connect(
                cfg,
                force,
                env_key.map(|s| s.to_string()),
                dry_run,
                no_service,
            )
            .await
        }
        "opencode" => wire::opencode::connect(cfg, force, dry_run, no_service).await,
        "muse" => wire::muse::connect(cfg, dry_run).await,
        other => Err(anyhow::anyhow!(
            "unknown agent {other:?} (claude | codex | opencode | muse). \
             For anything else, run `shrinkgate env` and point the agent's base URL at the engine."
        )),
    }
}

async fn uninstall_agent(agent: &str, purge: bool) -> anyhow::Result<bool> {
    let _lock = wire::config_lock()?;
    let cfg = Config::load()?;
    let restored = match agent {
        "claude" => wire::claude::uninstall(&cfg),
        "codex" => wire::codex::uninstall(),
        "opencode" => wire::opencode::uninstall(),
        "muse" => wire::muse::uninstall(),
        other => Err(anyhow::anyhow!("unknown agent {other:?}")),
    }?;
    if restored && purge {
        anyhow::ensure!(
            wire::no_other_wires(None),
            "settings restored; other integrations remain connected, so binary/state were kept"
        );
        wire::systemd::remove();
        let active = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            tokio::net::TcpStream::connect(("127.0.0.1", cfg.port)),
        )
        .await;
        anyhow::ensure!(!matches!(active, Ok(Ok(_))), "settings restored; an engine is still listening. Stop it before purging the binary/state");
        if paths::bin_install_path().exists() {
            std::fs::remove_file(paths::bin_install_path())?;
        }
        // Keep the held lock file; Windows does not allow unlinking a locked file.
        for entry in std::fs::read_dir(paths::state_dir())? {
            let entry = entry?;
            if entry.file_name() == "config.lock" {
                continue;
            }
            if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(entry.path())?;
            } else {
                std::fs::remove_file(entry.path())?;
            }
        }
        println!("Purged binary and local state (configuration lock retained).");
    }
    Ok(restored)
}

async fn print_status(cfg: &Config, agent: Option<&str>) {
    let healthy = wire::engine_healthy(cfg.port).await;
    println!(
        "engine: {} at {} (shrink: {})",
        if healthy { "running" } else { "NOT running" },
        cfg.engine_url(),
        cfg.shrink
    );
    let (today_s, today_n, all_s, all_n) = store::totals(&paths::state_dir());
    println!("estimated reductions (UTC): −{today_s} tok today ({today_n} req) · −{all_s} est. tok retained ({all_n} req)");
    match agent {
        Some("claude") => println!("{}", wire::claude::status(cfg)),
        Some("codex") => println!("{}", wire::codex::status()),
        Some("opencode") => println!("{}", wire::opencode::status()),
        Some("muse") => println!("{}", wire::muse::status()),
        Some(other) => println!("unknown agent {other:?}"),
        None => {
            println!("{}", wire::claude::status(cfg));
            println!("{}", wire::codex::status());
            println!("{}", wire::opencode::status());
            println!("{}", wire::muse::status());
            if wire::service::installed() {
                println!(
                    "service: {} installed (starts at login)",
                    wire::service::description()
                );
            } else {
                println!("service: not installed (SessionStart hook / detached fallback)");
            }
        }
    }
}

fn print_env(cfg: &Config) {
    println!("# ShrinkGate engine at {}", cfg.engine_url());
    println!("export ANTHROPIC_BASE_URL={}", cfg.engine_url());
    println!("#   ^ Claude Code and any Anthropic-format agent (auth forwarded as-is)");
    println!("export OPENAI_BASE_URL={}/v1", cfg.engine_url());
    println!("#   ^ OpenAI-compatible agents (Chat Completions / Responses)");
    println!(
        "# Codex config.toml: base_url = \"{}/v1\", wire_api = \"responses\"",
        cfg.engine_url()
    );
    println!(
        "# OpenCode: provider options.baseURL = \"{}/v1\"",
        cfg.engine_url()
    );
}

fn print_config(cfg: &Config) {
    println!("config file: {}", paths::config_file().display());
    println!("state dir:   {}", paths::state_dir().display());
    println!("port:                {}", cfg.port);
    println!("shrink:              {}", cfg.shrink);
    println!("min_block_bytes:     {}", cfg.min_block_bytes);
    println!("keep_last_messages:  {}", cfg.keep_last_messages);
    println!("allow_assistant_text:{}", cfg.allow_assistant_text);
    println!("allow_user_text:     {}", cfg.allow_user_text);
    println!("upstream_anthropic:  {}", cfg.upstream_anthropic);
    println!("upstream_openai:     {}", cfg.upstream_openai);
    println!("upstream_meta:       {}", cfg.upstream_meta);
    println!("upstream_opencode:   {}", cfg.upstream_opencode);
    println!("upstream_chatgpt:    {}", cfg.upstream_chatgpt);
    println!("default_upstream:    {}", cfg.default_upstream);
}
