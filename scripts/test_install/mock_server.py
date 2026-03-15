#!/usr/bin/env python3
"""
Mock HTTP server simulating GitHub releases for install.sh testing.

Usage: python3 mock_server.py <port> <fixtures_dir>

URL patterns served:
  GET /repos/{owner}/{repo}/releases/latest
      → {"tag_name": "v0.0.0-test"}

  GET /releases/download/v0.0.0-test/<filename>
  GET /releases/latest/<filename>
      → serve file from <fixtures_dir>/<filename>
      → if ?bad_checksum=1 on .sha256 requests, return a wrong hash

Any other path or file not found → 404.

Writes "READY <port>" to stdout on startup so the test runner knows it's up.
"""

import http.server
import os
import sys
import json
import re
from urllib.parse import urlparse, parse_qs


def main():
    if len(sys.argv) != 3:
        print(f"Usage: {sys.argv[0]} <port> <fixtures_dir>", file=sys.stderr)
        sys.exit(1)

    port = int(sys.argv[1])
    fixtures_dir = os.path.abspath(sys.argv[2])

    if not os.path.isdir(fixtures_dir):
        print(f"Error: fixtures_dir '{fixtures_dir}' does not exist", file=sys.stderr)
        sys.exit(1)

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, fmt, *args):
            # Suppress default access log; write structured messages to stderr
            sys.stderr.write(f"[mock] {self.address_string()} {fmt % args}\n")

        def send_json(self, data, status=200):
            body = json.dumps(data).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def send_file(self, filepath, bad_checksum=False):
            if not os.path.isfile(filepath):
                self.send_response(404)
                self.end_headers()
                self.wfile.write(b"Not found")
                return

            if bad_checksum and filepath.endswith(".sha256"):
                # Return a deliberately wrong hash
                body = b"0000000000000000000000000000000000000000000000000000000000000000  fake\n"
                self.send_response(200)
                self.send_header("Content-Type", "text/plain")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return

            with open(filepath, "rb") as f:
                data = f.read()
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            parsed = urlparse(self.path)
            path = parsed.path
            qs = parse_qs(parsed.query)
            bad_checksum = "bad_checksum" in qs and qs["bad_checksum"][0] == "1"

            # GitHub API: GET /repos/{owner}/{repo}/releases/latest
            if re.match(r"^/repos/[^/]+/[^/]+/releases/latest$", path):
                self.send_json({"tag_name": "v0.0.0-test"})
                return

            # Binary/checksum download: /releases/download/<tag>/<filename>
            # Only serves files for the test version tag (v0.0.0-test).
            m = re.match(r"^/releases/download/([^/]+)/(.+)$", path)
            if m:
                tag, filename = m.group(1), m.group(2)
                if tag != "v0.0.0-test":
                    self.send_response(404)
                    self.end_headers()
                    self.wfile.write(f"No release found for tag {tag}".encode())
                    return
                self.send_file(os.path.join(fixtures_dir, filename), bad_checksum)
                return

            # Alternative path: /releases/latest/<filename>  (used by some scripts)
            m = re.match(r"^/releases/latest/(.+)$", path)
            if m:
                filename = m.group(1)
                self.send_file(os.path.join(fixtures_dir, filename), bad_checksum)
                return

            self.send_response(404)
            self.end_headers()
            self.wfile.write(b"Not found")

    server = http.server.HTTPServer(("0.0.0.0", port), Handler)
    # Signal readiness to the parent process
    print(f"READY {port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
