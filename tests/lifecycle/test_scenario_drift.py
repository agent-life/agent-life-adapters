"""Drift guard: alflab/scenario.py vs the legacy bash scenario (D9).

The bash scenario (scripts/multiagent-scenario.sh) stays alive for the spike
testkits until WP3–5 absorb them. This test parses its round-1 turn records
and compares them against scenario.LEGACY_ROUND1_MARKERS, so neither source
can drift silently while both live.

Run: python3 -m unittest discover -s tests/lifecycle -p 'test_*.py'
"""

import re
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from alflab import scenario  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[2]
BASH_SCENARIO = REPO_ROOT / "scripts" / "multiagent-scenario.sh"


def parse_bash_turns() -> dict:
    """{agent: {type: marker}} from the TYPE|MARKER|PROMPT heredoc records."""
    text = BASH_SCENARIO.read_text(encoding="utf-8")
    out: dict = {}
    for agent in ("agent_a", "agent_b"):
        block = re.search(
            rf"{agent}\)\s*\n\s*cat <<'EOF'\n(.*?)\nEOF", text, re.DOTALL
        )
        if not block:
            continue
        turns = {}
        for line in block.group(1).splitlines():
            if "|" not in line:
                continue
            turn_type, marker, _prompt = line.split("|", 2)
            turns[turn_type] = marker
        out[agent] = turns
    return out


class ScenarioDriftTests(unittest.TestCase):
    def test_bash_scenario_still_exists(self):
        self.assertTrue(BASH_SCENARIO.is_file(),
                        "legacy bash scenario retired? Update scenario.py + this guard "
                        "(WP3–5 absorb the spike testkits).")

    def test_round1_marker_equivalence(self):
        parsed = parse_bash_turns()
        self.assertEqual(parsed, scenario.LEGACY_ROUND1_MARKERS,
                         "scripts/multiagent-scenario.sh drifted from "
                         "alflab/scenario.py's frozen legacy copy")

    def test_round_tagged_markers_are_stable(self):
        # The new grammar: {PERSONA}-{TYPE}{ROUND}-{NONCE4}; fake secrets.
        self.assertEqual(scenario.marker_for("default", "semantic", 1), "ATLAS-SEM1-7F3A")
        self.assertEqual(scenario.marker_for("default", "secret", 1), "sk-atlas-r1-FAKE-1A2B")
        self.assertEqual(len(scenario.turns("default", 1)), 4)
        # Round 2 is append-shaped: entirely new markers, no collisions.
        r1, r2 = set(scenario.markers("default", 1)), set(scenario.markers("default", 2))
        self.assertFalse(r1 & r2)

    def test_secret_markers_are_obviously_fake(self):
        for slot in ("default", "agent_a", "agent_b"):
            for rnd in (1, 2):
                secret = scenario.marker_for(slot, "secret", rnd)
                self.assertIn("-FAKE-", secret)


if __name__ == "__main__":
    unittest.main()
