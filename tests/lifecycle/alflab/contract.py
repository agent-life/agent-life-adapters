"""FrameworkKit ABC — the per-framework plug-in surface (WP3–5 extend this).

A kit owns everything framework-specific: the pinned install, LLM wiring,
real-store seeding, memory dumps, and how alf addresses its agents. Stages
in stages.py are framework-agnostic and speak only through this contract.
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

from .provision import RuntimeCreds  # noqa: F401  (re-exported contract type)


class SkipStage(Exception):
    """Raised by a kit/stage when a stage is out of scope. `wp` names the
    owning work package so `--full` renders planned slots, never invisible."""

    def __init__(self, reason: str, wp: Optional[str] = None):
        super().__init__(reason)
        self.reason = reason
        self.wp = wp


@dataclass
class TurnLog:
    slot: str
    turn_type: str
    marker: str
    prompt: str
    response_tail: str = ""
    ok: bool = True


@dataclass
class PlacementRow:
    """Where one memory landed in the framework's own store."""
    slot: str
    category: str
    key: str
    head: str          # first ~55 chars of content


@dataclass
class KitEnv:
    """Host-side context handed to a kit: the bind-mounted paths + tier."""
    run_dir: Path
    host_home: Path            # bind-mounted framework home (e.g. ~/.zeroclaw)
    host_alf_home: Path        # bind-mounted ~/.alf
    llm: str = "none"          # none | proxy
    backend: str = "none"      # none | real
    creds: Optional[RuntimeCreds] = None


class FrameworkKit(ABC):
    """One per framework under tests/lifecycle/frameworks/<name>/kit.py."""

    name: str = ""
    pinned_version: str = ""
    image_tag: str = ""
    home_mount: str = ""                 # container path of the framework home
    agent_slots: list = ["default"]
    config_paths: list = []              # home-relative files of config interest

    def __init__(self, env: KitEnv):
        self.env = env

    # -- Z1 -------------------------------------------------------------------
    @abstractmethod
    def install_probe(self, ctr) -> dict:
        """Return {'version': str, 'topology': [str], 'declared_agents': [str],
        'config': {…}} — what the standard install actually declares."""

    @abstractmethod
    def wire_llm(self, ctr, creds: RuntimeCreds) -> None:
        """Point the framework's model provider at the LLM proxy."""

    # -- Z2 -------------------------------------------------------------------
    @abstractmethod
    def seed_markers(self, ctr, slot: str, round: int) -> None:
        """No-LLM tier: materialize the REAL store and insert the round's
        marker rows through it (deterministic plumbing proof)."""

    @abstractmethod
    def llm_turn(self, ctr, slot: str, turn) -> TurnLog:
        """LLM tier: drive one real conversation turn (scenario.MarkerTurn)."""

    @abstractmethod
    def dump_memory(self, ctr, slot: str) -> str:
        """All memory-bearing text for `slot`, via the framework's OWN store."""

    @abstractmethod
    def placement(self, ctr, slot: str, markers: list) -> list:
        """[PlacementRow] — where each marker landed (category/key)."""

    # -- alf addressing ---------------------------------------------------------
    @abstractmethod
    def alf_target_args(self, slot: str) -> list:
        """Extra `alf` argv selecting this framework/agent (e.g. ['-r', name])."""

    # -- WP3–5 slots (default: planned, not invisible) --------------------------
    def create_agent(self, ctr, slot: str) -> None:                # Z8
        raise SkipStage(f"{self.name}: create_agent lands with multi-agent", wp="WP4")

    def mutate_slice(self, ctr, slot: str, round: int) -> None:    # Z12
        raise SkipStage(f"{self.name}: mutate_slice lands with the adapter fix", wp="WP3")

    def native_memory_stats(self, ctr, slot: str) -> dict:         # parity oracle
        raise SkipStage(f"{self.name}: native stats parity lands with the adapter fix",
                        wp="WP3")
