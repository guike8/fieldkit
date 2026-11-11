"""Workspace policy plus real official-SDK MCP clients; no provider requests."""
import asyncio
from copy import deepcopy
import importlib.util
import http.client
import json
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from fieldkit_core import mcp_server
from fieldkit_core.cli import Config, envelope
from fieldkit_core.common import Failure, Response, result
from fieldkit_core.manifest import COMMANDS


ROOT = Path(__file__).resolve().parents[1]
HAS_SDK = importlib.util.find_spec("mcp") is not None


def fixture_report(status="ok"):
    return envelope("person", "Example Name", result({"fixture": True}, status=status))


class BrokerTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.workspace = Path(self.directory.name).resolve()
        self.broker = mcp_server.ToolBroker(self.workspace)

    def test_every_manifest_command_has_normalized_tool_schema(self):
        for command, descriptor in COMMANDS.items():
            name = "fieldkit_" + command.replace("-", "_")
            self.assertIn(name, self.broker.schemas)
            schema = self.broker.schemas[name]
            self.assertFalse(schema["additionalProperties"])
            for key, specification in descriptor["options"].items():
                self.assertEqual(schema["properties"][key], specification)
            self.assertEqual("target" in schema.get("required", []), descriptor.get("target_required", True))

    def test_tool_schema_flattens_options_and_does_not_offer_paths_or_config(self):
        schema = self.broker.schemas["fieldkit_username"]
        self.assertIn("sites", schema["properties"])
        self.assertNotIn("options", schema["properties"])
        for key in ("case", "output", "timeout", "max_requests", "workspace"):
            self.assertNotIn(key, schema["properties"])

    def test_unknown_and_malformed_tool_calls_are_structured_failures(self):
        for name, arguments in (("missing", {}), ("fieldkit_person", []), ("fieldkit_person", {}),
                                ("fieldkit_person", {"target": "Example", "case": "/outside"}),
                                ("fieldkit_tls", {"target": "example.org", "port": True})):
            with self.subTest(name=name, arguments=arguments):
                report = self.broker.invoke(name, arguments)
                self.assertEqual(report["status"], "invalid")
                self.assertTrue(report["errors"])

    def test_option_arguments_become_a_valid_job(self):
        with patch.object(mcp_server, "execute", return_value=fixture_report()) as execute:
            self.broker.invoke("fieldkit_username", {"target": "alice", "sites": ["github"], "include_candidates": False})
        self.assertEqual(execute.call_args.args[0], {
            "command": "username", "target": "alice", "options": {"sites": ["github"], "include_candidates": False}})

    def test_workspace_file_is_allowed_and_normalized(self):
        path = self.workspace / "notes.txt"
        path.write_text("example.org\n")
        with patch.object(mcp_server, "execute", return_value=fixture_report()) as execute:
            self.broker.invoke("fieldkit_extract", {"target": "notes.txt"})
        self.assertEqual(execute.call_args.args[0]["target"], str(path))

    def test_traversal_absolute_outside_missing_files_and_symlinks_rejected(self):
        inside = self.workspace / "notes.txt"
        inside.write_text("test")
        link = self.workspace / "link.txt"
        link.symlink_to(inside)
        for target in ("../outside.txt", str(self.workspace.parent / "outside.txt"), "missing.txt", str(link)):
            with self.subTest(target=target), patch.object(mcp_server, "execute") as execute:
                report = self.broker.invoke("fieldkit_file", {"target": target})
                self.assertEqual(report["status"], "invalid")
                execute.assert_not_called()

    def test_parent_directory_symlink_is_rejected_even_if_it_points_inside(self):
        folder = self.workspace / "original"
        folder.mkdir()
        (folder / "notes.txt").write_text("test")
        (self.workspace / "alias").symlink_to(folder, target_is_directory=True)
        self.assertEqual(self.broker.invoke("fieldkit_file", {"target": "alias/notes.txt"})["status"], "invalid")

    def test_dynamic_local_target_descriptor_is_scoped(self):
        descriptor = {"description": "Local test export", "local_target": True, "options": {}}
        path = self.workspace / "export.json"
        path.write_text("{}")
        with patch.dict(mcp_server.COMMANDS, {"test-export": descriptor}):
            broker = mcp_server.ToolBroker(self.workspace)
        with patch.object(mcp_server, "execute", return_value=fixture_report()) as execute:
            broker.invoke("fieldkit_test_export", {"target": "export.json"})
        self.assertEqual(execute.call_args.args[0], {"command": "test-export", "target": str(path), "options": {}})
        self.assertEqual(broker.invoke("fieldkit_test_export", {"target": "../outside.json"})["status"], "invalid")

    def test_command_name_collision_is_rejected(self):
        descriptor = {"description": "Fixture", "options": {}}
        with patch.dict(mcp_server.COMMANDS, {"fixture-call": descriptor, "fixture_call": descriptor}):
            with self.assertRaises(Failure):
                mcp_server.ToolBroker(self.workspace)

    def test_fixed_case_is_confined_and_rechecked(self):
        with self.assertRaises(Failure):
            mcp_server.ToolBroker(self.workspace, case=self.workspace.parent / "case")
        broker = mcp_server.ToolBroker(self.workspace, case="cases/demo")
        with patch.object(mcp_server, "execute", return_value=fixture_report()) as execute:
            broker.invoke("fieldkit_person", {"target": "Example"})
        self.assertEqual(execute.call_args.args[1].case, str(self.workspace / "cases/demo"))

    def test_case_tools_are_only_advertised_when_configured(self):
        self.assertNotIn("fieldkit_case_show", self.broker.schemas)
        broker = mcp_server.ToolBroker(self.workspace, case="cases/demo")
        self.assertIn("fieldkit_case_verify", broker.schemas)
        report = broker.invoke("fieldkit_person", {"target": "Example Name"})
        self.assertEqual(report["status"], "ok")
        self.assertTrue(broker.invoke("fieldkit_case_verify", {})["data"]["ok"])
        self.assertEqual(broker.invoke("fieldkit_case_show", {})["data"]["summary"]["run_count"], 1)

    def test_annotations_describe_case_writes_without_destructive_claims(self):
        normal = {item["name"]: item for item in self.broker.tool_descriptions()}
        recorded = {item["name"]: item for item in mcp_server.ToolBroker(self.workspace, case="case").tool_descriptions()}
        self.assertTrue(normal["fieldkit_person"]["annotations"]["read_only_hint"])
        self.assertFalse(recorded["fieldkit_person"]["annotations"]["read_only_hint"])
        self.assertTrue(recorded["fieldkit_case_show"]["annotations"]["read_only_hint"])
        self.assertTrue(all(not item["annotations"]["destructive_hint"] for item in recorded.values()))

    def test_capabilities_are_local_and_do_not_expose_values_of_keys(self):
        with patch.dict("os.environ", {"BRAVE_SEARCH_API_KEY": "PRIVATE_TEST_VALUE"}):
            report = self.broker.invoke("fieldkit_capabilities", {})
        self.assertEqual(report["status"], "ok")
        self.assertNotIn("PRIVATE_TEST_VALUE", json.dumps(report))
        self.assertTrue(report["data"]["mcp"]["workspace_scoped_files"])

    def test_http_binding_rejects_public_hosts_and_invalid_ports(self):
        for options in ({"host": "0.0.0.0"}, {"host": "example.org"}, {"host": "::"}, {"port": 0}, {"port": True}):
            with self.subTest(options=options), patch.object(mcp_server, "build_server") as build:
                with self.assertRaises(Failure):
                    mcp_server.serve(self.workspace, transport="streamable-http", **options)
                build.assert_not_called()


@unittest.skipUnless(HAS_SDK, "Install the mcp extra for actual protocol tests")
class SDKTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.workspace = Path(self.directory.name).resolve()

    def test_sdk_unsupported_major_or_old_minor_is_explicit(self):
        for version in ("1.99.0", "2.2.9", "3.0.0", "unknown"):
            with self.subTest(version=version), patch.object(mcp_server.metadata, "version", return_value=version):
                with self.assertRaises(Failure) as caught:
                    mcp_server.build_server(self.workspace)
                self.assertEqual(caught.exception.code, "MCP_SDK_VERSION")

    async def test_actual_sdk_client_lists_schemas_calls_tools_and_reads_resource(self):
        from mcp import Client
        server = mcp_server.build_server(self.workspace)
        async with Client(server) as client:
            tools = (await client.list_tools()).tools
            self.assertIn("fieldkit_username", {tool.name for tool in tools})
            username = next(tool for tool in tools if tool.name == "fieldkit_username")
            self.assertIn("sites", username.input_schema["properties"])
            response = await client.call_tool("fieldkit_person", {"target": "Example Name"})
            self.assertFalse(response.is_error)
            self.assertEqual(response.structured_content["status"], "ok")
            self.assertEqual(json.loads(response.content[0].text), response.structured_content)
            resources = (await client.list_resources()).resources
            self.assertEqual(str(resources[0].uri), "fieldkit://capabilities")
            resource = await client.read_resource("fieldkit://capabilities")
            self.assertEqual(json.loads(resource.contents[0].text)["status"], "ok")

    async def test_actual_sdk_call_error_preserves_structured_envelope(self):
        from mcp import Client
        async with Client(mcp_server.build_server(self.workspace)) as client:
            response = await client.call_tool("fieldkit_file", {"target": "../private.txt"})
            self.assertTrue(response.is_error)
            self.assertEqual(response.structured_content["status"], "invalid")
            invalid = await client.call_tool("fieldkit_person", {"target": "Example", "unknown_option": True})
            self.assertTrue(invalid.is_error)

    async def test_inprocess_calls_respect_worker_concurrency(self):
        from mcp import Client
        active, peak = 0, 0
        lock = threading.Lock()
        def slow_execute(job, config):
            nonlocal active, peak
            with lock:
                active += 1
                peak = max(peak, active)
            time.sleep(0.04)
            with lock:
                active -= 1
            return fixture_report()
        with patch.object(mcp_server, "execute", side_effect=slow_execute):
            async with Client(mcp_server.build_server(self.workspace, concurrency=2)) as client:
                results = await asyncio.gather(*[client.call_tool("fieldkit_person", {"target": f"Example {i}"}) for i in range(6)])
        self.assertEqual(peak, 2)
        self.assertTrue(all(not response.is_error for response in results))

    @unittest.skipUnless(importlib.util.find_spec("bs4") is not None, "Install the telegram extra for Bellingcat's adapted parser")
    async def test_bellingcat_tool_call_runs_adapted_upstream_extractor(self):
        from mcp import Client
        from fieldkit_core.transport import Context
        markup = b'''<html><body><div class="tgme_widget_message" data-post="telegram/1">
          <a class="tgme_widget_message_photo_wrap" style="background-image:url('https://cdn4.telegram.org/file/fixture.jpg')"></a>
          <div class="tgme_widget_message_text">Public fixture <a href="https://example.org/">source</a></div>
          <time datetime="2026-11-11T00:00:00Z">00:12</time></div></body></html>'''
        requests = []
        def public_fixture(context, url, headers=None, follow_redirects=True):
            requests.append(url)
            context.reserve_requests()
            source = context.add_inline_source(url, markup, kind="http", metadata={"url": url, "http_status": 200})
            return Response(url, 200, {"content-type": "text/html; charset=utf-8"}, markup, source)
        with patch.object(Context, "fetch", public_fixture):
            async with Client(mcp_server.build_server(self.workspace)) as client:
                self.assertIn("fieldkit_bellingcat_telegram", {tool.name for tool in (await client.list_tools()).tools})
                response = await client.call_tool("fieldkit_bellingcat_telegram", {"target": "https://t.me/telegram/1"})
        self.assertFalse(response.is_error, response.structured_content)
        report = response.structured_content
        extraction = report["data"]["extraction"]
        self.assertTrue(extraction["extractor_invoked"])
        self.assertTrue(extraction["extractor_returned_media"])
        self.assertEqual(extraction["upstream"]["integration"], "adapted_pinned_extractor")
        self.assertEqual(extraction["media"][0]["url"], "https://cdn4.telegram.org/file/fixture.jpg")
        self.assertFalse(extraction["media"][0]["captured"])
        self.assertTrue(report["sources"][0]["sha256"])
        self.assertEqual(requests, ["https://t.me/telegram/1?embed=1"])

    async def test_telegram_export_tool_is_local_and_workspace_scoped(self):
        from mcp import Client
        from fieldkit_core.transport import Context
        (self.workspace / "export.json").write_text(json.dumps({"name": "Fixture", "messages": [
            {"id": 1, "type": "message", "text": "Local evidence fixture", "date": "2026-11-11T00:00:00"}]}))
        with patch.object(Context, "fetch", side_effect=AssertionError("Local exports must not use HTTP")):
            async with Client(mcp_server.build_server(self.workspace)) as client:
                response = await client.call_tool("fieldkit_telegram_export", {"target": "export.json"})
                outside = await client.call_tool("fieldkit_telegram_export", {"target": "../outside.json"})
        self.assertFalse(response.is_error, response.structured_content)
        self.assertEqual(response.structured_content["data"]["messages"][0]["text"], "Local evidence fixture")
        self.assertTrue(outside.is_error)

    async def test_actual_stdio_subprocess_modern_and_legacy_clients(self):
        from mcp import Client, StdioServerParameters
        for mode in ("auto", "legacy"):
            with self.subTest(mode=mode):
                params = StdioServerParameters(command=sys.executable, args=["-m", "fieldkit_core.mcp_server",
                    "--workspace", str(self.workspace)], cwd=ROOT)
                async with Client(params, mode=mode, read_timeout_seconds=10) as client:
                    response = await client.call_tool("fieldkit_person", {"target": "Example Name"})
                    self.assertEqual(response.structured_content["data"]["name"], "Example Name")

    async def test_actual_streamable_http_loopback(self):
        from mcp import Client
        with socket.socket() as listener:
            try:
                listener.bind(("127.0.0.1", 0))
            except PermissionError:
                self.skipTest("Execution sandbox disallows loopback sockets")
            port = listener.getsockname()[1]
        child = subprocess.Popen([sys.executable, "-m", "fieldkit_core.mcp_server", "--workspace", str(self.workspace),
                                  "--transport", "streamable-http", "--port", str(port)], cwd=ROOT,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            ready = False
            for _ in range(100):
                if child.poll() is not None:
                    self.fail(child.stderr.read().decode("utf-8", "replace"))
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=0.05):
                        ready = True
                    break
                except OSError:
                    await asyncio.sleep(0.03)
            self.assertTrue(ready)
            # A remote web origin or DNS-rebinding Host cannot use the local server.
            for headers in ({"Host": "attacker.example"}, {"Origin": "https://attacker.example"}):
                connection = http.client.HTTPConnection("127.0.0.1", port, timeout=3)
                try:
                    connection.request("POST", "/mcp", body=b"{}", headers={"Content-Type": "application/json", **headers})
                    rejected = connection.getresponse()
                    self.assertIn(rejected.status, {400, 403, 421})
                    rejected.read()
                finally:
                    connection.close()
            async with Client(f"http://127.0.0.1:{port}/mcp", read_timeout_seconds=10) as client:
                response = await client.call_tool("fieldkit_capabilities", {})
                self.assertEqual(response.structured_content["status"], "ok")
        finally:
            child.terminate()
            try:
                child.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.communicate()


if __name__ == "__main__":
    unittest.main()
