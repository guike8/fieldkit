"""Keyboard-driven case workbench. Textual is an optional UI dependency.

The terminal interface uses the same Workbench and immutable collection reports
as the browser application and agents. Collected text is displayed as plain
text, never interpreted as terminal markup or instructions.
"""
from __future__ import annotations

import argparse
import importlib.metadata
import json
from pathlib import Path
import sys

from .common import Failure
from .manifest import COMMANDS
from .transport import strict_json


def _display(value):
    """Render a one-line label without terminal controls; preserve stored bytes."""
    return "".join(character if ord(character) >= 32 and not 127 <= ord(character) <= 159 else " "
                   for character in str(value if value is not None else ""))


def _textual():
    try:
        version = importlib.metadata.version("textual")
        major, minor = [int(part) for part in version.split(".")[:2]]
        if not (major == 8 and minor >= 2):
            raise ValueError("unsupported version")
        from textual.app import App, ComposeResult
        from textual import work
        from textual.binding import Binding
        from textual.containers import Horizontal, Vertical, VerticalScroll
        from textual.screen import ModalScreen
        from textual.widgets import Button, DataTable, Footer, Header, Input, Label, Select, Static, TabbedContent, TabPane, TextArea
        from rich.text import Text
    except (ImportError, importlib.metadata.PackageNotFoundError, ValueError) as exc:
        raise Failure("TUI_DEPENDENCY", "Install the terminal UI extra from the Fieldkit checkout: python -m pip install '.[tui]' (Textual >=8.2,<9)", "unavailable") from exc
    return locals()


def create_app(case_path="cases/default", *, config=None, demo=False, workbench=None):
    """Return a Textual app without requiring Textual for normal CLI imports."""
    ui = _textual()
    App, ComposeResult, work, Binding = (ui[name] for name in ("App", "ComposeResult", "work", "Binding"))
    Horizontal, Vertical, VerticalScroll, ModalScreen = (ui[name] for name in ("Horizontal", "Vertical", "VerticalScroll", "ModalScreen"))
    Button, DataTable, Footer, Header, Input, Label, Select, Static, TabbedContent, TabPane, TextArea = (
        ui[name] for name in ("Button", "DataTable", "Footer", "Header", "Input", "Label", "Select", "Static", "TabbedContent", "TabPane", "TextArea"))
    Text = ui["Text"]
    if workbench is None:
        from .workbench import Workbench
        workbench = Workbench(case_path, config=config)
    if demo:
        workbench.seed_demo()

    class Form(ModalScreen):
        BINDINGS = [Binding("escape", "cancel", "Cancel")]
        DEFAULT_CSS = """
        Form {align: center middle; background: #050b10 80%;}
        Form > Vertical {width: 78; max-width: 96%; height: auto; max-height: 94%; border: round #4cc8b3; background: #11202b; padding: 1 2;}
        Form #form-title {text-style: bold; color: #76dfcb; height: 2;}
        Form #form-fields {height: auto; max-height: 60vh;}
        Form Label {margin-top: 1; color: #a6bcc9;}
        Form Input, Form Select {margin-bottom: 0;}
        Form TextArea {height: 6; border: solid #2a4a5d;}
        Form #form-error {height: auto; max-height: 4; color: #ffb28b; margin-top: 1;}
        Form #form-buttons {height: 3; margin-top: 1; align: right middle;}
        Form Button {margin-left: 1;}
        """

        def __init__(self, title, fields, submit):
            super().__init__()
            self.form_title, self.fields, self.submit = title, fields, submit

        def compose(self) -> ComposeResult:
            with Vertical():
                yield Static(self.form_title, id="form-title", markup=False)
                with VerticalScroll(id="form-fields"):
                    for field in self.fields:
                        yield Label(field["label"])
                        key, value = field["key"], str(field.get("value", ""))
                        if "choices" in field:
                            choices = field["choices"]
                            selected = field.get("value") or (choices[0][1] if choices else Select.BLANK)
                            yield Select(choices, value=selected, allow_blank=not bool(choices), id=key)
                        elif field.get("multiline"):
                            yield TextArea(value, id=key, show_line_numbers=False)
                        else:
                            yield Input(value=value, placeholder=field.get("placeholder", ""), id=key)
                yield Static("", id="form-error", markup=False)
                with Horizontal(id="form-buttons"):
                    yield Button("Cancel", id="cancel-form")
                    yield Button("Save", variant="success", id="submit-form")

        def action_cancel(self):
            self.dismiss(None)

        def on_button_pressed(self, event):
            event.stop()
            if event.button.id == "cancel-form":
                self.dismiss(None)
                return
            if event.button.id != "submit-form":
                return
            values = {}
            for field in self.fields:
                widget = self.query_one("#" + field["key"])
                value = widget.text if isinstance(widget, TextArea) else widget.value
                values[field["key"]] = "" if value is Select.BLANK else value
            try:
                self.submit(values)
            except Failure as exc:
                self.query_one("#form-error", Static).update(exc.message)
                return
            except (ValueError, TypeError, KeyError) as exc:
                self.query_one("#form-error", Static).update(str(exc)[:240])
                return
            self.dismiss(values)

    class FieldkitApp(App):
        TITLE = "Fieldkit · Case Workbench"
        SUB_TITLE = "Evidence → Research flows → Reviewed links"
        CSS = """
        Screen {background: #0b141c; color: #d7e6ed;}
        Header {background: #112631; color: #9ce8db;}
        Footer {background: #112631;}
        #case-heading {height: 3; padding: 0 2; color: #a7e6da; background: #10212c;}
        #filter {margin: 1 1 0 1; height: 3; border: round #294c5b;}
        #main {height: 1fr; padding: 0 1;}
        #tabs {width: 3fr; height: 1fr;}
        TabPane {padding: 1 0 0 0;}
        DataTable {height: 1fr; background: #0d1a24; border: solid #203b4b;}
        DataTable > .datatable--header {background: #17313f; color: #a7e6da; text-style: bold;}
        DataTable > .datatable--cursor {background: #244d5c; color: #ffffff;}
        #inspector-pane {width: 2fr; min-width: 28; padding: 1 1 0 2;}
        #inspector-title {height: 2; color: #74d8c3; text-style: bold;}
        #inspector-scroll {height: 1fr; border: round #294755; padding: 1;}
        #detail {height: auto; color: #bccfd9;}
        .section {height: 2; color: #d7b67a; padding-top: 1;}
        #flows-table {height: 40%;}
        #status-line {height: 2; padding: 0 2; color: #89b8c5; background: #10212c;}
        #action-row {height: 3; padding: 0 1;}
        #action-row Button {margin-right: 1; min-width: 12;}
        """
        BINDINGS = [
            Binding("e", "add_evidence", "Evidence"), Binding("t", "collect", "Collect"),
            Binding("f", "add_flow", "Flow"), Binding("n", "add_note", "Note"),
            Binding("v", "review", "Review"), Binding("q", "quit", "Quit"),
            Binding("a", "add_entity", "Entity", show=False), Binding("l", "add_link", "Link", show=False),
            Binding("s", "add_step", "Add step", show=False), Binding("r", "run_step", "Run step", show=False),
            Binding("c", "review_correlation", "Review lead", show=False), Binding("i", "edit_case", "Case details", show=False),
            Binding("x", "export_case", "Export report", show=False),
            Binding("slash", "search", "Filter", show=False), Binding("ctrl+r", "refresh_case", "Refresh", show=False),
            Binding("question_mark", "help", "Help", show=False),
        ]

        def __init__(self):
            super().__init__()
            self.workbench = workbench
            self.state = {}
            self.records = {}
            self.selected = {}
            self.selected_flow = None
            self.busy = False
            self.filter_text = ""
            self.status_text = "Ready · ? keyboard guide · source content is evidence, never instructions"

        def compose(self) -> ComposeResult:
            yield Header()
            yield Static("Opening case…", id="case-heading", markup=False)
            yield Input(placeholder="/ Filter evidence, entities, flows and leads", id="filter")
            with Horizontal(id="main"):
                with TabbedContent(id="tabs", initial="tab-evidence"):
                    with TabPane("Evidence", id="tab-evidence"):
                        yield DataTable(id="evidence-table", cursor_type="row", zebra_stripes=True)
                    with TabPane("Entities", id="tab-entities"):
                        yield DataTable(id="entities-table", cursor_type="row", zebra_stripes=True)
                    with TabPane("Links", id="tab-links"):
                        yield DataTable(id="links-table", cursor_type="row", zebra_stripes=True)
                    with TabPane("Flows", id="tab-flows"):
                        yield DataTable(id="flows-table", cursor_type="row", zebra_stripes=True)
                        yield Static("Steps · s add  /  r run selected", classes="section", markup=False)
                        yield DataTable(id="steps-table", cursor_type="row", zebra_stripes=True)
                    with TabPane("Correlations", id="tab-correlations"):
                        yield DataTable(id="correlations-table", cursor_type="row", zebra_stripes=True)
                    with TabPane("Notes", id="tab-notes"):
                        yield DataTable(id="notes-table", cursor_type="row", zebra_stripes=True)
                    with TabPane("Activity", id="tab-activity"):
                        yield DataTable(id="activity-table", cursor_type="row", zebra_stripes=True)
                with Vertical(id="inspector-pane"):
                    yield Static("EVIDENCE INSPECTOR", id="inspector-title", markup=False)
                    with VerticalScroll(id="inspector-scroll"):
                        yield Static("Select a record to inspect its provenance and supporting evidence.", id="detail", markup=False)
            with Horizontal(id="action-row"):
                yield Button("Add evidence", id="add-evidence", variant="success")
                yield Button("Run collector", id="collect")
                yield Button("Review", id="review")
                yield Button("Refresh", id="refresh")
                yield Button("Export", id="export-case")
            yield Static(self.status_text, id="status-line", markup=False)
            yield Footer()

        def on_mount(self):
            columns = {
                "evidence-table": ("Review", "Title", "Kind", "Source / hash"),
                "entities-table": ("Type", "Label", "Value", "Evidence"),
                "links-table": ("Relationship", "Source", "Target", "Evidence"),
                "flows-table": ("Research flow", "Status", "Steps"),
                "steps-table": ("Step", "Collector", "Target", "Status"),
                "correlations-table": ("Review", "Signal", "Reason", "Evidence"),
                "notes-table": ("Note", "Evidence", "Created"),
                "activity-table": ("When", "Action", "Detail"),
            }
            for table_id, titles in columns.items():
                self.query_one("#" + table_id, DataTable).add_columns(*titles)
            self.refresh_case()
            self.query_one("#evidence-table", DataTable).focus()

        def _notice(self, message, *, error=False):
            self.status_text = _display(message)[:240]
            self.query_one("#status-line", Static).update(self.status_text)
            if error:
                self.notify(self.status_text, severity="error", markup=False)

        def _form(self, title, fields, submit):
            def save(values):
                submit(values)
                self.refresh_case()
            self.push_screen(Form(title, fields, save))

        def _values(self, rows):
            return [row for row in rows if not self.filter_text or self.filter_text in json.dumps(row, ensure_ascii=False).casefold()]

        def _fill(self, table_id, rows, cells):
            table = self.query_one("#" + table_id, DataTable)
            table.clear()
            self.records[table_id] = {}
            selected = self.selected.get(table_id)
            selected_row = 0
            for index, row in enumerate(rows):
                key = str(row.get("id", row.get("run_id", index)))
                self.records[table_id][key] = row
                table.add_row(*[Text(_display(value)[:160]) for value in cells(row)], key=key)
                if key == selected:
                    selected_row = index
            if rows:
                table.move_cursor(row=selected_row, column=0)

        def refresh_case(self):
            try:
                self.state = self.workbench.snapshot()
            except Failure as exc:
                self._notice(exc.message, error=True)
                return
            metadata, stats = self.state.get("metadata", {}), self.state.get("stats", {})
            title = _display(metadata.get("title", "Untitled investigation"))
            question = _display(metadata.get("question") or "Set a research question with i")
            evidence = self.state.get("evidence", [])
            entities = self.state.get("entities", [])
            leads = self.state.get("correlations", [])
            self.query_one("#case-heading", Static).update(
                f"{title}   ·   {len(evidence)} evidence  /  {len(entities)} entities  /  {len(leads)} correlation leads\n{question}")
            self._fill("evidence-table", self._values(evidence), lambda row: (
                row.get("status", "unreviewed"), row.get("title"), row.get("kind"), row.get("source_url") or str(row.get("sha256") or "")[:16]))
            self._fill("entities-table", self._values(entities), lambda row: (
                row.get("type"), row.get("label") or row.get("value"), row.get("value"), len(row.get("evidence_ids", []))))
            labels = {row.get("id"): row.get("label") or row.get("value") or row.get("id") for row in entities}
            self._fill("links-table", self._values(self.state.get("links", [])), lambda row: (
                row.get("relation"), labels.get(row.get("source_id"), row.get("source_id")),
                labels.get(row.get("target_id"), row.get("target_id")), len(row.get("evidence_ids", []))))
            flows = self._values(self.state.get("flows", []))
            self._fill("flows-table", flows, lambda row: (row.get("title"), row.get("status"), len(row.get("steps", []))))
            flow = next((row for row in flows if row.get("id") == self.selected_flow), flows[0] if flows else None)
            self.selected_flow = flow.get("id") if flow else None
            self._populate_steps(flow)
            self._fill("correlations-table", self._values(leads), lambda row: (
                row.get("status", "candidate"), row.get("signal"), row.get("reason"), len(row.get("evidence_ids", []))))
            self._fill("notes-table", self._values(self.state.get("notes", [])), lambda row: (
                row.get("content", "")[:100], len(row.get("evidence_ids", [])), row.get("created_at", "")))
            self._fill("activity-table", self._values(self.state.get("activity", [])), lambda row: (
                row.get("created_at", row.get("timestamp", "")), row.get("action", row.get("kind", "")), row.get("title", row.get("summary", row.get("detail", row.get("target_id", ""))))))

        def _populate_steps(self, flow):
            steps = flow.get("steps", []) if flow else []
            self._fill("steps-table", steps, lambda row: (
                steps.index(row) + 1, row.get("command"), row.get("target", ""), row.get("status", "pending")))

        def on_data_table_row_highlighted(self, event):
            table_id, key = event.data_table.id, str(event.row_key.value)
            record = self.records.get(table_id, {}).get(key)
            if record is None:
                return
            self.selected[table_id] = key
            if table_id == "flows-table":
                self.selected_flow = record.get("id")
                self._populate_steps(record)
            self.query_one("#detail", Static).update(json.dumps(record, ensure_ascii=False, indent=2))
            self.query_one("#inspector-title", Static).update(table_id.removesuffix("-table").replace("-", " ").upper() + " · PROVENANCE")

        def on_input_changed(self, event):
            if event.input.id == "filter":
                self.filter_text = event.value[:200].casefold().strip()
                self.refresh_case()

        def on_button_pressed(self, event):
            actions = {"add-evidence": self.action_add_evidence, "collect": self.action_collect,
                       "review": self.action_review, "refresh": self.action_refresh_case, "export-case": self.action_export_case}
            if event.button.id in actions:
                actions[event.button.id]()

        def _selected(self, table_id):
            record = self.records.get(table_id, {}).get(self.selected.get(table_id))
            if record is None:
                record = next(iter(self.records.get(table_id, {}).values()), None)
            return record

        def _choices(self, kind, label):
            return [(str(row.get(label) or row.get("value") or row["id"])[:80], row["id"]) for row in self.state.get(kind, [])]

        def action_add_evidence(self):
            self._form("Capture an evidence item", [
                {"key": "evidence-title", "label": "Evidence title"},
                {"key": "evidence-source", "label": "Public source URL (optional)"},
                {"key": "evidence-content", "label": "Observation / captured text", "multiline": True},
            ], lambda values: self.workbench.add_evidence(values["evidence-title"], content=values["evidence-content"], source_url=values["evidence-source"]))

        def action_add_note(self):
            evidence = self._selected("evidence-table")
            self._form("Add an analyst note", [{"key": "note-content", "label": "Note · observations and reasoning", "multiline": True}],
                       lambda values: self.workbench.add_note(values["note-content"], evidence_ids=[evidence["id"]] if evidence else []))

        def action_edit_case(self):
            metadata = self.state.get("metadata", {})
            self._form("Research question", [
                {"key": "case-title", "label": "Case title", "value": metadata.get("title", "")},
                {"key": "case-question", "label": "What are you trying to establish?", "value": metadata.get("question", ""), "multiline": True},
            ], lambda values: self.workbench.update_case(values["case-title"], question=values["case-question"]))

        def action_add_entity(self):
            self._form("Add a subject or entity", [
                {"key": "entity-type", "label": "Entity type", "choices": [(value, value) for value in ["person", "organization", "username", "url", "domain", "email", "telegram_channel"]]},
                {"key": "entity-value", "label": "Identifier / value"}, {"key": "entity-label", "label": "Display label (optional)"},
            ], lambda values: self.workbench.add_entity(values["entity-type"], values["entity-value"], label=values["entity-label"]))

        def action_add_link(self):
            entities, evidence = self._choices("entities", "label"), self._choices("evidence", "title")
            if len(entities) < 2 or not evidence:
                self._notice("Add at least two entities and supporting evidence before creating a link", error=True)
                return
            self._form("Link entities with supporting evidence", [
                {"key": "link-source", "label": "Source entity", "choices": entities},
                {"key": "link-target", "label": "Target entity", "choices": entities, "value": entities[1][1]},
                {"key": "link-relation", "label": "Observed / asserted relationship", "value": "associated_with"},
                {"key": "link-evidence", "label": "Supporting evidence", "choices": evidence},
                {"key": "link-note", "label": "Rationale", "multiline": True},
            ], lambda values: self.workbench.add_link(values["link-source"], values["link-target"], values["link-relation"], [values["link-evidence"]], note=values["link-note"]))

        def _tool_fields(self):
            return [{"key": "tool-command", "label": "Collector", "choices": [(name, name) for name in COMMANDS], "value": "username"},
                    {"key": "tool-target", "label": "Target (blank only for commands without a target)"},
                    {"key": "tool-options", "label": "Options JSON object · consult capabilities for schema", "value": "{}", "multiline": True}]

        def _job(self, values):
            try:
                options = strict_json(values["tool-options"])
            except (ValueError, RecursionError) as exc:
                raise Failure("INVALID_OPTIONS", "Options must be a strict JSON object", "invalid") from exc
            if not isinstance(options, dict):
                raise Failure("INVALID_OPTIONS", "Options must be a JSON object", "invalid")
            return values["tool-command"], values["tool-target"].strip() or None, options

        def action_add_flow(self):
            fields = [{"key": "flow-title", "label": "Research flow title"}] + self._tool_fields()
            def save(values):
                command, target, options = self._job(values)
                step = {"command": command, "options": options, **({"target": target} if target is not None else {})}
                created = self.workbench.create_flow(values["flow-title"], steps=[step])
                self.selected_flow = created["id"]
                self.selected["flows-table"] = created["id"]
                self.query_one("#tabs", TabbedContent).active = "tab-flows"
            self._form("Plan a research flow · first step", fields, save)

        def action_add_step(self):
            flow = self._selected("flows-table")
            if not flow:
                self._notice("Create a research flow first", error=True)
                return
            def save(values):
                command, target, options = self._job(values)
                self.workbench.add_step(flow["id"], command, target, options=options)
            self._form("Add step to " + flow.get("title", "flow"), self._tool_fields(), save)

        @work(group="collectors")
        async def _execute(self, callback, description):
            import asyncio
            if self.busy:
                self._notice("A collector is already running; wait for its result", error=True)
                return
            self.busy = True
            self._notice("Running " + description + " · evidence will be added to this case")
            try:
                report = await asyncio.to_thread(callback)
                self._notice(description + " · " + str(report.get("status", "completed")))
            except Failure as exc:
                self._notice(exc.message, error=True)
            except Exception as exc:
                self._notice("Collector failed: " + type(exc).__name__, error=True)
            finally:
                self.busy = False
                self.refresh_case()

        def action_collect(self):
            def save(values):
                command, target, options = self._job(values)
                from .manifest import validate_job
                job = {"command": command, "options": options}
                if target:
                    job["target"] = target
                validate_job(job)
                self._execute(lambda: self.workbench.run_tool(command, target, options=options), command)
            self._form("Run a collector · capture to this case", self._tool_fields(), save)

        def action_run_step(self):
            flow, step = self._selected("flows-table"), self._selected("steps-table")
            if not flow or not step:
                self._notice("Select a planned flow step first", error=True)
                return
            self._execute(lambda: self.workbench.run_step(flow["id"], step["id"]), step.get("command", "flow step"))

        def action_review(self):
            if self.query_one("#tabs", TabbedContent).active == "tab-correlations":
                self.action_review_correlation()
                return
            evidence = self._selected("evidence-table")
            if not evidence:
                self._notice("Select an evidence item to review", error=True)
                return
            self._form("Review evidence · analyst assessment", [
                {"key": "review-status", "label": "Assessment", "value": evidence.get("status", "unreviewed"), "choices": [(value, value) for value in ["unreviewed", "corroborated", "contested", "discarded"]]},
                {"key": "review-note", "label": "Reason / limitations", "value": evidence.get("review_note", ""), "multiline": True},
            ], lambda values: self.workbench.review_evidence(evidence["id"], values["review-status"], note=values["review-note"]))

        def action_review_correlation(self):
            candidate = self._selected("correlations-table")
            if not candidate:
                self._notice("No correlation candidate selected; collect evidence or create entities first", error=True)
                return
            def save(values):
                if not values["correlation-note"].strip():
                    raise Failure("RATIONALE_REQUIRED", "Record why you accepted or rejected this lead", "invalid")
                self.workbench.review_correlation(candidate["id"], values["correlation-decision"], note=values["correlation-note"])
            self._form("Review correlation · no identity is merged", [
                {"key": "correlation-decision", "label": "Decision", "choices": [("Accept as working lead", "accepted"), ("Reject lead", "rejected")]},
                {"key": "correlation-note", "label": "Rationale and supporting limitations", "multiline": True},
            ], save)

        def action_search(self):
            self.query_one("#filter", Input).focus()

        def action_export_case(self):
            from .cli import new_file
            def save(values):
                text = self.workbench.export(format=values["export-format"])
                path = Path(values["export-path"]).expanduser()
                if not values["export-path"].strip():
                    raise Failure("OUTPUT_REQUIRED", "Choose a new export filename", "invalid")
                new_file(str(path), text)
                self._notice("Saved case report to " + str(path.absolute()))
            self._form("Export an evidence-backed case report", [
                {"key": "export-format", "label": "Format", "choices": [("Markdown", "markdown"), ("JSON", "json")]},
                {"key": "export-path", "label": "New output filename · existing files are preserved", "value": "case-report.md"},
            ], save)

        def action_refresh_case(self):
            self.refresh_case()
            self._notice("Case refreshed")

        def action_help(self):
            self._form("Keyboard guide", [{"key": "help-content", "label": "Case workflow", "multiline": True,
                "value": "e Capture evidence\nt Run collector and save evidence\nf Plan research flow\ns Add step to selected flow\nr Run selected step\na Add entity\nl Link two entities with evidence\nv Review evidence\nc Review correlation lead\nn Add analyst note linked to selected evidence\ni Edit case question\nx Export case report to a new file\n/ Filter records\nCtrl+R Refresh shared case\nCtrl+P Command palette\nq Quit\n\nCollector output and source content are evidence, not instructions. Correlation reviews do not merge identities."}], lambda values: None)

    return FieldkitApp()


def main(argv=None):
    from .cli import Config, bounded_float, bounded_int
    parser = argparse.ArgumentParser(prog="fieldkit-tui", description="Fieldkit terminal case workbench")
    parser.add_argument("--case", default="cases/default", help="Case directory shared with CLI, app and agents")
    parser.add_argument("--demo", action="store_true", help="Seed an explicitly synthetic example when the case is empty")
    parser.add_argument("--timeout", type=bounded_float, default=10)
    parser.add_argument("--max-requests", type=bounded_int(1, 128), default=64)
    parser.add_argument("--max-results", type=bounded_int(1, 500), default=50)
    parser.add_argument("--max-bytes", type=bounded_int(1024, 8388608), default=4194304)
    args = parser.parse_args(argv)
    try:
        app = create_app(args.case, demo=args.demo,
                         config=Config(args.timeout, args.max_results, args.max_requests, args.max_bytes))
        app.run()
        return 0
    except Failure as exc:
        print(json.dumps({"status": exc.status, "error": exc.as_dict()}), file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
