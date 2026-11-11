"""Headless terminal workbench interaction tests; no remote collection."""
from __future__ import annotations
from copy import deepcopy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from fieldkit_core.common import Failure
from fieldkit_core import tui

HAS_TEXTUAL = importlib.util.find_spec("textual") is not None


class FixtureWorkbench:
    def __init__(self):
        self.data = {"metadata": {"title": "Synthetic case", "question": "What does the evidence support?"},
            "evidence": [{"id": "ev_1", "title": "Public profile [red]literal[/red]", "kind": "note", "content": "Original observation",
                          "status": "unreviewed", "source_url": "https://example.org/", "sha256": "a" * 64}],
            "entities": [{"id": "en_1", "type": "username", "value": "site:example", "label": "Profile one", "evidence_ids": ["ev_1"]},
                         {"id": "en_2", "type": "username", "value": "other:example", "label": "Profile two", "evidence_ids": ["ev_1"]}],
            "links": [], "flows": [], "correlations": [{"id": "co_1", "source_id": "en_1", "target_id": "en_2",
                "signal": "shared_handle", "reason": "Same handle is a lead only", "evidence_ids": ["ev_1"], "status": "candidate"}],
            "notes": [], "activity": [], "stats": {}}
        self.calls = []

    def snapshot(self):
        return deepcopy(self.data)

    def seed_demo(self):
        self.calls.append(("seed_demo",))

    def add_evidence(self, title, **fields):
        if not title.strip():
            raise Failure("TITLE_REQUIRED", "Evidence title is required", "invalid")
        entry = {"id": "ev_" + str(len(self.data["evidence"]) + 1), "title": title, "kind": "note", "status": "unreviewed", **fields}
        self.data["evidence"].append(entry)
        self.calls.append(("add_evidence", title, fields))
        return entry

    def add_note(self, content, evidence_ids=None):
        if not content.strip():
            raise Failure("NOTE_REQUIRED", "Note is required", "invalid")
        entry = {"id": "note_1", "content": content, "evidence_ids": evidence_ids or [], "created_at": "2026-11-11T00:00:00Z"}
        self.data["notes"].append(entry)
        return entry

    def review_evidence(self, identifier, status, note=""):
        entry = next(row for row in self.data["evidence"] if row["id"] == identifier)
        entry.update(status=status, review_note=note)
        return entry

    def add_entity(self, kind, value, label=""):
        entry = {"id": "en_" + str(len(self.data["entities"]) + 1), "type": kind, "value": value, "label": label, "evidence_ids": []}
        self.data["entities"].append(entry)
        return entry

    def add_link(self, source_id, target_id, relation, evidence_ids, note=""):
        if not evidence_ids:
            raise Failure("EVIDENCE_REQUIRED", "Supporting evidence is required", "invalid")
        entry = {"id": "link_1", "source_id": source_id, "target_id": target_id, "relation": relation,
                 "evidence_ids": evidence_ids, "note": note}
        self.data["links"].append(entry)
        return entry

    def create_flow(self, title, steps=None):
        if not title.strip():
            raise Failure("TITLE_REQUIRED", "Flow title is required", "invalid")
        entry = {"id": "flow_1", "title": title, "status": "planned", "steps": [dict(row, id="step_1", status="pending") for row in steps or []]}
        self.data["flows"].append(entry)
        return entry

    def add_step(self, identifier, command, target, options=None):
        flow = next(row for row in self.data["flows"] if row["id"] == identifier)
        entry = {"id": "step_" + str(len(flow["steps"]) + 1), "command": command, "target": target, "options": options or {}, "status": "pending"}
        flow["steps"].append(entry)
        return entry

    def run_step(self, flow_id, step_id):
        self.calls.append(("run_step", flow_id, step_id))
        flow = next(row for row in self.data["flows"] if row["id"] == flow_id)
        step = next(row for row in flow["steps"] if row["id"] == step_id)
        step.update(status="completed", run_id="run_fixture")
        return {"status": "ok", "run_id": "run_fixture"}

    def run_tool(self, command, target, options=None):
        self.calls.append(("run_tool", command, target, options))
        return {"status": "ok", "run_id": "run_fixture"}

    def review_correlation(self, identifier, decision, note=""):
        item = next(row for row in self.data["correlations"] if row["id"] == identifier)
        item.update(status=decision, review_note=note)
        return item

    def update_case(self, title, question=""):
        self.data["metadata"].update(title=title, question=question)
        return self.data["metadata"]


@unittest.skipUnless(HAS_TEXTUAL, "Install the tui extra for headless terminal tests")
class TerminalTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.model = FixtureWorkbench()
        self.app = tui.create_app(workbench=self.model)

    async def fill(self, selector, value):
        from textual.widgets import Input, TextArea
        widget = self.app.screen.query_one(selector)
        if isinstance(widget, TextArea):
            widget.load_text(value)
        elif isinstance(widget, Input):
            widget.value = value
        else:
            widget.value = value

    async def save(self, pilot):
        # Textual deliberately debounces repeated button presses. A pause models
        # the user editing the form rather than double-clicking an active button.
        await pilot.pause(0.2)
        await pilot.click("#submit-form")
        await pilot.pause()

    async def test_case_tables_and_plain_source_text_mount(self):
        from textual.widgets import DataTable
        self.model.data["evidence"][0]["title"] += "\x1b[2J"
        async with self.app.run_test(size=(140, 45)) as pilot:
            await pilot.pause()
            table = self.app.query_one("#evidence-table", DataTable)
            self.assertEqual(table.row_count, 1)
            cells = table.get_row("ev_1")
            self.assertIn("[red]literal[/red]", str(cells[1]))
            self.assertNotIn("\x1b", str(cells[1]))
            self.assertEqual(self.app.query_one("#entities-table", DataTable).row_count, 2)

    async def test_capture_evidence_and_note_are_persisted_through_model(self):
        from textual.widgets import DataTable
        async with self.app.run_test(size=(140, 45)) as pilot:
            await pilot.pause()
            self.app.action_add_evidence()
            await pilot.pause()
            await self.fill("#evidence-title", "Collected observation")
            await self.fill("#evidence-source", "https://example.org/source")
            await self.fill("#evidence-content", "A bounded observation")
            await self.save(pilot)
            self.assertEqual(len(self.model.data["evidence"]), 2)
            self.assertEqual(self.app.query_one("#evidence-table", DataTable).row_count, 2)
            self.app.action_add_note()
            await pilot.pause()
            await self.fill("#note-content", "Keep the two identities separate until more evidence exists")
            await self.save(pilot)
            self.assertEqual(self.model.data["notes"][0]["evidence_ids"], ["ev_1"])

    async def test_validation_keeps_the_evidence_form_open(self):
        async with self.app.run_test(size=(140, 45)) as pilot:
            await pilot.pause()
            self.app.action_add_evidence()
            await pilot.pause()
            await self.save(pilot)
            self.assertEqual(len(self.model.data["evidence"]), 1)
            self.assertIn("required", str(self.app.screen.query_one("#form-error").render()).lower())
            await pilot.press("escape")

    async def test_research_flow_can_be_created_and_its_step_run(self):
        async with self.app.run_test(size=(140, 55)) as pilot:
            await pilot.pause()
            self.app.action_add_flow()
            await pilot.pause()
            await self.fill("#flow-title", "Check the public handle")
            await self.fill("#tool-command", "username")
            await self.fill("#tool-target", "example_handle")
            await self.fill("#tool-options", '{"sites":["github"]}')
            await self.save(pilot)
            self.assertEqual(self.model.data["flows"][0]["steps"][0]["options"], {"sites": ["github"]})
            self.app.action_run_step()
            await self.app.workers.wait_for_complete()
            await pilot.pause()
            self.assertIn(("run_step", "flow_1", "step_1"), self.model.calls)
            self.assertFalse(self.app.busy)

    async def test_correlation_review_requires_rationale_without_merging_entities(self):
        async with self.app.run_test(size=(140, 45)) as pilot:
            await pilot.pause()
            self.app.action_review_correlation()
            await pilot.pause()
            await self.save(pilot)
            self.assertEqual(self.model.data["correlations"][0]["status"], "candidate")
            await self.fill("#correlation-note", "Shared handle is a weak lead; retain for manual follow-up")
            await self.save(pilot)
            self.assertEqual(self.model.data["correlations"][0]["status"], "accepted")
            self.assertEqual(len(self.model.data["entities"]), 2)

    async def test_entity_link_records_supporting_evidence(self):
        async with self.app.run_test(size=(140, 55)) as pilot:
            await pilot.pause()
            self.app.action_add_link()
            await pilot.pause()
            await self.fill("#link-note", "Observed cross-reference")
            await self.save(pilot)
            self.assertEqual(self.model.data["links"][0]["evidence_ids"], ["ev_1"])
            self.assertNotEqual(self.model.data["links"][0]["source_id"], self.model.data["links"][0]["target_id"])

    async def test_collect_uses_validated_options_and_async_worker(self):
        async with self.app.run_test(size=(140, 55)) as pilot:
            await pilot.pause()
            self.app.action_collect()
            await pilot.pause()
            await self.fill("#tool-command", "person")
            await self.fill("#tool-target", "Example Name")
            await self.fill("#tool-options", "{}")
            await self.save(pilot)
            await self.app.workers.wait_for_complete()
            await pilot.pause()
            self.assertIn(("run_tool", "person", "Example Name", {}), self.model.calls)

    async def test_filter_and_review_status_refresh_tables(self):
        from textual.widgets import DataTable, Input
        async with self.app.run_test(size=(140, 45)) as pilot:
            await pilot.pause()
            self.app.action_review()
            await pilot.pause()
            await self.fill("#review-status", "contested")
            await self.fill("#review-note", "The source is disputed")
            await self.save(pilot)
            self.assertEqual(self.model.data["evidence"][0]["status"], "contested")
            self.app.query_one("#filter", Input).value = "nonexistent phrase"
            await pilot.pause()
            self.assertEqual(self.app.query_one("#evidence-table", DataTable).row_count, 0)

    async def test_collector_evidence_without_a_source_hash_remains_visible(self):
        self.model.data["evidence"][0].update(sha256=None, source_url="")
        from textual.widgets import DataTable
        async with self.app.run_test(size=(140, 45)) as pilot:
            await pilot.pause()
            self.assertEqual(self.app.query_one("#evidence-table", DataTable).row_count, 1)

    async def test_real_case_flow_saves_report_and_refreshes_evidence(self):
        from fieldkit_core.workbench import Workbench
        from textual.widgets import DataTable
        with tempfile.TemporaryDirectory() as directory:
            model = Workbench(Path(directory) / "case")
            model.create_flow("Public query plan", [{"command": "person", "target": "Synthetic Person", "options": {}}])
            self.app = tui.create_app(workbench=model)
            async with self.app.run_test(size=(140, 45)) as pilot:
                await pilot.pause()
                self.app.action_run_step()
                await self.app.workers.wait_for_complete()
                await pilot.pause()
                state = model.snapshot()
                self.assertEqual(state["flows"][0]["steps"][0]["status"], "completed")
                self.assertEqual(self.app.query_one("#evidence-table", DataTable).row_count, 1)
                self.assertTrue(model.store.verify()["ok"])

    async def test_real_case_note_and_new_file_export_persist(self):
        from fieldkit_core.workbench import Workbench
        with tempfile.TemporaryDirectory() as directory:
            path, report_path = Path(directory) / "case", Path(directory) / "report.md"
            model = Workbench(path)
            evidence = model.add_evidence("Synthetic observation", "Supplied training text", "https://example.org/")
            self.app = tui.create_app(workbench=model)
            async with self.app.run_test(size=(140, 50)) as pilot:
                await pilot.pause()
                self.app.action_add_note()
                await pilot.pause()
                await self.fill("#note-content", "Record an alternative explanation before correlating")
                await self.save(pilot)
                self.app.action_export_case()
                await pilot.pause()
                await self.fill("#export-path", str(report_path))
                await self.save(pilot)
                self.assertIn("Synthetic observation", report_path.read_text())
                self.assertEqual(Workbench(path).snapshot()["notes"][0]["evidence_ids"], [evidence["id"]])
                self.assertEqual(report_path.stat().st_mode & 0o777, 0o600)


class BootstrapTests(unittest.TestCase):
    def test_missing_optional_dependency_is_actionable(self):
        with patch.object(tui.importlib.metadata, "version", side_effect=importlib.metadata.PackageNotFoundError()):
            with self.assertRaises(Failure) as caught:
                tui.create_app(workbench=FixtureWorkbench())
        self.assertEqual(caught.exception.code, "TUI_DEPENDENCY")
        self.assertIn(".[tui]", caught.exception.message)


if __name__ == "__main__":
    unittest.main()
