"""Self-tests for the alf invoker seam (WP-M4 task 1a).

Two guarantees, both stdlib-only and server-free (CI tier):

  * `CliInvoker` is a TRANSPARENT passthrough — `run.alf.json(argv)` becomes
    exactly `container.exec_json(["alf", *argv])` — so the zeroclaw/openclaw/
    hermes tiers are byte-for-byte unchanged by the seam (goal c for the harness).
  * `McpInvoker._map` routes only the single-agent, non-destructive, tool-backed
    commands to a `tools/call`; every CLI-only or unmappable shape (export,
    --version, keygen/decrypt, sync --all/--force-first-sync, a foreign --agent,
    agents enable) returns None so the invoker falls back to the CLI. This is the
    explicit CLI/MCP boundary (design L10), pinned so it can't drift silently.

Run:  python3 -m unittest tests.lifecycle.test_invoker
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from alflab.invoker import CliInvoker, McpInvoker  # noqa: E402


class _RecordingContainer:
    """Records the argv `exec`/`exec_json` are called with (no docker)."""

    def __init__(self):
        self.calls = []

    def exec_json(self, argv, **kw):
        self.calls.append(("json", list(argv), kw))
        return ("PROC", {"ok": True})

    def exec(self, argv, **kw):
        self.calls.append(("exec", list(argv), kw))
        return "PROC"


class CliInvokerPassthroughTest(unittest.TestCase):
    def test_json_prepends_alf_and_forwards_kwargs(self):
        ctr = _RecordingContainer()
        inv = CliInvoker(ctr)
        proc, parsed = inv.json(["check", "-r", "zeroclaw"], timeout=300)
        self.assertEqual(ctr.calls, [("json", ["alf", "check", "-r", "zeroclaw"],
                                      {"timeout": 300})])
        self.assertEqual((proc, parsed), ("PROC", {"ok": True}))

    def test_exec_prepends_alf(self):
        ctr = _RecordingContainer()
        inv = CliInvoker(ctr)
        inv.exec(["export", "-r", "zeroclaw", "-o", "/x.alf"], timeout=120)
        self.assertEqual(ctr.calls, [("exec", ["alf", "export", "-r", "zeroclaw",
                                               "-o", "/x.alf"], {"timeout": 120})])


class McpMappingTest(unittest.TestCase):
    def setUp(self):
        # Bypass __init__ (no container/session needed for pure mapping).
        self.inv = McpInvoker.__new__(McpInvoker)
        self.inv.agent = "default"

    def assertMaps(self, argv, expected):
        self.assertEqual(self.inv._map(argv), expected, argv)

    def test_tool_backed_commands_map(self):
        self.assertMaps(["check", "-r", "generic"], ("alf_check", {}))
        self.assertMaps(["status"], ("alf_status", {}))
        self.assertMaps(["sync", "-r", "generic"], ("alf_sync", {}))
        self.assertMaps(["sync", "-r", "generic", "--recover"],
                        ("alf_sync", {"recover": True}))
        self.assertMaps(["restore", "-r", "generic", "--at-sequence", "7", "--dry-run"],
                        ("alf_restore", {"at_sequence": 7, "dry_run": True}))
        self.assertMaps(["restore", "-r", "generic", "--mode", "merge"],
                        ("alf_restore", {"mode": "merge"}))
        self.assertMaps(
            ["vault", "add", "-r", "generic", "--service", "email",
             "--label", "z6", "--secret", "sk-FAKE"],
            ("alf_vault_add", {"service": "email", "secret": "sk-FAKE", "label": "z6"}))
        self.assertMaps(["vault", "list", "-r", "generic"], ("alf_vault_list", {}))
        self.assertMaps(["vault", "delete", "--label", "z6"],
                        ("alf_vault_delete", {"by": "label", "value": "z6"}))
        self.assertMaps(["agents", "-r", "generic"], ("alf_agents_list", {}))
        self.assertMaps(["add", "notes.txt", "--external"],
                        ("alf_track", {"path": "notes.txt", "external": True}))

    def test_cli_only_and_unmappable_fall_back(self):
        for argv in (
            ["--version"],
            ["export", "-r", "generic", "-o", "/x.alf"],
            ["import", "-r", "generic", "-i", "/x.alf"],
            ["vault", "keygen", "--out", "/k"],
            ["vault", "decrypt", "--label", "z6"],
            ["sync", "-r", "generic", "--all", "--force-first-sync"],
            ["agents", "-r", "generic", "enable", "agent_b"],
            ["purge", "-r", "generic"],
        ):
            self.assertIsNone(self.inv._map(argv), argv)

    def test_foreign_agent_falls_back_but_pinned_agent_maps(self):
        # A command addressing a different agent than the pinned server can't go
        # through this one-agent session (design §7.W7) → CLI fallback.
        self.assertIsNone(self.inv._map(["sync", "-r", "generic", "--agent", "agent_b"]))
        self.assertMaps(["sync", "-r", "generic", "--agent", "default"], ("alf_sync", {}))

    def test_secret_file_add_falls_back(self):
        # --secret-file / --secret-json have no in-context analog → CLI.
        self.assertIsNone(self.inv._map(
            ["vault", "add", "--service", "x", "--secret-file", "/s"]))

    def test_add_with_runtime_flag_maps_the_path_not_the_flag_value(self):
        # `alf add -r generic notes.txt` must map path=notes.txt, never the
        # runtime flag's VALUE ("generic") — the pre-WP-O.6 parser took the
        # first non-dash token, which was the flag value.
        self.assertMaps(["add", "-r", "generic", "notes.txt"],
                        ("alf_track", {"path": "notes.txt"}))
        self.assertMaps(["add", "--runtime", "generic", "-w", "/ws",
                         "notes.txt", "--external"],
                        ("alf_track", {"path": "notes.txt", "external": True}))
        # A value-flag with no positional left has no path → CLI fallback.
        self.assertIsNone(self.inv._map(["add", "-r", "generic"]))

    def test_vault_add_default_type_maps(self):
        # An explicit --type account is the tool's hardcoded type → still maps.
        self.assertMaps(
            ["vault", "add", "--type", "account", "--service", "email",
             "--secret", "sk-FAKE"],
            ("alf_vault_add", {"service": "email", "secret": "sk-FAKE"}))

    def test_vault_add_nondefault_type_falls_back_to_cli(self):
        # The v1 alf_vault_add tool hardcodes the `account` credential type; a
        # non-account --type/-t must go to the CLI, not be silently coerced.
        self.assertIsNone(self.inv._map(
            ["vault", "add", "--type", "api_key", "--service", "email",
             "--secret", "sk-FAKE"]))
        self.assertIsNone(self.inv._map(
            ["vault", "add", "-t", "token", "--service", "email",
             "--secret", "sk-FAKE"]))


if __name__ == "__main__":
    unittest.main()
