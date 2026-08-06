#!/usr/bin/env python3
"""Regression tests for the RF-025 integration fixture tooling."""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPTS_DIR = REPO_ROOT / "scripts"
sys.path.insert(0, str(SCRIPTS_DIR))

from generate_synthetic_data import FixtureInputs, generate_fixture  # noqa: E402

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

