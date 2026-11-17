# Changelog

All notable changes to **ShrinkGate** will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

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
