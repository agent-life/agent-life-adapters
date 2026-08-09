"""Reproducible-release provenance (RF-024, plan §4 A1).

Binds every lifecycle artifact to the exact source + binary that produced it:
source commit, dirty-tree state, tested binary SHA-256 + version, command,
toolchain, and — for live runs — the mint/scavenge service checkout's state.

`capture()` shells `git` and hashes the binary. It NEVER throws: any git /
subprocess failure degrades the field (`"unknown"` commit, `""` toolchain /
digest) and, because the `release_evidence` property keys off those same
fields, forces `release_evidence = False`. A run that cannot be proven
reproducible must say so, not crash.
"""

from __future__ import annotations

import shlex
import subprocess
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

from .dockerctl import host_sha256
from .redact import redact


@dataclass
class Provenance:
    captured_at: str              # UTC "%Y-%m-%dT%H:%M:%SZ"
    command: str                  # shlex.join(sys.argv)
    adapters_commit: str          # git -C <repo> rev-parse HEAD, else "unknown"
    adapters_dirty: bool          # git -C <repo> status --porcelain non-empty
    adapters_dirty_summary: str   # first ~20 porcelain lines, redacted; "" when clean
    binary_path: str
    binary_sha256: str            # host_sha256(alf_binary); "" on failure
    binary_version: str           # expected_alf_version()
    toolchain: str                # `rustc -V` / `cargo -V`, best-effort, "" on failure
    service_commit: str = ""      # captured only when backend == "real"
    service_dirty: bool = False

    @property
    def release_evidence(self) -> bool:
        ok = (not self.adapters_dirty
              and self.adapters_commit not in ("", "unknown")
              and bool(self.binary_sha256))
        # A live run is only reproducible if the mint/scavenge repo is clean too.
        return ok and not self.service_dirty


def _git(repo: Path, args: list, timeout: float = 5.0) -> Optional[str]:
    """`git -C <repo> <args>` → stripped stdout, or None on ANY failure
    (git missing, non-zero exit, timeout). Callers degrade None to the safe
    non-release value for the field."""
    try:
        proc = subprocess.run(
            ["git", "-C", str(repo), *args],
            capture_output=True, text=True, timeout=timeout,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if proc.returncode != 0:
        return None
    return proc.stdout


def _porcelain_dirty(repo: Path) -> tuple[bool, str]:
    """(dirty, redacted first-~20-line summary). A git-status failure fails
    SAFE — reported dirty with a note — so an un-provable tree never reads clean."""
    out = _git(repo, ["status", "--porcelain"])
    if out is None:
        return True, redact("(git status unavailable)")
    lines = [ln for ln in out.splitlines() if ln.strip()]
    if not lines:
        return False, ""
    return True, redact("\n".join(lines[:20]))


def _toolchain() -> str:
    parts = []
    for tool in (["rustc", "-V"], ["cargo", "-V"]):
        try:
            proc = subprocess.run(tool, capture_output=True, text=True, timeout=5.0)
        except (OSError, subprocess.SubprocessError):
            continue
        if proc.returncode == 0:
            parts.append(proc.stdout.strip())
    return "; ".join(parts)


def capture(repo_root, alf_binary, backend: str, service_repo) -> Provenance:
    """Build the provenance for this invocation. Pure-ish: reads git + hashes
    the binary, never mutates anything, never raises."""
    repo_root = Path(repo_root)

    head = _git(repo_root, ["rev-parse", "HEAD"])
    adapters_commit = head.strip() if head else "unknown"
    adapters_dirty, adapters_dirty_summary = _porcelain_dirty(repo_root)

    binary_path = str(alf_binary) if alf_binary else ""
    binary_sha256 = ""
    if alf_binary:
        try:
            binary_sha256 = host_sha256(Path(alf_binary))
        except OSError:
            binary_sha256 = ""

    # Lazy import breaks the runner → report → provenance → runner cycle;
    # the version is informational, so any failure degrades to "".
    binary_version = ""
    try:
        from .runner import expected_alf_version
        binary_version = expected_alf_version()
    except Exception:  # noqa: BLE001
        binary_version = ""

    service_commit = ""
    service_dirty = False
    if backend == "real" and service_repo and Path(service_repo).is_dir():
        sc = _git(Path(service_repo), ["rev-parse", "HEAD"])
        service_commit = sc.strip() if sc else "unknown"
        service_dirty, _ = _porcelain_dirty(Path(service_repo))

    return Provenance(
        captured_at=datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        command=shlex.join(sys.argv),
        adapters_commit=adapters_commit,
        adapters_dirty=adapters_dirty,
        adapters_dirty_summary=adapters_dirty_summary,
        binary_path=binary_path,
        binary_sha256=binary_sha256,
        binary_version=binary_version,
        toolchain=_toolchain(),
        service_commit=service_commit,
        service_dirty=service_dirty,
    )
