# Production-hardening audit — 2026-11-17

Scope: source-level review of the gateway, three protocol transformers, usage storage, CLI/configuration, four agent helpers, service adapters, installation, CI/release workflows, container and public documentation. This is a maintainer engineering audit, not an independent security certification.

Follow-up defects and their fixes are tracked in [AUDIT-FIXES.md](AUDIT-FIXES.md). Earlier validation below describes its historical revision.

## Findings addressed

| Area | Fix and rationale |
| --- | --- |
| Installation | Version-consistent release URLs, download/verify/extract ordering, platform detection, no-root Unix and Windows installers |
| Claims | Remove unsupported 40–70% savings, loss-free behavior, stable-cache and zero-egress claims; distinguish estimated byte reduction from billing |
| Deduplication | Stateless per-request transforms replace shared session/LRU maps; full SHA-256, borrowed map keys, fewer large string copies and shorter content-linked stubs |
| Content safety | Tools-only default; user/assistant text opt-in; mixed-media chat protection; preserve last copy and tail; never grow a short duplicate |
| Buffers and resources | Limit bytes during reads, cap connections, connect/header/body timeouts, graceful shutdown; SSE parser avoids repeated drain/memmove and bounds partial lines |
| HTTP routing | Explicit provider selection beats JWT heuristics; fix Meta provider header; strip Connection-nominated/internal headers; reject compressed requests, browser Origins and wrong Host |
| Configuration | Strict option validation; atomic private writes and original recovery snapshots; process lock; idempotent reconnect; refuse malformed provider shapes, corrupt recovery and symlink destinations |
| Recovery | Preserve changed settings and backups, isolate `lean off`, rollback saved state on failed config writes, refuse unsafe shared/running-engine purge |
| Health | Validate gateway engine/version/port JSON rather than accepting any listener or HTTP 200 |
| Accounting | Separate provider usage from byte/4 estimates; exclude failed/cancelled streams from reduction credit; rotated journals and streamed summary reads |
| Distribution | Locked release builds, package/tag check, image publish waits for binary validation, unprivileged container, optional trusted Cargo build CA secret |
| Adoption | Recorded offline demo, fixture benchmark command/report, dry runs, doctor, JSON stats, compatibility gates, roadmap, static project page and social artwork |

The transformer only deduplicates multiple copies **within a single request**. Persistent state was not needed to decide which copy to retain; removing it avoids global transform locking, per-session memory growth, cross-session bookkeeping collisions and restart-dependent stub text. This is a structural efficiency improvement; no before/after throughput claim is made.

## Local validation

- Linux locked release build, formatting and Clippy with warnings denied.
- 39 passing tests: 32 unit/proxy tests plus 7 CLI integration tests. Coverage includes all three real loopback proxy routes, unchanged SSE bytes, fragmented SSE parsing, body limits, rejected inputs, routing precedence, protected content, deterministic replay, backups/reconnect/restore, dry runs, lean isolation, corrupt state, symlinks, guarded purge and partial journal-line reads.
- Rust 1.85 check for all targets in this package (host platform), including tests.
- Asserted three-turn offline proxy demo; 12 synthetic scenarios benchmarked with 500 iterations each. Results are in `benchmark.json`; timing is environment-specific.
- Unix installer syntax and actual v0.1.2 download/checksum/install into an isolated temporary directory.
- RustSec advisory scan of the lockfile. Yanked-version checking is separate from vulnerability matching; see CI for the full dependency check.

CI additionally runs on Linux, macOS and Windows, checks PowerShell syntax, builds the container and runs the dependency audit. Local full container compilation encountered the managed environment's certificate chain and then disk capacity; the Dockerfile supports an optional trusted build CA secret. The clean CI container job is the full-build validation gate. Do not interpret a runtime version smoke check as live API validation.

## Remaining release gates and limits

- Follow-up testing exercised the installed Codex client against a local Responses provider and a bounded Linux soak; see [VALIDATION.md](VALIDATION.md). Direct live Codex access returned HTTP 401 before the gateway was involved. Metered billing tests remain blocked by unavailable credentials. Live provider/auth acceptance, real task quality, cached-token economics and multi-hour sessions still require validation before a production release or quantitative savings launch.
- The product is for a trusted single-user workstation, not hostile public/multi-tenant traffic. Other local applications can reach loopback. Recovery snapshots can contain pre-existing agent secrets; Unix permissions and Windows profile ACLs matter.
- Rewriting can invalidate prompt caches and alter model interpretation. The local benchmark cannot establish net billed savings. The compatibility table intentionally lists live versions as unverified.
- Resource limits are compiled defaults; an SSE response has no fixed lifetime. A slow or idle stream can occupy one connection slot. Journals retain roughly 16 MiB, not lifetime history; midnight boundaries are UTC. Journals describe forwarded responses/streams, not every rejected HTTP request or upstream connection failure.
- Atomic replacement protects each individual file. A process/power failure between separate recovery/config writes may still require manual recovery from the saved snapshot.
- Muse remains experimental; WebSocket transport, public access, automatic enterprise-CA configuration, signed installers and Homebrew/winget distribution are not delivered.
- The project page and preview artwork are repository assets. Hosting and GitHub social-preview settings require separate configuration. No external promotion was posted.

Hardening on `main` is marked Unreleased. Publishing a tag, crates.io package or new container should follow `COMPATIBILITY.md` after the live checks. A star target is a distribution goal, not a quality or reliability guarantee.
