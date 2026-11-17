# Contributing to ShrinkGate

Thank you for your interest in contributing to ShrinkGate! We welcome bug reports, documentation improvements, feature suggestions, and code contributions.

---

## Getting Started

### Prerequisites
- **Rust**: Version 1.85 or newer (`rustup update stable`)
- **Cargo**: Standard package manager included with Rust
- **Linux**: Primary target OS (systemd user units, Linux loopback IPC)

### Cloning & Building

```sh
# Clone your fork
git clone https://github.com/KotalaKishanReddy/shrinkgate.git
cd shrinkgate

# Build in debug mode
cargo build

# Run the test suite
cargo test

# Run the self-contained end-to-end integration demo
cargo run -- demo
```

---

## Code Quality Standards

Before submitting a Pull Request, ensure that your changes satisfy all checks:

1. **Formatting**:
   ```sh
   cargo fmt --check
   ```
2. **Clippy Linter**:
   ```sh
   cargo clippy -- -D warnings
   ```
3. **Tests**:
   All unit and integration tests must pass cleanly:
   ```sh
   cargo test --release
   ```

---

## Codebase Architecture

```
shrinkgate/
├── src/
│   ├── main.rs            # CLI entry point (clap subcommands & dispatch)
│   ├── server.rs          # Hyper HTTP engine on 127.0.0.1:18793
│   ├── config.rs          # TOML configuration loader (~/.config/shrinkgate/config.toml)
│   ├── upstream.rs        # Hyper-rustls client, TLS pools, header forwarding
│   ├── store.rs           # Session cache, fingerprinting (ring SHA-256), usage journal
│   ├── paths.rs           # XDG base directories & agent config paths
│   ├── statusline.rs      # Daily savings formatter for terminal statuslines
│   ├── demo.rs            # Mock upstream & multi-turn integration test
│   ├── shrink/            # Content-level shrink passes
│   │   ├── mod.rs         # Common types, stubs, candidates
│   │   ├── anthropic.rs   # Anthropic Messages API (/v1/messages)
│   │   ├── openai.rs      # OpenAI Chat Completions (/v1/chat/completions)
│   │   └── responses.rs   # OpenAI Responses API (/v1/responses)
│   └── wire/              # Agent dotfile configuration & restore
│       ├── mod.rs         # Atomic config write/restore engine
│       ├── claude.rs      # Claude Code (~/.claude/settings.json)
│       ├── codex.rs       # OpenAI Codex (~/.codex/config.toml)
│       ├── opencode.rs    # OpenCode (~/.config/opencode/opencode.json)
│       ├── muse.rs        # Muse Code helper environment
│       └── systemd.rs     # systemd --user service management
```

---

## Pull Request Guidelines

- Provide a concise description of the motivation and changes in your PR.
- If introducing or modifying a shrink pass or protocol adapter, include corresponding unit tests in `src/shrink/`.
- Ensure no sensitive credentials, personal paths, or PII are added to git history.
- Pull requests are automatically verified by GitHub Actions CI against formatting, clippy, unit tests, and integration demos.

---

## Maintainer Release Process

To publish a new release across GitHub, Crates.io, and GHCR:

1. **Verify All Checks**:
   ```sh
   cargo fmt --check && cargo clippy -- -D warnings && cargo test --release
   ```
2. **Bump Version**: Update `version = "x.y.z"` in `Cargo.toml` and log changes in `CHANGELOG.md`.
3. **Commit, Tag & Push**:
   ```sh
   git commit -am "chore: release vx.y.z"
   git tag -a vx.y.z -m "Release vx.y.z"
   git push origin main --tags
   ```
4. **Publish to Crates.io**:
   ```sh
   cargo publish
   ```
