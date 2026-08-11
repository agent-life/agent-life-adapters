"""Docker choreography (D6/D7): one long-lived container per run, stages via
`docker exec`; the alf-under-test is bind-mounted ro and installed by an
idempotent cp-if-sha-differs, so images stay alf-free and secret-free."""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import tempfile
import uuid
from pathlib import Path
from typing import Optional

ALF_DIST = "/opt/alf-dist/alf"       # ro bind-mount of the host binary
ALF_BIN = "/usr/local/bin/alf"       # where the container runs it from


class DockerError(RuntimeError):
    pass


def _run(argv: list[str], *, timeout: int = 600, check: bool = True,
         capture: bool = True) -> subprocess.CompletedProcess:
    proc = subprocess.run(argv, capture_output=capture, text=True, timeout=timeout)
    if check and proc.returncode != 0:
        raise DockerError(
            f"{' '.join(argv[:4])}… failed (exit {proc.returncode}):\n"
            f"{(proc.stderr or proc.stdout or '')[-2000:]}"
        )
    return proc


def host_sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def build_image(tag: str, context_dir: Path, build_args: Optional[dict] = None,
                stream=None) -> None:
    """`docker build` with context = the framework dir ONLY (no secrets in
    scope, nothing to leak into a layer)."""
    argv = ["docker", "build", "-t", tag]
    for k, v in (build_args or {}).items():
        argv += ["--build-arg", f"{k}={v}"]
    argv.append(str(context_dir))
    proc = subprocess.run(argv, capture_output=True, text=True, timeout=1800)
    if proc.returncode != 0:
        raise DockerError(f"docker build {tag} failed:\n{(proc.stderr or '')[-3000:]}")
    if stream:
        stream(f"built image {tag}")


def seed_home_from_image(image: str, src_path: str, host_dest: Path,
                         seed_name: str) -> None:
    """Populate `host_dest` (the run's fresh home) from `image`'s `src_path`,
    for frameworks whose runtime lives inside the framework home (Hermes). Uses a
    throwaway (created, never started) container + `docker cp`, which preserves
    the image's `agent` ownership. The runner's post-start chown then aligns it
    to the host uid. So the per-run mounted home is the real colocated install a
    user has — runtime + data — instead of an empty dir the mount would leave."""
    _run(["docker", "create", "--name", seed_name, image], timeout=120)
    try:
        _run(["docker", "cp", f"{seed_name}:{src_path}/.", str(host_dest)], timeout=600)
    finally:
        _run(["docker", "rm", "-f", seed_name], check=False)


class DockerContainer:
    """One `sleep infinity` container; every stage is a `docker exec`.

    `env_dir`: where per-exec `--env-file` files are written (the runner sets
    `{run_dir}/env-files` — ephemeral, chmod-700 parent, gitignored). Secrets
    NEVER travel as `-e K=V` argv elements (argv is world-readable via
    /proc/*/cmdline and `ps`); every exec env goes through a 0600 env file."""

    def __init__(self, name: str, image: str, *, user: str = "agent",
                 home: str = "/home/agent", env_dir: Optional[Path] = None):
        self.name = name
        self.image = image
        self.user = user
        self.home = home
        self.env_dir = env_dir

    # -- lifecycle -----------------------------------------------------------

    def start(self, mounts: list[tuple[Path, str, str]], env_file: Optional[Path] = None):
        """mounts: [(host_path, container_path, 'ro'|'rw')]. Secrets travel
        ONLY via --env-file (never argv)."""
        argv = ["docker", "run", "-d", "--name", self.name]
        if env_file is not None:
            argv += ["--env-file", str(env_file)]
        for host, ctr, mode in mounts:
            argv += ["-v", f"{host}:{ctr}:{mode}"]
        argv += [self.image, "sleep", "infinity"]
        _run(argv)
        # The host uid owning the bind-mounted homes (e.g. GitHub runner uid
        # 1001) rarely matches the image's `agent` uid (1000) — chown the rw
        # mounts so in-container writes succeed regardless of host identity.
        rw_targets = [ctr for _, ctr, mode in mounts if mode == "rw"]
        if rw_targets:
            _run(["docker", "exec", "-u", "0", self.name,
                  "chown", "-R", f"{self.user}:{self.user}", *rw_targets])

    def alive(self) -> bool:
        proc = _run(["docker", "inspect", "-f", "{{.State.Running}}", self.name],
                    check=False)
        return proc.returncode == 0 and proc.stdout.strip() == "true"

    def destroy(self):
        _run(["docker", "rm", "-f", self.name], check=False)

    # -- exec ----------------------------------------------------------------

    def _env_args(self, env: Optional[dict]) -> list[str]:
        """Write `env` to a fresh 0600 env file and return `["--env-file", path]`
        (`[]` when there is nothing to pass). Never `-e K=V` — a secret on argv
        leaks via /proc/*/cmdline and `ps`. Files are uuid-named per exec and NOT
        deleted at close: the run dir is ephemeral + gitignored, and a live
        `exec_stdio` session may outlast the call that spawned it."""
        if not env:
            return []
        env_dir = self.env_dir
        if env_dir is None:  # no runner wiring (e.g. teardown CLI): private tmp
            env_dir = Path(tempfile.mkdtemp(prefix=f"alf-envfiles-{self.name}-"))
            self.env_dir = env_dir
        env_dir.mkdir(parents=True, exist_ok=True)
        path = env_dir / f"exec-{uuid.uuid4().hex}.env"
        body = "".join(f"{k}={v}\n" for k, v in env.items())
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(body)
        return ["--env-file", str(path)]

    def _exec_argv(self, argv: list[str], *, user: Optional[str] = None,
                   env: Optional[dict] = None, stdio: bool = False) -> list[str]:
        """The full `docker exec` argv for a one-shot (`exec`) or persistent
        (`exec_stdio`, adds `-i`) invocation. Factored so tests can assert the
        no-secrets-on-argv property without docker."""
        cmd = ["docker", "exec"]
        if stdio:
            cmd.append("-i")
        cmd += ["-u", user or self.user]
        cmd += self._env_args(env)
        cmd += [self.name] + argv
        return cmd

    def exec(self, argv: list[str], *, user: Optional[str] = None,
             env: Optional[dict] = None, timeout: int = 300,
             check: bool = False) -> subprocess.CompletedProcess:
        return _run(self._exec_argv(argv, user=user, env=env),
                    timeout=timeout, check=check)

    def exec_json(self, argv: list[str], **kw) -> tuple[subprocess.CompletedProcess, Optional[dict]]:
        """Run a JSON-first CLI (ALF_HUMAN stays unset) and parse stdout."""
        proc = self.exec(argv, **kw)
        parsed = None
        out = (proc.stdout or "").strip()
        if out:
            try:
                parsed = json.loads(out)
            except json.JSONDecodeError:
                parsed = None
        return proc, parsed

    def sh(self, script: str, *, user: Optional[str] = None, timeout: int = 300,
           check: bool = False) -> subprocess.CompletedProcess:
        return self.exec(["sh", "-c", script], user=user, timeout=timeout, check=check)

    def exec_stdio(self, argv: list[str], *, user: Optional[str] = None,
                   env: Optional[dict] = None) -> "StdioSession":
        """Start a PERSISTENT `docker exec -i` process and return a bidirectional
        stdio handle to it — the primitive an MCP client needs (WP-M4 task 1b).

        The plain `exec`/`exec_json` above are one-shot: they run to completion
        and capture output. An MCP server is a long-lived subprocess the client
        drives with request/response JSON-RPC over stdin/stdout, so it needs an
        OPEN pipe, not `subprocess.run`. `-i` keeps stdin attached; stdout/stderr
        are byte pipes. Secrets travel via a 0600 `--env-file` exactly as the
        one-shot `exec` does (never `-e K=V` on argv) — for the MCP server that
        is `ALF_API_KEY` / `ALF_AGENT`."""
        return StdioSession(self._exec_argv(argv, user=user, env=env, stdio=True))

    # -- alf injection (D6) ---------------------------------------------------

    def inject_alf(self, host_binary: Path) -> str:
        """Idempotent root install of the ro-mounted host binary; returns the
        container-side sha256 (must equal the host sha)."""
        want = host_sha256(host_binary)
        probe = self.sh(f"sha256sum {ALF_BIN} 2>/dev/null | cut -d' ' -f1", user="root")
        have = (probe.stdout or "").strip()
        if have != want:
            self.sh(f"cp {ALF_DIST} {ALF_BIN} && chmod 755 {ALF_BIN}", user="root",
                    check=True)
        out = self.sh(f"sha256sum {ALF_BIN} | cut -d' ' -f1", user="root", check=True)
        return (out.stdout or "").strip()

    def glibc_probe(self) -> tuple[bool, str]:
        """`alf --version` in-container. A loader failure (exit 127 /
        'not found' / GLIBC errors) means the host binary isn't runnable in
        the image — the caller exits 2 with the musl remedy (D6)."""
        proc = self.sh(f"{ALF_BIN} --version")
        text = ((proc.stdout or "") + (proc.stderr or "")).strip()
        ok = proc.returncode == 0 and text.startswith("alf ")
        return ok, text


class StdioSession:
    """A live child process exposing byte-pipe stdin/stdout/stderr — the
    transport an `McpClient` drives. Backs both a `docker exec -i` MCP server
    (in-container) and a bare local `alf mcp serve` (unit tests), so the client
    is identical in CI and in the lifecycle container."""

    def __init__(self, argv: list[str], *, env: Optional[dict] = None,
                 cwd: Optional[str] = None):
        self.argv = argv
        self.proc = subprocess.Popen(
            argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, bufsize=0, env=env, cwd=cwd,
        )

    def alive(self) -> bool:
        return self.proc.poll() is None

    def close(self, timeout: int = 10) -> Optional[int]:
        """Close stdin (EOF → the server exits, MCP-style), then reap and close
        the read pipes (the process is dead, so the reader threads have hit EOF)."""
        code = None
        try:
            if self.proc.stdin is not None:
                try:
                    self.proc.stdin.close()
                except (BrokenPipeError, ValueError):
                    pass
            try:
                code = self.proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                code = self.proc.wait(timeout=timeout)
        except Exception:  # noqa: BLE001 — best-effort teardown
            code = None
        finally:
            for pipe in (self.proc.stdout, self.proc.stderr):
                if pipe is not None:
                    try:
                        pipe.close()
                    except (OSError, ValueError):
                        pass
        return code


def local_stdio_session(argv: list[str], *, env: Optional[dict] = None,
                        cwd: Optional[str] = None) -> StdioSession:
    """A local (no-docker) stdio session — `alf mcp serve` as a host subprocess.
    Used by the harness self-tests so the MCP client + invoker mapping are
    covered in the zero-secrets CI tier without a container."""
    return StdioSession(argv, env=env, cwd=cwd)
