"""Offline generic MCP surface (WP-M4 task 2, R1–R4 without a backend).

Drives a REAL `alf mcp serve -r generic -w <fixture>` (no docker, no backend,
no secrets) against a copy of the committed generic fixture and exercises the
tools that work offline: status/check/export_dry_run/configure/track/vault
add+list+delete/agents_list/docs. This is the substantive proof that the toy
runtime's MCP surface is dashboard-shaped and functional; the container
lifecycle tier (test.sh `lifecycle-generic`) then proves the same over a
`docker exec -i` session.

Skips cleanly when no alf-under-test is built (e.g. bare GitHub CI, which does
not build alf) — mirroring how test.sh SKIPs a tier whose tools are absent.

Run:  python3 -m unittest tests.lifecycle.test_mcp_generic
"""

from __future__ import annotations

import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from alflab.dockerctl import local_stdio_session  # noqa: E402
from alflab.mcp_client import McpClient  # noqa: E402

LIFECYCLE_DIR = Path(__file__).resolve().parent
REPO_ROOT = LIFECYCLE_DIR.parents[1]
FIXTURE = LIFECYCLE_DIR / "frameworks" / "generic" / "fixture"
GENERIC_AGENT_ID = "a11cef1c-7e57-4a1f-9c0d-e5f6a7b8c9d0"  # the fixture's .alf-agent-id


def _find_alf():
    for c in (REPO_ROOT / "target" / "x86_64-unknown-linux-musl" / "release" / "alf",
              REPO_ROOT / "target" / "release" / "alf"):
        if c.is_file():
            return c
    return None


@unittest.skipUnless(_find_alf(), "no alf-under-test built (run cargo build -p alf-cli)")
class GenericMcpSurfaceTest(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="alf-m4-generic-"))
        self.ws = self.tmp / "ws"
        shutil.copytree(FIXTURE, self.ws)
        self.alf_home = self.tmp / "alfhome"
        self.alf_home.mkdir()
        env = dict(os.environ)
        env["ALF_HOME"] = str(self.alf_home)
        env.pop("ALF_API_KEY", None)          # offline: no key → no watch loop
        argv = [str(_find_alf()), "mcp", "serve", "-r", "generic", "-w", str(self.ws)]
        self.sess = local_stdio_session(argv, env=env)
        self.client = McpClient(self.sess.proc, client_name="alflab-m4-generic")
        self.client.initialize()

    def tearDown(self):
        self.client.close()
        self.sess.close()
        shutil.rmtree(self.tmp, ignore_errors=True)

    def call(self, name, args=None):
        return self.client.call_tool(name, args or {}, timeout=60)

    # -- surface ---------------------------------------------------------------

    def test_full_v1_tool_surface_present(self):
        names = sorted(t["name"] for t in self.client.list_tools())
        # The full v1 surface: 12 M2b tools + alf_watch_set (M3).
        self.assertEqual(names, [
            "alf_agents_list", "alf_check", "alf_configure", "alf_docs",
            "alf_export_dry_run", "alf_restore", "alf_status", "alf_sync",
            "alf_track", "alf_vault_add", "alf_vault_delete", "alf_vault_list",
            "alf_watch_set",
        ])

    def test_status_reports_unconfigured_and_watch_inactive(self):
        r = self.call("alf_status")
        s = r.parsed()
        self.assertFalse(s["api_key_set"])
        self.assertFalse(s["watch"]["active"])   # no key → loop down (goal e)

    def test_check_discovers_the_single_generic_agent(self):
        r = self.call("alf_check")
        agents = r.parsed()["agents"]
        self.assertTrue(agents["first_run"])
        rows = agents["agents"]
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["runtime_agent"], "default")
        self.assertEqual(rows[0]["alf_agent_id"], GENERIC_AGENT_ID)

    def test_export_dry_run_is_dashboard_shaped(self):  # R1 + R2
        r = self.call("alf_export_dry_run")
        d = r.parsed()
        self.assertEqual(d["agent_name"], "Atlas")
        # knowledge/facts.md (semantic) + procedures/deploy.md (procedural) →
        # at least two records before any journal seeding.
        self.assertGreaterEqual(d["memory_records"], 2)
        paths = {f["path"] for f in d["files"]}
        self.assertIn(".alf-map.json", paths)
        self.assertIn("knowledge/facts.md", paths)

    def test_configure_patches_the_map(self):  # R2/R3
        r = self.call("alf_configure",
                      {"operation": "merge",
                       "body": {"watch": {"default_interval": "30m"}}})
        self.assertTrue(r.ok, r.parsed())
        import json
        got = json.loads((self.ws / ".alf-map.json").read_text())
        self.assertEqual(got["watch"]["default_interval"], "30m")

    def test_track_is_idempotent(self):  # R1
        (self.ws / "extra.txt").write_text("tracked\n", encoding="utf-8")
        first = self.call("alf_track", {"path": "extra.txt"}).parsed()
        self.assertTrue(first["ok"])
        self.assertTrue(first.get("added"))
        second = self.call("alf_track", {"path": "extra.txt"}).parsed()
        self.assertFalse(second.get("added"))    # already tracked

    def test_vault_add_auto_keygens_then_list_then_delete(self):  # R4
        add = self.call("alf_vault_add", {
            "service": "email", "label": "m4", "secret": "sk-atlas-FAKE-do-not-use"})
        payload = add.parsed()
        self.assertTrue(payload["ok"], payload)
        kg = payload.get("key_generated")
        self.assertIsNotNone(kg, "first add should auto-generate a key")
        self.assertIn("fingerprint", kg)
        self.assertNotIn("sk-atlas-FAKE", str(payload))  # never echoes the secret
        # 0600 key file at the generic default path (fingerprint-only result).
        key_file = self.alf_home / ".alf" / "vault-keys" / f"{GENERIC_AGENT_ID}.key"
        self.assertTrue(key_file.is_file())
        self.assertEqual(oct(key_file.stat().st_mode & 0o777), "0o600")

        labels = [c.get("label") for c in self.call("alf_vault_list").parsed()["credentials"]]
        self.assertIn("m4", labels)
        dele = self.call("alf_vault_delete", {"by": "label", "value": "m4"}).parsed()
        self.assertTrue(dele["ok"], dele)

    def test_agents_list_carries_generic_runtime(self):
        self.call("alf_check")  # persist the mapping first
        rows = self.call("alf_agents_list").parsed()["agents"]
        self.assertTrue(any(r.get("runtime") == "generic" for r in rows))

    def test_docs_resolves_a_topic(self):
        r = self.call("alf_docs", {"topic": "sync"})
        self.assertTrue(r.ok)
        self.assertTrue(r.parsed().get("content"))

    def test_sync_errors_offline_as_a_tool_error(self):
        r = self.call("alf_sync")
        self.assertTrue(r.is_error)          # no API key → tool error, not a crash
        self.assertFalse(r.ok)


if __name__ == "__main__":
    unittest.main()
