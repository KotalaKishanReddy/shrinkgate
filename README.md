# ShrinkGate

A **local context-shrinking gateway** for coding agents. One small Rust binary
runs on `127.0.0.1:18793`; your coding agents (Claude Code, Codex, OpenCode,
Muse Code, anything Anthropic- or OpenAI-compatible) send their model traffic
through it, and it trims the content they **re-send on every turn** before it
reaches the model API — with your own auth, streaming untouched.

Built for Linux as an independent, from-scratch alternative to
[ContextShrink Code](https://code.contextshrink.com) (whose Windows-only
engine ships today). The design was informed by studying the Apache-2.0
connector packages on npm (`contextshrink-claude`, `contextshrink-codex`);
no part of their closed engine was used or decompiled.

```
Claude Code ──► ShrinkGate (127.0.0.1:18793) ──► api.anthropic.com   (your API key)
Codex       ──►                             ──► api.openai.com / ChatGPT backend (your login)
OpenCode    ──►                             ──► api.openai.com     (your API key)
Muse Code   ──►                             ──► api.meta.ai        (your key)
```

## How the shrink pass works

Coding agents replay the whole conversation (including every tool result —
file reads, command output, search results) on **every** request. The same
bytes go to the model again and again.

ShrinkGate fingerprints every large text payload (SHA-256 over length-prefixed
content, so concatenations can't collide). When the same fingerprint has
already been sent in this session, every occurrence **except the newest** is
replaced by a short deterministic stub:

> `[shrinkgate] unchanged content — an identical copy was already sent earlier
> in this conversation (first seen in request #3, 18422 bytes). This duplicate
> was trimmed locally to save context; the original text above is still accurate.`

Properties:

- **The newest occurrence is always verbatim** — the model always has the
  current copy of any file or command output.
- **Deterministic stubs** — the rewritten prefix is byte-identical from the
  request after the first one, so the provider's prompt cache keeps hitting.
- **Stubs say what happened** — first-seen request number and original size,
  so the model can still reason about earlier context.
- **Never touched**: system prompts, tool/function definitions, tool-use and
  tool-call blocks, thinking/reasoning blocks, images, other media, and the
  newest `keep_last_messages` (default 2) messages of the conversation.
- **Never re-stubbed** — content that is already a stub is skipped, whatever
  the size threshold.
- **Idempotent + stateful per session** — sessions are keyed by model +
  system prompt (+ metadata id), capped at `max_sessions` with LRU eviction.

Nothing is uploaded anywhere: fingerprinting, rewriting and journaling all
happen locally. Auth headers pass through unchanged. Responses (SSE or
buffered) stream through untouched; ShrinkGate only taps `usage` fields to
keep a local savings journal.

## Quick start

```sh
cargo build --release
./target/release/shrinkgate demo          # 3-turn end-to-end demo, no keys needed
./target/release/shrinkgate connect claude    # wire Claude Code + install service
./target/release/shrinkgate status
./target/release/shrinkgate uninstall claude  # restore previous settings
```

`connect` starts the engine (systemd user unit `shrinkgate.service` if
available, else a detached process), copies the binary to
`~/.local/bin/shrinkgate`, writes the agent config, and records the previous
value of every key it touched. `uninstall` restores each key **only while it
still holds what ShrinkGate wrote** — anything you changed since connect is
left alone and reported.

### Claude Code

`connect claude` writes to `~/.claude/settings.json` (honoring
`CLAUDE_CONFIG_DIR`):

```json
{ "env": { "ANTHROPIC_BASE_URL": "http://127.0.0.1:18793",
           "ENABLE_TOOL_SEARCH": "true" },
  "hooks": { "SessionStart": [ { "hooks": [
      { "type": "command", "command": "~/.local/bin/shrinkgate serve --ensure" } ] } ] } }
```

- `ENABLE_TOOL_SEARCH` is only set when you haven't set it yourself.
- SessionStart hook keeps the engine alive even without systemd.
- `--lean` / `shrinkgate lean on` caps `CLAUDE_CODE_AUTO_COMPACT_WINDOW` at
  200000 tokens (lean mode, like the original's "lean").
- `--statusline` wires `shrinkgate statusline` (savings per day).
- Your `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN` env is untouched and
  forwarded with every request.

### Codex

`connect codex` adds a managed provider block to `~/.codex/config.toml`
(honoring `CODEX_HOME`), between `# --- ShrinkGate local engine ---` markers:

```toml
model_provider = "shrinkgate_local"
[model_providers.shrinkgate_local]
name = "ShrinkGate (local engine)"
base_url = "http://127.0.0.1:18793/v1"
wire_api = "responses"
requires_openai_auth = true   # ChatGPT sign-in kept; forwarded as-is
```

ChatGPT-login requests (JWT bearer / `chatgpt-account-id`) are routed to the
ChatGPT backend with the `/v1` prefix stripped; API-key requests go to the
configured OpenAI upstream. Use `--env-key VAR` to authenticate with an env
var instead.

### OpenCode

`connect opencode` adds providers to `~/.config/opencode/opencode.json`:

- `shrinkgate` → `@ai-sdk/anthropic` at `http://127.0.0.1:18793/v1`
  (the `/v1` is required — the SDK only adds it for the official host)
- `shrinkgate-openai` → `@ai-sdk/openai`
- `shrinkgate-meta` → Muse models via the Responses adapter
- `shrinkgate-opencode` → `@ai-sdk/openai` pointed at OpenCode's own inference
  endpoint (`https://opencode.ai/inference/openai`), with
  `x-shrinkgate-upstream: opencode` set automatically; free models
  (`glm-5.3-flash-free`, `deepseek-v4.1-flash-free`, `flint-40b-free`,
  `beam-free`) use `{env:OPENCODE_API_KEY}`

Pick models as `shrinkgate/claude-sonnet-4-5` etc. Keys use `{env:...}` or
`opencode auth login` — whatever you already use.

### Muse Code (Meta) — best effort

Muse Code is first-party paired with Meta's Model API and has no official
provider override. `connect muse` writes a helper env file and documents the
community `--base-url` route (`MODEL_API_KEY` unchanged; ShrinkGate forwards
to `api.meta.ai`).

### Any other agent

Point it at the engine and pick the protocol by path:

| Protocol               | Path                  | Upstream default            |
|------------------------|-----------------------|------------------------------|
| Anthropic Messages     | `POST /v1/messages`   | `https://api.anthropic.com`  |
| OpenAI Chat Completions| `POST /v1/chat/completions` | `https://api.openai.com` |
| OpenAI Responses       | `POST /v1/responses`  | OpenAI or ChatGPT backend    |
| anything else          | any path              | reverse-proxied by guess     |

Or run `shrinkgate env` and set the printed variables
(`ANTHROPIC_BASE_URL`, `OPENAI_BASE_URL`). A request header
`x-shrinkgate-upstream: anthropic|openai|meta|opencode` forces the upstream (it is
stripped before forwarding).

## Commands

| Command | What it does |
| --- | --- |
| `serve [--port P] [--ensure] [--quiet] [--no-shrink]` | run the engine (`--ensure`: start if not healthy, used by the hook) |
| `connect claude [--lean] [--statusline] [--force] [--no-service]` | wire Claude Code |
| `connect codex [--env-key VAR] [--force]` | wire Codex |
| `connect opencode [--force]` | wire OpenCode |
| `connect muse` | write the Muse helper env |
| `status [claude\|codex\|opencode\|muse]` | engine health, savings, wiring state |
| `uninstall claude\|codex\|opencode\|muse [--purge]` | restore settings (`--purge` removes binary, service and state) |
| `lean on\|off\|status [--force]` | Claude Code auto-compact window |
| `statusline` | one line for Claude Code's statusline |
| `demo` | 3-turn mock-upstream demo showing per-request savings |
| `env`, `config` | manual wiring exports / resolved configuration |

## Configuration

`~/.config/shrinkgate/config.toml` (written as a commented template on first
run; CLI flags win):

```toml
port = 18793
shrink = true
min_block_bytes = 600        # never stub blocks smaller than this
keep_last_messages = 2       # never stub the N newest messages
allow_assistant_text = false # also stub identical assistant text
max_sessions = 1024
upstream_anthropic = "https://api.anthropic.com"
upstream_openai = "https://api.openai.com"
upstream_meta = "https://api.meta.ai"
upstream_opencode = "https://opencode.ai/inference/openai"
upstream_chatgpt = "https://chatgpt.com/backend-api/codex"
default_upstream = "openai"
```

State lives in `~/.local/state/shrinkgate/`:

- `usage.jsonl` — one record per request (route, session, estimated
  orig/sent tokens, saved tokens, blocks stubbed, status, latency, real
  upstream usage when reported)
- `wire-<agent>.json` — what `connect` wrote and what was there before

Token estimates are `bytes/4`; real usage comes from the model's own usage
fields (tapped from JSON bodies and SSE events).

## Privacy & safety

- Local-only listener: binds `127.0.0.1`, never a public interface.
- No account, no license server, no telemetry — there is nothing to sign in to.
- Your API keys / ChatGPT login are only ever forwarded to the upstream you
  configured, over TLS with webpki roots.
- `uninstall` restores original settings and refuses to delete anything that
  changed after connect.
- The gateway never modifies response bodies.

## Limitations

- Assistant text is not stubbed by default (`allow_assistant_text = false`)
  since continuity there is usually worth its tokens.
- Responses-API `reasoning` items and tool definitions are never shrunk —
  replaying reasoning across turns is how those APIs work.
- Muse Code wiring is best-effort (no official override exists yet).
- HTTP/1 only on the local leg and to upstreams (coding agents don't need
  more); TLS with webpki roots on the upstream leg.

## License

Apache-2.0 — see [LICENSE](LICENSE). Not affiliated with Context Shrink LLC or
Anthropic/OpenAI/Meta.
