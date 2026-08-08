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
        (with the correct platform filename, so it reads as a genuine mismatch)
      → if ?missing-checksum=1 on .sha256 requests, return 404
      → if ?empty-checksum=1 on .sha256 requests, return an empty body
      → if ?dup-checksum=1 on .sha256 requests, return two lines that both name
        the platform binary (ambiguous / unusable)

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

        def send_file(self, filepath, bad_checksum=False,
                      missing_checksum=False, empty_checksum=False,
                      dup_checksum=False):
            is_checksum = filepath.endswith(".sha256")
            # The binary filename this checksum file is for (e.g. alf-linux-amd64).
            bin_name = os.path.basename(filepath)[: -len(".sha256")] if is_checksum else ""

            # Simulate a missing .sha256: 404 even though the fixture exists.
            if is_checksum and missing_checksum:
                self.send_response(404)
                self.end_headers()
                self.wfile.write(b"Not found")
                return

            if not os.path.isfile(filepath):
                self.send_response(404)
                self.end_headers()
                self.wfile.write(b"Not found")
                return

            # Simulate an empty .sha256.
            if is_checksum and empty_checksum:
                self.send_response(200)
                self.send_header("Content-Type", "text/plain")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return

            if is_checksum and bad_checksum:
                # Deliberately wrong hash, but with the correct platform filename
                # so the installer reads it as a genuine mismatch (not a
                # wrong-file rejection).
                body = ("0" * 64 + "  " + bin_name + "\n").encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/plain")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return

            if is_checksum and dup_checksum:
                # Two lines both naming the platform binary → ambiguous.
                body = ("a" * 64 + "  " + bin_name + "\n"
                        + "b" * 64 + "  " + bin_name + "\n").encode()
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
            missing_checksum = "missing-checksum" in qs and qs["missing-checksum"][0] == "1"
            empty_checksum = "empty-checksum" in qs and qs["empty-checksum"][0] == "1"
            dup_checksum = "dup-checksum" in qs and qs["dup-checksum"][0] == "1"

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
                self.send_file(os.path.join(fixtures_dir, filename), bad_checksum,
                               missing_checksum, empty_checksum, dup_checksum)
                return

            # Alternative path: /releases/latest/<filename>  (used by some scripts)
            m = re.match(r"^/releases/latest/(.+)$", path)
            if m:
                filename = m.group(1)
                self.send_file(os.path.join(fixtures_dir, filename), bad_checksum,
                               missing_checksum, empty_checksum, dup_checksum)
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
