"""SQLite helpers for the ZeroClaw shared-store assertions (WP3).

`brain.db` is one store shared by every agent, partitioned by `agent_id`. The
Z12 restore assertion needs to prove that restoring ONE agent's slice leaves
every OTHER agent's rows byte-identical. `db_identical_except_agent` compares two
`brain.db` snapshots (before/after a restore) and returns True only when every
non-target row — other agents' memories, and the untouched `agents` /
`embedding_cache` / `schema_version` tables — is unchanged.
"""

from __future__ import annotations

import sqlite3
from pathlib import Path

_MEM_COLS = (
    "id, key, content, category, embedding, created_at, updated_at, "
    "session_id, namespace, importance, superseded_by, agent_id"
)


def slot_agent_id(db_path: Path, slot: str) -> str | None:
    """Resolve the `agents.id` for `slot` (by alias, adopting a sole row)."""
    conn = sqlite3.connect(db_path)
    try:
        rows = conn.execute("SELECT id, alias FROM agents").fetchall()
    finally:
        conn.close()
    for rid, alias in rows:
        if alias == slot:
            return rid
    if len(rows) == 1:
        return rows[0][0]
    return None


def _rows(conn, sql, params=()) -> list:
    return conn.execute(sql, params).fetchall()


def _memories_except(conn, agent_id: str | None) -> list:
    if agent_id is None:
        return _rows(conn, f"SELECT {_MEM_COLS} FROM memories ORDER BY agent_id, id")
    return _rows(
        conn,
        f"SELECT {_MEM_COLS} FROM memories WHERE agent_id != ? ORDER BY agent_id, id",
        (agent_id,),
    )


def db_identical_except_agent(before: Path, after: Path, slot: str) -> tuple[bool, str]:
    """True iff every row that does NOT belong to `slot` is byte-identical
    between `before` and `after`. Returns (ok, human-readable detail)."""
    cb = sqlite3.connect(before)
    ca = sqlite3.connect(after)
    try:
        # Resolve from `after` (the alias always exists post-restore), then `before`.
        sid = slot_agent_id(after, slot) or slot_agent_id(before, slot)

        ob = _memories_except(cb, sid)
        oa = _memories_except(ca, sid)
        if ob != oa:
            return (
                False,
                f"other-agent memory rows changed ({len(ob)} → {len(oa)} rows; slot id={sid})",
            )

        # `embedding_cache` and `schema_version` are global and must be untouched.
        for table, order in (("embedding_cache", "content_hash"), ("schema_version", "component")):
            b = _rows(cb, f"SELECT * FROM {table} ORDER BY {order}")
            a = _rows(ca, f"SELECT * FROM {table} ORDER BY {order}")
            if b != a:
                return False, f"{table} changed by the restore"

        # The `agents` table may gain the slot's own row (restore-creates-alias);
        # every OTHER agent row must be unchanged.
        ab = [r for r in _rows(cb, "SELECT id, alias, created_at FROM agents ORDER BY id") if r[0] != sid]
        aa = [r for r in _rows(ca, "SELECT id, alias, created_at FROM agents ORDER BY id") if r[0] != sid]
        if ab != aa:
            return False, "other agents' rows in the `agents` table changed"

        return True, f"other agents byte-identical (slot '{slot}' id={sid}, {len(oa)} foreign rows)"
    finally:
        cb.close()
        ca.close()


def agent_row_count(db_path: Path, slot: str) -> int:
    """Rows in `memories` for `slot` (0 when the store/alias is absent)."""
    if not Path(db_path).is_file():
        return 0
    sid = slot_agent_id(db_path, slot)
    if sid is None:
        return 0
    conn = sqlite3.connect(db_path)
    try:
        return conn.execute(
            "SELECT COUNT(*) FROM memories WHERE agent_id = ?", (sid,)
        ).fetchone()[0]
    finally:
        conn.close()
