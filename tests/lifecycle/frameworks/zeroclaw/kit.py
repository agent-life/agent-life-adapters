"""ZeroClawKit — the WP2 pilot framework plug-in (full Z1–Z4+Z13 support).

Ported from adapter-zeroclaw/testkit (setup-agents.sh / converse.sh / captured
DDL), adapted to the pilot's Phase-1 single agent: a bare `--skip-quickstart`
install has NO declared `[agents.*]` blocks — the framework's implicit sole
agent (brain.db alias `default`, created by ZeroClaw itself on first
`memory reindex`), which alf's WP0 M=1 fallback discovery maps 1:1.

Empirically verified against the pinned 0.8.2 image:
  * `zeroclaw status` materializes ~/.zeroclaw/config.toml (`schema_version = 3`);
  * `zeroclaw memory reindex` materializes data/memory/brain.db with the REAL
    captured schema AND inserts the implicit `default` agents row — no LLM.
"""

from __future__ import annotations

import sqlite3
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))       # tests/lifecycle
sys.path.insert(0, str(Path(__file__).resolve().parent))           # this dir

import seed_markers as seeder  # noqa: E402
from alflab import scenario, sqlite_util  # noqa: E402
from alflab.contract import FrameworkKit, PlacementRow, TurnLog  # noqa: E402
from alflab.report import Check  # noqa: E402


class ZeroClawKit(FrameworkKit):
    name = "zeroclaw"
    pinned_version = "0.8.2"
    image_tag = "alf-lifecycle-zeroclaw"
    home_mount = "/home/agent/.zeroclaw"
    agent_slots = ["default"]
    config_paths = ["config.toml"]
    memory_store_label = "brain.db"                # one shared SQLite store
    memory_topology = "shared"                      # partitioned by agent_id

    def seed_narrative(self) -> str:
        return ("No-LLM tier: `zeroclaw memory reindex` materializes the EMPTY "
                "real-schema brain.db, then the seeder inserts the four round-1 "
                "marker rows through the real DDL (agents row ensured first; "
                "UNIQUE(agent_id,key) respected; embedding NULL; RFC3339 "
                "timestamps). Deterministic plumbing — same store, no model.")

    def seed_flow(self) -> str:
        return "seed_markers.py ──real DDL──▶ brain.db (FTS via triggers, never direct)"

    def isolation_narrative(self) -> str:
        return ("Populate agent b's OWN marked memories in the shared brain.db, "
                "sync, and assert isolation BOTH ways: b's archive carries only b's "
                "markers, and a's slice is untouched. The agent_id filter is what "
                "keeps the shared store clean.")

    # -- paths -----------------------------------------------------------------

    @property
    def _db(self) -> Path:
        return self.env.host_home / seeder.DB_RELPATH

    @property
    def _config(self) -> Path:
        return self.env.host_home / "config.toml"

    # -- Z1 ----------------------------------------------------------------------

    def install_probe(self, ctr) -> dict:
        version = (ctr.exec(["zeroclaw", "--version"]).stdout or "").strip()
        version = version.removeprefix("zeroclaw ").strip()
        # `status` materializes the declared config on a bare home (verified).
        ctr.exec(["zeroclaw", "status"], timeout=60)
        topology = []
        if self.env.host_home.is_dir():
            for p in sorted(self.env.host_home.rglob("*")):
                if p.is_file():
                    topology.append(str(p.relative_to(self.env.host_home)))
        text = self._config.read_text(encoding="utf-8") if self._config.is_file() else ""
        schema_version = None
        has_workspace_dir = False
        declared = []
        for line in text.splitlines():
            s = line.strip()
            if s.startswith("schema_version"):
                schema_version = s.split("=", 1)[1].strip()
            if s.startswith("workspace_dir"):
                has_workspace_dir = True
            if s.startswith("[agents.") and s.count(".") == 1:
                declared.append(s.strip("[]").split(".", 1)[1])
        return {
            "version": version,
            "topology": topology,
            "declared_agents": sorted(set(declared)),
            "config": {"schema_version": schema_version,
                       "has_workspace_dir": has_workspace_dir},
        }

    @staticmethod
    def _agent_block(alias: str) -> str:
        """The explicit, runnable `[agents.<alias>]` block — IDENTICAL for every
        agent (the `default` written here in `wire_llm` and any second agent
        added in `create_agent`). 0.8.2 requires an explicit `risk_profile` to
        run an agent standalone via `agent -a`; `zeroclaw agents create` alone
        writes a delegating profile that fails validation (see `create_agent`).
        Keeping the two agents symmetric is deliberate — a working second agent
        looks exactly like the first."""
        return (
            f"[agents.{alias}]\n"
            'model_provider = "custom.agentlife"\n'
            'risk_profile = "assistant"\n'
            'runtime_profile = "assistant"\n'
            'channels = ["cli"]\n'
        )

    def wire_llm(self, ctr, creds) -> None:
        """Point the sole agent at the LLM proxy (converse.sh wiring). 0.8.2
        requires a declared [agents.<alias>] block to drive an agent via the
        CLI (verified: bare `zeroclaw agent` demands --agent, and --agent
        errors without the block), so wiring declares the SAME alias the
        implicit agent already has in brain.db — `default` — keeping the WP0
        mapping and the framework's own store aligned. The bare install's
        declared set is recorded at Z1 BEFORE this runs (Z3-nuance evidence).
        Written host-side into the mounted home; the raw key lives only inside
        the gitignored, chmod-700 run dir."""
        base = creds.llm_proxy_url.rstrip("/")
        if not base.endswith("/v1"):
            base += "/v1"
        self._config.write_text(
            "schema_version = 3\n"
            'default_model_provider = "custom.agentlife"\n\n'
            "[providers.models.custom.agentlife]\n"
            f'uri = "{base}"\n'
            f'model = "{creds.llm_model_id}"\n'
            f'api_key = "{creds.runtime_api_key}"\n\n'
            + self._agent_block("default") + "\n"
            "[risk_profiles.assistant]\n"
            'level = "full"\n'
            "allowed_commands = []\n\n"
            "[runtime_profiles.assistant]\n"
            "agentic = true\n"
            "max_tool_iterations = 4\n"
            "max_actions_per_hour = 200\n\n"
            "[memory]\n"
            'backend = "sqlite"\n'
            "auto_save = true\n"
            'embedding_provider = "none"\n\n'
            "[secrets]\n"
            "encrypt = false\n",
            encoding="utf-8",
        )

    # -- Z2 ----------------------------------------------------------------------

    def _reindex(self, ctr) -> None:
        """Materialize the empty REAL-schema brain.db (+ the framework's own
        implicit `default` agents row) without an LLM."""
        ctr.exec(["zeroclaw", "memory", "reindex"], timeout=120)

    def seed_markers(self, ctr, slot: str, round: int) -> None:
        self._reindex(ctr)
        if not self._db.is_file():
            raise RuntimeError(f"reindex did not materialize {self._db}")
        seeder.seed_round(self._db, slot, scenario.turns(slot, round))

    def llm_turn(self, ctr, slot: str, turn) -> TurnLog:
        # 0.8.2 requires --agent even for the implicit sole agent (verified;
        # the plan's flag assumption adjusted per its own rule 9).
        sess = f"/home/agent/.zeroclaw/sess-{slot}.json"
        proc = ctr.exec(
            ["zeroclaw", "agent", "-a", slot, "--session-state-file", sess,
             "-m", turn.prompt],
            timeout=200,
        )
        tail = "\n".join(((proc.stdout or "") + (proc.stderr or "")).splitlines()[-10:])
        return TurnLog(slot=slot, turn_type=turn.turn_type, marker=turn.marker,
                       prompt=turn.prompt, response_tail=tail,
                       ok=proc.returncode == 0)

    def _slot_scope(self, conn, slot: str):
        """(where-clause, params) scoping a query to the slot's agent row —
        by alias, adopting the sole row when only one exists."""
        rows = conn.execute("SELECT id, alias FROM agents").fetchall()
        for row_id, alias in rows:
            if alias == slot:
                return "agent_id = ?", (row_id,)
        if len(rows) == 1:
            return "agent_id = ?", (rows[0][0],)
        return "1=1", ()

    def dump_memory(self, ctr, slot: str) -> str:
        """All memory content for the slot via ZeroClaw's OWN store (the
        agent-scoped join from converse.sh, host-side on the mounted db)."""
        if not self._db.is_file():
            return ""
        conn = sqlite3.connect(self._db)
        try:
            where, params = self._slot_scope(conn, slot)
            rows = conn.execute(
                f"SELECT content FROM memories WHERE {where}", params).fetchall()
            return "\n".join(r[0] for r in rows)
        finally:
            conn.close()

    def placement(self, ctr, slot: str, markers: list) -> list:
        if not self._db.is_file():
            return []
        conn = sqlite3.connect(self._db)
        try:
            where, params = self._slot_scope(conn, slot)
            rows = conn.execute(
                f"SELECT category, key, content FROM memories WHERE {where} "
                f"ORDER BY category, key", params).fetchall()
        finally:
            conn.close()
        out = []
        for category, key, content in rows:
            if any(m in content for m in markers):
                head = content.replace("\n", " ")[:55]
                out.append(PlacementRow(slot=slot, category=category, key=key, head=head))
        return out

    def native_memory_stats(self, ctr, slot: str) -> dict:
        """`zeroclaw memory stats` (the framework's own listing) with a
        DB-count fallback if the CLI output shape drifts."""
        proc = ctr.exec(["zeroclaw", "memory", "stats"], timeout=60)
        text = (proc.stdout or "") + (proc.stderr or "")
        import re

        m = re.search(r"(\d+)\s+(?:memor|entri|row)", text, re.IGNORECASE)
        if m:
            return {"count": int(m.group(1)), "source": "zeroclaw memory stats"}
        if self._db.is_file():
            conn = sqlite3.connect(self._db)
            try:
                where, params = self._slot_scope(conn, slot)
                n = conn.execute(
                    f"SELECT COUNT(*) FROM memories WHERE {where}", params).fetchone()[0]
                return {"count": n, "source": "brain.db count (stats output unparsed)"}
            finally:
                conn.close()
        return {"count": 0, "source": "no brain.db"}

    # -- Z8 / Z12 (WP3 multi-agent + restore) --------------------------------------

    @staticmethod
    def _replace_section(text: str, header: str, replacement: str) -> str:
        """Replace the BODY of the top-level TOML section `header` (from the
        header line up to the next `[...]` header or EOF) with `replacement`
        (which itself includes the header line). Sub-tables like `[<header>.x]`
        are preserved. If the section is absent, `replacement` is appended."""
        if header not in text:
            return f"{text.rstrip()}\n\n{replacement}"
        lines = text.splitlines(keepends=True)
        out, i, n = [], 0, len(lines)
        while i < n:
            if lines[i].strip() == header:
                out.append(replacement if replacement.endswith("\n") else replacement + "\n")
                out.append("\n")
                i += 1
                while i < n and not lines[i].lstrip().startswith("["):
                    i += 1
            else:
                out.append(lines[i])
                i += 1
        return "".join(out)

    def create_agent(self, ctr, slot: str) -> None:
        """Configure a second agent, symmetric with `default`.

        A real user runs `zeroclaw agents create <alias>` — but on 0.8.2 that
        writes an `[agents.<alias>]` block that DELEGATES its risk profile
        (`delegate_same_risk_profile = true`, no explicit `risk_profile`).
        Running `zeroclaw agent -a <alias>` standalone (the documented, required
        way to run an agent — there is no default agent) then fails validation
        ("agents.<alias>.risk_profile does not name a configured risk_profiles
        entry"), because delegation has no parent: the agent never initializes,
        its turns no-op, no memory is written, and no brain.db `agents` row is
        materialized (Z10 saw b=0/4). So after the CLI call we rewrite the top
        block to the SAME explicit form `wire_llm` gives `default`
        ([`_agent_block`]) — a working second agent looks exactly like the first.
        The brain.db row still materializes lazily, on the first `agent -a` turn
        (Z10). This mirrors what a real user must do on 0.8.2; the guide
        documents it as a ZeroClaw second-agent onboarding step."""
        ctr.exec(["zeroclaw", "agents", "create", slot], timeout=60)
        text = self._config.read_text(encoding="utf-8") if self._config.is_file() else ""
        self._config.write_text(
            self._replace_section(text, f"[agents.{slot}]", self._agent_block(slot)),
            encoding="utf-8",
        )

    def agent_declared(self, ctr, slot: str) -> bool:
        """ZeroClaw declares agents as `[agents.<alias>]` TOML blocks."""
        text = self._config.read_text(encoding="utf-8") if self._config.is_file() else ""
        return f"[agents.{slot}]" in text

    def is_per_agent_workspace(self, ws: str) -> bool:
        """ZeroClaw maps each agent to `<home>/agents/<alias>/workspace`."""
        return ws.startswith(self.home_mount) and "/agents/" in ws

    def mutate_slice(self, ctr, slot: str, round: int) -> None:
        """Diverge the slot's brain.db slice from its archive so restore has
        something to correct: rewrite one row's content and delete another —
        BOTH scoped to the slot's own agent_id, leaving every other agent's rows
        untouched. FTS is maintained by the update/delete triggers."""
        if not self._db.is_file():
            raise RuntimeError(f"mutate_slice: {self._db} does not exist")
        conn = sqlite3.connect(self._db)
        try:
            where, params = self._slot_scope(conn, slot)
            keys = [
                r[0]
                for r in conn.execute(
                    f"SELECT key FROM memories WHERE {where} ORDER BY key", params
                ).fetchall()
            ]
            if not keys:
                raise RuntimeError(f"mutate_slice: no rows for slot '{slot}'")
            # Corrupt the first row's content (restore must overwrite it).
            conn.execute(
                f"UPDATE memories SET content = ? WHERE {where} AND key = ?",
                ("[[MUTATED — should be overwritten by restore]]", *params, keys[0]),
            )
            # Delete the last row (restore must bring it back in total mode).
            if len(keys) > 1:
                conn.execute(
                    f"DELETE FROM memories WHERE {where} AND key = ?",
                    (*params, keys[-1]),
                )
            conn.commit()
        finally:
            conn.close()

    def assert_restore_isolation(self, run, result, slot: str) -> None:
        """Shared-store restore oracle: baseline brain.db, diverge the slot's
        slice, `alf restore --agent <slot>` (total), and assert the slice equals
        the archive again while every OTHER agent's rows stay byte-identical."""
        import shutil

        if not self._db.is_file():
            result.add(Check(name="brain.db present before restore", status="FAIL"))
            return
        baseline = run.paths.run_dir / "z12-brain-before.db"
        shutil.copy2(self._db, baseline)
        b_before = sqlite_util.agent_row_count(self._db, slot)

        self.mutate_slice(run.container, slot, round=2)
        mutated = sqlite_util.agent_row_count(self._db, slot)
        result.add(Check(
            name="mutate_slice diverged agent b's slice from its archive",
            status="PASS" if mutated != b_before else "FAIL",
            detail=f"rows {b_before} → {mutated}"))

        proc, res = run.container.exec_json(
            ["alf", "restore", "-r", self.name, "--agent", slot], timeout=300)
        result.add(Check(
            name=f"alf restore --agent {slot} (default = total)",
            status="PASS" if (proc.returncode == 0 and bool(res) and res.get("ok")) else "FAIL",
            detail=(proc.stderr or "")[:120] if proc.returncode else ""))

        restored = sqlite_util.agent_row_count(self._db, slot)
        result.add(Check(
            name="total restore: agent b's slice equals the archive",
            status="PASS" if restored == b_before else "FAIL",
            detail=f"rows now {restored} (was {b_before})"))
        ok, detail = sqlite_util.db_identical_except_agent(baseline, self._db, slot)
        result.add(Check(
            name="restore leaves every OTHER agent byte-identical",
            status="PASS" if ok else "FAIL", detail=detail))

    # -- alf addressing ------------------------------------------------------------

    def alf_target_args(self, slot: str) -> list:
        return ["-r", self.name]


KIT_CLASS = ZeroClawKit
