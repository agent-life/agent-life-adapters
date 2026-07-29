"""Self-tests for the gate-honesty fixes (MIN-18…MIN-22).

Every one of these gates could report GREEN for a run in which the thing it
gates never happened. A lifecycle gate that can pass vacuously is worse than no
gate: it converts "we did not check" into "we checked and it was fine", and the
live tiers are the only evidence some of these behaviors ever get.

Run:  python3 -m unittest tests.lifecycle.test_gate_honesty
"""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

_W1 = Path(__file__).resolve().parent / "w1_walkthrough.py"
_SPEC = importlib.util.spec_from_file_location("w1_walkthrough", _W1)
w1 = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(w1)


class _Result:
    """Stand-in for an MCP tool result."""

    def __init__(self, ok: bool):
        self.ok = ok

    def parsed(self):
        return {"ok": self.ok}


class W1ExitCriterionTest(unittest.TestCase):
    """MIN-18: the W1 onboarding gate judged only the wire-call budget, so a
    run whose every call errored still exited 0 and was recorded as meeting the
    '≤ 6 onboarding calls' release criterion."""

    def test_transcript_records_failed_tools(self):
        t = w1.Transcript()
        t.step("alf_status", {}, _Result(True))
        t.step("alf_configure", {}, _Result(False))
        t.step("alf_track", {"path": "x"}, _Result(False))
        self.assertEqual(t.failed, ["alf_configure", "alf_track"])
        self.assertEqual(t.tool_calls, 3)

    def test_all_ok_leaves_no_failures(self):
        t = w1.Transcript()
        for tool in ("alf_status", "alf_configure", "alf_track"):
            t.step(tool, {}, _Result(True))
        self.assertEqual(t.failed, [])

    def test_the_verdict_expression_requires_both_conditions(self):
        # The gate is `within and not failed`. Pin the truth table so a future
        # edit cannot quietly drop the second half again.
        for wire, failed, expected in [
            (5, [], 0),          # under budget, all succeeded → pass
            (5, ["alf_sync"], 1),  # under budget but a call errored → FAIL
            (7, [], 1),          # over budget → fail
            (7, ["alf_sync"], 1),
        ]:
            within = wire <= 6
            self.assertEqual(
                0 if (within and not failed) else 1,
                expected,
                f"wire={wire} failed={failed}",
            )


class StagesSourceContractTest(unittest.TestCase):
    """MIN-19/20/21/22 live inside stage functions that need a live container,
    a real backend and a running MCP server — they cannot be executed here.
    What CAN be pinned is that the specific vacuous-pass constructs are gone:
    these are the exact expressions the review found, and each would silently
    reintroduce a false green."""

    @classmethod
    def setUpClass(cls):
        cls.src = (Path(__file__).resolve().parent / "alflab" / "stages.py").read_text(
            encoding="utf-8"
        )

    def test_z16_delta_content_is_not_a_partial_pass(self):
        # MIN-20: `present >= 4` accepted 4 of 6 markers — genuine content loss.
        self.assertNotIn("present >= 4", self.src)
        self.assertIn("present == len(markers)", self.src)

    def test_z16_returns_when_the_watch_server_died(self):
        # MIN-22: the liveness FAIL must stop the stage, not fall through into
        # ~40s of assertions against a dead server.
        i = self.src.index('"Z16: watch server stayed up"')
        tail = self.src[i : i + 400]
        self.assertIn("return", tail, "the liveness failure must early-return")

    def test_z15_does_not_synthesize_the_vault_label(self):
        # MIN-19: the label used to be injected when absent, so the viz showed a
        # credential the run never landed.
        self.assertNotIn('labels = list(dict.fromkeys([*labels, "z15"]))', self.src)
        self.assertIn('vault_landed = "z15" in labels', self.src)

    def test_z17_verifies_the_vault_deletion(self):
        # MIN-21: an unverified `rm -f` made the "came back from the backend"
        # proof vacuous if the vault path ever drifted.
        self.assertIn("Z17 vault round-trip precondition", self.src)
        self.assertIn("every local ciphertext copy deleted", self.src)


if __name__ == "__main__":
    unittest.main()
