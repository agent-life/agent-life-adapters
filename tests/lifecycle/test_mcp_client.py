"""Self-tests for the stdlib MCP client (WP-M4 task 1c).

These run in the zero-secrets CI tier (`lifecycle-nollm.yml` → `python3 -m
unittest discover`) and need NO alf binary and NO docker: the client is driven
against a fake JSON-RPC server implemented in this very file (re-launched with
`--fake-server`). They pin the three exchanges the harness relies on plus the
two things that are easy to get wrong on a pipe — interleaved progress
notifications must not be mistaken for a response, and a protocol `error` must
raise while a tool `isError` must not.

Run:  python3 -m unittest tests.lifecycle.test_mcp_client
"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from alflab.dockerctl import local_stdio_session  # noqa: E402
from alflab.mcp_client import McpClient, McpError  # noqa: E402

FAKE = [sys.executable, str(Path(__file__).resolve()), "--fake-server"]


class McpClientTest(unittest.TestCase):
    def setUp(self):
        self.sess = local_stdio_session(FAKE)
        self.client = McpClient(self.sess.proc, client_name="alflab-test")

    def tearDown(self):
        self.client.close()
        self.sess.close()

    def test_initialize_returns_server_info_and_instructions(self):
        init = self.client.initialize()
        self.assertEqual(init.get("protocolVersion"), "2025-11-25")
        self.assertEqual(init["serverInfo"]["name"], "fake-alf")
        self.assertIn("continuity", self.client.instructions)

    def test_tools_list(self):
        self.client.initialize()
        names = [t["name"] for t in self.client.list_tools()]
        self.assertEqual(sorted(names), ["boom", "echo", "slow"])

    def test_dual_result_structured_and_text(self):
        self.client.initialize()
        r = self.client.call_tool("echo", {"value": "hi"})
        self.assertFalse(r.is_error)
        self.assertTrue(r.ok)
        # structuredContent AND a serialized-JSON text block (the 2025-06-18 dual).
        self.assertEqual(r.structured, {"ok": True, "echo": "hi"})
        self.assertEqual(json.loads(r.text), {"ok": True, "echo": "hi"})
        # `.parsed()` prefers structured; `.parsed()` == the exec_json dict.
        self.assertEqual(r.parsed(), {"ok": True, "echo": "hi"})

    def test_tool_error_is_not_a_protocol_error(self):
        self.client.initialize()
        r = self.client.call_tool("boom", {})
        self.assertTrue(r.is_error)     # isError → surfaced, not raised
        self.assertFalse(r.ok)
        self.assertEqual(r.parsed().get("code"), "kaboom")

    def test_progress_notifications_do_not_corrupt_the_response(self):
        self.client.initialize()
        # `slow` emits two progress notifications BEFORE its result; the reader
        # must demux them out of band and still return the matching response.
        r = self.client.call_tool("slow", {}, timeout=10)
        self.assertFalse(r.is_error)
        self.assertEqual(r.parsed(), {"ok": True, "done": True})

    def test_unknown_method_raises_mcp_error(self):
        self.client.initialize()
        with self.assertRaises(McpError):
            # A JSON-RPC `error` object (method not found) must raise.
            self.client._request("does/not/exist", {}, timeout=10)

    def test_ok_is_false_when_payload_ok_is_false(self):
        self.client.initialize()
        # `alf_check`-shaped: not a tool error, but payload ok:false.
        r = self.client.call_tool("echo", {"value": "x", "ok": False})
        self.assertFalse(r.is_error)
        self.assertFalse(r.ok)          # honours the payload's own ok flag

    def test_non_json_stdout_is_counted_as_protocol_violation(self):
        # stdout is the MCP transport: a non-JSON line is a protocol violation.
        # The pump must record it AND keep going — the request on the same pipe
        # still succeeds (WP-O.5; the runner turns the record into a FAIL stage).
        self.client.initialize()
        r = self.client.call_tool("noise", {})
        self.assertFalse(r.is_error)
        self.assertEqual(r.parsed(), {"ok": True, "noise": True})
        self.assertEqual(len(self.client.protocol_violations), 1)
        self.assertIn("NOT-JSON", self.client.protocol_violations[0])

    def test_call_tool_increments_sent_counter(self):
        # The wire counter backs the W1 ≤6-call budget (WP-O.8): it counts
        # tools/call requests SENT, and the handshake is not a tool call.
        self.client.initialize()
        self.assertEqual(self.client.tool_calls_sent, 0)
        self.client.call_tool("echo", {"value": "a"})
        self.client.call_tool("echo", {"value": "b"})
        self.assertEqual(self.client.tool_calls_sent, 2)


# ---------------------------------------------------------------------------
# Fake MCP server (line-delimited JSON-RPC) — the transport under test.
# ---------------------------------------------------------------------------

def _fake_server() -> None:
    stdin = sys.stdin.buffer
    stdout = sys.stdout.buffer

    def send(obj):
        stdout.write((json.dumps(obj) + "\n").encode("utf-8"))
        stdout.flush()

    for raw in iter(stdin.readline, b""):
        line = raw.decode("utf-8").strip()
        if not line:
            continue
        msg = json.loads(line)
        method = msg.get("method")
        rid = msg.get("id")
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": rid, "result": {
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake-alf", "version": "0"},
                "instructions": "durable memory continuity via ALF (fake)."}})
        elif method == "notifications/initialized":
            pass  # notifications carry no id and get no response
        elif method == "tools/list":
            send({"jsonrpc": "2.0", "id": rid, "result": {"tools": [
                {"name": "echo", "inputSchema": {"type": "object"}},
                {"name": "boom", "inputSchema": {"type": "object"}},
                {"name": "slow", "inputSchema": {"type": "object"}}]}})
        elif method == "tools/call":
            name = msg["params"]["name"]
            args = msg["params"].get("arguments", {})
            if name == "echo":
                payload = {"ok": args.get("ok", True), "echo": args.get("value")}
                send({"jsonrpc": "2.0", "id": rid, "result": {
                    "content": [{"type": "text", "text": json.dumps(payload)}],
                    "structuredContent": payload, "isError": False}})
            elif name == "boom":
                payload = {"ok": False, "code": "kaboom", "error": "boom"}
                send({"jsonrpc": "2.0", "id": rid, "result": {
                    "content": [{"type": "text", "text": json.dumps(payload)}],
                    "structuredContent": payload, "isError": True}})
            elif name == "noise":
                # A raw non-JSON line on stdout (a protocol violation) BEFORE
                # the valid response — deliberately unlisted in tools/list.
                stdout.write(b"NOT-JSON stray banner line\n")
                stdout.flush()
                payload = {"ok": True, "noise": True}
                send({"jsonrpc": "2.0", "id": rid, "result": {
                    "content": [{"type": "text", "text": json.dumps(payload)}],
                    "structuredContent": payload, "isError": False}})
            elif name == "slow":
                token = msg.get("params", {}).get("_meta", {}).get("progressToken")
                for step in (1, 2):
                    send({"jsonrpc": "2.0", "method": "notifications/progress",
                          "params": {"progressToken": token or 0, "progress": step,
                                     "message": f"step {step}"}})
                payload = {"ok": True, "done": True}
                send({"jsonrpc": "2.0", "id": rid, "result": {
                    "content": [{"type": "text", "text": json.dumps(payload)}],
                    "structuredContent": payload, "isError": False}})
            else:
                send({"jsonrpc": "2.0", "id": rid,
                      "error": {"code": -32601, "message": f"unknown tool {name}"}})
        else:
            send({"jsonrpc": "2.0", "id": rid,
                  "error": {"code": -32601, "message": f"method not found: {method}"}})


if __name__ == "__main__":
    if "--fake-server" in sys.argv:
        _fake_server()
    else:
        unittest.main()
