#!/usr/bin/env python3
"""Oracle: prove a REAL Hermes can open and read a (rebuilt) ``state.db``.

Phase 0 acceptance gate. This opens the database with Hermes's own
``SessionDB`` — the exact code the live agent uses — and exercises the read
paths that matter for a restored agent:

  * list sessions (incl. children + archived)
  * read each session's messages
  * FTS5 keyword search (``messages_fts``)
  * trigram substring search for CJK (``messages_fts_trigram``)

If the Rust rebuild produced a structurally-faithful DB, every check passes
with NO schema migration/repair being triggered. Exit 0 = gate passed.

Usage:
    PYTHONPATH=/path/to/hermes-agent python3 verify_open.py <db> [expected_sessions]
"""
import json
import sys
from pathlib import Path

from hermes_state import SessionDB


def main() -> int:
    db_path = Path(sys.argv[1])
    expected = int(sys.argv[2]) if len(sys.argv) > 2 else 3

    # Open READ-WRITE, exactly as a real restored Hermes agent does (it writes
    # new sessions continuously). NB: read_only=True disables Hermes's FTS5
    # capability probe — which creates a temp virtual table — and would make
    # search a silent no-op; that is a test artifact, not a rebuild property.
    db = SessionDB(db_path)

    sessions = db.list_sessions_rich(
        limit=100, include_children=True, include_archived=True
    )
    by_id = {s["id"]: s for s in sessions}

    failures = []
    if len(sessions) != expected:
        failures.append(f"expected {expected} sessions, got {len(sessions)}")

    # Lineage must survive the round-trip.
    child = by_id.get("20260101_130000_bbbb")
    if not child or child.get("parent_session_id") != "20260101_120000_aaaa":
        failures.append("parent_session_id lineage not preserved on B→A")

    # Messages readable per session.
    total_messages = 0
    for sid in by_id:
        msgs = db.get_messages(sid)
        total_messages += len(msgs)
        if not msgs:
            failures.append(f"session {sid} has no readable messages")

    # FTS5 keyword search must work against the rebuilt index.
    hits = db.search_messages("retry", limit=10)
    if not hits:
        failures.append("FTS5 search for 'retry' returned no hits")

    # Trigram substring search (CJK) — exercises messages_fts_trigram. The
    # term must be a contiguous ≥3-char substring of the seeded message
    # "好的，我来起草发布说明。" (trigram can't match shorter/non-contiguous CJK).
    cjk = db.search_messages("起草发", limit=10)
    if not cjk:
        failures.append("trigram search for CJK '起草发' returned no hits")

    summary = {
        "ok": not failures,
        "sessions": len(sessions),
        "messages": total_messages,
        "fts_keyword_hits": len(hits),
        "fts_trigram_hits": len(cjk),
        "failures": failures,
    }
    print(json.dumps(summary, indent=2, ensure_ascii=False))
    return 0 if not failures else 1


if __name__ == "__main__":
    raise SystemExit(main())
