# Roadmap

## Current hardening

Reliable installation and rollback, bounded proxy buffers, stateless deduplication, conservative content eligibility, honest diagnostics and statistics, fixture benchmarks, and cross-platform CI.

## Next, with evidence

- Collect sanitized real-session benchmarks and task-quality comparisons.
- Record exact agent/auth versions tested; investigate preserving cache prefixes without removing the only in-request copy.
- Add configurable resource limits after soak measurements; validate enterprise CA/proxy requirements.
- Homebrew, winget and signed release provenance after verifying the release process. No package-manager availability is claimed today.

## Small contributor tasks

Each task should become a scoped issue with sample inputs and an acceptance test:

- Add an anonymized Unicode-heavy compiler-output fixture and document tokenizer-estimation error.
- Add an agent compatibility report with exact version, auth mode and connect/uninstall steps.
- Test verified installation paths containing spaces on Windows and macOS.
- Add a provider-cache comparison using the documented matched-run methodology.

## November launch preparation

Fix installation and reliability first, then record a short terminal demo and publish a reproducible technical write-up. Share it in relevant Rust/agent communities following their rules; respond to real bug reports and contributor questions. Track qualified visits, successful installs and repeat use alongside stars. No purchased stars, engagement exchanges, unsolicited bulk messages, or unsupported savings claims.
