"""The `alf` invoker seam — CLI-exec vs MCP-session (WP-M4 task 1a).

The Z-stages must not know *how* alf is driven. Historically every stage hard-
coded `run.container.exec_json(["alf", …])`; that is exactly one of the two
transports. This module lifts the alf calls behind an `AlfInvoker` so a kit can
choose:

  * `CliInvoker` — the terminal path. `run.alf.json(argv)` is byte-for-byte
    `container.exec_json(["alf", *argv])`; `run.alf.exec(argv)` is
    `container.exec(["alf", *argv])`. The three shipped frameworks
    (zeroclaw/openclaw/hermes) keep this, so their tiers are unchanged — the
    seam is a transparent passthrough (pinned by `test_invoker.py`).

  * `McpInvoker` — the MCP-session path (the generic kit, task 2). It keeps ONE
    persistent `docker exec -i … alf mcp serve -r <rt> -w <ws>` session open (a
    `StdioSession` driven by an `McpClient`) and maps each tool-shaped alf
    command to a `tools/call`. Commands the v1 tool surface deliberately does
    NOT cover (`export` copy-out, `--version`, `vault keygen`/`decrypt`,
    `sync --all`/`--force-first-sync`, agent-switching, `agents enable`) fall
    back to a one-shot `container.exec` — the honest CLI/MCP boundary from the
    design (L10 destructive-op exclusions, export-is-CLI). Every fallback is
    logged so a run artifact shows exactly which calls took the MCP path.

The invoker returns the SAME `(proc, parsed)` / `proc` shapes the stages already
consume (`proc.returncode`, `proc.stdout`, `proc.stderr`), so the stage bodies
change only at the call site, never in their assertions. A tool error maps to
the CLI's observable contract: `returncode=1` with the `{ok:false,…}` payload as
`stdout` (exactly what a failed `alf` prints), so `_passfail` logic is identical
across transports.
"""

from __future__ import annotations

import json
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Optional

from .mcp_client import McpClient, McpError


@dataclass
class FakeProc:
    """A `subprocess.CompletedProcess` look-alike the MCP path synthesizes so a
    tool result reads exactly like a one-shot `alf` invocation to the stages."""
    returncode: int
    stdout: str = ""
    stderr: str = ""
    args: list = field(default_factory=list)


class AlfInvoker(ABC):
    """How a run drives `alf`. `argv` never includes the leading "alf"."""

    #: which of the tool-shaped commands actually went over MCP vs fell back to
    #: the CLI — surfaced in the run artifact (design goal e / keep-us-honest).
    def __init__(self):
        self.tool_calls: list[str] = []
        self.cli_fallbacks: list[str] = []
        #: stdout protocol violations snapshotted from the MCP client at close()
        #: (always empty for the CLI path). The runner reads this after the
        #: session is gone and turns any entries into a synthetic FAIL stage.
        self.last_protocol_violations: list[str] = []

    @abstractmethod
    def json(self, argv: list, **kw):
        """Run `alf <argv>` and parse its JSON stdout → (proc, parsed|None)."""

    @abstractmethod
    def exec(self, argv: list, **kw):
        """Run `alf <argv>` and return the proc (no JSON parse)."""

    def close(self) -> None:
        """Release any persistent session (no-op for the CLI path)."""


class CliInvoker(AlfInvoker):
    """The terminal transport — a transparent passthrough to `container.exec*`."""

    def __init__(self, container):
        super().__init__()
        self.container = container

    def json(self, argv: list, **kw):
        return self.container.exec_json(["alf", *argv], **kw)

    def exec(self, argv: list, **kw):
        return self.container.exec(["alf", *argv], **kw)


# Flags whose NEXT argv token is a value, not a positional (used by the `add`
# mapping so `alf add -r generic notes.txt` maps the path, never "generic").
VALUE_FLAGS = {"-r", "--runtime", "-w", "--workspace", "--agent"}


# ---------------------------------------------------------------------------
# MCP transport
# ---------------------------------------------------------------------------

class McpInvoker(AlfInvoker):
    """Drives the tool-shaped alf commands over a persistent MCP stdio session;
    falls back to a one-shot `container.exec` for everything the v1 tool surface
    doesn't cover."""

    def __init__(self, container, *, runtime: str, workspace: str,
                 agent: Optional[str] = None, env: Optional[dict] = None,
                 log=None):
        super().__init__()
        self.container = container
        self.runtime = runtime
        self.workspace = workspace
        self.agent = agent
        self.env = env or {}
        self._log = log or (lambda _m: None)
        self._session = None
        self._client: Optional[McpClient] = None

    # -- session lifetime ------------------------------------------------------

    def start(self) -> McpClient:
        """Launch `alf mcp serve` in-container and complete the handshake. The
        host owns the process (design §5); `close()` shuts it via stdin EOF."""
        if self._client is not None:
            return self._client
        argv = ["alf", "mcp", "serve", "-r", self.runtime, "-w", self.workspace]
        self._session = self.container.exec_stdio(argv, env=self.env)
        self._client = McpClient(self._session.proc,
                                 client_name="alflab-generic-host")
        init = self._client.initialize()
        self._log(f"MCP session up: {init.get('serverInfo', {})} "
                  f"proto={init.get('protocolVersion')}")
        return self._client

    def client(self) -> McpClient:
        return self.start()

    def close(self) -> None:
        if self._client is not None:
            try:
                self._client.close()
            finally:
                # Snapshot BEFORE dropping the client: the runner asserts stdout
                # protocol discipline after teardown, when the client is gone.
                self.last_protocol_violations = list(self._client.protocol_violations)
                self._client = None
        if self._session is not None:
            self._session.close()
            self._session = None

    def server_stderr(self) -> str:
        return self._client.stderr_text() if self._client is not None else ""

    # -- the seam --------------------------------------------------------------

    def json(self, argv: list, **kw):
        mapped = self._map(argv)
        if mapped is None:
            self.cli_fallbacks.append(" ".join(str(a) for a in argv))
            self._log(f"MCP: no tool for `alf {' '.join(map(str, argv))}` → CLI fallback")
            return self.container.exec_json(["alf", *argv], **kw)
        tool, arguments = mapped
        timeout = float(kw.get("timeout", 300))
        try:
            result = self.client().call_tool(tool, arguments, timeout=timeout)
        except McpError as e:
            return FakeProc(returncode=1, stderr=f"MCP transport error: {e}"), None
        self.tool_calls.append(tool)
        parsed = result.parsed()
        stdout = json.dumps(parsed) if parsed is not None else (result.text or "")
        proc = FakeProc(returncode=1 if result.is_error else 0, stdout=stdout,
                        stderr="" if not result.is_error else stdout)
        return proc, parsed

    def exec(self, argv: list, **kw):
        # No tool returns raw stdout text; every `.exec` call (`--version`,
        # `export -o`, `vault decrypt`, keygen) is CLI-only by design.
        self.cli_fallbacks.append(" ".join(str(a) for a in argv))
        return self.container.exec(["alf", *argv], **kw)

    # -- argv → (tool, arguments) ---------------------------------------------

    def _map(self, argv: list) -> Optional[tuple]:
        """Map a tool-shaped alf argv to (tool_name, arguments), or None to fall
        back to the CLI. Only single-agent, non-destructive shapes map; the flags
        the v1 tools don't accept (`--all`, `--force-first-sync`, a foreign
        `--agent`) force a fallback so the boundary is explicit, not silent."""
        if not argv:
            return None
        flags = _Flags(argv)
        # A command addressing a DIFFERENT agent than the pinned session can't go
        # through this server (one server per agent — design §7.W7); fall back.
        target = flags.value("--agent")
        if target is not None and self.agent is not None and target != self.agent:
            return None

        head = argv[0]
        if head == "check":
            return ("alf_check", {})
        if head == "status":
            return ("alf_status", {})
        if head == "sync":
            if flags.has("--all") or flags.has("--force-first-sync"):
                return None
            args = {}
            if flags.has("--recover"):
                args["recover"] = True
            return ("alf_sync", args)
        if head == "restore":
            args = {}
            at = flags.value("--at-sequence")
            if at is not None:
                args["at_sequence"] = int(at)
            if flags.has("--dry-run"):
                args["dry_run"] = True
            mode = flags.value("--mode")
            if mode is not None:
                args["mode"] = mode
            return ("alf_restore", args)
        if head == "vault" and len(argv) > 1:
            sub = argv[1]
            if sub == "add":
                if flags.value("--secret") is None:  # --secret-file/-json: CLI
                    return None
                # The v1 tool hardcodes the `account` credential type — any other
                # --type/-t must go through the CLI, not be silently coerced.
                ctype = flags.value("--type") or flags.value("-t")
                if ctype is not None and ctype != "account":
                    return None
                args = {
                    "service": flags.value("--service") or "",
                    "secret": flags.value("--secret"),
                }
                for cli_flag, key in (("--username", "username"), ("--label", "label"),
                                      ("--description", "description")):
                    v = flags.value(cli_flag)
                    if v is not None:
                        args[key] = v
                if flags.has("--update"):
                    args["update"] = True
                return ("alf_vault_add", args)
            if sub == "list":
                return ("alf_vault_list", {})
            if sub == "delete":
                # MCP uses a discriminated by/value selector (the CLI keeps
                # --id/--label/--service and enforces exactly one). Map the single
                # set flag to (by, value); leave empty if none (the tool errors).
                args = {}
                for cli_flag, by in (("--id", "id"), ("--label", "label"),
                                     ("--service", "service")):
                    v = flags.value(cli_flag)
                    if v is not None:
                        args = {"by": by, "value": v}
                        break
                return ("alf_vault_delete", args)
            return None  # keygen / decrypt / encrypt / rotate-key: CLI-only (L10)
        if head == "add":  # `alf add <path>` → alf_track
            # Flag-value-aware: `alf add -r generic notes.txt` must map the PATH,
            # not the flag's value ("generic"). Skip each value-taking flag's
            # argument; the first remaining non-dash token is the path.
            rest = [str(a) for a in argv[1:]]
            path = None
            skip_next = False
            for a in rest:
                if skip_next:
                    skip_next = False
                    continue
                if a in VALUE_FLAGS:
                    skip_next = True
                    continue
                if a.startswith("-"):
                    continue
                path = a
                break
            if path is None:
                return None
            args = {"path": path}
            if flags.has("--external") or flags.has("--yes-external"):
                args["external"] = True
            return ("alf_track", args)
        if head == "agents" and not any(
                a in ("enable", "disable") for a in argv[1:]):
            return ("alf_agents_list", {})
        # export / import / validate / login / purge / agents enable|disable / …
        return None


class _Flags:
    """Tiny argv reader: `--flag value` and bare `--flag` presence."""

    def __init__(self, argv: list):
        self._argv = [str(a) for a in argv]

    def has(self, flag: str) -> bool:
        return flag in self._argv

    def value(self, flag: str) -> Optional[str]:
        for i, a in enumerate(self._argv):
            if a == flag and i + 1 < len(self._argv):
                return self._argv[i + 1]
            if a.startswith(flag + "="):
                return a.split("=", 1)[1]
        return None
