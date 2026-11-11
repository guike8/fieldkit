"""Agents and human interfaces operate the same evidence-backed case model."""
from pathlib import Path
import tempfile
import unittest

from fieldkit_core.mcp_server import ToolBroker


class WorkspaceMCPTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name).resolve()
        self.broker = ToolBroker(self.root, case="case")

    def test_add_evidence_and_notes_are_visible_in_shared_workspace(self):
        evidence = self.broker.invoke("fieldkit_evidence_add", {"title": "Public source observation", "content": "Observed fact", "source_url": "https://example.org/"})
        self.assertEqual(evidence["status"], "ok")
        identifier = evidence["data"]["id"]
        note = self.broker.invoke("fieldkit_note_add", {"content": "Needs corroboration", "evidence_ids": [identifier]})
        self.assertEqual(note["status"], "ok")
        snapshot = self.broker.invoke("fieldkit_case_workspace", {})
        self.assertEqual(snapshot["data"]["evidence"][0]["id"], identifier)
        self.assertEqual(snapshot["data"]["notes"][0]["content"], "Needs corroboration")

    def test_manual_links_require_existing_supporting_evidence(self):
        a = self.broker.invoke("fieldkit_entity_add", {"kind": "profile", "value": "https://example.org/a"})["data"]
        b = self.broker.invoke("fieldkit_entity_add", {"kind": "profile", "value": "https://example.org/b"})["data"]
        rejected = self.broker.invoke("fieldkit_entity_link", {"source_id": a["id"], "target_id": b["id"], "relation": "possible association", "evidence_ids": []})
        self.assertEqual(rejected["status"], "invalid")
        evidence = self.broker.invoke("fieldkit_evidence_add", {"title": "Cross-link", "content": "Page A links to page B"})["data"]
        accepted = self.broker.invoke("fieldkit_entity_link", {"source_id": a["id"], "target_id": b["id"], "relation": "links_to", "evidence_ids": [evidence["id"]]})
        self.assertEqual(accepted["status"], "ok")
        self.assertEqual(accepted["data"]["status"], "analyst_assertion")

    def test_flow_runs_produce_immutable_evidence_in_same_case(self):
        flow = self.broker.invoke("fieldkit_flow_create", {"title": "Research lead", "steps": [{"command": "person", "target": "Example Name"}]})["data"]
        run = self.broker.invoke("fieldkit_flow_run_step", {"flow_id": flow["id"], "step_id": flow["steps"][0]["id"]})
        self.assertEqual(run["status"], "ok", run)
        snapshot = self.broker.invoke("fieldkit_case_workspace", {})["data"]
        self.assertEqual(snapshot["flows"][0]["steps"][0]["run_id"], run["data"]["run_id"])
        self.assertTrue(self.broker.invoke("fieldkit_case_verify", {})["data"]["ok"])

    def test_flow_cannot_bypass_workspace_path_policy(self):
        flow = self.broker.invoke("fieldkit_flow_create", {"title": "Local scope", "steps": [{"command": "file", "target": "../private.txt"}]})["data"]
        run = self.broker.invoke("fieldkit_flow_run_step", {"flow_id": flow["id"], "step_id": flow["steps"][0]["id"]})
        self.assertEqual(run["status"], "invalid")
        self.assertEqual(run["errors"][0]["code"], "MCP_PATH_OUTSIDE_WORKSPACE")

    def test_workspace_tools_only_exist_for_operator_configured_case(self):
        broker = ToolBroker(self.root)
        self.assertNotIn("fieldkit_evidence_add", broker.schemas)
        self.assertEqual(broker.invoke("fieldkit_evidence_add", {"title": "note"})["status"], "invalid")


if __name__ == "__main__":
    unittest.main()
