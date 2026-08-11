"""Generic no-LLM seeder — markdown writes into a mapped `by_heading` source.

The generic runtime has no framework agent to drive; the seeder writes the
round's marker turns straight into the workspace file the `.alf-map.json`'s
`journal` source globs (`memories/YYYY-MM-DD.md`, episodic / daily / by_heading /
filename_date). One `## ` section per marker → one episodic record per marker
(the same chunk → birth-id → dashboard-card path a real MCP agent's writes take;
the extractor is pinned by the adapter-generic golden corpus). Rounds are
append-shaped: round N writes its own dated file, so earlier rounds are never
rewritten and a later delta is exactly the new round's records.
"""

from __future__ import annotations

from pathlib import Path

# Round N → a distinct YYYY-MM-DD journal file (filename_date anchors created_at
# to that day's midnight UTC — the map's `timestamp: filename_date`).
_ROUND_DATE = {1: "2026-07-04", 2: "2026-07-05"}


def seed_round(workspace: Path, slot: str, turns) -> None:
    mem_dir = workspace / "memories"
    mem_dir.mkdir(parents=True, exist_ok=True)
    rnd = turns[0].round if turns else 1
    dated = mem_dir / f"{_ROUND_DATE.get(rnd, '2026-07-04')}.md"
    lines: list[str] = []
    for t in turns:
        # `## <marker>` is the section boundary; the body carries the marker
        # verbatim so the record's content is non-empty (empty-body sections drop).
        lines.append(f"## [{t.turn_type}] {t.marker}")
        lines.append("")
        lines.append(f"Round {rnd} {t.turn_type} memory. Verbatim marker: {t.marker}.")
        lines.append("")
    with dated.open("a", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
