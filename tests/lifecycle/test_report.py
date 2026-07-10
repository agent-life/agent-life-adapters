"""Self-tests for the report verdict line (WP-O.1).

A stage the runner skipped outright (status SKIP, no checks) is excluded from
`passed/ran` — correct, but previously invisible: a run that skipped half its
stages rendered a verdict indistinguishable from a full pass. The verdict line
now ALWAYS carries `skipped={n}` so bare skips are visible at a glance (and a
zero is emitted too, so parsers never have to special-case the field's absence).

Run:  python3 -m unittest tests.lifecycle.test_report
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from alflab.report import Check, RunReport, StageResult  # noqa: E402


def _report(stages: list) -> RunReport:
    r = RunReport(framework="testfw", tier="none/none",
                  stages_requested=[s.stage_id for s in stages])
    r.stages = stages
    return r


class VerdictLineTest(unittest.TestCase):
    def test_verdict_line_counts_bare_skipped_stages(self):
        s1 = StageResult(stage_id="z01", title="passes")
        s1.add(Check(name="ok", status="PASS"))
        s2 = StageResult(stage_id="z02", title="bare skip")   # no checks ran
        s2.status, s2.skip_reason = "SKIP", "not this tier"
        s3 = StageResult(stage_id="z03", title="fails")
        s3.add(Check(name="broken", status="FAIL"))

        line = _report([s1, s2, s3]).verdict_line()
        # The bare skip is out of passed/ran but visible as skipped=1.
        self.assertIn("passed=1/2", line)
        self.assertIn("skipped=1", line)
        # Every requested stage id still appears in stages=, skipped or not.
        self.assertIn("stages=Z01,Z02,Z03", line)

    def test_verdict_line_zero_skips_still_emits_field(self):
        s1 = StageResult(stage_id="z01", title="passes")
        s1.add(Check(name="ok", status="PASS"))

        line = _report([s1]).verdict_line()
        self.assertIn("skipped=0", line)
        self.assertIn("passed=1/1", line)


if __name__ == "__main__":
    unittest.main()
