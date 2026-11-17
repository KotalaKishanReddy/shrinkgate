# Security policy

Report vulnerabilities privately through [GitHub advisories](https://github.com/KotalaKishanReddy/shrinkgate/security/advisories/new). Do not include credentials in public issues. Only the latest released 0.1.x revision receives fixes; changes on `main` may not be released yet.

## Intended deployment

A trusted, single-user workstation. ShrinkGate binds IPv4 loopback only. It is not an authenticated multi-tenant service, sandbox, or protection against malicious programs under your account. Do not publish its port or use a public reverse proxy. Docker requires host networking and uses an unprivileged runtime user.

Browser Origin requests and Host headers other than the configured loopback address are rejected. There is no CORS opt-in. Other local clients can still connect; protecting the workstation and account remains necessary.

## Traffic and credentials

Requests leave your machine for the configured model provider. There is no telemetry, analytics, license service, or extra forwarding destination. Configured upstreams require HTTPS with Rustls/webpki roots, except loopback HTTP for local providers/testing. Custom upstreams receive your agent's forwarded auth headers: configure only trusted endpoints. Enterprise private CAs and HTTP proxies are not automatically configured.

Hop-by-hop headers, Connection-nominated headers and internal routing headers are removed before forwarding. Compressed incoming bodies are rejected. Resource limits/timeouts are documented in the README; SSE streams have no fixed duration, and shutdown drains for 10 seconds. These controls do not make an exposed gateway appropriate for hostile traffic.

## Local storage

Prompt bodies and auth headers are not journaled. Usage journals include timestamps, route, a derived session fingerprint, status, latency, byte estimates and provider-reported usage. The fingerprint is pseudonymous metadata, not guaranteed anonymous. Journals rotate at approximately 8 MiB, with one previous file retained. Unix journal/config files use owner-only permissions; Windows relies on profile ACLs.

**Agent recovery snapshots can contain secrets already present in your original agent settings.** They are needed for rollback and are written atomically with private Unix permissions. Protect the entire state directory and backups. Configuration commands take a process lock. Reconnect preserves original snapshots; externally edited settings are retained. Symlink destinations are refused rather than silently replaced. Atomic rename protects each file, but a power failure between the recovery write and agent write is not a multi-file transaction: keep the saved snapshot for manual recovery.

## Transformation limits

Only sufficiently large repeated tool-output text is eligible by default. User/assistant prose requires explicit opt-in. System/developer prompts, schemas, reasoning and media are protected. The newest identical candidate and configured tail remain intact. Deduplication is stateless and uses full SHA-256; it does not remove a sole copy based on earlier requests.

Retaining identical content does not guarantee unchanged model behavior. Historical ordering changes can affect answers; rewriting can invalidate prompt caches and increase billed cost. Test your workloads. No security certification, semantic-equivalence guarantee, or live-provider acceptance is implied.
