import copy
import importlib.util
import json
import os
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "fieldkit_agent", Path(__file__).with_name("fieldkit_agent.py")
)
agent = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(agent)


def reply(content=None, calls=None, *, finish="stop", tokens=10):
    return {
        "choices": [
            {
                "message": {
                    "role": "assistant",
                    "content": content,
                    "tool_calls": calls or [],
                },
                "finish_reason": finish,
            }
        ],
        "usage": {"prompt_tokens": tokens, "completion_tokens": 2},
    }


def call(name="fieldkit_graph_show", arguments="{}", call_id="c1"):
    return {
        "id": call_id,
        "type": "function",
        "function": {"name": name, "arguments": arguments},
    }


TOOLS = [
    {
        "name": "fieldkit_graph_show",
        "description": "Inspect graph",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": False,
        },
    }
]


class Scripted:
    def __init__(self, *responses):
        self.responses = list(responses)
        self.requests = []

    def complete(self, messages, tools):
        self.requests.append(copy.deepcopy((messages, tools)))
        return self.responses.pop(0)


class AgentTests(unittest.TestCase):
    def test_tool_round_trip_and_metrics(self):
        client = Scripted(
            reply(calls=[call()], finish="tool_calls"),
            reply("No identity is established."),
        )
        observed = []
        result = agent.run_agent(
            client,
            TOOLS,
            lambda name, args: observed.append((name, args)) or {"isError": False},
            "Map evidence",
        )
        self.assertEqual(result["stop_reason"], "completed")
        self.assertEqual(observed, [("fieldkit_graph_show", {})])
        self.assertEqual(result["metrics"]["tool_calls"], 1)
        self.assertEqual(result["metrics"]["prompt_tokens"], 20)
        message = client.requests[1][0][-1]
        self.assertEqual(message["role"], "tool")
        self.assertEqual(message["tool_call_id"], "c1")

    def test_legacy_write_review_and_collector_tools_not_advertised(self):
        tools = TOOLS + [
            {"name": name, "inputSchema": {}}
            for name in (
                "fieldkit_note_add",
                "fieldkit_claim_review",
                "fieldkit_http",
                "shell",
            )
        ]
        client = Scripted(reply("Insufficient evidence."))
        agent.run_agent(
            client, tools, lambda *_: self.fail("unexpected tool"), "Question"
        )
        self.assertEqual(
            [t["function"]["name"] for t in client.requests[0][1]],
            ["fieldkit_graph_show"],
        )

    def test_whole_batch_validated_before_effects(self):
        for invalid in [
            call("fieldkit_note_add", call_id="c2"),
            call(arguments="[]", call_id="c2"),
            call(arguments="{bad", call_id="c2"),
            call(arguments='{"v":NaN}', call_id="c2"),
            call(arguments="{}", call_id="c1"),
        ]:
            with self.subTest(invalid=invalid):
                client = Scripted(reply(calls=[call(), invalid], finish="tool_calls"))
                with self.assertRaises(agent.AgentError):
                    agent.run_agent(
                        client,
                        TOOLS,
                        lambda *_: self.fail("must have no side effects"),
                        "Question",
                    )

    def test_budget_stops_without_partial_batch(self):
        client = Scripted(
            reply(calls=[call(), call(call_id="c2")], finish="tool_calls")
        )
        result = agent.run_agent(
            client,
            TOOLS,
            lambda *_: self.fail("over budget"),
            "Question",
            max_tool_calls=1,
        )
        self.assertEqual(result["stop_reason"], "tool_limit")
        self.assertEqual(result["trace"], [])
        client = Scripted(reply(calls=[call()], tokens=100))
        result = agent.run_agent(
            client,
            TOOLS,
            lambda *_: self.fail("over budget"),
            "Question",
            max_total_tokens=10,
        )
        self.assertEqual(result["stop_reason"], "token_limit")

    def test_turn_limit_and_failed_tool_observation(self):
        client = Scripted(reply(calls=[call()]))

        def fail(*_):
            raise RuntimeError("private secret not to publish")

        result = agent.run_agent(client, TOOLS, fail, "Question", max_turns=1)
        self.assertEqual(result["stop_reason"], "turn_limit")
        self.assertTrue(result["trace"][0]["result"]["isError"])
        self.assertNotIn("private secret", json.dumps(result))

    def test_transport_failure_is_not_retried(self):
        client = Scripted(reply(calls=[call()]))

        def fail(*_):
            raise agent.AgentError("uncertain action")

        with self.assertRaisesRegex(agent.AgentError, "uncertain"):
            agent.run_agent(client, TOOLS, fail, "Question")
        self.assertEqual(len(client.requests), 1)

    def test_truncated_or_missing_usage_stops_before_tools(self):
        for response in [
            reply(calls=[call()], finish="length"),
            {"choices": reply("answer")["choices"]},
            reply(""),
        ]:
            with self.subTest(response=response), self.assertRaises(agent.AgentError):
                agent.run_agent(
                    Scripted(response),
                    TOOLS,
                    lambda *_: self.fail("must stop"),
                    "Question",
                )

    def test_output_private_and_exclusive(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "report.json"
            agent.save_report(path, {"ok": True})
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            with self.assertRaises(FileExistsError):
                agent.save_report(path, {"ok": False})
            self.assertEqual(json.loads(path.read_text()), {"ok": True})


class HTTPTests(unittest.TestCase):
    def setUp(self):
        self.requests = []
        self.status = 200
        self.body = json.dumps(reply("Synthetic answer")).encode()
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                owner.requests.append(
                    (
                        self.path,
                        dict(self.headers),
                        json.loads(
                            self.rfile.read(int(self.headers["Content-Length"]))
                        ),
                    )
                )
                self.send_response(owner.status)
                if owner.status == 307:
                    self.send_header(
                        "Location",
                        f"http://127.0.0.1:{owner.server.server_port}/unexpected",
                    )
                self.end_headers()
                self.wfile.write(owner.body)

            def log_message(self, *_):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.base_url = f"http://127.0.0.1:{self.server.server_port}/v1"

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def test_openai_request_contract(self):
        client = agent.ChatClient(self.base_url, agent.MODEL, "synthetic-test-key")
        response = client.complete(
            [{"role": "user", "content": "Synthetic"}], [{"type": "function"}]
        )
        path, headers, payload = self.requests[0]
        self.assertEqual(path, "/v1/chat/completions")
        self.assertEqual(headers["Authorization"], "Bearer synthetic-test-key")
        self.assertEqual(payload["model"], agent.MODEL)
        self.assertFalse(payload["parallel_tool_calls"])
        self.assertFalse(payload["stream"])
        self.assertEqual(
            response["choices"][0]["message"]["content"], "Synthetic answer"
        )

    def test_http_errors_never_echo_secrets_or_retry(self):
        self.status = 429
        self.body = b"provider-body-with-private-secret"
        with self.assertRaisesRegex(agent.AgentError, "HTTP 429") as caught:
            agent.ChatClient(self.base_url, agent.MODEL, "secret").complete([], [])
        self.assertNotIn("private-secret", str(caught.exception))
        self.assertEqual(len(self.requests), 1)

    def test_redirect_does_not_forward_credentials(self):
        self.status = 307
        with self.assertRaisesRegex(agent.AgentError, "HTTP 307"):
            agent.ChatClient(self.base_url, agent.MODEL, "secret").complete([], [])
        self.assertEqual(len(self.requests), 1)

    def test_base_url_validation(self):
        for url in [
            "http://public.example/v1",
            "https://user:pass@example.org/v1",
            "https://example.org/v1?key=secret",
            "https://example.org/#fragment",
            "file:///tmp/key",
        ]:
            with self.subTest(url=url), self.assertRaises(agent.AgentError):
                agent.ChatClient(url, agent.MODEL, "secret")


class FakeMCP:
    def __init__(self):
        self.tools = [{"name": name} for name in agent.ALLOWED_TOOLS]
        self.calls = []

    def call(self, name, arguments):
        self.calls.append((name, arguments))
        return {"isError": False, "structuredContent": {"data": {"id": "inv_scoped"}}}


class ScopeTests(unittest.TestCase):
    def setUp(self):
        self.mcp = FakeMCP()
        self.scoped = agent.ScopedTools(
            self.mcp, "Question", "person", ["synthetic_handle"]
        )
        self.create = {
            "question": "Question",
            "mode": "person",
            "seeds": ["synthetic_handle"],
        }

    def test_collection_disabled_by_default(self):
        self.assertNotIn(
            "fieldkit_investigation_run_step", [t["name"] for t in self.scoped.tools]
        )
        self.assertTrue(
            self.scoped.call("fieldkit_investigation_run_step", {})["isError"]
        )
        self.assertEqual(self.mcp.calls, [])

    def test_creation_scope_cannot_be_changed(self):
        for key, value in [
            ("question", "Changed"),
            ("mode", "organization"),
            ("seeds", ["other"]),
        ]:
            self.assertTrue(
                self.scoped.call(
                    "fieldkit_investigation_create", {**self.create, key: value}
                )["isError"]
            )
        self.assertEqual(self.mcp.calls, [])
        self.assertFalse(
            self.scoped.call("fieldkit_investigation_create", self.create)["isError"]
        )
        self.assertEqual(self.scoped.investigation_id, "inv_scoped")
        self.assertTrue(
            self.scoped.call("fieldkit_investigation_create", self.create)["isError"]
        )
        self.assertEqual(len(self.mcp.calls), 1)

    def test_source_reader_is_enabled_without_collection_but_investigation_scoped(self):
        name = "fieldkit_investigation_source"
        self.assertIn(name, {t["name"] for t in self.scoped.tools})
        arguments = {"id": "inv_scoped", "step_id": "step_1", "source_id": "src_1"}
        self.assertTrue(self.scoped.call(name, arguments)["isError"])
        self.assertEqual(self.mcp.calls, [])
        self.scoped.call("fieldkit_investigation_create", self.create)
        self.assertTrue(self.scoped.call(name, {**arguments, "id": "foreign"})["isError"])
        self.assertFalse(self.scoped.call(name, arguments)["isError"])
        self.assertEqual(self.mcp.calls[-1], (name, arguments))

    def test_foreign_case_ids_and_repeated_steps_rejected(self):
        scoped = agent.ScopedTools(
            self.mcp, "Question", "person", ["synthetic_handle"], True
        )
        scoped.call("fieldkit_investigation_create", self.create)
        self.assertTrue(
            scoped.call("fieldkit_investigation_show", {"id": "foreign"})["isError"]
        )
        step = {"id": "inv_scoped", "step_id": "step_1"}
        self.assertFalse(
            scoped.call("fieldkit_investigation_run_step", step)["isError"]
        )
        self.assertTrue(scoped.call("fieldkit_investigation_run_step", step)["isError"])
        self.assertEqual(len(self.mcp.calls), 2)


class NativeMCPTests(unittest.TestCase):
    def test_real_mcp_plan_only_keeps_review_boundary(self):
        root = Path(__file__).resolve().parents[2]
        binary = Path(
            os.environ.get("FIELDKIT_TEST_BINARY", root / "target/debug/fieldkit")
        )
        if not binary.is_file():
            self.skipTest(
                "Build Fieldkit or set FIELDKIT_TEST_BINARY for native MCP integration"
            )
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory).resolve()
            with agent.MCPClient(binary, workspace, workspace / "case") as mcp:
                names = {tool["name"] for tool in mcp.tools}
                self.assertIn("fieldkit_investigation_create", names)
                scoped = agent.ScopedTools(
                    mcp,
                    "Map synthetic infrastructure",
                    "infrastructure",
                    ["example.org"],
                )
                self.assertNotIn("fieldkit_note_add", {t["name"] for t in scoped.tools})
                result = scoped.call(
                    "fieldkit_investigation_create",
                    {
                        "question": "Map synthetic infrastructure",
                        "mode": "infrastructure",
                        "seeds": ["example.org"],
                    },
                )
                self.assertFalse(result["isError"])
                inv_id = scoped.investigation_id
                result = scoped.call(
                    "fieldkit_investigation_plan_step",
                    {
                        "id": inv_id,
                        "job": {
                            "command": "dns",
                            "target": "example.org",
                            "options": {"types": ["A"]},
                        },
                        "rationale": "Plan only: no network request is authorized",
                    },
                )
                self.assertFalse(result["isError"], result)
                state = scoped.call("fieldkit_investigation_show", {"id": inv_id})[
                    "structuredContent"
                ]["data"]
                self.assertEqual(state["summary"], [])
                self.assertEqual(state["steps"][0]["status"], "pending")
                rejected = scoped.call(
                    "fieldkit_investigation_propose",
                    {
                        "id": inv_id,
                        "content": "Unverified assertion",
                        "kind": "identity_sensitive",
                        "citations": [
                            {"step_id": state["steps"][0]["id"], "pointer": "/data"}
                        ],
                    },
                )
                self.assertTrue(rejected["isError"], rejected)


if __name__ == "__main__":
    unittest.main()
