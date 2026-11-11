from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import stat
import tempfile
import unittest
import uuid

from fieldkit_core.case import CaseStore
from fieldkit_core.common import Failure


def report(run_id=None, body=b"public source", target="example.org", *, sources=True):
    digest = hashlib.sha256(body).hexdigest()
    value = {"schema_version": "1", "run_id": run_id or "run_" + uuid.uuid4().hex,
             "command": "dns", "target": target, "status": "ok", "started_at": "2026-11-11T12:00:00Z",
             "data": {"records": [{"type": "A", "data": "93.184.215.14"}]},
             "sources": [{"source_id": "source_one", "kind": "http", "url": "https://dns.example/",
                          "sha256": digest, "bytes": len(body)}] if sources else [],
             "errors": [],
             "entities": [{"type": "domain", "value": target, "source_ids": ["source_one"]}],
             "relationships": [{"source": {"type": "domain", "value": target}, "relation": "resolves_to",
                                "target": {"type": "ipv4", "value": "93.184.215.14"}, "source_ids": ["source_one"]}]}
    return value, {digest: body} if sources else {}


class CaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.path = Path(self.temporary.name) / "case"

    def tearDown(self):
        self.temporary.cleanup()

    def test_private_storage_and_round_trip(self):
        case = CaseStore(self.path)
        value, artifacts = report()
        original = json.loads(json.dumps(value))
        receipt = case.save(value, artifacts)
        self.assertEqual(value, original)
        self.assertEqual(receipt["run_id"], value["run_id"])
        canonical = json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"), allow_nan=False)
        self.assertEqual(receipt["report_sha256"], hashlib.sha256(canonical.encode()).hexdigest())
        self.assertEqual(stat.S_IMODE(self.path.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(case.blobs_path.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(case.database_path.stat().st_mode), 0o600)
        for digest, body in artifacts.items():
            self.assertEqual((case.blobs_path / digest).read_bytes(), body)
            self.assertEqual(stat.S_IMODE((case.blobs_path / digest).stat().st_mode), 0o600)
        shown = CaseStore(self.path, create=False).show()
        self.assertEqual(shown["summary"]["run_count"], 1)
        self.assertEqual(shown["recent_runs"][0]["run_id"], value["run_id"])
        self.assertTrue(case.verify()["ok"])

    def test_duplicate_run_is_not_overwritten(self):
        case = CaseStore(self.path)
        value, artifacts = report()
        case.save(value, artifacts)
        value["status"] = "error"
        with self.assertRaises(Failure) as caught:
            case.save(value, artifacts)
        self.assertEqual(caught.exception.code, "RUN_EXISTS")
        self.assertEqual(case.show()["recent_runs"][0]["status"], "ok")

    def test_artifact_dedup_and_existing_reference(self):
        case = CaseStore(self.path)
        first, artifacts = report()
        second, _ = report()
        case.save(first, artifacts)
        case.save(second, {})
        self.assertEqual(case.show()["summary"]["artifact_count"], 1)
        self.assertEqual(len(list(case.blobs_path.iterdir())), 1)
        self.assertEqual(case.verify()["source_refs_checked"], 2)

    def test_missing_artifact_rejected_without_run(self):
        case = CaseStore(self.path)
        value, _ = report()
        with self.assertRaises(Failure) as caught:
            case.save(value, {})
        self.assertEqual(caught.exception.code, "MISSING_ARTIFACT")
        self.assertEqual(case.show()["summary"]["run_count"], 0)

    def test_bad_artifact_key_does_not_create_path(self):
        case = CaseStore(self.path)
        value, _ = report(sources=False)
        for digest, body in (("../escape", b"x"), ("f" * 64, b"wrong")):
            with self.assertRaises(Failure):
                case.save(value, {digest: body})
        self.assertFalse((self.path / "escape").exists())
        self.assertEqual(list(case.blobs_path.iterdir()), [])

    def test_local_hash_only_source_is_not_copied_or_reopened(self):
        case = CaseStore(self.path)
        value, _ = report(sources=False)
        value["sources"] = [{"source_id": "local", "kind": "local_file", "path": "/not/present",
                             "sha256": "f" * 64, "raw_copied": False}]
        case.save(value, {})
        verified = case.verify()
        self.assertTrue(verified["ok"])
        self.assertEqual(verified["artifacts_checked"], 0)
        self.assertEqual(verified["source_refs_checked"], 0)

    def test_show_deduplicates_explicit_graph_only(self):
        case = CaseStore(self.path)
        first, artifacts = report()
        second, _ = report()
        second["entities"][0]["source_ids"] = ["source_two"]
        second["relationships"][0]["source_ids"] = ["source_two"]
        case.save(first, artifacts)
        case.save(second, artifacts)
        graph = case.show()["graph"]
        self.assertEqual(len(graph["entities"]), 2)
        self.assertEqual(len(graph["relationships"]), 1)
        self.assertEqual(set(graph["relationships"][0]["source_ids"]), {"source_one", "source_two"})
        self.assertEqual(set(graph["relationships"][0]["run_ids"]), {first["run_id"], second["run_id"]})
        self.assertEqual(graph["relationships"][0]["relation"], "resolves_to")
        self.assertNotIn("same_person", json.dumps(graph))

    def test_show_selection_is_explicitly_truncated(self):
        case = CaseStore(self.path)
        for index in range(3):
            value, artifacts = report(target=f"host{index}.example.org")
            case.save(value, artifacts)
        shown = case.show(limit=1)
        self.assertTrue(shown["selection"]["truncated"])
        self.assertEqual(shown["selection"]["returned"], 1)
        self.assertTrue(shown["graph"]["truncated"])
        self.assertEqual(shown["graph"]["runs_included"], 1)
        self.assertEqual(shown["recent_runs"][0]["target"], "host2.example.org")

    def test_graph_entities_and_provenance_are_bounded(self):
        case = CaseStore(self.path)
        value, artifacts = report()
        value["relationships"] = []
        value["entities"] = [{"type": "domain", "value": f"host{x}.example.org", "source_ids": [f"src{i}" for i in range(150)]}
                             for x in range(2050)]
        case.save(value, artifacts)
        graph = case.show()["graph"]
        self.assertEqual(len(graph["entities"]), 2000)
        self.assertTrue(graph["truncated"])
        self.assertEqual(graph["omitted_entities"], 50)
        self.assertTrue(graph["provenance_truncated"])
        self.assertEqual(len(graph["entities"][0]["source_ids"]), 128)

    def test_export_formats_and_raw_body_exclusion(self):
        case = CaseStore(self.path)
        value, artifacts = report(body=b"RAW_BODY_NOT_EMBEDDED")
        case.save(value, artifacts)
        document = json.loads(case.export("json"))
        self.assertEqual(document["runs"][0]["report"], value)
        self.assertEqual(document["artifacts"][0]["path"], "blobs/" + next(iter(artifacts)))
        self.assertNotIn("RAW_BODY_NOT_EMBEDDED", case.export("json"))
        lines = [json.loads(line) for line in case.export("jsonl").splitlines()]
        self.assertEqual([line["record_type"] for line in lines], ["case", "run"])
        self.assertEqual(lines[1]["report"], value)
        markdown = case.export("markdown")
        self.assertIn("# Fieldkit case export", markdown)
        self.assertIn(value["run_id"], markdown)
        self.assertIn("Report SHA-256", markdown)
        self.assertIn("blobs/", markdown)

    def test_markdown_escapes_table_and_fences(self):
        case = CaseStore(self.path)
        value, artifacts = report(target="x|<script>\nnext")
        value["data"] = {"untrusted": "```\nattack"}
        case.save(value, artifacts)
        markdown = case.export("markdown")
        self.assertIn("x\\|&lt;script&gt; next", markdown)
        self.assertIn("````json", markdown)

    def test_readonly_missing_case_does_not_initialize(self):
        with self.assertRaises(Failure):
            CaseStore(self.path, create=False)
        self.assertFalse(self.path.exists())

    def test_existing_unrelated_sqlite_file_unchanged(self):
        self.path.mkdir()
        database = self.path / "case.sqlite3"
        connection = sqlite3.connect(database)
        connection.execute("CREATE TABLE unrelated(value TEXT)")
        connection.execute("INSERT INTO unrelated VALUES('keep')")
        connection.commit()
        connection.close()
        original = database.read_bytes()
        with self.assertRaises(Failure) as caught:
            CaseStore(self.path)
        self.assertEqual(caught.exception.code, "CASE_SCHEMA")
        self.assertEqual(database.read_bytes(), original)

    def test_database_and_directory_symlinks_rejected(self):
        external = Path(self.temporary.name) / "original"
        external.write_bytes(b"do not overwrite")
        self.path.mkdir()
        (self.path / "case.sqlite3").symlink_to(external)
        with self.assertRaises(Failure) as caught:
            CaseStore(self.path)
        self.assertEqual(caught.exception.code, "CASE_PATH_UNSAFE")
        self.assertEqual(external.read_bytes(), b"do not overwrite")
        link = Path(self.temporary.name) / "directory_link"
        link.symlink_to(self.path, target_is_directory=True)
        with self.assertRaises(Failure):
            CaseStore(link)

    def test_blob_symlink_rejected_and_external_untouched(self):
        case = CaseStore(self.path)
        value, artifacts = report()
        digest = next(iter(artifacts))
        external = Path(self.temporary.name) / "outside"
        external.write_bytes(artifacts[digest])
        (case.blobs_path / digest).symlink_to(external)
        with self.assertRaises(Failure) as caught:
            case.save(value, artifacts)
        self.assertEqual(caught.exception.code, "CASE_PATH_UNSAFE")
        self.assertEqual(external.read_bytes(), artifacts[digest])
        self.assertEqual(case.show()["summary"]["run_count"], 0)

    def test_artifact_tampering_detected_and_never_overwritten(self):
        case = CaseStore(self.path)
        value, artifacts = report()
        case.save(value, artifacts)
        path = case.blobs_path / next(iter(artifacts))
        path.write_bytes(b"tampered")
        verified = case.verify()
        self.assertFalse(verified["ok"])
        self.assertTrue(any(item["code"] == "ARTIFACT_HASH" for item in verified["errors"]))
        other, _ = report()
        with self.assertRaises(Failure):
            case.save(other, artifacts)
        self.assertEqual(path.read_bytes(), b"tampered")

    def test_report_tampering_detected(self):
        case = CaseStore(self.path)
        value, artifacts = report()
        case.save(value, artifacts)
        connection = sqlite3.connect(case.database_path)
        connection.execute("UPDATE runs SET result_json='{}'")
        connection.commit()
        connection.close()
        verified = case.verify()
        self.assertFalse(verified["ok"])
        self.assertTrue(any(item["code"] == "REPORT_HASH" for item in verified["errors"]))
        with self.assertRaises(Failure) as caught:
            case.show()
        self.assertEqual(caught.exception.code, "CASE_REPORT_CORRUPT")

    def test_deleted_reference_detected(self):
        case = CaseStore(self.path)
        value, artifacts = report()
        case.save(value, artifacts)
        connection = sqlite3.connect(case.database_path)
        connection.execute("DELETE FROM run_artifacts")
        connection.commit()
        connection.close()
        self.assertFalse(case.verify()["ok"])
        self.assertTrue(any(item["code"] == "SOURCE_ARTIFACT_REFERENCE" for item in case.verify()["errors"]))

    def test_concurrent_saves(self):
        case = CaseStore(self.path)
        jobs = [report() for _ in range(20)]
        with ThreadPoolExecutor(max_workers=8) as executor:
            receipts = list(executor.map(lambda job: case.save(*job), jobs))
        self.assertEqual(len({item["run_id"] for item in receipts}), 20)
        self.assertEqual(case.show()["summary"]["run_count"], 20)
        self.assertEqual(case.show()["summary"]["artifact_count"], 1)
        self.assertTrue(case.verify()["ok"])

    def test_concurrent_constructors_and_saves(self):
        def task(index):
            case = CaseStore(self.path)
            value, artifacts = report(target=f"host{index}.example.org")
            return case.save(value, artifacts)
        with ThreadPoolExecutor(max_workers=6) as executor:
            receipts = list(executor.map(task, range(12)))
        self.assertEqual(len(receipts), 12)
        self.assertEqual(CaseStore(self.path, create=False).show()["summary"]["run_count"], 12)

    def test_limits_and_invalid_report(self):
        case = CaseStore(self.path)
        for limit in (0, -1, 1.5, True, 10001):
            with self.assertRaises(Failure):
                case.show(limit)
        with self.assertRaises(Failure):
            case.export("html")
        value, artifacts = report()
        value["data"] = {"score": float("nan")}
        with self.assertRaises(Failure) as caught:
            case.save(value, artifacts)
        self.assertEqual(caught.exception.code, "INVALID_REPORT")


if __name__ == "__main__":
    unittest.main()
