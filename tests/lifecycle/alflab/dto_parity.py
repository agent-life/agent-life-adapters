"""Dashboard memory-DTO parity (WP-M4 task 3).

The dashboard's Memory tab reads `GET /v1/agents/:id/memory`, whose
`MemoryRecordDto` (agent-life-service `lambda-agent-manage/src/agent_memory.rs`)
is a runtime-BLIND projection of the archive's memory records — the indexer
never reads the runtime and does no per-type validation (design F2). So a
generic agent's DTO is field-identical to an OpenClaw agent's **iff** their
archive memory records project to the same shape, which is checkable fully
OFFLINE (no backend, no indexer) from two `alf export` archives.

`project` mirrors `row_to_dto` exactly; `shape_profile` reduces a record set to
the structural fingerprint the dashboard depends on (the DTO field/type shape +
the sub-key sets of the two nested objects the cards group on). Live-tier
confirmation (`GET /v1/agents/:id/memory` on the test backend) then holds by F2:
the endpoint is this same projection applied to the indexed rows.
"""

from __future__ import annotations

# The MemoryRecordDto fields the archive record populates. `source_sequence` is
# assigned by the indexer (not in the archive), so it is excluded from the
# archive-level shape and re-asserted on the live tier.
DTO_FIELDS = [
    "id", "memory_type", "namespace", "content", "observed_at",
    "created_at_alf", "updated_at_alf", "tags", "source", "raw_source_format",
]

# The dashboard's canonical chip sets (design F3): type chips are hardcoded to
# these; namespace grouping/filtering keys on these.
CANONICAL_TYPES = {"semantic", "episodic", "procedural", "preference", "summary"}
CHIP_NAMESPACES = {"daily", "curated", "procedural"}


def project(record: dict) -> dict:
    """One archive MemoryRecord → the MemoryRecordDto field mapping (row_to_dto).
    observed_at falls back to created_at exactly like the indexer's partitioner."""
    t = record.get("temporal") or {}
    return {
        "id": record.get("id"),
        "memory_type": record.get("memory_type"),
        "namespace": record.get("namespace"),
        "content": record.get("content"),
        "observed_at": t.get("observed_at") or t.get("created_at"),
        "created_at_alf": t.get("created_at"),
        "updated_at_alf": t.get("updated_at"),
        "tags": record.get("tags"),
        "source": record.get("source"),
        "raw_source_format": record.get("raw_source_format"),
    }


def _typename(v) -> str:
    if v is None:
        return "null"
    if isinstance(v, bool):
        return "bool"
    if isinstance(v, str):
        return "str"
    if isinstance(v, (int, float)):
        return "number"
    if isinstance(v, list):
        return "list"
    if isinstance(v, dict):
        return "dict"
    return type(v).__name__


def shape_profile(records: list) -> dict:
    """The DTO structural fingerprint across a record set: for each DTO field the
    set of non-null JSON types observed, plus the sub-key sets of the two nested
    objects the dashboard groups/sources on (`source`, `raw_source_format`).
    Robust to per-record nullability and value differences — it compares SHAPE."""
    field_types = {k: set() for k in DTO_FIELDS}
    source_keys: set = set()
    rsf_keys: set = set()
    for r in records:
        dto = project(r)
        for k in DTO_FIELDS:
            v = dto[k]
            if v is not None:
                field_types[k].add(_typename(v))
        if isinstance(dto["source"], dict):
            source_keys |= set(dto["source"].keys())
        if isinstance(dto["raw_source_format"], dict):
            rsf_keys |= set(dto["raw_source_format"].keys())
    return {
        "dto_keys": list(DTO_FIELDS),
        "field_types": {k: sorted(v) for k, v in field_types.items()},
        "source_keys": sorted(source_keys),
        "raw_source_format_keys": sorted(rsf_keys),
    }


def dashboard_validity(records: list) -> list:
    """Issues that would break dashboard rendering (design F3): a non-canonical
    memory_type (type chip falls through), a non-chip namespace (loses filtering/
    grouping), empty content, a missing source.origin_file, or a chunked record
    with no raw_source_format.line_start. Returns a list of human-readable issues
    (empty == dashboard-clean)."""
    issues = []
    for r in records:
        rid = r.get("id", "?")
        mt = r.get("memory_type")
        if mt not in CANONICAL_TYPES:
            issues.append(f"{rid}: non-canonical memory_type {mt!r}")
        ns = r.get("namespace")
        if ns is not None and ns not in CHIP_NAMESPACES:
            issues.append(f"{rid}: non-chip namespace {ns!r}")
        if not (r.get("content") or "").strip():
            issues.append(f"{rid}: empty content")
        src = r.get("source") or {}
        if not src.get("origin_file"):
            issues.append(f"{rid}: missing source.origin_file")
        rsf = r.get("raw_source_format")
        if isinstance(rsf, dict) and rsf.get("line_start") is None:
            issues.append(f"{rid}: raw_source_format without line_start")
    return issues
