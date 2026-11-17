# Changelog

## Unreleased

- Enforce body limits during reads, add connection/header/read timeouts, bound concurrent connections and drain on SIGTERM/Ctrl+C.
- Use full SHA-256, short accurate stubs and request-local deduplication: remove persistent session maps and the global transform lock; do not expand short blocks.
- Deduplicate tool outputs by default; user/assistant text is opt-in. Preserve mixed-media chat messages.
- Atomically write private recovery data before agent changes; preserve backups on reconnect; serialize configuration commands and retain externally changed settings.
- Fix Claude lean-off isolation, OpenCode Meta routing, provider shape validation, upstream override priority, hop-by-hop header filtering and daemon identity checks.
- Separate estimated successful-request reductions from provider usage; mark cancelled streams and rotate journals.
- Add dry runs, doctor, JSON stats, offline fixture benchmarks, regression tests, verified installers, documented release gates and a project page.
- Add MSRV/dependency/container CI checks and validate release tag/version consistency.
- Remove unsupported savings/cache/privacy claims. No live-provider billing or agent-version certification is implied.

All notable changes to **ShrinkGate** will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.1.2] - 2026-11-17

### Added
- **Per-Request Panic Isolation**: Replaced release `panic = "abort"` with standard unwinding and wrapped HTTP request handlers in `std::panic::AssertUnwindSafe(..).catch_unwind()`, returning an isolated 500 error if malformed requests occur while keeping the daemon alive.
- **Automated Checksum Verification**: Added `sha256sums.txt` generation and upload to GitHub Releases for all binary archives.
- **Prompt Cache Documentation**: Comprehensive breakdown of prefix caching dynamics, deterministic SHA-256 stub stability, and comparison with reactive `/compact`.

### Fixed
- Replaced static test badge in README with live GitHub Actions CI status badge.
- Corrected net reduction percentage calculations on 3-turn test fixture.
- Moved maintainer release instructions from user-facing README into `CONTRIBUTING.md`.
- Updated repository description to reflect cross-platform positioning.

## [0.1.1] - 2026-11-17

### Added
- **Full Windows Support**: Cross-platform compatibility for Windows (x86_64 MSVC / GNU):
  - Path normalization with `USERPROFILE`, `.exe` extensions, and Windows directory separators.
  - Semicolon-delimited `PATH` resolution for binary discovery (`muse.exe`).
  - Windows detached background process spawning with `CREATE_NO_WINDOW` and `DETACHED_PROCESS` flags.
  - Windows atomic file overwriting for binary installation.
- **Prebuilt Windows Binaries**: Automated `.zip` and `.tar.gz` packaging for `x86_64-pc-windows-msvc` on GitHub Releases.
- **Continuous Integration**: Added `windows-latest` matrix runner to CI.

## [0.1.0] - 2026-11-17

### Added
- **Local Context-Shrinking Gateway**: High-performance HTTP/1 proxy on `127.0.0.1:18793` built with Hyper, Tokio, and Rustls.
- **Protocol Support**:
  - Anthropic Messages API (`/v1/messages`, `/v1/messages/count_tokens`)
  - OpenAI Chat Completions API (`/v1/chat/completions`)
  - OpenAI Responses API (`/v1/responses`)
  - Catch-all fallback proxy for custom endpoints
- **Deduplication & Stubbing Engine**:
  - Length-prefixed SHA-256 fingerprinting for candidate text payloads.
  - Deterministic replacement of re-sent duplicate content with informative stubs.
  - Preservation of newest content occurrences, thinking/reasoning blocks, system prompts, and media.
  - Configurable `keep_last_messages` window and `min_block_bytes` thresholds.
- **Agent Integration (`shrinkgate connect`)**:
  - Native configuration and zero-downtime wiring for **Claude Code**, **OpenAI Codex**, **OpenCode**, and **Muse Code**.
  - Atomic config write/restore with rollback safety (`shrinkgate uninstall`).
  - systemd user service management for automatic background execution.
- **Tooling & Observability**:
  - Built-in multi-turn simulation demo (`shrinkgate demo`).
  - Terminal statusline integration for real-time daily token savings.
  - Local JSONL usage journal with UTC date aggregation.
- **Distribution & CI/CD**:
  - Multi-stage minimal container image (`ghcr.io/kotalakishanreddy/shrinkgate`).
  - Automated GitHub Actions release pipeline for binary tarballs and container images.
