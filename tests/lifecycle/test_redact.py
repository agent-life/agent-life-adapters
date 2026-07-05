"""Self-test for the central redaction (plain unittest — no pytest).

Run: python3 -m unittest discover -s tests/lifecycle -p 'test_*.py'
"""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from alflab.redact import redact, redact_obj  # noqa: E402

FAKE_KEY = "alf_" + "a1B2" * 8  # matches alf_[A-Za-z0-9]{32}


class RedactTests(unittest.TestCase):
    def test_runtime_key_shape(self):
        self.assertNotIn(FAKE_KEY, redact(f"key is {FAKE_KEY} ok"))
        self.assertIn("alf_[REDACTED]", redact(FAKE_KEY))

    def test_bearer_header(self):
        out = redact("Authorization: Bearer abc.def-123")
        self.assertNotIn("abc.def-123", out)
        self.assertIn("Bearer [REDACTED]", out)

    def test_toml_api_key_line(self):
        out = redact('uri = "https://x"\napi_key = "supersecretvalue"\nmodel = "m"')
        self.assertNotIn("supersecretvalue", out)
        self.assertIn('uri = "https://x"', out)
        self.assertIn('model = "m"', out)

    def test_env_file_lines(self):
        out = redact(f"ALF_API_URL=https://x\nALF_API_KEY={FAKE_KEY}\nRUNTIME_API_KEY=zz")
        self.assertNotIn(FAKE_KEY, out)
        self.assertNotIn("RUNTIME_API_KEY=zz", out)
        self.assertIn("ALF_API_URL=https://x", out)

    def test_postgres_password(self):
        out = redact("postgres://user:hunter2@host.neon.tech/db")
        self.assertNotIn("hunter2", out)
        self.assertIn("user:[REDACTED]@host.neon.tech", out)

    def test_fake_markers_survive(self):
        for marker in ("sk-atlas-r1-FAKE-1A2B", "ATLAS-SEM1-7F3A", "nova-FAKE-9z8y7x"):
            self.assertEqual(marker, redact(marker))

    def test_idempotent(self):
        once = redact(f"Bearer {FAKE_KEY}")
        self.assertEqual(once, redact(once))

    def test_redact_obj_walks(self):
        obj = {"a": [FAKE_KEY, {"b": f"Bearer {FAKE_KEY}"}], "n": 7}
        red = redact_obj(obj)
        self.assertNotIn(FAKE_KEY, str(red))
        self.assertEqual(red["n"], 7)


if __name__ == "__main__":
    unittest.main()


class RegisteredSecretTests(unittest.TestCase):
    """Review fixes: exact-value + truncated-prefix scrubbing must survive
    slicing that breaks the pattern shapes."""

    def test_registered_secret_exact_and_prefix(self):
        from alflab.redact import redact, register_secret
        key = "alf_" + "c1c9" * 8  # full repo-shape key
        register_secret(key)
        try:
            self.assertNotIn(key, redact(f"invalid api key {key} rejected"))
            # A slice cutting the key mid-token defeats the 32-char pattern —
            # the registered prefix scrub must still kill it.
            truncated = f"invalid api key {key}"[:30]
            out = redact(truncated)
            self.assertNotIn(key[:14], out, f"prefix leaked: {out!r}")
        finally:
            from alflab import redact as mod
            mod._SECRET_VALUES.discard(key)

    def test_short_values_not_registered(self):
        from alflab import redact as mod
        mod.register_secret("short")
        self.assertNotIn("short", mod._SECRET_VALUES)
