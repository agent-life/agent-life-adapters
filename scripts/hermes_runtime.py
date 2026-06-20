"""Faithful Hermes runtime emulation for the integration walkthrough.

The Hermes branch of the walkthrough must exercise the adapter against
*authentic* Hermes data, not an approximation. So this module drives Hermes's
own storage layer — ``hermes_state.SessionDB`` from a checkout of
NousResearch/hermes-agent — to seed and mutate ``state.db`` (real schema v16,
real FTS5 + trigram triggers, real compression lineage), and to verify a
rebuilt ``state.db`` the same way a live agent would read it.

The checkout is located via ``$HERMES_AGENT_DIR``, an existing
``/tmp/hermes-agent``, or a shallow clone. Importing this module is cheap;
the (heavier) Hermes import happens lazily on first seed/verify call.
"""
from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

HERMES_REPO = "https://github.com/NousResearch/hermes-agent"
_DEFAULT_DIR = "/tmp/hermes-agent"
_SessionDB_cls = None  # cached class object

# ---------------------------------------------------------------------------
# Locating / importing the real Hermes storage layer
# ---------------------------------------------------------------------------

def locate_hermes_agent() -> Path:
    """Return a path to a hermes-agent checkout, shallow-cloning if needed."""
    d = os.environ.get("HERMES_AGENT_DIR") or _DEFAULT_DIR
    p = Path(d)
    if (p / "hermes_state.py").is_file():
        return p
    if p.exists() and any(p.iterdir()):
        raise RuntimeError(
            f"{p} exists but is not a hermes-agent checkout — set HERMES_AGENT_DIR "
            f"to a NousResearch/hermes-agent clone."
        )
    subprocess.run(
        ["git", "clone", "--depth", "1", HERMES_REPO, str(p)],
        check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    if not (p / "hermes_state.py").is_file():
        raise RuntimeError("hermes-agent clone did not yield hermes_state.py")
    return p


def _SessionDB():
    """Import + cache the real ``SessionDB`` class (lazy)."""
    global _SessionDB_cls
    if _SessionDB_cls is None:
        hd = locate_hermes_agent()
        if str(hd) not in sys.path:
            sys.path.insert(0, str(hd))
        from hermes_state import SessionDB  # real storage layer
        _SessionDB_cls = SessionDB
    return _SessionDB_cls


def schema_version() -> int:
    _SessionDB()
    from hermes_state import SCHEMA_VERSION
    return SCHEMA_VERSION


# ---------------------------------------------------------------------------
# Seed content (every adapter-relevant surface of a HERMES_HOME)
# ---------------------------------------------------------------------------

SOUL = "# Atlas\n\nA demo Hermes agent for the integration walkthrough. Values correctness.\n"

# config.yaml carries a custom system_prompt + personality (→ identity blocks)
# and an inlined api_key that MUST be redacted in the archived raw copy.
CONFIG_YAML = (
    "agent:\n"
    '  system_prompt: "Be terse and cite sources."\n'
    "  personalities:\n"
    '    witty: "Be witty and concise."\n'
    "model:\n"
    '  default: "anthropic/claude-opus-4.6"\n'
    '  api_key: "sk-REDACT-ME-this-is-secret"\n'
)

# Curated memory: three §-delimited entries (one is a rule → procedural).
MEMORY_MD = (
    "User prefers Rust over Go.\n"
    "§\n"
    "Always run cargo fmt before committing.\n"
    "§\n"
    "Staging URL is https://staging.example.com\n"
)

# Plaintext .env → triggers the D4 "not backed up" advisory; never archived.
ENV = "OPENAI_API_KEY=sk-live-not-backed-up\nTELEGRAM_TOKEN=bot-1234567890\n"

# An agent-created skill (procedural memory) — not in any bundle manifest, so
# the adapter exports it as an artifact (D5).
SKILL_MD = "# deploy\n\nThe agent's own deploy skill — created from experience.\n"

# Native session ids in Hermes's real format (YYYYMMDD_HHMMSS_<hex>).
SESSION_A = "20260101_120000_aaaa"
SESSION_B = "20260101_130000_bbbb"


def seed_home(home: Path) -> dict:
    """Build a complete HERMES_HOME. Returns a summary dict for the walkthrough."""
    home.mkdir(parents=True, exist_ok=True)
    (home / "SOUL.md").write_text(SOUL, encoding="utf-8")
    (home / "config.yaml").write_text(CONFIG_YAML, encoding="utf-8")
    (home / ".env").write_text(ENV, encoding="utf-8")
    mem = home / "memories"
    mem.mkdir(exist_ok=True)
    (mem / "MEMORY.md").write_text(MEMORY_MD, encoding="utf-8")
    skill = home / "skills" / "custom" / "deploy"
    skill.mkdir(parents=True, exist_ok=True)
    (skill / "SKILL.md").write_text(SKILL_MD, encoding="utf-8")
    sessions = seed_state_db(home / "state.db")
    return {"sessions": sessions, "schema_version": schema_version(), "curated_entries": 3}


def seed_state_db(db_path: Path) -> int:
    """Seed state.db through the REAL SessionDB. Two ended sessions on two
    platforms, with a compression lineage (B → A) and a tool call. Returns count."""
    SessionDB = _SessionDB()
    for p in (db_path, Path(str(db_path) + "-wal"), Path(str(db_path) + "-shm")):
        if p.exists():
            p.unlink()
    db = SessionDB(Path(db_path))

    db.create_session(SESSION_A, source="cli", model="claude-opus-4-8",
                      system_prompt="You are Hermes, a local-first agent.")
    db.set_session_title(SESSION_A, "Investigate flaky sync")
    db.append_message(SESSION_A, role="user", content="Why does sync fail intermittently?")
    db.append_message(SESSION_A, role="assistant",
                      content="Let me grep the logs for retry markers.")
    db.append_message(SESSION_A, role="tool", tool_name="grep",
                      tool_calls=[{"name": "grep", "args": {"pattern": "retry"}}],
                      content="3 matches in sync.log")
    db.end_session(SESSION_A, end_reason="completed")

    db.create_session(SESSION_B, source="telegram", model="claude-opus-4-8",
                      parent_session_id=SESSION_A)
    db.set_session_title(SESSION_B, "Investigate flaky sync #2")
    db.append_message(SESSION_B, role="user", content="Continue the investigation from before.")
    db.append_message(SESSION_B, role="assistant",
                      content="The retries cluster around WAL checkpoint contention.")
    db.end_session(SESSION_B, end_reason="compression")
    return 2


def append_curated(home: Path, entry: str) -> None:
    """Append a §-delimited curated entry — drives a curated-record delta."""
    p = home / "memories" / "MEMORY.md"
    p.write_text(p.read_text(encoding="utf-8") + "§\n" + entry.rstrip("\n") + "\n", encoding="utf-8")


def add_session(home: Path, sid: str, source: str, title: str, messages: list[tuple[str, str]]) -> None:
    """Add one new ended session — drives a single session-record create delta."""
    SessionDB = _SessionDB()
    db = SessionDB(home / "state.db")
    db.create_session(sid, source=source, model="claude-opus-4-8")
    db.set_session_title(sid, title)
    for role, content in messages:
        db.append_message(sid, role=role, content=content)
    db.end_session(sid, end_reason="completed")


def edit_soul(home: Path, text: str) -> None:
    """Rewrite SOUL.md — drives an identity (Layer 1) delta."""
    (home / "SOUL.md").write_text(text, encoding="utf-8")


def write_user_md(home: Path, text: str) -> None:
    """Create memories/USER.md — the human principal (Layer 3)."""
    (home / "memories" / "USER.md").write_text(text, encoding="utf-8")


# ---------------------------------------------------------------------------
# Verifying a rebuilt state.db the way a live agent reads it
# ---------------------------------------------------------------------------

def verify_state_db(db_path: Path, expect_sessions: int) -> tuple[bool, dict]:
    """Open a (rebuilt) state.db with the REAL SessionDB read-write — exactly as
    a restored agent does, FTS5 capability probe included — and confirm sessions,
    compression lineage, and full-text search all work. Returns (ok, details)."""
    SessionDB = _SessionDB()
    db = SessionDB(Path(db_path))  # read-write → FTS5 write-probe runs
    sessions = db.list_sessions_rich(limit=100, include_children=True, include_archived=True)
    by_id = {s["id"]: s for s in sessions}
    child = by_id.get(SESSION_B)
    lineage_ok = bool(child and child.get("parent_session_id") == SESSION_A)
    fts = db.search_messages("retry", limit=10)        # keyword (session A)
    fts_wal = db.search_messages("WAL", limit=10)      # keyword (session B)
    ok = (
        len(sessions) == expect_sessions
        and lineage_ok
        and len(fts) >= 1
        and len(fts_wal) >= 1
    )
    return ok, {
        "sessions": len(sessions),
        "expected": expect_sessions,
        "lineage_ok": lineage_ok,
        "fts_retry_hits": len(fts),
        "fts_wal_hits": len(fts_wal),
    }
