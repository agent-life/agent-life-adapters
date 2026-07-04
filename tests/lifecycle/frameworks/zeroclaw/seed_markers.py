"""ZeroClaw no-LLM seeder — real-DDL INSERTs into brain.db (WP2, D9).

Runs HOST-SIDE against the bind-mounted framework home (the container is idle
between execs; sqlite is safe to write). The schema is the REAL captured DDL
(adapter-zeroclaw/testkit/captured/brain.db.schema.sql):

  * `agents` row ensured first (INSERT OR IGNORE id+alias) — memories carry a
    NOT NULL FK to it and UNIQUE(agent_id, key);
  * `embedding` stays NULL (embedding_provider = "none");
  * timestamps are RFC3339;
  * FTS5 is a shadow table maintained by triggers — NEVER written directly;
    plain INSERTs into `memories` fire the triggers.

Categories use ZeroClaw's real taxonomy via scenario.ZEROCLAW_CATEGORY
(core / episodic / procedure / credentials).
"""

from __future__ import annotations

import sqlite3
import uuid
from datetime import datetime, timezone
from pathlib import Path

DB_RELPATH = Path("data") / "memory" / "brain.db"


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def slot_agent_id(slot: str) -> str:
    """Deterministic agent id for seeded rows (uuid5 — stable across runs)."""
    return str(uuid.uuid5(uuid.NAMESPACE_URL, f"alf-lifecycle:zeroclaw:{slot}"))


def ensure_agent(db_path: Path, slot: str) -> str:
    """INSERT OR IGNORE the agents row; adopt an existing sole row's id if the
    framework already created one (e.g. after real LLM turns)."""
    conn = sqlite3.connect(db_path)
    try:
        rows = conn.execute("SELECT id, alias FROM agents").fetchall()
        for row_id, alias in rows:
            if alias == slot:
                return row_id
        if len(rows) == 1 and slot == "default":
            return rows[0][0]  # sole implicit agent — adopt
        agent_id = slot_agent_id(slot)
        conn.execute(
            "INSERT OR IGNORE INTO agents (id, alias, created_at) VALUES (?, ?, ?)",
            (agent_id, slot, _now()),
        )
        conn.commit()
        return agent_id
    finally:
        conn.close()


def seed_round(db_path: Path, slot: str, turns) -> int:
    """Insert one category-correct row per scenario turn. Idempotent: the
    UNIQUE(agent_id, key) constraint is respected via INSERT OR IGNORE.
    Returns the number of rows present for the slot afterwards."""
    from alflab.scenario import ZEROCLAW_CATEGORY  # single source of truth

    agent_id = ensure_agent(db_path, slot)
    conn = sqlite3.connect(db_path)
    try:
        for turn in turns:
            key = f"alf_lifecycle_{turn.turn_type}_r{turn.round}"
            content = (
                f"[{turn.turn_type} r{turn.round}] {turn.prompt[:120]} "
                f"(verbatim marker: {turn.marker})"
            )
            now = _now()
            conn.execute(
                "INSERT OR IGNORE INTO memories "
                "(id, key, content, category, embedding, created_at, updated_at, "
                " session_id, namespace, importance, agent_id) "
                "VALUES (?, ?, ?, ?, NULL, ?, ?, NULL, 'default', 0.5, ?)",
                (str(uuid.uuid4()), key, content,
                 ZEROCLAW_CATEGORY[turn.turn_type], now, now, agent_id),
            )
        conn.commit()
        return conn.execute(
            "SELECT COUNT(*) FROM memories WHERE agent_id = ?", (agent_id,)
        ).fetchone()[0]
    finally:
        conn.close()
