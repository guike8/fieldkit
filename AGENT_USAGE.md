# Fieldkit agent usage

The standalone Rust binary provides JSON collection, a shared evidence workspace, MCP tools, and bounded discovery.

```sh
fieldkit capabilities
fieldkit doctor
fieldkit --case cases/example username HANDLE --sites github,bluesky
fieldkit --case cases/example run - < job.json
fieldkit --case cases/example batch jobs.jsonl
fieldkit --case cases/example discover HANDLE --sites github,bluesky --jobs 12 --depth 2
fieldkit --case cases/example ontology --sync
fieldkit --case cases/example nvidia status
fieldkit --case cases/example discover HANDLE --planner nvidia --reasoning-budget 5
fieldkit case verify cases/example
fieldkit --case cases/example mcp --workspace "$PWD"
```

A collector job has `command`, optional `target`, and `options`:

```json
{"command":"username","target":"YOUR_HANDLE","options":{"sites":["github","bluesky"],"include_candidates":false}}
```

Every collector returns a stable report containing `run_id`, `command`, `target`, `status`, `data`, `sources`, `entities`, `relationships`, `errors`, and `timing`. With `--case`, captured source bytes and the immutable report are retained. Exit 0 means ok/not_found; unresolved collector outcomes return 2; invalid CLI/job transport returns 1.

Discovery follows structured observations and explicit links. Plan without collecting using `discover ... --plan-only`; resume the returned session with `discover --resume DISCOVERY_ID`. Job/depth budgets persist across restarts. Web search is opt-in and requires `BRAVE_SEARCH_API_KEY`. Candidate URLs and search snippets remain unverified leads. Blocked or failed lookups do not prove absence.

The ontology records observed concepts, relationship signatures, and source-attributed claims. Claims preserve exact report pointers and hashes. Correlations retain support for both endpoints and require rationale when reviewed. A generated schema definition or an accepted lead is not independent source evidence or identity attribution.

The optional NVIDIA planner uses the real Auto Ontology gateway over a separate read-only projection. Configure it through [the local setup](integrations/auto-ontology/README.md) or operator environment variables. NVIDIA recommendations choose existing pending jobs or propose allowed public collectors grounded in exact report pointers and hashes. The cumulative reasoning quota counts questions, including failed/interrupted attempts; collector budgets remain separate. Unavailable or invalid suggestions pause the session. Model assessments are retained for review and excluded from subsequent source-evidence projections. Use `nvidia ask "QUESTION"` for a read-only semantic query; it preserves the public answer but executes no investigation collector.

## Question → Telegram investigation → reviewed findings

Use the investigation tools for a topic investigation, rather than loose notes or an untracked series of collectors. The connected agent interprets the user's natural-language question; Fieldkit stores the plan and evidence without adding a model SDK or changing the existing NVIDIA integration.

1. `fieldkit_investigation_create`: preserve the original `question` and up to five `searches` (ordinary `telegram-search` jobs). Derive narrow queries and explicit historical dates; do not pretend the user's entire question is a search planner. Saving makes no network requests.
2. `fieldkit_investigation_run_step`: pass investigation `id` and planned `step_id`. Each step is attempted once, with no automatic retry. Read its actual collector status.
3. `fieldkit_investigation_evidence`: inspect the full saved report for `id`/`step_id`, including messages, limitations and raw-source references. This lookup also works beyond the latest-500-report case view.
4. `fieldkit_investigation_select_post`: pass `id`, search `step_id`, and zero-based result `index`. This plans the exact public URL from that archived observation; run the returned step separately. At most ten collection steps, including searches, fit in one investigation. A current embed may be unavailable or differ from the archive.
5. `fieldkit_investigation_propose`: submit `id`, `content`, `kind` (`observation`, `hypothesis`, `identity_sensitive`), and 1–20 `citations`, each `{ "step_id": "…", "pointer": "/data/messages/0" }` for an archive observation or `/data` for a public-post capture. Fieldkit checks case membership, pointers, report and retained-source hashes. Citations are support to inspect, not semantic proof of the claim.
6. `fieldkit_investigation_show`: distinguish pending proposals from `summary`, which contains only locally accepted findings. Tell the user to review in TUI **w → Enter → select finding → r → Ctrl+S**. Do not report pending identity interpretations as conclusions.

Every kind requires local human review, so labeling an identity interpretation as an observation cannot bypass approval. Proposals and reviews are separate from source evidence and never generate identity/ownership graph edges. Findings cannot be edited through agent tools; make a new proposal for a revision, which needs its own review.

**Boundary:** approval is absent from MCP and HTTP operations. The trusted local TUI is the only supplied review interface. This is not a sandbox against an agent with shell, Rust-library or database access to the case. Existing generic notes, links and claim/correlation reviews remain separate surfaces; they are not an approved investigation summary. There is no semantic classifier that can detect identity language in arbitrary free text. Do not use those surfaces to evade the review policy.

If interrupted after a step starts, its outcome may be unknown. Inspect the ledger before planning a new investigation; an attempted step is never silently replayed. Other pending steps remain usable. Refresh an idle TUI with Ctrl+R after agent changes.

## Telegram archive discovery

For a Telegram topic question, `telegram-search` can discover historical leads before selecting exact public posts for inspection:

```sh
fieldkit --case cases/example telegram-search "bellingcat" --since 2024-01-01 --until 2024-02-01 --limit 10
```

This sends the query to Open Measures' public API. Its documented public limits are 39 queries/day and data at least six months old. Do not use it for recent/live coverage or interpret an empty query as Telegram-wide absence. One request runs per invocation, without automatic retries/pagination; Fieldkit surfaces rate limits rather than tracking a local daily allowance. Use explicit inclusive `since` and exclusive `until` dates for reproducibility. Default window: twelve months ago to six months plus one day ago; optional `query_type` is `content` (default) or `boolean_content`.

Keep `provider_archive_observation`, original source IDs/pointers/hashes, publication time, provider archive time and Fieldkit collection time distinct. Search-result URLs are unverified leads; `telegram-post` can inspect a selected public post separately. Do not infer ownership, authorship, criminal involvement or identity from channel labels, shared URLs or matching handles. Seek human approval before proposing identity-related conclusions; this connector itself emits none and does not add permission separation to existing case-review tools.

Run `fieldkit tui --case cases/example` to inspect the same state. See [WORKBENCH.md](WORKBENCH.md) for keys and [MCP.md](MCP.md) for agent tool names and configuration. The browser backend is native Rust too: `fieldkit --case cases/example app --workspace "$PWD"`.
