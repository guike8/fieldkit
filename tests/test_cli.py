"""Offline CLI contract tests: stable JSON, strict jobs, batching and outputs."""
from contextlib import redirect_stderr, redirect_stdout
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import tempfile
import time
import unittest
from unittest.mock import patch

from fieldkit_core import cli
from fieldkit_core.common import Failure, result


def invoke(arguments, input_text="", *, parse=True):
    output, error = io.StringIO(), io.StringIO()
    stdin = io.TextIOWrapper(io.BytesIO(input_text.encode("utf-8")), encoding="utf-8")
    with redirect_stdout(output), redirect_stderr(error), patch.object(cli.sys, "stdin", stdin):
        code = cli.main(arguments)
    text = output.getvalue()
    return code, json.loads(text) if parse else text, error.getvalue()


def echo_dispatch(ctx, command, target, options):
    return result({"command": command, "target": target, "options": options,
                   "timeout": ctx.timeout, "max_results": ctx.max_results,
                   "max_requests": ctx.max_requests, "max_bytes": ctx.max_bytes})


class DiscoveryTests(unittest.TestCase):
    def test_capabilities_default_is_json_and_key_value_never_printed(self):
        with patch("fieldkit_core.people.catalog", return_value={"verified_adapters": [], "candidate_only_platforms": []}), \
             patch.dict(os.environ, {"BRAVE_SEARCH_API_KEY": "secret-must-not-print"}):
            code, report, error = invoke(["capabilities"])
        self.assertEqual(code, 0)
        self.assertEqual(report["schema_version"], "1.0")
        self.assertEqual(report["data"]["default_output"], "json")
        self.assertTrue(report["data"]["optional_providers"]["brave_search"]["configured"])
        self.assertNotIn("secret-must-not-print", json.dumps(report) + error)
        self.assertIn("username", [item["name"] for item in report["data"]["commands"]])

    def test_command_schema_has_strict_properties(self):
        code, report, _ = invoke(["schema", "username"])
        self.assertEqual(code, 0)
        schema = report["data"]
        self.assertFalse(schema["additionalProperties"])
        self.assertFalse(schema["properties"]["options"]["additionalProperties"])
        self.assertEqual(schema["properties"]["command"]["const"], "username")

    def test_early_argument_errors_are_json_without_traceback_or_ansi(self):
        for arguments in ([], ["no-such-command"], ["username"], ["dns", "example.com", "--unknown"]):
            with self.subTest(arguments=arguments):
                code, report, error = invoke(arguments)
                self.assertEqual(code, 1)
                self.assertEqual(report["status"], "invalid")
                self.assertEqual(report["errors"][0]["code"], "ARGUMENT_ERROR")
                self.assertNotIn("Traceback", error)
                self.assertNotIn("\x1b", json.dumps(report) + error)

    def test_doctor_has_no_network_and_only_checks_backend_version(self):
        from types import SimpleNamespace
        completed = SimpleNamespace(returncode=0, stdout=b"handlecheck 0.1.0\n", stderr=b"")
        with patch.object(cli.subprocess, "run", return_value=completed) as backend:
            code, report, _ = invoke(["doctor"])
        self.assertEqual(code, 0)
        self.assertFalse(report["data"]["network_checks_run"])
        self.assertEqual(backend.call_args.args[0][-1], "--version")

    def test_doctor_missing_backend_is_partial(self):
        with patch.object(cli.subprocess, "run", side_effect=OSError("wrong architecture")):
            code, report, _ = invoke(["doctor"])
        self.assertEqual(code, 2)
        self.assertEqual(report["status"], "partial")
        self.assertIn("remedy", report["data"]["native_backend"])


class OptionTests(unittest.TestCase):
    def test_global_flags_before_and_after_command_are_equivalent(self):
        before = ["--timeout", "4", "--max-results", "7", "--max-requests", "8", "--max-bytes", "4096", "dns", "example.com"]
        after = ["dns", "example.com", "--timeout", "4", "--max-results", "7", "--max-requests", "8", "--max-bytes", "4096"]
        with patch.object(cli, "dispatch", side_effect=echo_dispatch):
            first, second = invoke(before), invoke(after)
        self.assertEqual(first[0], 0)
        self.assertEqual(first[1]["data"], second[1]["data"])
        self.assertEqual(first[1]["data"]["timeout"], 4)

    def test_site_and_type_commas_are_flattened(self):
        with patch.object(cli, "dispatch", side_effect=echo_dispatch):
            _, handles, _ = invoke(["username", "alice", "--site", "github,gitlab", "--site", "reddit"])
            _, dns, _ = invoke(["dns", "example.com", "--type", "a,AAAA", "--type", "mx"])
        self.assertEqual(handles["data"]["options"]["sites"], ["github", "gitlab", "reddit"])
        self.assertEqual(dns["data"]["options"]["types"], ["A", "AAAA", "MX"])

    def test_empty_comma_item_is_invalid_without_dispatch(self):
        with patch.object(cli, "dispatch") as dispatch:
            code, report, _ = invoke(["username", "alice", "--site", "github,,reddit"])
        self.assertEqual(code, 1)
        self.assertEqual(report["status"], "invalid")
        dispatch.assert_not_called()

    def test_invalid_global_bounds_do_not_dispatch(self):
        bad = ("nan", "inf", "-1", "0", "61")
        with patch.object(cli, "dispatch") as dispatch:
            for value in bad:
                code, report, _ = invoke(["dns", "example.com", "--timeout", value])
                self.assertEqual(code, 1)
                self.assertEqual(report["status"], "invalid")
            for flag, value in (("--max-results", "0"), ("--max-requests", "129"), ("--max-bytes", "1")):
                self.assertEqual(invoke(["dns", "example.com", flag, value])[0], 1)
        dispatch.assert_not_called()

    def test_pretty_does_not_change_protocol_and_default_has_no_ansi(self):
        with patch.object(cli, "dispatch", side_effect=echo_dispatch):
            code, text, _ = invoke(["--pretty", "dns", "example.com"], parse=False)
        self.assertEqual(code, 0)
        self.assertIn("\n  ", text)
        self.assertNotIn("\x1b", text)
        self.assertEqual(json.loads(text)["status"], "ok")


class JobAndBatchTests(unittest.TestCase):
    def test_run_stdin_strict_job_dispatch(self):
        job = {"command": "dns", "target": "example.com", "options": {"types": ["A"]}}
        with patch.object(cli, "dispatch", side_effect=echo_dispatch):
            code, report, _ = invoke(["run", "-"], json.dumps(job))
        self.assertEqual(code, 0)
        self.assertEqual(report["command"], "dns")
        self.assertEqual(report["data"]["options"], {"types": ["A"]})

    def test_unknown_fields_and_bad_types_are_invalid_not_internal(self):
        jobs = [
            {"command": "dns", "target": "example.com", "extra": True},
            {"command": "dns", "target": "example.com", "options": {"shell": "unsafe"}},
            {"command": "dns", "target": "example.com", "options": {"types": "A"}},
            {"command": "tls", "target": "example.com", "options": {"port": True}},
            {"command": "tls", "target": "example.com", "options": {"port": 10 ** 1000}},
            {"command": [], "target": "example.com"},
            {"command": {}, "target": "example.com"},
            {"command": True, "target": "example.com"},
            {"command": "dns", "target": ["example.com"]},
            ["dns", "example.com"],
        ]
        with patch.object(cli, "dispatch") as dispatch:
            for job in jobs:
                with self.subTest(job=str(job)[:100]):
                    code, report, _ = invoke(["run", "-"], json.dumps(job))
                    self.assertEqual(code, 1)
                    self.assertEqual(report["status"], "invalid")
                    self.assertEqual(report["errors"][0]["code"], "INVALID_JOB")
                    self.assertIsInstance(report["command"], str)
                    self.assertTrue(report["target"] is None or isinstance(report["target"], str))
        dispatch.assert_not_called()

    def test_malformed_duplicate_nonfinite_and_deep_job_json_invalid(self):
        malformed = ("{", '{"command":"dns","command":"tls","target":"example.com"}',
                     '{"command":"tls","target":"example.com","options":{"port":NaN}}',
                     "[" * 500 + "]" * 500)
        with patch.object(cli, "dispatch") as dispatch:
            for document in malformed:
                code, report, _ = invoke(["run", "-"], document)
                self.assertEqual(code, 1)
                self.assertEqual(report["errors"][0]["code"], "INVALID_JOB_JSON")
        dispatch.assert_not_called()

    def test_stdin_input_limit_and_exact_boundary(self):
        job = '{"command":"dns","target":"example.com"}'
        with patch.object(cli, "dispatch", side_effect=echo_dispatch) as dispatch:
            code, report, _ = invoke(["run", "-"], " " * (1048576 - len(job)) + job)
            self.assertEqual(code, 0)
            dispatch.reset_mock()
            code, report, _ = invoke(["run", "-"], " " * 1048577)
            self.assertEqual(code, 1)
            self.assertEqual(report["errors"][0]["code"], "INPUT_LIMIT")
            dispatch.assert_not_called()

    def test_job_file_input_and_oversized_file(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(cli, "dispatch", side_effect=echo_dispatch):
            path = Path(directory) / "job.json"
            path.write_text('{"command":"dns","target":"example.com"}')
            self.assertEqual(invoke(["run", str(path)])[0], 0)
            path.write_bytes(b" " * 1048577)
            code, report, _ = invoke(["run", str(path)])
            self.assertEqual(code, 1)
            self.assertEqual(report["errors"][0]["code"], "INPUT_LIMIT")

    def test_batch_preserves_input_order_despite_concurrent_completion(self):
        jobs = [{"command": "dns", "target": "slow.example"}, {"command": "dns", "target": "fast.example"}]
        def handler(ctx, command, target, options):
            if target.startswith("slow"):
                time.sleep(0.02)
            return result({"target": target})
        with patch.object(cli, "dispatch", side_effect=handler):
            code, report, _ = invoke(["batch", "-", "--jobs", "2"], "\n".join(map(json.dumps, jobs)))
        self.assertEqual(code, 0)
        self.assertEqual([item["target"] for item in report["data"]["results"]], ["slow.example", "fast.example"])

    def test_batch_invalid_line_and_failure_are_partial_with_details(self):
        jobs = '{"command":"dns","target":"example.com"}\nnot-json\n{"command":[],"target":"x"}\n'
        with patch.object(cli, "dispatch", side_effect=echo_dispatch):
            code, report, _ = invoke(["batch", "-"], jobs)
        self.assertEqual(code, 2)
        self.assertEqual(report["status"], "partial")
        self.assertEqual(report["data"]["total"], 3)
        self.assertEqual(report["data"]["complete_count"], 1)
        self.assertEqual(report["data"]["unresolved_count"], 2)
        self.assertTrue(all(item["status"] == "invalid" for item in report["data"]["results"][1:]))

    def test_batch_limits_and_job_count_bounds(self):
        for source in ("", "\n".join('{"command":"doctor"}' for _ in range(101))):
            code, report, _ = invoke(["batch", "-"], source)
            self.assertEqual(code, 1)
            self.assertEqual(report["errors"][0]["code"], "BATCH_LIMIT")
        self.assertEqual(invoke(["batch", "-", "--jobs", "0"], "{}")[0], 1)

    def test_module_protocol_missing_fields_is_typed_error(self):
        with patch.object(cli, "dispatch", return_value={"status": "ok"}):
            code, report, _ = invoke(["dns", "example.com"])
        self.assertEqual(code, 2)
        self.assertEqual(report["errors"][0]["code"], "MODULE_PROTOCOL")
        self.assertIn("data", report)

    def test_typed_module_failure_has_no_traceback_and_correct_exit(self):
        with patch.object(cli, "dispatch", side_effect=Failure("RATE_LIMITED", "quota reached", "rate_limited", retryable=True)):
            code, report, error = invoke(["dns", "example.com"])
        self.assertEqual(code, 2)
        self.assertEqual(report["status"], "rate_limited")
        self.assertTrue(report["errors"][0]["retryable"])
        self.assertNotIn("Traceback", error)


class OutputAndCaseTests(unittest.TestCase):
    def test_new_output_private_and_existing_file_or_symlink_refused(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(cli, "dispatch", side_effect=echo_dispatch):
            output = Path(directory) / "report.json"
            code, report, _ = invoke(["dns", "example.com", "--output", str(output)])
            self.assertEqual(code, 0)
            self.assertEqual(json.loads(output.read_text())["run_id"], report["run_id"])
            self.assertEqual(stat.S_IMODE(output.stat().st_mode) & 0o077, 0)
            original = output.read_bytes()
            code, report, _ = invoke(["dns", "example.com", "--output", str(output)])
            self.assertEqual(code, 1)
            self.assertEqual(report["errors"][0]["code"], "OUTPUT_ERROR")
            self.assertEqual(output.read_bytes(), original)
            link = Path(directory) / "symlink.json"
            link.symlink_to(output)
            self.assertEqual(invoke(["dns", "example.com", "--output", str(link)])[0], 1)
            self.assertEqual(output.read_bytes(), original)

    @unittest.skipUnless(importlib.util.find_spec("fieldkit_core.case"), "Case module not available yet")
    def test_case_save_show_verify_and_export_file(self):
        def captured(ctx, command, target, options):
            source_id = ctx.add_inline_source("fixture source", b"fixture evidence", kind="fixture")
            return result({"fixture": True, "source_id": source_id})
        with tempfile.TemporaryDirectory() as directory:
            case = str(Path(directory) / "case")
            self.assertEqual(invoke(["case", "init", case])[0], 0)
            with patch.object(cli, "dispatch", side_effect=captured):
                code, report, _ = invoke(["dns", "example.com", "--case", case])
            self.assertEqual(code, 0)
            code, shown, _ = invoke(["case", "show", case])
            self.assertEqual(code, 0)
            self.assertIn(report["run_id"], json.dumps(shown))
            self.assertEqual(invoke(["case", "verify", case])[0], 0)
            export = Path(directory) / "report.md"
            code, exported, _ = invoke(["case", "export", case, "--format", "markdown", "--output", str(export)])
            self.assertEqual(code, 0)
            self.assertTrue(exported["data"]["saved"])
            self.assertTrue(export.read_text().strip())


if __name__ == "__main__":
    unittest.main()
