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
from alflab import scenario  # noqa: E402
from alflab.contract import FrameworkKit, PlacementRow, TurnLog  # noqa: E402


class ZeroClawKit(FrameworkKit):
    name = "zeroclaw"
    pinned_version = "0.8.2"
    image_tag = "alf-lifecycle-zeroclaw"
    home_mount = "/home/agent/.zeroclaw"
    agent_slots = ["default"]
    config_paths = ["config.toml"]

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
            "[agents.default]\n"
            'model_provider = "custom.agentlife"\n'
            'risk_profile = "assistant"\n'
            'runtime_profile = "assistant"\n'
            'channels = ["cli"]\n\n'
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

    # -- alf addressing ------------------------------------------------------------

    def alf_target_args(self, slot: str) -> list:
        return ["-r", self.name]


KIT_CLASS = ZeroClawKit
