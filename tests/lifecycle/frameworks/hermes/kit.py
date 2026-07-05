"""HermesKit — WP5 framework plug-in (profile-isolated topology).

Hermes is profile-isolated: each agent is a Hermes *profile* with its own
`state.db` (session-keyed, NO agent column) + curated `memories/*.md`. The
default profile is `~/.hermes` itself — interleaved with the shared runtime
(`node/`, `bin/`, `hermes-agent/`, caches); named profiles are clean at
`~/.hermes/profiles/<name>/`. One profile = one ALF agent. Isolation is a
filesystem fact (like OpenClaw), so `assert_restore_isolation` compares the
OTHER profiles' agent data rather than SQL-slicing one store (contrast
ZeroClaw's shared brain.db).

Verified verbs (adapter-hermes/testkit scripts, pinned hermes v0.17.0):
  * create → `hermes profile create <name> --no-skills`
  * list   → `hermes profile list`
  * turn   → `hermes -p <profile> chat -Q -q <prompt>`
  * version→ `hermes --version` → "Hermes Agent v0.17.0 (2026.6.19) · upstream …"
Config is `config.yaml` (a YAML model/agent mapping), NOT `config.toml`.

memory_shape = "append": Hermes memory is append/stable-id shaped (curated
entries carry content-derived ids; sessions carry native ids), so Z14 (curated
in-place ops) SKIPs and Z5's round-1 survival stays strict. Export excludes the
shared runtime + `.env` + `state.db` binary by an allowlist (D7), so the
default-profile archive carries agent data only.
"""

from __future__ import annotations

import json
import re
import sqlite3
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import seed_markers as seeder  # noqa: E402
from alflab import scenario, snapshots  # noqa: E402
from alflab.contract import FrameworkKit, PlacementRow, TurnLog  # noqa: E402
from alflab.report import Check  # noqa: E402


class HermesKit(FrameworkKit):
    name = "hermes"
    pinned_version = "0.17.0"
    image_tag = "alf-lifecycle-hermes"
    home_mount = "/home/agent/.hermes"
    agent_slots = ["default"]              # the default profile is a real agent
    config_paths = ["config.yaml"]
    memory_store_label = "memories/*.md + state.db"
    memory_topology = "isolated"          # one profile (HERMES_HOME) per agent
    memory_shape = "append"               # curated stable-id + append-only sessions
    # Hermes installs its runtime (hermes-agent/venv/node/bin) INSIDE ~/.hermes,
    # which the per-run home mount would shadow. Seed the run's home from the
    # image so it is the real colocated install a user has (runtime + data).
    seed_home_from_image = True

    # Raw-carried verbatim agent data — the only surfaces a same-runtime restore
    # reproduces byte-for-byte (config.yaml is exported REDACTED and state.db is
    # REBUILT from session records, so both are excluded from the restore oracle).
    _RAW_TOP_DIRS = ("memories", "cron", "skill-bundles")
    _RAW_ROOT_FILES = ("SOUL.md",)

    def seed_narrative(self) -> str:
        return ("No-LLM tier: the seeder appends the four round-1 markers as `§`-"
                "entries to the profile's memories/MEMORY.md — Hermes's curated "
                "store, no schema, no DB. Deterministic plumbing, same store a real "
                "turn consolidates into, no model.")

    def seed_flow(self) -> str:
        return "seed_markers.py ──append `§`-entries──▶ <profile>/memories/MEMORY.md"

    def isolation_narrative(self) -> str:
        return ("Populate agent b's OWN marked memories in its profile, sync, and "
                "assert isolation BOTH ways: b's archive carries only b's markers, "
                "and the default profile's agent data is untouched. Separate "
                "HERMES_HOME profiles keep them isolated — there is no shared store.")

    # -- paths -----------------------------------------------------------------

    def _profile_dir(self, slot: str) -> Path:
        """Host-side profile dir for `slot`: the default profile is the home
        mount itself (`~/.hermes`), a named profile is `profiles/<slot>/` —
        exactly what discover_agents enumerates."""
        if slot == "default":
            return self.env.host_home
        return self.env.host_home / "profiles" / slot

    def _container_profile(self, slot: str) -> str:
        """Container-side HERMES_HOME for `slot`. Each Hermes profile IS a
        separate HERMES_HOME (there is no `-p`/`--profile` flag in the pinned
        CLI; `hermes profile use` is sticky-global), so a turn targets a profile
        by pointing HERMES_HOME at its dir — the default profile at the home
        mount, a named profile at `profiles/<slot>/`."""
        if slot == "default":
            return self.home_mount
        return f"{self.home_mount}/profiles/{slot}"

    def _write_provider_config(self, profile_dir: Path, creds) -> None:
        """Write the LLM-proxy provider into a profile's config.yaml (host-side
        on the mounted profile dir). Each profile is a separate HERMES_HOME, so
        each needs its own config — a named profile created by `hermes profile
        create` has none until written here."""
        base = creds.llm_proxy_url.rstrip("/")
        if not base.endswith("/v1"):
            base += "/v1"
        profile_dir.mkdir(parents=True, exist_ok=True)
        (profile_dir / "config.yaml").write_text(
            "model:\n"
            f'  default: "{creds.llm_model_id}"\n'
            '  provider: "custom"\n'
            f'  base_url: "{base}"\n'
            f'  api_key: "{creds.runtime_api_key}"\n'
            "agent:\n"
            "  max_turns: 12\n",
            encoding="utf-8",
        )

    def _memory_files(self, slot: str) -> list:
        """The profile's curated memory files (`memories/*.md` — MEMORY.md +
        USER.md)."""
        mem = self._profile_dir(slot) / "memories"
        if not mem.is_dir():
            return []
        return sorted(mem.glob("*.md"))

    def _session_text(self, slot: str) -> str:
        """All session message text from the profile's state.db (empty when the
        DB is lazily absent — the no-LLM tier). Read-only."""
        db = self._profile_dir(slot) / "state.db"
        if not db.is_file():
            return ""
        try:
            conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
            try:
                rows = conn.execute(
                    "SELECT content FROM messages WHERE content IS NOT NULL"
                ).fetchall()
            finally:
                conn.close()
            return "\n".join(r[0] for r in rows if r[0])
        except sqlite3.Error:
            return ""

    def _db_count(self, slot: str, table: str) -> int:
        db = self._profile_dir(slot) / "state.db"
        if not db.is_file():
            return 0
        try:
            conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
            try:
                return conn.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
            finally:
                conn.close()
        except sqlite3.Error:
            return 0

    @staticmethod
    def _curated_entries(text: str) -> int:
        if "§" in text:
            return len([e for e in text.split("§") if e.strip()])
        return 1 if text.strip() else 0

    # -- Z1 --------------------------------------------------------------------

    def install_probe(self, ctr) -> dict:
        proc = ctr.exec(["hermes", "--version"], timeout=120)
        # Output is "Hermes Agent v0.17.0 (2026.6.19) · upstream <hash>"; extract
        # the semver token so Z1's `== pinned_version` holds (mirrors ZeroClaw/
        # OpenClaw). The Dockerfile guard is already substring-tolerant.
        raw = ((proc.stdout or "") + (proc.stderr or "")).strip()
        m = re.search(r"\d+\.\d+\.\d+", raw)
        version = m.group(0) if m else raw
        # Top-level layout (small + informative — the shared runtime is visible
        # here but excluded from the default-profile archive by the adapter).
        topology = []
        if self.env.host_home.is_dir():
            topology = sorted(p.name for p in self.env.host_home.iterdir())
        return {"version": version, "topology": topology,
                "declared_agents": self._declared_profiles(), "config": {}}

    def _declared_profiles(self) -> list:
        names = ["default"]
        pd = self.env.host_home / "profiles"
        if pd.is_dir():
            names += [d.name for d in pd.iterdir()
                      if d.is_dir() and not d.name.startswith(".")]
        return sorted(set(names))

    def wire_llm(self, ctr, creds) -> None:
        """Point the default profile's model provider at the LLM proxy by
        writing its config.yaml host-side (Hermes has no `config set` verb in
        scope; the ZeroClaw host-side-write model). Named profiles are wired at
        create_agent time. The raw key lives only in the gitignored, chmod-700
        run dir — never a committed file."""
        self._write_provider_config(self.env.host_home, creds)

    # -- Z2 --------------------------------------------------------------------

    def seed_markers(self, ctr, slot: str, round: int) -> None:
        seeder.seed_round(self._profile_dir(slot), slot, scenario.turns(slot, round))

    def llm_turn(self, ctr, slot: str, turn) -> TurnLog:
        # Target the profile via its own HERMES_HOME (see _container_profile);
        # `hermes chat -Q` is one-shot/quiet for programmatic use.
        proc = ctr.exec(
            ["hermes", "chat", "-Q", "-q", turn.prompt],
            env={"HERMES_HOME": self._container_profile(slot)},
            timeout=240,
        )
        tail = "\n".join(((proc.stdout or "") + (proc.stderr or "")).splitlines()[-10:])
        return TurnLog(slot=slot, turn_type=turn.turn_type, marker=turn.marker,
                       prompt=turn.prompt, response_tail=tail,
                       ok=proc.returncode == 0)

    def dump_memory(self, ctr, slot: str) -> str:
        parts = [p.read_text(encoding="utf-8", errors="replace")
                 for p in self._memory_files(slot)]
        # Include session message text so proxy-tier markers written into
        # state.db (episodic turns) are covered too.
        parts.append(self._session_text(slot))
        return "\n".join(parts)

    def placement(self, ctr, slot: str, markers: list) -> list:
        out = []
        prof = self._profile_dir(slot)
        for p in self._memory_files(slot):
            text = p.read_text(encoding="utf-8", errors="replace")
            for m in markers:
                if m in text:
                    out.append(PlacementRow(slot=slot, category="curated",
                                            key=str(p.relative_to(prof)), head=m))
        stext = self._session_text(slot)
        for m in markers:
            if m in stext:
                out.append(PlacementRow(slot=slot, category="session",
                                        key="state.db", head=m))
        return out

    def native_memory_stats(self, ctr, slot: str) -> dict:
        """Count the profile's curated `§`-entries + session/message rows as the
        parity oracle (no `hermes memory list` verb exists on the pinned image).
        Messages are counted alongside sessions so the proxy-tier `count >= 4`
        gate holds even if the four turns land in fewer than four sessions."""
        count = 0
        for p in self._memory_files(slot):
            count += self._curated_entries(p.read_text(encoding="utf-8", errors="replace"))
        count += self._db_count(slot, "sessions") + self._db_count(slot, "messages")
        return {"count": count, "source": "curated §-entries + sessions + messages"}

    # -- Z8 / Z12 (multi-agent + restore) --------------------------------------

    def create_agent(self, ctr, slot: str) -> None:
        """`hermes profile create <name> --no-skills` — scaffolds a clean,
        isolated profile at `profiles/<name>/` (skills opt-out keeps the create
        fast + offline)."""
        ctr.exec(["hermes", "profile", "create", slot, "--no-skills"], timeout=120)
        # On the proxy tier the named profile (a separate HERMES_HOME) needs its
        # OWN provider config before its turns (Z10) run — `profile create` writes
        # none, and profiles don't inherit the root config. No-op on --llm none.
        if self.env.llm == "proxy" and self.env.creds is not None:
            self._write_provider_config(self._profile_dir(slot), self.env.creds)

    def agent_declared(self, ctr, slot: str) -> bool:
        # `hermes profile create` materializes profiles/<slot>/ (what
        # discover_agents enumerates); cross-check `hermes profile list`.
        if (self.env.host_home / "profiles" / slot).is_dir():
            return True
        proc = ctr.exec(["hermes", "profile", "list"], timeout=60)
        return slot in ((proc.stdout or "") + (proc.stderr or ""))

    def is_per_agent_workspace(self, ws: str) -> bool:
        """Hermes maps each agent to a profile dir under the install root: the
        default profile is `~/.hermes` itself, named profiles are
        `~/.hermes/profiles/<name>/`."""
        return ws.startswith(self.home_mount)

    def raw_parity_entry(self) -> str:
        """Hermes's synthesized config.yaml is always in the archive under
        raw/hermes/ (redacted, unfilterable) — a stable Z4 parity anchor."""
        return f"raw/{self.name}/config.yaml"

    def llm_wired(self) -> tuple:
        """Hermes wires the proxy as the `custom` provider in config.yaml."""
        cfg = self.env.host_home / "config.yaml"
        text = cfg.read_text(encoding="utf-8") if cfg.is_file() else ""
        return ('provider: "custom"' in text and "base_url" in text, text)

    def archive_marker_prefix(self) -> str:
        """Both curated entries and sessions extract into the structured
        `memory/` layer, so markers are scanned there (like ZeroClaw)."""
        return "memory/"

    def mutate_slice(self, ctr, slot: str, round: int) -> None:
        """Diverge the profile's memories/MEMORY.md from its archive: append a
        mutation marker and drop one existing curated entry, so restore has both
        an overwrite and a re-add to correct. Edited host-side on the mounted
        dir; the raw layer restores it byte-for-byte."""
        mem = self._profile_dir(slot) / "memories" / "MEMORY.md"
        if not mem.is_file():
            raise RuntimeError(f"mutate_slice: {mem} does not exist")
        lines = mem.read_text(encoding="utf-8").splitlines()
        content = [ln for ln in lines if ln.strip() and ln.strip() != "§"]
        if not content:
            raise RuntimeError(f"mutate_slice: no content lines for slot '{slot}'")
        dropped = content[-1]
        kept = [ln for ln in lines if ln != dropped]
        kept.append("[[MUTATED — should be reverted by restore]]")
        mem.write_text("\n".join(kept) + "\n", encoding="utf-8")

    def assert_restore_isolation(self, run, result, slot: str) -> None:
        """Profile-isolation restore oracle (the OpenClaw dir-isolated shape):
        baseline the target profile + every other profile's raw-carried agent
        data, diverge the target, `alf restore --agent <slot>`, and assert the
        target returns to the archive while each OTHER profile stays
        byte-identical. `_snap` restricts to the raw-carried verbatim surfaces
        (memories/, SOUL.md, cron/, skill-bundles/) — config.yaml is exported
        redacted and state.db is rebuilt, so neither round-trips byte-identical
        and both are excluded; the shared runtime + nested profiles are excluded
        too (they are not this profile's agent data)."""
        slot_a = self.agent_slots[0]
        other_slots = [s for s in (slot_a, run.state.get("slot_b", "agent_b")) if s != slot]

        target = self._profile_dir(slot)
        before_target = self._snap(target)
        # For OTHER profiles a correct restore must touch NOTHING, so compare the
        # wider surface (incl. state.db + config.yaml) — unlike the target, whose
        # state.db is legitimately rebuilt and config.yaml redacted.
        before_others = {s: self._snap_other(self._profile_dir(s)) for s in other_slots}

        self.mutate_slice(run.container, slot, round=2)
        mutated = self._snap(target)
        result.add(Check(
            name="mutate diverged agent b's profile from its archive",
            status="PASS" if not snapshots.is_empty(snapshots.diff(before_target, mutated)) else "FAIL"))

        proc, res = run.container.exec_json(
            ["alf", "restore", "-r", self.name, "--agent", slot], timeout=300)
        result.add(Check(
            name=f"alf restore --agent {slot} (per-profile)",
            status="PASS" if (proc.returncode == 0 and bool(res) and res.get("ok")) else "FAIL",
            detail=(proc.stderr or "")[:120] if proc.returncode else ""))

        d_target = snapshots.diff(before_target, self._snap(target))
        result.add(Check(
            name="restore: agent b's profile equals the archive",
            status="PASS" if snapshots.is_empty(d_target) else "FAIL", detail=str(d_target)))

        # Non-vacuity guard: the "unchanged" claim only means something if the
        # other profiles actually have raw-carried files to leave alone.
        have_others = any(before_others[s] for s in other_slots)
        all_ok, detail = have_others, "" if have_others else "no other-profile files to compare"
        for s in other_slots:
            d = snapshots.diff(before_others[s], self._snap_other(self._profile_dir(s)))
            if not snapshots.is_empty(d):
                all_ok, detail = False, f"{s}: {d}"
        result.add(Check(
            name="restore leaves every OTHER profile's agent data byte-identical",
            status="PASS" if all_ok else "FAIL", detail=detail))

    def _snap(self, root: Path) -> dict:
        """snapshots.snapshot restricted to the raw-carried verbatim agent data
        (memories/, SOUL.md, cron/, skill-bundles/). Excludes the redacted
        config.yaml, the rebuilt state.db, the shared runtime (node/, bin/,
        hermes-agent/, caches), nested profiles/, sessions/, logs/ and .env —
        none of which a same-runtime restore reproduces byte-for-byte. Used for
        the TARGET profile's "equals the archive" check."""
        keep = {}
        for rel, digest in snapshots.snapshot(root).items():
            top = rel.split("/", 1)[0]
            if rel in self._RAW_ROOT_FILES or top in self._RAW_TOP_DIRS:
                keep[rel] = digest
        return keep

    def _snap_other(self, root: Path) -> dict:
        """Like `_snap` but ALSO includes `state.db` + `config.yaml`. Used for
        NON-target profiles: restoring one profile must not touch another's data
        at all, so a wiped/rebuilt other-profile state.db or config.yaml is a
        real isolation failure this catches (the target snapshot excludes them
        because its own restore legitimately rewrites them)."""
        keep = {}
        for rel, digest in snapshots.snapshot(root).items():
            top = rel.split("/", 1)[0]
            if (rel in self._RAW_ROOT_FILES or top in self._RAW_TOP_DIRS
                    or rel in ("state.db", "config.yaml")):
                keep[rel] = digest
        return keep

    # -- alf addressing --------------------------------------------------------

    def alf_target_args(self, slot: str) -> list:
        return ["-r", self.name]


KIT_CLASS = HermesKit
