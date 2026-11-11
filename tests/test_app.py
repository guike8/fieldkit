"""Real loopback API tests for the shared evidence workspace."""
import base64
import http.client
import json
from pathlib import Path
import tempfile
import threading
import time
import unittest

from fieldkit_core.app import CaseApplication, CaseHTTPServer


class AppTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.workspace = Path(self.directory.name).resolve()
        self.app = CaseApplication(self.workspace, "case")
        self.addCleanup(self.app.close)
        try:
            self.server = CaseHTTPServer(("127.0.0.1", 0), self.app)
        except PermissionError:
            self.skipTest("Execution sandbox disallows loopback sockets")
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": .01}, daemon=True)
        self.thread.start()
        _, session, _ = self.request("GET", "/api/session")
        self.token = session["csrf_token"]

    def request(self, method, path, payload=None, headers=None, raw=False):
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=5)
        prepared = {"Content-Type": "application/json"}
        if hasattr(self, "token"):
            prepared["X-Fieldkit-Token"] = self.token
        prepared.update(headers or {})
        body = json.dumps(payload).encode() if payload is not None else None
        connection.request(method, path, body, headers=prepared)
        response = connection.getresponse()
        data, response_headers, status = response.read(), dict(response.getheaders()), response.status
        connection.close()
        return status, data if raw else json.loads(data), response_headers

    def test_capture_review_and_reload_share_the_same_case(self):
        status, response, _ = self.request("POST", "/api/evidence", {"title": "Observed source", "content": "An observation", "source_url": "https://example.org/"})
        self.assertEqual(status, 200)
        evidence = response["record"]
        self.assertEqual(evidence["status"], "unreviewed")
        _, reviewed, _ = self.request("POST", "/api/evidence/review", {"evidence_id": evidence["id"], "status": "corroborated", "note": "Cross-checked by analyst"})
        self.assertEqual(reviewed["record"]["status"], "corroborated")
        from fieldkit_core.workbench import Workbench
        persisted = Workbench(self.app.case).snapshot()
        self.assertEqual(persisted["evidence"][0]["status"], "corroborated")
        self.assertTrue(persisted["evidence"][0]["sha256"])

    def test_csrf_host_and_origin_guards_prevent_cross_site_writes(self):
        for headers in ({"X-Fieldkit-Token": "wrong"}, {"Origin": "https://attacker.example"},
                        {"Host": "attacker.example"}, {"Sec-Fetch-Site": "cross-site"}):
            with self.subTest(headers=headers):
                status, _, _ = self.request("POST", "/api/notes", {"content": "Injected"}, headers=headers)
                self.assertEqual(status, 403)
        self.assertEqual(self.app.model.snapshot()["notes"], [])

    def test_local_collector_paths_stay_in_workspace(self):
        status, response, _ = self.request("POST", "/api/tools/run", {"command": "file", "target": "../outside"})
        self.assertEqual(status, 400)
        self.assertEqual(response["error"]["code"], "MCP_PATH_OUTSIDE_WORKSPACE")
        self.assertEqual(self.app.jobs, {})

    def test_flow_step_executes_in_background_and_produces_evidence(self):
        _, created, _ = self.request("POST", "/api/flows", {"title": "Local diagnostic", "steps": [{"command": "person", "target": "Example Name"}]})
        flow = created["record"]
        status, launched, _ = self.request("POST", "/api/flows/run", {"flow_id": flow["id"], "step_id": flow["steps"][0]["id"]})
        self.assertEqual(status, 202)
        for _ in range(100):
            job = self.app.jobs[launched["job_id"]]
            if job["status"] in {"completed", "failed"}:
                break
            time.sleep(.01)
        self.assertEqual(job["status"], "completed", job)
        state = self.app.model.snapshot()
        self.assertEqual(state["flows"][0]["steps"][0]["status"], "completed")
        self.assertTrue(any(item.get("run_id") == job["run_id"] for item in state["evidence"]))

    def test_attachment_capture_and_integrity_checked_download(self):
        body = b"original evidence bytes"
        status, saved, _ = self.request("POST", "/api/attachments", {"name": "source.txt", "content_base64": base64.b64encode(body).decode(), "media_type": "text/plain"})
        self.assertEqual(status, 200, saved)
        digest = saved["record"]["sha256"]
        status, downloaded, headers = self.request("GET", "/api/artifacts/" + digest, raw=True)
        self.assertEqual(status, 200)
        self.assertEqual(downloaded, body)
        self.assertIn("attachment", headers["Content-Disposition"])
        self.assertEqual(headers["X-Content-Type-Options"], "nosniff")

    def test_workspace_export_includes_notes_and_flows(self):
        self.app.model.add_note("Analyst rationale")
        self.app.model.create_flow("Research plan")
        status, body, headers = self.request("GET", "/api/export?format=json", raw=True)
        self.assertEqual(status, 200)
        self.assertIn(b"Analyst rationale", body)
        self.assertIn(b"Research plan", body)
        self.assertIn("attachment", headers["Content-Disposition"])

    def test_unknown_fields_cannot_control_case_paths(self):
        status, _, _ = self.request("POST", "/api/evidence", {"title": "note", "case": "/outside"})
        self.assertEqual(status, 400)
        self.assertEqual(len(self.app.model.snapshot()["evidence"]), 0)


if __name__ == "__main__":
    unittest.main()
