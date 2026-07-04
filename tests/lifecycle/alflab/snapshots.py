"""Directory snapshot/diff — proves "zero framework-home changes" (Z3) and
feeds the interactive per-stage diff rendering. Content-hash based, adapted
from integration_walkthrough.py's workspace_file_hashes/workspace_diff."""

from __future__ import annotations

import difflib
import hashlib
from pathlib import Path

# SQLite sidecar churn is not a "change" for our purposes.
_IGNORE_SUFFIXES = ("-wal", "-shm", "-journal")


def snapshot(root: Path) -> dict:
    """{relative-path: sha256} for every file under root (empty if absent)."""
    out: dict[str, str] = {}
    if not root.is_dir():
        return out
    for p in sorted(root.rglob("*")):
        if not p.is_file() or p.is_symlink():
            continue
        rel = str(p.relative_to(root))
        if rel.endswith(_IGNORE_SUFFIXES):
            continue
        try:
            out[rel] = hashlib.sha256(p.read_bytes()).hexdigest()
        except OSError:
            out[rel] = "<unreadable>"
    return out


def diff(before: dict, after: dict) -> dict:
    """{'added': [...], 'removed': [...], 'changed': [...]}, all sorted."""
    return {
        "added": sorted(set(after) - set(before)),
        "removed": sorted(set(before) - set(after)),
        "changed": sorted(r for r in set(before) & set(after) if before[r] != after[r]),
    }


def is_empty(d: dict) -> bool:
    return not (d["added"] or d["removed"] or d["changed"])


def unified_text_diff(root: Path, rel: str, old_text: str) -> str:
    """Unified diff of one text file vs its previous content (for rendering;
    the caller redacts). Binary/undecodable files return a marker line."""
    try:
        new_text = (root / rel).read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return f"(binary or unreadable: {rel})"
    return "\n".join(difflib.unified_diff(
        old_text.splitlines(), new_text.splitlines(),
        fromfile=f"a/{rel}", tofile=f"b/{rel}", lineterm="",
    ))
