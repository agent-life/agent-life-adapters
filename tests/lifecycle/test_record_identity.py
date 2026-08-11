"""Self-tests for the archive identity oracles (the BLK-1 class).

`archives.record_identity` must flag duplicate record ids across an archive's
memory partitions, and `archives.marker_record_counts` must pair exact content
probes with the records that carry them. Both are EXTERNAL oracles: a
deterministic id-preimage collision survives every self-comparison check
(round-trip, Z13' byte-equality), so only an invariant like this can see it.

Run:  python3 -m unittest tests.lifecycle.test_record_identity
"""

from __future__ import annotations

import json
import tempfile
import unittest
import zipfile
from pathlib import Path

from alflab import archives


def _write_archive(path: Path, partitions: dict) -> None:
    """Minimal .alf stand-in: `partitions` is {entry_name: [record dicts]}."""
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("manifest.json", json.dumps({"alf_version": "1.1.0"}))
        for name, records in partitions.items():
            z.writestr(name, "\n".join(json.dumps(r) for r in records) + "\n")


class RecordIdentityTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.path = Path(self._tmp.name) / "t.alf"

    def tearDown(self):
        self._tmp.cleanup()

    def test_distinct_ids_are_clean(self):
        _write_archive(self.path, {
            "memory/2026-Q1.jsonl": [
                {"id": "aaa", "content": "alpha"},
                {"id": "bbb", "content": "beta"},
            ],
        })
        ident = archives.record_identity(self.path)
        self.assertEqual(ident["total"], 2)
        self.assertEqual(ident["unique"], 2)
        self.assertEqual(ident["duplicates"], {})

    def test_duplicate_ids_within_a_partition_are_flagged(self):
        _write_archive(self.path, {
            "memory/2026-Q1.jsonl": [
                {"id": "aaa", "content": "file A row 1"},
                {"id": "aaa", "content": "file B row 1 (collided)"},
            ],
        })
        ident = archives.record_identity(self.path)
        self.assertEqual(ident["duplicates"], {"aaa": 2})

    def test_duplicate_ids_across_partitions_are_flagged(self):
        _write_archive(self.path, {
            "memory/2025-Q4.jsonl": [{"id": "aaa", "content": "old"}],
            "memory/2026-Q1.jsonl": [{"id": "aaa", "content": "new"}],
        })
        ident = archives.record_identity(self.path)
        self.assertEqual(ident["total"], 2)
        self.assertEqual(ident["duplicates"], {"aaa": 2})

    def test_non_memory_entries_are_ignored(self):
        _write_archive(self.path, {
            "memory/2026-Q1.jsonl": [{"id": "aaa", "content": "alpha"}],
        })
        with zipfile.ZipFile(self.path, "a") as z:
            z.writestr("raw/generic/brain.db", "not jsonl")
        self.assertEqual(archives.record_identity(self.path)["total"], 1)


class MarkerRecordCountsTest(unittest.TestCase):
    def test_counts_are_per_record_not_per_occurrence(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "t.alf"
            _write_archive(path, {
                "memory/2026-Q1.jsonl": [
                    {"id": "a", "content": "PROBE-1 and PROBE-1 again"},
                    {"id": "b", "content": "PROBE-2"},
                    {"id": "c", "content": "PROBE-2 elsewhere"},
                ],
            })
            counts = archives.marker_record_counts(
                path, ["PROBE-1", "PROBE-2", "PROBE-3"])
            # 1:1, multiply-minted, and lost — the three verdicts the Z13'
            # probe check distinguishes.
            self.assertEqual(counts, {"PROBE-1": 1, "PROBE-2": 2, "PROBE-3": 0})


if __name__ == "__main__":
    unittest.main()
