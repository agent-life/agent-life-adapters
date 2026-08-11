"""Self-tests for the docker exec env plumbing (WP-O.7).

Secrets must never appear as `-e K=V` argv elements — argv is world-readable
via /proc/*/cmdline and `ps`, so a minted runtime key on the exec command line
leaks to every local user for the lifetime of the call. `DockerContainer` now
routes every exec env through a per-call, uuid-named, 0600 `--env-file` under
the run dir's `env-files/`. `_exec_argv` is factored exactly so this property
is assertable without docker.

Run:  python3 -m unittest tests.lifecycle.test_dockerctl_env
"""

from __future__ import annotations

import stat
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from alflab.dockerctl import DockerContainer  # noqa: E402

SECRET = "alf-FAKE-runtime-key-do-not-use"


class DockerExecEnvTest(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="alf-envfile-test-"))
        self.ctr = DockerContainer("test-ctr", "test-img",
                                   env_dir=self.tmp / "env-files")

    def tearDown(self):
        import shutil
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _env_file_from(self, argv: list) -> Path:
        self.assertIn("--env-file", argv)
        return Path(argv[argv.index("--env-file") + 1])

    def test_secret_never_appears_on_argv(self):
        argv = self.ctr._exec_argv(["alf", "sync"],
                                   env={"ALF_API_KEY": SECRET, "ALF_AGENT": "a1"})
        for element in argv:
            self.assertNotIn(SECRET, element,
                             f"secret leaked into argv element {element!r}")
        # And no -e K=V mechanism at all — the flag itself is banned.
        self.assertNotIn("-e", argv)

    def test_env_file_is_0600_with_correct_content(self):
        argv = self.ctr._exec_argv(["alf", "sync"],
                                   env={"ALF_API_KEY": SECRET, "ALF_AGENT": "a1"})
        path = self._env_file_from(argv)
        self.assertTrue(path.is_file())
        mode = stat.S_IMODE(path.stat().st_mode)
        self.assertEqual(mode, 0o600, f"env file mode is {oct(mode)}, want 0600")
        content = path.read_text(encoding="utf-8")
        self.assertIn(f"ALF_API_KEY={SECRET}\n", content)
        self.assertIn("ALF_AGENT=a1\n", content)

    def test_env_files_land_in_env_dir_and_are_unique_per_exec(self):
        a1 = self.ctr._exec_argv(["true"], env={"K": "v1"})
        a2 = self.ctr._exec_argv(["true"], env={"K": "v2"})
        p1, p2 = self._env_file_from(a1), self._env_file_from(a2)
        self.assertNotEqual(p1, p2)  # uuid-named per exec, never reused
        for p in (p1, p2):
            self.assertEqual(p.parent, self.tmp / "env-files")
            self.assertTrue(p.name.startswith("exec-"))
            self.assertTrue(p.name.endswith(".env"))

    def test_no_env_means_no_env_file_flag(self):
        argv = self.ctr._exec_argv(["alf", "--version"])
        self.assertNotIn("--env-file", argv)
        self.assertEqual(argv[:4], ["docker", "exec", "-u", "agent"])
        self.assertEqual(argv[4:], ["test-ctr", "alf", "--version"])

    def test_stdio_variant_keeps_stdin_and_env_file(self):
        argv = self.ctr._exec_argv(["alf", "mcp", "serve"], stdio=True,
                                   env={"ALF_API_KEY": SECRET})
        self.assertEqual(argv[:3], ["docker", "exec", "-i"])
        self.assertIn("--env-file", argv)
        for element in argv:
            self.assertNotIn(SECRET, element)

    def test_unwired_env_dir_falls_back_to_private_tmp(self):
        # A container built without runner wiring (e.g. the teardown CLI) still
        # never puts a secret on argv: the env file goes to a private temp dir.
        ctr = DockerContainer("bare-ctr", "img")
        argv = ctr._exec_argv(["true"], env={"K": SECRET})
        path = self._env_file_from(argv)
        self.assertTrue(path.is_file())
        self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
        for element in argv:
            self.assertNotIn(SECRET, element)
        # cleanup the fallback dir
        import shutil
        shutil.rmtree(ctr.env_dir, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()
