"""Docker choreography (D6/D7): one long-lived container per run, stages via
`docker exec`; the alf-under-test is bind-mounted ro and installed by an
idempotent cp-if-sha-differs, so images stay alf-free and secret-free."""

from __future__ import annotations

import hashlib
import json
import subprocess
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


class DockerContainer:
    """One `sleep infinity` container; every stage is a `docker exec`."""

    def __init__(self, name: str, image: str, *, user: str = "agent",
                 home: str = "/home/agent"):
        self.name = name
        self.image = image
        self.user = user
        self.home = home

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

    def exec(self, argv: list[str], *, user: Optional[str] = None,
             env: Optional[dict] = None, timeout: int = 300,
             check: bool = False) -> subprocess.CompletedProcess:
        cmd = ["docker", "exec", "-u", user or self.user]
        for k, v in (env or {}).items():
            cmd += ["-e", f"{k}={v}"]
        cmd += [self.name] + argv
        return _run(cmd, timeout=timeout, check=check)

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
