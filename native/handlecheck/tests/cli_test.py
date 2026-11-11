#!/usr/bin/env python3
"""Deterministic CLI integration tests using the test-only HTTP transport."""
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest


BINARY = Path(sys.argv.pop(1) if len(sys.argv) > 1 else "build/handlecheck_fixture").resolve()


class CliTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="handlecheck-cli-")
        self.root = Path(self.directory.name)
        self.trace = self.root / "requests.txt"
        self.addCleanup(self.directory.cleanup)

    def run_cli(self, *arguments, scenario="normal"):
        self.trace.unlink(missing_ok=True)
        environment = os.environ.copy()
        environment.update(HC_TEST_SCENARIO=scenario, HC_TEST_TRACE=str(self.trace))
        return subprocess.run(
            [str(BINARY), *arguments], env=environment,
            capture_output=True, text=True, timeout=10,
        )

    def requests(self):
        if not self.trace.exists():
            return []
        return [line[4:] for line in self.trace.read_text().splitlines() if line.startswith("GET ")]

    def report(self, process, expected_code):
        self.assertEqual(process.returncode, expected_code, process.stderr + process.stdout)
        try:
            report = json.loads(process.stdout)
        except json.JSONDecodeError as error:
            self.fail(f"stdout must contain only a JSON report: {error}\n{process.stdout!r}")
        self.assertEqual(report["tool"], "handlecheck")
        return report

    def test_matching_public_identity_requires_successful_negative_control(self):
        process = self.run_cli("alice", "--site", "github", "--json")
        report = self.report(process, 0)
        self.assertTrue(report["complete"])
        result = report["results"][0]
        self.assertEqual(result["status"], "found")
        self.assertEqual(result["control"], "passed")
        self.assertEqual(result["negative_control"]["status"], "not_found")
        self.assertTrue(result["evidence"])
        self.assertEqual(report["summary"]["found"], 1)
        self.assertEqual(len(self.requests()), 2)
        self.assertRegex(result["negative_control"]["username"], r"^hc[0-9a-f]{12}$")

    def test_endpoint_echoing_random_handles_is_withheld(self):
        report = self.report(self.run_cli("alice", "--site", "github", "--json",
                                         scenario="control_false_positive"), 2)
        result = report["results"][0]
        self.assertFalse(report["complete"])
        self.assertEqual(result["status"], "unknown")
        self.assertEqual(result["control"], "failed")
        self.assertEqual(result["negative_control"]["status"], "found")
        self.assertEqual(report["summary"]["found"], 0)

    def test_blocked_negative_control_cannot_confirm_match(self):
        report = self.report(self.run_cli("alice", "--site", "github", "--json",
                                         scenario="control_blocked"), 2)
        result = report["results"][0]
        self.assertEqual(result["status"], "unknown")
        self.assertEqual(result["negative_control"]["status"], "blocked")
        self.assertFalse(report["complete"])

    def test_missing_profile_id_remains_unresolved(self):
        report = self.report(self.run_cli("alice", "--site", "github", "--json",
                                         scenario="positive_missing_id"), 2)
        self.assertEqual(report["results"][0]["status"], "unknown")
        self.assertEqual(len(self.requests()), 1)

    def test_public_api_negative_does_not_need_a_control(self):
        report = self.report(self.run_cli("missingfixture", "--site", "github", "--json"), 0)
        self.assertEqual(report["results"][0]["status"], "not_found")
        self.assertEqual(report["summary"]["found"], 0)
        self.assertTrue(report["complete"])
        self.assertEqual(len(self.requests()), 1)

    def test_total_network_failure_is_incomplete(self):
        report = self.report(self.run_cli("alice", "--json", scenario="network_error"), 2)
        self.assertFalse(report["complete"])
        self.assertEqual(report["summary"]["found"], 0)
        self.assertEqual(report["summary"]["not_found"], 0)
        self.assertEqual(report["summary"]["error"], len(report["results"]))
        self.assertGreater(len(report["results"]), 1)

    def test_text_output_explains_incomplete_scan(self):
        process = self.run_cli("alice", "--site", "github", scenario="network_error")
        self.assertEqual(process.returncode, 2)
        self.assertIn("unresolved", process.stdout.lower())
        self.assertIn("incomplete", process.stdout.lower())

    def test_doctor_uses_known_positive_and_control(self):
        report = self.report(self.run_cli("--doctor", "--site", "github", "--json"), 0)
        self.assertEqual(report["mode"], "doctor")
        self.assertIsNone(report["username"])
        self.assertEqual(report["results"][0]["username"], "octocat")
        self.assertEqual(report["results"][0]["control"], "passed")
        self.assertIn("https://api.github.com/users/octocat", self.requests())

    def test_missing_doctor_fixture_is_unhealthy(self):
        report = self.report(self.run_cli("--doctor", "--site", "github", "--json",
                                         scenario="doctor_missing_positive"), 2)
        self.assertEqual(report["results"][0]["status"], "unknown")
        self.assertFalse(report["complete"])

    def test_help_list_and_version_do_not_initialize_http(self):
        for option in ("--help", "--list-sites", "--version"):
            with self.subTest(option=option):
                process = self.run_cli(option, scenario="network_error")
                self.assertEqual(process.returncode, 0, process.stderr)
                self.assertTrue(process.stdout)
                self.assertFalse(self.trace.exists(), "read-only information must not initialize HTTP")

    def test_invalid_timeout_values_fail_before_http(self):
        for value in ("nan", "inf", "-inf", "1e100", "-1", "0", "61", "nope", ""):
            with self.subTest(value=value):
                process = self.run_cli("alice", "--timeout", value)
                self.assertEqual(process.returncode, 1, process.stderr)
                self.assertFalse(self.trace.exists())

    def test_invalid_jobs_values_fail_before_http(self):
        for value in ("nan", "inf", "1e100", "-1", "0", "9", "1.5", "nope", ""):
            with self.subTest(value=value):
                process = self.run_cli("alice", "--jobs", value)
                self.assertEqual(process.returncode, 1, process.stderr)
                self.assertFalse(self.trace.exists())

    def test_unknown_site_fails_before_http(self):
        process = self.run_cli("alice", "--site", "unknown-site")
        self.assertEqual(process.returncode, 1)
        self.assertIn("Unknown site", process.stderr)
        self.assertFalse(self.trace.exists())

    def test_username_path_traversal_and_controls_are_rejected(self):
        for username in ("../alice", "alice/bob", "alice?admin=1", "alice bob", "alice\n", "." * 65):
            with self.subTest(username=username):
                output = self.root / "must-not-exist.json"
                process = self.run_cli(username, "--json", "--output", str(output))
                self.assertEqual(process.returncode, 1)
                self.assertFalse(output.exists())
                self.assertFalse(self.trace.exists())

    def test_site_invalid_username_makes_no_request(self):
        # Dot is accepted by the CLI for some providers, but not by GitHub.
        process = self.run_cli("alice.bob", "--site", "github", "--json")
        self.assertEqual(process.returncode, 1)
        self.assertFalse(self.trace.exists())
        self.assertEqual(self.requests(), [])

    def test_explicit_output_is_json_and_private_by_default(self):
        output = self.root / "report.json"
        report = self.report(self.run_cli("alice", "--site", "github", "--json",
                                         "--output", str(output)), 0)
        self.assertEqual(json.loads(output.read_text()), report)
        self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o600)

    def test_existing_output_is_never_overwritten(self):
        output = self.root / "existing.json"
        original = b"do not overwrite this\n"
        output.write_bytes(original)
        process = self.run_cli("alice", "--site", "github", "--output", str(output))
        self.assertEqual(process.returncode, 1)
        self.assertEqual(output.read_bytes(), original)
        self.assertIn("Cannot create report", process.stderr)

    def test_symlink_output_does_not_modify_target(self):
        target = self.root / "target.json"
        target.write_bytes(b"untouched target\n")
        output = self.root / "symlink.json"
        output.symlink_to(target)
        process = self.run_cli("alice", "--site", "github", "--output", str(output))
        self.assertEqual(process.returncode, 1)
        self.assertTrue(output.is_symlink())
        self.assertEqual(target.read_bytes(), b"untouched target\n")


if __name__ == "__main__":
    if not BINARY.is_file():
        sys.exit(f"Missing fixture executable: {BINARY}; run make test")
    unittest.main(verbosity=2)
