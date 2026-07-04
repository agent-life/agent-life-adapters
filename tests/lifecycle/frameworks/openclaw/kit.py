"""OpenClawKit — WP2 stub. Z1 install probe + Z2 markdown seeding work today;
every alf stage raises SkipStage(wp="WP4") until the OpenClaw kit lands
(multi-agent + positional-id rules live there)."""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import seed_markers as seeder  # noqa: E402
from alflab import scenario  # noqa: E402
from alflab.contract import FrameworkKit, PlacementRow, SkipStage, TurnLog  # noqa: E402


class OpenClawKit(FrameworkKit):
    name = "openclaw"
    pinned_version = "2026.6.11"
    image_tag = "alf-lifecycle-openclaw"
    home_mount = "/home/agent/.openclaw"
    agent_slots = ["default"]
    config_paths = ["openclaw.json"]

    def install_probe(self, ctr) -> dict:
        proc = ctr.exec(["openclaw", "--version"], timeout=120)
        version = ((proc.stdout or "").strip().splitlines() or [""])[-1]
        topology = []
        if self.env.host_home.is_dir():
            topology = [str(p.relative_to(self.env.host_home))
                        for p in sorted(self.env.host_home.rglob("*")) if p.is_file()]
        return {"version": version, "topology": topology,
                "declared_agents": [], "config": {}}

    def wire_llm(self, ctr, creds) -> None:
        raise SkipStage("openclaw LLM wiring lands with the OpenClaw kit", wp="WP4")

    def seed_markers(self, ctr, slot: str, round: int) -> None:
        ws = self.env.host_home / "workspace"
        seeder.seed_round(ws, slot, scenario.turns(slot, round))

    def llm_turn(self, ctr, slot: str, turn) -> TurnLog:
        raise SkipStage("openclaw LLM turns land with the OpenClaw kit", wp="WP4")

    def dump_memory(self, ctr, slot: str) -> str:
        ws = self.env.host_home / "workspace"
        if not ws.is_dir():
            return ""
        parts = []
        for p in sorted(ws.rglob("*.md")):
            parts.append(p.read_text(encoding="utf-8", errors="replace"))
        return "\n".join(parts)

    def placement(self, ctr, slot: str, markers: list) -> list:
        ws = self.env.host_home / "workspace"
        out = []
        for p in sorted(ws.rglob("*.md")) if ws.is_dir() else []:
            text = p.read_text(encoding="utf-8", errors="replace")
            for m in markers:
                if m in text:
                    out.append(PlacementRow(slot=slot, category="markdown",
                                            key=str(p.relative_to(ws)),
                                            head=m))
        return out

    def alf_target_args(self, slot: str) -> list:
        return ["-r", self.name]


KIT_CLASS = OpenClawKit
