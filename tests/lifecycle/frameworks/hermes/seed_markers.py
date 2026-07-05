"""Hermes no-LLM seeder — curated `§`-entries into a profile's memories/MEMORY.md.

Hermes memory has two surfaces: the curated `memories/*.md` store (`§`-separated
entries, parsed by adapter-hermes/src/curated_parser.rs) and per-session
`state.db`. The deterministic no-LLM tier writes the four round-1 markers as four
curated entries — the same store a real turn consolidates into, no schema, no DB,
no model. All four (including the `secret` marker) go into memory verbatim: ALF is
framework-neutral, so memory backs up as-is, exactly like the OpenClaw/ZeroClaw
seeders (the vault is the encrypted alternative, exercised by Z6/Z11). Hermes's
`.env` is never archived, but that is a separate adapter invariant (export's D7
allowlist), not where the scenario's `secret` marker lives.

Append-shaped: a later round only ADDS entries (`§`-separated), earlier rounds are
never rewritten, so a delta between round N and N+1 is exactly the new round's
entries (delta exactness; content-derived ids keep unchanged entries stable).

The `state.db` session surface is exercised by the proxy tier's real `hermes chat`
turns (kit.llm_turn), not by this deterministic plumbing.
"""

from __future__ import annotations

from pathlib import Path

# Hermes curated entry separator (U+00A7), per curated_parser.rs.
SEP = "§"

# Lead-ins chosen so classify() in curated_parser.rs tags the procedural entry
# `procedural` (its head starts with "always ") and the rest `semantic` —
# coverage is type-agnostic, but this keeps the extracted record types faithful.
_LEAD = {
    "semantic": "Remember this durable fact",
    "episodic": "On this day the agent recorded",
    "procedural": "Always run this saved procedure",
    "secret": "Stored credential (throwaway test value)",
}


def _entry_text(turn) -> str:
    lead = _LEAD.get(turn.turn_type, "Note")
    return f"{lead} — [{turn.turn_type} r{turn.round}] verbatim marker: {turn.marker}"


def seed_round(home: Path, slot: str, turns) -> None:
    """Append one curated entry per marker turn to `<home>/memories/MEMORY.md`.

    `home` is the profile dir (the default profile is `~/.hermes` itself; a named
    profile is `~/.hermes/profiles/<name>/`). Append-shaped: round N+1 appends
    without touching round N's bytes.
    """
    home.mkdir(parents=True, exist_ok=True)
    # A named profile created via `hermes profile create` already has a SOUL.md;
    # the default profile always does. Seed a skeleton only if it is missing so
    # export (which requires the home to be a dir with identity) always has one.
    soul = home / "SOUL.md"
    if not soul.is_file():
        persona = turns[0].persona.capitalize() if turns else "Atlas"
        soul.write_text(f"# {persona}\n\nLifecycle harness seed agent.\n", encoding="utf-8")

    mem_dir = home / "memories"
    mem_dir.mkdir(parents=True, exist_ok=True)
    mem = mem_dir / "MEMORY.md"

    new_entries = [_entry_text(t) for t in turns]
    has_content = mem.is_file() and mem.read_text(encoding="utf-8").strip() != ""
    if has_content:
        # Append `\n§\n<entry>` per new entry — round-1 bytes stay identical.
        block = "".join(f"\n{SEP}\n{e}\n" for e in new_entries)
        with mem.open("a", encoding="utf-8") as f:
            f.write(block)
    else:
        mem.write_text(f"\n{SEP}\n".join(new_entries) + "\n", encoding="utf-8")
