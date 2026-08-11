""".alf / .alf-delta readers — content-parity oracles for the ⊙ checks.

An .alf archive is a ZIP (spec: agent-life-data-format). The harness only
needs three read-side views: the entry listing (layer layout), a text scan
for scenario markers, and byte/entry-level equality for the Z13' determinism
proof. Product-path first: archives are produced by `alf export` copy-out."""

from __future__ import annotations

import hashlib
import json
import zipfile
from pathlib import Path

TEXT_SUFFIXES = (".json", ".jsonl", ".md", ".toml", ".yaml", ".yml", ".txt")


def entries(path: Path) -> list[str]:
    with zipfile.ZipFile(path) as z:
        return sorted(z.namelist())


def manifest(path: Path) -> dict:
    with zipfile.ZipFile(path) as z:
        with z.open("manifest.json") as f:
            return json.load(f)


def layer_listing(path: Path) -> dict:
    """Entry names grouped by top-level directory (memory/, raw/, …)."""
    out: dict[str, list[str]] = {}
    for name in entries(path):
        top = name.split("/", 1)[0] if "/" in name else "(root)"
        out.setdefault(top, []).append(name)
    return out


def scan_markers(path: Path, markers: list[str], prefix: str = "") -> dict:
    """{marker: [entry names containing it]} across text entries (optionally
    restricted to entries under `prefix`, e.g. 'memory/')."""
    hits: dict[str, list[str]] = {m: [] for m in markers}
    with zipfile.ZipFile(path) as z:
        for info in z.infolist():
            name = info.filename
            if prefix and not name.startswith(prefix):
                continue
            if not name.lower().endswith(TEXT_SUFFIXES):
                continue
            try:
                text = z.read(name).decode("utf-8", errors="replace")
            except (OSError, zipfile.BadZipFile):
                continue
            for m in markers:
                if m in text:
                    hits[m].append(name)
    return hits


def memory_records(path: Path) -> list[dict]:
    """All structured memory records (the `memory/*.jsonl` partition lines) in
    the archive — the Z14' oracle for birth-id stability under curation."""
    records: list[dict] = []
    with zipfile.ZipFile(path) as z:
        for name in sorted(z.namelist()):
            if not (name.startswith("memory/") and name.endswith(".jsonl")):
                continue
            text = z.read(name).decode("utf-8", errors="replace")
            for line in text.splitlines():
                line = line.strip()
                if line:
                    records.append(json.loads(line))
    return records


def record_identity(path: Path) -> dict:
    """Identity-integrity oracle: `{'total', 'unique', 'duplicates': {id: n}}`.

    Record ids must be unique across the whole archive — a duplicate is an
    adapter id-preimage collision (the v1.1.0 pre-release BLK-1 class): every
    by-id consumer (reconcile, the indexer, the dashboard) silently keeps one
    record and drops the rest. This is an EXTERNAL oracle on purpose: a
    deterministic collision is self-consistent, so the round-trip and Z13'
    byte-equality checks can never see it."""
    counts: dict[str, int] = {}
    for r in memory_records(path):
        rid = str(r.get("id", "(missing)"))
        counts[rid] = counts.get(rid, 0) + 1
    return {
        "total": sum(counts.values()),
        "unique": len(counts),
        "duplicates": {rid: n for rid, n in counts.items() if n > 1},
    }


def marker_record_counts(path: Path, probes: list[str]) -> dict:
    """{probe: number of memory RECORDS whose content contains it} — pairs the
    kit's seeded store rows/sections 1:1 with structured records (0 = the row
    was lost to shadowing/extraction; 2+ = it was multiply minted)."""
    recs = memory_records(path)
    return {p: sum(1 for r in recs if p in str(r.get("content", ""))) for p in probes}


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def entry_hashes(path: Path) -> dict:
    """{entry: sha256(content)} — the fallback comparison when byte-equality
    fails, to show WHERE two archives diverge."""
    out: dict[str, str] = {}
    with zipfile.ZipFile(path) as z:
        for name in z.namelist():
            out[name] = hashlib.sha256(z.read(name)).hexdigest()
    return out
