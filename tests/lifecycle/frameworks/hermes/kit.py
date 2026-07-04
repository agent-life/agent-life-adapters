"""HermesKit — WP2 stub. Everything raises SkipStage(wp="WP5") until the
Hermes kit lands (SessionDB seeding evolves adapter-hermes/testkit/seed.py)."""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from alflab.contract import FrameworkKit, SkipStage, TurnLog  # noqa: E402


class HermesKit(FrameworkKit):
    name = "hermes"
    pinned_version = "0.17.0"
    image_tag = "alf-lifecycle-hermes"
    home_mount = "/home/agent/.hermes"
    agent_slots = ["default"]
    config_paths = ["config.yaml"]

    def install_probe(self, ctr) -> dict:
        proc = ctr.exec(["hermes", "--version"], timeout=120)
        return {"version": (proc.stdout or "").strip(), "topology": [],
                "declared_agents": [], "config": {}}

    def wire_llm(self, ctr, creds) -> None:
        raise SkipStage("hermes LLM wiring lands with the Hermes kit", wp="WP5")

    def seed_markers(self, ctr, slot: str, round: int) -> None:
        # WP5: evolve adapter-hermes/testkit/seed.py into a SessionDB seeder
        # (state.db sessions + curated memories/MEMORY.md entries).
        raise SkipStage("hermes SessionDB seeder lands with the Hermes kit", wp="WP5")

    def llm_turn(self, ctr, slot: str, turn) -> TurnLog:
        raise SkipStage("hermes LLM turns land with the Hermes kit", wp="WP5")

    def dump_memory(self, ctr, slot: str) -> str:
        raise SkipStage("hermes memory dump lands with the Hermes kit", wp="WP5")

    def placement(self, ctr, slot: str, markers: list) -> list:
        raise SkipStage("hermes placement lands with the Hermes kit", wp="WP5")

    def alf_target_args(self, slot: str) -> list:
        return ["-r", self.name]


KIT_CLASS = HermesKit
