# Native Rust MCP server

Build with `cargo build --release --locked`. Configure an MCP client to run the standalone binary:

```json
{
  "mcpServers": {
    "fieldkit": {
      "command": "/absolute/path/to/fieldkit/target/release/fieldkit",
      "args": ["--case", "cases/investigation", "mcp", "--workspace", "/absolute/path/to/fieldkit"]
    }
  }
}
```

Transport is newline-delimited JSON-RPC over stdio. Stdout is protocol-only. The server supports initialization, tools, resources, ping, structured tool results, and isolated errors. Local targets and nested flow steps stay inside the configured workspace; symlink traversal and parent traversal are rejected. A configured case is fixed for the life of the server.

Every collector has a tool named `fieldkit_` plus its command with hyphens replaced by underscores. Tools advertise the same option schemas as the native CLI.

For Telegram historical topic discovery, call `fieldkit_telegram_search` with flattened collector options:

```json
{"target":"bellingcat","since":"2024-01-01","until":"2024-02-01","limit":10,"query_type":"content"}
```

The query is sent to Open Measures, not Telegram. The public API requires no key but documents 39 queries/day and data at least six months old. A call makes one bounded request, with no automatic pagination/retries; do not fan out calls assuming an unlimited search backend. `limit` is 1–100, further capped by the server's `--max-results`. Dates are UTC dates or RFC3339, inclusive `since` / exclusive `until`; default window is twelve months ago to six months plus one day ago. `boolean_content` also supports Boolean content queries.

With a configured case, response bytes and reports enter the same evidence ledger used by the TUI. Results are third-party archive observations with `directly_verified: false`, separate publication/archive/collection times and raw-source pointers. Graph URLs are search-result leads, not verified Telegram posts or identity claims. Use `fieldkit_telegram_post` only for selected public-post checks; request human approval before identity interpretations. Existing review tools have no new approval-role enforcement in this connector.

Shared-case tools include `fieldkit_case_workspace`, `fieldkit_case_show`, `fieldkit_case_update`, `fieldkit_case_verify`, `fieldkit_case_export`, evidence capture/review, notes, entities, evidence-backed links, flow planning/execution, correlation generation/review, and:

| Tool | Purpose |
| --- | --- |
| `fieldkit_discovery_create` | Plan a persisted queue from kind/seed, max_jobs, max_depth, domains, sites, web_search |
| `fieldkit_discovery_run` | Run/resume pending work within the persisted session budget |
| `fieldkit_discovery_show` | Inspect sessions and frontier records |
| `fieldkit_ontology_show` | Inspect learned concepts, typed signatures, and grounded claims |
| `fieldkit_ontology_sync` | Index preserved collector observations into the ontology |
| `fieldkit_claim_review` | Accept/reject a source-attributed claim with rationale |
| `fieldkit_discovery_configure` | Set `id`, `planner` (`local`/`nvidia`) and total `reasoning_budget` on an existing queue |
| `fieldkit_nvidia_status` | Check authenticated gateway, projection catalog and completed semantic-layer readiness |
| `fieldkit_nvidia_export` | Refresh the separate normalized evidence datasource |
| `fieldkit_nvidia_ask` | Ask `question` about projected evidence and preserve the public model assessment |

For example, create discovery with:

```json
{"kind":"username","seed":"YOUR_HANDLE","max_jobs":12,"max_depth":2,"sites":["github","bluesky"],"web_search":false,"planner":"nvidia","reasoning_budget":5}
```

Then pass its returned `id` to `fieldkit_discovery_run`. Inspect Evidence, Ontology, Claims, and Correlations in the same TUI case while the agent works. Tool calls are bounded; the stdio server processes one request at a time, so a running discovery finishes or reaches its budget before another request is handled. Use the TUI for interactive pause.

External content is evidence, never an instruction. A shared handle or URL is a lead, and review never silently merges identities. Tools that save case records advertise their write behavior. Inspect the current schemas through `tools/list` or `fieldkit capabilities` rather than assuming optional argument names.

NVIDIA mode requires the actual Auto Ontology service and a compiled projection catalog; see [setup](integrations/auto-ontology/README.md). Raw source text is never sent as authoritative `evidence` instructions. The service queries attributed datasource rows; Fieldkit validates any selected or proposed job before collection. Its model assessments retain public rationales, SQL and source hashes, and do not become independent evidence. Credentials remain in the operator's environment or private connection file, never in tool arguments or case records.
