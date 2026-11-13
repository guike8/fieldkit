"""MCP transport failure tests; fake processes never perform collection."""

import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "fieldkit_agent_transport", Path(__file__).with_name("fieldkit_agent.py")
)
agent = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(agent)


class TransportTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.workspace = Path(self.directory.name).resolve()
        self.binary = self.workspace / "fake-mcp"

    def script(self, source):
        self.binary.write_text(f"#!{sys.executable}\n" + source, encoding="utf-8")
        self.binary.chmod(0o700)

    def connect(self, timeout=1.0, key_env="INKLING_API_KEY"):
        return agent.MCPClient(
            self.binary,
            self.workspace,
            self.workspace / "case",
            timeout=timeout,
            key_env=key_env,
        )

    def test_initialization_timeout_fails_closed(self):
        # Read requests without answering. Parent deadline must terminate this child.
        self.script("import sys\nfor line in sys.stdin:\n    pass\n")
        with self.assertRaisesRegex(agent.AgentError, "timeout.*unknown"):
            self.connect(timeout=0.05)

    def test_wrong_response_id_rejected(self):
        self.script(
            "import json, sys\n"
            "request = json.loads(sys.stdin.readline())\n"
            "print(json.dumps({'jsonrpc':'2.0','id':request['id']+1,'result':{}}), flush=True)\n"
        )
        with self.assertRaisesRegex(agent.AgentError, "response id"):
            self.connect()

    def test_invalid_json_rejected(self):
        self.script("import sys\nsys.stdin.readline()\nprint('{invalid', flush=True)\n")
        with self.assertRaisesRegex(agent.AgentError, "Invalid MCP JSON"):
            self.connect()

    def test_oversized_response_rejected(self):
        self.script(
            "import sys\nsys.stdin.readline()\nprint('x' * (1024*1024+1), flush=True)\n"
        )
        with self.assertRaisesRegex(agent.AgentError, "exceeds 1 MiB"):
            self.connect()

    def test_model_key_not_inherited_by_mcp_process(self):
        self.script(
            "import json, os, sys\n"
            "for line in sys.stdin:\n"
            "    request = json.loads(line)\n"
            "    if 'id' not in request:\n"
            "        continue\n"
            "    result = {'key_present': 'SYNTHETIC_MODEL_KEY' in os.environ}\n"
            "    if request['method'] == 'tools/list':\n"
            "        result = {'tools': []}\n"
            "    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}), flush=True)\n"
        )
        with (
            patch.dict(os.environ, {"SYNTHETIC_MODEL_KEY": "test-only-not-a-real-key"}),
            self.connect(key_env="SYNTHETIC_MODEL_KEY") as client,
        ):
            self.assertFalse(client.initialization["key_present"])
            self.assertEqual(client.tools, [])


if __name__ == "__main__":
    unittest.main()
