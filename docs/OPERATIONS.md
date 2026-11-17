# Configuration and operations

## Configuration

`~/.config/shrinkgate/config.toml` (honors `XDG_CONFIG_HOME`):

```toml
port = 18793
shrink = true
min_block_bytes = 600
keep_last_messages = 2
allow_user_text = false
allow_assistant_text = false
# upstream_openai = "https://api.openai.com"  # No /v1 suffix
# default_upstream = "openai"
```

Restart after changes. Invalid/unknown options fail explicitly. The deprecated `max_sessions` key is accepted but ignored because transformation is stateless. Responses API tail protection counts input items, including tool-call items.

User/assistant text deduplication is opt-in: repeated instructions and historical placement can be intentional. Tool outputs containing mixed media are excluded. A sole copy in the current request is never removed based on earlier requests.

## Limits and shutdown

| Control | Compiled default |
| --- | --- |
| Concurrent HTTP connections | 16 |
| Transformed request body | 16 MiB, enforced while reading |
| Buffered non-streaming response | 64 MiB, enforced while reading |
| Upstream connection timeout | 10 seconds |
| Incoming request headers | 15 seconds |
| Request body, upstream response headers, non-streaming response body | 120 seconds each |
| Graceful shutdown drain | Up to 10 seconds |
| Usage journals | Approximately 8 MiB each, one previous file retained |

SSE responses stream incrementally with no fixed lifetime limit. A stalled stream can occupy a connection slot. Serve runs in the foreground; Ctrl+C or SIGTERM drains connections. Services use systemd on Linux or launchd on macOS; Windows uses a detached process. Stop detached processes before purging shared state.

## Safe connection and recovery

`connect <agent> --dry-run` prints the proposed agent configuration without writing it, starting services or installing a binary. The output can contain secrets already present in original settings; do not post it publicly.

Connection saves a recovery snapshot before replacing settings. Reconnect preserves the first snapshot; uninstall before changing options or the gateway port. Externally edited settings and recovery data are retained. Symlink destinations are refused instead of silently replaced.

`uninstall <agent> --purge` refuses to delete shared state while another integration or a listening engine remains. It retains the configuration lock file. Agent backups can contain pre-existing secrets; Unix files use private permissions and Windows relies on your profile ACLs. A crash between separate recovery/config writes may need manual restoration from the saved snapshot.

## Statistics

`stats --json` separates estimated candidate-byte reductions from provider-reported usage. It does not calculate billed savings. Failed/cancelled forwarded requests do not earn reduction credit. Retained journals do not include every rejected HTTP request or upstream connection failure.

Journals contain metadata, never prompt bodies or auth headers: timestamps, route, a derived session fingerprint, status, latency, estimates and provider usage. The fingerprint is pseudonymous, not guaranteed anonymous. Totals cover retained history, not lifetime usage. Day boundaries are UTC.

## Docker

```sh
docker run --rm --network host \
  -v shrinkgate-state:/data \
  ghcr.io/kotalakishanreddy/shrinkgate:latest
```

Published images track tagged releases, not `main`. The hardened Dockerfile on `main` runs as an unprivileged user; this must not be assumed for older published tags. Use a dedicated volume. Docker Desktop needs supported/enabled host networking; native binaries are preferable otherwise. Never expose the loopback gateway publicly.

See [SECURITY.md](../SECURITY.md) for the threat model, TLS roots and credential handling. See [COMPATIBILITY.md](COMPATIBILITY.md) for an optional corporate build-CA secret; runtime proxy/private-CA settings are not automatically configured.
