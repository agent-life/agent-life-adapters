#!/usr/bin/env python3
"""Verify the exact Python environment used for integration fixture generation."""

from __future__ import annotations

import argparse
import importlib.metadata as metadata
import re
import sys
from pathlib import Path

REQUIREMENT = re.compile(
    r"^\s*([A-Za-z0-9_.-]+)(?:\[[^]]+\])?==([^\s;]+)"
)


def parse_lock(path: Path) -> dict[str, str]:
    locked: dict[str, str] = {}
    for line_number, raw_line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("--hash="):
            continue
        match = REQUIREMENT.match(line)
        if match is None:
            raise ValueError(
                f"{path}:{line_number}: expected an exact 'name==version' requirement"
            )
        name = canonical_name(match.group(1))
        version = match.group(2)
        previous = locked.setdefault(name, version)
        if previous != version:
            raise ValueError(
                f"{path}:{line_number}: {name} is locked more than once "
                f"({previous} and {version})"
            )
    if not locked:
        raise ValueError(f"{path}: no requirements found")
    return locked


def canonical_name(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


def verify(lock: dict[str, str], python_minor: str) -> list[str]:
    actual_minor = f"{sys.version_info.major}.{sys.version_info.minor}"
    errors: list[str] = []
    if actual_minor != python_minor:
        errors.append(
            f"Python {python_minor} is required, but this interpreter is "
            f"{sys.version.split()[0]}."
        )

    for name, expected in sorted(lock.items()):
        try:
            actual = metadata.version(name)
        except metadata.PackageNotFoundError:
            errors.append(f"{name}=={expected} is not installed.")
            continue
        if actual != expected:
            errors.append(f"{name} must be {expected}, but {actual} is installed.")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Verify the pinned integration-fixture Python environment."
    )
    parser.add_argument("--lock", required=True, type=Path)
    parser.add_argument("--python-minor", required=True)
    parser.add_argument(
        "--print-version",
        metavar="PACKAGE",
        help="print one installed distribution version after verification",
    )
    args = parser.parse_args()

    try:
        lock = parse_lock(args.lock)
    except (OSError, ValueError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 2

    errors = verify(lock, args.python_minor)
    if errors:
        print("ERROR: integration fixture Python environment is not locked:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1

    if args.print_version:
        name = canonical_name(args.print_version)
        if name not in lock:
            print(f"ERROR: {args.print_version} is not in {args.lock}", file=sys.stderr)
            return 2
        print(metadata.version(name))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

