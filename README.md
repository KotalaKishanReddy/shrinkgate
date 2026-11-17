<div align="center">

# ShrinkGate

**Local context-shrinking gateway for coding agents.**

[![Crates.io](https://img.shields.io/crates/v/shrinkgate?style=flat-square&color=blue&logo=rust)](https://crates.io/crates/shrinkgate)
[![Release](https://img.shields.io/github/v/release/KotalaKishanReddy/shrinkgate?style=flat-square&color=blue&logo=github)](https://github.com/KotalaKishanReddy/shrinkgate/releases)
[![Container](https://img.shields.io/badge/ghcr.io-docker-2496ED?style=flat-square&logo=docker)](https://github.com/KotalaKishanReddy/shrinkgate/pkgs/container/shrinkgate)
[![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS-lightgrey?style=flat-square&logo=linux)](https://github.com/KotalaKishanReddy/shrinkgate)
[![License](https://img.shields.io/badge/license-Apache--2.0-green.svg?style=flat-square)](LICENSE)

A ~2.3 MB local daemon on `127.0.0.1:18793` that cuts your coding agent token costs and latency by deduplicating conversation history before it reaches LLM APIs. Streaming, prompt-cache friendly, 100% local.

```text
Claude Code / Codex / OpenCode ──► ShrinkGate (127.0.0.1:18793) ──► Anthropic / OpenAI / Meta APIs
```

</div>

---

## ⚡ Quick Start

```sh
# 1. Install (via Cargo or prebuilt binary)
cargo install shrinkgate

# 2. Wire your agent (auto-starts background service on Linux/macOS)
shrinkgate connect claude    # Claude Code
shrinkgate connect codex     # OpenAI Codex
shrinkgate connect opencode  # OpenCode

# 3. Check health and live savings
shrinkgate status
```

<details>
<summary><b>Other install options (prebuilt binaries / docker)</b></summary>

- **Prebuilt Binaries (Linux & macOS)** from [Releases](https://github.com/KotalaKishanReddy/shrinkgate/releases):
  ```sh
  # macOS Apple Silicon (M1/M2/M3/M4)
  curl -sSL https://github.com/KotalaKishanReddy/shrinkgate/releases/latest/download/shrinkgate-v0.1.0-aarch64-macos.tar.gz | tar -xz && sudo mv shrinkgate /usr/local/bin/

  # Linux x86_64
  curl -sSL https://github.com/KotalaKishanReddy/shrinkgate/releases/latest/download/shrinkgate-v0.1.0-x86_64-linux.tar.gz | tar -xz && sudo mv shrinkgate /usr/local/bin/

  # macOS Intel x86_64
  curl -sSL https://github.com/KotalaKishanReddy/shrinkgate/releases/latest/download/shrinkgate-v0.1.0-x86_64-macos.tar.gz | tar -xz && sudo mv shrinkgate /usr/local/bin/
  ```
- **Docker**:
  ```sh
  docker run -d --network host -v ~/.local/state/shrinkgate:/data ghcr.io/kotalakishanreddy/shrinkgate:latest
  ```
</details>

---

## 💡 How It Works

Coding agents re-send the entire conversation history on every turn. ShrinkGate intercepts outbound requests, SHA-256 fingerprints candidate payloads (tool results, file reads), and replaces re-sent duplicates with deterministic stubs while keeping the newest copy verbatim.

- **40–70% token savings** across multi-turn sessions.
- **Cache-stable**: Deterministic stubs preserve upstream prompt caching.
- **Zero data egress**: Runs on `127.0.0.1`, forwards auth headers untouched over TLS.
- **Untouched**: System prompts, tool definitions, reasoning traces, and media are **never modified**.

Try the built-in 3-turn fixture without keys:
```sh
shrinkgate demo
```

---

## 🔌 Supported Agents

| Agent | Connect Command | What It Does |
| :--- | :--- | :--- |
| **Claude Code** | `shrinkgate connect claude [--lean] [--statusline]` | Sets `ANTHROPIC_BASE_URL`, registers session hook & optional statusline |
| **OpenAI Codex** | `shrinkgate connect codex [--env-key VAR]` | Configures `base_url` and `responses` API in `~/.codex/config.toml` |
| **OpenCode** | `shrinkgate connect opencode` | Adds `shrinkgate/` providers in `opencode.json` |
| **Muse Code** | `shrinkgate connect muse` | Writes helper environment file for `--base-url` routing |
| **Any Agent** | `shrinkgate env` | Exports `ANTHROPIC_BASE_URL` & `OPENAI_BASE_URL` for manual wiring |

---

## 🛠️ CLI Reference

| Command | Description |
| :--- | :--- |
| `serve [--port 18793]` | Run background engine (`launchd` on macOS, `systemd` on Linux) |
| `connect <agent>` | Wire an agent (`claude`, `codex`, `opencode`, `muse`) |
| `status [agent]` | Check engine health, wiring state, and token savings |
| `uninstall <agent>` | Restore agent's original settings (`--purge` removes binary & state) |
| `statusline` | Print today's token savings formatted for terminal statuslines |
| `demo` | Run local 3-turn savings simulation |
| `env` | Print shell exports for manual agent routing |

---

## ⚙️ Configuration

`~/.config/shrinkgate/config.toml` (auto-generated template):

```toml
port = 18793
shrink = true
min_block_bytes = 600        # Minimum content size to qualify for stubbing
keep_last_messages = 2       # Number of newest messages to always keep verbatim
max_sessions = 1024          # In-memory session tracking capacity
```

<details>
<summary><b>🚀 Release & Publishing Commands</b></summary>

```sh
# Push a new GitHub release (builds Linux, macOS aarch64/x86_64, & Docker)
git tag -a v0.1.1 -m "Release v0.1.1" && git push origin v0.1.1

# Publish to Crates.io
cargo publish
```
</details>

---

## 📄 License & Privacy

Apache-2.0. ShrinkGate is open source, private, local-only, and contains zero telemetry. Not affiliated with Anthropic, OpenAI, Meta, or Context Shrink LLC.
