#!/usr/bin/env python3
"""Seed a realistic Hermes ``state.db`` using Hermes's OWN storage code.

Phase 0 spike fixture. This imports ``hermes_state.SessionDB`` from a checkout
of NousResearch/hermes-agent and creates sessions/messages through the real
write path — so the resulting DB has the authentic schema (currently version
16) with FTS5 + trigram indexes populated by the real triggers. No LLM / API
key is needed: we drive ``create_session`` / ``append_message`` directly.

The DB it produces is the INPUT to the Rust rebuild spike, and the oracle in
``verify_open.py`` later opens the *rebuilt* DB with this same SessionDB code
to prove a real Hermes can read it.

Usage:
    PYTHONPATH=/path/to/hermes-agent python3 seed.py <out.db>
"""
import os
import sys
import time
from pathlib import Path

from hermes_state import SessionDB, SCHEMA_VERSION


def main() -> int:
    out = Path(sys.argv[1] if len(sys.argv) > 1 else "state.db")
    for p in (out, Path(str(out) + "-wal"), Path(str(out) + "-shm")):
        if p.exists():
            p.unlink()

    db = SessionDB(out)

    # Session A — cli, ended. Has tool_calls (exercises the FTS trigger's
    # content||tool_name||tool_calls concatenation).
    a = "20260101_120000_aaaa"
    db.create_session(a, source="cli", model="claude-opus-4-8",
                      system_prompt="You are Hermes, a local-first agent.")
    db.set_session_title(a, "Investigate flaky sync")
    db.append_message(a, role="user", content="Why does sync fail intermittently?")
    db.append_message(a, role="assistant",
                      content="Let me grep the logs for retry markers.",
                      token_count=42, finish_reason="stop")
    db.append_message(a, role="tool", tool_name="grep",
                      tool_calls=[{"name": "grep", "args": {"pattern": "retry"}}],
                      content="3 matches in sync.log")
    db.end_session(a, end_reason="completed")

    # Session B — telegram, child of A via compression lineage, ended.
    b = "20260101_130000_bbbb"
    db.create_session(b, source="telegram", model="claude-opus-4-8",
                      parent_session_id=a)
    db.set_session_title(b, "Investigate flaky sync #2")
    db.append_message(b, role="user", content="Continue the investigation from before.")
    db.append_message(b, role="assistant",
                      content="The retries cluster around WAL checkpoint contention.")
    db.end_session(b, end_reason="compression")

    # Session C — cli, still ACTIVE (no end_session). Proves the delta story:
    # only new/active sessions should change between syncs.
    c = "20260102_090000_cccc"
    db.create_session(c, source="cli", model="claude-sonnet-4-6")
    db.set_session_title(c, "Draft release notes")
    db.append_message(c, role="user", content="Draft the 1.0 release notes please.")

    # Unicode / CJK content to exercise the trigram FTS table on rebuild.
    db.append_message(c, role="assistant", content="好的，我来起草发布说明。")

    print(f"seeded {out} (schema_version={SCHEMA_VERSION})", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
