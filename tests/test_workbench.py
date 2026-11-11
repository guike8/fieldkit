"""Investigation workflow invariants, using local fixtures and mocked collectors."""
import hashlib
import json
from pathlib import Path
import sqlite3
import tempfile
import threading
import unittest
from unittest.mock import patch

from fieldkit_core.case import CaseStore
from fieldkit_core.common import Failure, result
from fieldkit_core.workbench import Workbench, _json


class WorkbenchTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.workbench = Workbench(self.directory.name)

    def evidence(self, **extra):
        return self.workbench.add_evidence("Source observation", "A supplied observation", **extra)

    def test_metadata_persists_and_case_updates_are_journaled(self):
        original = self.workbench.snapshot()["metadata"]
        updated = self.workbench.update_case("Harbor research", "What facts support this lead?")
        reopened = Workbench(self.directory.name).snapshot()
        self.assertEqual(updated["case_id"], original["case_id"])
        self.assertEqual(reopened["metadata"]["title"], "Harbor research")
        self.assertEqual(reopened["activity"][0]["action"], "case.updated")

    def test_supplied_evidence_preserves_exact_bytes_and_source_time(self):
        content = "\n  Unicode observation: café  \n"
        evidence = self.workbench.add_evidence("Observation", content, "https://example.org/source", "web", "2026-01-01T03:00:00+02:00")
        digest = hashlib.sha256(content.encode()).hexdigest()
        self.assertEqual(evidence["sha256"], digest)
        self.assertEqual(evidence["content"], content)
        self.assertEqual((Path(self.directory.name) / "blobs" / digest).read_bytes(), content.encode())
        self.assertEqual(evidence["observed_at"], "2026-01-01T03:00:00+02:00")
        self.assertNotEqual(evidence["observed_at"], evidence["collected_at"])
        self.assertEqual(evidence["collection_method"], "analyst_supplied")
        self.assertTrue(self.workbench.store.verify()["ok"])

    def test_manual_source_url_does_not_cause_a_request(self):
        with patch("fieldkit_core.transport.Context.fetch", side_effect=AssertionError("manual evidence is not a fetch")):
            evidence = self.evidence(source_url="https://example.org/claim")
        self.assertEqual(evidence["source_url"], "https://example.org/claim")

    def test_invalid_evidence_is_rejected_before_recording(self):
        inputs = [dict(title="", content="text"), dict(title="A", content="x" * 20001),
                  dict(title="A", source_url="javascript:alert(1)"), dict(title="A", source_url=[]),
                  dict(title="A", observed_at="not a date"), dict(title="A", kind="arbitrary kind"),
                  dict(title="A", content="\ud800")]
        for options in inputs:
            with self.subTest(options=str(options)[:80]), self.assertRaises(Failure):
                self.workbench.add_evidence(**options)
        self.assertEqual(self.workbench.store.show()["summary"]["run_count"], 0)

    def test_attachment_is_content_addressed_without_using_supplied_path(self):
        body = b"\x89PNG\r\n\x1a\nfixture"
        evidence = self.workbench.add_attachment("../../outside.png", body, "image/png")
        self.assertEqual(evidence["filename"], "outside.png")
        self.assertEqual(evidence["bytes"], len(body))
        self.assertEqual((Path(self.directory.name) / "blobs" / evidence["sha256"]).read_bytes(), body)
        self.assertFalse((Path(self.directory.name) / "outside.png").exists())
        with self.assertRaises(Failure):
            self.workbench.add_attachment("large.bin", b"x" * (8 * 1024 * 1024 + 1))

    def test_review_changes_assessment_without_replacing_original_report(self):
        evidence = self.evidence()
        before = json.loads(self.workbench.store.export())["runs"][0]
        reviewed = self.workbench.review_evidence(evidence["id"], "verified", "Compared with an independent record")
        after = json.loads(self.workbench.store.export())["runs"][0]
        self.assertEqual(reviewed["status"], "corroborated")
        self.assertEqual(before["report_sha256"], after["report_sha256"])
        self.assertEqual(before["report"], after["report"])
        self.assertEqual(self.workbench.snapshot()["activity"][0]["action"], "evidence.reviewed")
        self.assertTrue(self.workbench.store.verify()["ok"])

    def test_review_accepts_discarded_and_legacy_archive_alias(self):
        evidence = self.evidence()
        self.assertEqual(self.workbench.review_evidence(evidence["id"], "archived")["status"], "discarded")
        with self.assertRaises(Failure):
            self.workbench.review_evidence(evidence["id"], [])

    def test_notes_require_existing_case_evidence(self):
        evidence = self.evidence()
        note = self.workbench.add_note("Alternative explanation to investigate", [evidence["id"]])
        self.assertEqual(note["evidence_ids"], [evidence["id"]])
        for identifiers in (["missing"], [{}], "not-an-array"):
            with self.subTest(identifiers=identifiers), self.assertRaises(Failure):
                self.workbench.add_note("A note", identifiers)

    def test_entity_identity_remains_typed_and_manual_links_need_evidence(self):
        first = self.workbench.add_entity("username", "site:alice", "First")
        again = self.workbench.add_entity("username", "site:alice", "Changed label")
        other = self.workbench.add_entity("person", "site:alice")
        self.assertEqual(first["id"], again["id"])
        self.assertNotEqual(first["id"], other["id"])
        with self.assertRaises(Failure):
            self.workbench.add_link(first["id"], other["id"], "possible_association", [])
        evidence = self.evidence()
        link = self.workbench.add_link(first["id"], other["id"], "possible_association", [evidence["id"]], "Analyst assertion")
        self.assertEqual(link["status"], "analyst_assertion")
        self.assertEqual(link["evidence_ids"], [evidence["id"]])

    def test_link_requires_distinct_existing_endpoints_and_known_evidence(self):
        first = self.workbench.add_entity("person", "Sample A")
        second = self.workbench.add_entity("person", "Sample B")
        evidence = self.evidence()
        for origin, target, references in ((first["id"], first["id"], [evidence["id"]]),
                                           ([], second["id"], [evidence["id"]]),
                                           (first["id"], "missing", [evidence["id"]]),
                                           (first["id"], second["id"], ["missing"])):
            with self.subTest(origin=origin, target=target), self.assertRaises(Failure):
                self.workbench.add_link(origin, target, "related_to", references)

    def test_flow_steps_are_validated_before_a_plan_is_written(self):
        for steps in ([{"command": "not-a-command"}], [{"command": "tls", "target": "example.org", "options": {"port": True}}],
                      [{"command": "doctor", "shell": "anything"}], [{}] * 21):
            with self.subTest(steps=str(steps)[:80]), self.assertRaises(Failure):
                self.workbench.create_flow("Invalid plan", steps)
        self.assertEqual(self.workbench.snapshot()["flows"], [])

    def test_flow_run_persists_result_as_evidence_and_tracks_step(self):
        flow = self.workbench.create_flow("Collect one record", [{"command": "person", "target": "Example Name"}])
        with patch("fieldkit_core.cli.dispatch", return_value=result({"fixture": True}, entities=[{"type": "person", "value": "Example Name", "source_ids": []}])):
            report = self.workbench.run_step(flow["id"], flow["steps"][0]["id"])
        snapshot = self.workbench.snapshot()
        step = snapshot["flows"][0]["steps"][0]
        self.assertEqual(step["status"], "completed")
        self.assertEqual(step["run_id"], report["run_id"])
        self.assertEqual(snapshot["flows"][0]["status"], "completed")
        evidence = snapshot["evidence"][0]
        self.assertEqual(evidence["id"], "evidence_" + report["run_id"])
        self.assertEqual(evidence["hash_scope"], "collector_report")
        self.assertEqual(len(evidence["sha256"]), 64)
        self.assertTrue(self.workbench.store.verify()["ok"])

    def test_flow_failure_remains_failed_and_keeps_the_provider_result(self):
        flow = self.workbench.create_flow("Blocked record", [{"command": "person", "target": "Example"}])
        failure = Failure("BLOCKED", "Fixture provider blocked this request", "blocked")
        with patch("fieldkit_core.cli.dispatch", side_effect=failure):
            report = self.workbench.run_step(flow["id"], flow["steps"][0]["id"])
        snapshot = self.workbench.snapshot()
        self.assertEqual(report["status"], "blocked")
        self.assertEqual(snapshot["flows"][0]["status"], "failed")
        self.assertEqual(snapshot["flows"][0]["steps"][0]["error"], failure.message)
        self.assertEqual(snapshot["evidence"][0]["collector_status"], "blocked")

    def test_running_step_cannot_be_started_twice(self):
        flow = self.workbench.create_flow("Single execution", [{"command": "person", "target": "Example"}])
        entered, release = threading.Event(), threading.Event()
        errors = []
        def slow(*args):
            entered.set()
            release.wait(3)
            return result({"fixture": True})
        def first():
            try:
                self.workbench.run_step(flow["id"], flow["steps"][0]["id"])
            except Exception as exc:
                errors.append(exc)
        with patch("fieldkit_core.cli.dispatch", side_effect=slow):
            thread = threading.Thread(target=first)
            thread.start()
            self.assertTrue(entered.wait(2))
            try:
                with self.assertRaises(Failure) as caught:
                    Workbench(self.directory.name).run_step(flow["id"], flow["steps"][0]["id"])
                self.assertEqual(caught.exception.code, "WORKBENCH_STEP_RUNNING")
            finally:
                release.set()
                thread.join(3)
        self.assertFalse(errors)

    def test_job_policy_normalizes_local_targets_before_execution(self):
        seen = []
        def policy(command, target, options):
            seen.append((command, target, options))
            return command, "/approved/evidence.txt", options
        scoped = Workbench(self.directory.name, job_policy=policy)
        with patch("fieldkit_core.cli.dispatch", return_value=result({"fixture": True})) as dispatch:
            scoped.run_tool("file", "relative.txt")
        self.assertEqual(seen, [("file", "relative.txt", {})])
        self.assertEqual(dispatch.call_args.args[2], "/approved/evidence.txt")

    def test_job_policy_rejection_is_not_executed(self):
        def policy(*args):
            raise Failure("SCOPE", "Path outside workspace", "invalid")
        scoped = Workbench(self.directory.name, job_policy=policy)
        with patch("fieldkit_core.cli.dispatch") as dispatch, self.assertRaises(Failure):
            scoped.run_tool("file", "outside.txt")
        dispatch.assert_not_called()

    def test_demo_is_explicit_synthetic_and_idempotent_without_network(self):
        with patch("fieldkit_core.transport.Context.fetch", side_effect=AssertionError("seeding must not fetch")):
            first = self.workbench.seed_demo()
            second = self.workbench.seed_demo()
        self.assertTrue(first["metadata"]["is_demo"])
        self.assertEqual(first["stats"], second["stats"])
        self.assertTrue(all(item["is_demo"] for item in first["evidence"]))
        self.assertEqual(len(first["correlations"]), 2)
        self.assertTrue(all(item["status"] == "candidate" for item in first["correlations"]))

    def test_demo_refuses_to_overwrite_a_real_investigation(self):
        self.evidence()
        with self.assertRaises(Failure):
            self.workbench.seed_demo()
        self.assertFalse(self.workbench.snapshot()["metadata"]["is_demo"])

    def test_correlations_need_review_and_never_merge_entities(self):
        snapshot = self.workbench.seed_demo()
        lead = next(item for item in snapshot["correlations"] if item["signal"] == "shared_handle")
        self.assertEqual(lead["strength"], "weak")
        count = len(snapshot["entities"])
        with self.assertRaises(Failure):
            self.workbench.review_correlation(lead["id"], "accepted", "")
        reviewed = self.workbench.review_correlation(lead["id"], "accepted", "Keep as a working lead; independent contributors remain possible")
        after = self.workbench.snapshot()
        self.assertEqual(reviewed["status"], "accepted")
        self.assertEqual(len(after["entities"]), count)
        association = next(item for item in after["links"] if item["status"] == "analyst_accepted_lead")
        self.assertEqual(association["relation"], "possible_association")
        self.assertTrue(association["evidence_ids"])
        self.assertIn("Not an identity merge", association["interpretation"])

    def test_rejected_correlation_remains_auditable(self):
        lead = self.workbench.seed_demo()["correlations"][0]
        self.workbench.review_correlation(lead["id"], "rejected", "Common project membership is a sufficient alternative explanation")
        reopened = Workbench(self.directory.name).snapshot()
        self.assertEqual(next(item for item in reopened["correlations"] if item["id"] == lead["id"])["status"], "rejected")
        self.assertEqual(reopened["activity"][0]["action"], "correlation.reviewed")
        self.assertFalse(any(item["status"] == "analyst_accepted_lead" for item in reopened["links"]))

    def test_same_handle_in_one_scope_does_not_create_cross_platform_lead(self):
        evidence = self.evidence()
        a = self.workbench.add_entity("username", "site:Alice")
        b = self.workbench.add_entity("handle", "site:Alice")
        marker = self.workbench.add_entity("person", "Fixture")
        self.workbench.add_link(a["id"], marker["id"], "observed_on", [evidence["id"]])
        self.workbench.add_link(b["id"], marker["id"], "observed_on", [evidence["id"]])
        self.assertFalse(any(item["signal"] == "shared_handle" for item in self.workbench.correlations()))

    def test_workspace_export_includes_notes_flows_and_review_provenance(self):
        snapshot = self.workbench.seed_demo()
        self.workbench.review_correlation(snapshot["correlations"][0]["id"], "accepted", "A lead to follow, not a conclusion")
        exported = json.loads(self.workbench.export("json"))
        self.assertTrue(exported["workspace"]["notes"])
        self.assertTrue(exported["workspace"]["flows"])
        self.assertTrue(exported["collector_ledger"]["runs"])
        self.assertTrue(exported["integrity"]["ok"])
        markdown = self.workbench.export("markdown")
        self.assertIn("Research flows", markdown)
        self.assertIn("Analyst notes", markdown)
        self.assertIn("Correlation reviews", markdown)

    def test_concurrent_analyst_actions_keep_all_notes_and_audit_hashes(self):
        errors = []
        def add(index):
            try:
                Workbench(self.directory.name).add_note("Concurrent note " + str(index))
            except Exception as exc:
                errors.append(exc)
        threads = [threading.Thread(target=add, args=(index,)) for index in range(8)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(5)
        self.assertFalse(errors)
        self.assertEqual(len(self.workbench.snapshot()["notes"]), 8)
        connection = sqlite3.connect(self.workbench.store.database_path)
        try:
            previous = "0" * 64
            for payload, prior, digest in connection.execute("SELECT payload,previous_hash,event_hash FROM wb_activity ORDER BY rowid"):
                self.assertEqual(prior, previous)
                self.assertEqual(digest, hashlib.sha256((previous + _json(json.loads(payload))).encode()).hexdigest())
                previous = digest
        finally:
            connection.close()


if __name__ == "__main__":
    unittest.main()
