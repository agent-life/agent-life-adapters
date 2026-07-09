#!/usr/bin/env python3
"""Reconstruct events.ndjson for a finished run that predates the emitter.

Usage:
  python3 tests/lifecycle/viz/backfill.py <run-dir>

Sources: report.json (authoritative stage/check tree) + driver.log (flow /
show_data lines) + mcp-interactions.log + z16-serve-stderr.log. Also copies
viz/index.html → visualization.html when missing.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

VIZ_DIR = Path(__file__).resolve().parent
TEMPLATE = VIZ_DIR / "index.html"

STAGE_RE = re.compile(r"^──\s+(Z\d+|OPS):\s+(.*?)\s*──\s*$")
FLOW_RE = re.compile(r"^\s*data flow:\s+(.*)$")
PLACEMENT_RE = re.compile(r"^\s*placement:\s*$")
KV_RE = re.compile(r"^\s+(\w+):\s+(.*)$")


def parse_started_at(report: dict) -> datetime:
    raw = report.get("started_at") or ""
    try:
        return datetime.strptime(raw, "%Y-%m-%d %H:%M:%S UTC").replace(tzinfo=timezone.utc)
    except ValueError:
        return datetime.now(timezone.utc)


def iso(ts: datetime) -> str:
    return ts.strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"


def emit(events: list, seq: list, t: datetime, kind: str, **payload):
    seq[0] += 1
    events.append({"seq": seq[0], "t": iso(t), "kind": kind, **payload})


def infer_state_for_stage(stage_id: str, stage: dict, events: list, seq: list, t: datetime):
    """Synthesize the boundary-crossing state events the live emitter would have."""
    sid = stage_id
    status = stage.get("status")
    if sid == "z02" and status == "PASS":
        # Reconstruct per-turn LLM hops when the live emitter wasn't present.
        # Prefer check names/details; fall back to scenario prompts for text.
        try:
            sys.path.insert(0, str(VIZ_DIR.parent))  # tests/lifecycle
            from alflab import scenario as _scenario
            scen_turns = _scenario.turns("default", round=1)
        except Exception:  # noqa: BLE001
            scen_turns = []
        turn_checks = []
        for c in stage.get("checks") or []:
            name = c.get("name") or ""
            m = re.search(
                r"turn\s+(semantic|episodic|procedural|secret)\s+\(([^)]+)\)",
                name, re.I)
            if m:
                turn_checks.append({
                    "type": m.group(1).lower(),
                    "marker": m.group(2),
                    "ok": (c.get("status") or "").upper() == "PASS",
                    "response": (c.get("detail") or "")[:240],
                })
        markers = [tc["marker"] for tc in turn_checks]
        if not markers:
            for c in stage.get("checks") or []:
                m = re.search(r"(ATLAS-[A-Z0-9-]+|sk-atlas-[A-Za-z0-9-]+)",
                              c.get("name", ""))
                if m:
                    markers.append(m.group(1))
        n_turns = max(len(turn_checks), len(scen_turns), 1)
        for i in range(len(turn_checks) or len(scen_turns)):
            tc = turn_checks[i] if i < len(turn_checks) else {}
            st = scen_turns[i] if i < len(scen_turns) else None
            typ = tc.get("type") or (st.turn_type if st else "semantic")
            marker = tc.get("marker") or (st.marker if st else f"turn-{i+1}")
            prompt = (st.prompt[:220] if st else f"[{typ}] remember {marker}")
            reply = tc.get("response") or f"stored {marker}"
            emit(events, seq, t, "state", stage_id=sid, subsystem="agentA",
                 patch={"activity": f"LLM turn {i+1}/{n_turns} · {typ} →",
                        "last_turn": {"n": i+1, "of": n_turns, "type": typ,
                                     "marker": marker, "phase": "prompt",
                                     "prompt": prompt},
                        "packet": "llm-prompt"})
            emit(events, seq, t, "data", stage_id=sid, label="llm prompt",
                 data={"turn": i+1, "of": n_turns, "type": typ,
                       "marker": marker, "prompt": prompt})
            emit(events, seq, t, "state", stage_id=sid, subsystem="service",
                 patch={"llm_proxy": True,
                        "activity": f"proxy · {typ} turn",
                        "packet": "llm-prompt",
                        "llm": {"phase": "prompt", "turn": i+1, "of": n_turns,
                                "type": typ, "marker": marker,
                                "prompt": prompt, "response": None}})
            t = t + timedelta(milliseconds=40)
            emit(events, seq, t, "state", stage_id=sid, subsystem="service",
                 patch={"llm_proxy": True,
                        "activity": f"proxy replied · {typ}",
                        "packet": "llm-response",
                        "llm": {"phase": "response", "turn": i+1, "of": n_turns,
                                "type": typ, "marker": marker,
                                "prompt": prompt, "response": reply}})
            emit(events, seq, t, "state", stage_id=sid, subsystem="agentA",
                 patch={"activity": f"stored {typ} · {marker}",
                        "markers": markers[: i + 1],
                        "records": i + 1,
                        "last_turn": {"n": i+1, "of": n_turns, "type": typ,
                                     "marker": marker, "phase": "response",
                                     "prompt": prompt, "response": reply,
                                     "ok": tc.get("ok", True)},
                        "packet": "llm-response",
                        "mutations": [
                            {"path": "memories/MEMORY.md", "op": typ[:4],
                             "note": f"{typ} · {marker}"},
                            {"path": "state.db", "op": "session",
                             "note": f"session row · {marker}"},
                        ]})
            emit(events, seq, t, "data", stage_id=sid, label="llm response",
                 data={"turn": i+1, "of": n_turns, "type": typ,
                       "marker": marker, "ok": tc.get("ok", True),
                       "response": reply})
            t = t + timedelta(milliseconds=40)
        emit(events, seq, t, "state", stage_id=sid, subsystem="agentA",
             patch={"born": True, "slot": "default", "markers": markers[:4],
                    "records": max(4, len(markers)),
                    "activity": f"coverage done · {len(markers[:4])} markers",
                    "mutations": [
                        {"path": "memories/MEMORY.md", "op": "seed",
                         "note": f"{len(markers[:4])} markers"},
                        {"path": "state.db", "op": "seed", "note": "session rows"},
                    ]})
    elif sid == "z01" and status == "PASS":
        emit(events, seq, t, "state", stage_id=sid, subsystem="agentA",
             patch={"born": True, "slot": "default",
                    "activity": "install probe done"})
    elif sid == "z04" and status == "PASS":
        emit(events, seq, t, "state", stage_id=sid, subsystem="agentA",
             patch={"synced": True, "seq": 0, "activity": "synced · seq 0",
                    "mutations": []})
        emit(events, seq, t, "state", stage_id=sid, subsystem="service",
             patch={"registered": True, "latest_sequence": 0, "snapshot": True,
                    "deltas": 0, "packet": "snapshot",
                    "activity": "agent A registered · seq 0",
                    "agents": {"A": {"registered": True, "seq": 0,
                                    "snapshot": True, "deltas": 0}}})
        emit(events, seq, t, "state", stage_id=sid, subsystem="mcp",
             patch={"role": "cli-sync", "last": "snapshot"})
    elif sid == "z06" and status == "PASS":
        emit(events, seq, t, "state", stage_id=sid, subsystem="agentA",
             patch={"vault": ["vault-z6"],
                    "activity": "vault-z6 encrypted under per-agent key",
                    "mutations": [
                        {"path": "state/<id>/.alf-vault-key", "op": "keygen",
                         "note": "0600 local key"},
                        {"path": "credentials.json (vault)", "op": "add",
                         "note": "vault-z6 ciphertext"},
                    ],
                    "workspace": [
                        {"path": "config.yaml", "kind": "file"},
                        {"path": "SOUL.md", "kind": "file"},
                        {"path": "memories/MEMORY.md", "kind": "file"},
                        {"path": "state.db", "kind": "db"},
                        {"path": "state/<id>/.alf-vault-key", "kind": "file"},
                        {"path": "credentials.json (vault)", "kind": "file"},
                        {"path": "skills/", "kind": "dir"},
                    ]})
    elif sid == "z07" and status == "PASS":
        seq_n = 2
        for c in stage.get("checks") or []:
            m = re.search(r"sequence=(\d+)|seq=(\d+)", c.get("detail") or "")
            if m:
                seq_n = int(m.group(1) or m.group(2))
        emit(events, seq, t, "state", stage_id=sid, subsystem="agentA",
             patch={"seq": seq_n, "vault_synced": True,
                    "activity": f"delta synced · seq {seq_n}",
                    "mutations": [
                        {"path": "state/<id>/.alf-vault-key", "op": "vault",
                         "note": "Layer-4 ciphertext"},
                    ]})
        emit(events, seq, t, "state", stage_id=sid, subsystem="service",
             patch={"latest_sequence": seq_n, "deltas": 2, "packet": "delta",
                    "activity": f"agent A · seq {seq_n}",
                    "agents": {"A": {"seq": seq_n, "deltas": 2, "snapshot": True}}})
        emit(events, seq, t, "state", stage_id=sid, subsystem="mcp",
             patch={"role": "cli-sync", "last": "delta"})
    elif sid == "z08" and status == "PASS":
        emit(events, seq, t, "state", stage_id=sid, subsystem="agentB",
             patch={"born": True, "slot": "agent_b", "records": 0,
                    "activity": "profile created — empty store",
                    "workspace": [
                        {"path": "profiles/agent_b/", "kind": "dir"},
                        {"path": "profiles/agent_b/config.yaml", "kind": "file"},
                    ],
                    "mutations": [
                        {"path": "profiles/agent_b/", "op": "create",
                         "note": "framework profile"},
                        {"path": "profiles/agent_b/config.yaml", "op": "write",
                         "note": "declared in config"},
                    ]})
        emit(events, seq, t, "state", stage_id=sid, subsystem="service",
             patch={"activity": "awaiting agent B first sync",
                    "agents": {"B": {"visible": True, "registered": False}}})
    elif sid == "z10" and status == "PASS":
        emit(events, seq, t, "state", stage_id=sid, subsystem="agentB",
             patch={"synced": True, "records": 4, "isolation": "clean",
                    "activity": "synced · isolation clean",
                    "workspace": [
                        {"path": "profiles/agent_b/", "kind": "dir"},
                        {"path": "profiles/agent_b/config.yaml", "kind": "file"},
                        {"path": "profiles/agent_b/memories/MEMORY.md", "kind": "file"},
                        {"path": "profiles/agent_b/state.db", "kind": "db"},
                    ],
                    "mutations": [
                        {"path": "profiles/agent_b/memories/MEMORY.md", "op": "seed",
                         "note": "4 markers"},
                        {"path": "profiles/agent_b/state.db", "op": "seed",
                         "note": "session rows"},
                    ]})
        emit(events, seq, t, "state", stage_id=sid, subsystem="service",
             patch={"agent_b_registered": True, "packet": "snapshot",
                    "activity": "agent B registered",
                    "agents": {"B": {"visible": True, "registered": True,
                                    "snapshot": True, "seq": 0, "deltas": 0}}})
    elif sid == "z15" and status == "PASS":
        emit(events, seq, t, "state", stage_id=sid, subsystem="mcp",
             patch={"role": "stdio-server", "active": True,
                    "tools": ["mcp_alf_alf_sync", "mcp_alf_alf_vault_add",
                              "mcp_alf_alf_vault_list"],
                    "last": "tool-driven-sync", "packet": "tool-call",
                    "activity": "mcp_alf_* tools invoked by agent"})
        emit(events, seq, t, "state", stage_id=sid, subsystem="service",
             patch={"packet": "mcp-sync", "activity": "MCP tool-driven sync landed",
                    "agents": {"A": {"snapshot": True}}})
        emit(events, seq, t, "state", stage_id=sid, subsystem="agentA",
             patch={"activity": "synced via mcp_alf_* (no terminal alf sync)",
                    "synced": True,
                    "mutations": [
                        {"path": "memories/MEMORY.md", "op": "sync",
                         "note": "tool-driven export"},
                        {"path": "state.db", "op": "sync",
                         "note": "tool-driven export"},
                    ]})
    elif sid == "z16" and status == "PASS":
        base, end, n_deltas = 4, 10, 6
        for c in stage.get("checks") or []:
            d = c.get("detail") or ""
            m = re.search(r"seq\s+(\d+)\s*→\s*(\d+)", d)
            if m:
                base, end = int(m.group(1)), int(m.group(2))
            m2 = re.search(r"(\d+)\s+deltas since seq", d)
            if m2:
                n_deltas = int(m2.group(1))
        # Per-tick mutations so replay shows the watch loop lighting rows.
        for i in range(min(n_deltas, 6)):
            emit(events, seq, t, "state", stage_id=sid, subsystem="agentA",
                 patch={"activity": f"watch mutate {i + 1}/{min(n_deltas, 6)}",
                        "mutations": [
                            {"path": "memories/MEMORY.md", "op": "append",
                             "note": f"watch tick {i}"},
                            {"path": "state.db", "op": "insert",
                             "note": f"z16_watch · tick {i}"},
                        ],
                        "packet": "watch-delta"})
            t = t + timedelta(milliseconds=40)
        emit(events, seq, t, "state", stage_id=sid, subsystem="mcp",
             patch={"role": "watch-loop", "active": True, "watch_sources": 8,
                    "packet": "watch-delta",
                    "activity": f"watch-sync · {n_deltas} deltas"})
        emit(events, seq, t, "state", stage_id=sid, subsystem="service",
             patch={"latest_sequence": end, "deltas_since": n_deltas,
                    "seq_from": base, "seq_to": end, "packet": "watch-delta",
                    "activity": f"agent A · seq {base}→{end}",
                    "agents": {"A": {"seq": end, "deltas": n_deltas, "snapshot": True}}})
        emit(events, seq, t, "state", stage_id=sid, subsystem="agentA",
             patch={"seq": end, "activity": f"watch done · seq {end}",
                    "mutations": [
                        {"path": "memories/MEMORY.md", "op": "watch",
                         "note": f"{n_deltas} entries"},
                        {"path": "state.db", "op": "watch",
                         "note": f"{n_deltas} rows"},
                    ]})


def flows_from_log(driver_log: Path) -> dict[str, list]:
    """Map stage_id → list of (flow|data, ...) tuples from driver.log."""
    if not driver_log.is_file():
        return {}
    out: dict[str, list] = {}
    current = None
    pending_placement = False
    placement: dict = {}
    for line in driver_log.read_text(encoding="utf-8", errors="replace").splitlines():
        m = STAGE_RE.match(line)
        if m:
            if pending_placement and placement and current:
                out.setdefault(current, []).append(("data", "placement", dict(placement)))
            label = m.group(1)
            current = label.lower() if label.startswith("Z") else None
            pending_placement = False
            placement = {}
            continue
        if not current:
            continue
        fm = FLOW_RE.match(line)
        if fm:
            out.setdefault(current, []).append(("flow", fm.group(1).strip()))
            continue
        if PLACEMENT_RE.match(line):
            if pending_placement and placement:
                out.setdefault(current, []).append(("data", "placement", dict(placement)))
            pending_placement = True
            placement = {}
            continue
        if pending_placement:
            km = KV_RE.match(line)
            if km:
                placement[km.group(1)] = km.group(2).strip()
            else:
                if placement:
                    out.setdefault(current, []).append(("data", "placement", dict(placement)))
                pending_placement = False
                placement = {}
    if pending_placement and placement and current:
        out.setdefault(current, []).append(("data", "placement", dict(placement)))
    return out


def backfill(run_dir: Path) -> Path:
    report_path = run_dir / "report.json"
    if not report_path.is_file():
        raise SystemExit(f"no report.json in {run_dir}")
    report = json.loads(report_path.read_text(encoding="utf-8"))
    t0 = parse_started_at(report)
    log_bits = flows_from_log(run_dir / "driver.log")

    events: list = []
    seq = [0]
    t = t0
    emit(events, seq, t, "run_start",
         framework=report.get("framework"),
         tier=report.get("tier"),
         stages_requested=report.get("stages_requested"),
         alf_version=report.get("alf_version"),
         run_dir=str(run_dir))

    for stage in report.get("stages") or []:
        sid = stage["stage_id"]
        title = stage.get("title") or sid
        dur = float(stage.get("duration_ms") or 100)
        stage_start = t + timedelta(milliseconds=30)
        t = stage_start
        emit(events, seq, t, "stage_start", stage_id=sid, title=title)

        extras = log_bits.get(sid, [])
        checks = stage.get("checks") or []
        n_slots = max(1, len(extras) + len(checks) + 2)
        slot = dur / n_slots

        for kind, *rest in extras:
            t = t + timedelta(milliseconds=slot)
            if kind == "flow":
                emit(events, seq, t, "flow", stage_id=sid, arrows=rest[0])
            elif kind == "data":
                emit(events, seq, t, "data", stage_id=sid, label=rest[0], data=rest[1])

        for chk in checks:
            t = t + timedelta(milliseconds=slot)
            emit(events, seq, t, "check", stage_id=sid,
                 name=chk.get("name", ""), status=chk.get("status", ""),
                 detail=chk.get("detail") or "")

        t = t + timedelta(milliseconds=slot)
        infer_state_for_stage(sid, stage, events, seq, t)

        if sid == "z15":
            mcp_log = run_dir / "mcp-interactions.log"
            if mcp_log.is_file():
                for line in mcp_log.read_text(encoding="utf-8", errors="replace").splitlines():
                    m = re.match(r"\s*\d+\.\s+(mcp_alf_\S+)\s+([\d.]+)s\s+(\w+)", line)
                    if m:
                        t = t + timedelta(milliseconds=15)
                        emit(events, seq, t, "data", stage_id=sid, label="mcp interaction",
                             data={"tool": m.group(1), "secs": m.group(2), "ok": m.group(3)})

        if sid == "z16":
            z16 = run_dir / "z16-serve-stderr.log"
            if z16.is_file():
                syncs = [
                    ln for ln in z16.read_text(encoding="utf-8", errors="replace").splitlines()
                    if "watch sync ok" in ln or "Uploading delta" in ln
                ]
                for ln in syncs[:12]:
                    t = t + timedelta(milliseconds=20)
                    emit(events, seq, t, "data", stage_id=sid, label="watch",
                         data={"line": ln.strip()[:160]})

        stage_end = stage_start + timedelta(milliseconds=dur)
        if t < stage_end:
            t = stage_end
        emit(events, seq, t, "stage_end", stage_id=sid,
             status=stage.get("status"), duration_ms=dur)

    counts = {"PASS": 0, "FAIL": 0, "SKIP": 0, "XFAIL": 0}
    for s in report.get("stages") or []:
        for c in s.get("checks") or []:
            st = c.get("status")
            if st in counts:
                counts[st] += 1
    t = t + timedelta(milliseconds=50)
    emit(events, seq, t, "run_end",
         passed=counts["PASS"], failed=counts["FAIL"], skipped=counts["SKIP"],
         xfail=counts["XFAIL"],
         coverage=report.get("coverage"), isolation=report.get("isolation"),
         teardown=report.get("teardown") or {},
         exit_code=report.get("exit_code", 0))

    out = run_dir / "events.ndjson"
    out.write_text(
        "".join(json.dumps(e, ensure_ascii=False) + "\n" for e in events),
        encoding="utf-8")

    viz_dst = run_dir / "visualization.html"
    if TEMPLATE.is_file():
        shutil.copyfile(TEMPLATE, viz_dst)

    print(f"wrote {out} ({len(events)} events)")
    if viz_dst.is_file():
        print(f"wrote {viz_dst}")
    return out


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("run_dir", type=Path)
    args = p.parse_args(argv)
    run_dir = args.run_dir.resolve()
    if not run_dir.is_dir():
        print(f"not a directory: {run_dir}", file=sys.stderr)
        return 2
    backfill(run_dir)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
