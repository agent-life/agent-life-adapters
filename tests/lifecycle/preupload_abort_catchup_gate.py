#!/usr/bin/env python3
"""Pre-upload abort catch-up gate (WP-M6 task 0b / WP-M4 review Finding 2).

WHAT THIS PROVES — stated honestly (WP-O.9): the fault build's
`ALF_WATCH_FAULT_BEFORE_UPLOAD` seam makes the watch loop take a COOPERATIVE
`exit(137)` at the pre-upload point. The exit CODE mimics 128+SIGKILL, but no
kernel signal is delivered — so the gate proves catch-up + no-duplicate under
worst-moment process death AT THAT SEAM (the process vanishes with base+state
still at the old sequence, nothing uploaded), NOT kernel SIGKILL delivery
semantics. A true-SIGKILL variant (`true_sigkill_catchup_gate`: MockBackend's
blocking-upload knob holds the upload open + `os.kill(pid, SIGKILL)`
mid-upload) is a SPECCED FOLLOW-UP — deliberately not implemented here.

The WP-M3 review (D1) built the crash seam — `--features fault-injection` +
`ALF_WATCH_FAULT_BEFORE_UPLOAD` — precisely so the watch loop's crash-safety
premise (design §5.3) could be verified end to end. No harness stage exercised
it (it fell through the M3→M4→M6 seams). This gate closes that: it drives a
real `alf mcp serve` watch loop into the fault at the pre-upload point, lets
the process die (cooperative exit 137), restarts a CLEAN server, and asserts
the restart's catch-up scan produces EXACTLY ONE correct delta — no loss, no
duplicate.

TIER: live (`--backend real`). Like every prior live tier (M1 sync, M2b/M3
backend gates, M4 hermes-mcp proxy/real, M5's three confirmations) it needs a
minted runtime key + the test backend, so it is scheduled once and its artifact
kept — it is NOT a CI gate (the Neon test branch auto-expires). Runbook: the
WP-M6 handoff §"pre-upload abort catch-up gate" (formerly "kill-9").

Why host-side (no Docker): the generic toy runtime needs no framework install —
just a workspace + `.alf-map.json`. The watch loop runs on the host FS via
`notify`; the backend is the test service via the minted key. So the whole gate
is two `alf mcp serve` subprocesses over a temp workspace.

VERIFIED FACTS this script rests on (source-checked 2026-07-08):
  * The loop is spawned on process boot when an API key is set, concurrently
    with the protocol handler (`mcp/mod.rs:1092`) — it does NOT wait for an MCP
    `initialize`. Keep stdin OPEN (an unclosed pipe) and the server stays up and
    the loop runs; close stdin and it shuts down (design §5).
  * The delta interval floor is 60 s (`engine::DELTA_FLOOR`); the catch-up scan
    marks everything dirty on start (`mod.rs:404`) and syncs on the first due
    tick (~one interval later). Hence the ~interval+margin waits below.
  * The fault fires BEFORE `persist_local` and before the upload on every path
    (`sync.rs:842,940,1132`), so a killed sync leaves base+state at the old
    sequence and the restart re-derives the identical delta.

FINALIZE AT FIRST LIVE-RUN (named so they are not silent assumptions):
  * `--variant`: the mint variant that provisions a usable tenant/key. The
    runtime key is tenant-scoped and runtime-agnostic (the agent's
    `source_runtime` comes from the first sync's registration, F10), so any
    known-good variant works; default is chosen for that reason, override if the
    provisioner names it differently.
  * `--interval` / `--settle`: the 60 s floor plus notify/quiesce latency; bump
    `--settle` if the first-tick sync is slower than the default on the runner.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

LIFECYCLE_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(LIFECYCLE_DIR))

from alflab import provision  # noqa: E402
from alflab.backend import ApiClient  # noqa: E402
from alflab.redact import redact  # noqa: E402

# The agent NAME (derived from `framework` for the generic adapter) must be
# UNIQUE PER TENANT. A fixed name collides (HTTP 409 registration_failed) whenever
# a prior run leaked an agent — e.g. a run that failed after registering but before
# teardown. A fresh per-run suffix makes every run independent, so a leaked orphan
# can never block a re-run.
MAP = {
    "version": 1,
    "framework": "preupload-gate",  # made unique per run in _seed_workspace()
    "memory_sources": [
        {"id": "journal", "glob": "journal/*.md", "memory_type": "episodic",
         "namespace": "daily", "chunking": "by_heading", "timestamp": "file_mtime"}
    ],
    # Floor is 60 s; the gate waits one interval + settle margin per phase.
    "watch": {"default_interval": "60s"},
}


def _log(msg: str) -> None:
    print(redact(msg), file=sys.stderr, flush=True)


def _seed_workspace(ws: Path) -> None:
    (ws / "journal").mkdir(parents=True, exist_ok=True)
    # Unique agent name per run so a leaked orphan can never 409-block a re-run.
    run_map = dict(MAP, framework=f"preupload-gate-{uuid.uuid4().hex[:8]}")
    (ws / ".alf-map.json").write_text(json.dumps(run_map, indent=2), encoding="utf-8")
    (ws / "journal" / "day.md").write_text(
        "## Section A\n\nfirst entry, present at the base snapshot.\n",
        encoding="utf-8")


def _alf_env(alf_home: Path, creds, extra: dict | None = None) -> dict:
    env = dict(os.environ)
    env.update({
        "ALF_API_KEY": creds.runtime_api_key,
        "ALF_API_URL": creds.alf_api_url,
        "ALF_HOME": str(alf_home),
        "HOME": str(alf_home.parent),
    })
    env.pop("ALF_HUMAN", None)  # JSON-first stdout
    env.update(extra or {})
    return env


def _cli_sync(alf_bin: Path, ws: Path, env: dict) -> dict:
    proc = subprocess.run([str(alf_bin), "sync", "-r", "generic", "-w", str(ws)],
                          capture_output=True, text=True, timeout=300, env=env)
    out = (proc.stdout or "").strip()
    try:
        return json.loads(out) if out else {"ok": False, "raw": proc.stderr}
    except json.JSONDecodeError:
        return {"ok": False, "raw": out or proc.stderr}


def _serve(alf_bin: Path, ws: Path, env: dict) -> subprocess.Popen:
    """Start `alf mcp serve` with stdin held OPEN so the loop keeps running."""
    return subprocess.Popen(
        [str(alf_bin), "mcp", "serve", "-r", "generic", "-w", str(ws)],
        stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE, env=env)


def _latest_sequence(api: ApiClient, agent_id: str) -> int | None:
    r = api.get(f"/agents/{agent_id}")
    if r.status_code != 200:
        return None
    return r.json().get("latest_sequence")


def run_gate(args) -> int:
    run_dir = Path(tempfile.mkdtemp(prefix="alf-preupload-"))
    home = run_dir / "home"
    alf_home = home / ".alf"
    ws = home / "ws"
    alf_home.mkdir(parents=True)
    ws.mkdir(parents=True)
    checks: list[tuple[str, bool, str]] = []

    def check(name: str, ok: bool, detail: str = "") -> None:
        checks.append((name, ok, detail))
        _log(f"  {'PASS' if ok else 'FAIL'} — {name}{(' — ' + detail) if detail else ''}")

    creds = None
    api = None
    agent_id = ""
    try:
        _log("minting a runtime key (test backend)…")
        creds = provision.mint(Path(args.service_repo), args.variant, run_dir)
        _seed_workspace(ws)
        env = _alf_env(alf_home, creds)

        # (0) First sync (CLI, clean build): register + establish the base at S0.
        first = _cli_sync(args.alf_bin, ws, env)
        # The sync JSON nests the id under `agent.alf_agent_id` (SyncAgentRef);
        # the top-level `agent_id`/`alf_agent_id` keys do not exist. A fresh first
        # sync legitimately returns `sequence: 0` (the base; the catch-up delta is 1).
        agent_id = (first.get("agent") or {}).get("alf_agent_id") \
            or first.get("agent_id") or first.get("alf_agent_id") or ""
        s0 = first.get("sequence")
        check("first sync registered the agent + base snapshot",
              bool(first.get("ok")) and bool(agent_id) and s0 is not None,
              f"agent={agent_id} seq={s0}")
        if not agent_id or s0 is None:
            raise RuntimeError(f"first sync did not establish a base: {first}")

        api = ApiClient(creds.alf_api_url, creds.runtime_api_key)
        api.resolve_base(creds.seed_agent_id)
        check("⊙ backend sequence == S0 after first sync",
              _latest_sequence(api, agent_id) == s0, f"S0={s0}")

        # (1) Mutate a mapped source → one pending delta (Section B is new).
        (ws / "journal" / "day.md").write_text(
            "## Section A\n\nfirst entry, present at the base snapshot.\n\n"
            "## Section B\n\nadded after the base — the delta the loop must carry.\n",
            encoding="utf-8")

        # (2) Fault build: the loop reaches the pre-upload seam and takes a
        #     COOPERATIVE exit(137) — the code mimics 128+SIGKILL, but no kernel
        #     signal is delivered (see the module docstring).
        fault_env = _alf_env(alf_home, creds, {"ALF_WATCH_FAULT_BEFORE_UPLOAD": "1"})
        _log("serving FAULT build (cooperative exit(137) at the pre-upload seam "
             "on the first due tick)…")
        p = _serve(args.alf_fault_bin, ws, fault_env)
        try:
            rc = p.wait(timeout=args.interval + args.settle)
        except subprocess.TimeoutExpired:
            p.kill()
            rc = p.wait(timeout=30)
        check("watch loop aborted at the pre-upload seam (cooperative exit 137, "
              "not a kernel SIGKILL)", rc == 137, f"exit={rc}")
        check("⊙ backend sequence UNCHANGED after the crash (no partial upload)",
              _latest_sequence(api, agent_id) == s0, f"still S0={s0}")

        # (3) Clean restart: catch-up on start → exactly one delta → S0+1.
        _log("restarting CLEAN build (catch-up scan must sync the pending delta)…")
        clean_env = _alf_env(alf_home, creds)
        p2 = _serve(args.alf_bin, ws, clean_env)
        try:
            time.sleep(args.interval + args.settle)
            seq_after = _latest_sequence(api, agent_id)
            check("⊙ exactly one catch-up delta after restart (S0+1)",
                  seq_after == (s0 + 1), f"seq={seq_after} expected={s0 + 1}")

            # (4) No duplicate: a further idle interval advances nothing.
            _log("idling one more interval to prove no duplicate delta…")
            time.sleep(args.interval + args.settle)
            seq_idle = _latest_sequence(api, agent_id)
            check("⊙ no duplicate delta on the next idle interval",
                  seq_idle == seq_after, f"seq={seq_idle}")
        finally:
            if p2.stdin:
                p2.stdin.close()  # EOF → the server shuts down (design §5)
            try:
                p2.wait(timeout=30)
            except subprocess.TimeoutExpired:
                p2.kill()
    finally:
        # Teardown: purge the cloud agent (no confirmation gate — §7.W8) + temp.
        if creds and agent_id and not args.keep:
            try:
                subprocess.run(
                    [str(args.alf_bin), "purge", "-r", "generic", "-w", str(ws),
                     "--agent", agent_id],
                    capture_output=True, text=True, timeout=120,
                    env=_alf_env(alf_home, creds))
            except Exception as exc:  # noqa: BLE001 — teardown best-effort
                _log(f"purge failed (scavenge later): {exc}")
        if args.keep:
            _log(f"--keep: run dir retained at {run_dir}")
        else:
            shutil.rmtree(run_dir, ignore_errors=True)

    passed = sum(1 for _, ok, _ in checks if ok)
    verdict = {"gate": "preupload-abort-catchup", "passed": passed, "total": len(checks),
               "ok": passed == len(checks) and len(checks) == 6}
    print(json.dumps(verdict))
    return 0 if verdict["ok"] else 1


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--service-repo", required=True,
                    help="sibling agent-life-service checkout (mint + teardown)")
    ap.add_argument("--alf-bin", type=Path, default=Path("target/release/alf"),
                    help="the clean (default-feature) alf binary")
    ap.add_argument("--alf-fault-bin", type=Path,
                    default=Path("target/faultbuild/release/alf"),
                    help="alf built with `--features fault-injection` "
                         "(cargo build --release --features fault-injection "
                         "--target-dir target/faultbuild)")
    ap.add_argument("--variant", default="openclaw",
                    help="mint variant (tenant/key is runtime-agnostic; see header)")
    ap.add_argument("--interval", type=int, default=60,
                    help="delta interval seconds (floor 60)")
    ap.add_argument("--settle", type=int, default=45,
                    help="extra seconds past one interval for notify/quiesce+upload")
    ap.add_argument("--keep", action="store_true", help="retain the run dir")
    args = ap.parse_args()
    for b in (args.alf_bin, args.alf_fault_bin):
        if not Path(b).is_file():
            _log(f"missing binary: {b} (build it — see the WP-M6 handoff runbook)")
            return 2
    return run_gate(args)


if __name__ == "__main__":
    raise SystemExit(main())
