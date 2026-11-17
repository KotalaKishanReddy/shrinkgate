# Measuring ShrinkGate

## Reproducible local fixtures

```sh
cargo build --release --locked
./target/release/shrinkgate benchmark --iterations 500 > benchmark.json
./target/release/shrinkgate benchmark --input private-request.json --iterations 100
```

The 12 built-in fixtures cover Messages, Chat Completions and Responses with repeated outputs, unique outputs, a changed final output, and short outputs. Tool-call/result IDs are paired. Each iteration starts with the original request and measures transformation plus JSON serialization; cloning, disk reads and network calls are excluded. Percentages use the entire compact JSON request, including schemas/metadata. Custom fixtures are never uploaded and output contains aggregate measurements only; the benchmark uses compiled default configuration.

`benchmark.json` records a local Linux run on this revision's development environment. Timings were collected in a shared development environment and depend on hardware, contention and compiler settings. The suite verifies that no fixture expands. These synthetic fixtures do not establish task success, billing savings, provider cache hit rates, or live agent compatibility.

## Real sessions: required before publishing savings claims

1. Use a small representative task corpus: repeated file reads, changing files, large compiler output, no repeated content, and mixed media. Use a disposable repository. Never publish private code or credentials.
2. Fix the agent version, model, prompts, tool permissions, task inputs and provider pricing/date. Capture successful task outcomes and failures.
3. Run matched direct/bypass (`serve --no-shrink`) and shrink-enabled sessions on separate clean daemon runs. Record full provider input/output/cache-read/cache-creation usage, billed totals, end-to-end latency and task outcomes. Reset agent history and control warm/cold cache conditions; randomize run order and repeat trials.
4. Inspect `stats --json` and the local journal. Values named `estimated_*` use bytes/4 and must not be treated as provider token counts. A single transformed run cannot establish the counterfactual cost without shrinking.
5. Publish the sanitized task corpus, exact commands/versions, run counts, variability, provider pricing assumptions, outcome regressions, and cases with zero or negative cost benefit.

No live provider benchmark or real-session cost/quality result has been collected by this audit. This is a release gate for quantitative marketing, not a promise that savings will match the fixture percentages.
