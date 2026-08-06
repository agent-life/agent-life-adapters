#!/usr/bin/env python3
"""Regression tests for the RF-025 integration fixture tooling."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile
from datetime import timezone
from pathlib import Path
from unittest.mock import call, patch

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPTS_DIR = REPO_ROOT / "scripts"
sys.path.insert(0, str(SCRIPTS_DIR))

from jsf.schema_types import string as jsf_string

from generate_synthetic_data import (  # noqa: E402
    FIXED_NOW,
    FixtureInputs,
    generate_fixture,
    generation_context,

)
LOCK_FILE = SCRIPTS_DIR / "integration-fixture.lock"
RUNNER = SCRIPTS_DIR / "run_integration_tests.sh"
ENV_VERIFIER = SCRIPTS_DIR / "verify_generator_env.py"
SCHEMA_CHECKOUT = REPO_ROOT.parent / "agent-life-data-format"


def lock_value(name: str) -> str:
    prefix = f"{name}="
    for line in LOCK_FILE.read_text(encoding="utf-8").splitlines():
        if line.startswith(prefix):
            return line.removeprefix(prefix)
    raise AssertionError(f"{name} missing from {LOCK_FILE}")


class DeterministicFixtureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        if not (SCHEMA_CHECKOUT / "schemas/manifest.schema.json").is_file():
            raise unittest.SkipTest("sibling data-format checkout is unavailable")

    def test_same_inputs_produce_identical_archive_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            inputs = {
                "schema_dir": SCHEMA_CHECKOUT / "schemas",
                "alf_version": lock_value("FIXTURE_ALF_FORMAT_VERSION"),
                "seed": int(lock_value("FIXTURE_SEED")),
                "schema_revision": lock_value("SCHEMA_COMMIT"),
            }
            first = root / "first.alf"
            second = root / "second.alf"
            generate_fixture(FixtureInputs(output=first, **inputs))
            generate_fixture(FixtureInputs(output=second, **inputs))

            self.assertEqual(first.read_bytes(), second.read_bytes())
            with zipfile.ZipFile(first) as archive:
                names = archive.namelist()
                self.assertEqual(names, sorted(names))
                for info in archive.infolist():
                    self.assertEqual(info.date_time, (1980, 1, 1, 0, 0, 0))
                    self.assertEqual(info.compress_type, zipfile.ZIP_STORED)


    def test_jsf_temporal_formats_pin_their_upper_bound(self) -> None:
        generation_context(int(lock_value("FIXTURE_SEED")))
        with patch.object(jsf_string.faker, "date_time", return_value=FIXED_NOW) as date_time:
            self.assertEqual(jsf_string.format_map["date-time"](), FIXED_NOW.isoformat())
            self.assertEqual(jsf_string.format_map["date"](), "2026-01-01")
            self.assertEqual(jsf_string.format_map["time"](), "00:00:00+00:00")

        self.assertEqual(date_time.call_args_list, [call(timezone.utc, end_datetime=FIXED_NOW)] * 3)

class RunnerPreflightTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        if not (SCHEMA_CHECKOUT / ".git").exists():
            raise unittest.SkipTest("sibling data-format checkout is unavailable")

    def test_offline_mode_rejects_dirty_schema_checkout(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary) / "schema"
            subprocess.run(
                ["git", "clone", "--quiet", "--no-local", str(SCHEMA_CHECKOUT), str(checkout)],
                check=True,
            )
            subprocess.run(
                ["git", "-C", str(checkout), "checkout", "--quiet", "--detach", lock_value("SCHEMA_COMMIT")],
                check=True,
            )
            manifest = checkout / "schemas/manifest.schema.json"
            manifest.write_text(manifest.read_text(encoding="utf-8") + "\n", encoding="utf-8")

            result = subprocess.run(
                [str(RUNNER), "--offline", "--schema-dir", str(checkout), "--generate-only"],
                cwd=REPO_ROOT,
                text=True,
                capture_output=True,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("modified or untracked schema files", result.stderr)
            self.assertNotIn("Generated ", result.stdout)

    def test_offline_mode_rejects_wrong_schema_commit(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary) / "schema"
            subprocess.run(
                ["git", "clone", "--quiet", "--no-local", str(SCHEMA_CHECKOUT), str(checkout)],
                check=True,
            )
            subprocess.run(
                ["git", "-C", str(checkout), "checkout", "--quiet", "--detach", "HEAD^"],
                check=True,
            )
            result = subprocess.run(
                [str(RUNNER), "--offline", "--schema-dir", str(checkout), "--generate-only"],
                cwd=REPO_ROOT,
                text=True,
                capture_output=True,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("schema checkout is at", result.stderr)
            self.assertNotIn("Generated ", result.stdout)

    def test_offline_mode_makes_no_network_or_pip_calls_and_cleans_up(self) -> None:
        tracked_paths = (
            REPO_ROOT / "alf-cli/fixtures/synthetic-agent.alf",
            REPO_ROOT / "alf-cli/fixtures/schema_version.txt",
        )
        before = {path: path.read_bytes() for path in tracked_paths}

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bin_dir = root / "bin"
            run_root = root / "runs"
            bin_dir.mkdir()
            run_root.mkdir()
            forbidden_calls = root / "forbidden-calls"
            real_git = shutil.which("git")
            self.assertIsNotNone(real_git)
            git_wrapper = bin_dir / "git"
            git_wrapper.write_text('#!/usr/bin/env bash\ncase " $* " in\n  *" fetch "*|*" pull "*|*" clone "*|*" ls-remote "*)\n    printf "%s\\n" "git $*" >> "$RUNNER_FORBIDDEN_CALLS"; exit 91 ;;\nesac\nexec "$RUNNER_REAL_GIT" "$@"\n', encoding="utf-8")
            python_wrapper = bin_dir / "python3"
            python_wrapper.write_text('#!/usr/bin/env bash\nif [[ ${PYTHONHASHSEED:-} != 0 ]]; then\n  printf "%s\\n" "PYTHONHASHSEED=${PYTHONHASHSEED:-missing}" >> "$RUNNER_FORBIDDEN_CALLS"; exit 93\nfi\nif [[ ${1:-} == -m && ${2:-} == pip ]]; then\n  printf "%s\\n" "python3 $*" >> "$RUNNER_FORBIDDEN_CALLS"; exit 92\nfi\nexec "$RUNNER_REAL_PYTHON" "$@"\n', encoding="utf-8")
            for wrapper in (git_wrapper, python_wrapper):
                wrapper.chmod(0o755)

            env = os.environ | {"PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}", "RUNNER_FORBIDDEN_CALLS": str(forbidden_calls), "RUNNER_REAL_GIT": real_git, "RUNNER_REAL_PYTHON": sys.executable, "TMPDIR": str(run_root)}
            result = subprocess.run([str(RUNNER), "--offline", "--schema-dir", str(SCHEMA_CHECKOUT), "--generate-only"], cwd=REPO_ROOT, env=env, text=True, capture_output=True)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(forbidden_calls.exists(), forbidden_calls.read_text(encoding="utf-8") if forbidden_calls.exists() else "")
            self.assertEqual(list(run_root.iterdir()), [])
        for path, expected in before.items():
            self.assertEqual(path.read_bytes(), expected)

    def test_check_rejects_a_stale_fixture_without_replacing_it(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary) / "adapter-checkout"
            subprocess.run(
                ["git", "clone", "--quiet", "--no-local", str(REPO_ROOT), str(checkout)],
                check=True,
            )
            fixture = checkout / "alf-cli/fixtures/synthetic-agent.alf"
            fixture.write_bytes(fixture.read_bytes() + b"\0")
            run_root = Path(temporary) / "runs"
            run_root.mkdir()
            env = os.environ | {"TMPDIR": str(run_root)}
            result = subprocess.run([str(checkout / "scripts/run_integration_tests.sh"), "--offline", "--schema-dir", str(SCHEMA_CHECKOUT), "--generate-only", "--check"], cwd=checkout, env=env, text=True, capture_output=True)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn(f"STALE: {fixture}", result.stderr)
            self.assertIn("run --regenerate-fixture after reviewing the reported differences.", result.stderr)

    def test_regeneration_cannot_be_skipped_by_generate_only(self) -> None:
        result = subprocess.run(
            [str(RUNNER), "--regenerate-fixture", "--generate-only"],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "--regenerate-fixture cannot be combined with --generate-only.",
            result.stderr,
        )

    def test_locked_environment_matches_current_interpreter(self) -> None:
        result = subprocess.run(
            [
                sys.executable,
                str(ENV_VERIFIER),
                "--lock",
                str(REPO_ROOT / lock_value("GENERATOR_REQUIREMENTS_LOCK")),
                "--python-minor",
                lock_value("GENERATOR_PYTHON_MINOR"),
            ],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()

