"""GenericMcpKit — the curated in-place-edit MCP tier for the toy runtime.

`generic` proves the MCP transport (`alf mcp serve -r generic`, driven by the
harness's `McpInvoker`) but its store is *append-shaped*, so it skips Z14. This
variant makes Z14 — the WP4.1 curated in-place-edit → reconcile-delta test — run
**through the MCP `alf_sync` tool** by declaring a `curated` store and a
`curate_memory` that rewrites a `by_heading` journal in place (the same shape as
OpenClaw's `MEMORY.md`, but no framework install).

It also seeds a synthetic **SQLite memory source** (`brain.db`, mapped via the
adapter-generic `sqlite_rows` chunking) so the run exercises row-level memory
extraction through the MCP server. A SQLite row's ALF id is derived from its
primary key, so an in-place row `UPDATE` reconciles to exactly one `Update` —
proven exhaustively by the Rust unit tests in `adapter-generic/src/sqlite.rs`
(`in_place_row_edit_is_exactly_one_update`, insert/delete/touch). The generic
adapter treats the `.db` as raw for restore fidelity while its rows become
records; here it stays static during Z14 so the markdown delta shapes stay clean.

Tier: `--llm none` (no bespoke LLM host — the LLM gate is Hermes) with
`--backend real` for the Z14 delta lane. `run.alf` is the inherited `McpInvoker`,
so Z14's sync goes over the persistent `alf mcp serve` stdio session.
"""

from __future__ import annotations

import importlib.util
import sqlite3
import sys
from pathlib import Path

_LIFECYCLE = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(_LIFECYCLE))

from alflab import scenario  # noqa: E402
from alflab.contract import SkipStage  # noqa: E402

# Load the base GenericKit and subclass it. Loading generic/kit.py runs its own
# `import seed_markers`; save/restore the cache entry so a TEST SUITE loading
# several kits in one process keeps each framework's `import seed_markers`
# resolving to its own dir (mirrors hermes-mcp).
_GENERIC_KIT = _LIFECYCLE / "frameworks" / "generic" / "kit.py"
_saved_seed = sys.modules.pop("seed_markers", None)
try:
    _spec = importlib.util.spec_from_file_location("kit_generic_base", _GENERIC_KIT)
    _generic_mod = importlib.util.module_from_spec(_spec)
    _spec.loader.exec_module(_generic_mod)
    GenericKit = _generic_mod.KIT_CLASS
    _GENERIC_FIXTURE = _generic_mod.FIXTURE_DIR
finally:
    if _saved_seed is not None:
        sys.modules["seed_markers"] = _saved_seed
    else:
        sys.modules.pop("seed_markers", None)

# The Z14-owned curated journal file (a `by_heading` episodic source). Kept
# separate from the seeded round journals so curation never clobbers coverage.
_CURATED = "memories/curated.md"
# The synthetic SQLite memory store, mapped as a `sqlite_rows` source.
_BRAIN = "brain.db"


class GenericMcpKit(GenericKit):
    name = "generic"                          # the alf runtime is still generic
    image_tag = "alf-lifecycle-generic-mcp"   # layered on alf-lifecycle-generic
    memory_store_label = "mapped markdown + sqlite (.alf-map.json)"
    memory_shape = "curated"                  # rewritten in place → Z14 runs

    # -- fixture: reuse generic's tree, overlay the map, add brain.db ----------

    def _seed_fixture(self) -> None:
        """Copy the generic fixture, then overlay a map that adds the sqlite
        source and create the synthetic `brain.db` with baseline rows."""
        super()._seed_fixture()  # generic's IDENTITY/knowledge/procedures/map
        home = self.env.host_home
        (home / "memories").mkdir(parents=True, exist_ok=True)
        (home / ".alf-map.json").write_text(self._map_json(), encoding="utf-8")
        self._build_brain(home / _BRAIN, self._baseline_rows())

    def _map_json(self) -> str:
        # A by_heading journal (curation target + seeder sink) plus the sqlite
        # source. `filename_date` on the journal falls back to mtime for the
        # date-less curated.md, which is fine (reconcile carries created_at).
        import json

        return json.dumps(
            {
                "version": 1,
                "framework": "toybox-mcp",
                "framework_version": self.pinned_version,
                "identity_file": "IDENTITY.md",
                "memory_sources": [
                    {
                        "id": "journal",
                        "glob": "memories/*.md",
                        "memory_type": "episodic",
                        "namespace": "daily",
                        "chunking": "by_heading",
                        "timestamp": "filename_date",
                        "tags": ["hashtags"],
                    },
                    {
                        "id": "brain",
                        "glob": _BRAIN,
                        "memory_type": "semantic",
                        "namespace": "curated",
                        "chunking": "sqlite_rows",
                        "sqlite": {
                            "table": "memories",
                            "id_column": "id",
                            "content_column": "content",
                            "timestamp_column": "updated_at",
                        },
                    },
                ],
                "watch": {"default_interval": "5m", "per_source": {"journal": "1m"}},
            },
            indent=2,
        )

    @staticmethod
    def _build_brain(path: Path, rows: list[tuple[str, str]]) -> None:
        """(Re)create the synthetic brain.db with the given (id, content) rows.
        DELETE journal mode + a checkpoint keep the `.db` file self-contained, so
        the whole store travels raw and no `-wal` sidecar is left dangling."""
        if path.exists():
            path.unlink()
        conn = sqlite3.connect(path)
        try:
            conn.execute("PRAGMA journal_mode=DELETE")
            conn.execute(
                "CREATE TABLE memories (id TEXT PRIMARY KEY, content TEXT, updated_at TEXT)"
            )
            for rid, content in rows:
                conn.execute(
                    "INSERT INTO memories (id, content, updated_at) "
                    "VALUES (?, ?, '2026-01-01T00:00:00Z')",
                    (rid, content),
                )
            conn.commit()
        finally:
            conn.close()

    def _baseline_rows(self) -> list[tuple[str, str]]:
        # Neutral facts — deliberately NOT the scenario marker, so the sqlite
        # store is independent memory and never confuses Z14's markdown checks.
        return [
            ("fact-1", "The reef-camera uploads a frame at 06:00 UTC daily."),
            ("fact-2", "Prefers metric units and terse answers."),
            ("fact-3", "The tide-log automation runs on a Raspberry Pi."),
        ]

    # -- Z2 seed: into the SINGLE curated journal (the store Z14 rewrites) ------

    def seed_markers(self, ctr, slot: str, round: int) -> None:
        """Seed the round's markers as `## ` sections into the ONE curated
        journal — the OpenClaw single-file curated shape, so Z14's `reset` wipes
        seeded content and its `old marker absent` check holds (a separate dated
        journal would keep the round-1 marker and break it)."""
        mem = self.env.host_home / _CURATED
        mem.parent.mkdir(parents=True, exist_ok=True)
        lines: list[str] = [] if mem.is_file() else ["# Curated journal", ""]
        for t in scenario.turns(slot, round):
            lines.append(f"## [{t.turn_type}] {t.marker}")
            lines.append("")
            lines.append(f"Round {round} {t.turn_type} memory. Verbatim marker: {t.marker}.")
            lines.append("")
        with mem.open("a", encoding="utf-8") as f:
            f.write("\n".join(lines) + "\n")

    def seed_flow(self) -> str:
        return "seed_markers.py ──append `## `-sections──▶ memories/curated.md (curated, by_heading)"

    # -- Z14 curated in-place edit (WP4.1) — on the by_heading journal ---------

    def _workspace(self, slot: str) -> Path:
        # Generic is single-agent: the workspace IS the bind-mounted home.
        return self.env.host_home

    def _reset_baseline(self, slot: str) -> str:
        marker = scenario.marker_for(slot, "semantic", 1)
        return (
            "# Curated journal\n\n"
            f"## Identity\n\nReference code: {marker}\n\n"
            "## Preferences\n\nTerse answers. Metric units.\n\n"
            "## Projects\n\nReef-camera build; tide-log automation.\n"
        )

    def curate_memory(self, ctr, slot: str, op: str) -> None:
        """Edit the curated `by_heading` journal in place, host-side on the
        mounted workspace — the OpenClaw curation shape, minus a framework. The
        generic adapter re-chunks its `## ` sections; content-addressed birth ids
        + reconcile make each op the exact delta Z14 asserts."""
        mem = self._workspace(slot) / _CURATED
        if op == "reset":
            mem.parent.mkdir(parents=True, exist_ok=True)
            mem.write_text(self._reset_baseline(slot), encoding="utf-8")
            return
        if not mem.is_file():
            raise RuntimeError(f"curate_memory: {mem} does not exist (run 'reset' first)")
        text = mem.read_text(encoding="utf-8")

        if op == "touch":
            mem.write_text(text, encoding="utf-8")  # identical bytes, fresh mtime
        elif op == "reorder":
            preamble, sections = self._split_sections(text)
            if len(sections) < 2:
                raise RuntimeError("curate_memory: need >= 2 sections to reorder")
            mem.write_text(preamble + "".join(reversed(sections)), encoding="utf-8")
        elif op == "edit":
            old = scenario.marker_for(slot, "semantic", 1)
            new = scenario.curated_marker(slot)
            if old in text:
                mem.write_text(text.replace(old, new, 1), encoding="utf-8")
            else:
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
                "## Curated insert (Z14)\n\nReconcile must see exactly one new record.\n\n",
            )
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
        """Fence-aware split on top-level `## ` headings; each chunk is newline-
        terminated so reconcatenation can't merge sections (matches the adapter's
        `split_markdown_sections`)."""
        lines = text.splitlines(keepends=True)
        preamble: list = []
        sections: list = []
        current = None
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

    # -- a SQLite row curation the adapter reconciles to one Update ------------

    def curate_sqlite_row(self, op: str) -> None:
        """In-place edit the synthetic brain.db (edit/insert/delete one row).
        Not driven by the markdown-shaped Z14 op sequence — the row-level
        reconcile (edit → 1 Update, insert → 1 Create, delete → 1 Delete) is
        proven by the adapter's Rust tests — but exposed so a live check can
        exercise the sqlite path through `alf_sync` if wired to a stage."""
        db = self._workspace(self.agent_slots[0]) / _BRAIN
        conn = sqlite3.connect(db)
        try:
            if op == "edit":
                conn.execute("UPDATE memories SET content=? WHERE id='fact-1'",
                             ("Reference code: CURATED-VIA-MCP",))
            elif op == "insert":
                conn.execute(
                    "INSERT INTO memories (id, content, updated_at) "
                    "VALUES ('fact-4', 'A newly learned fact.', '2026-01-02T00:00:00Z')")
            elif op == "delete":
                conn.execute("DELETE FROM memories WHERE id='fact-3'")
            else:
                raise RuntimeError(f"curate_sqlite_row: unknown op {op!r}")
            conn.commit()
        finally:
            conn.close()

    def seed_narrative(self) -> str:
        return (super().seed_narrative() + " This tier ALSO maps a synthetic "
                "brain.db (sqlite_rows), so row-level memory syncs through the "
                "MCP server; Z14 then curates a by_heading journal in place.")


KIT_CLASS = GenericMcpKit
