"""Registry and CLI extension contracts; no external service calls."""
import contextlib
import io
import json
import unittest
from unittest.mock import patch

from fieldkit_core import cli, integrations
from fieldkit_core.common import result
from fieldkit_core.manifest import validate_job


class IntegrationTests(unittest.TestCase):
    def test_optional_versions_are_checked_without_importing_the_packages(self):
        versions = {"mcp": "2.3.0", "beautifulsoup4": "4.15.0"}
        with patch.object(integrations, "package_version", side_effect=versions.get):
            catalog = integrations.catalog()
        providers = {entry["id"]: entry for entry in catalog["providers"]}
        self.assertTrue(providers["mcp"]["available"])
        self.assertTrue(providers["bellingcat_auto_archiver"]["available"])
        self.assertEqual(providers["bellingcat_auto_archiver"]["upstream_commit"],
                         "0507bf21793c2d3621a31295a7e166c8bfcec3e4")
        self.assertFalse(providers["bellingcat_auto_archiver"]["requires_telegram_login"])

    def test_absent_or_unsupported_dependencies_are_unavailable(self):
        for version in (None, "1.26.0", "2.2.9", "3.0.0", "2.4.0rc1", "malformed"):
            with self.subTest(version=version), patch.object(integrations, "package_version", return_value=version):
                self.assertFalse(integrations.catalog()["providers"][0]["available"])
        for version in (None, "4.11.9", "5.0.0"):
            with self.subTest(version=version), patch.object(integrations, "package_version", return_value=version):
                self.assertFalse(integrations.catalog()["providers"][1]["available"])

    def test_schema_uses_parser_query_limit_and_marks_local_exports(self):
        schema = cli.command_schema("telegram-export")
        self.assertEqual(schema["properties"]["options"]["properties"]["query"]["maxLength"], 200)
        descriptors = {item["name"]: item for item in cli.capabilities()["commands"]}
        self.assertTrue(descriptors["telegram-export"]["local_target"])
        self.assertFalse(descriptors["bellingcat-telegram"]["local_target"])

    def test_cli_routes_bounded_telegram_options(self):
        output = io.StringIO()
        with patch.object(cli, "dispatch", return_value=result({"fixture": True})) as dispatch, contextlib.redirect_stdout(output):
            code = cli.main(["telegram-messages", "telegram", "--limit", "3", "--before", "460", "--query", "news"])
        self.assertEqual(code, 0)
        self.assertEqual(dispatch.call_args.args[1:], ("telegram-messages", "telegram", {"limit": 3, "before": 460, "query": "news"}))
        self.assertEqual(json.loads(output.getvalue())["status"], "ok")

    def test_bellingcat_media_capture_is_explicit(self):
        for extra, expected in (([], False), (["--download-media", "--max-media", "2"], True)):
            with self.subTest(extra=extra), patch.object(cli, "dispatch", return_value=result({})) as dispatch, contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(cli.main(["bellingcat-telegram", "https://t.me/telegram/457", *extra]), 0)
            self.assertEqual(dispatch.call_args.args[3]["download_media"], expected)
        for options in ({"download_media": "yes"}, {"max_media": 0}, {"max_media": 11}, {"output_path": "/tmp/escape"}):
            with self.subTest(options=options):
                with self.assertRaises(Exception):
                    validate_job({"command": "bellingcat-telegram", "target": "https://t.me/telegram/457", "options": options})

    def test_mcp_frontend_forwards_without_printing_cli_json(self):
        output = io.StringIO()
        with patch("fieldkit_core.mcp_server.main", return_value=0) as serve, contextlib.redirect_stdout(output):
            self.assertEqual(cli.main(["mcp", "--workspace", "/configured/evidence"]), 0)
        serve.assert_called_once_with(["--workspace", "/configured/evidence"])
        self.assertEqual(output.getvalue(), "")

    def test_integrations_command_needs_no_target_or_network(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = cli.main(["integrations"])
        self.assertEqual(code, 0)
        self.assertEqual(len(json.loads(output.getvalue())["data"]["providers"]), 3)


if __name__ == "__main__":
    unittest.main()
