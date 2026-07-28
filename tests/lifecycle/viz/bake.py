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
import re
import sys
from pathlib import Path

VIZ_DIR = Path(__file__).resolve().parent
TEMPLATE = VIZ_DIR / "index.html"

# The harness redacts events at emit time, but ARTIFACTS are raw file bytes —
# a proxy-tier run's home/config.yaml holds the minted runtime key verbatim
# (MAJ-10). Everything inlined below goes through the central redaction,
# seeded with the exact secret values the run dir itself recorded, and the
# bake REFUSES to write a share file that still carries one.
sys.path.insert(0, str(VIZ_DIR.parent))
from alflab import redact as _redact  # noqa: E402

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


def harvest_run_secrets(run_dir: Path) -> list[str]:
    """Exact secret values this run recorded on purpose (its 0600 stores):
    every KEY/TOKEN/SECRET/PASSWORD-named value in run.env + env-files/*.env.
    Registered with the redactor for exact-value (and truncated-prefix)
    scrubbing — pattern shapes alone cannot know a foreign key format."""
    values: list[str] = []
    candidates = [run_dir / "run.env"]
    env_dir = run_dir / "env-files"
    if env_dir.is_dir():
        candidates.extend(sorted(env_dir.glob("*.env")))
    for path in candidates:
        if not path.is_file():
            continue
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            if "=" not in line or line.lstrip().startswith("#"):
                continue
            key, value = line.split("=", 1)
            if re.search(r"KEY|TOKEN|SECRET|PASSWORD", key, re.I):
                value = value.strip().strip('"').strip("'")
                if len(value) >= 12:
                    values.append(value)
                    _redact.register_secret(value)
    return values


def assert_no_secrets(html: str, secrets: list[str]) -> None:
    """Refuse to write a customer-share file that still carries a run secret
    (belt and braces over the redaction pass — a leak here is worse than no
    bake at all)."""
    for value in secrets:
        if value in html:
            raise SystemExit(
                "bake: refusing to write — a run secret survived redaction "
                "(value from run.env/env-files)"
            )
    if re.search(r"alf_[A-Za-z0-9]{32}", html):
        raise SystemExit(
            "bake: refusing to write — a runtime-key-shaped token (alf_<32 "
            "alnum>) survived redaction"
        )


def load_artifacts(run_dir: Path) -> dict:
    arts = {}
    for rel in ARTIFACT_CANDIDATES:
        p = run_dir / rel
        if not p.is_file():
            continue
        if p.stat().st_size > MAX_ARTIFACT_BYTES:
            continue
        try:
            arts[rel] = _redact.redact(p.read_text(encoding="utf-8", errors="replace"))
        except OSError:
            continue
    return arts


def bake(run_dir: Path, out: Path) -> None:
    secrets = harvest_run_secrets(run_dir)
    # Events/report are emit-time-redacted already; redacting again is
    # idempotent and additionally covers backfilled streams + the freshly
    # harvested exact values.
    events = _redact.redact_obj(load_events(run_dir))
    report = None
    rp = run_dir / "report.json"
    if rp.is_file():
        report = _redact.redact_obj(json.loads(rp.read_text(encoding="utf-8")))
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
    assert_no_secrets(html, secrets)
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
