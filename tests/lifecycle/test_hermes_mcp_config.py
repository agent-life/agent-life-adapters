"""Self-tests for the Hermes MCP-host config wiring (WP-M4 task 4).

The MCP LLM tier itself is a live gate (`--llm proxy --backend real`, run once —
see the WP-M4 handoff), so it can't run in CI. What CAN run here, container- and
backend-free, is the ONE thing that is easy to get wrong and load-bearing: the
`mcp_servers.alf` block the kit writes into the Hermes profile config.yaml —
explicit env (Hermes strips inherited env from stdio children), the right
command/args, and the `mcp_alf_*` tool-name derivation. Stdlib-only string
assertions (PyYAML deepens them when present, but is not required in CI).

Run:  python3 -m unittest tests.lifecycle.test_hermes_mcp_config
"""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

LIFECYCLE_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(LIFECYCLE_DIR))

from alflab.contract import KitEnv  # noqa: E402
from alflab.dockerctl import ALF_BIN  # noqa: E402
from alflab.provision import RuntimeCreds  # noqa: E402
from alflab.redact import redact, register_secret  # noqa: E402

# Obviously fake AND cannot match the secret gate `alf_[A-Za-z0-9]{32}` (the
# hyphens break the 32-alphanumeric run), like the committed scenario secrets.
FAKE_KEY = "alf_FAKE-do-not-use-lifecycle-test-key"


def _load_hermes_mcp():
    spec = importlib.util.spec_from_file_location(
        "kit_hermes_mcp_test", LIFECYCLE_DIR / "frameworks" / "hermes-mcp" / "kit.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# -- fakes for the server-side ledger unit tests (no container/backend) --------

class _NS:
    def __init__(self, **kw): self.__dict__.update(kw)


class _FakeProc:
    def __init__(self, out): self.stdout = out


class _FakeCtr:
    """Returns canned mcp-stderr.log / agent.log content regardless of the path
    the kit builds, so `_server_sessions` / `_emit_interaction_ledger` can be
    exercised without a container."""
    def __init__(self, stderr_log: str = "", agent_log: str = ""):
        self._stderr, self._agent = stderr_log, agent_log

    def sh(self, cmd, **kw):
        if "mcp-stderr.log" in cmd:
            return _FakeProc(self._stderr)
        if "agent.log" in cmd:
            return _FakeProc(self._agent)
        return _FakeProc("")


class _FakeResult:
    def __init__(self): self.checks = []
    def add(self, c): self.checks.append(c)


def _session_block(ts, agent=None, not_started=None, fp=None) -> str:
    """One `alf mcp serve` spawn block in the mcp-stderr.log format."""
    out = [f"===== [{ts}] starting MCP server 'alf' =====",
           "alf mcp serve: stdio server ready (runtime=hermes)"]
    if agent:
        out.append(f"alf mcp serve: watch loop active (5 sources, agent {agent})")
    if not_started:
        out.append(f"alf mcp serve: watch loop not started: {not_started}")
    if fp:
        out.append(f"Encrypting with key from file:/home/agent/.alf/.hermes/state/"
                   f"{agent or 'unknown'}/.alf-vault-key (fingerprint {fp})")
    out.append("alf mcp serve: stopped (Closed)")
    return "\n".join(out)


def _agent_ok(tool, chars=100) -> str:
    """A Hermes tool_executor success line, as agent.log records it."""
    return (f"2026-07-08 21:00:00 INFO agent.tool_executor: "
            f"tool {tool} completed (0.01s, {chars} chars)")


def _agent_err(tool, msg) -> str:
    """A Hermes tool_executor error line."""
    return (f'2026-07-08 21:00:00 WARNING agent.tool_executor: Tool {tool} '
            f'returned error (0.00s): {{"error": "{msg}"}}')


class HermesMcpConfigTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.mod = _load_hermes_mcp()
        cls.tmp = Path(tempfile.mkdtemp(prefix="alf-m4-hermes-mcp-"))
        env = KitEnv(run_dir=cls.tmp, host_home=cls.tmp / "home",
                     host_alf_home=cls.tmp / "alf", llm="proxy", backend="real")
        env.host_home.mkdir(parents=True)
        cls.creds = RuntimeCreds(
            runtime_api_key=FAKE_KEY, alf_api_url="https://api.example/v1",
            llm_proxy_url="https://proxy.example", llm_model_id="minimax.minimax-m2.5",
            seed_agent_id="seed", tenant_id="t", runtime_id="r")
        env.creds = cls.creds
        cls.kit = cls.mod.KIT_CLASS(env)
        cls.kit.wire_llm(None, cls.creds)
        cls.cfg = (env.host_home / "config.yaml").read_text(encoding="utf-8")

    @classmethod
    def tearDownClass(cls):
        import shutil
        shutil.rmtree(cls.tmp, ignore_errors=True)

    def test_tool_name_is_mcp_server_tool(self):
        self.assertEqual(self.mod.mcp_tool("alf_sync"), "mcp_alf_alf_sync")
        self.assertEqual(self.mod.mcp_tool("alf_vault_add"), "mcp_alf_alf_vault_add")

    def test_config_has_model_provider_and_mcp_server(self):
        self.assertIn('provider: "custom"', self.cfg)
        self.assertIn("mcp_servers:", self.cfg)
        # ABSOLUTE command: the agent's terminal PATH hides alf, but Hermes still
        # spawns the MCP child by absolute path (the WP-M6 task-0a construction).
        self.assertIn(f"command: {ALF_BIN}", self.cfg)
        self.assertIn("args: [mcp, serve, -r, hermes]", self.cfg)

    def test_mcp_command_is_absolute(self):
        # A relative `command: alf` would resolve on the agent PATH — the very
        # thing we hide — so the MCP server must be spawned by absolute path.
        self.assertTrue(ALF_BIN.startswith("/"),
                        "the MCP server command must be an absolute path")
        self.assertNotIn("command: alf\n", self.cfg)

    def test_agent_terminal_path_excludes_alf_dir(self):
        # The construction proof depends on the agent's terminal PATH NOT
        # containing the dir alf is installed in (dockerctl.ALF_BIN's parent).
        alf_dir = ALF_BIN.rsplit("/", 1)[0]
        parts = self.mod._AGENT_PATH_NO_ALF.split(":")
        self.assertNotIn(alf_dir, parts,
                         f"{alf_dir} (where alf lives) must be off the agent PATH")
        self.assertIn("/home/agent/.local/bin", parts,
                      "hermes itself must still resolve on the agent PATH")

    def test_env_is_declared_explicitly(self):
        # Hermes strips inherited env → the alf child's vars MUST be in the block.
        for var in ("ALF_API_KEY", "ALF_API_URL", "ALF_HOME", "HOME"):
            self.assertIn(var, self.cfg, f"{var} must be declared in the server env")

    def test_llm_wired_predicate(self):
        wired, _ = self.kit.llm_wired()
        self.assertTrue(wired)

    def test_agent_id_is_pinned_when_known(self):
        # The Z15 gate re-writes the config once the mapping id exists.
        pinned = self.kit._config_yaml(self.creds, agent_id="kleo-a1b2")
        self.assertIn('ALF_AGENT: "kleo-a1b2"', pinned)

    def test_runtime_key_is_redactable(self):
        register_secret(FAKE_KEY)
        self.assertNotIn(FAKE_KEY, redact(self.cfg))

    def test_valid_yaml_structure_when_pyyaml_present(self):
        try:
            import yaml
        except ImportError:
            self.skipTest("PyYAML not installed (CI is stdlib-only)")
        doc = yaml.safe_load(self.cfg)
        alf = doc["mcp_servers"]["alf"]
        self.assertEqual(alf["command"], ALF_BIN)
        self.assertEqual(alf["args"], ["mcp", "serve", "-r", "hermes"])
        self.assertEqual(
            sorted(alf["env"]),
            ["ALF_API_KEY", "ALF_API_URL", "ALF_HOME", "ALF_WATCH_QUIESCE_MS",
             "ALF_WATCH_TICK_MS", "HOME"])

    def test_watch_loop_is_suppressed_for_the_hermes_spawned_server(self):
        # Z03 asserts lazy registration by probing the backend for the mapping
        # id. Hermes spawns this server for every agent turn, INCLUDING Z02's,
        # and a fresh watch loop's first sync is due immediately (`last_fire =
        # None` ⇒ `cooled_down`), so a session that lived ~5 s registered the
        # agent and Z03 saw HTTP 200. It fired on 3 of 42 recorded runs.
        #
        # Both values are the 24 h clamp ceiling: QUIESCE means no source is ever
        # quiesced (no sync can fire however the ticks land), TICK covers a source
        # added later by rediscovery whose `last_change = None` reads as quiesced.
        # Drop either and the race comes back as a rare red on a LIVE tier —
        # so this is pinned rather than left to the comment in kit.py.
        for var in ("ALF_WATCH_QUIESCE_MS", "ALF_WATCH_TICK_MS"):
            self.assertIn(f'{var}: "86400000"', self.cfg,
                          f"{var} must pin the watch loop off for this server")


class MintRuntimeVariantTest(unittest.TestCase):
    """The driver mints keys and runs `alf purge -r <runtime>` on the alf RUNTIME,
    not the harness framework dir. For hermes-mcp those differ (framework
    `hermes-mcp`, runtime `hermes`); the provisioner only knows
    openclaw|zeroclaw|hermes, so a mint with `hermes-mcp` fails outright. This
    pins the resolution (regression: the WP-M4 live gate mis-minted `hermes-mcp`)."""

    def test_kit_runtime_name_maps_framework_to_alf_runtime(self):
        from alflab.runner import kit_runtime_name
        self.assertEqual(kit_runtime_name("hermes-mcp"), "hermes")
        # base frameworks: framework dir == runtime (no regression)
        for fw in ("openclaw", "zeroclaw", "hermes"):
            self.assertEqual(kit_runtime_name(fw), fw)


class HermesMcpServerLedgerTest(unittest.TestCase):
    """The ledger folds in ALF's OWN server log (mcp-stderr.log) and asserts the
    agent ALF *resolved* matches the pinned ALF_AGENT. Without this, the ALF_HOME
    double-`.alf` bug read green: every mcp_alf call `completed`, but ALF had
    silently bound a throwaway workspace-derived agent and written creds to its
    vault. These pin that the assertion FAILs a wrong-agent run and PASSes a
    correct one (regression: the live Z15 run 20260708T200537Z greened 14/14
    while ALF bound 7fc50a28 instead of the pinned 4fb553ed)."""

    @classmethod
    def setUpClass(cls):
        cls.mod = _load_hermes_mcp()
        cls.tmp = Path(tempfile.mkdtemp(prefix="alf-m6-server-ledger-"))

    @classmethod
    def tearDownClass(cls):
        import shutil
        shutil.rmtree(cls.tmp, ignore_errors=True)

    def _kit(self):
        k = self.mod.KIT_CLASS.__new__(self.mod.KIT_CLASS)
        k._container_profile = lambda slot: "/home/agent/.hermes"
        return k

    @staticmethod
    def _server_check(checks):
        return next((c for c in checks if "server-resolved agent" in c.name), None)

    def test_parses_every_spawn_binding_and_key(self):
        log = "\n\n".join([
            _session_block("t1", agent="AAA-agent", fp="fp1"),
            _session_block("t2", not_started="No agent named 'BBB' for runtime 'hermes'"),
            _session_block("t3", agent="AAA-agent", fp="fp2"),
        ])
        sessions = self._kit()._server_sessions(_FakeCtr(stderr_log=log), "default")
        self.assertEqual(len(sessions), 3)
        self.assertEqual(sessions[0]["bound_agent"], "AAA-agent")
        self.assertEqual(sessions[0]["key_fp"], "fp1")
        self.assertIs(sessions[0]["watch_ok"], True)
        self.assertIs(sessions[1]["watch_ok"], False)
        self.assertIsNone(sessions[1]["bound_agent"])

    def test_wrong_agent_binding_fails(self):
        k = self._kit()
        w = k._server_sessions(_FakeCtr(stderr_log=_session_block("t", agent="WRONG", fp="fp")), "default")
        res = _FakeResult()
        k._add_server_checks(w, "PINNED", w, res)
        c = self._server_check(res.checks)
        self.assertEqual(c.status, "FAIL")
        self.assertIn("WRONG", c.detail)

    def test_watch_not_started_fails(self):
        k = self._kit()
        w = k._server_sessions(
            _FakeCtr(stderr_log=_session_block("t", not_started="No agent named 'PINNED'")), "default")
        res = _FakeResult()
        k._add_server_checks(w, "PINNED", w, res)
        self.assertEqual(self._server_check(res.checks).status, "FAIL")

    def test_correct_binding_passes(self):
        k = self._kit()
        w = k._server_sessions(_FakeCtr(stderr_log=_session_block("t", agent="PINNED", fp="fp")), "default")
        res = _FakeResult()
        k._add_server_checks(w, "PINNED", w, res)
        self.assertEqual(self._server_check(res.checks).status, "PASS")

    def test_missing_mcp_stderr_log_fails_closed(self):
        # FAIL-CLOSED (WP-O.4, replaces the old skips-never-fails behavior): the
        # server ALWAYS writes its startup banner to stderr, so a missing/empty
        # mcp-stderr.log means the capture plumbing is broken and the gate's
        # server-side lane is blind — that must FAIL, not SKIP into a green run.
        k = self._kit()
        res = _FakeResult()
        k._add_server_checks([], "PINNED", [], res)
        c = self._server_check(res.checks)
        self.assertEqual(c.status, "FAIL")
        self.assertIn("fails closed", c.detail)

    def test_no_resolved_spawn_in_window_still_skips(self):
        # The LATER legitimate SKIP branch survives: sessions were captured (the
        # plumbing works) but no gate-window spawn recorded a watch/vault agent
        # line — the tool may have errored before bind; that is a SKIP, not a
        # fail-closed condition.
        k = self._kit()
        log = "===== [t] starting MCP server 'alf' =====\n" \
              "alf mcp serve: stdio server ready (runtime=hermes)"
        sessions = k._server_sessions(_FakeCtr(stderr_log=log), "default")
        res = _FakeResult()
        k._add_server_checks(sessions, "PINNED", sessions, res)
        self.assertEqual(self._server_check(res.checks).status, "SKIP")

    def test_since_idx_scopes_to_gate_window(self):
        # A PRE-pin spawn bound the wrong agent; the gate-window spawn bound the
        # pinned one. since_idx must exclude the pre-pin spawn (a different stage's
        # concern) → PASS; but at since_idx=0 the wrong spawn is in scope → FAIL.
        log = "\n\n".join([
            _session_block("pre", agent="WRONG", fp="fpx"),
            _session_block("gate", agent="PINNED", fp="fpy"),
        ])
        k = self._kit()
        run = _NS(state={"alf_agent_id": "PINNED"},
                  container=_FakeCtr(stderr_log=log),
                  paths=_NS(run_dir=self.tmp))
        res_scoped = _FakeResult()
        k._emit_interaction_ledger(run.container, "default", run, res_scoped, since_idx=1)
        self.assertEqual(self._server_check(res_scoped.checks).status, "PASS")

        res_all = _FakeResult()
        k._emit_interaction_ledger(run.container, "default", run, res_all, since_idx=0)
        self.assertEqual(self._server_check(res_all.checks).status, "FAIL")

    def test_ledger_file_records_server_sessions(self):
        # The persisted mcp-interactions.log must now carry ALF's server-side view.
        log = _session_block("t", agent="PINNED", fp="fp")
        k = self._kit()
        run = _NS(state={"alf_agent_id": "PINNED"},
                  container=_FakeCtr(stderr_log=log),
                  paths=_NS(run_dir=self.tmp))
        k._emit_interaction_ledger(run.container, "default", run, _FakeResult(), since_idx=0)
        text = (self.tmp / "mcp-interactions.log").read_text(encoding="utf-8")
        self.assertIn("server sessions (mcp-stderr.log)", text)
        self.assertIn("watch active", text)
        self.assertIn("PINNED", text)

    def _interaction_checks(self, agent_log):
        k = self._kit()
        run = _NS(state={"alf_agent_id": "PINNED"},
                  container=_FakeCtr(agent_log=agent_log), paths=_NS(run_dir=self.tmp))
        res = _FakeResult()
        k._emit_interaction_ledger(run.container, "default", run, res, since_idx=0)
        return [c for c in res.checks if c.name.strip().startswith("mcp interaction")]

    def test_recovered_tool_error_is_still_fail(self):
        # STRICT gate: even when the agent mis-calls a tool and RETRIES successfully
        # (the live Z15 run 20260708T210926Z: vault_delete with two selectors, then
        # a valid retry), the errored interaction is a FAIL — the fumble is a
        # product signal (unclear tool schema), not something the harness tolerates.
        # This locks the revert of the earlier recovered->XFAIL compensation.
        inter = self._interaction_checks("\n".join([
            _agent_err("mcp_alf_alf_vault_delete", "Pass only one selector (id, label, or service)"),
            _agent_ok("mcp_alf_alf_vault_delete", 436),
        ]))
        self.assertEqual([c.status for c in inter], ["FAIL", "PASS"])

    def test_unrecovered_tool_error_is_fail(self):
        # The agent_not_found class: an op errors and never succeeds → FAIL (the
        # reason the per-interaction checks exist).
        inter = self._interaction_checks(_agent_err("mcp_alf_alf_sync", "agent_not_found"))
        self.assertEqual(inter[0].status, "FAIL")

    def test_one_good_call_does_not_whitewash_another_failure(self):
        # A success on one tool must never turn another tool's error green.
        inter = self._interaction_checks("\n".join([
            _agent_err("mcp_alf_alf_sync", "agent_not_found"),
            _agent_ok("mcp_alf_alf_vault_add", 400),
        ]))
        self.assertEqual(inter[0].status, "FAIL")
        self.assertEqual(inter[1].status, "PASS")


if __name__ == "__main__":
    unittest.main()
