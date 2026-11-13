"""Keep model context compact without dropping evidence fields."""

import importlib.util
import json
import os
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "fieldkit_agent_context", Path(__file__).with_name("fieldkit_agent.py")
)
agent = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(agent)


class ContextTests(unittest.TestCase):
    def test_structured_observation_sent_once_with_full_trace_retained(self):
        observation = {
            "isError": False,
            "structuredContent": {"data": {"exact": "evidence"}},
            "content": [{"type": "text", "text": '{"data":{"exact":"evidence"}}'}],
        }

        class Client:
            def __init__(self):
                self.count = 0
                self.last = None

            def complete(self, messages, tools):
                self.count += 1
                self.last = messages[-1]
                message = {"role": "assistant", "content": "Evidence inspected."}
                if self.count == 1:
                    message = {
                        "role": "assistant",
                        "content": None,
                        "tool_calls": [
                            {
                                "id": "show1",
                                "type": "function",
                                "function": {
                                    "name": "fieldkit_graph_show",
                                    "arguments": "{}",
                                },
                            }
                        ],
                    }
                return {
                    "choices": [{"message": message, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 10},
                }

        client = Client()
        report = agent.run_agent(
            client,
            [{"name": "fieldkit_graph_show", "inputSchema": {"type": "object"}}],
            lambda *_: observation,
            "Inspect synthetic evidence",
        )
        sent = json.loads(client.last["content"])
        self.assertNotIn("content", sent)
        self.assertEqual(sent["structuredContent"], observation["structuredContent"])
        self.assertEqual(report["trace"][0]["result"], observation)

    def test_native_schema_lookup_returns_one_command_without_collecting(self):
        root = Path(__file__).resolve().parents[2]
        binary = Path(
            os.environ.get("FIELDKIT_TEST_BINARY", root / "target/debug/fieldkit")
        )
        if not binary.is_file():
            self.skipTest("Build Fieldkit or set FIELDKIT_TEST_BINARY")
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory).resolve()
            with agent.MCPClient(binary, workspace, workspace / "case") as mcp:
                scoped = agent.ScopedTools(
                    mcp, "Question", "infrastructure", ["example.org"]
                )
                descriptor = next(
                    tool
                    for tool in scoped.tools
                    if tool["name"] == "fieldkit_capabilities"
                )
                self.assertEqual(descriptor["inputSchema"]["required"], ["command"])
                self.assertTrue(scoped.call("fieldkit_capabilities", {})["isError"])
                self.assertTrue(
                    scoped.call("fieldkit_capabilities", {"command": "invented"})[
                        "isError"
                    ]
                )
                result = scoped.call("fieldkit_capabilities", {"command": "dns"})
                self.assertFalse(result["isError"])
                commands = result["structuredContent"]["data"]["commands"]
                self.assertEqual([row["name"] for row in commands], ["dns"])
                self.assertIn("input_schema", commands[0])
                self.assertLess(len(json.dumps(result)), 5000)
                self.assertIsNone(scoped.investigation_id)


if __name__ == "__main__":
    unittest.main()
