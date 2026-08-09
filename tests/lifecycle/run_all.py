#!/usr/bin/env python3
"""Run every lifecycle tier in sequence and print ONE summary for the whole run.

The driver runs a single (framework × tier); a release needs several, and until
now that meant running them by hand and eyeballing each report. This runs the
set, keeps going after a failure (so one broken tier does not hide the rest),
and ends with a table plus a machine-readable verdict line.

    python3 tests/lifecycle/run_all.py                      # offline set (zero secrets)
    python3 tests/lifecycle/run_all.py --set live --yes-live # the proxy/real gates
    python3 tests/lifecycle/run_all.py --set all --yes-live
    python3 tests/lifecycle/run_all.py --list          # show the set, run nothing
    python3 tests/lifecycle/run_all.py --json out.json # also write the summary

The live tiers mint a real runtime key, create real cloud agents and drive a
real LLM, so they require an explicit `--yes-live`. Without it they are refused,
not silently skipped: a machine that HAS credentials would otherwise start
billing work from a flag that reads like a filter. (Learned the hard way — a
`--set live` typed to check the skip path started three live tiers.)

Exit code: 0 only if every tier that RAN passed. A tier whose prerequisites are
missing is SKIPPED (reported, not fatal) unless --strict, which makes a missing
prerequisite a failure — the flag a release run should use.

Stdlib only, like the rest of the harness.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
DRIVER = HERE / "driver.py"

# The `<!-- LIFECYCLE ... -->` line the driver prints last: the run's own
# machine-readable verdict. Parsed rather than re-derived so this script can
# never disagree with the report it summarizes.
_MARKER = re.compile(r"<!--\s*LIFECYCLE\s+(?P<body>[^>]*?)-->")


@dataclass
class Tier:
    """One driver invocation."""

    name: str
    args: list[str]
    needs_live: bool = False
    note: str = ""


@dataclass
class Outcome:
    tier: str
    status: str  # PASS | FAIL | SKIP
    detail: str = ""
    duration_s: float = 0.0
    fields: dict = field(default_factory=dict)
    run_dir: str | None = None


# --- the sets -------------------------------------------------------------
# Offline: zero secrets, no backend — what CI can run and what a working tree
# should always pass. Live: the proxy/real gates that mint a runtime key.
OFFLINE: list[Tier] = [
    Tier("zeroclaw", ["--framework", "zeroclaw", "--llm", "none", "--backend",
                      "none", "--ci", "--stages", "Z1-Z3,Z13"],
         note="real zeroclaw install, discovery + Z13' determinism"),
    Tier("generic", ["--framework", "generic", "--llm", "none", "--backend",
                     "none", "--ci", "--stages", "Z1-Z3,Z13"],
         note="map-driven toy runtime"),
    Tier("generic-mcp", ["--framework", "generic-mcp", "--llm", "none", "--backend",
                         "none", "--ci", "--stages", "Z1-Z3,Z13"],
         note="every alf op over an `alf mcp serve` stdio session"),
]

LIVE: list[Tier] = [
    Tier("zeroclaw-live", ["--framework", "zeroclaw", "--llm", "proxy", "--backend",
                           "real", "--no-pause"],
         needs_live=True, note="real LLM turns + the ⊙ API/S3/Neon lanes"),
    Tier("hermes-mcp-live", ["--framework", "hermes-mcp", "--llm", "proxy", "--backend",
                             "real", "--no-pause", "--stages", "Z1-Z3,Z15,Z16"],
         needs_live=True,
         note="MCP HOST gate: an agent drives sync/vault by tool call, + watch autosync"),
    Tier("hermes-mcp-multi", ["--framework", "hermes-mcp", "--llm", "proxy", "--backend",
                              "real", "--no-pause", "--stages", "Z1-Z12,Z17"],
         needs_live=True, note="two watch loops, per-agent isolation + vault round-trip"),
]


def live_ready() -> tuple[bool, str]:
    """The live tiers mint a per-run runtime key from the sibling service
    checkout; without it they cannot run at all."""
    service = Path(os.environ.get("ALF_SERVICE_REPO", REPO.parent / "agent-life-service"))
    if not (service / "scripts" / "provision-test-runtime.sh").is_file():
        return False, f"no provisioner at {service}/scripts/provision-test-runtime.sh"
    if not (service / ".env").is_file():
        return False, f"no {service}/.env"
    try:
        import requests  # noqa: F401
    except ImportError:
        return False, "python3 cannot import requests (tests/lifecycle/requirements.txt)"
    return True, ""


def docker_ready() -> tuple[bool, str]:
    try:
        p = subprocess.run(["docker", "info"], capture_output=True, timeout=30)
        return (p.returncode == 0, "docker daemon not responding")
    except (OSError, subprocess.SubprocessError):
        return False, "docker not found"


def parse_marker(stdout: str) -> dict:
    """The driver's own verdict line → a dict (passed=4/4, xfail=0, …)."""
    m = None
    for m in _MARKER.finditer(stdout):
        pass  # last one wins
    if not m:
        return {}
    out = {}
    for token in m.group("body").split():
        if "=" in token:
            k, v = token.split("=", 1)
            out[k] = v
    return out


def run_tier(tier: Tier, extra: list[str]) -> Outcome:
    started = time.monotonic()
    cmd = [sys.executable, str(DRIVER), *tier.args, *extra]
    print(f"\n\033[1m━━━ {tier.name} ━━━\033[0m", flush=True)
    print(f"  {' '.join(cmd[1:])}", flush=True)
    proc = subprocess.run(cmd, cwd=REPO, capture_output=True, text=True)
    dur = time.monotonic() - started
    combined = proc.stdout + proc.stderr
    # Stream the tail so a failure is diagnosable without opening the run dir.
    if proc.returncode != 0:
        print("".join(combined.splitlines(keepends=True)[-40:]), flush=True)
    fields = parse_marker(combined)
    status = "PASS" if proc.returncode == 0 else "FAIL"
    detail = fields.get("passed", "")
    if fields.get("xfail", "0") not in ("0", ""):
        detail += f" xfail={fields['xfail']}"
    if fields.get("skipped", "0") not in ("0", ""):
        detail += f" skipped={fields['skipped']}"
    if not fields:
        detail = f"exit {proc.returncode} (no verdict line — the driver died early)"
    run_dir = None
    for line in combined.splitlines():
        if "report:" in line and "/runs/" in line:
            run_dir = line.split("report:", 1)[1].strip()
    print(f"  {'✔' if status == 'PASS' else '✘'} {tier.name}: {detail} ({dur:.0f}s)",
          flush=True)
    return Outcome(tier.name, status, detail.strip(), dur, fields, run_dir)


def summarize(outcomes: list[Outcome], release_evidence: bool | None = None) -> int:
    print("\n\033[1m═══════ lifecycle summary ═══════\033[0m")
    width = max((len(o.tier) for o in outcomes), default=10)
    for o in outcomes:
        mark = {"PASS": "\033[32m✔", "FAIL": "\033[31m✘", "SKIP": "\033[33m⊘"}[o.status]
        print(f"  {mark} {o.tier:<{width}}\033[0m  {o.detail}")
    ran = [o for o in outcomes if o.status != "SKIP"]
    failed = [o for o in outcomes if o.status == "FAIL"]
    skipped = [o for o in outcomes if o.status == "SKIP"]
    total_s = sum(o.duration_s for o in outcomes)
    print()
    # One machine-readable line for the whole run — the same posture as the
    # driver's per-run marker, so a wrapper (or a human grepping) has one thing
    # to read. `isolation` is the weakest link across the tiers that ran.
    isolation = "clean"
    for o in ran:
        if o.fields.get("isolation") not in (None, "clean"):
            isolation = o.fields["isolation"]
    ev = ""
    if release_evidence is not None:
        ev = f" release_evidence={'true' if release_evidence else 'false'}"
    print(f"<!-- LIFECYCLE-RUN tiers={len(outcomes)} passed={len(ran) - len(failed)}/{len(ran)}"
          f" failed={len(failed)} skipped={len(skipped)} isolation={isolation}"
          f" duration={total_s:.0f}s{ev} -->")
    if release_evidence is False:
        print("\033[33m⚠ NON-RELEASE EVIDENCE\033[0m — this run is not reproducible "
              "from a clean candidate (see provenance in --json summary)")
    if failed:
        print(f"\033[31m{len(failed)} tier(s) failed\033[0m: "
              f"{', '.join(o.tier for o in failed)}")
        return 1
    if not ran:
        print("\033[33mno tiers ran\033[0m")
        return 1
    print("\033[32mall lifecycle tiers passed\033[0m")
    return 0


def _alf_bin_from_extra(extra: list) -> str | None:
    """Honor a passed-through `--alf-bin PATH` / `--alf-bin=PATH` so the combined
    run stamps the same binary the driver will actually exercise."""
    for i, a in enumerate(extra):
        if a == "--alf-bin" and i + 1 < len(extra):
            return extra[i + 1]
        if a.startswith("--alf-bin="):
            return a.split("=", 1)[1]
    return None


def main(argv=None) -> int:
    p = argparse.ArgumentParser(prog="run_all.py", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--set", choices=["offline", "live", "all"], default="offline",
                   help="which tiers to run (default: offline — zero secrets)")
    p.add_argument("--yes-live", action="store_true",
                   help="required to run any live tier: they mint a runtime key, "
                        "create cloud agents and drive a real LLM")
    p.add_argument("--strict", action="store_true",
                   help="a missing prerequisite is a FAILURE, not a skip; also a "
                        "hard release gate — refuses a dirty tree / unknown commit / "
                        "missing binary digest before any tier runs (RF-024)")
    p.add_argument("--allow-dirty", action="store_true",
                   help="with --strict, downgrade the release gate to a labelled "
                        "non-release run for local development instead of refusing")
    p.add_argument("--list", action="store_true", help="print the set and exit")
    p.add_argument("--json", type=Path, default=None, metavar="PATH",
                   help="also write the summary as JSON")
    p.add_argument("--only", default=None, metavar="NAME",
                   help="run just the named tier from the set")
    p.add_argument("extra", nargs="*",
                   help="extra args passed through to every driver invocation")
    args = p.parse_args(argv)

    tiers = {"offline": OFFLINE, "live": LIVE, "all": OFFLINE + LIVE}[args.set]
    if args.only:
        tiers = [t for t in tiers if t.name == args.only]
        if not tiers:
            print(f"no tier named {args.only!r} in set {args.set!r}", file=sys.stderr)
            return 2

    if args.list:
        for t in tiers:
            print(f"{t.name:<20} {'[live]' if t.needs_live else '[offline]':<10} {t.note}")
        return 0

    # RF-024: one provenance capture for the whole combined run — every tier in
    # this invocation runs the same binary + checkout, so one capture is correct.
    from alflab.provenance import capture
    from alflab.runner import find_alf_binary
    try:
        binary = find_alf_binary(_alf_bin_from_extra(args.extra))
    except SystemExit:
        binary = None   # no binary resolvable ⇒ missing digest ⇒ non-release
    prov_backend = "real" if any(t.needs_live for t in tiers) else "none"
    service_repo = Path(os.environ.get(
        "ALF_SERVICE_REPO", REPO.parent / "agent-life-service"))
    prov = capture(REPO, binary, prov_backend, service_repo)

    # Reject-or-label gate: strict is a hard release gate. A dirty tree, an
    # unknown commit, or a missing binary digest cannot be release evidence, so
    # refuse BEFORE any tier runs — unless --allow-dirty downgrades it to a
    # labelled non-release run for local development.
    if args.strict and not args.allow_dirty:
        problems = []
        if prov.adapters_dirty:
            problems.append("adapters working tree is dirty (source_commit not clean)")
        if prov.adapters_commit in ("", "unknown"):
            problems.append("adapters commit is unknown (git unavailable)")
        if not prov.binary_sha256:
            problems.append("binary digest missing (no alf binary resolved)")
        if problems:
            print("refusing strict release run — not reproducible evidence:",
                  file=sys.stderr)
            for pr in problems:
                print(f"  - {pr}", file=sys.stderr)
            print("  release runs come from a clean checkout of the candidate; for a "
                  "labelled local run add --allow-dirty.", file=sys.stderr)
            return 2

    docker_ok, docker_why = docker_ready()
    live_ok, live_why = live_ready()

    # Refuse live work that was not explicitly asked for — before anything runs,
    # so a mistyped set cannot mint a key on a credentialed machine.
    if any(t.needs_live for t in tiers) and not args.yes_live:
        live_names = ", ".join(t.name for t in tiers if t.needs_live)
        print(f"refusing to run live tiers without --yes-live: {live_names}\n"
              f"  they mint a runtime key, create cloud agents and drive a real LLM.\n"
              f"  Re-run with --yes-live, or use --set offline.", file=sys.stderr)
        return 2

    outcomes: list[Outcome] = []
    for tier in tiers:
        if not docker_ok:
            outcomes.append(Outcome(tier.name, "FAIL" if args.strict else "SKIP",
                                    docker_why))
            continue
        if tier.needs_live and not live_ok:
            outcomes.append(Outcome(tier.name, "FAIL" if args.strict else "SKIP",
                                    live_why))
            continue
        outcomes.append(run_tier(tier, args.extra))

    code = summarize(outcomes, release_evidence=prov.release_evidence)
    if args.json:
        from alflab.redact import redact_obj
        payload = redact_obj({
            "tiers": [o.__dict__ for o in outcomes],
            "exit_code": code,
            "provenance": asdict(prov),
            "release_evidence": prov.release_evidence,
        })
        args.json.write_text(json.dumps(payload, indent=2), encoding="utf-8")
        print(f"summary written to {args.json}")
    return code


if __name__ == "__main__":
    raise SystemExit(main())
