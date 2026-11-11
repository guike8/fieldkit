# Fieldkit

Fieldkit is a native Rust OSINT CLI, terminal workbench, local web app, and MCP server. It collects public-source observations into a local case, builds an evidence-backed graph and evolving ontology, and runs bounded discovery from usernames, public profiles, URLs, domains, or search queries.

## Run the terminal workbench

Build with stable Rust:

```sh
cargo build --release --locked
./fieldkit tui --case cases/my-investigation
```

Or install the standalone binary:

```sh
cargo install --path . --locked
fieldkit tui --case cases/my-investigation
```

The runtime is Rust throughout: collectors, SQLite storage, discovery, ontology, TUI, HTTP server, and MCP. The browser uses embedded HTML/CSS/JavaScript. Prior Python/C++ implementations are archived under `legacy/` for reference and are never invoked by the Rust runtime.

## Username → discovery → evidence → correlations

1. Press **i** to set a case title and research question. **Ctrl+S** saves forms; **Tab** moves between fields.
2. Press **d**. Keep seed kind `username`, enter a handle, choose a job budget and depth, and optionally restrict providers, such as `github,bluesky`. Save with **Ctrl+S**.
3. Press **g** to run. Each collector report and captured source enters Evidence; confirmed profiles and explicit links add follow-up jobs with reasons and supporting evidence. The queue is deduplicated and persisted.
4. Press **p** to pause after the current collector. Reopen the same case and press **g** to resume pending work within the original budget.
5. Open **Ontology** to inspect observed entity types and relationship signatures. **Claims** shows captured relationships with source references, exact JSON pointers, and report hashes. The ontology version advances when its vocabulary grows.
6. Open **Correlations**, select a lead, and press **o** to inspect its supporting evidence. Press **v** to accept or reject with a rationale. A shared handle is a weak lead; accepted leads preserve distinct entities.
7. Press **x** to export JSON or Markdown to a new file.

Use **Tab/Shift+Tab** to switch views; **1–9/0** jump directly, **↑/↓** select records, **Enter** switches between sessions/jobs or flows/steps, and **?** opens the full keyboard guide. **t** runs one collector; its results are saved automatically.

Discovery defaults to explicit public links and structured collector observations. Optional web search requires `BRAVE_SEARCH_API_KEY`. Domain scope is optional; when set, excluded follow-ups remain visible as skipped jobs. Sessions accept 1–100 collector jobs and depth 0–4. A job may make several HTTP requests, each constrained by the global request, byte, and timeout settings. Generic navigation links have lower priority than observed profiles and outbound profile links.

## Agent interface

```sh
fieldkit capabilities
fieldkit --case cases/example username HANDLE --sites github,gitlab,bluesky
fieldkit --case cases/example run examples/username.json
fieldkit --case cases/example discover HANDLE --sites github,bluesky --jobs 12 --depth 2
fieldkit --case cases/example ontology
fieldkit case verify cases/example
fieldkit --case cases/example mcp --workspace "$PWD"
```

`run` accepts a JSON job or `-` for stdin. `batch` accepts up to 100 JSONL jobs. Results preserve the Fieldkit `schema_version: "1.0"` envelope and explicit collector statuses. Use `--options '{"sites":["github"]}'` for JSON options or the corresponding named flags. Array flags take comma-separated values.

The MCP server exposes every collector plus shared case, flow, discovery, ontology, claim-review, export, and integrity tools. See [MCP.md](MCP.md) and [AGENT_USAGE.md](AGENT_USAGE.md).

## Collectors

- People: `username`, `profile`, `person`, optional Brave `search`.
- Public infrastructure: `dns`, `rdap`, `tls`, `ct`, `http`, `archive`, `email`, composed `domain`.
- Local sources: `file`, `extract`, `telegram-export`.
- Telegram: `telegram-profile`, `telegram-messages`, `telegram-post`, `bellingcat-telegram`.
- Diagnostics: `doctor`, `integrations`.

Username adapters cover GitHub, GitLab, Codeberg, DEV, Lichess, Chess.com, Hugging Face, Hacker News, Reddit, Bluesky, Mastodon, Steam, and YouTube. Provider-specific identity/schema checks and fresh negative controls gate a `found` result. Other platform URLs remain unverified candidates. Public visibility, access restrictions, rate limits, and provider changes remain visible in results.

## Local browser app

```sh
fieldkit --case cases/example app --workspace "$PWD" --port 8765
```

Visit http://127.0.0.1:8765/. The native server embeds the browser assets and shares the same case with the TUI and agents. Collection runs in bounded background jobs. Discovery and ontology also have native HTTP APIs; the complete interactive discovery workflow is in the TUI.

## Evidence and ontology

Captured response bytes are retained under SHA-256 in `CASE/blobs/`, with immutable reports in `CASE/case.sqlite3`. Source publication time, collection time, collector status, analyst assessment, and correlation review remain separate. Local file/extract inputs retain hash-only references; importing an attachment explicitly preserves its bytes. Public Telegram previews are bounded observations, not complete channel histories.

The ontology is currently learned deterministically from preserved structured collector output. It records concepts, typed relationship signatures, and source-attributed claims. It does not perform free-form LLM extraction from arbitrary documents. NVIDIA Auto Ontology is an optional future semantic-service integration; no NVIDIA service or model is required or silently contacted by this release.

A correlation records support for both endpoints. Structured claims cite the exact relationship in a preserved report, its SHA-256, and available source hashes. Shared handles and links never automatically establish a person's identity or account ownership. `case verify` checks retained report/artifact hashes and the workspace activity chain; hashes do not certify source truth. Original captures survive reviews and rejected leads.

Rust opens existing v1 SQLite case ledgers and workspace records in place. New discovery/ontology records use additional workspace kinds. The active case view and ontology synchronization inspect the latest 500 reports; ontology records persist across synchronizations and expose their limits. Exports include the selected collector reports; raw source blobs remain in the case directory. Preserve the complete directory when transferring an investigation.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
node tests/frontend_test.mjs
cargo build --release --locked
```

Tests cover identity checks and negative controls, bounded source parsing, local scope, report/artifact integrity, legacy case compatibility, discovery deduplication and recovery, ontology anchors and reviews, native MCP, HTTP guards, and Ratatui interaction/rendering.

See [WORKBENCH.md](WORKBENCH.md) for the investigation walkthrough and [THIRD_PARTY.md](THIRD_PARTY.md) for integration provenance.
