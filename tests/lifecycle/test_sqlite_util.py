"""Unit tests for alflab.sqlite_util.db_identical_except_agent (the Z12 oracle).

Builds real-schema brain.db snapshots (the committed captured DDL) with two
agents sharing the store and asserts the helper isolates one agent's slice.
"""

from __future__ import annotations

import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from alflab import sqlite_util  # noqa: E402

_SCHEMA = (
    Path(__file__).resolve().parents[2]
    / "adapter-zeroclaw"
    / "testkit"
    / "captured"
    / "brain.db.schema.sql"
).read_text(encoding="utf-8")

A = "aaaaaaaa-0000-0000-0000-0000000000a1"
B = "bbbbbbbb-0000-0000-0000-0000000000b2"


def _build(path: Path, rows: list[tuple[str, str, str]]) -> None:
    """rows: (agent_id, key, content). Agents A/B are always created."""
    conn = sqlite3.connect(path)
    conn.executescript(_SCHEMA)
    for aid, alias in ((A, "agent_a"), (B, "agent_b")):
        conn.execute(
            "INSERT INTO agents (id, alias, created_at) VALUES (?, ?, '2026-01-01T00:00:00Z')",
            (aid, alias),
        )
    for aid, key, content in rows:
        conn.execute(
            "INSERT INTO memories (id, key, content, category, embedding, created_at, "
            "updated_at, session_id, namespace, importance, superseded_by, agent_id) "
            "VALUES (?, ?, ?, 'core', NULL, '2026-01-01T00:00:00Z', "
            "'2026-01-01T00:00:00Z', NULL, 'default', 0.5, NULL, ?)",
            (f"{aid}:{key}", key, content, aid),
        )
    conn.commit()
    conn.close()


class DbIdenticalExceptAgentTests(unittest.TestCase):
    def test_ok_when_only_target_slice_changes(self):
        d = Path(tempfile.mkdtemp())
        _build(d / "before.db", [(A, "k1", "a one"), (A, "k2", "a two"), (B, "k1", "b one")])
        # agent_a's slice mutated (rewrite + delete); agent_b untouched.
        _build(d / "after.db", [(A, "k1", "a CHANGED"), (B, "k1", "b one")])
        ok, detail = sqlite_util.db_identical_except_agent(
            d / "before.db", d / "after.db", "agent_a"
        )
        self.assertTrue(ok, detail)

    def test_fails_when_other_agent_changes(self):
        d = Path(tempfile.mkdtemp())
        _build(d / "before.db", [(A, "k1", "a one"), (B, "k1", "b one")])
        _build(d / "after.db", [(A, "k1", "a one"), (B, "k1", "b TAMPERED")])
        ok, detail = sqlite_util.db_identical_except_agent(
            d / "before.db", d / "after.db", "agent_a"
        )
        self.assertFalse(ok, detail)

    def test_agent_row_count(self):
        d = Path(tempfile.mkdtemp())
        _build(d / "db.db", [(A, "k1", "x"), (A, "k2", "y"), (B, "k1", "z")])
        self.assertEqual(sqlite_util.agent_row_count(d / "db.db", "agent_a"), 2)
        self.assertEqual(sqlite_util.agent_row_count(d / "db.db", "agent_b"), 1)


if __name__ == "__main__":
    unittest.main()
