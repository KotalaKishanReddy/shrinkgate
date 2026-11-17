# Security Policy

ShrinkGate takes the privacy, confidentiality, and security of your coding agent traffic and developer credentials seriously.

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

## Reporting a Vulnerability

If you discover a security vulnerability or sensitive defect in ShrinkGate, please report it privately:

1. **GitHub Security Advisory**: Use the [Private Vulnerability Reporting](https://github.com/KotalaKishanReddy/shrinkgate/security/advisories/new) feature on GitHub.
2. **Alternative**: Contact the maintainer via GitHub profile or email.

Please **do not** file public GitHub issues for security vulnerabilities. We will review and respond promptly.

---

## Security Architecture & Threat Model

ShrinkGate is engineered with defense-in-depth principles:

### 1. Local-Only Loopback Binding
- ShrinkGate strictly binds to `127.0.0.1:<port>`. It never listens on `0.0.0.0` or public network interfaces.
- When running in Docker, `--network host` is required so that traffic never crosses external container bridges.

### 2. Upstream TLS & Certificate Verification
- All upstream connections (Anthropic, OpenAI, Meta, OpenCode) require TLS with Mozilla's root certificates via `webpki-roots` and Rustls.
- Hop-by-hop headers and internal proxy headers (`x-shrinkgate-*`) are strictly stripped before forwarding.

### 3. Credential & Privacy Guarantees
- **No telemetry / no accounts**: ShrinkGate has zero analytics, zero external logging, and no license server.
- **Pass-through authentication**: Your API keys and ChatGPT authorization tokens pass through directly to official provider APIs. ShrinkGate never writes API keys or secrets to disk.
- **Local usage journal**: Usage metadata (`~/.local/state/shrinkgate/usage.jsonl`) contains only request status, latency, and token totals—never prompt text, code, or secret headers.

### 4. Deterministic & Safe Rewriting
- ShrinkGate replaces duplicate content blocks with deterministic, structured stubs.
- System prompts, tool definitions, reasoning/thinking traces, and media/image payloads are **never** altered or trimmed.
- High-recency messages (the newest `keep_last_messages`) are protected and always sent verbatim.
