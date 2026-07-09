"""A minimal, stdlib-only MCP client — the WP-M4 spike decision (task 1c).

The lifecycle CI tier is stdlib-only with zero third-party imports (it is the
zero-secrets PR guard, `lifecycle-nollm.yml`). Driving `alf mcp serve` from it
therefore cannot pull in the `mcp` pip package; this module hand-rolls exactly
the three JSON-RPC exchanges the harness needs — `initialize` (+ the
`notifications/initialized` follow-up), `tools/list`, `tools/call` — over the
MCP **stdio** transport (newline-delimited JSON, no Content-Length framing; the
spec forbids embedded newlines in a message, so one line == one message).

Design notes:
  * Transport-agnostic: it drives any `subprocess.Popen` whose stdin/stdout are
    byte pipes — a local `alf mcp serve` (unit tests) or an in-container
    `docker exec -i … alf mcp serve` session (the generic/Hermes kits). The
    caller owns process lifetime; `close()` shuts the server the MCP way (close
    stdin → the server sees EOF and exits — design §5, F14).
  * A background reader thread demultiplexes the stream: responses are matched
    by JSON-RPC `id`; notifications (no `id`, e.g. interleaved `notifications/
    progress` from `alf_sync`) are collected out of band, never confused for a
    response. This is the only robust way to honour a per-call timeout on a pipe
    with stdlib alone.
  * stderr is drained on its own thread and retained (ring-buffered): it is the
    server's diagnostic channel (the protocol owns stdout), and the WP-M4 MCP
    LLM tier asserts a **server-log marker** to prove a sync went through the
    MCP path rather than the terminal.

No timing primitives beyond `queue.Queue.get(timeout=…)`; nothing here imports
anything outside the standard library.
"""

from __future__ import annotations

import json
import queue
import threading
from dataclasses import dataclass, field
from typing import Optional

PROTOCOL_VERSION = "2025-11-25"  # rmcp ProtocolVersion::LATEST (design L8)


class McpError(RuntimeError):
    """A JSON-RPC protocol error (an `error` object on a response), a transport
    failure, or a timeout. Distinct from a *tool* error — a tool that fails
    returns a normal result with `is_error=True` (see `ToolResult`)."""


@dataclass
class ToolResult:
    """The outcome of one `tools/call`. `is_error` is the MCP `isError` flag: a
    tool-execution failure (the CLI's `{ok:false, code?, error, hint}` payload)
    rather than a protocol error. `structured` is `structuredContent` (the typed
    result); `text` is the serialized-JSON `TextContent` block (the dual result
    every tool also returns for pre-2025-06-18 clients)."""

    name: str
    is_error: bool
    structured: Optional[dict]
    text: Optional[str]
    raw: dict = field(default_factory=dict)

    @property
    def ok(self) -> bool:
        """True iff the call succeeded AND the payload's own `ok` isn't false —
        so a caller can treat a ToolResult like the CLI's `(proc, parsed)`."""
        if self.is_error:
            return False
        if isinstance(self.structured, dict) and self.structured.get("ok") is False:
            return False
        return True

    def parsed(self) -> Optional[dict]:
        """The structured payload if present, else the parsed text block — the
        `exec_json`-shaped dict the stages read."""
        if self.structured is not None:
            return self.structured
        if self.text:
            try:
                return json.loads(self.text)
            except json.JSONDecodeError:
                return None
        return None


class McpClient:
    """A synchronous JSON-RPC client over one `alf mcp serve` process."""

    def __init__(self, proc, *, client_name: str = "alflab-lifecycle",
                 client_version: str = "1.0", stderr_ring: int = 400):
        """`proc` is a started `subprocess.Popen` with byte-pipe stdin/stdout
        (and optionally stderr). The client does not spawn it — the transport
        (a local subprocess or a docker-exec session) owns that."""
        if proc.stdin is None or proc.stdout is None:
            raise McpError("MCP transport process needs piped stdin and stdout")
        self._proc = proc
        self._client_name = client_name
        self._client_version = client_version
        self._next_id = 0
        self._lock = threading.Lock()
        self._responses: dict[int, dict] = {}
        self._response_ready = threading.Condition()
        self._notifications: "queue.Queue[dict]" = queue.Queue()
        self._eof = threading.Event()
        self._stderr_lines: list[str] = []
        self._stderr_ring = stderr_ring
        self.server_info: dict = {}
        self.instructions: str = ""

        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._reader.start()
        if proc.stderr is not None:
            self._stderr_reader = threading.Thread(target=self._stderr_loop, daemon=True)
            self._stderr_reader.start()

    # -- background pumps ------------------------------------------------------

    def _read_loop(self) -> None:
        try:
            for raw in iter(self._proc.stdout.readline, b""):
                line = raw.decode("utf-8", errors="replace").strip()
                if not line:
                    continue
                try:
                    msg = json.loads(line)
                except json.JSONDecodeError:
                    # A non-JSON line on stdout is a protocol violation; keep it
                    # for diagnostics but don't crash the pump.
                    self._append_stderr(f"[stdout non-json] {line[:200]}")
                    continue
                if isinstance(msg, dict) and "id" in msg and (
                        "result" in msg or "error" in msg):
                    with self._response_ready:
                        self._responses[msg["id"]] = msg
                        self._response_ready.notify_all()
                else:
                    self._notifications.put(msg)
        finally:
            self._eof.set()
            with self._response_ready:
                self._response_ready.notify_all()

    def _stderr_loop(self) -> None:
        for raw in iter(self._proc.stderr.readline, b""):
            self._append_stderr(raw.decode("utf-8", errors="replace").rstrip("\n"))

    def _append_stderr(self, line: str) -> None:
        self._stderr_lines.append(line)
        if len(self._stderr_lines) > self._stderr_ring:
            del self._stderr_lines[: len(self._stderr_lines) - self._stderr_ring]

    # -- low-level JSON-RPC ----------------------------------------------------

    def _send(self, obj: dict) -> None:
        data = (json.dumps(obj, separators=(",", ":")) + "\n").encode("utf-8")
        with self._lock:
            if self._proc.stdin is None:
                raise McpError("MCP transport stdin is closed")
            try:
                self._proc.stdin.write(data)
                self._proc.stdin.flush()
            except (BrokenPipeError, ValueError) as e:
                raise McpError(f"MCP transport write failed: {e}") from e

    def _request(self, method: str, params: Optional[dict] = None,
                 *, timeout: float = 60.0) -> dict:
        with self._lock_id() as rid:
            self._send({"jsonrpc": "2.0", "id": rid, "method": method,
                        "params": params or {}})
        deadline_msg = f"MCP {method} timed out after {timeout}s"
        with self._response_ready:
            end = _Deadline(timeout)
            while rid not in self._responses:
                if self._eof.is_set():
                    raise McpError(
                        f"MCP server closed the connection before answering {method}"
                        + self._stderr_tail())
                remaining = end.remaining()
                if remaining <= 0:
                    raise McpError(deadline_msg + self._stderr_tail())
                self._response_ready.wait(timeout=remaining)
            msg = self._responses.pop(rid)
        if "error" in msg:
            err = msg["error"]
            raise McpError(f"MCP {method} error {err.get('code')}: {err.get('message')}")
        return msg.get("result", {})

    def _notify(self, method: str, params: Optional[dict] = None) -> None:
        self._send({"jsonrpc": "2.0", "method": method, "params": params or {}})

    def _lock_id(self):
        client = self

        class _IdCtx:
            def __enter__(self):
                with client._lock:
                    client._next_id += 1
                    self.rid = client._next_id
                return self.rid

            def __exit__(self, *exc):
                return False

        return _IdCtx()

    # -- MCP surface -----------------------------------------------------------

    def initialize(self, *, timeout: float = 30.0) -> dict:
        """The initialize handshake + the mandatory `notifications/initialized`.
        Returns the server's initialize result (serverInfo, capabilities,
        instructions)."""
        result = self._request("initialize", {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": self._client_name, "version": self._client_version},
        }, timeout=timeout)
        self.server_info = result.get("serverInfo", {})
        self.instructions = result.get("instructions", "") or ""
        self._notify("notifications/initialized")
        return result

    def list_tools(self, *, timeout: float = 30.0) -> list[dict]:
        return self._request("tools/list", {}, timeout=timeout).get("tools", [])

    def call_tool(self, name: str, arguments: Optional[dict] = None,
                  *, timeout: float = 300.0) -> ToolResult:
        result = self._request("tools/call", {
            "name": name, "arguments": arguments or {},
        }, timeout=timeout)
        return _to_tool_result(name, result)

    # -- diagnostics + lifetime ------------------------------------------------

    def drain_notifications(self) -> list[dict]:
        out: list[dict] = []
        try:
            while True:
                out.append(self._notifications.get_nowait())
        except queue.Empty:
            pass
        return out

    def stderr_text(self) -> str:
        return "\n".join(self._stderr_lines)

    def _stderr_tail(self, n: int = 12) -> str:
        tail = self._stderr_lines[-n:]
        return ("\n  server stderr:\n    " + "\n    ".join(tail)) if tail else ""

    def close(self, *, timeout: float = 10.0) -> None:
        """Shut the server the MCP way: close stdin → EOF → the server exits
        (design §5). SIGKILL only if it ignores the close."""
        try:
            if self._proc.stdin is not None:
                try:
                    self._proc.stdin.close()
                except (BrokenPipeError, ValueError):
                    pass
            try:
                self._proc.wait(timeout=timeout)
            except Exception:  # noqa: BLE001 — best-effort teardown
                self._proc.kill()
                try:
                    self._proc.wait(timeout=timeout)
                except Exception:  # noqa: BLE001
                    pass
        finally:
            self._eof.set()


class _Deadline:
    """A monotonic countdown for `Condition.wait` — avoids `Date.now`-style
    drift when a wait is re-armed after a spurious wake."""

    def __init__(self, timeout: float):
        import time
        self._end = time.monotonic() + timeout

    def remaining(self) -> float:
        import time
        return self._end - time.monotonic()


def _to_tool_result(name: str, result: dict) -> ToolResult:
    is_error = bool(result.get("isError", False))
    structured = result.get("structuredContent")
    text = None
    for block in result.get("content", []) or []:
        if isinstance(block, dict) and block.get("type") == "text":
            text = block.get("text")
            break
    return ToolResult(name=name, is_error=is_error, structured=structured,
                      text=text, raw=result)
