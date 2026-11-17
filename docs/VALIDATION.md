# Validation report — 2026-11-17

For the subsequent correctness fixes and their new regression/client/soak results, see [AUDIT-FIXES.md](AUDIT-FIXES.md). This report retains the original run history.

This report distinguishes actual client execution, local provider simulation, live authentication, and billed-cost measurement. The tested gateway binary is based on commit `481e69d`; no Rust source changes were needed for these follow-up checks. Binary SHA-256 values accompany the JSON records.

| Check | Result | What it establishes |
| --- | --- | --- |
| Installed Codex `codex-cli 0.167.0-alpha.4` | Passed against local provider fixture | Actual client consumes gateway SSE, forwards dummy API-key auth, handles 401 errors and restores configuration |
| Direct live Codex sign-in | Blocked | Baseline provider requests return HTTP 401 and token refresh fails without ShrinkGate |
| Claude Code / OpenCode live authentication | Blocked | Clients and provider credentials are unavailable in this environment |
| Real billed savings | Blocked | No metered provider credentials; user requested continuing without them |
| Ten-minute mixed-protocol Linux soak | Passed | Bounded local reliability check covering errors, cancellation, long request histories and restart |

## Installed-client test

`validate_codex.py` runs the installed Codex executable, not a reimplementation of it. It creates a disposable workspace, a local Responses API provider, an isolated Codex home and a dummy key. It compares a direct fixture turn with a turn routed through the real `connect codex --env-key` configuration, verifies the exact streamed answer, checks that an injected 401 fails the client turn, and confirms uninstall restores the original config.

```sh
python3 scripts/validate_codex.py --binary target/release/shrinkgate \
  --output codex-client.json
```

The fixture uses a deliberately synthetic model and text-only turn. Codex falls back to model metadata for this fixture name. The test does not establish real model access, tool execution, task quality, subscription authentication, model catalog compatibility, or WebSocket support. Provider-reported usage in this record is synthetic and must not be treated as billed tokens.

[Installed-client JSON](validation/codex-client-2026-11-17.json)

## Extended gateway soak

- **Duration:** 600 active seconds; 601.55 seconds including setup and shutdown.
- **Concurrency:** 8 clients, two five-minute phases and one graceful daemon restart.
- **Traffic:** 108,617 transformed requests across Anthropic Messages, Chat Completions and Responses.
- **Outcomes:** 104,353 successful responses, 2,487 deliberately injected HTTP 503 responses, 1,777 intentional stream cancellations, **zero unexpected failures**.
- **History shape:** 32 paired tool calls/results per request, repeated diagnostics and a changed final output. Earlier duplicates and a retained original were checked at the local upstream.
- **Memory:** highest sampled daemon RSS 9,792 KiB; phase-end RSS 8412, 8632 KiB. Samples are approximately one second apart and can miss short peaks. This is a shared development host, not a controlled performance benchmark.
- **Journals:** `usage.jsonl`: 8,294,655 bytes; `usage.previous.jsonl`: 8,388,768 bytes. Rotation was exercised and files remained within the configured size bound plus one record. Retained history contains 888 cancelled-stream records.

```sh
python3 scripts/soak.py --duration 600 --workers 8 \
  --binary target/release/shrinkgate --output soak.json
```

CI runs a ten-second Linux smoke version of the same harness. The full run remains reproducible locally. No provider keys or API calls are used. This is a bounded daemon soak, not a continuous ten-minute live-agent task or multi-hour certification. The history fixture is fixed per protocol/worker, not a growing real model conversation. Injected cancellations verify HTTP stream cleanup, not a specific agent's cancellation UI.

[Full soak JSON](validation/soak-2026-11-17.json)

## Live-access and billing blockers

The environment's `codex login status` reported a ChatGPT sign-in. An actual direct CLI probe then received HTTP 401 on the provider model-catalog path, and token refresh failed. The authenticated model turn could not complete. This failure occurred before routing through ShrinkGate, so it is recorded as an unavailable baseline, not a gateway authentication success or defect.

Raw account identifiers, auth material, prompts, provider request IDs and raw client logs are excluded from committed results. The local fixture tests use only dummy credentials. No metered OpenAI/Anthropic keys are configured, so no billed-cost comparison was attempted. Byte/4 estimates and mock usage are not substituted for it.

[Access/blocker JSON](validation/live-access-2026-11-17.json)

## README audit

The README now starts with the source installation required for unreleased commands, rather than suggesting the published v0.1.2 package implements them. A new header graphic and compact navigation lead into the demo, measurement guidance and evidence table. Long configuration, limit and recovery details moved to [OPERATIONS.md](OPERATIONS.md).

The audit also corrected the container claim: the hardened Dockerfile runs unprivileged, but that behavior must not be assumed for old published images. Fixture results are separated from live-auth/billing claims. Relative links, SVG syntax and embedded JSON were checked before publication.

## Remaining release gates

Renew the Codex sign-in and validate a direct and proxied real task before claiming ChatGPT compatibility for this version. Validate current Claude Code and OpenCode clients with their real providers. With metered credentials, run matched cache-controlled billing/task-quality comparisons using [BENCHMARKS.md](BENCHMARKS.md). Run multi-hour live-agent sessions and OS-specific service lifecycle checks before a production release. These checks remain open; the completed local tests do not replace them.
