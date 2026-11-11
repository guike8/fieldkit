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

Run `fieldkit tui --case cases/example` to inspect the same state. See [WORKBENCH.md](WORKBENCH.md) for keys and [MCP.md](MCP.md) for agent tool names and configuration. The browser backend is native Rust too: `fieldkit --case cases/example app --workspace "$PWD"`.
