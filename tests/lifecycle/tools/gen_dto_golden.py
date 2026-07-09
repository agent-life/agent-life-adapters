#!/usr/bin/env python3
"""Regenerate the OpenClaw memory-DTO golden (WP-M4 task 3).

Exports the committed OpenClaw fixture with the built alf, reduces its archive
memory records to the DTO structural fingerprint (`alflab.dto_parity`), and
writes `frameworks/generic/openclaw-memory-dto.golden.json`. Run once when the
MemoryRecord/DTO shape legitimately changes (a data-format train), never per CI.

    python3 tests/lifecycle/tools/gen_dto_golden.py
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

LIFECYCLE_DIR = Path(__file__).resolve().parents[1]
REPO_ROOT = LIFECYCLE_DIR.parents[1]
sys.path.insert(0, str(LIFECYCLE_DIR))

from alflab import dto_parity  # noqa: E402

OPENCLAW_FIXTURE = REPO_ROOT / "scripts" / "fixtures" / "openclaw-workspace"
GOLDEN = LIFECYCLE_DIR / "frameworks" / "generic" / "openclaw-memory-dto.golden.json"


def find_alf() -> Path:
    for c in (REPO_ROOT / "target" / "x86_64-unknown-linux-musl" / "release" / "alf",
              REPO_ROOT / "target" / "release" / "alf"):
        if c.is_file():
            return c
    sys.exit("no alf binary built (cargo build --release -p alf-cli)")


def archive_records(archive: Path) -> list:
    z = zipfile.ZipFile(archive)
    return [json.loads(line)
            for name in z.namelist()
            if name.startswith("memory/") and name.endswith(".jsonl")
            for line in z.read(name).decode("utf-8").splitlines() if line.strip()]


def main() -> None:
    alf = find_alf()
    with tempfile.TemporaryDirectory(prefix="alf-dto-golden-") as tmp:
        tmp = Path(tmp)
        env = dict(os.environ)
        env["ALF_HOME"] = str(tmp / "alfhome")
        (tmp / "alfhome").mkdir()
        archive = tmp / "openclaw.alf"
        subprocess.run(
            [str(alf), "export", "-r", "openclaw", "-w", str(OPENCLAW_FIXTURE),
             "-o", str(archive)],
            env=env, check=True, capture_output=True)
        recs = archive_records(archive)

    golden = {
        "_comment": ("OpenClaw memory-DTO golden (WP-M4 task 3): the structural "
                     "fingerprint of GET /v1/agents/:id/memory (MemoryRecordDto), "
                     "captured OFFLINE from an OpenClaw alf-export archive. The generic "
                     "runtime archive must project to this same shape (design F2: the "
                     "indexer is runtime-blind). Regenerate with tools/gen_dto_golden.py."),
        "captured_from": "scripts/fixtures/openclaw-workspace (alf export -r openclaw)",
        "profile": dto_parity.shape_profile(recs),
        "sample_dto": dto_parity.project(recs[0]),
        "validity_issues": dto_parity.dashboard_validity(recs),
    }
    GOLDEN.write_text(json.dumps(golden, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {GOLDEN} from {len(recs)} OpenClaw records")


if __name__ == "__main__":
    main()
