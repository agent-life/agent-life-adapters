"""GenericKit — the WP-M4 MCP-driven framework plug-in (the toy runtime).

Unlike the three shipped kits, "generic" is not a framework you install: it is
*any* MCP-capable host, driven through `alf mcp serve`. So this kit differs on
two axes the contract already anticipates:

  * **Transport.** `make_invoker` returns an `McpInvoker`, not the CLI path — the
    harness itself is the MCP host here (design: the toy runtime's coverage is
    *scripted*, never a bespoke LLM host). Every tool-shaped alf call in the
    Z-stages goes over one persistent `alf mcp serve -r generic -w <ws>` stdio
    session; the server pins the workspace, which is exactly why generic's
    per-tool calls need no `-w` even though the CLI does (WP-M1's stray-write
    guard). Non-tool ops (the Z13' `export` copy-out, `--version`) fall back to
    the CLI, and `[defaults].workspace` (from `config_defaults`) lets those
    resolve.

  * **Store.** There is no framework memory store; the agent's memories are the
    workspace files a committed `.alf-map.json` maps (episodic `memories/*.md`,
    semantic `knowledge/**`, procedural `procedures/*`). The no-LLM seeder writes
    marker sections into the mapped `by_heading` journal, so coverage/placement
    read plain markdown (the OpenClaw-isolated shape).

Tier: `--llm none --backend none` only (the CI tier). `--llm proxy` is refused —
a bespoke LLM host for the toy runtime is an explicit WP-M4 non-goal; the real
LLM-in-the-loop gate is the Hermes MCP host tier.
"""

from __future__ import annotations

import json
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))       # tests/lifecycle
sys.path.insert(0, str(Path(__file__).resolve().parent))           # this dir

import seed_markers as seeder  # noqa: E402
from alflab import scenario, ui  # noqa: E402
from alflab.contract import FrameworkKit, PlacementRow, SkipStage, TurnLog  # noqa: E402
from alflab.invoker import McpInvoker  # noqa: E402

FIXTURE_DIR = Path(__file__).resolve().parent / "fixture"
# Control/derived files the map never turns into memories (they travel raw); the
# topology probe lists them but coverage/placement ignore them.
_CONTROL_FILES = {".alf-map.json", ".alf-include.json", ".alf-agent-id", ".alfignore"}


class GenericKit(FrameworkKit):
    name = "generic"
    pinned_version = "0.1.0"                 # the fixture map's framework_version
    image_tag = "alf-lifecycle-generic"
    home_mount = "/home/agent/generic-workspace"
    agent_slots = ["default"]               # the map's single `.alf-agent-id` agent
    config_paths = [".alf-map.json"]
    memory_store_label = "mapped markdown (.alf-map.json)"
    memory_topology = "isolated"            # one workspace = one agent
    memory_shape = "append"                 # dated journal files, never rewritten

    def seed_narrative(self) -> str:
        return ("No-LLM tier: the seeder writes the four round-1 markers as `## `-"
                "sections into the mapped journal (memories/YYYY-MM-DD.md) — the "
                "same by_heading source a real MCP agent writes; the generic "
                "adapter chunks them into episodic records. Deterministic "
                "plumbing, no model, no framework store.")

    def seed_flow(self) -> str:
        return "seed_markers.py ──append `## `-sections──▶ memories/*.md (mapped, by_heading)"

    def isolation_narrative(self) -> str:
        return ("Generic is single-agent by design (one workspace = one "
                "`.alf-agent-id`); multi-agent hosts run one `alf mcp serve` per "
                "agent — out of the toy runtime's scripted scope.")

    # -- transport + config (WP-M4) -------------------------------------------

    def make_invoker(self, run):
        """Seed the toy fixture into the run's (bind-mounted) home, then return
        the MCP-session invoker. Seeding here — before the stage loop — means the
        `.alf-map.json` is present for Z1's probe and the workspace is ready
        whenever the first tool call opens the server session (lazily, at Z3).
        The raw runtime key never enters this path (backend=none)."""
        self._seed_fixture()
        env = {}
        if run.creds is not None:  # only ever set on a (non-default) backend tier
            env = {"ALF_API_KEY": run.creds.runtime_api_key,
                   "ALF_API_URL": run.creds.alf_api_url}
        return McpInvoker(run.container, runtime=self.name, workspace=self.home_mount,
                          agent=self.agent_slots[0], env=env,
                          log=lambda m: ui.emit(f"  [mcp] {m}"))

    def config_defaults(self) -> dict:
        # Generic CLI ops require an explicit workspace; pinning it lets the
        # invoker's CLI fallbacks (Z13' export copy-out) resolve without -w.
        return {"workspace": self.home_mount}

    def _seed_fixture(self) -> None:
        """Copy the committed fixture into the bind-mounted home (idempotent)."""
        if not FIXTURE_DIR.is_dir():
            raise RuntimeError(f"generic fixture missing: {FIXTURE_DIR}")
        shutil.copytree(FIXTURE_DIR, self.env.host_home, dirs_exist_ok=True)

    # -- paths -----------------------------------------------------------------

    def _map(self) -> dict:
        mp = self.env.host_home / ".alf-map.json"
        if not mp.is_file():
            return {}
        try:
            return json.loads(mp.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            return {}

    def _memory_files(self, slot: str) -> list:
        """Every mapped memory-bearing markdown file (the map's globs, flattened
        to `*.md` under the workspace). Control/derived files are excluded — they
        ride raw, they are not memories."""
        ws = self.env.host_home
        if not ws.is_dir():
            return []
        return [p for p in sorted(ws.rglob("*.md"))
                if ".git" not in p.parts and p.name not in _CONTROL_FILES]

    # -- Z1 --------------------------------------------------------------------

    def install_probe(self, ctr) -> dict:
        """The generic "install" is the committed map + fixture (seeded in
        make_invoker). Version comes from the map's `framework_version`; topology
        is the seeded file tree; the sole declared agent is the map's agent id."""
        mp = self._map()
        version = str(mp.get("framework_version", "")) or "unknown"
        topology = []
        if self.env.host_home.is_dir():
            for p in sorted(self.env.host_home.rglob("*")):
                if p.is_file():
                    topology.append(str(p.relative_to(self.env.host_home)))
        return {
            "version": version,
            "topology": topology,
            "declared_agents": self.agent_slots,
            # No framework config file, so no workspace_dir and no schema_version
            # (Z1 asserts exactly this: install root IS the workspace).
            "config": {"schema_version": None, "has_workspace_dir": False},
        }

    def wire_llm(self, ctr, creds) -> None:
        raise SkipStage(
            "generic is a scripted MCP runtime — a bespoke LLM host for the toy "
            "runtime is a WP-M4 non-goal (the LLM gate is the Hermes MCP tier)",
            wp="WP-M4")

    # -- Z2 --------------------------------------------------------------------

    def seed_markers(self, ctr, slot: str, round: int) -> None:
        seeder.seed_round(self.env.host_home, slot, scenario.turns(slot, round))

    def llm_turn(self, ctr, slot: str, turn) -> TurnLog:
        raise SkipStage("generic runs the no-LLM tier only (scripted coverage)",
                        wp="WP-M4")

    def dump_memory(self, ctr, slot: str) -> str:
        return "\n".join(p.read_text(encoding="utf-8", errors="replace")
                         for p in self._memory_files(slot))

    def placement(self, ctr, slot: str, markers: list) -> list:
        out = []
        ws = self.env.host_home
        for p in self._memory_files(slot):
            text = p.read_text(encoding="utf-8", errors="replace")
            for m in markers:
                if m in text:
                    out.append(PlacementRow(slot=slot, category="mapped",
                                            key=str(p.relative_to(ws)), head=m))
        return out

    def native_memory_stats(self, ctr, slot: str) -> dict:
        # Only exercised on the proxy tier, which generic never runs.
        raise SkipStage("generic has no LLM tier", wp="WP-M4")

    # -- topology predicates ---------------------------------------------------

    def is_per_agent_workspace(self, ws: str) -> bool:
        """The generic workspace IS the agent (no per-agent subdir); the mapped
        workspace equals the install root."""
        return ws == self.home_mount or ws.startswith(self.home_mount)

    # -- alf addressing --------------------------------------------------------

    def alf_target_args(self, slot: str) -> list:
        return ["-r", self.name]


KIT_CLASS = GenericKit
