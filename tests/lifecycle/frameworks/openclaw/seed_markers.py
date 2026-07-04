"""OpenClaw no-LLM seeder — markdown writes into the workspace (WP2 stub;
WP4 owns the full kit). OpenClaw memory is markdown-file based: MEMORY.md +
memory/YYYY-MM-DD.md daily logs. Round N appends — earlier rounds' files are
never rewritten (delta exactness, positional-id safety)."""

from __future__ import annotations

from datetime import date, timedelta
from pathlib import Path


def seed_round(workspace: Path, slot: str, turns) -> None:
    workspace.mkdir(parents=True, exist_ok=True)
    memdir = workspace / "memory"
    memdir.mkdir(exist_ok=True)
    soul = workspace / "SOUL.md"
    if not soul.is_file():
        persona = turns[0].persona.capitalize() if turns else "Atlas"
        soul.write_text(f"# {persona}\n\nLifecycle harness seed agent.\n",
                        encoding="utf-8")
    rnd = turns[0].round if turns else 1
    day = date(2026, 1, 14) + timedelta(days=rnd)
    daily = memdir / f"{day.isoformat()}.md"
    lines = [f"## Round {rnd} seeded memories", ""]
    for t in turns:
        lines.append(f"- [{t.turn_type}] verbatim marker: {t.marker}")
    daily.write_text("\n".join(lines) + "\n", encoding="utf-8")
