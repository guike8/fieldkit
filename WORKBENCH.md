# Investigate in the Rust TUI

Open a case:

```sh
./fieldkit tui --case cases/my-investigation
```

Cases created by Fieldkit's earlier Python version remain compatible. A new case is empty; the TUI never seeds demo evidence automatically.

## Agent-driven recon and relationship graph

Ask your connected terminal agent a scoped person, organization or infrastructure question. It preserves the question and explicit seeds, chooses justified collection steps, inspects results, follows exact source-backed leads and drafts findings. The agent runs outside Fieldkit through MCP; the TUI is the same persistent workspace, not a second model client. See [AGENT_USAGE.md](AGENT_USAGE.md#agent-led-recon--terminal-relationship-graph).

Press **G** for the relationship graph. It is a focused neighborhood, not a tangled whole-case diagram: typed node → directed, labeled relationships → neighboring nodes. Select a relationship to inspect its sources; **Enter** follows the neighbor, **Backspace** returns, and **o** opens original evidence. Incoming and outgoing directions remain explicit in text, independent of color. Disconnected nodes and empty cases remain visible without invented links.

An agent can call `fieldkit_graph_focus` to bring a source-backed neighborhood into the idle terminal. The TUI checks a lightweight activity token and refreshes when the case changes; it does not repeatedly reload all evidence while nothing changes. An open review/editor is never dismissed by agent focus. Ctrl+R still forces a refresh.

Graph edges mean **attributed source observations**, not verified ownership. Matching handles and different typed representations stay distinct; candidate correlations, analyst links, model assessments and investigation proposals do not become observed graph edges. Legacy claim reviews are labeled separately from local investigation approval. The case graph covers the latest 500 reports and states its limitations; keep the whole case for older captures. Hashes prove retained bytes, not source truth.

## Crypto Forensics: Ask → Explore → Review

Press **C** (uppercase, from any view) to enter a question and one explicit `ethereum:0x…` address or transaction. **Ctrl+S** saves a local crypto investigation with one pending seed step; Enter alone does not save. No provider/model call occurs on opening or saving. Addresses have 40 hex digits; transaction hashes have 64. Unsupported chains and invalid seeds fail locally.

In **w:Investigations**, **Enter** selects the child step and **g** attempts it once, sending the target to Ethereum Blockscout (up to two requests for an address, one for a transaction). There is no automatic pagination, retry or follow-up. An external MCP agent can plan further steps from exact retained chain-qualified leads, within ten total steps. **G** explores source observations, **o** inspects citations and **r** opens deliberate finding review. A pending/failed transaction's roles do not prove value moved. Provider labels do not prove identity, ownership or criminality.

This is initial Ethereum support, not multichain/bridge tracing or a full wallet history. No toolkit subscriptions or sensitive lookup services are activated. See [Crypto Forensics](CRYPTO_FORENSICS.md).

## Telegram question → cited findings

The main entry is an agent chat connected to Fieldkit MCP: ask a question in ordinary language. The agent saves the question, derives bounded historical searches, inspects results, selects exact public-post captures, and drafts cited findings. No model-provider configuration is added inside Fieldkit.

Press **w** for Investigations. **d** opens a local question/search form; the query is optional and dates/limit apply to a supplied query. **Ctrl+S saves only**, with no collection. **Enter** focuses the children (planned collection steps followed by findings). Select a pending step and press **g** to run it in the background; navigation remains available while collection finishes. No attempted step is automatically retried, even after interruption. The MCP tools handle selecting exact archived posts and proposing findings; this first TUI form is not an automatic language planner.

A plan supports up to five archive searches and ten total collection steps. Open Measures exposes historical, incomplete third-party observations with a documented six-month minimum age and 39 public queries/day. Query text leaves the machine for that provider. A selected public-page capture is separate from an archived observation and may be unavailable or changed. Neither establishes identity or historical authorship.

Select a finding and press **o** to inspect its supporting evidence. Pending proposals are not conclusions. Press **r**, choose `accepted` or `rejected`, write a rationale, then **Ctrl+S**. Enter alone never approves; Esc cancels. Every finding kind needs this review, including anything labeled an observation. Accepted findings alone enter the investigation's structured summary; the original proposal, report and captured bytes remain separate. A revised proposal needs a new review.

Approval is available only in the local TUI, not MCP/HTTP/CLI operations. This is an interface boundary, not protection from an agent with direct shell/database access; arbitrary notes and older claim/correlation reviews are not covered by the new summary gate. Citations prove retained provenance, not the truth or adequacy of an interpretation. **Ctrl+R** shows changes made by an agent while the TUI is idle.

## Start a username investigation

Press `i`, set the title and a question such as “Which public profiles explicitly reference the same project?”, then `Ctrl+S`.

Press `d` and fill these fields:

| Field | Example |
| --- | --- |
| Seed kind | `username` |
| Seed | `YOUR_HANDLE` |
| Collector job budget | `12` |
| Maximum expansion depth | `2` |
| Scope domains | Leave blank, or supply comma-separated domains |
| Username providers | `github,gitlab,bluesky` |
| Include web search | `no`, or `yes` with `BRAVE_SEARCH_API_KEY` configured |

Use Tab/Shift+Tab to navigate, Ctrl+U to clear a field, and Ctrl+S to save. Press `g` to run the selected discovery. `Enter` switches between session selection and its frontier jobs.

The first collector checks the seed. Confirmed profile URLs and explicit public links become prioritized follow-up jobs. Each follow-up records a reason, parent evidence, report hash, and structured pointer. Search-index URLs are candidates to inspect. Candidate-only generated social links do not become confirmed accounts or seed verified correlations.

Job budget counts collector attempts over the whole session, including resumed runs. A username collector can issue multiple provider requests. Global `--timeout`, `--max-requests`, `--max-results`, and `--max-bytes` constrain each collector. Depth 0 collects the seed only. Out-of-scope and excessive-depth jobs remain visible as skipped. There is no infinite crawl.

## Let NVIDIA choose the next investigation

Complete the [local Auto Ontology setup](integrations/auto-ontology/README.md) and check readiness with `z`. The actual service must have an authenticated connection, an ingested Fieldkit projection and a completed semantic compilation. A running gateway alone is insufficient.

In a new `d` form, choose planner `nvidia` and a total reasoning budget. For an existing queue, press `m`, select `nvidia`, and save with Ctrl+S. This preserves the queue and prior captures. Press `g` to run. Explicit user-supplied seeds are collected first; NVIDIA then queries the projected evidence and proposes the next public-source collection. The job and depth budgets still apply.

Press `b` to open Reasoning. Select a decision to see the public rationale, chosen command/target, original evidence references, query SQL and hashes. Press `o` for supporting captures or Enter to jump to its queued job. A rejected or unavailable recommendation is recorded and pauses the session. It does not silently revert to the local planner. Press `m` after pausing to change modes or explicitly increase the total reasoning quota; calls already attempted remain counted.

New proposals must cite an exact preserved source value and pass Fieldkit's public-collector, source-hash, pointer, provider, scope and depth checks. Model assessments are retained separately and cannot supply independent evidence for another model recommendation. Existing correlation review still requires analyst rationale and preserves distinct entities.

`p` requests a pause after the active collector. `q` requests a pause and waits for the current result to be preserved before closing. Reopen the same case and press `g` to resume pending work. A process interruption marks an in-flight job interrupted rather than silently repeating it; inspect Evidence for captures committed before interruption. Exhausted budgets remain exhausted when resumed.

## Inspect evidence and the ontology

Switch to Evidence with Tab or `4`. Running collectors already saves their reports and available response bytes. You do not need to re-enter results with `e`.

Select a record and inspect its collector status, source URL, collection time, hash scope, source references, and report hash. PageUp/PageDown scroll the inspector. Blocked, rate-limited, partial, and failed results retain those limitations. They do not prove account absence.

Ontology (`2`) shows entity types and relationship signatures learned from the captured graph. Claims (`3`) shows actual source-attributed relationships. Each claim retains supporting evidence IDs, report hashes, exact JSON pointers, and available source hashes. `y` synchronizes an older case's preserved reports. Schema versions advance when new concepts or signatures appear; source text cannot issue collector commands.

In Claims, select a row and press `o`. One citation opens directly; multiple citations open a picker. The selected evidence shows the original observation, while `v` records a separate accepted/rejected assessment with mandatory rationale. Source-attributed claims remain observations even after acceptance.

## Review correlations

Correlations (`8`) contains shared-handle and explicit shared-reference leads. Each lead includes both endpoints' supporting evidence and available structured claim anchors. Two typed representations of the same URL are excluded from correlation pairs.

Select a lead, press `o`, and inspect the supporting captures. Press `v` or `c` to accept/reject and explain the decision. Accepting a lead adds a working association; original entities stay distinct. Account existence, common references, and identity ownership are different claims.

Press `n` to add a note linked to selected evidence or claim citations. Record alternative explanations and the next observation needed.

## Manual collection, flows, and export

`t` runs a collector directly. For username collection, use command `username`, target `YOUR_HANDLE`, and options such as:

```json
{"sites":["github","gitlab","bluesky"],"include_candidates":true}
```

`e` captures a supplied observation with source context. `a` adds an analyst entity; `l` requires supporting evidence when linking two existing entities. `f` creates a research flow with its first collector, `s` adds a step, Enter selects flows/steps, and `r` executes the selected step.

`x` exports JSON or Markdown to a new filename. Existing files are preserved. JSON includes workspace data, selected collector reports, and an integrity result. Raw captured bytes remain under the case's `blobs/` directory. Keep the whole case for a complete evidence transfer.

## Keys

| Key | Action |
| --- | --- |
| `G` | Relationship graph; Enter follows a neighbor, Backspace returns, `o` opens evidence |
| `w` | Shared investigations; `d` Telegram plan, `g` run selected step, `r` review selected finding |
| `C` | Create a local Crypto Forensics plan from an Ethereum seed; no collection |
| `d` / `g` / `p` | Plan discovery / run-resume / pause |
| `m` / `b` / `z` | Configure selected planner / Reasoning view / NVIDIA readiness |
| `Tab` / `Shift+Tab` | Next / previous view |
| `1`–`9`, `0` | Jump to a view |
| `↑` / `↓`, `j` / `k` | Select a row |
| `Enter` | Switch parent/child tables |
| `o` | Open supporting evidence |
| `v` / `c` | Review selected evidence/claim, or correlation |
| `y` | Synchronize ontology from stored reports |
| `t` / `e` | Run collector / capture evidence |
| `a` / `l` / `n` | Add entity / link / analyst note |
| `f` / `s` / `r` | Create flow / add step / run step |
| `i` / `x` | Edit question / export |
| `/` / `Esc` | Filter / clear filter or close a form |
| `Ctrl+R` | Refresh the shared case |
| `Ctrl+S` | Save a form |
| `?` / `q` | Help / quit after preserving active collection |
