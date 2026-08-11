"""The teardown ledger must name EVERY agent it touched (2026-07-29).

The per-agent rungs each called `record()` inside their loop, and `record()`
overwrites `manifest.teardown[rung]`. So a two-agent run kept only the LAST
agent's disposition — and a FAILED purge/scavenge on agent A followed by a
success on B was overwritten into a clean "ok". The ledger is the forensic
record of what teardown actually did: a run whose cleanup half-failed could
therefore be filed as clean, and the leaked agent's id would appear nowhere.

Spotted in a real run (20260729T215550Z-hermes-mcp): two agents in the manifest,
one named in `rung2-api-delete`.

Run:  python3 -m unittest discover -s tests/lifecycle -p 'test_teardown_ledger.py'
"""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from alflab import provision

A = "aaaaaaaa-0000-0000-0000-000000000001"
B = "bbbbbbbb-0000-0000-0000-000000000002"
SEED = "cccccccc-0000-0000-0000-000000000003"


class _Proc:
    def __init__(self, rc, stdout="", stderr=""):
        self.returncode, self.stdout, self.stderr = rc, stdout, stderr


class _Container:
    """`alf purge` returns the rc this map gives for the --agent argument."""

    def __init__(self, rcs: dict):
        self.rcs = rcs

    def alive(self):
        return True

    def exec(self, argv, timeout=None):
        return _Proc(self.rcs[argv[argv.index("--agent") + 1]])


class _Resp:
    def __init__(self, code):
        self.status_code = code


class _Api:
    """GET returns `gets[id]`; DELETE returns 204 and is recorded."""

    def __init__(self, gets: dict):
        self.gets, self.deleted = gets, []

    def get(self, path):
        return _Resp(self.gets[path.rsplit("/", 1)[1]])

    def delete(self, path):
        self.deleted.append(path.rsplit("/", 1)[1])
        return _Resp(204)


class TeardownLedgerNamesEveryAgentTest(unittest.TestCase):
    def _run(self, container, api, agents=(A, B)):
        tmp = Path(tempfile.mkdtemp())
        m = provision.Manifest(framework="t", created_at="now", backend="real",
                               llm="proxy", lifecycle_agents=list(agents))
        path = tmp / "run-manifest.json"
        # No seed agent and no service repo ⇒ rungs 4/5 are the skip/dry-run
        # paths; this test is only about the per-agent rungs.
        provision.teardown_ladder(m, path, api, container, tmp, "hermes", env={})
        return m.teardown

    def test_rung1_names_both_agents(self):
        led = self._run(_Container({A: 0, B: 0}), None)
        self.assertIn(A, led["rung1-alf-purge"])
        self.assertIn(B, led["rung1-alf-purge"])

    def test_rung1_keeps_a_failure_that_a_later_success_used_to_erase(self):
        # A fails, B succeeds. The old code recorded only B's "ok".
        led = self._run(_Container({A: 1, B: 0}), None)
        self.assertIn(f"{A}: best-effort (exit 1)", led["rung1-alf-purge"])
        self.assertIn(f"{B}: ok", led["rung1-alf-purge"])

    def test_rung2_names_both_agents(self):
        api = _Api({A: 200, B: 404})
        led = self._run(_Container({A: 0, B: 0}), api)
        self.assertIn(f"{A}: 204", led["rung2-api-delete"])
        self.assertIn(f"{B}: already gone (404)", led["rung2-api-delete"])
        self.assertEqual(api.deleted, [A])

    def test_rung3_counts_what_it_verified(self):
        led = self._run(_Container({A: 0, B: 0}), _Api({A: 404, B: 404}))
        self.assertIn("2 agent(s) verified 404", led["rung3-verify-404"])

    def test_rung3_does_not_claim_ok_when_there_was_nothing_to_verify(self):
        # A bare "ok" asserted a verification that never ran.
        led = self._run(_Container({}), _Api({}), agents=())
        self.assertNotEqual(led["rung3-verify-404"], "ok")
        self.assertIn("none were registered", led["rung3-verify-404"])


class Rung4bDirectScavengeTest(unittest.TestCase):
    """rung4b (`api is None` fallback) is the ONLY rung whose failure is a REAL
    leak: the lifecycle agents are invisible to batch scavenge. It must name
    every agent and never overwrite A's failure with B's success (RF-024 A4)."""

    def _fake_scavenge(self, service_repo, args, env=None):
        # rung4 seed-scavenge, rung4b per-agent, and rung5 dry-run all land here.
        # Only the A/B lifecycle-agent deletes drive the assertion; everything
        # else (the seed delete, the empty-args dry-run) succeeds.
        if "--agent" in args:
            agent = args[args.index("--agent") + 1]
            if agent == A:
                return _Proc(1, stderr="scavenge boom for A")
        return _Proc(0)

    def test_rung4b_names_every_agent_and_keeps_a_failure(self):
        tmp = Path(tempfile.mkdtemp())
        m = provision.Manifest(framework="t", created_at="now", backend="real",
                               llm="proxy", seed_agent_id=SEED,
                               lifecycle_agents=[A, B])
        path = tmp / "run-manifest.json"
        orig = provision._scavenge
        provision._scavenge = self._fake_scavenge
        try:
            # api=None forces the rung4b fallback; container=None skips rung1.
            ok = provision.teardown_ladder(m, path, None, None, tmp, "hermes", env={})
        finally:
            provision._scavenge = orig
        line = m.teardown["rung4b-scavenge-lifecycle"]
        # BOTH agents named, A's failure retained, B's success not erased.
        self.assertIn(A, line)
        self.assertIn(B, line)
        self.assertIn("FAILED", line)
        self.assertIn("scavenge boom for A", line)
        self.assertIn(f"{B}: ok", line)
        # rung4 seed-scavenge succeeded, so only rung4b drives the verdict.
        self.assertEqual(m.teardown["rung4-scavenge-seed"], "ok")
        # A real leak ⇒ the whole ladder reports non-clean.
        self.assertFalse(ok)


if __name__ == "__main__":
    unittest.main()
