"""Self-tests for the viz server's deny-by-default policy (MIN-17).

The server used to root a stdlib file server at the run dir: `run.env` and
`env-files/*.env` — 0600 files holding the raw minted runtime key — plus a
directory listing were readable by any local process for the whole run. These
tests drive a REAL server over a real socket, because the defect was about what
crosses the wire, not about a helper's return value.

Run:  python3 -m unittest tests.lifecycle.test_viz_server
"""

from __future__ import annotations

import http.client
import tempfile
import unittest
from pathlib import Path

from alflab import redact
from alflab.viz_server import SERVABLE, VizServer, servable_path

KEY = "alf_" + "S3cr3tK" * 4  # runtime-key shaped (alf_ + 28 chars)


def _run_dir(tmp: Path) -> Path:
    run = tmp / "run"
    (run / "home").mkdir(parents=True)
    (run / "env-files").mkdir()
    (run / "visualization.html").write_text("<html>viz</html>", encoding="utf-8")
    (run / "events.ndjson").write_text('{"type":"run_start"}\n', encoding="utf-8")
    (run / "report.json").write_text('{"verdict":"ok"}', encoding="utf-8")
    # The secret-bearing files that must never be reachable…
    (run / "run.env").write_text(f"ALF_API_KEY={KEY}\n", encoding="utf-8")
    (run / "env-files" / "agent.env").write_text(f"ALF_API_KEY={KEY}\n", encoding="utf-8")
    (run / "driver.log").write_text(f"minted {KEY}\n", encoding="utf-8")
    # …and an ALLOWLISTED artifact that legitimately contains the key on disk.
    (run / "home" / "config.yaml").write_text(
        f'api_key: "{KEY}"\nmodel: test\n', encoding="utf-8"
    )
    return run


class VizServerPolicyTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.run = _run_dir(Path(self._tmp.name))
        redact.register_secret(KEY)
        self.srv = VizServer(self.run, port=0)
        self.srv.start()

    def tearDown(self):
        self.srv.stop()
        self._tmp.cleanup()

    def get(self, path: str):
        """Raw request — `http.client` sends the path verbatim, so traversal
        shapes reach the server instead of being normalized away by a client."""
        conn = http.client.HTTPConnection("127.0.0.1", self.srv.port, timeout=5)
        try:
            conn.putrequest("GET", path, skip_host=False, skip_accept_encoding=True)
            conn.endheaders()
            resp = conn.getresponse()
            return resp.status, resp.read().decode("utf-8", errors="replace")
        finally:
            conn.close()

    def test_secret_bearing_files_are_not_served(self):
        for path in ("/run.env", "/env-files/agent.env", "/driver.log"):
            status, body = self.get(path)
            self.assertEqual(status, 404, f"{path} must not be served")
            self.assertNotIn(KEY, body)

    def test_no_directory_listing(self):
        for path in ("/", "/env-files/", "/home/"):
            status, body = self.get(path)
            self.assertNotIn("run.env", body, f"{path} leaked a listing")
            self.assertNotIn(KEY, body)

    def test_traversal_shapes_are_refused(self):
        for path in (
            "/../run.env",
            "/./../run.env",
            "/home/../run.env",
            "/%2e%2e/run.env",
            "//run.env",
            "/run.env?x=1",
        ):
            status, body = self.get(path)
            self.assertEqual(status, 404, f"{path} must 404")
            self.assertNotIn(KEY, body)

    def test_the_page_and_its_data_are_served(self):
        status, body = self.get("/visualization.html")
        self.assertEqual(status, 200)
        self.assertIn("viz", body)
        status, body = self.get("/events.ndjson")
        self.assertEqual(status, 200)
        self.assertIn("run_start", body)
        status, _ = self.get("/report.json")
        self.assertEqual(status, 200)

    def test_allowlisted_artifacts_are_redacted_on_the_way_out(self):
        # config.yaml is fetched by the drawer AND holds the key on disk: it is
        # served, but never with the secret in it.
        status, body = self.get("/home/config.yaml")
        self.assertEqual(status, 200)
        self.assertIn("model: test", body, "the artifact is still usable")
        self.assertNotIn(KEY, body, "the key must be redacted before it leaves")
        self.assertIn("[REDACTED]", body)

    def test_missing_allowlisted_file_is_404_not_500(self):
        status, _ = self.get("/run-manifest.json")  # allowlisted, absent here
        self.assertEqual(status, 404)


class ServablePathTest(unittest.TestCase):
    def test_allowlist_excludes_secret_paths(self):
        for name in ("run.env", "env-files/agent.env", "driver.log", "report.md"):
            self.assertNotIn(name, SERVABLE)

    def test_root_maps_to_the_page(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "visualization.html").write_text("x", encoding="utf-8")
            self.assertIsNotNone(servable_path(root, "/"))
            self.assertIsNotNone(servable_path(root, ""))


if __name__ == "__main__":
    unittest.main()
