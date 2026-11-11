# Investigate with Fieldkit

The browser app, terminal UI, CLI, and MCP tools share a local case. A case combines preserved collector reports and source artifacts with an investigator's question, assessments, notes, entity links, research flows, and reviewed correlation leads.

## Open one case across interfaces

Install from the public repository with Python 3.11 or later:

```sh
git clone https://github.com/guike8/fieldkit.git
cd fieldkit
python3 -m venv .venv
. .venv/bin/activate
python -m pip install -e '.[mcp,telegram,tui]'
mkdir -p investigations
```

Start the browser app and visit **http://127.0.0.1:8765/**:

```sh
fieldkit app --workspace "$PWD/investigations" --case cases/example --port 8765
```

In another terminal, activate the same environment and open the same case:

```sh
fieldkit tui --case "$PWD/investigations/cases/example"
```

Agents can use that case through MCP:

```sh
fieldkit-mcp --workspace "$PWD/investigations" --case cases/example
```

The console entry points `fieldkit-app` and `fieldkit-tui` accept their respective subcommand options. The app binds to loopback and serves its browser interface and API from one origin. Its request token and origin checks keep workspace changes within that local session.

For a guided sample, choose a separate empty case and add `--demo`:

```sh
fieldkit app --workspace "$PWD/investigations" --case cases/training --demo
fieldkit tui --case "$PWD/investigations/cases/training" --demo
```

Harbor Signal is explicitly synthetic training material. Demo seeding is opt-in and idempotent; it does not replace a real investigation that already contains evidence.

## A practitioner workflow

### 1. Frame the question

Set the case title and a specific research question. Record what evidence would support it and an alternative explanation. For example: “Which public records link to the same project site?” is more testable than assuming two similarly named accounts have one owner.

Use case details in the app or `i` in the TUI. Add an analyst note for assumptions, scope, and next questions.

### 2. Capture evidence with context

Capture a supplied observation with its title, text, source URL, and any source time you know. Run a collector when you need a current public-source observation. Attachments added in the local app retain their captured bytes under a content hash rather than a user-controlled storage filename.

Collector results enter the same evidence list, retaining run IDs, source references, collection times, statuses, and hash scope. The inspector shows the record and its provenance. Source publication time and collection time are separate observations; do not substitute one for the other.

Native username evidence hashes the Handlecheck engine's JSON report. Python HTTP sources hash bounded captured response bytes. Local `file`/`extract` sources are hash-only references; importing an attachment through the workbench explicitly captures its body. The browser app makes the available evidence and collection method visible.

### 3. Assess what the record supports

Review each item as **unreviewed**, **corroborated**, **contested**, or **discarded** and record the reason. Include limitations such as an inaccessible original, a partial preview, an unsupported handle, or contradictory dates. These are analyst assessments stored alongside the original capture.

Blocked, rate-limited, and failed collectors retain their actual statuses. They can help explain an investigation's gaps and never become proof that an account or record is absent.

### 4. Build evidence-backed links

Add typed entities such as profiles, usernames, domains, organizations, or people with clear labels. Create a relationship only with supporting evidence IDs. For example, connect a captured profile to the website it explicitly links to, then inspect the supporting capture.

The browser's link-analysis view supports inspecting entities and relationships. The TUI provides Entities and Links tables with the same IDs and provenance. A label is an analyst aid; it does not change a source's identifier or combine accounts automatically.

### 5. Plan and run research flows

Create a flow, choose collector operations, and supply targets/options. The browser includes starting plans for public links, Telegram channels, and domains; a blank flow supports your own investigation. The TUI creates a flow with its first step and adds further steps with `s`.

Run a selected step explicitly. The step moves through its execution status, then records the resulting run ID or failure. Its original collector report and captured sources enter the case evidence list. Follow evidence references to decide the next step rather than treating the flow as a conclusion generator.

CLI collection can add evidence to the same case:

```sh
fieldkit --case "$PWD/investigations/cases/example" telegram-messages telegram --limit 10
fieldkit --case "$PWD/investigations/cases/example" bellingcat-telegram https://t.me/telegram/457
```

Refresh the app or TUI to see changes made by another interface or agent. Public Telegram helpers use bounded previews; they do not collect a complete channel history.

### 6. Review correlation leads

Open Review leads in the browser or Correlations in the TUI. Candidates retain a signal, reason, and supporting evidence references. Shared handles and shared outbound URLs can suggest a next research step; their meaning depends on the sources and context.

Accept or reject a candidate with a rationale. An accepted lead appears as a working association while the original entities stay distinct. No automated identity merge or ownership attribution follows from this review. Keep alternative explanations in notes and add further evidence when a lead is worth pursuing.

### 7. Inspect the trail and export

Use the timeline/activity views and evidence inspector to reconstruct collection and analyst decisions. The collection ledger preserves original report/artifact bytes; workspace actions record assessments, plans, notes, and reviewed links separately.

Export the full investigation from the browser or press `x` in the TUI for JSON/Markdown. The TUI creates a new output file and refuses to overwrite an existing one. MCP's `fieldkit_case_export` returns the export content to its client.

Full investigation exports include workspace records and collector-ledger references. Raw captured blobs remain in the case's `blobs` directory. `fieldkit case verify CASE_DIRECTORY` checks stored collection report/artifact hashes and references; it does not authenticate source claims. The original `fieldkit case export` command remains a collector-ledger export and does not replace a full workbench export.

## Terminal keys

| Key | Action |
| --- | --- |
| `e` | Capture evidence. |
| `t` | Run a collector into the case. |
| `a` | Add a typed entity. |
| `l` | Link entities with supporting evidence. |
| `f` | Create a research flow with a first step. |
| `s` | Add a step to the selected flow. |
| `r` | Run the selected step. |
| `v` | Review selected evidence, or a lead when the Correlations tab is active. |
| `c` | Review a correlation with rationale. |
| `n` | Add a note linked to selected evidence. |
| `i` | Edit the case title and research question. |
| `x` | Export the investigation to a new file. |
| `/` | Filter records. |
| `Ctrl+R` | Refresh changes from the shared case. |
| `?` | Open the keyboard guide. |
| `q` | Quit. |

Use Tab/Shift+Tab to move between controls, arrow keys to inspect table rows, and Escape to close a form. Collector work runs in a background worker so the terminal remains responsive.

## Browser preview and local work

The [hosted browser preview](https://fieldkit-investigations.seithargroup.chatgpt.site) uses synthetic data and a browser-local workspace. It supports trying the evidence, graph, flow-planning, notes, and review interactions. It does not have the Python collector backend or access to your local SQLite case. Collector jobs can be copied for use in Fieldkit.

The local app uses the Python backend and shares actual case data with the TUI and MCP server. Preview attachments retain metadata and a hash in browser state; keep the original file on your device. Use the local app to preserve captured attachment bodies in the content-addressed evidence store. Preview exports contain the browser workspace state, while local full-case exports include the collector ledger and integrity summary.

External profiles, posts, captions, snippets, URLs, and file text are evidence supplied by sources. They remain untrusted data and cannot change the authorized investigation or act as instructions for an agent.
