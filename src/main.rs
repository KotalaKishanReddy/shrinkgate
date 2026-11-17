//! ShrinkGate — a local context-shrinking gateway for coding agents.
//!
//! Rebuilt from scratch for Linux after studying the public (Apache-2.0)
//! connector of ContextShrink Code: a local engine on 127.0.0.1 that coding
//! agents address as their model API, trimming content they resend every turn
//! before forwarding to the real upstream with the agent's own auth.

mod config;
mod demo;
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
        #[arg(long, env = "SHRINKGATE_PORT")]
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
    /// Undo a connect: restore the agent's original settings.
    Uninstall {
        agent: String,
        /// Also stop the engine, remove the service, binary and all state.
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
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let cfg = Config::load();
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
        } => match connect_agent(
            &cfg,
            agent,
            *force,
            *lean,
            *statusline,
            *no_service,
            env_key.as_deref(),
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
            print_status(&cfg, agent.as_deref());
            0
        }
        Cmd::Uninstall { agent, purge } => match uninstall_agent(agent, *purge) {
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
        Cmd::Lean { action, force } => match wire::claude::lean(action, *force) {
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
    lean: bool,
    statusline: bool,
    no_service: bool,
    env_key: Option<&str>,
) -> anyhow::Result<()> {
    match agent {
        "claude" => wire::claude::connect(cfg, force, lean, statusline, no_service).await,
        "codex" => wire::codex::connect(cfg, force, env_key.map(|s| s.to_string())).await,
        "opencode" => wire::opencode::connect(cfg, force).await,
        "muse" => wire::muse::connect(cfg).await,
        other => Err(anyhow::anyhow!(
            "unknown agent {other:?} (claude | codex | opencode | muse). \
             For anything else, run `shrinkgate env` and point the agent's base URL at the engine."
        )),
    }
}

fn uninstall_agent(agent: &str, purge: bool) -> anyhow::Result<bool> {
    let cfg = Config::load();
    match agent {
        "claude" => wire::claude::uninstall(&cfg, purge),
        "codex" => wire::codex::uninstall(),
        "opencode" => wire::opencode::uninstall(),
        "muse" => wire::muse::uninstall(),
        other => Err(anyhow::anyhow!("unknown agent {other:?}")),
    }
}

fn print_status(cfg: &Config, agent: Option<&str>) {
    let healthy = futures_block_on_health(cfg.port);
    println!(
        "engine: {} at {} (shrink: {})",
        if healthy { "running" } else { "NOT running" },
        cfg.engine_url(),
        cfg.shrink
    );
    let (today_s, today_n, all_s, all_n) = store::totals(&paths::state_dir());
    println!("savings: −{today_s} tok today ({today_n} req) · −{all_s} tok all time ({all_n} req)");
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
            if wire::systemd::installed() {
                println!("service: systemd user unit installed (starts at login)");
            } else {
                println!("service: not installed (SessionStart hook / detached fallback)");
            }
        }
    }
}

/// Blocking health probe for the status command.
fn futures_block_on_health(port: u16) -> bool {
    // The CLI is async; use a tiny one-shot runtime-free probe instead.
    std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        std::time::Duration::from_millis(500),
    )
    .is_ok()
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
    println!("upstream_anthropic:  {}", cfg.upstream_anthropic);
    println!("upstream_openai:     {}", cfg.upstream_openai);
    println!("upstream_meta:       {}", cfg.upstream_meta);
    println!("upstream_opencode:   {}", cfg.upstream_opencode);
    println!("upstream_chatgpt:    {}", cfg.upstream_chatgpt);
    println!("default_upstream:    {}", cfg.default_upstream);
}
