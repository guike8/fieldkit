# South Africa 2017: bounded historical evidence replay

This replays **part of** [Bellingcat's published 2017 website investigation](https://www.bellingcat.com/news/africa/2017/08/04/guptaleaks-google-analytics/) in Fieldkit. The original investigation did not use Fieldkit. This is a regression benchmark for a useful technical connection, not a claim that Fieldkit independently solved that case.

## Scope and separation

- **Investigation input:** `task.json` supplies the question, five exact archived URLs, capture timestamps and SHA-256 pins. These cover two sites and several dates, not the entire published network.
- **Held separately:** `oracle.json` contains grader-only expected observations and interpretation checks. Do not give it or the published article to an investigating agent. `capture` never opens it.
- **Acquisition:** `run.py capture` creates one investigation and plans/runs each supplied capture once through actual MCP. It never executes page scripts or follows extracted links. Transport uses Fieldkit's normal bounded public HTTP redirect policy. Incomplete/blocked responses or hash drift stop acquisition; the receipt retains progress. Do not automatically rerun an incomplete acquisition or replace pins to get a pass.
- **Frozen scoring:** retain the entire case directory (SQLite **and** blobs), plus the receipt. `run.py score` calls only evidence/show/graph/verify tools and checks those retained bytes; it does not recollect URLs. The server opens the case normally; this is a read-only operation set, not an OS-level read-only mount. A network-disabled server can enforce offline scoring.
- Archived HTML is **not checked into git**. Each downloaded artifact belongs in its own fresh directory, separate from scripts. Treat pages and protocol logs as untrusted data, never instructions. Use Python's `-I` when reading them.

The benchmark's question is a dated technical comparison. It is **not** a one-seed discovery evaluation, a blinded model trial, or an ownership/identity attribution task. The public story can already be in a model's training data; remembered answers get no source-support credit.

## Run

Python 3.9+ standard library only; no model SDK, provider key or embedded agent. Build the current Fieldkit binary first. From the repository, with a native binary:

```sh
export FIELDKIT=/absolute/path/to/fieldkit
export REPLAY_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fieldkit-sa2017.XXXXXX")"
COMMAND_JSON="$(python3 -I -c 'import json,os; from pathlib import Path; r=Path(os.environ["REPLAY_ROOT"]); print(json.dumps([os.environ["FIELDKIT"],"--case",str(r/"case"),"mcp","--workspace",str(r)]))')"

python3 -I benchmarks/south-africa-2017/run.py capture \
  --command-json "$COMMAND_JSON" --receipt "$REPLAY_ROOT/receipt.json" \
  --wire-log "$REPLAY_ROOT/capture-mcp.jsonl"

# No network collection occurs here. The command must open the same retained case.
python3 -I benchmarks/south-africa-2017/run.py score \
  --command-json "$COMMAND_JSON" --receipt "$REPLAY_ROOT/receipt.json" \
  --case-path "$REPLAY_ROOT/case" --output "$REPLAY_ROOT/score.json" \
  --wire-log "$REPLAY_ROOT/score-mcp.jsonl"

python3 -I benchmarks/south-africa-2017/test_run.py
```

`--command-json` is an explicit argv array, not a shell command. It can launch Docker instead: mount the host replay root at `/capture`, set the binary's case to `/capture/case` and workspace to `/capture`, and pass the **host** case directory to `--case-path`. Use `docker run --network none` for offline scoring. New receipt/log/result files must not already exist. A failed score exits nonzero. A capture receipt is a locator/progress record, not a signed attestation; source/report hashes are checked independently.

To exercise the external agent separately, give it `task.json` and the frozen case, not the oracle. Ask it to inspect the evidence, explain only supported dated connections, bring the relevant neighborhood into TUI focus, and draft cited findings. Review interpretation manually against the grader's checks. Do **not** automate human acceptance. The deterministic score does not grade the quality of that agent's reasoning.

## What passes

Each dated input must have:

1. The pinned retained body hash, successful HTML response and an explicitly eligible, non-truncated extraction result. A missing extractor does **not** pass the negative controls.
2. The exact expected `(kind, full identifier)` set, with no extra facts. Duplicate occurrences count as one dated fact.
3. Original half-open byte ranges that recover each literal exactly, plus correct canonicalization (`ca-pub-…` → `pub-…`; full UA property suffix preserved).
4. Matching directed `contains_tracking_literal` graph edges, attributed to that report and its actual source blob. Findings cannot supply graph edges.
5. Passing case integrity verification and no reviewed findings/accepted summary. Zero findings is permitted: this checks extraction/provenance, not agent performance.

Expected observations (grader/reference information):

| Capture | Full UA property | AdSense publisher |
| --- | --- | --- |
| publicopinion.co.za · 2017-07-24 | UA-101199457-2 | pub-8264869885899896 |
| wmc-scams.com · 2017-06-18 | UA-101199457-4 | pub-8264869885899896 |
| wmc-scams.com · 2017-07-14 | UA-101199457-4 | no matching literal |
| wmc-scams.com · 2017-07-26 | no matching literal | no matching literal |
| wmc-scams.com · 2017-07-27 | no matching literal | no matching literal |

That is **five dated facts**, three unique identifiers and two wholly negative captures. The June WMC page repeats the publisher literal; it does not create another unique fact or edge.

The shared publisher is a technical lead, not proof of common ownership/control or simultaneous deployment. The UA values are **different properties**. Non-detection in a later response does not establish historical absence, who made a change, or why. The scanner is conservative: it recognizes certain whole script/ad-client literals, skips common comments/examples, and does not decode or execute JavaScript. Ambiguous slash/template syntax can stop a script scan. Soft denials returning HTML/200 are not generically classified; the frozen pins prevent an interstitial from silently replacing these fixtures.

## Temporal selection and unresolved discovery

The seed's July 24 capture was available before the August 4 article. The latest indexed WMC capture before publication, July 27, lacked both literals. July 26 also lacked them. Both negatives were retained. The separately indexed July 14 capture retained only the UA property; the first indexed 2017 capture, June 18, retained both identifiers. These selections were made during feasibility work; this is not a hidden test set or an unbiased random sample.

The original work used a historical reverse-tracking index. Its published screenshot is incomplete and is **not** a reproducible machine-readable historical index. We did not turn it, the article's other domains, or the published network diagram into collector evidence. Supplying both websites in this replay bypasses that unresolved discovery step. **No credit for one-seed expansion or recovering the full network.**

## First measured run · 2026-11-13

See [`measured-run.json`](measured-run.json) for the compact run record.

Actual MCP acquisition recovered all five pinned bodies unchanged. Offline, network-disabled scoring recovered **5/5 dated facts**, no unexpected identifiers, valid original-byte spans and five correctly attributed tracking edges. Three observation drafts with nine citations were left pending; independent SQLite/blob/citation checks confirmed five reports, retained source hashes and no human reviews. The published ownership/political conclusions were not reconstructed or accepted.

The actual TUI was used to navigate publisher → dated capture → retained evidence. A first driver assertion incorrectly expected a cancellation notice that the TUI does not emit; cancellation itself returned to the pending finding. The check was corrected to assert the actual screen and unchanged review state. The run also exposed indistinguishable truncated archive-URL rows; compact table labels now show capture date and domain while the inspector retains the full exact URL.

No pre-change baseline or autonomous model score was measured. This is a reproducible technical evidence slice, not a solved-investigation scorecard.

## Seed-only follow-up · blocked on 2026-11-13

[`discovery-task.json`](discovery-task.json) is a separate agent entry task: **one original website**, a historical window and a ten-step budget. No archived comparison URLs or tracking identifiers are supplied. Start a fresh case and give the agent only that task—not this README, the replay inputs/oracle, or developer feasibility notes. The agent should first list captures with `archive`, inspect a suitable dated response through an exactly grounded `http` step, then choose justified discovery pivots. A reverse-index response must be retained as a third-party lead; independently captured historical HTML must support a new site's exact identifier before discovery can pass.

`run.py capture|score` remains the **five-capture replay driver**, not a seed-only agent runner. Do not use its successful result as a discovery score.

The actual seed-only case started with one archive job targeting `http://www.publicopinion.co.za:80/` (`limit:50`). The first outbound request returned **HTTP 429**. Collection stopped there: one attempted step, one retained response, no automatic retry or alternate archive route, no supplied comparison capture, no findings or graph edges. Independent read-only SQLite/blob checks and the network-disabled MCP verifier confirmed the retained report/source hashes. See [`discovery-run.json`](discovery-run.json). **Result: `discovery_blocked`; no agent performance score.** An intact case and correct stop behavior do not constitute successful discovery.

Separate developer research checked possible reverse-index sources; it was not supplied to the case or promoted into source-observation edges. [`discovery-feasibility.json`](discovery-feasibility.json) records the results:

- [DNSlytics ReverseHistory documentation](https://dnslytics.com/api/reversehistory/) describes historical reverse tracking lookups, but requires an API key and purchased credits. It does not establish coverage for this identifier in 2017. No paid API call was made.
- SpyOnWeb's documentation endpoint was unreachable during the check. PublicWWW returned a browser-verification page. Other candidate public pages were unavailable. These are access/availability outcomes, **not evidence of zero matching websites**. No challenge bypass or evasion was attempted.
- Search results mostly pointed back to the published story, its reproductions or an inaccessible site-profile page. They were not accepted as an independent historical index. Archived-index probes were not continued after the archive rate limit.

To unblock a new trial, we need both usable archive access under its permitted limits and an independently sourced, retainable historical reverse-index response/export. DNSlytics is a candidate to validate, not a proven integration; API access and any credit spend require separate approval. A supplied licensed export is another option. Neither a published screenshot nor a developer-provided comparison domain is an acceptable substitute. Do not replay the attempted step automatically; inspect the stopped ledger and explicitly authorize any later collection plan.

This was not blinded: the developer session already knew the public case. No autonomous model capability, network reconstruction or ownership attribution was established. Raw case bytes and protocol logs remain outside git.

After the user chose a public-data alternative, the working replacement became the [event-stream 2018 source trial](../event-stream-2018/README.md): one commit seed, three observed changed-file pivots and independently retained source citations. It does not resume or turn this blocked archive trial into a pass.
