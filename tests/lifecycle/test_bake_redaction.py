"""Self-tests for the bake-time secret redaction (MAJ-10).

`viz/bake.py` inlines run-dir artifacts verbatim into the customer-share HTML;
a proxy-tier run's ``home/config.yaml`` holds the raw minted runtime key. The
bake must (1) harvest the run's own recorded secret values (run.env,
env-files/), (2) redact every inlined string, and (3) REFUSE to write a file
that still carries a secret — a leak in a shared artifact is worse than no
bake at all.

Run:  python3 -m unittest tests.lifecycle.test_bake_redaction
"""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

_VIZ = Path(__file__).resolve().parent / "viz" / "bake.py"
_SPEC = importlib.util.spec_from_file_location("bake", _VIZ)
bake = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(bake)

# A runtime-key-shaped token (alf_ + 32 alnum) — caught by pattern alone.
KEY_SHAPED = "alf_" + "A1b2C3d4" * 4
# An exotic value matching NO pattern — only the run.env harvest catches it.
EXOTIC = "zz-exotic-secret-0123456789"


def _run_dir(tmp: Path) -> Path:
    run = tmp / "run"
    (run / "home").mkdir(parents=True)
    (run / "env-files").mkdir()
    run.joinpath("events.ndjson").write_text(
        json.dumps({"type": "state", "note": f"leaked {KEY_SHAPED} in an event"}) + "\n",
        encoding="utf-8",
    )
    run.joinpath("run.env").write_text(
        f"ALF_API_KEY={KEY_SHAPED}\nEXOTIC_PROVIDER_KEY={EXOTIC}\n", encoding="utf-8"
    )
    # The hermes-mcp kit's exact YAML shapes, plus a field only the exact-value
    # harvest can recognize as secret.
    run.joinpath("home", "config.yaml").write_text(
        f'ALF_API_KEY: "{KEY_SHAPED}"\napi_key: "{KEY_SHAPED}"\n'
        f"unlabeled_field: {EXOTIC}\n",
        encoding="utf-8",
    )
    run.joinpath("report.json").write_text(json.dumps({"verdict": "ok"}), encoding="utf-8")
    return run


class BakeRedactionTest(unittest.TestCase):
    def test_baked_html_carries_no_run_secret(self):
        with tempfile.TemporaryDirectory() as tmp:
            run = _run_dir(Path(tmp))
            out = Path(tmp) / "portable.html"
            bake.bake(run, out)
            html = out.read_text(encoding="utf-8")
            self.assertNotIn(KEY_SHAPED, html, "runtime-key-shaped token leaked")
            self.assertNotIn(EXOTIC, html, "harvested exact-value secret leaked")
            self.assertIn("[REDACTED]", html, "redaction placeholders present")
            # The artifact is still inlined (redacted, not dropped).
            self.assertIn("home/config.yaml", html)

    def test_bake_refuses_to_write_on_a_survivor(self):
        with self.assertRaises(SystemExit):
            bake.assert_no_secrets(f"<html>{EXOTIC}</html>", [EXOTIC])
        with self.assertRaises(SystemExit):
            bake.assert_no_secrets(f"<html>{KEY_SHAPED}</html>", [])
        # Clean HTML passes.
        bake.assert_no_secrets("<html>alf_[REDACTED]</html>", [EXOTIC])

    def test_yaml_key_assignments_are_pattern_redacted(self):
        # The redact.py gap MAJ-10 exposed: config.yaml uses `key: value`,
        # which the TOML/env patterns never matched.
        from alflab import redact

        out = redact.redact('api_key: "some-plain-value"\nALF_API_KEY: xyz\n')
        self.assertNotIn("some-plain-value", out)
        self.assertNotIn("xyz", out)
        self.assertIn("[REDACTED]", out)


if __name__ == "__main__":
    unittest.main()
