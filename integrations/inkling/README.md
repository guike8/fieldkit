# Inkling as the Fieldkit agent

A standalone Rust 2024 workspace package, `fieldkit-agent`, talks to the existing
Rust stdio MCP server. It provides a reusable library and the `fieldkit-agent` and
`fieldkit-agent-benchmark` binaries; no Python interpreter is needed.
No provider SDK, NVIDIA gateway changes, shell tool, generic collector tool, or
finding-approval tool is added. Model/provider selection remains interchangeable.
Default model: `thinkingmachines/Inkling-2-Small`; default OpenAI-compatible API:
Together (`https://api.together.ai/v1`). Hosted availability, account access and
function-calling support must be checked with your provider. Local replay passing
does **not** establish that the hosted model works or is a good investigator.

## 1. Offline checks (no account, no external network)

From the repository root:

```sh
cargo build --locked --workspace
cargo test --locked --workspace
cargo run --locked -p fieldkit-agent --bin fieldkit-agent-benchmark -- --mode replay
```

The unit tests cover the chat request contract with a loopback HTTP server,
protocol failures, budgets, restricted tools, redirect/key protection, preserved
operator scope, and native MCP planning/proposal rejection. The native tests use
`target/debug/fieldkit`; set `FIELDKIT_TEST_BINARY` to another **current** build if
needed. Without a binary they explicitly print `SKIP native MCP integration` (use
`cargo test -p fieldkit-agent -- --nocapture` to see captured diagnostics). An old
binary missing investigation tools fails rather than pretending compatibility.
The stdio transport uses nonblocking Unix pipes and deadline-bounded reads **and
writes**; Linux and macOS are supported. Tests use native Rust, synthetic tools,
loopback mock HTTP servers and disposable shell MCP stubs, never host Python.
Several related negative cases are grouped into Rust tests; test counts and token
counts are not advertised as numerically equivalent to the former implementation.

The benchmark's default `--mode replay` uses scripted reference decisions and
synthetic tool results. This validates the **harness**, not Inkling's quality.
It never opens a real case or performs real collection. Standard output contains a
summary; `--output NEW_PATH.json` retains the full report including tool traces.

## 2. Live model benchmark (synthetic data only)

Export `INKLING_API_KEY` in your local shell using your provider's secret-management
workflow; do not paste it into chat, commit it, or pass it as a CLI argument. Then:

```sh
cargo run --locked -p fieldkit-agent --bin fieldkit-agent-benchmark -- --mode live \
  --output /tmp/fieldkit-inkling-live-01.json
```

This sends synthetic questions, tool schemas and simulated observations to the
configured model provider and incurs API charges. It does **not** connect to MCP,
read existing case data, contact collection targets, or approve findings. Choose a
new output filename on each run; reports are created privately and never overwrite.

For DeepInfra (with the corresponding provider key):

```sh
cargo run --locked -p fieldkit-agent --bin fieldkit-agent-benchmark -- --mode live \
  --base-url https://api.deepinfra.com/v1/openai \
  --api-key-env DEEPINFRA_API_KEY \
  --output /tmp/fieldkit-inkling-deepinfra-01.json
```

`INKLING_BASE_URL` and `INKLING_MODEL` override defaults. `--model` allows a baseline
model on the same compatible provider. Keep scenarios, budgets and generation
settings fixed when comparing. Start with at least three separately saved live
runs; inspect every failure and tool trace. These small deterministic scenarios
are regression probes, not a comprehensive capability or safety certification.
The grader checks tool actions/state and citation structure; it does not prove
semantic entailment or reliably detect every unsupported identity claim in prose.

## 3. Connect to an actual shared case

**Real investigation questions, tool schemas and returned evidence are disclosed to
the configured inference provider.** Use an approved endpoint/data policy. Start
with a new disposable case, not a private existing investigation. Collection is
disabled by default; even plan-only mode writes a local investigation and plans.

```sh
cargo run --locked -p fieldkit-agent --bin fieldkit-agent -- \
  --binary "$PWD/target/debug/fieldkit" \
  --workspace "$PWD" --case "$PWD/cases/inkling-trial" \
  --mode infrastructure --seed example.org \
  'What public infrastructure is observable for example.org?'
```

The actual-case runner accepts `--mode person|organization|infrastructure|crypto`. Crypto mode requires explicit `ethereum:0x…` address/transaction seeds and the engine's separate crypto collector allowlist. It is still plan-only unless `--allow-collection` is supplied; plan-only execution **does** call the configured model provider. See [Crypto Forensics](../../CRYPTO_FORENSICS.md). The six existing scripted replay scenarios do not evaluate crypto tracing or live provider availability.

Add `--allow-collection` only to authorize bounded public collection for that scope.
View the same case using the existing TUI:

```sh
./target/debug/fieldkit tui --case cases/inkling-trial
```

The runner preserves the exact question/mode/seeds, permits one new investigation,
rejects other investigation IDs, and never repeats an attempted step. Fieldkit
still checks exact observed-lead grounding, retained hashes, citation pointers and
the ten-step investigation budget. An observed lead is not scope permission.
Only investigation/graph tools and the read-only collector-schema catalog are
advertised. `fieldkit_investigation_source` reads verified retained UTF-8 bodies
in chunks of at most 16 KiB, bound to this investigation's step/source IDs; it
returns original byte offsets and an explicit continuation, never filesystem paths.
The agent must read retained text before citing body content. Neither metadata nor
a synthetic fixture's expected answer substitutes for that read. No shell, local file reader,
notes, legacy review operations, or provider-generated code can run. All findings
remain proposals for trusted local TUI review. This is not an OS sandbox: protect
the case/database from unrelated processes with direct filesystem access.

Limits default to 12 model turns, 24 tool calls, 2,048 completion tokens/request,
32,768 cumulative reported input+output tokens, and 90-second request timeouts.
Requests/tools are sequential and never automatically retried. The token limit is
checked **after** each response, so one request can overshoot it; it is not a hard
billing cap. Budget exhaustion exits 2; protocol/transport failure exits 1. A normal
final response exits 0, not a claim that the investigation or benchmark succeeded.
Timeouts can leave action outcome uncertain: inspect the saved ledger before any
new run. Oversized results stop rather than silently truncating source evidence.
Provider errors do not print response bodies or credentials; redirects are refused.

Optional `--output NEW_PATH.json` retains the full tool trace with mode 0600. Treat
real-case traces as sensitive evidence. Without it the runner prints only the final
answer, public metrics and investigation ID; it does not store chat transcripts in
the case or turn model answers into independent source evidence.

## Provider references

- [Inkling model card](https://huggingface.co/thinkingmachines/Inkling-2-Small)
- [Together model page](https://www.together.ai/models/inkling-2-small)
- [DeepInfra model/API](https://deepinfra.com/thinkingmachines/Inkling-2-Small/api)

Fine-tuning and local weight serving are deliberately outside this integration.
