"""Dashboard memory-DTO parity: generic vs OpenClaw (WP-M4 task 3).

Proves the generic runtime renders identically in the dashboard's Memory tab by
showing its archive memory records project to the SAME `GET /v1/agents/:id/memory`
shape as OpenClaw's — fully offline, from `alf export` archives, because the
indexer/DTO are runtime-blind (design F2). The OpenClaw side is a committed
golden (`frameworks/generic/openclaw-memory-dto.golden.json`, captured once via
`tools/gen_dto_golden.py`); the generic side is exported live in the test.

Tier: OFFLINE (this test). The live `GET /v1/agents/:id/memory` confirmation is
scheduled once on the backend tier and holds by F2 — see the WP-M4 handoff. Skips
when no alf is built (bare GitHub CI); runs under `./test.sh` / locally.

Run:  python3 -m unittest tests.lifecycle.test_dto_parity
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from alflab import dto_parity, scenario  # noqa: E402

LIFECYCLE_DIR = Path(__file__).resolve().parent
REPO_ROOT = LIFECYCLE_DIR.parents[1]
GENERIC_FIXTURE = LIFECYCLE_DIR / "frameworks" / "generic" / "fixture"
GOLDEN = LIFECYCLE_DIR / "frameworks" / "generic" / "openclaw-memory-dto.golden.json"
sys.path.insert(0, str(LIFECYCLE_DIR / "frameworks" / "generic"))


def _find_alf():
    for c in (REPO_ROOT / "target" / "x86_64-unknown-linux-musl" / "release" / "alf",
              REPO_ROOT / "target" / "release" / "alf"):
        if c.is_file():
            return c
    return None


@unittest.skipUnless(_find_alf(), "no alf-under-test built (run cargo build -p alf-cli)")
class DtoParityTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        import seed_markers as seeder
        cls.tmp = Path(tempfile.mkdtemp(prefix="alf-m4-dto-"))
        ws = cls.tmp / "ws"
        shutil.copytree(GENERIC_FIXTURE, ws)
        # Seed episodic markers so the generic archive spans all three memory
        # types (semantic knowledge + procedural + seeded episodic).
        seeder.seed_round(ws, "default", scenario.turns("default", 1))
        env = dict(os.environ)
        env["ALF_HOME"] = str(cls.tmp / "alfhome")
        (cls.tmp / "alfhome").mkdir()
        archive = cls.tmp / "generic.alf"
        subprocess.run([str(_find_alf()), "export", "-r", "generic", "-w", str(ws),
                        "-o", str(archive)], env=env, check=True, capture_output=True)
        z = zipfile.ZipFile(archive)
        cls.records = [json.loads(line)
                       for name in z.namelist()
                       if name.startswith("memory/") and name.endswith(".jsonl")
                       for line in z.read(name).decode("utf-8").splitlines() if line.strip()]
        cls.golden = json.loads(GOLDEN.read_text(encoding="utf-8"))

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp, ignore_errors=True)

    def test_generic_archive_has_records_across_the_canonical_types(self):
        types = {r["memory_type"] for r in self.records}
        self.assertEqual(types, {"semantic", "episodic", "procedural"},
                         f"got {sorted(types)}")

    def test_dto_shape_matches_the_openclaw_golden(self):
        generic = dto_parity.shape_profile(self.records)
        golden = self.golden["profile"]
        # The DTO field/type shape + the two nested key sets must be identical —
        # so GET /agents/:id/memory is field-for-field the same for both runtimes.
        self.assertEqual(generic["dto_keys"], golden["dto_keys"])
        self.assertEqual(generic["field_types"], golden["field_types"],
                         "generic vs OpenClaw DTO field types diverge")
        self.assertEqual(generic["source_keys"], golden["source_keys"])
        self.assertEqual(generic["raw_source_format_keys"],
                         golden["raw_source_format_keys"])

    def test_generic_records_are_dashboard_clean(self):
        # Generic controls its own map, so unlike OpenClaw (which legitimately
        # uses non-chip namespaces) every generic record must be renderable:
        # canonical type, chip namespace, content, origin_file, line_start.
        issues = dto_parity.dashboard_validity(self.records)
        self.assertEqual(issues, [], f"generic dashboard-render issues: {issues}")

    def test_every_record_projects_to_the_full_dto(self):
        for r in self.records:
            dto = dto_parity.project(r)
            for key in ("id", "memory_type", "content", "created_at_alf",
                        "updated_at_alf", "tags", "source"):
                self.assertIsNotNone(dto[key], f"record {r.get('id')} missing {key}")
            self.assertIn("origin_file", dto["source"])


if __name__ == "__main__":
    unittest.main()
