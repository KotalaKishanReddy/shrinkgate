# Compatibility and release gates

| Surface | Automated coverage | Still requires live validation |
| --- | --- | --- |
| Anthropic Messages | Tool-result dedup, protected blocks, mock SSE proxy | Current Claude Code versions; API-key and subscription auth |
| Chat Completions | Tool outputs, media protection, mock SSE proxy | Current OpenCode adapter versions and selected providers |
| OpenAI Responses | Function outputs, reasoning preservation, mock SSE proxy | Live provider/auth acceptance; installed Codex 0.167.0-alpha.4 passes a local fixture (streaming, dummy API-key forwarding, 401 handling and restore) |
| Configuration | Dry runs, reconnect/restore, invalid data, lean isolation | OS-specific service lifecycle and real agent config evolution |
| Platforms | CI matrix: Linux, macOS, Windows | Long soak recorded on Linux only; live-agent/OS-specific service testing remains |

See [the dated validation report](VALIDATION.md) for installed-client results and blocked live-auth/billing checks. A passing client fixture is deliberately not listed as live-provider certification. Profiles and existing proxy routing may be refused rather than guessed. HTTP/SSE is supported; WebSocket transport, arbitrary request compression, and public/multi-user deployments are not supported.

Before tagging a release:

- Run CI, dependency audit, locked release tests/build, demo, benchmark and installer checks.
- Manually verify connect, task execution, cancellation, restart and uninstall with each supported agent/auth method. Record exact versions here.
- Run a long-session soak and observe peak RSS, journal rotation, daemon restart and provider failures.
- Review the audit's remaining limitations and publish measured results only with the benchmark methodology.
- Update Cargo package version, lockfile and changelog; tag only the validated commit. Verify every published checksum/archive, crate install and container start. Release workflow does not publish crates.io automatically.

This change improves production hardening; it is not independent security certification or evidence of live-provider acceptance.

Enterprise container builds can provide a trusted build CA without baking it into the runtime image:

```sh
docker build --secret id=cargo_ca,src=/path/to/trusted-ca-bundle.pem -t shrinkgate .
```

The runtime gateway still uses the documented Rustls roots. The build secret only configures Cargo's download trust.
