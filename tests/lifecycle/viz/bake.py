#!/usr/bin/env python3
"""Bake a portable single-file visualization for customer sharing.

Usage:
  python3 tests/lifecycle/viz/bake.py <run-dir> [-o out.html]

Inlines events.ndjson, report.json, and small text artifacts into a copy of
index.html as ``window.__ALF_VIZ_DATA__``. The result opens via file:// with
no local server.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

VIZ_DIR = Path(__file__).resolve().parent
TEMPLATE = VIZ_DIR / "index.html"

# Small text artifacts worth inlining for the hybrid drawer.
ARTIFACT_CANDIDATES = [
    "report.json",
    "run-manifest.json",
    "mcp-interactions.log",
    "z16-serve-stderr.log",
    "home/config.yaml",
    "home/memories/MEMORY.md",
    "home/profiles/agent_b/config.yaml",
    "home/profiles/agent_b/SOUL.md",
    "alf-home/config.toml",
]

MAX_ARTIFACT_BYTES = 400_000


def load_events(run_dir: Path) -> list:
    path = run_dir / "events.ndjson"
    if not path.is_file():
        raise SystemExit(f"no events.ndjson in {run_dir} — run the harness or backfill.py first")
    out = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        out.append(json.loads(line))
    return out


def load_artifacts(run_dir: Path) -> dict:
    arts = {}
    for rel in ARTIFACT_CANDIDATES:
        p = run_dir / rel
        if not p.is_file():
            continue
        if p.stat().st_size > MAX_ARTIFACT_BYTES:
            continue
        try:
            arts[rel] = p.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
    return arts


def bake(run_dir: Path, out: Path) -> None:
    events = load_events(run_dir)
    report = None
    rp = run_dir / "report.json"
    if rp.is_file():
        report = json.loads(rp.read_text(encoding="utf-8"))
    artifacts = load_artifacts(run_dir)
    # report.json is also available as an artifact path for the drawer.
    payload = {
        "events": events,
        "report": report,
        "artifacts": artifacts,
        "baked_from": str(run_dir),
    }
    html = TEMPLATE.read_text(encoding="utf-8")
    blob = json.dumps(payload, ensure_ascii=False)
    inject = f"<script>window.__ALF_VIZ_DATA__ = {blob};</script>\n"
    if "</head>" in html:
        html = html.replace("</head>", inject + "</head>", 1)
    else:
        html = inject + html
    out.write_text(html, encoding="utf-8")
    print(f"wrote {out} ({out.stat().st_size:,} bytes, {len(events)} events)")


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("run_dir", type=Path)
    p.add_argument("-o", "--output", type=Path, default=None,
                   help="default: <run-dir>/visualization-portable.html")
    args = p.parse_args(argv)
    run_dir = args.run_dir.resolve()
    if not run_dir.is_dir():
        print(f"not a directory: {run_dir}", file=sys.stderr)
        return 2
    out = args.output or (run_dir / "visualization-portable.html")
    bake(run_dir, out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
