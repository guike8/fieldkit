# Nightjar desktop

An optional native Rust/egui interface over the existing Fieldkit case ledger. Nightjar is not macOS-only: the TUI remains a first-class interface over the same engine and cases. The `fieldkit` library, CLI and MCP names remain unchanged for compatibility.

## Launch

```sh
cargo run --locked -p nightjar -- /absolute/path/to/existing-case
```

The case argument is optional. **Open…** selects an existing case folder containing `case.sqlite3`; **New case…** creates an empty case at a new folder path. Opening, refreshing and saving a question never collect public data or call a model.

Build a macOS application bundle with sibling `nightjar`, `fieldkit` and `fieldkit-agent` executables:

```sh
cargo run --locked -p xtask -- bundle-macos
```

Optional bundling flags: `--debug`, `--output NEW_PATH.app`. Existing output bundles are not overwritten.

## macOS icon and menu bar

The bundle/window use the same Rust-rendered bird-and-crescent artwork, with multi-resolution ICNS assets. While the macOS app is running, a native template status icon provides **Open Nightjar** and **Quit Nightjar**. Open restores/activates the window; Quit uses normal close/cancellation. The ordinary Dock icon remains. There is no login service, hidden daemon or privacy-permission change.

The native binary checks Cocoa installation/template state and logs a status-item rectangle when available; that diagnostic does not prove an auto-hidden or crowded menu bar is visually unobscured. The `visual_smoke` example captures the workspace only, **not** the system menu bar. Verify the status item with the actual `nightjar` executable after a native build. macOS tray dependencies are platform-conditional, so terminal-only builds do not require them.

## Ask → Explore → Review

- **Ask** puts your natural-language question first. Choose Person, Organization, Infrastructure or Crypto, then add one to five explicit starting points. **Save question locally** persists only the original question and scope; nothing runs. Saved questions appear in the sidebar. The next action is **Set up agent…** or, when configured, **Run agent…**.
- **Explore** opens source relationships directly, without an agent transcript or duplicate question above the graph. Search exact values, select a neighbor to change the shared focus, or switch to **Table**. Selecting a record opens **Details**; there is no empty inspector taking up space by default.
- **Review** separates **Sources** from **Findings**. Sources retain collector/review status and provenance. Findings are analysis, not source evidence; decisions are read-only here. **Review decisions in the terminal** offers a deliberately copied, quoted TUI command, never a fake approval control.

**Case details** holds the technical folder path, integrity verification, coverage and case activity. **Refresh** reads changes from another agent or CLI without collection. The status bar is concise; hover it for the full operation message. Errors retain their full detail on hover.

### Crypto scope

Crypto currently scopes Ethereum addresses and transactions only. Use `ethereum:0x` followed by a 40-digit hexadecimal address or 64-digit transaction hash. **Trace an address** and **Inspect a transaction** only fill a local question template; you must supply the actual starting point and save deliberately. No collection starts from a template. Other chains and named intelligence providers are not implied to be connected. Attributed address labels are claims, not verified human identities.

### Agent execution

Open **Agent settings…** to check the absolute sidecar/Fieldkit executable paths, HTTPS endpoint, model and key **environment variable name**. **Check setup** checks binaries/key presence without a network request; it does not prove provider connectivity. Technical setup does not occupy the main question workspace.

**Run agent…** always discloses the exact question, scope, endpoint and model. Provider/charges consent is required; public-source collection is a separate checkbox, **off by default**. Plan-only execution still calls the provider. Each run starts a new scoped sidecar investigation from the selected question and seeds.

The Ask view shows the actual returned response and stop reason. **Response details & tool trace** exposes the complete paged result. Nonzero exits, missing/malformed reports, cancellation and timeouts are not presented as successful investigations. No assistant replies are fabricated.

### Credentials

Never enter a secret value into the desktop fields, chat or command arguments. Use your existing secret-management workflow to export the named key environment variable, then launch Nightjar from that same terminal environment (for example, run the bundle's `Contents/MacOS/nightjar` executable). The default variable name is `INKLING_API_KEY`; the endpoint and model defaults match the separate agent integration.

A Finder-launched app may not inherit shell environment variables. A missing-key message means the variable is absent from **that app process**, not that credentials do not exist elsewhere. Restart from the configured environment, then check again. Configuration is session-local and the app does not persist secret values. Child stdout/stderr are discarded to avoid displaying credential-bearing logs; structured agent output is read from the sidecar's new private report file within the case directory.

### Agent limits and cancellation

The desktop invokes the existing Rust `fieldkit-agent` binary, not an embedded provider SDK or scheduler. Limits are 8 turns, 16 tool calls, 16,384 total tokens, 2,048 tokens per response, 60 seconds per request and a 10-minute desktop deadline. Reports are limited to 8 MiB. **Cancel agent** terminates the child process group, including MCP descendants. Closing Nightjar also requests cancellation. Already transmitted provider requests cannot be recalled and previously persisted observations remain; refresh the case after interruption.

## Source navigation and inspection

**Explore** opens graph navigation directly. The monochrome graph shows the focused source and up to four incident directed relations. A blue outline **and FOCUS label** identify focus; arrowheads show recorded direction. URL nodes emphasize their distinguishing profile/path tail, with the host below; crypto hashes retain both ends and chain context. Original values are unchanged and available on hover and in **Details**. Select a neighbor to persist shared focus; select a relation or focused node for its exact record. **Table** includes every projected node/edge. Details opens only on selection and can be closed; provenance, hashes and full paged JSON are expandable.

The projection covers the latest 500 materialized reports, not the entire case history. Missing nodes are not evidence of absence. Edges represent cited collector observations, never verified identity or ownership; manual notes, model assessments and analytical findings do not create source edges. **Review** keeps these record classes separate. Source text is plain, untrusted text; it is never executed and links are not automatically opened. Invisible control and bidi characters are displayed as escapes.

## Review boundaries

Findings are read-only in this desktop version. No approve/reject action, identity resolution, automatic finding promotion or automatic collection runs here. Use the existing deliberate local review workflow in the Fieldkit TUI for finding review, then refresh Nightjar. Accepted findings still do not become source graph edges.

## Tests

```sh
cargo test --locked -p nightjar
```

Tests cover explicit plan validation/persistence without collection, case open/create safeguards, Unicode/control rendering, JSON paging, empty/real-case headless egui frames and synthetic mock-sidecar output/cancellation. They do not call providers. Headless frame tests are not screenshots or native-window visual verification.

For a native visual smoke, use a disposable **copy** of an existing case and a new PNG path:

```sh
cargo run --locked -p nightjar --example visual_smoke -- /absolute/copied-case /tmp/nightjar-new.png 1480 800
```

This opens the actual app, waits for layout/case loading, captures only its own rendered framebuffer and exits. It does not capture other applications or require macOS screen-recording permissions. Optional final `X Y` arguments inject a local navigation click in logical window coordinates before capture; do not target collection/provider controls. Existing PNGs are never overwritten. A saved image still needs visual inspection; this is not an investigation-quality test.
