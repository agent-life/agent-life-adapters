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
    # Some frameworks install their RUNTIME inside the framework home (Hermes:
    # ~/.hermes/hermes-agent + node + bin). The harness bind-mounts a fresh host
    # dir over the home per run, which would shadow that runtime. When True, the
    # runner seeds the run's home from the image's home_mount first, so the
    # mounted home is the real colocated install a user has (not an empty dir).
    seed_home_from_image: bool = False

    # -- narration: how THIS framework physically stores memory (design §3). The
    #    stage narrator speaks through these so it never says "brain.db" for a
    #    markdown framework. Override per kit.
    memory_store_label: str = "its memory store"   # e.g. "brain.db", "MEMORY.md + memory/*.md"
    memory_topology: str = "isolated"              # "shared" (one store, filtered) | "isolated"
    # How the framework's agent treats its long-term store (WP4.1): "append"
    # (rows/sections only ever added — ZeroClaw brain.db, Hermes sessions) or
    # "curated" (rewritten in place — OpenClaw's MEMORY.md). Z14 runs only for
    # curated stores; Z5's round-1 survival is informational on the LLM tier
    # for them (a real model may legitimately overwrite earlier memories).
    memory_shape: str = "append"                   # "append" | "curated"
    # WP-M4 MCP LLM tier: when True, the framework is an MCP *host* — the LLM
    # agent drives sync/vault by calling `mcp_alf_*` tools, and the Z15 gate runs
    # instead of the harness driving alf itself. Only the hermes-mcp kit sets it;
    # every other kit leaves it False, so Z15 SKIPs and no other stage changes.
    mcp_llm_mode: bool = False
    # When True, the Z16 watch-auto-sync gate runs: the harness starts a
    # persistent `alf mcp serve` with a ~1s watch cadence (test-only env
    # overrides), mutates memory files + the sqlite store on a timer, and asserts
    # the watch loop auto-uploaded the deltas. Only hermes-mcp sets it; every
    # other kit leaves it False, so Z16 SKIPs.
    watch_autosync_mode: bool = False

    def __init__(self, env: KitEnv):
        self.env = env

    def seed_narrative(self) -> str:
        """Z2 no-LLM branch: prose for how the seeder writes the round-1 markers
        into THIS framework's real store, deterministically (no model)."""
        return (f"No-LLM tier: the seeder writes the four round-1 markers straight "
                f"into {self.memory_store_label} — deterministic plumbing, same "
                f"store, no model.")

    def seed_flow(self) -> str:
        """Z2 no-LLM data-flow line (framework-specific store)."""
        return f"seed_markers.py ──▶ {self.memory_store_label}"

    def isolation_narrative(self) -> str:
        """Z10: how per-agent isolation is achieved in THIS framework."""
        return ("Populate agent b's OWN marked memories, sync, and assert isolation "
                "BOTH ways: b's archive carries only b's markers, and a's memory is "
                "untouched.")

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

    # -- alf transport (WP-M4) --------------------------------------------------
    def make_invoker(self, run):
        """How this framework drives `alf` in the Z-stages. Default is the
        terminal path (`CliInvoker`, a transparent passthrough to
        `container.exec*`); the generic kit overrides this to return an
        `McpInvoker` that keeps one `alf mcp serve` stdio session open and maps
        each tool-shaped command to a `tools/call`. Called once, after the
        container is up. The three shipped frameworks keep the CLI path, so their
        tiers are byte-for-byte unchanged."""
        from .invoker import CliInvoker
        return CliInvoker(run.container)

    def config_defaults(self) -> dict:
        """Extra `[defaults]` entries the harness writes into ~/.alf/config.toml
        at Z3 (empty for the shipped frameworks → identical config). The generic
        kit pins `workspace` here so its CLI-fallback ops (e.g. the Z13' export
        copy-out) resolve without an explicit `-w` — a real generic user sets the
        same key (the CLI's own `workspace_not_found` hint says so)."""
        return {}

    # -- alf addressing ---------------------------------------------------------
    @abstractmethod
    def alf_target_args(self, slot: str) -> list:
        """Extra `alf` argv selecting this framework/agent (e.g. ['-r', name])."""

    # -- topology predicates (stages speak through these, not store internals) --
    def is_per_agent_workspace(self, ws: str) -> bool:             # Z3
        """True if `ws` is a per-agent workspace in this framework's layout.
        Default: anything under the framework home. ZeroClaw narrows to
        `<home>/agents/<alias>/workspace`; OpenClaw uses `<home>/workspace-<name>`,
        both of which start with `home_mount`."""
        return ws.startswith(self.home_mount)

    def agent_declared(self, ctr, slot: str) -> bool:              # Z8
        """True if the framework's own config now declares `slot` (checked
        through the config, not by string-matching one framework's dialect)."""
        raise SkipStage(f"{self.name}: agent_declared lands with multi-agent", wp="WP4")

    def raw_parity_entry(self) -> str:                             # Z4
        """Archive entry that proves the framework's raw source round-tripped
        (the fidelity safety net). Default is `raw/<name>/config.toml`; OpenClaw
        has no config file, so it points at its durable `MEMORY.md`."""
        return f"raw/{self.name}/config.toml"

    def archive_marker_prefix(self) -> str:                        # Z4 / Z10
        """Archive entry prefix to scan for seeded markers. ZeroClaw stores every
        memory as a brain.db record → all markers land under `memory/`. OpenClaw's
        agent may write a marker into any workspace file (a credential into
        `TOOLS.md` → `raw/`), all captured, so it scans the whole archive."""
        return "memory/"

    def llm_wired(self) -> tuple:                                  # Z1 (proxy tier)
        """After `wire_llm`: `(wired?, config_text)` — read the framework's own
        config and report whether the LLM proxy provider is now present.
        `config_text` feeds the redaction self-check. Default is ZeroClaw's
        `config.toml` shape; OpenClaw checks `openclaw.json`."""
        cfg = self.env.host_home / "config.toml"
        text = cfg.read_text(encoding="utf-8") if cfg.is_file() else ""
        return ("agentlife" in text and 'embedding_provider = "none"' in text, text)

    # -- WP3–5 slots (default: planned, not invisible) --------------------------
    def create_agent(self, ctr, slot: str) -> None:                # Z8
        raise SkipStage(f"{self.name}: create_agent lands with multi-agent", wp="WP4")

    def curate_memory(self, ctr, slot: str, op: str) -> None:      # Z14 (WP4.1)
        """Perform one in-place curation `op` on the slot's memory store:
        'touch' (re-save identical bytes), 'reorder' (re-rank entries),
        'edit' (rewrite one memory in place — the WP4.1 §1a shape),
        'insert' (add one mid-store), 'delete' (remove the inserted one).
        Only meaningful for kits declaring `memory_shape == "curated"`."""
        raise SkipStage(f"{self.name}: curated-memory ops apply to curated stores only",
                        wp="WP4.1")

    def mutate_slice(self, ctr, slot: str, round: int) -> None:    # Z12
        raise SkipStage(f"{self.name}: mutate_slice lands with the adapter fix", wp="WP3")

    def assert_restore_isolation(self, run, result, slot: str) -> None:  # Z12
        """Diverge `slot` from its archive, `alf restore --agent <slot>`, and
        assert the slot returns to the archive while every OTHER agent stays
        byte-identical — measured through THIS framework's store (ZeroClaw:
        brain.db rows; OpenClaw: the other workspace dirs). Appends Checks to
        `result`."""
        raise SkipStage(f"{self.name}: restore isolation lands with the adapter fix", wp="WP3")

    def native_memory_stats(self, ctr, slot: str) -> dict:         # parity oracle
        raise SkipStage(f"{self.name}: native stats parity lands with the adapter fix",
                        wp="WP3")

    def mcp_llm_gate(self, run, result) -> None:                   # Z15 (WP-M4)
        """The MCP LLM-in-the-loop release gate: the LLM agent drives sync (and
        vault) by calling the `mcp_alf_*` tools its host spawned, and this asserts
        the effect through the ⊙ backend lanes plus an MCP-path marker (the
        harness itself never calls `alf sync` in this tier). Only the hermes-mcp
        kit implements it; every other framework SKIPs Z15."""
        raise SkipStage(f"{self.name}: the MCP LLM gate is the hermes-mcp host tier",
                        wp="WP-M4")
