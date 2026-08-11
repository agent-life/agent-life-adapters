"""Run context + stage loop + ops choreography (plan §4) + exit contract.

Exit codes: 0 green (XFAILs allowed) · 1 FAIL · 2 preflight/infra ·
130 interactive abort. Every post-mint exit path — including SystemExit from
the P4/glibc probes and unexpected exceptions — runs finish() (teardown
ladder + container destroy + report); SIGINT/SIGTERM are converted to
exceptions so they take the same path; `--teardown <run-dir>` recovers after
a hard abort.
"""

from __future__ import annotations

import argparse
import os
import signal
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

from . import events, models, provenance, provision, stages, ui
from .config import HarnessConfig
from .contract import KitEnv, SkipStage
from .dockerctl import (
    DockerContainer, DockerError, build_image, host_sha256, seed_home_from_image, ALF_DIST,
)
from .events import EventLog
from .narrator import InteractiveAbort, NullNarrator, RichNarrator
from .provision import Manifest, RuntimeCreds
from .report import Check, RunReport, StageResult
from .viz_server import VizServer

LIFECYCLE_DIR = Path(__file__).resolve().parents[1]
REPO_ROOT = LIFECYCLE_DIR.parents[1]
VIZ_SRC = LIFECYCLE_DIR / "viz" / "index.html"
SENTINEL_API_URL = "http://127.0.0.1:9"   # backend=none: refuses instantly
DEFAULT_STAGES = "Z1-Z4,Z13"
DEFAULT_VIZ_PORT = 8765


def _install_viz(run_dir: Path) -> None:
    """Copy the committed viz into the run dir so relative fetches work."""
    if VIZ_SRC.is_file():
        (run_dir / "visualization.html").write_text(
            VIZ_SRC.read_text(encoding="utf-8"), encoding="utf-8")


def _start_viz_server(run: "Run") -> None:
    """Default-on local HTTP server for the live visualization."""
    if getattr(run.args, "no_viz_server", False):
        return
    port = int(getattr(run.args, "viz_port", DEFAULT_VIZ_PORT) or DEFAULT_VIZ_PORT)
    try:
        srv = VizServer(run.paths.run_dir, port=port)
        url = srv.start()
        run.viz_server = srv
        ui.section("OPS", "Lifecycle visualization (live)")
        ui.ok(f"viz server listening on 127.0.0.1:{srv.port}")
        ui.emit(f"  open:  {ui.c('cyan', url)}")
        ui.emit(f"  (serves {run.paths.run_dir} — events.ndjson updates as stages run)")
        ui.emit("")
    except Exception as e:  # noqa: BLE001 — viz is best-effort; never block the run
        from .redact import redact
        ui.warn(f"viz server failed to start ({type(e).__name__}: {redact(str(e))[:120]}) "
                "— continuing without it; open visualization.html manually later")


@dataclass
class RunPaths:
    run_dir: Path
    home: Path
    alf_home: Path
    manifest: Path
    run_env: Path
    driver_log: Path


class Preflight(SystemExit):
    def __init__(self, msg: str):
        ui.fail(f"preflight: {msg}")
        super().__init__(2)


class Run:
    """The shared context every stage sees (duck-typed by stages.py)."""

    def __init__(self):
        self.args = None
        self.cfg: HarnessConfig = None
        self.kit = None
        self.paths: RunPaths = None
        self.narrator = None
        self.creds: Optional[RuntimeCreds] = None
        self.api = None
        self.db = None
        self.s3 = None
        self.container: Optional[DockerContainer] = None
        self.alf = None                       # AlfInvoker (WP-M4): CLI or MCP
        self.manifest: Optional[Manifest] = None
        self.report: Optional[RunReport] = None
        self.llm = "none"
        self.backend = "none"
        self.framework_dir: Path = None
        self.alf_binary: Path = None
        self.expected_alf_version = ""
        self.sentinel_api_url = SENTINEL_API_URL
        self.state: dict = {}
        self.viz_server = None                 # VizServer | None


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------

def parse_stage_selection(spec: str) -> list[str]:
    """'Z1-Z4,Z13' → ['z01','z02','z03','z04','z13'] (registry order)."""
    wanted: set[int] = set()
    for token in spec.replace(" ", "").split(","):
        if not token:
            continue
        token = token.upper().lstrip("Z")
        if "-" in token:
            lo, hi = token.split("-", 1)
            wanted.update(range(int(lo), int(hi.upper().lstrip("Z")) + 1))
        else:
            wanted.add(int(token))
    return [sid for sid in stages.STAGE_IDS if int(sid[1:]) in wanted]


def _load_kit_module(framework: str):
    import importlib.util

    kit_path = LIFECYCLE_DIR / "frameworks" / framework / "kit.py"
    if not kit_path.is_file():
        raise Preflight(f"no kit for framework {framework!r} ({kit_path})")
    spec = importlib.util.spec_from_file_location(f"kit_{framework}", kit_path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def load_kit(framework: str, env: KitEnv):
    return _load_kit_module(framework).KIT_CLASS(env)


def kit_runtime_name(framework: str) -> str:
    """The alf RUNTIME a framework's kit drives — its `KIT_CLASS.name`. For the
    three base frameworks this equals the framework dir name, but a host variant
    like `hermes-mcp` drives the `hermes` runtime, so anything that keys on the
    runtime (the mint variant, backend/service ops) must use THIS, not
    `args.framework`. Read as a class attribute — no `KitEnv`, no instantiation."""
    return _load_kit_module(framework).KIT_CLASS.name


def expected_alf_version() -> str:
    text = (REPO_ROOT / "alf-cli" / "Cargo.toml").read_text(encoding="utf-8")
    for line in text.splitlines():
        if line.strip().startswith("version"):
            return "alf " + line.split('"')[1]
    return "alf ?"


def find_alf_binary(explicit: Optional[str]) -> Path:
    """--alf-bin wins; else prefer the musl build (bookworm images can't run a
    newer-glibc host build), else the glibc release build (the in-container
    glibc probe catches the loader mismatch with a remedy)."""
    if explicit:
        p = Path(explicit).expanduser().resolve()
        if not p.is_file():
            raise Preflight(f"--alf-bin {p} not found")
        return p
    for candidate in (
        REPO_ROOT / "target" / "x86_64-unknown-linux-musl" / "release" / "alf",
        REPO_ROOT / "target" / "release" / "alf",
    ):
        if candidate.is_file():
            return candidate
    raise Preflight(
        "no alf binary found — build one:\n"
        "  cargo build --release -p alf-cli   (glibc; may need the musl remedy)\n"
        "  cargo zigbuild --release --target x86_64-unknown-linux-musl -p alf-cli"
    )


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

def build_arg_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="driver.py",
        description="agent-life lifecycle harness — real-install Z1–Z13 driver (WP2)")
    p.add_argument("--framework", default="zeroclaw",
                   choices=["zeroclaw", "openclaw", "hermes", "generic", "hermes-mcp",
                            "generic-mcp"])
    p.add_argument("--llm", choices=["none", "proxy"], default="none")
    p.add_argument("--backend", choices=["none", "real"], default=None,
                   help="default: real locally, none under --ci/CI=true")
    mode = p.add_mutually_exclusive_group()
    mode.add_argument("--interactive", action="store_true",
                      help="pause + render at every stage (default on a TTY)")
    mode.add_argument("--no-pause", action="store_true")
    mode.add_argument("--ci", action="store_true",
                      help="compact output; hard-refuses --llm proxy/--backend real")
    p.add_argument("--stages", default=DEFAULT_STAGES,
                   help=f"e.g. Z1-Z4,Z13 (default) or Z1-Z3,Z13")
    p.add_argument("--full", action="store_true", help="all thirteen Z slots")
    p.add_argument("--model", default=None, help="LLM model alias/id for the mint")
    p.add_argument("--alf-bin", default=None, help="path to the alf-under-test")
    p.add_argument("--keep", action="store_true", help="keep the container running")
    p.add_argument("--keep-agent", action="store_true", help="skip cloud teardown")
    p.add_argument("--no-viz-server", action="store_true",
                   help="do not start the local visualization HTTP server "
                        f"(default: serve run dir on 127.0.0.1:{DEFAULT_VIZ_PORT})")
    p.add_argument("--viz-port", type=int, default=DEFAULT_VIZ_PORT, metavar="PORT",
                   help=f"port for the visualization server (default {DEFAULT_VIZ_PORT}; "
                        "falls back to an ephemeral port if busy)")
    p.add_argument("--teardown", metavar="RUN_DIR", default=None,
                   help="recover: run the teardown ladder for a previous run dir")
    p.add_argument("--leak-scan", action="store_true",
                   help="Neon lane: internal-tenant agents unaccounted for by runs/")
    return p


def main(argv=None) -> int:
    args = build_arg_parser().parse_args(argv)
    cfg = HarnessConfig.from_env()

    if args.teardown:
        return teardown_cli(Path(args.teardown), cfg, args.framework)
    if args.leak_scan:
        return leak_scan_cli(cfg)

    run = Run()
    run.args, run.cfg = args, cfg

    # -- tier resolution (D2) + CI refusal ------------------------------------
    in_ci = args.ci or os.environ.get("CI", "").lower() == "true"
    if in_ci and (args.llm == "proxy" or args.backend == "real"):
        raise Preflight("CI runs are no-LLM and no-backend by policy (D2) — "
                        "refusing --llm proxy / --backend real")
    run.llm = "none" if in_ci else args.llm
    run.backend = args.backend if args.backend is not None else ("none" if in_ci else "real")
    if in_ci:
        run.backend = "none"
    if run.llm == "proxy" and run.backend != "real":
        raise Preflight("--llm proxy requires --backend real (the mint provides the proxy)")

    interactive = args.interactive or (
        not args.no_pause and not args.ci and sys.stdin.isatty() and sys.stdout.isatty())

    # -- P0 preflight ----------------------------------------------------------
    if subprocess.run(["docker", "info"], capture_output=True).returncode != 0:
        raise Preflight("docker not available (daemon running?)")
    run.framework_dir = LIFECYCLE_DIR / "frameworks" / args.framework
    run.alf_binary = find_alf_binary(args.alf_bin)
    run.expected_alf_version = expected_alf_version()
    if run.backend == "real":
        # We mint via the `e2e` cargo bin directly (adapters/.env is the sole
        # config source), so require the crate, not the service shell wrapper.
        crate = cfg.service_repo / "tests" / "e2e" / "Cargo.toml"
        if not crate.is_file():
            raise Preflight(f"service e2e crate not found: {crate} (set ALF_SERVICE_REPO)")

    # -- run dir (fresh-HOME invariant is structural: E3's 409 guard) ----------
    ts = provision.utc_ts()
    run_dir = LIFECYCLE_DIR / "runs" / f"{ts}-{args.framework}"
    run_dir.mkdir(parents=True, exist_ok=True)
    os.chmod(run_dir, 0o700)
    home, alf_home = run_dir / "home", run_dir / "alf-home"
    if home.exists() and any(home.iterdir()):
        raise Preflight(f"run home {home} pre-exists non-empty — fresh HOME per run "
                        "is the structural invariant (E3 409 guard)")
    home.mkdir(exist_ok=True)
    alf_home.mkdir(exist_ok=True)
    run.paths = RunPaths(
        run_dir=run_dir, home=home, alf_home=alf_home,
        manifest=run_dir / "run-manifest.json", run_env=run_dir / "run.env",
        driver_log=run_dir / "driver.log",
    )
    ui.set_log_file(run.paths.driver_log)
    event_log = EventLog(run_dir / "events.ndjson")
    events.set_event_log(event_log)
    _install_viz(run_dir)

    stage_ids = stages.STAGE_IDS if args.full else parse_stage_selection(args.stages)
    # RF-024: one provenance capture per run, bound into report + manifest + the
    # run_start event so every artifact carries the source + binary identity.
    prov = provenance.capture(REPO_ROOT, run.alf_binary, run.backend, cfg.service_repo)
    run.report = RunReport(
        framework=args.framework, tier=f"{run.llm}/{run.backend}",
        stages_requested=stage_ids, run_dir=str(run_dir),
        alf_version=run.expected_alf_version, provenance=prov,
    )

    container_name = f"alf-lifecycle-{args.framework}-{ts.lower()}"
    run.narrator = (NullNarrator() if args.ci
                    else RichNarrator(interactive, container_name))
    run.manifest = Manifest(framework=args.framework, created_at=ts,
                            backend=run.backend, llm=run.llm,
                            container_name=container_name,
                            source_commit=prov.adapters_commit,
                            dirty=prov.adapters_dirty,
                            binary_sha256=prov.binary_sha256)

    ui.banner(f"agent-life lifecycle — {args.framework} · tier {run.llm}/{run.backend}"
              f" · stages {','.join(s.upper() for s in stage_ids)}")
    ui.emit(f"  run dir: {run_dir}")
    ui.emit(f"  alf under test: {run.alf_binary} ({run.expected_alf_version})")
    events.emit(
        "run_start",
        framework=args.framework,
        tier=f"{run.llm}/{run.backend}",
        stages_requested=stage_ids,
        alf_version=run.expected_alf_version,
        run_dir=str(run_dir),
        adapters_commit=prov.adapters_commit,
        binary_sha256=prov.binary_sha256,
    )
    # Viz server before any stage work so operators can open the page and watch
    # events arrive live (mint / docker build / Z01…).
    _start_viz_server(run)

    # Signals → exceptions so finally (teardown) always runs.
    def _sig(_n, _f):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, _sig)
    signal.signal(signal.SIGINT, _sig)

    aborted = False
    try:
        # -- P2 mint + P4 probe (before any docker work) -----------------------
        if run.backend == "real":
            ui.section("OPS", "Mint runtime credentials (one per driver invocation)")
            if args.model:
                run_model = models.resolve(args.model)
                if run_model is None:
                    ui.warn(f"--model {args.model!r} is not a known alias/index; "
                            "forwarding verbatim as a model id (must be in the "
                            "proxy allowlist)")
                    run_model = args.model
            else:
                run_model = models.default_for(args.framework)
            ui.emit(f"  model: {run_model} ({models.label_for(run_model)})")
            # Mint keys on the RUNTIME (`hermes`), not the harness framework dir
            # (`hermes-mcp`) — the provisioner only knows openclaw|zeroclaw|hermes.
            run.creds = provision.mint(cfg.service_repo,
                                       kit_runtime_name(args.framework), run_dir,
                                       model=run_model, env=cfg.subprocess_env())
            provision.write_run_env(run_dir, run.creds)
            run.manifest.tenant_id = run.creds.tenant_id
            run.manifest.seed_agent_id = run.creds.seed_agent_id
            run.manifest.runtime_id = run.creds.runtime_id
            run.manifest.alf_api_url = run.creds.alf_api_url
            run.manifest.llm_model_id = run.creds.llm_model_id
            run.manifest.save(run.paths.manifest)
            ui.ok(f"minted runtime key (len {len(run.creds.runtime_api_key)}) · "
                  f"seed agent {run.creds.seed_agent_id}")

            from .backend import ApiClient
            run.api = ApiClient(run.creds.alf_api_url, run.creds.runtime_api_key)
            status = run.api.resolve_base(run.creds.seed_agent_id)
            if status != 200:
                ui.fail(provision.NEON_EXPIRY_REMEDY)
                raise Preflight(f"P4 backend probe: GET seed agent → HTTP {status}")
            ui.ok(f"backend probe OK ({run.api.url})")
            if cfg.has_db_lane:
                try:
                    from .backend import DbClient
                    run.db = DbClient(cfg.db_url)
                    run.db.query_one("SELECT 1")
                except Exception as e:  # noqa: BLE001 — enrichment only
                    ui.warn(f"Neon enrichment lane unavailable: {str(e)[:80]}")
                    run.db = None
            if cfg.has_s3_lane:
                try:
                    from .backend import S3Client
                    run.s3 = S3Client(cfg.s3_bucket, cfg.aws_region)
                except Exception as e:  # noqa: BLE001
                    ui.warn(f"S3 enrichment lane unavailable: {str(e)[:80]}")
                    run.s3 = None
        else:
            # Benign env-file: proves the D10 ALF_API_URL mechanism without
            # secrets, and keeps `alf check` off the public default URL.
            run.paths.run_env.write_text(
                f"ALF_API_URL={SENTINEL_API_URL}\n", encoding="utf-8")
            os.chmod(run.paths.run_env, 0o600)
            run.manifest.save(run.paths.manifest)

        # -- image + container -------------------------------------------------
        kit_env = KitEnv(run_dir=run_dir, host_home=home, host_alf_home=alf_home,
                         llm=run.llm, backend=run.backend, creds=run.creds)
        run.kit = load_kit(args.framework, kit_env)
        run.manifest.image_tag = run.kit.image_tag
        run.manifest.save(run.paths.manifest)

        ui.section("OPS", f"Build {run.kit.image_tag} (pinned {run.kit.pinned_version}; "
                          "no alf, no secrets in any layer)")
        # CI pre-builds via buildx with a gha layer cache and sets
        # ALFLAB_SKIP_IMAGE_BUILD=1; locally the driver always builds (cached).
        if os.environ.get("ALFLAB_SKIP_IMAGE_BUILD") == "1" and subprocess.run(
                ["docker", "image", "inspect", run.kit.image_tag],
                capture_output=True).returncode == 0:
            ui.ok(f"image {run.kit.image_tag} pre-built (ALFLAB_SKIP_IMAGE_BUILD)")
        else:
            build_image(run.kit.image_tag, run.framework_dir, stream=ui.ok)

        # Per-exec env files live in the run dir (0700, gitignored): secrets
        # travel to `docker exec` via --env-file, never as `-e K=V` argv.
        run.container = DockerContainer(container_name, run.kit.image_tag,
                                        env_dir=run.paths.run_dir / "env-files")
        run.container.destroy()  # stale name from a crashed run
        # Frameworks whose runtime lives inside the framework home (Hermes) need
        # the run's fresh home seeded from the image, or the bind-mount would
        # shadow the runtime. Makes the mounted home the real colocated install.
        if run.kit.seed_home_from_image:
            ui.ok(f"seeding {run.kit.home_mount} from image (real colocated install)")
            seed_home_from_image(run.kit.image_tag, run.kit.home_mount, home,
                                 f"{container_name}-seed")
        run.container.start(
            mounts=[
                (home, run.kit.home_mount, "rw"),
                (alf_home, "/home/agent/.alf", "rw"),
                (run.alf_binary, ALF_DIST, "ro"),
            ],
            env_file=run.paths.run_env if run.paths.run_env.is_file() else None,
        )
        ui.ok(f"container up: {container_name} (sleep infinity; stages via docker exec)")

        # D6 glibc preflight.
        run.state["alf_sha_host"] = host_sha256(run.alf_binary)
        run.state["alf_sha_container"] = run.container.inject_alf(run.alf_binary)
        ok, text = run.container.glibc_probe()
        if not ok:
            ui.fail(f"alf-under-test cannot run in the image: {text[:200]}")
            ui.fail("remedy: cargo zigbuild --release --target x86_64-unknown-linux-musl "
                    "-p alf-cli   (clean: reqwest uses rustls), or pass --alf-bin "
                    "pointing at a musl build")
            raise SystemExit(2)

        # -- alf transport (WP-M4) ---------------------------------------------
        # How the stages drive alf: the terminal path for the shipped frameworks,
        # a persistent `alf mcp serve` stdio session for the generic MCP kit. The
        # invoker is created here (container up, alf injected) and drives every
        # `run.alf.*` call in the stage loop; it is closed in finish().
        run.alf = run.kit.make_invoker(run)

        # -- stage loop ---------------------------------------------------------
        registry = {sid: (title, uses_alf, fn) for sid, title, uses_alf, fn in stages.REGISTRY}
        halted = False
        for sid in stage_ids:
            title, uses_alf, fn = registry[sid]
            result = StageResult(stage_id=sid, title=title)
            run.report.stages.append(result)
            run.narrator.stage_start(sid, title)
            if halted:
                result.status, result.skip_reason = "SKIP", "aborted after earlier failure"
                run.narrator.check("SKIP", title, result.skip_reason)
                continue
            t0 = time.time()
            try:
                if uses_alf:  # re-inject (cp-if-sha-differs) before every alf stage
                    run.state["alf_sha_container"] = run.container.inject_alf(run.alf_binary)
                fn(run, result)
            except SkipStage as s:
                # A recorded FAIL must never be masked by a later skip: the
                # stage stays FAIL and the skip becomes a footnote.
                if any(c.status == "FAIL" for c in result.checks):
                    result.skip_reason = f"(skip after failure) {s.reason}"
                    run.narrator.check("FAIL", title,
                                       f"failed before skip point — {s.reason}")
                else:
                    result.status, result.skip_reason, result.wp = "SKIP", s.reason, s.wp
                    run.narrator.check("SKIP", title,
                                       s.reason + (f" (owner: {s.wp})" if s.wp else ""))
            except InteractiveAbort:
                raise
            except Exception as e:  # noqa: BLE001 — a stage crash is a FAIL
                from .redact import redact
                from .report import Check
                result.add(Check(name="stage crashed", status="FAIL",
                                 detail=f"{type(e).__name__}: {redact(str(e))[:300]}"))
            result.duration_ms = (time.time() - t0) * 1000
            for chk in result.checks:
                run.narrator.check(chk.status, chk.name, chk.detail)
            events.emit(
                "stage_end",
                stage_id=sid,
                status=result.status,
                duration_ms=result.duration_ms,
            )
            if result.status == "FAIL":
                halted = True
            run.narrator.pause()

        run.report.exit_code = 1 if run.report.failed else 0
        return finish(run, aborted=False)

    except InteractiveAbort:
        aborted = True
        ui.warn("aborted by operator — container kept for inspection")
        ui.emit(f"  attach : docker exec -it -u agent {container_name} bash")
        ui.emit(f"  clean  : python3 tests/lifecycle/driver.py --teardown {run_dir}"
                f" && docker rm -f {container_name}")
        run.report.exit_code = 130
        return finish(run, aborted=True)
    except KeyboardInterrupt:
        ui.warn("interrupted — running teardown ladder")
        run.report.exit_code = 130
        return finish(run, aborted=False)
    except (DockerError, RuntimeError) as e:
        ui.fail(str(e))
        run.report.exit_code = 2
        return finish(run, aborted=False)
    except SystemExit as e:
        # Post-mint Preflight (P4 probe) / glibc probe: cloud resources and a
        # running container may already exist — the ladder MUST still run.
        run.report.exit_code = e.code if isinstance(e.code, int) else 2
        finish(run, aborted=False)
        return run.report.exit_code
    except BaseException as e:  # noqa: BLE001 — never leak a minted key/container
        from .redact import redact
        ui.fail(f"unexpected error: {type(e).__name__}: {redact(str(e))[:300]}")
        run.report.exit_code = 2
        return finish(run, aborted=False)


def finish(run: Run, aborted: bool) -> int:
    """Teardown ladder (unless aborted/keep) + report + verdict line."""
    try:
        if run.backend == "real" and not aborted and run.manifest is not None:
            if run.args.keep_agent:
                ui.warn("--keep-agent: skipping the cloud teardown ladder")
            else:
                ui.section("OPS", "Teardown ladder (rungs 1–6, ledger-driven)")
                try:
                    ok = provision.teardown_ladder(
                        run.manifest, run.paths.manifest, run.api, run.container,
                        run.cfg.service_repo,
                        run.kit.name if run.kit else kit_runtime_name(run.args.framework),
                        env=run.cfg.subprocess_env())
                except Exception as e:  # noqa: BLE001 — an interrupted ladder must
                    # never skip container destroy / report / the recovery hint.
                    from .redact import redact
                    ui.fail(f"teardown ladder crashed: {type(e).__name__}: "
                            f"{redact(str(e))[:300]}")
                    ok = False
                (ui.ok if ok else ui.fail)("teardown ladder " + ("complete" if ok else "INCOMPLETE"))
                if not ok:
                    ui.emit(f"  recover: python3 tests/lifecycle/driver.py "
                            f"--teardown {run.paths.run_dir}")
                    if run.report.exit_code == 0:
                        run.report.exit_code = 1
            run.report.teardown = dict(run.manifest.teardown)
    finally:
        # Shut any persistent MCP session (stdin EOF → server exits) before the
        # container goes away. Best-effort: teardown must never crash here.
        if run.alf is not None:
            try:
                run.alf.close()
            except Exception:  # noqa: BLE001
                pass
            # Stdout is the MCP transport: any non-JSON line the server wrote is
            # a protocol violation. The client tolerated them mid-run so the
            # tier could finish (diagnosability), but a run with violations must
            # never exit green — surface them as a synthetic FAIL stage.
            violations = getattr(run.alf, "last_protocol_violations", None) or []
            if violations and run.report is not None:
                synthetic = StageResult(stage_id="mcp-protocol",
                                        title="MCP stdout protocol discipline")
                synthetic.add(Check(
                    name="MCP stdout protocol discipline",
                    status="FAIL",
                    detail=f"{len(violations)} non-JSON stdout line(s) from the MCP "
                           f"server; first: {violations[0][:200]!r}"))
                run.report.stages.append(synthetic)
                if run.report.exit_code == 0:
                    run.report.exit_code = 1
        if run.container is not None and not aborted and not run.args.keep:
            run.container.destroy()
        elif run.container is not None and run.args.keep:
            ui.warn(f"--keep: container {run.container.name} left running")
        if run.report is not None and run.paths is not None:
            run.report.write(run.paths.run_dir)
            counts = run.report.counts()
            events.emit(
                "run_end",
                passed=counts.get("PASS", 0),
                failed=counts.get("FAIL", 0),
                skipped=counts.get("SKIP", 0),
                xfail=counts.get("XFAIL", 0),
                coverage=run.report.coverage,
                isolation=run.report.isolation,
                teardown=run.report.teardown,
                exit_code=run.report.exit_code,
            )
            events.set_event_log(None)
            ui.emit("")
            ui.emit(run.report.verdict_line())
            ui.emit(f"  report: {run.paths.run_dir}/report.md")
            if (run.paths.run_dir / "visualization.html").is_file():
                ui.emit(f"  viz:    {run.paths.run_dir}/visualization.html")
                ui.emit(f"  replay: cd {run.paths.run_dir} && "
                        f"python3 -m http.server {DEFAULT_VIZ_PORT}")
        # Stop after run_end is on disk so a live tab can poll the final event.
        if run.viz_server is not None:
            try:
                run.viz_server.stop()
            except Exception:  # noqa: BLE001
                pass
            run.viz_server = None
    return run.report.exit_code if run.report else 2


# ---------------------------------------------------------------------------
# --teardown / --leak-scan entry points
# ---------------------------------------------------------------------------

def teardown_cli(run_dir: Path, cfg: HarnessConfig, framework: str) -> int:
    manifest_path = run_dir / "run-manifest.json"
    if not manifest_path.is_file():
        ui.fail(f"no run-manifest.json under {run_dir}")
        return 2
    manifest = Manifest.load(manifest_path)
    ui.set_log_file(run_dir / "driver.log")
    ui.banner(f"teardown recovery — {run_dir.name}")
    container = DockerContainer(manifest.container_name, manifest.image_tag) \
        if manifest.container_name else None

    # A backend=none run has no cloud resources — only the kept container
    # needs cleaning (the abort hint prints this command for every tier).
    if manifest.backend != "real":
        if container is not None:
            container.destroy()
        ui.ok("backend=none run: no cloud resources; container removed")
        return 0

    api = None
    env = provision.load_run_env(run_dir)
    if env.get("ALF_API_KEY") and manifest.alf_api_url:
        from .backend import ApiClient
        from .redact import register_secret
        register_secret(env["ALF_API_KEY"])
        api = ApiClient(manifest.alf_api_url, env["ALF_API_KEY"])
        if manifest.seed_agent_id:
            try:
                status = api.resolve_base(manifest.seed_agent_id)
            except Exception as e:  # noqa: BLE001 — transport error ≠ dead key,
                # but the ladder's direct-DB rungs still work without the API.
                ui.warn(f"backend probe unreachable ({type(e).__name__}) — "
                        "falling back to targeted scavenge")
                status = None
            if status != 200:
                if status is not None:
                    ui.warn(f"runtime key already dead (probe HTTP {status}) — "
                            "falling back to targeted scavenge")
                api = None
    # Rung 1's `alf purge -r <runtime>` needs the alf RUNTIME, not the harness
    # framework dir (hermes-mcp → hermes); derive it from the kit, tolerating a
    # missing kit dir in a recovery context.
    try:
        teardown_runtime = kit_runtime_name(manifest.framework)
    except Preflight:
        teardown_runtime = manifest.framework
    ok = provision.teardown_ladder(manifest, manifest_path, api, container,
                                   cfg.service_repo, teardown_runtime,
                                   env=cfg.subprocess_env())
    if container is not None:
        container.destroy()
    (ui.ok if ok else ui.fail)("teardown " + ("clean" if ok else "left residue — see ledger"))
    for rung, status in manifest.teardown.items():
        ui.emit(f"    {rung}: {status}")
    return 0 if ok else 1


def leak_scan_cli(cfg: HarnessConfig) -> int:
    if not cfg.has_db_lane:
        ui.fail("--leak-scan needs NEON_DATABASE_URL in .env (optional Neon lane)")
        return 2
    from .backend import DbClient
    db = DbClient(cfg.db_url)
    manifests = sorted((LIFECYCLE_DIR / "runs").glob("*/run-manifest.json"))
    # A safety tool must fail as a VERDICT, never as a traceback: an operator
    # checking for leaks after a crashed run reads the exit code, and a stack
    # trace scrolling past is easy to mistake for noise. One retry covers the
    # pooled-endpoint transients this hit on 2026-07-28.
    try:
        rows = provision.leak_scan(db, manifests)
    except provision.ScanUntrustworthy as e:
        ui.fail(f"leak scan REFUSED — {e}")
        return 2
    except Exception as e:  # noqa: BLE001 — any DB/driver failure
        try:
            rows = provision.leak_scan(db, manifests)
        except Exception as retry_err:  # noqa: BLE001
            ui.fail(f"leak scan could NOT run ({type(retry_err).__name__}: "
                    f"{str(retry_err).strip()[:200]}) — this is NOT a clean result; "
                    f"re-run, and check the DB lane in .env")
            return 2
        ui.emit(f"  (first attempt failed: {type(e).__name__}; retry succeeded)")
    if rows:
        ui.fail(f"{len(rows)} internal-tenant agent(s) unaccounted for by runs/:")
        for r in rows:
            ui.emit(f"    {r['id']}  {r['name']!r}  created {r['created_at']}")
        return 1
    ui.ok("leak scan clean — every recent internal-tenant agent is accounted for")
    return 0
