"""Coverage / isolation / placement verdicts — ported from
scripts/multiagent-verify.sh, generalized from the fixed two-agent bash form
to M agent slots (the pilot runs M=1).

Deterministic: matches on the unique scenario markers, so model phrasing is
irrelevant. Coverage counts each slot's own markers present in that slot's
memory dump; isolation flags any OTHER slot's marker appearing in it.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from . import scenario


@dataclass
class SlotVerdict:
    slot: str
    persona: str
    present: dict          # type -> bool
    leaks: list = field(default_factory=list)   # foreign markers found


@dataclass
class Verdict:
    slots: list            # [SlotVerdict]
    covered: int
    total: int
    isolation: str         # "clean" | "leak"

    @property
    def coverage(self) -> str:
        return f"{self.covered}/{self.total}"

    @property
    def clean(self) -> bool:
        return self.isolation == "clean" and self.covered == self.total


def check_coverage(dumps: dict, round: int = 1) -> Verdict:
    """`dumps` = {slot: memory-dump-text}. Returns the M-generalized verdict."""
    slots = []
    covered = 0
    total = 0
    leak = False
    for slot, dump in dumps.items():
        dump = dump or ""
        present = {}
        for turn in scenario.turns(slot, round):
            hit = turn.marker in dump
            present[turn.turn_type] = hit
            covered += 1 if hit else 0
            total += 1
        leaks = []
        for other in dumps:
            if other == slot:
                continue
            for m in scenario.markers(other, round):
                if m in dump:
                    leaks.append(m)
        if leaks:
            leak = True
        slots.append(SlotVerdict(slot=slot, persona=scenario.PERSONAS[slot],
                                 present=present, leaks=leaks))
    return Verdict(slots=slots, covered=covered, total=total,
                   isolation="leak" if leak else "clean")


def render_verdict(v: Verdict) -> list[str]:
    """Markdown lines mirroring the bash verifier's table + verdict block."""
    lines = [
        "| slot | semantic | episodic | procedural | secret |",
        "|------|:--------:|:--------:|:----------:|:------:|",
    ]
    for s in v.slots:
        cells = " | ".join("✓" if s.present.get(t) else "·" for t in scenario.TYPES)
        lines.append(f"| {s.slot} ({s.persona.capitalize()}) | {cells} |")
    lines.append("")
    for s in v.slots:
        if s.leaks:
            lines.append(f"- **{s.slot}: LEAK** -> {' '.join(s.leaks)}")
        else:
            lines.append(f"- {s.slot}: clean (no foreign markers)")
    lines.append("")
    lines.append(f"**Verdict:** coverage {v.coverage} memory markers · isolation {v.isolation}")
    return lines
