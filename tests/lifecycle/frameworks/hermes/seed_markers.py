"""Hermes no-LLM seeder — SKELETON (WP5).

WP5 evolves adapter-hermes/testkit/seed.py + scripts/hermes_runtime.py
(the faithful SessionDB layer) into this module:

  * ensure the HERMES_HOME skeleton (config.yaml redact-shape, SOUL.md);
  * write curated §-entries into memories/MEMORY.md carrying the round's
    semantic/procedural markers;
  * add sessions through the REAL hermes_state.SessionDB (state.db) carrying
    the episodic marker (sessions are decomposed to records at export — the
    binary is never archived);
  * the secret marker goes to .env, which must NEVER be archived — it is the
    D4 vault-advisory assertion, not a memory row.
"""

from __future__ import annotations

from pathlib import Path


def seed_round(home: Path, slot: str, turns) -> None:
    raise NotImplementedError("WP5: Hermes SessionDB seeder (see module docstring)")
