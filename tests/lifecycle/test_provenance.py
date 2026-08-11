"""Provenance / release-evidence unit tests (RF-024 A5).

Every lifecycle artifact must be bound to the exact source + binary that made
it, and a run that cannot be release evidence must say so — rejected in strict
mode or labelled NON-RELEASE EVIDENCE. These tests drive `git` against throwaway
temp repos so they never depend on the real working tree's cleanliness.

Run:  python3 -m unittest discover -s tests/lifecycle -p 'test_provenance.py'
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from alflab.provenance import Provenance, capture  # noqa: E402
from alflab.report import RunReport  # noqa: E402


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), *args],
                   check=True, capture_output=True, text=True)


def _init_repo() -> Path:
    """A committed, clean git repo in a fresh temp dir."""
    repo = Path(tempfile.mkdtemp())
    _git(repo, "init", "-q")
    _git(repo, "config", "user.email", "t@example.test")
    _git(repo, "config", "user.name", "Test")
    (repo / "tracked.txt").write_text("hello\n", encoding="utf-8")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "init")
    return repo


def _fake_binary() -> Path:
    """A hashable binary file OUTSIDE any repo (so it never dirties a tree)."""
    p = Path(tempfile.mkdtemp()) / "alf"
    p.write_bytes(b"\x7fELF fake-alf-under-test bytes")
    return p


def _report(prov: Provenance) -> RunReport:
    return RunReport(framework="testfw", tier="none/none",
                     stages_requested=["z01"], provenance=prov)


class ProvenanceCaptureTest(unittest.TestCase):
    def test_clean_repo_real_commit_and_digest_is_release_evidence(self):
        repo, binary = _init_repo(), _fake_binary()
        prov = capture(repo, binary, "none", None)
        self.assertFalse(prov.adapters_dirty)
        self.assertNotIn(prov.adapters_commit, ("", "unknown"))
        self.assertEqual(len(prov.adapters_commit), 40)
        self.assertTrue(prov.binary_sha256)
        self.assertEqual(prov.adapters_dirty_summary, "")
        self.assertTrue(prov.release_evidence)
        # A clean candidate carries NO banner.
        self.assertNotIn("NON-RELEASE EVIDENCE", _report(prov).to_markdown())

    def test_dirty_tree_is_not_release_evidence_and_banner_names_it(self):
        repo, binary = _init_repo(), _fake_binary()
        # An untracked file whose NAME embeds a secret shape proves the porcelain
        # summary is redacted before it is ever stored.
        secret = "alf_" + "A" * 32
        (repo / f"{secret}.txt").write_text("x", encoding="utf-8")
        prov = capture(repo, binary, "none", None)
        self.assertTrue(prov.adapters_dirty)
        self.assertFalse(prov.release_evidence)
        # Porcelain summary present, redacted (raw secret gone, marker present).
        self.assertTrue(prov.adapters_dirty_summary)
        self.assertNotIn(secret, prov.adapters_dirty_summary)
        self.assertIn("[REDACTED]", prov.adapters_dirty_summary)
        md = _report(prov).to_markdown()
        self.assertTrue(md.startswith("> ⚠️ **NON-RELEASE EVIDENCE**"))
        self.assertIn("dirty tree", md)

    def test_non_git_dir_yields_unknown_commit_and_no_release(self):
        # A dir that is not a git repo: rev-parse fails ⇒ "unknown".
        not_a_repo = Path(tempfile.mkdtemp())
        prov = capture(not_a_repo, _fake_binary(), "none", None)
        self.assertEqual(prov.adapters_commit, "unknown")
        self.assertFalse(prov.release_evidence)
        self.assertIn("unknown commit", _report(prov).to_markdown())

    def test_missing_binary_digest_is_not_release_evidence(self):
        repo = _init_repo()
        prov = capture(repo, None, "none", None)   # no binary ⇒ empty digest
        self.assertEqual(prov.binary_sha256, "")
        self.assertFalse(prov.release_evidence)
        self.assertIn("missing binary digest", _report(prov).to_markdown())

    def test_live_run_with_dirty_service_repo_is_not_release_evidence(self):
        adapters, binary = _init_repo(), _fake_binary()
        service = _init_repo()
        (service / "untracked.txt").write_text("y", encoding="utf-8")  # dirty
        prov = capture(adapters, binary, "real", service)
        # Adapters side is clean, but the mint/scavenge checkout is not.
        self.assertFalse(prov.adapters_dirty)
        self.assertTrue(prov.service_dirty)
        self.assertTrue(prov.service_commit)
        self.assertFalse(prov.release_evidence)
        self.assertIn("service dirty", _report(prov).to_markdown())

    def test_service_state_not_captured_when_backend_none(self):
        adapters, binary = _init_repo(), _fake_binary()
        service = _init_repo()
        (service / "untracked.txt").write_text("y", encoding="utf-8")
        prov = capture(adapters, binary, "none", service)  # offline ⇒ ignore service
        self.assertEqual(prov.service_commit, "")
        self.assertFalse(prov.service_dirty)
        self.assertTrue(prov.release_evidence)


class ProvenanceReportBindingTest(unittest.TestCase):
    def test_report_json_round_trips_provenance(self):
        prov = capture(_init_repo(), _fake_binary(), "none", None)
        run_dir = Path(tempfile.mkdtemp())
        _report(prov).write(run_dir)
        import json
        data = json.loads((run_dir / "report.json").read_text(encoding="utf-8"))
        self.assertIn("provenance", data)
        self.assertEqual(data["provenance"]["adapters_commit"], prov.adapters_commit)
        self.assertEqual(data["provenance"]["binary_sha256"], prov.binary_sha256)
        self.assertFalse(data["provenance"]["adapters_dirty"])
        # The derived property is NOT a dataclass field (must be re-derived).
        self.assertNotIn("release_evidence", data["provenance"])

    def test_verdict_line_carries_release_evidence_token(self):
        clean = capture(_init_repo(), _fake_binary(), "none", None)
        self.assertIn("release_evidence=true", _report(clean).verdict_line())
        dirty_repo = _init_repo()
        (dirty_repo / "x.txt").write_text("z", encoding="utf-8")
        dirty = capture(dirty_repo, _fake_binary(), "none", None)
        self.assertIn("release_evidence=false", _report(dirty).verdict_line())

    def test_banner_present_exactly_when_not_release_evidence(self):
        clean = _report(capture(_init_repo(), _fake_binary(), "none", None))
        self.assertNotIn("NON-RELEASE EVIDENCE", clean.to_markdown())
        dirty_repo = _init_repo()
        (dirty_repo / "x.txt").write_text("z", encoding="utf-8")
        dirty = _report(capture(dirty_repo, _fake_binary(), "none", None))
        self.assertIn("NON-RELEASE EVIDENCE", dirty.to_markdown())


if __name__ == "__main__":
    unittest.main()
