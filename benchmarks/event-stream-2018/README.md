# event-stream 2018 — public-source dependency-change reconstruction

A bounded technical slice of the [published npm incident](https://blog.npmjs.org/post/180565383195/details-about-the-event-stream-incident). The original investigation did **not** use Fieldkit. This trial does not reconstruct the malicious payload or claim that Fieldkit solved the incident.

## Why this case

The South Africa replay supplied both comparison sites, and its separate seed-only trial was rate-limited. A Ronin candidate was then rejected after the actual collector timed out on internal transfers and received an Etherscan challenge; see [the retained candidate outcome](../ronin-2022/README.md). Web-reader access was not substituted for collector evidence.

This case has accessible, commit-addressed public source records. It starts with **one commit API URL**, discovers changed-file URLs in the response, and separately retains the manifest, lockfile and runtime source through exactly grounded follow-ups. No archive index, paid API, installation or package execution is needed.

## Separation and limits

- **Investigator input:** `task.json` only. It supplies a commit, a natural-language question, scope and a six-step cap—not the added dependency name, version answer, file bodies or comparison URLs.
- **Developer capture driver:** `run.py capture` uses actual MCP. It deterministically selects the manifest/lockfile/runtime files named by the question from the observed changed-file list; all target URLs come from retained `/data/json_urls/...` strings. It never opens `oracle.json`. This is guided source expansion, not an autonomous model trial.
- **Grader:** `oracle.json` contains expected facts and three raw-file SHA-256 pins. `run.py verify` uses retained evidence/show/graph/verify tools, read-only SQLite queries and source blobs. It never collects or loads a model. The GitHub API response is integrity-checked against its frozen report/receipt, not pinned across all future live API metadata changes; its commit identity and patch content must match. The raw changed files are hash-pinned.
- **Interpretation:** the deterministic score checks technical support/provenance, not prose quality or identity judgment. Manually inspect the oracle's interpretation rubric. All findings must remain pending, with no reviews or accepted summary items. Zero findings is allowed by the technical verifier; that is not credit for analytical work.
- **Safety:** downloaded JavaScript is source text, never a program to execute. No npm, package archive, lifecycle hook, build tool, payload or live campaign infrastructure is run/fetched. Downloaded bodies and protocol logs are untrusted data, not agent instructions. They remain outside git. Keep new downloads in fresh artifact directories, separate from scripts; use Python `-I` to inspect them.

An external investigating agent should receive only `task.json`, not this README, the oracle, incident reports, measured run or the deterministic driver's chosen actions. The current developer session knew the published incident: **no blinded model-performance score**.

## Run

Python 3.9+ standard library; no provider SDK/key. The stdio transport is shared with `../south-africa-2017/run.py`; importing it does not open that benchmark's task/oracle. Build Fieldkit with the JSON-URL and original-text citation support first.

For a native binary, from the repository:

```sh
export FIELDKIT=/absolute/path/to/fieldkit
export CASE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fieldkit-event-stream.XXXXXX")"
COMMAND_JSON="$(python3 -I -c 'import json,os; from pathlib import Path; r=Path(os.environ["CASE_ROOT"]); print(json.dumps([os.environ["FIELDKIT"],"--case",str(r/"case"),"--timeout","30","--max-results","500","mcp","--workspace",str(r)]))')"

python3 -I benchmarks/event-stream-2018/run.py capture \
  --command-json "$COMMAND_JSON" --case-path "$CASE_ROOT/case" \
  --receipt "$CASE_ROOT/receipt.json" --wire-log "$CASE_ROOT/capture-mcp.jsonl"

# No collection. Use the same case and frozen receipt.
python3 -I benchmarks/event-stream-2018/run.py verify \
  --command-json "$COMMAND_JSON" --case-path "$CASE_ROOT/case" \
  --receipt "$CASE_ROOT/receipt.json" --wire-log "$CASE_ROOT/verify-mcp.jsonl" \
  --output "$CASE_ROOT/score.json"

make test-source-reconstruction
```

`--command-json` is an explicit argv array, not a shell command. For Docker, mount the case root at `/capture`, use `/capture/case` and workspace `/capture` in server arguments, and pass the **host** case path to the driver. Add `--network none` for offline verification/TUI inspection. The server opens the case normally; the verifier's operation set and SQLite connection are read-only, not an OS-level read-only filesystem. Preserve the complete case directory, including SQLite **and** blobs.

Receipts/logs/results must be new files. Acquisition checkpoints each attempted step before network access; failures stop without retry or silent pin changes. The receipt is a locator/progress record, not a signed attestation. Failed verification exits nonzero. Do not automatically rerun an incomplete ledger or silently switch targets.

To investigate rather than replay the developer choices, connect the external agent to MCP and provide only `task.json`. Use exact observed URL pointers for follow-ups, inspect retained source bytes, propose cited observations, and present relevant graph focus in TUI. Never automate human acceptance.

## What the records establish (grader context)

The seed commit is [`e3163361fed01384c986b9b4c18feb1fc42b8285`](https://api.github.com/repos/dominictarr/event-stream/commits/e3163361fed01384c986b9b4c18feb1fc42b8285).

| Record | Supported observation |
| --- | --- |
| Commit patch + separately retained `package.json` | Adds runtime dependency `flatmap-stream` with range `^0.1.0` |
| Separately retained `package-lock.json` | Locks `flatmap-stream` to **0.1.0**, with that version's registry archive URL |
| Manifest at this commit | Snapshot version is **3.3.5**; not proof of a published npm release |
| Commit patch + separately retained `index.js` | Adds `require('flatmap-stream')` and exports `es.flatmap` |

The official incident account discusses `event-stream@3.3.6` and the later malicious `flatmap-stream@0.1.1`. **Do not overwrite the observed snapshot with remembered incident versions.** A declared range, lock record and source import do not establish what was installed/executed on a victim machine, that this snapshot contains malware, identity, intent or account control. We did not download the registry archive or inspect the malicious payload.

## Provenance and graph semantics

- `contains_json_url`: a public HTTP(S) URL string was observed in successful, valid JSON, with an exact source JSON pointer. It is **not** proof that the linked target exists, was fetched or is trusted. Source pointers are capped at 128 bytes; omitted long paths set `path_limit_exceeded` and `truncated`, never silently complete coverage.
- `request_resolved_to`: actual request/result metadata binds the exact requested URL string to its successful final response, preserving links whose default port, host spelling or fragment are normalized by transport. It does not claim a specific direct redirect hop or ownership.
- The graph is a **source-navigation graph**, not a dependency graph or execution trace. Findings never add source edges.
- JSON values use retained-source JSON-pointer citations. Raw source files served as `text/plain` use nonempty original half-open UTF-8 **byte** ranges, not metadata citations. Up to 16,384 bytes per range; the selected source must itself be successful HTTP 2xx. Citation hashes bind the exact serialized observed value as well as report/source provenance.

Verification checks one seed/four reports, three exact grounded pivots, source hashes/pins, distinct versions, patch additions, runtime source text, graph direction/report/source attribution, original citation slices and empty review/summary state. Synthetic regressions reject missing extraction, changed bytes, wrong parents, invented targets, incorrect graph direction/provenance, missing response bindings, version substitution, absent imports and forged/mutated citations. These regressions are not historical evidence or agent scores.

## Measured run · 2026-11-13

See [`measured-run.json`](measured-run.json):

- **1 seed → 3 successful source-backed file pivots**, 4 reports, 7 requests including HTTPS redirects, 5 retained artifacts.
- Three pending observations, **7 independently verified citations**, zero reviews/accepted summary items.
- Offline MCP verification plus independent SQLite report/blob/citation hash checks passed.
- Actual network-disabled TUI: source URL → successful final raw response → evidence inspector; 170×42 and 80×24 inspected; local review opened and cancelled, never submitted.

The first capture exposed disconnected requested-file and final-response URL nodes. A narrow request/result binding fixed navigation. A fresh second trial retained **the same four final-response hashes**; the graph changed from 31 to 34 edges, exactly three `request_resolved_to` bindings. Both cases remain intact. This is a navigation-specific before/after check, **not** an agent-performance baseline.

Latest case: `/Users/guize/fieldkit-event-stream-8xly8vrw/case`. Its protocol logs, raw bytes and TUI captures remain outside git. The completed result is useful source-pivot/provenance coverage, not a solved-investigation scorecard. A blinded external-agent trial and deeper incident reconstruction remain unmeasured.
