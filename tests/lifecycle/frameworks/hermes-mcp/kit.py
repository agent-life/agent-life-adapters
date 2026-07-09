"""HermesMcpKit — the WP-M4 MCP LLM-in-the-loop release gate.

Hermes is the reference MCP *host*: it has a native MCP client, is already a
lifecycle framework with LLM-proxy wiring, and needs no new host software. This
kit is the plain HermesKit with two differences:

  * `wire_llm` also writes an `mcp_servers.alf` block into the profile's
    `config.yaml` — a stdio `alf mcp serve` server whose tools surface to the
    agent as `mcp_alf_*`. Hermes STRIPS the inherited environment from stdio
    children (verified — docs/mcp-surface-decision.md §A.2, zeroclaw findings
    §2), so the alf child's `ALF_API_KEY` / `ALF_API_URL` / `ALF_HOME` are
    declared explicitly in the server's own `env` block.

  * `mcp_llm_mode = True`, so the Z15 gate runs: the LLM agent drives sync and
    vault by CALLING the `mcp_alf_*` tools (not the terminal), and the harness
    asserts the effect through the ⊙ backend lanes plus an MCP-path marker.

Tier: `--llm proxy --backend real` (the release gate). This is the same bar the
shipped frameworks met on their CLI path — see the WP-M4 handoff runbook. It is
NOT a CI tier (needs a minted runtime key + LLM proxy) and is run once, with the
standard teardown ladder.
"""

from __future__ import annotations

import importlib.util
import re
import sys
from pathlib import Path

_LIFECYCLE = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(_LIFECYCLE))

from alflab.contract import TurnLog  # noqa: E402
from alflab.dockerctl import ALF_BIN  # noqa: E402
from alflab.report import Check  # noqa: E402

# The construction proof of the gate — "sync went via the MCP tool, not the
# terminal" — is only genuine if the agent has NO terminal path to `alf`. The
# harness installs alf at `/usr/local/bin/alf` (dockerctl.ALF_BIN); we launch the
# Hermes agent with a PATH that EXCLUDES /usr/local/{bin,sbin}, so a terminal
# `alf sync` is "command not found", while the MCP server child is spawned by
# ABSOLUTE path (see `_config_yaml`) and is unaffected. This is the brief's
# "remove alf from the agent's terminal PATH" option — chosen over a config-level
# tool-disable because the pinned Hermes exposes no verified tools-config key and
# a wrong key could break config parsing outright; a PATH scope cannot.
# FINALIZE AT LIVE-RUN: if the pinned Hermes builds its terminal tool's PATH
# itself rather than inheriting the process env, tighten this then — the positive
# MCP-path marker (below) is the backstop that keeps the gate from false-greening
# regardless.
_AGENT_PATH_NO_ALF = "/home/agent/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin"

# Load the plain HermesKit and subclass it. Loading its module runs the hermes
# kit's own `import seed_markers`, which would leave hermes's `seed_markers` in
# sys.modules — a driver run only ever loads one framework, but the TEST SUITE
# imports several kits in one process, so we save/restore the `seed_markers`
# entry to keep each framework's `import seed_markers` resolving to its own dir.
# (HermesKit captured its `seeder` reference during the load, so restoring the
# cache below does not disturb hermes's own seeding.)
_HERMES_KIT = _LIFECYCLE / "frameworks" / "hermes" / "kit.py"
_saved_seed = sys.modules.pop("seed_markers", None)
try:
    _spec = importlib.util.spec_from_file_location("kit_hermes_base", _HERMES_KIT)
    _hermes_mod = importlib.util.module_from_spec(_spec)
    _spec.loader.exec_module(_hermes_mod)
    HermesKit = _hermes_mod.KIT_CLASS
finally:
    if _saved_seed is not None:
        sys.modules["seed_markers"] = _saved_seed
    else:
        sys.modules.pop("seed_markers", None)

# The alf runtime is still "hermes"; only the harness DIR is hermes-mcp.
MCP_SERVER_NAME = "alf"


def mcp_tool(tool: str) -> str:
    """A tool `alf_sync` from a server declared `alf` surfaces to the Hermes
    agent as `mcp_{server}_{tool}` = `mcp_alf_alf_sync` (design §7.W2 /
    mcp-surface-decision.md §A.2)."""
    return f"mcp_{MCP_SERVER_NAME}_{tool}"


class HermesMcpKit(HermesKit):
    name = "hermes"                              # the alf runtime is hermes
    image_tag = "alf-lifecycle-hermes-mcp"       # layered on alf-lifecycle-hermes
    mcp_llm_mode = True
    watch_autosync_mode = True                   # the Z16 watch-auto-sync gate

    # -- Z16 watch auto-sync: mutate a watched file + the sqlite store ----------

    def mutate_watched(self, ctr, slot: str, i: int) -> str:
        """One Z16 watch-cycle mutation, written INSIDE the container (via `ctr`)
        so the loop's inotify watch fires natively — a host-side write to the bind
        mount does NOT generate a container inotify event, so the memory dir-watch
        would miss it. Appends a `§`-separated entry to `memories/MEMORY.md` (a
        REAL hermes semantic-memory source → one new memory RECORD, so the marker
        reaches the /memory DTO) AND upserts a row in the watched `state.db` (the
        additive `z16_watch` table Hermes ignores; the `.db` change is a raw
        delta). Returns the marker so the stage can assert it synced."""
        import shlex

        prof = self._container_profile(slot)
        marker = f"Z16-WATCH-{slot}-{i:02d}"
        # MEMORY.md entries are `\n§\n`-separated; append one (it exists from Z2).
        entry = f"\n§\nWatch entry {marker}: auto-synced by the Z16 watch loop at tick {i}."
        ctr.sh(
            f"mkdir -p {prof}/memories; "
            f"printf %s {shlex.quote(entry)} >> {prof}/memories/MEMORY.md",
            user="agent")
        ctr.exec(
            ["sqlite3", f"{prof}/state.db",
             "CREATE TABLE IF NOT EXISTS z16_watch (id INTEGER PRIMARY KEY, marker TEXT); "
             f"INSERT INTO z16_watch (id, marker) VALUES ({i}, '{marker}');"],
            user="agent")
        return marker

    # -- config wiring ---------------------------------------------------------

    def _config_yaml(self, creds, agent_id: str = "") -> str:
        """The full profile config.yaml: the model provider (as HermesKit writes
        it) PLUS the mcp_servers.alf block with an explicit env (Hermes strips
        inherited env from stdio children). `ALF_AGENT` is pinned once the mapping
        id is known (Z15); before that the sole enabled hermes agent resolves it.

        The env carries the minted runtime key — but it lives only in the
        gitignored, chmod-700 run dir, the adapter scopes mcp_servers OUT of the
        exported (and redacted) config.yaml (design §13), and every rendered sink
        passes through redact.py."""
        base = creds.llm_proxy_url.rstrip("/")
        if not base.endswith("/v1"):
            base += "/v1"
        env_lines = [
            f'      ALF_API_KEY: "{creds.runtime_api_key}"',
            f'      ALF_API_URL: "{creds.alf_api_url}"',
            # ALF_HOME is the home *base*; alf appends `.alf` (paths.rs), so this
            # must be /home/agent, NOT /home/agent/.alf — the latter resolves the
            # config to /home/agent/.alf/.alf/config.toml, a different file from
            # the mapping the harness builds, and the server never sees the agent.
            '      ALF_HOME: "/home/agent"',
            '      HOME: "/home/agent"',
        ]
        if agent_id:
            env_lines.insert(0, f'      ALF_AGENT: "{agent_id}"')
        env_block = "\n".join(env_lines)
        return (
            "model:\n"
            f'  default: "{creds.llm_model_id}"\n'
            '  provider: "custom"\n'
            f'  base_url: "{base}"\n'
            f'  api_key: "{creds.runtime_api_key}"\n'
            "agent:\n"
            "  max_turns: 12\n"
            # Tools surface as mcp_alf_* — the agent drives sync/vault by name.
            # `command` is the ABSOLUTE alf path (dockerctl.ALF_BIN): the agent
            # runs under a PATH that hides /usr/local/bin (see _AGENT_PATH_NO_ALF),
            # so the terminal can't reach `alf`, but Hermes still spawns the MCP
            # child by absolute path. Together these make "sync went via the tool"
            # a genuine construction, not an assumption.
            "mcp_servers:\n"
            "  alf:\n"
            f"    command: {ALF_BIN}\n"
            "    args: [mcp, serve, -r, hermes]\n"
            "    env:\n"
            f"{env_block}\n"
        )

    def wire_llm(self, ctr, creds) -> None:
        """Point the default profile at the LLM proxy AND declare the alf MCP
        server. Written host-side on the mounted profile dir."""
        self.env.host_home.mkdir(parents=True, exist_ok=True)
        (self.env.host_home / "config.yaml").write_text(
            self._config_yaml(creds), encoding="utf-8")

    def llm_wired(self) -> tuple:
        """After wire_llm: the proxy provider AND the alf MCP server must be
        present (config_text feeds the redaction self-check)."""
        cfg = self.env.host_home / "config.yaml"
        text = cfg.read_text(encoding="utf-8") if cfg.is_file() else ""
        wired = ('provider: "custom"' in text and "mcp_servers:" in text
                 and "alf mcp serve" not in text  # args are a list, not a string
                 and f"command: {ALF_BIN}" in text)
        return (wired, text)

    # -- Z15 gate --------------------------------------------------------------

    def drive_tool_via_agent(self, ctr, slot: str, instruction: str) -> TurnLog:
        """One Hermes chat turn that instructs the agent to call an `mcp_alf_*`
        tool. Reuses HermesKit's one-shot `hermes chat -Q` against the profile's
        HERMES_HOME; the agent's own MCP client spawns `alf mcp serve` and invokes
        the tool. Returns the turn (its tail is the model's report of the call).

        The turn runs under `_AGENT_PATH_NO_ALF` so the agent's terminal tool has
        no `alf` on its PATH — the only route to alf is the MCP server Hermes
        spawns by absolute path. This makes the MCP-path a genuine construction."""
        proc = ctr.exec(
            ["hermes", "chat", "-Q", "-q", instruction],
            env={"HERMES_HOME": self._container_profile(slot),
                 "PATH": _AGENT_PATH_NO_ALF},
            timeout=300,
        )
        tail = "\n".join(((proc.stdout or "") + (proc.stderr or "")).splitlines()[-15:])
        return TurnLog(slot=slot, turn_type="mcp", marker="mcp",
                       prompt=instruction, response_tail=tail,
                       ok=proc.returncode == 0)

    def _terminal_alf_reachable(self, ctr, slot: str) -> tuple:
        """Negative control for the construction proof: could the AGENT have run
        `alf` on the terminal? Resolve `alf` under the SAME restricted PATH the
        agent's Hermes process (and thus its inherited terminal tool) runs under.
        Reachable ⇒ a terminal `alf sync` was possible ⇒ the "via MCP" claim is
        unproven ⇒ the caller FAILs. Returns (reachable, where)."""
        probe = ctr.exec(["sh", "-c", "command -v alf || true"],
                         env={"PATH": _AGENT_PATH_NO_ALF}, user="agent")
        where = (probe.stdout or "").strip()
        return (bool(where), where or "(not on the agent's terminal PATH)")

    def _mcp_path_marker(self, ctr, slot: str) -> tuple:
        """Positive evidence that a sync went through the MCP tool: grep the
        profile's Hermes logs / session store for the tool name. Combined with
        `_terminal_alf_reachable` (which proves the agent had NO terminal alf),
        a positive marker here is a PASS; its absence is an XFAIL pending the
        live-run sink finalization (never a silent PASS). Returns (found, where)."""
        needle = mcp_tool("alf_sync")
        # Hermes routes child stderr and records tool calls in its session store;
        # the exact sink is finalized at live-run time (see the handoff). Grep the
        # candidate log files first (from the captured Hermes trees), then the
        # whole profile tree, for the tool name as a portable signal.
        prof = self._container_profile(slot)
        candidates = " ".join(f"{prof}/logs/{f}" for f in
                              ("agent.log", "errors.log", "conversation/transcript.log"))
        probe = ctr.sh(
            f"grep -lI {needle} {candidates} 2>/dev/null | head -1 || "
            f"grep -rIl {needle} {prof} 2>/dev/null | head -1", user="agent")
        where = (probe.stdout or "").strip()
        return (bool(where), where or f"(no {needle} marker file found; "
                "construction proof stands)")

    # -- MCP interaction ledger (WP-M6: make every mcp_alf_* call visible) ------

    # Hermes logs each tool call in agent.log via its tool_executor:
    #   success:  `... tool mcp_alf_alf_sync completed (0.07s, 595 chars)`
    #   failure:  `... Tool mcp_alf_alf_sync returned error (0.06s): {json}`
    # These are the AUTHORITATIVE record of what the agent actually invoked and
    # what the MCP server returned — not the model's prose report. The gate reads
    # them so a check reflects the real tool result (an errored mcp_alf_alf_sync
    # is a FAIL, not a green "the turn completed").
    _RE_OK = re.compile(r"\btool (mcp_alf_\w+) completed \(([\d.]+)s(?:, (\d+) chars)?\)")
    _RE_ERR = re.compile(r"\bTool (mcp_alf_\w+) returned error \(([\d.]+)s\): (.*)")
    _RE_CODE = re.compile(r'code\\?"\s*:\s*\\?"([a-z_]+)')

    # ALF's OWN server-side voice — a SECOND channel the agent.log tool_executor
    # lines above are blind to. Hermes captures each `alf mcp serve` stdio spawn's
    # stderr in mcp-stderr.log, one block per spawn:
    #   ===== [ts] starting MCP server 'alf' =====
    #   alf mcp serve: watch loop active (5 sources, agent <id>)   <- the agent ALF
    #                                                                  actually BOUND
    #   Encrypting with key from …/state/<id>/.alf-vault-key (fingerprint <fp>)
    # The ledger reads these so a run where ALF silently resolves the WRONG agent
    # (the ALF_HOME double-.alf bug: every mcp_alf call `completed` OK while creds
    # landed in a throwaway agent's vault) FAILs instead of reading green.
    # Capture the agent id by its delimiters (…agent <id>) / state/<id>/…), not a
    # charset — alf ids are UUIDs today but may be slugs (cf. kleo-a1b2), so an
    # `[0-9a-f-]` class would silently drop a valid id and mask a mismatch.
    _RE_SESSION = re.compile(r"^=====\s*\[(.+?)\]\s*starting MCP server")
    _RE_WATCH_OK = re.compile(r"watch loop active \((\d+) sources?, agent ([^)]+)\)")
    _RE_WATCH_NO = re.compile(r"watch loop not started:\s*(.*)")
    _RE_KEY = re.compile(r"state/([^/]+)/\.alf-vault-key \(fingerprint (\w+)\)")

    def _mcp_interactions(self, ctr, slot: str) -> list[dict]:
        """Parse agent.log into an ordered list of every mcp_alf_* tool call:
        `{tool, ok, dur, code, detail}`. Empty if the log is absent."""
        prof = self._container_profile(slot)
        out = ctr.sh(f"cat {prof}/logs/agent.log 2>/dev/null || true").stdout or ""
        calls: list[dict] = []
        for line in out.splitlines():
            m = self._RE_OK.search(line)
            if m:
                calls.append({"tool": m.group(1), "ok": True, "dur": m.group(2),
                              "code": None, "detail": f"{m.group(3) or '0'} chars"})
                continue
            m = self._RE_ERR.search(line)
            if m:
                raw = m.group(3)
                code = (self._RE_CODE.search(raw) or [None, None])[1]
                calls.append({"tool": m.group(1), "ok": False, "dur": m.group(2),
                              "code": code, "detail": raw[:200]})
        return calls

    def _last_call(self, ctr, slot: str, tool: str) -> dict | None:
        """The most recent mcp_alf_* interaction for `tool`, or None if never called."""
        matches = [c for c in self._mcp_interactions(ctr, slot) if c["tool"] == tool]
        return matches[-1] if matches else None

    def _server_sessions(self, ctr, slot: str) -> list[dict]:
        """Parse ALF's own mcp-stderr.log into one dict per `alf mcp serve` spawn:
        `{ts, bound_agent, watch_ok, watch_detail, key_agent, key_fp}`. This is the
        server-side view the agent.log ledger cannot see — which agent ALF actually
        resolved, whether its watch loop started, and the vault key it used. Empty
        if the log is absent (older Hermes may route child stderr elsewhere)."""
        prof = self._container_profile(slot)
        out = ctr.sh(f"cat {prof}/logs/mcp-stderr.log 2>/dev/null || true").stdout or ""
        sessions: list[dict] = []
        cur: dict | None = None
        for line in out.splitlines():
            m = self._RE_SESSION.search(line)
            if m:
                cur = {"ts": m.group(1), "bound_agent": None, "watch_ok": None,
                       "watch_detail": None, "key_agent": None, "key_fp": None}
                sessions.append(cur)
                continue
            if cur is None:
                continue
            m = self._RE_WATCH_OK.search(line)
            if m:
                cur["watch_ok"] = True
                cur["watch_detail"] = f"{m.group(1)} sources"
                cur["bound_agent"] = m.group(2)
                continue
            m = self._RE_WATCH_NO.search(line)
            if m:
                cur["watch_ok"] = False
                cur["watch_detail"] = m.group(1).strip()
                continue
            m = self._RE_KEY.search(line)
            if m:
                cur["key_agent"] = m.group(1)
                cur["key_fp"] = m.group(2)
        return sessions

    def _add_server_checks(self, window: list[dict], expected: str,
                           all_sessions: list[dict], result) -> None:
        """Assert ALF resolved the PINNED agent on every gate-window spawn. A
        wrong-agent binding, a mismatched vault key, or a `watch loop not started`
        is a first-class FAIL — the exact signal that was invisible when the ledger
        read only agent.log (creds silently written to a throwaway agent's vault)."""
        if not all_sessions:
            result.add(Check(
                name="server-resolved agent matches the pinned ALF_AGENT",
                status="SKIP",
                detail="mcp-stderr.log had no server-spawn sessions to parse — cannot "
                       "confirm which agent ALF bound (check Hermes stderr routing)"))
            return
        problems: list[str] = []
        for s in window:
            if s["watch_ok"] is False:
                problems.append(f"watch loop NOT started ({(s['watch_detail'] or '')[:70]})")
            if s["bound_agent"] and expected and s["bound_agent"] != expected:
                problems.append(f"server bound {s['bound_agent']} != pinned {expected}")
            if s["key_agent"] and expected and s["key_agent"] != expected:
                problems.append(f"vault op used key for {s['key_agent']} != pinned {expected}")
        resolved = [s for s in window if s["bound_agent"] or s["key_agent"]]
        if problems:
            result.add(Check(
                name="server-resolved agent matches the pinned ALF_AGENT",
                status="FAIL",
                detail="; ".join(dict.fromkeys(problems))[:320]))
        elif not resolved:
            result.add(Check(
                name="server-resolved agent matches the pinned ALF_AGENT",
                status="SKIP",
                detail="no gate-window spawn recorded a watch/vault agent line — cannot "
                       "confirm resolution (the tool may have errored before bind)"))
        else:
            result.add(Check(
                name="server-resolved agent matches the pinned ALF_AGENT",
                status="PASS",
                detail=f"all {len(resolved)} resolved gate spawn(s) bound {expected}"))

    def _tool_check(self, ctr, slot: str, tool: str, turn) -> Check:
        """A tool-drive check that reflects the REAL result: the tool must have
        been invoked AND completed without error (not just: the chat turn ran)."""
        call = self._last_call(ctr, slot, tool)
        if not turn.ok:
            return Check(name=f"agent turn drove {tool}", status="FAIL",
                         detail=f"the Hermes turn itself failed: {turn.response_tail[:120]}")
        if call is None:
            return Check(name=f"agent turn drove {tool}", status="FAIL",
                         detail=f"agent never invoked {tool} (no tool_executor record in agent.log)")
        if not call["ok"]:
            return Check(name=f"agent turn drove {tool}", status="FAIL",
                         detail=f"{tool} returned error ({call['dur']}s): "
                                f"{call['code'] or call['detail']}")
        return Check(name=f"agent turn drove {tool}", status="PASS",
                     detail=f"{tool} completed ({call['dur']}s, {call['detail']})")

    def _emit_interaction_ledger(self, ctr, slot: str, run, result,
                                 since_idx: int = 0) -> None:
        """Render every mcp_alf_* call into the report AND persist a
        `mcp-interactions.log` in the run dir. Errored calls render as FAIL so a
        failed MCP interaction can never hide behind a green summary.

        Also folds in ALF's OWN server-side log (mcp-stderr.log) — the second
        channel the agent.log tool_executor lines are blind to — and asserts the
        agent ALF actually resolved matches the pinned ALF_AGENT. `since_idx`
        scopes that assertion to the spawns THIS gate triggered (after the pin),
        so the earlier lifecycle stages' pre-pin spawns don't fail the gate."""
        expected = run.state.get("alf_agent_id", "")
        calls = self._mcp_interactions(ctr, slot)
        lines = [f"# MCP interactions (mcp_alf_*) — hermes-mcp Z15 gate",
                 f"# agent (pinned ALF_AGENT): {expected or '?'}", ""]
        # STRICT: an errored mcp_alf interaction is a FAIL — full stop. The gate
        # must NOT tolerate an agent that mis-calls a tool (even if it retries):
        # a fumble means the tool's input schema is unclear to a limited LLM, and
        # that is a PRODUCT signal to fix in the schema, not to paper over here.
        for i, c in enumerate(calls, 1):
            verdict = "OK" if c["ok"] else f"ERROR code={c['code'] or '?'}"
            lines.append(f"{i:>2}. {c['tool']:<28} {c['dur']:>5}s  {verdict}"
                         + ("" if c["ok"] else f"  :: {c['detail']}"))
            result.add(Check(
                name=f"  mcp interaction #{i}: {c['tool']}",
                status="PASS" if c["ok"] else "FAIL",
                detail=(f"ok ({c['dur']}s, {c['detail']})" if c["ok"]
                        else f"ERROR ({c['dur']}s): code={c['code'] or '?'} — {c['detail'][:140]}")))
        if not calls:
            result.add(Check(name="  mcp interactions: NONE recorded",
                             status="FAIL",
                             detail="agent.log shows no mcp_alf_* tool call — the "
                                    "agent never reached the MCP server"))
            lines.append("(none recorded)")

        # ALF's server-side voice — which agent it BOUND, whether the watch loop
        # started, the vault key it used. Blind spot of the agent.log ledger above.
        sessions = self._server_sessions(ctr, slot)
        window = sessions[since_idx:]
        lines.append("")
        lines.append(f"# server sessions (mcp-stderr.log): {len(sessions)} spawn(s); "
                     f"asserting agent resolution on {len(window)} gate-window spawn(s)")
        for j, s in enumerate(sessions, 1):
            if s["watch_ok"] is True:
                st = f"watch active ({s['watch_detail']}), agent={s['bound_agent']}"
            elif s["watch_ok"] is False:
                st = f"watch NOT started: {(s['watch_detail'] or '')[:80]}"
            else:
                st = "watch state unrecorded"
            key = f", key-agent={s['key_agent']}({s['key_fp']})" if s["key_agent"] else ""
            mark = "  <<gate" if j > since_idx else ""
            lines.append(f"  s{j} [{s['ts']}] {st}{key}{mark}")
        self._add_server_checks(window, expected, sessions, result)

        try:
            (run.paths.run_dir / "mcp-interactions.log").write_text(
                "\n".join(lines) + "\n", encoding="utf-8")
        except Exception:  # noqa: BLE001 — the report ledger is the primary sink
            pass

    def mcp_llm_gate(self, run, result) -> None:
        slot = self.agent_slots[0]
        agent_id = run.state.get("alf_agent_id", "")

        # (1) The host offers the agent the alf tools via an ABSOLUTE-path MCP
        # server; the agent's terminal PATH (below) hides alf entirely.
        cfg = (self.env.host_home / "config.yaml").read_text(encoding="utf-8")
        result.add(Check(
            name="config.yaml declares mcp_servers.alf (stdio `alf mcp serve`, absolute command, explicit env)",
            status="PASS" if ("mcp_servers:" in cfg and f"command: {ALF_BIN}" in cfg
                              and "ALF_API_KEY" in cfg) else "FAIL"))
        # Pin ALF_AGENT now that Z3 established the mapping (single agent also
        # resolves by sole-enabled, but the design pins it explicitly).
        if agent_id:
            (self.env.host_home / "config.yaml").write_text(
                self._config_yaml(run.creds, agent_id=agent_id), encoding="utf-8")

        # Snapshot the server-spawn count BEFORE the gate drives any tool, so the
        # server-side agent-resolution assertion (in the ledger) scopes to the
        # spawns THIS gate triggers under the pinned ALF_AGENT — not the earlier
        # lifecycle stages whose spawns ran before the pin.
        pre_sessions = len(self._server_sessions(run.container, slot))

        # (2) The agent drives the FIRST sync by calling the tool.
        sync_tool = mcp_tool("alf_sync")
        turn = self.drive_tool_via_agent(
            run.container, slot,
            f"You have ALF memory-continuity tools. Sync your memory to the cloud "
            f"now by calling the {sync_tool} tool (no arguments needed). After it "
            f"returns, tell me the sequence number.")
        # Reflect the REAL tool result from agent.log, not just turn.returncode:
        # the original gate greened on turn.ok even when mcp_alf_alf_sync errored
        # (e.g. agent_not_found) — WP-M6 review. Now an errored sync FAILs here.
        result.add(self._tool_check(run.container, slot, sync_tool, turn))

        # (3) ⊙ effect: the MCP-driven sync registered the agent + a snapshot.
        r = run.api.get(f"/agents/{agent_id}")
        body = r.json() if r.status_code == 200 else {}
        result.add(Check(name="⊙ API: agent registered by the MCP-driven sync",
                         status="PASS" if r.status_code == 200 else "FAIL",
                         detail=f"HTTP {r.status_code}"))
        result.add(Check(name="⊙ API: snapshot uploaded (latest_snapshot_seq set)",
                         status="PASS" if body.get("latest_snapshot_seq") is not None else "FAIL",
                         detail=f"latest_sequence={body.get('latest_sequence')}"))
        if agent_id and agent_id not in run.manifest.lifecycle_agents:
            run.manifest.lifecycle_agents.append(agent_id)
            run.manifest.save(run.paths.manifest)

        # (4) MCP-path marker — this check CAN fail. Two conditions gate it:
        #   * the agent must have had NO terminal path to alf (negative control);
        #     if `alf` was reachable on the agent PATH the "via MCP" claim is
        #     unproven → FAIL (the exact defect the WP-M4 review found).
        #   * a POSITIVE log marker (the tool name in the Hermes logs/session
        #     store) → PASS. Absent it, the construction proof stands but we lack
        #     positive proof → XFAIL pending the live-run sink finalization —
        #     never a silent PASS. The live proxy/real run upgrades this to PASS
        #     once the log/session sink is confirmed (WP-M6 task 6).
        reachable, r_where = self._terminal_alf_reachable(run.container, slot)
        found, m_where = self._mcp_path_marker(run.container, slot)
        marker_name = "MCP-path: agent had no terminal alf AND sync used mcp_alf_alf_sync"
        if reachable:
            result.add(Check(
                name=marker_name, status="FAIL",
                detail=(f"construction BROKEN: `alf` resolves on the agent's "
                        f"terminal PATH ({r_where}) — a terminal `alf sync` was "
                        f"possible, so 'via MCP' is unproven")))
        elif found:
            result.add(Check(
                name=marker_name, status="PASS",
                detail=f"positive marker in {m_where}; terminal alf blocked"))
        else:
            result.add(Check(
                name=marker_name, status="XFAIL",
                xfail_id="wpm6-mcp-path-marker-sink",
                detail=("terminal alf blocked (construction proof holds) but no "
                        "positive log marker found — finalize the log/session "
                        "sink at live-run, then this becomes a PASS")))

        # (5) The agent drives a vault add by calling the tool.
        secret = scenario_secret(slot)
        vault_tool = mcp_tool("alf_vault_add")
        turn2 = self.drive_tool_via_agent(
            run.container, slot,
            f"Store a throwaway TEST credential in your vault by calling the "
            f"{vault_tool} tool with service=\"email\", label=\"z15\", and "
            f"secret=\"{secret}-DO-NOT-USE\". Confirm when done.")
        result.add(self._tool_check(run.container, slot, vault_tool, turn2))

        # (6) ⊙ Layer-4 ciphertext rode the next MCP-driven sync (export copy-out).
        run.alf.exec(["export", "-r", self.name, "--agent", slot,
                      "-o", "/home/agent/.alf/z15-export.alf"], timeout=300)
        host_alf = run.paths.alf_home / "z15-export.alf"
        if host_alf.is_file():
            from alflab import archives
            names = archives.entries(host_alf)
            result.add(Check(name="vault ciphertext present in the agent's Layer 4",
                             status="PASS" if "credentials.json" in names else "FAIL",
                             detail=f"{len(names)} entries"))

        # (7) Full MCP interaction ledger — every mcp_alf_* call the agent made,
        # with its real result, into the report + a run-dir mcp-interactions.log.
        # This is the record that makes a failed tool call visible instead of
        # masked behind the model's prose (WP-M6), and folds in ALF's server-side
        # log to assert it resolved the pinned agent (not a throwaway).
        self._emit_interaction_ledger(run.container, slot, run, result,
                                      since_idx=pre_sessions)


def scenario_secret(slot: str) -> str:
    from alflab import scenario
    return scenario.marker_for(slot, "secret", 1)


KIT_CLASS = HermesMcpKit
