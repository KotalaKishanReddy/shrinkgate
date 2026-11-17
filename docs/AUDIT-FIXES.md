# Follow-up audit fixes — 2026-11-17

All seven reproduced defects from the audit of `1fad32e`, plus its journal-backlog risk, are addressed in this revision. Verification below describes a local Linux build; cross-platform checks run in CI. Existing live-authentication and billing blockers are not relabeled as successes.

| Finding | Change | Regression evidence |
| --- | --- | --- |
| Muse/Meta traffic sent to OpenAI | Explicit provider/default selection takes precedence for Responses, Chat Completions and passthrough routes. JWT shape alone no longer selects ChatGPT; its account header is required. | Real loopback backends assert Meta receives the requests and dummy auth, OpenAI receives none until explicitly selected; separate JWT-identity test |
| Stale lean backup deletes a later user value | A new management cycle captures its current original value; `lean off` retires the completed backup entry. | On/off → custom value → force-on/off restores that custom value |
| Pre-existing Claude hook removed | Snapshots record whether connect added the hook. Uninstall removes only owned entries and preserves original empty hook containers. | Exact pre-existing hook survives a full connect/uninstall round trip |
| Existing/custom service overwritten or deleted | systemd/launchd writes have a separate ownership snapshot. Unmanaged or externally edited files are preserved; only unchanged owned files are removed. | Stubbed OS service managers verify user-file preservation, drift diagnostics, and owned-file cleanup on supported Unix CI platforms |
| Failed/incomplete SSE streams credited | Parse bounded SSE events, including multi-line data; classify completion, API failure, incomplete EOF, transport failure and early cancellation. | Fragmented input, all supported terminal markers, API errors, incomplete streams, early closure and unchanged forwarded bytes |
| Cancelled usage missing from totals | Aggregate all observed provider usage before filtering reduction credit; expose the failed/cancelled portion separately. | Status-499 record retains input/output/cache usage and earns no estimated reduction credit |
| Doctor returns success for drift/disabled engine | Health protocol v2 verifies an effective-configuration fingerprint. Doctor also checks managed settings/hooks/service ownership and journal overflow. | Changed Codex routing, removed owned hook and `--no-shrink` mismatch produce failure; connect refuses an incompatible engine |
| Unbounded blocking journal jobs | One disk worker consumes a 256-record queue. Full/closed queues drop new metadata with a counter and rate-limited warnings; shutdown flush is bounded. | Deterministically stalled writer proves queue capacity, drop count, nonblocking submission and recovery after release |

## Verification

- **55 passing Linux release tests:** 40 unit/proxy tests, eight audit CLI regressions, seven existing CLI regressions.
- Formatting, Clippy with warnings denied, Rust 1.85 host/all-target check, locked release build and asserted offline demo passed.
- Installed **Codex 0.167.0-alpha.4** again passed the isolated local Responses-provider fixture: streaming, dummy-auth forwarding, 401 failure handling and configuration restoration.
- A new **120-second, eight-client soak** completed **21,674 requests**, including **496 injected HTTP 503 errors**, **354 intentional cancellations** and one graceful restart, with **zero unexpected failures**.
- All 21,674 submitted journal records were retained in this run. Queue-overflow behavior was tested separately with a deliberately blocked writer. The short soak did not exercise rotation; the earlier dated ten-minute run did.
- Highest sampled daemon RSS in this follow-up soak was **8,604 KiB**. This is a shared-host observation with approximately one-second samples, not a comparative performance or leak-proof claim.

[Verification JSON](validation/audit-fix-verification-2026-11-17.json) · [Client JSON](validation/codex-client-audit-fixes-2026-11-17.json) · [Soak JSON](validation/soak-audit-fixes-2026-11-17.json)

The binary hash in all three records identifies the tested working build based on `1fad32e`; release-package version remains 0.1.2 with these changes marked Unreleased. Earlier dated reports remain historical evidence for their original revisions.

## Upgrade and accounting notes

Stop/restart older daemons before using this revision. An old health response or mismatched active configuration is rejected instead of being silently accepted. Service files from installations without an ownership snapshot are preserved for manual migration. Legacy hook snapshots that cannot establish ownership keep the hook and emit a warning.

`provider_usage` totals now include observed failed/cancelled usage and a nested `failed_or_cancelled` group. Estimated reduction credit still requires successful completion. Historical records cannot be reclassified without their original stream events. A dropped metadata record makes local summaries incomplete; `/healthz` exposes the running daemon's drop count and `doctor` fails when it is nonzero. The counter resets on restart.

See [OPERATIONS.md](OPERATIONS.md) for limits, shutdown flushing and recovery details. Live-provider authentication, real invoiced savings/task-quality comparisons and multi-hour live-agent runs remain release gates.
