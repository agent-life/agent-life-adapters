"""OpenClawKit — WP4 framework plug-in (directory-isolated topology).

OpenClaw is the *easy* topology: each agent owns a `workspace-<name>/` subtree
holding its real memory (`MEMORY.md`, `memory/*.md`), declared in
`openclaw.json` `agents.list[]`. There is NO shared store — isolation is a
filesystem fact, so `assert_restore_isolation` compares the OTHER agents' dirs
rather than SQL-slicing one store (contrast ZeroClaw's shared brain.db).

Verified verbs (adapter-openclaw/testkit scripts, pinned openclaw@2026.6.11):
  * create   → `openclaw agents add <name> --non-interactive --workspace <dir>`
  * wire LLM → `openclaw config set models.providers.agent-life '<json>'`
               + `openclaw config set agents.defaults.model 'agent-life/<model>'`
  * turn     → `openclaw agent --local --agent <name> --session-id <id> -m <prompt>`
A default `main` agent always pre-exists (the Phase-1 sole agent); Z8 adds the
second (`agent_b`).

OpenClaw's agent curates MEMORY.md IN PLACE (memory_shape = "curated"): a real
model overwrites/re-ranks earlier sections rather than appending. WP4.1's
base-aware reconciliation makes those curations sync as exact deltas; Z14
exercises the ops deterministically (curate_memory below), and Z5's round-1
survival is informational on the proxy tier for this kit. Design:
docs/multi-agent-support/wp4.1-robust-diff-delta-design.md.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import seed_markers as seeder  # noqa: E402
from alflab import scenario, snapshots  # noqa: E402
from alflab.contract import FrameworkKit, PlacementRow, TurnLog  # noqa: E402
from alflab.report import Check  # noqa: E402


class OpenClawKit(FrameworkKit):
    name = "openclaw"
    pinned_version = "2026.6.11"
    image_tag = "alf-lifecycle-openclaw"
    home_mount = "/home/agent/.openclaw"
    agent_slots = ["main"]                 # the install's always-present agent
    config_paths = ["openclaw.json"]
    memory_store_label = "MEMORY.md + memory/*.md"  # per-agent markdown, no DB
    memory_topology = "isolated"                     # one workspace dir per agent
    memory_shape = "curated"                         # rewritten in place (WP4.1)

    def seed_narrative(self) -> str:
        return ("No-LLM tier: the seeder appends the four round-1 markers to the "
                "agent's workspace/MEMORY.md — plain markdown, no schema, no DB. "
                "Deterministic plumbing — same files a real turn writes, no model.")

    def seed_flow(self) -> str:
        return "seed_markers.py ──append──▶ <agent workspace>/MEMORY.md (markdown, verbatim)"

    def isolation_narrative(self) -> str:
        return ("Populate agent b's OWN marked memories in its workspace, sync, and "
                "assert isolation BOTH ways: b's archive carries only b's markers, "
                "and a's workspace is untouched. Separate per-agent directories keep "
                "them isolated — there is no shared store.")

    # -- paths -----------------------------------------------------------------

    def _workspace(self, slot: str) -> Path:
        """Host-side per-agent workspace dir for `slot`, resolved like the
        adapter's discover_agents: the explicit `workspace` from openclaw.json
        (a container path, translated onto the host mount) when the agent
        recorded one, else the convention default — `<home>/workspace` for the
        pre-existing `main`, `<home>/workspace-<slot>` for a named agent
        (verified on 2026.6.11)."""
        cfg = self.env.host_home / "openclaw.json"
        if cfg.is_file():
            try:
                data = json.loads(cfg.read_text(encoding="utf-8"))
                for a in (data.get("agents", {}).get("list") or []):
                    if a.get("id") == slot:
                        ws = a.get("workspace")
                        if ws:
                            ws_path = Path(ws)
                            try:
                                rel = ws_path.relative_to(self.home_mount)
                                return self.env.host_home / rel
                            except ValueError:
                                return self.env.host_home / ws_path.name
                        break
            except (json.JSONDecodeError, OSError):
                pass
        return self.env.host_home / ("workspace" if slot == "main"
                                     else f"workspace-{slot}")

    def _memory_files(self, slot: str) -> list:
        """The slot's memory-bearing markdown — every `*.md` alf backs up from the
        workspace (`MEMORY.md`, `memory/**`, and the identity files). A real agent
        turn scatters memories across these (e.g. a credential into `TOOLS.md`),
        so coverage reads the whole captured store, not just `MEMORY.md`. `.git/`
        internals are excluded."""
        ws = self._workspace(slot)
        if not ws.is_dir():
            return []
        return [p for p in sorted(ws.rglob("*.md")) if ".git" not in p.parts]

    # -- Z1 --------------------------------------------------------------------

    def install_probe(self, ctr) -> dict:
        proc = ctr.exec(["openclaw", "--version"], timeout=120)
        # Output is "OpenClaw <version> (<commit>)"; extract the version token so
        # Z1's `== pinned_version` holds (mirrors ZeroClaw stripping its prefix).
        raw = ((proc.stdout or "") + (proc.stderr or "")).strip()
        import re
        m = re.search(r"\d+\.\d+\.\d+", raw)
        version = m.group(0) if m else raw
        topology = []
        if self.env.host_home.is_dir():
            topology = [str(p.relative_to(self.env.host_home))
                        for p in sorted(self.env.host_home.rglob("*")) if p.is_file()]
        declared, config = [], {}
        cfg = self.env.host_home / "openclaw.json"
        if cfg.is_file():
            try:
                config = json.loads(cfg.read_text(encoding="utf-8"))
                declared = [a["id"] for a in (config.get("agents", {}).get("list") or [])
                            if "id" in a]
            except (json.JSONDecodeError, OSError):
                pass
        return {"version": version, "topology": topology,
                "declared_agents": sorted(set(declared)), "config": config}

    def wire_llm(self, ctr, creds) -> None:
        """Point OpenClaw's model provider at the LLM proxy via `openclaw config
        set` (the run-agents.sh mechanism), keeping the raw key out of any
        committed file — it rides the gitignored run dir + container env."""
        base = creds.llm_proxy_url.rstrip("/")
        if not base.endswith("/v1"):
            base += "/v1"
        provider = json.dumps({
            "baseUrl": base,
            "apiKey": creds.runtime_api_key,
            "api": "openai-completions",
            "models": [{"id": creds.llm_model_id, "name": creds.llm_model_id}],
        })
        ctr.exec(["openclaw", "config", "set",
                  "models.providers.agent-life", provider], timeout=60)
        ctr.exec(["openclaw", "config", "set",
                  "agents.defaults.model", f"agent-life/{creds.llm_model_id}"], timeout=60)

    # -- Z2 --------------------------------------------------------------------

    def seed_markers(self, ctr, slot: str, round: int) -> None:
        seeder.seed_round(self._workspace(slot), slot, scenario.turns(slot, round))

    def llm_turn(self, ctr, slot: str, turn) -> TurnLog:
        proc = ctr.exec(
            ["openclaw", "agent", "--local", "--agent", slot,
             "--session-id", f"conv-{slot}", "-m", turn.prompt],
            timeout=200,
        )
        tail = "\n".join(((proc.stdout or "") + (proc.stderr or "")).splitlines()[-10:])
        return TurnLog(slot=slot, turn_type=turn.turn_type, marker=turn.marker,
                       prompt=turn.prompt, response_tail=tail,
                       ok=proc.returncode == 0)

    def dump_memory(self, ctr, slot: str) -> str:
        return "\n".join(p.read_text(encoding="utf-8", errors="replace")
                         for p in self._memory_files(slot))

    def placement(self, ctr, slot: str, markers: list) -> list:
        ws = self._workspace(slot)
        out = []
        for p in self._memory_files(slot):
            text = p.read_text(encoding="utf-8", errors="replace")
            for m in markers:
                if m in text:
                    out.append(PlacementRow(slot=slot, category="markdown",
                                            key=str(p.relative_to(ws)), head=m))
        return out

    def native_memory_stats(self, ctr, slot: str) -> dict:
        """OpenClaw memory is markdown; count the slot's memory items
        (MEMORY.md `##`/`-` entries + `memory/*.md` files) as the parity oracle.
        (Prefer an `openclaw memory` listing if the pinned image exposes one.)"""
        count = 0
        for p in self._memory_files(slot):
            if p.name == "MEMORY.md":
                count += sum(1 for line in p.read_text(encoding="utf-8", errors="replace").splitlines()
                             if line.startswith("## ") or line.startswith("- "))
            else:
                count += 1
        return {"count": count, "source": "workspace markdown count"}

    # -- Z8 / Z12 (multi-agent + restore) --------------------------------------

    def create_agent(self, ctr, slot: str) -> None:
        """`openclaw agents add <name> --non-interactive --workspace <dir>` — the
        idiomatic multi-agent verb (`--non-interactive` requires `--workspace`)."""
        ws = f"{self.home_mount}/workspace-{slot}"
        ctr.exec(["openclaw", "agents", "add", slot,
                  "--non-interactive", "--workspace", ws], timeout=120)

    def agent_declared(self, ctr, slot: str) -> bool:
        cfg = self.env.host_home / "openclaw.json"
        if not cfg.is_file():
            return False
        try:
            data = json.loads(cfg.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            return False
        return any(a.get("id") == slot for a in (data.get("agents", {}).get("list") or []))

    def is_per_agent_workspace(self, ws: str) -> bool:
        """OpenClaw maps each agent to a workspace dir under the install root
        (`<home>/workspace` for main, `<home>/workspace-<name>` for named)."""
        return ws.startswith(self.home_mount)

    def raw_parity_entry(self) -> str:
        """OpenClaw has no config file; its durable raw source is MEMORY.md."""
        return f"raw/{self.name}/MEMORY.md"

    def llm_wired(self) -> tuple:
        """OpenClaw wires the proxy as the `agent-life` provider in openclaw.json."""
        cfg = self.env.host_home / "openclaw.json"
        text = cfg.read_text(encoding="utf-8") if cfg.is_file() else ""
        return ("agent-life" in text, text)

    def archive_marker_prefix(self) -> str:
        """Tier-aware. The deterministic seeded tier writes markers into MEMORY.md,
        which the adapter extracts into the structured `memory/` layer — so it
        holds the parser to the same bar as ZeroClaw (`memory/`). A real LLM turn
        may scatter a marker into any workspace file (a credential into TOOLS.md →
        `raw/`), so the proxy tier scans the whole archive."""
        return "" if self.env.llm == "proxy" else "memory/"

    def mutate_slice(self, ctr, slot: str, round: int) -> None:
        """Diverge the slot's MEMORY.md from its archive: append a mutation
        marker and drop one existing memory line, so restore has both an
        overwrite and a re-add to correct. Edited host-side on the mounted dir."""
        mem = self._workspace(slot) / "MEMORY.md"
        if not mem.is_file():
            raise RuntimeError(f"mutate_slice: {mem} does not exist")
        lines = mem.read_text(encoding="utf-8").splitlines()
        # Any existing non-blank, non-heading line (a seeded `- ` bullet OR prose a
        # real LLM turn wrote) — restore must bring it back.
        content = [ln for ln in lines if ln.strip() and not ln.lstrip().startswith("#")]
        if not content:
            raise RuntimeError(f"mutate_slice: no content lines for slot '{slot}'")
        dropped = content[-1]
        kept = [ln for ln in lines if ln != dropped]
        kept.append("[[MUTATED — should be reverted by restore]]")
        mem.write_text("\n".join(kept) + "\n", encoding="utf-8")

    # -- Z14 (WP4.1 curated in-place memory) -------------------------------------

    # A deterministic 3-section MEMORY.md the curation ops operate on. Written
    # by the `reset` op so Z14 is independent of whatever a real LLM left in
    # the file — reorder/insert/delete always have the structure they need, and
    # `edit` always has the round-1 semantic marker to overwrite. No code
    # fences (so the split is unambiguous) and no trailing-blank games.
    def _reset_baseline(self, slot: str) -> str:
        marker = scenario.marker_for(slot, "semantic", 1)
        return (
            "# Memory\n\n"
            f"## Identity\n\nReference code: {marker}\n\n"
            "## Preferences\n\nTerse answers. Metric units.\n\n"
            "## Projects\n\nReef-camera build; tide-log automation.\n"
        )

    def curate_memory(self, ctr, slot: str, op: str) -> None:
        """One in-place curation of the slot's MEMORY.md, host-side on the
        mounted workspace (the mutate_slice pattern). The ops mirror what a
        real `openclaw agent` turn was observed doing (WP4.1 brief §1):
        reset / touch / reorder / edit / insert / delete. `reset` writes a
        deterministic multi-section baseline so the structural ops never depend
        on the model's output shape (Z14 setup)."""
        mem = self._workspace(slot) / "MEMORY.md"
        if op == "reset":
            mem.parent.mkdir(parents=True, exist_ok=True)
            mem.write_text(self._reset_baseline(slot), encoding="utf-8")
            return
        if not mem.is_file():
            raise RuntimeError(f"curate_memory: {mem} does not exist")
        text = mem.read_text(encoding="utf-8")

        if op == "touch":
            # Identical bytes, fresh mtime: the classic false-positive source.
            mem.write_text(text, encoding="utf-8")
        elif op == "reorder":
            preamble, sections = self._split_sections(text)
            if len(sections) < 2:
                raise RuntimeError("curate_memory: need >= 2 sections to reorder")
            mem.write_text(preamble + "".join(reversed(sections)), encoding="utf-8")
        elif op == "edit":
            # The §1a shape: overwrite the round-1 semantic marker in its slot.
            old = scenario.marker_for(slot, "semantic", 1)
            new = scenario.curated_marker(slot)
            if old in text:
                mem.write_text(text.replace(old, new, 1), encoding="utf-8")
            else:
                # Proxy tier: a real model may already have curated the marker
                # away (Z5). Still exercise an in-place body edit.
                lines = text.splitlines()
                for i, ln in enumerate(lines):
                    if ln.strip() and not ln.lstrip().startswith("#"):
                        lines[i] = f"{ln}  (curated: {new})"
                        break
                else:
                    raise RuntimeError("curate_memory: no content line to edit")
                mem.write_text("\n".join(lines) + "\n", encoding="utf-8")
        elif op == "insert":
            preamble, sections = self._split_sections(text)
            if not sections:
                raise RuntimeError("curate_memory: no sections to insert between")
            mid = max(1, len(sections) // 2)
            sections.insert(
                mid,
                "## Curated insert (Z14)\n\nReconcile must see exactly one new record.\n\n")
            mem.write_text(preamble + "".join(sections), encoding="utf-8")
        elif op == "delete":
            preamble, sections = self._split_sections(text)
            kept = [s for s in sections if not s.startswith("## Curated insert (Z14)")]
            if len(kept) == len(sections):
                raise RuntimeError("curate_memory: nothing to delete (run 'insert' first)")
            mem.write_text(preamble + "".join(kept), encoding="utf-8")
        else:
            raise RuntimeError(f"curate_memory: unknown op {op!r}")

    @staticmethod
    def _split_sections(text: str) -> tuple:
        """(preamble, [section chunks]) split on `## ` headings — the same
        top-level shape the adapter chunks, and fence-aware to match it: a
        `## ` line inside a ``` fenced block is body, not a boundary (see the
        adapter's split_markdown_sections). Each chunk is newline-terminated so
        re-concatenation in any order cannot merge two sections onto one line."""
        lines = text.splitlines(keepends=True)
        preamble, sections, current = [], [], None
        in_fence = False
        for ln in lines:
            if ln.lstrip().startswith("```"):
                in_fence = not in_fence
                (preamble if current is None else current).append(ln)
                continue
            if not in_fence and ln.startswith("## "):
                if current is not None:
                    sections.append("".join(current))
                current = [ln]
            elif current is None:
                preamble.append(ln)
            else:
                current.append(ln)
        if current is not None:
            sections.append("".join(current))
        sections = [s if s.endswith("\n") else s + "\n" for s in sections]
        return "".join(preamble), sections

    def assert_restore_isolation(self, run, result, slot: str) -> None:
        """Directory-isolation restore oracle: baseline the target + every other
        agent's workspace, diverge the target, `alf restore --agent <slot>`, and
        assert the target returns to the archive while each OTHER agent's dir
        stays byte-identical. `.git/` internals are excluded (not agent memory)."""
        slot_a = self.agent_slots[0]
        other_slots = [s for s in (slot_a, run.state.get("slot_b", "agent_b")) if s != slot]

        target = self._workspace(slot)
        before_target = self._snap(target)
        before_others = {s: self._snap(self._workspace(s)) for s in other_slots}

        self.mutate_slice(run.container, slot, round=2)
        mutated = self._snap(target)
        result.add(Check(
            name="mutate diverged agent b's workspace from its archive",
            status="PASS" if not snapshots.is_empty(snapshots.diff(before_target, mutated)) else "FAIL"))

        proc, res = run.container.exec_json(
            ["alf", "restore", "-r", self.name, "--agent", slot], timeout=300)
        result.add(Check(
            name=f"alf restore --agent {slot} (per-workspace)",
            status="PASS" if (proc.returncode == 0 and bool(res) and res.get("ok")) else "FAIL",
            detail=(proc.stderr or "")[:120] if proc.returncode else ""))

        d_target = snapshots.diff(before_target, self._snap(target))
        result.add(Check(
            name="restore: agent b's workspace equals the archive",
            status="PASS" if snapshots.is_empty(d_target) else "FAIL", detail=str(d_target)))

        # Non-vacuity guard: the "unchanged" claim only means something if the
        # other agents actually have files to leave alone.
        have_others = any(before_others[s] for s in other_slots)
        all_ok, detail = have_others, "" if have_others else "no other-agent files to compare"
        for s in other_slots:
            d = snapshots.diff(before_others[s], self._snap(self._workspace(s)))
            if not snapshots.is_empty(d):
                all_ok, detail = False, f"{s}: {d}"
        result.add(Check(
            name="restore leaves every OTHER agent's workspace byte-identical",
            status="PASS" if all_ok else "FAIL", detail=detail))

    @staticmethod
    def _snap(root: Path) -> dict:
        """snapshots.snapshot minus `.git/` internals (git churn is not memory)."""
        return {k: v for k, v in snapshots.snapshot(root).items()
                if not k.startswith(".git/") and "/.git/" not in k}

    # -- alf addressing --------------------------------------------------------

    def alf_target_args(self, slot: str) -> list:
        return ["-r", self.name]


KIT_CLASS = OpenClawKit
