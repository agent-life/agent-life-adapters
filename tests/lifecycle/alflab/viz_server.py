"""Background HTTP server for the lifecycle visualization.

Serves what ``visualization.html`` fetches while the harness is still running.
Default-on for local runs; opt out with ``--no-viz-server``.

**Deny by default (MIN-17).** The server used to root a stdlib file server at
the run dir, which served `run.env` and `env-files/*.env` — the raw minted
runtime key, in 0600 files — plus a directory listing, to any local process for
the run's whole duration. That recreates the argv-leak class WP-O.7 exists to
prevent. Now: only the exact files the page fetches are servable (a literal
name lookup, so path traversal is impossible by construction, and there is no
listing), and every text response goes through the central redactor — so even
an allowlisted `home/config.yaml` cannot carry the key.
"""

from __future__ import annotations

import socket
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Optional

from . import redact

# Run-dir files the visualization's artifact drawer fetches (index.html
# `fetchText`). Shared with `viz/bake.py`, which inlines the same set into the
# portable HTML — one list so the live and baked views cannot drift.
VIZ_ARTIFACTS: tuple[str, ...] = (
    "report.json",
    "run-manifest.json",
    "mcp-interactions.log",
    "z16-serve-stderr.log",
    "home/config.yaml",
    "home/memories/MEMORY.md",
    "home/profiles/agent_b/config.yaml",
    "home/profiles/agent_b/SOUL.md",
    "alf-home/config.toml",
)

# Everything the live page may request: the page itself, its event stream, and
# the drawer artifacts. NOTHING else in the run dir is reachable — notably
# `run.env`, `env-files/`, `driver.log`, and the framework home trees.
SERVABLE: frozenset[str] = frozenset(
    ("visualization.html", "events.ndjson") + VIZ_ARTIFACTS
)

_CONTENT_TYPES = {
    ".html": "text/html; charset=utf-8",
    ".json": "application/json; charset=utf-8",
    ".ndjson": "application/x-ndjson; charset=utf-8",
    ".md": "text/markdown; charset=utf-8",
    ".log": "text/plain; charset=utf-8",
    ".yaml": "text/plain; charset=utf-8",
    ".toml": "text/plain; charset=utf-8",
}


class VizServer:
    """Daemon-thread HTTP server rooted at ``run_dir``."""

    def __init__(self, run_dir: Path, port: int = 8765):
        self.run_dir = run_dir.resolve()
        self.requested_port = port
        self.port: Optional[int] = None
        self.url: Optional[str] = None
        self._httpd: Optional[ThreadingHTTPServer] = None
        self._thread: Optional[threading.Thread] = None

    def start(self) -> str:
        """Bind (falling back to an ephemeral port if busy) and return the URL."""
        run_dir = self.run_dir

        class _Handler(_AllowlistHandler):
            root = run_dir

        handler = _Handler
        port = self.requested_port
        last_err: Optional[OSError] = None
        for candidate in (port, 0):  # 0 = OS-assigned if preferred port is taken
            try:
                httpd = ThreadingHTTPServer(("127.0.0.1", candidate), handler)
                break
            except OSError as e:
                last_err = e
                httpd = None  # type: ignore[assignment]
        else:
            raise RuntimeError(f"could not bind viz server: {last_err}")

        self._httpd = httpd
        self.port = httpd.server_address[1]
        self.url = f"http://127.0.0.1:{self.port}/visualization.html"
        self._thread = threading.Thread(
            target=httpd.serve_forever, name="alf-viz-http", daemon=True)
        self._thread.start()
        return self.url

    def stop(self) -> None:
        if self._httpd is None:
            return
        try:
            self._httpd.shutdown()
        except Exception:  # noqa: BLE001 — best-effort teardown
            pass
        try:
            self._httpd.server_close()
        except Exception:  # noqa: BLE001
            pass
        self._httpd = None
        if self._thread is not None:
            self._thread.join(timeout=2)
            self._thread = None


def servable_path(root: Path, request_path: str) -> Optional[Path]:
    """Resolve a request path to a servable file, or ``None``.

    The request name is looked up in [`SERVABLE`] *literally* — it is never
    joined into a filesystem path unless it is a known-good member — so `..`,
    absolute paths, URL-encoded traversal, and symlink games cannot reach
    anything else. Returns ``None`` for anything unknown (→ 404) and for a
    member that simply does not exist in this run."""
    name = request_path.split("?", 1)[0].split("#", 1)[0].lstrip("/")
    if name in ("", "/"):
        name = "visualization.html"
    if name not in SERVABLE:
        return None
    candidate = root / name
    return candidate if candidate.is_file() else None


class _AllowlistHandler(BaseHTTPRequestHandler):
    """Serves only [`SERVABLE`], redacted; no listings, no other paths."""

    root: Path = Path(".")
    server_version = "alf-viz/1.1"

    def log_message(self, format, *args):  # noqa: A003 — stdlib signature
        return  # no per-request access-log spam in the driver terminal

    def do_GET(self):  # noqa: N802 — stdlib signature
        path = servable_path(self.root, self.path)
        if path is None:
            self.send_error(404, "Not found")
            return
        try:
            raw = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            self.send_error(404, "Not found")
            return
        # The page itself is our own template (no run data); everything else is
        # run output and goes through the central redactor before it leaves the
        # process — the same guarantee bake.py gives the portable artifact.
        body = raw if path.name == "visualization.html" else redact.redact(raw)
        encoded = body.encode("utf-8")
        ctype = _CONTENT_TYPES.get(path.suffix, "text/plain; charset=utf-8")
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(encoded)))
        # Live polling of events.ndjson must not be cached by the browser.
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(encoded)


def port_free(port: int, host: str = "127.0.0.1") -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            s.bind((host, port))
            return True
        except OSError:
            return False
