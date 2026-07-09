"""Background HTTP server for the lifecycle visualization.

Serves the run directory so ``visualization.html`` can fetch ``events.ndjson``
(and sibling artifacts) while the harness is still running. Default-on for
local runs; opt out with ``--no-viz-server``.
"""

from __future__ import annotations

import socket
import threading
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Optional


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
        handler = partial(_QuietHandler, directory=str(self.run_dir))
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


class _QuietHandler(SimpleHTTPRequestHandler):
    """No per-request access log spam into the driver terminal."""

    def log_message(self, format, *args):  # noqa: A003 — stdlib signature
        return

    def end_headers(self):
        # Live polling of events.ndjson must not be cached by the browser.
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


def port_free(port: int, host: str = "127.0.0.1") -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            s.bind((host, port))
            return True
        except OSError:
            return False
