"""report.md + report.json + the machine verdict line.

Four-valued checks (D4): PASS / FAIL / SKIP / XFAIL (+ XPASS when a
registered known-gap check unexpectedly passes — reported loudly so the
owning WP flips it deliberately). Every string sinks through redact."""

from __future__ import annotations

import json
from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

from .redact import redact, redact_obj

STATUS_ICON = {"PASS": "✅", "FAIL": "❌", "SKIP": "⊙", "XFAIL": "⊘", "XPASS": "‼️"}


@dataclass
class Check:
    name: str
    status: str                 # PASS | FAIL | SKIP | XFAIL | XPASS
    detail: str = ""
    xfail_id: Optional[str] = None   # e.g. "wp3-brain-db-extraction"


@dataclass
class StageResult:
    stage_id: str
    title: str
    status: str = "PASS"        # worst-of checks, or SKIP
    duration_ms: float = 0.0
    checks: list = field(default_factory=list)
    skip_reason: str = ""
    wp: Optional[str] = None    # owning WP when skipped as a planned slot

    def add(self, check: Check):
        self.checks.append(check)
        order = {"FAIL": 3, "XPASS": 2, "XFAIL": 1, "PASS": 0, "SKIP": 0}
        if order.get(check.status, 0) > order.get(self.status, 0):
            self.status = check.status


@dataclass
class RunReport:
    framework: str
    tier: str                   # "<llm>/<backend>"
    stages_requested: list
    started_at: str = field(
        default_factory=lambda: datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M:%S UTC"))
    stages: list = field(default_factory=list)          # [StageResult]
    coverage: str = "-"
    isolation: str = "-"
    teardown: dict = field(default_factory=dict)
    run_dir: str = ""
    alf_version: str = ""
    exit_code: int = 0

    # -- aggregation ---------------------------------------------------------

    def counts(self) -> dict:
        c = {"PASS": 0, "FAIL": 0, "SKIP": 0, "XFAIL": 0, "XPASS": 0}
        for s in self.stages:
            for chk in s.checks:
                c[chk.status] = c.get(chk.status, 0) + 1
        return c

    @property
    def failed(self) -> bool:
        # Check-level FAILs count even if a later status transition (e.g. a
        # SkipStage raised after a failed check) touched the stage status —
        # a recorded FAIL can never produce exit 0.
        return any(
            s.status == "FAIL" or any(c.status == "FAIL" for c in s.checks)
            for s in self.stages
        )

    def verdict_line(self) -> str:
        c = self.counts()
        ran = [s for s in self.stages if s.status != "SKIP" or s.checks]
        passed = sum(1 for s in ran if s.status in ("PASS", "XFAIL", "XPASS"))
        # Bare-skipped stages (SKIP with no checks) are invisible in passed/ran;
        # always emit their count so a run that silently skipped work can't
        # read as a full pass at a glance.
        skipped = len(self.stages) - len(ran)
        stages = ",".join(s.stage_id.upper() for s in self.stages)
        line = (f"<!-- LIFECYCLE framework={self.framework} tier={self.tier} "
                f"stages={stages} passed={passed}/{len(ran)} skipped={skipped} "
                f"xfail={c['XFAIL']}")
        if c["XPASS"]:
            line += f" xpass={c['XPASS']}"
        line += f" coverage={self.coverage} isolation={self.isolation} -->"
        return line

    # -- output --------------------------------------------------------------

    def to_markdown(self) -> str:
        c = self.counts()
        lines = [
            f"# Lifecycle run — {self.framework} ({self.tier})",
            "",
            f"**Date:** {self.started_at}  ",
            f"**Run dir:** `{self.run_dir}`  ",
            f"**alf under test:** `{self.alf_version}`  ",
            "",
            "## Summary",
            "",
            "| Status | Count |",
            "|--------|-------|",
        ]
        for k in ("PASS", "FAIL", "XFAIL", "XPASS", "SKIP"):
            lines.append(f"| {STATUS_ICON[k]} {k} | {c[k]} |")
        lines += ["", "## Stages", ""]
        for s in self.stages:
            head = f"### {s.stage_id.upper()} — {s.title} [{s.status}]"
            lines.append(head)
            if s.skip_reason:
                wp = f" (owner: {s.wp})" if s.wp else ""
                lines.append(f"\n_⊙ {s.skip_reason}{wp}_\n")
            if s.checks:
                lines += ["", "| Check | Status | Detail |", "|---|---|---|"]
                for chk in s.checks:
                    icon = STATUS_ICON.get(chk.status, "?")
                    note = chk.detail
                    if chk.xfail_id:
                        note = f"`known-gap: {chk.xfail_id}` — {note}"
                    lines.append(f"| {chk.name} | {icon} {chk.status} | {note} |")
                lines.append("")
        if self.teardown:
            lines += ["## Teardown ledger", ""]
            for rung, status in self.teardown.items():
                lines.append(f"- `{rung}`: {status}")
            lines.append("")
        lines += ["", self.verdict_line(), ""]
        return redact("\n".join(lines))

    def write(self, run_dir: Path):
        (run_dir / "report.md").write_text(self.to_markdown(), encoding="utf-8")
        payload = redact_obj(asdict(self))
        (run_dir / "report.json").write_text(
            json.dumps(payload, indent=2) + "\n", encoding="utf-8")
