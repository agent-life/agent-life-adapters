"""Rendering primitives, ported from scripts/integration_walkthrough.py
(banner/section/explain/flow/ok/fail/show_data/inspect/inspect_online) with two
harness additions: every line passes through alflab.redact before it reaches
ANY sink, and everything printed is mirrored (ANSI-stripped) to driver.log.
"""

from __future__ import annotations

import json
import re
import textwrap
from pathlib import Path
from typing import Any, Optional

from .redact import redact

COLORS = {
    "reset":   "\033[0m",
    "bold":    "\033[1m",
    "dim":     "\033[2m",
    "green":   "\033[32m",
    "yellow":  "\033[33m",
    "blue":    "\033[34m",
    "cyan":    "\033[36m",
    "red":     "\033[31m",
    "magenta": "\033[35m",
}

_ANSI_RE = re.compile(r"\033\[[0-9;]*m")
_log_path: Optional[Path] = None


def set_log_file(path: Path) -> None:
    """Mirror all rendered output (redacted, ANSI-stripped) to `path`."""
    global _log_path
    _log_path = path
    path.parent.mkdir(parents=True, exist_ok=True)
    path.touch()


def emit(text: str = "") -> None:
    """The single output sink: redact -> stdout (+ redacted log copy)."""
    text = redact(text)
    print(text)
    if _log_path is not None:
        with _log_path.open("a", encoding="utf-8") as f:
            f.write(_ANSI_RE.sub("", text) + "\n")


def c(color: str, text: str) -> str:
    return f"{COLORS.get(color, '')}{text}{COLORS['reset']}"


def banner(text: str) -> None:
    width = 70
    emit()
    emit(c("cyan", "═" * width))
    emit(c("cyan", f"  {text}"))
    emit(c("cyan", "═" * width))
    emit()


def section(label: str, title: str) -> None:
    emit()
    emit(c("bold", f"── {label}: {title} ──"))
    emit()


def explain(text: str) -> None:
    for line in textwrap.dedent(text).strip().splitlines():
        emit(c("dim", f"  │ {line}"))
    emit()


def flow(arrows: str) -> None:
    """One-line cause→effect map for the step's data movement."""
    emit(f"  {c('cyan', 'data flow:')}  {arrows}")
    emit()


def ok(msg: str) -> None:
    emit(f"  {c('green', '✓')} {msg}")


def fail(msg: str) -> None:
    emit(f"  {c('red', '✗')} {msg}")


def warn(msg: str) -> None:
    emit(f"  {c('yellow', '⚠')} {msg}")


def skip(msg: str) -> None:
    emit(f"  {c('yellow', '⊙')} SKIPPED — {msg}")


def xfail(msg: str) -> None:
    emit(f"  {c('magenta', '⊘ XFAIL')} {msg}")


def xpass(msg: str) -> None:
    emit(c("magenta", "  ══ XPASS — a registered known-gap check now PASSES ══"))
    emit(f"  {c('magenta', '⊘→✓')} {msg}")
    emit(c("magenta", "  ══ flip it deliberately: remove the XFAIL registration ══"))


def show_data(label: str, data: Any) -> None:
    emit(f"  {c('yellow', label)}:")
    if isinstance(data, dict):
        for k, v in data.items():
            val = json.dumps(v) if isinstance(v, (dict, list)) else str(v)
            # Redact BEFORE truncation — a sliced secret fragment would no
            # longer match the pattern shapes (emit's redact is idempotent).
            val = redact(val)
            if len(val) > 100:
                val = val[:100] + "..."
            emit(f"    {c('dim', str(k))}: {val}")
    elif isinstance(data, list):
        for item in data[:10]:
            emit(f"    - {item}")
        if len(data) > 10:
            emit(f"    ... and {len(data) - 10} more")
    else:
        emit(f"    {data}")
    emit()


def cli_header() -> None:
    emit(f"  {c('magenta', '▸ CLI LANE')}  {c('dim', '— what runs inside the container')}")


def api_header() -> None:
    emit(f"  {c('blue', '▸ API / STORAGE LANE ⊙')}  {c('dim', '— what it produced in the cloud')}")


def inspect(run_dir: Path, items: list[tuple[str, str]]) -> None:
    """Copy-pasteable commands to inspect local run resources right now."""
    emit(f"  {c('yellow', 'inspect locally')} {c('dim', '(RUN=' + str(run_dir) + ')')}:")
    for desc, cmd in items:
        emit(f"    {c('dim', '# ' + desc)}")
        emit(f"    {cmd}")
    emit()


def inspect_online(bucket: str, items: list[tuple[str, str]]) -> None:
    """S3 URLs + pull commands for uploaded objects (the cloud lane)."""
    items = [(d, k) for d, k in items if k]
    if not items:
        return
    emit(f"  {c('yellow', 'inspect online (S3)')} {c('dim', 'bucket=' + bucket)}:")
    for desc, key in items:
        emit(f"    {c('dim', '# ' + desc)}")
        emit(f"    {c('cyan', 's3://' + bucket + '/' + key)}")
        if key.endswith("/"):
            emit(f"    aws s3 ls s3://{bucket}/{key} --recursive --human-readable")
        else:
            emit(f"    aws s3 cp s3://{bucket}/{key} /tmp/inspect.alf && unzip -l /tmp/inspect.alf")
    emit()
