"""Structured event stream for the lifecycle visualization.

Every record is one redacted JSON object appended to ``events.ndjson`` in the
run dir. The HTML viz tails this file live and replays it afterward — one
format, both modes. Secret hygiene matches ``ui.emit``: every string passes
through ``alflab.redact`` before it hits disk.
"""

from __future__ import annotations

import json
import threading
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

from .redact import redact_obj

# Module-level singleton — narrator / stages / runner all share one log per run.
_log: Optional["EventLog"] = None
_lock = threading.Lock()


def set_event_log(log: Optional["EventLog"]) -> None:
    global _log
    with _lock:
        _log = log


def get_event_log() -> Optional["EventLog"]:
    return _log


def emit(kind: str, **payload: Any) -> None:
    """Append one event if a log is active; no-op otherwise."""
    log = _log
    if log is not None:
        log.emit(kind, **payload)


def state(subsystem: str, patch: dict, stage_id: str = "") -> None:
    """Convenience: emit a ``state`` event for a subsystem panel update."""
    emit("state", stage_id=stage_id or (_log.current_stage if _log else ""),
         subsystem=subsystem, patch=patch)


class EventLog:
    """Append-only NDJSON writer. Thread-safe; flush after every record."""

    def __init__(self, path: Path):
        self.path = path
        self._seq = 0
        self.current_stage = ""
        self._started_at: Optional[str] = None
        path.parent.mkdir(parents=True, exist_ok=True)
        # Truncate so a re-run into the same dir (shouldn't happen) starts clean.
        path.write_text("", encoding="utf-8")

    def emit(self, kind: str, **payload: Any) -> None:
        now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"
        if self._started_at is None:
            self._started_at = now
        with _lock:
            self._seq += 1
            seq = self._seq
            if kind == "stage_start" and payload.get("stage_id"):
                self.current_stage = str(payload["stage_id"])
            record = {"seq": seq, "t": now, "kind": kind}
            # Prefer an explicit stage_id; otherwise stamp the current stage.
            if "stage_id" not in payload and self.current_stage and kind not in (
                    "run_start", "run_end"):
                record["stage_id"] = self.current_stage
            for k, v in payload.items():
                record[k] = redact_obj(v)
            line = json.dumps(record, ensure_ascii=False, separators=(",", ":"))
            with self.path.open("a", encoding="utf-8") as f:
                f.write(line + "\n")
                f.flush()
