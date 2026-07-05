"""OpenClaw no-LLM seeder — markdown writes into the per-agent workspace (WP4).

OpenClaw memory is markdown-file based; the durable long-term store a user
edits is `workspace-<name>/MEMORY.md` (what a real `openclaw agent` turn writes
to). The seeder appends one `## Round N` section of verbatim markers per round —
earlier rounds' text is never rewritten, so a delta between round N and N+1 is
exactly the new round's lines (delta exactness; positional-id safety).
"""

from __future__ import annotations

from pathlib import Path


def seed_round(workspace: Path, slot: str, turns) -> None:
    workspace.mkdir(parents=True, exist_ok=True)
    soul = workspace / "SOUL.md"
    if not soul.is_file():
        persona = turns[0].persona.capitalize() if turns else "Atlas"
        soul.write_text(f"# {persona}\n\nLifecycle harness seed agent.\n", encoding="utf-8")

    rnd = turns[0].round if turns else 1
    mem = workspace / "MEMORY.md"
    prefix = "" if mem.is_file() else "# Memory\n\n"
    lines = [f"## Round {rnd} seeded memories", ""]
    for t in turns:
        lines.append(f"- [{t.turn_type}] verbatim marker: {t.marker}")
    block = prefix + "\n".join(lines) + "\n\n"
    with mem.open("a", encoding="utf-8") as f:
        f.write(block)
