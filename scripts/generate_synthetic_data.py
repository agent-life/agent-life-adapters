#!/usr/bin/env python3
"""Generate a deterministic ALF archive from a verified schema checkout."""

from __future__ import annotations

import argparse
import json
import os
import random
import re
import stat
import tempfile
import zipfile
from pathlib import Path
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any

from faker import Faker
from jsf import JSF
from jsonschema import Draft202012Validator, FormatChecker

SCHEMA_FILES = {
    "manifest.json": "manifest.schema.json",
    "identity.json": "identity.schema.json",
    "principals.json": "principals.schema.json",
    "credentials.json": "credentials.schema.json",
    "attachments.json": "attachments.schema.json",
}
MEMORY_RECORD_SCHEMA = "memory-record.schema.json"
PARTITION_FILE = "memory/partitions/2026-Q1.jsonl"
FIXED_NOW = datetime(2026, 1, 1, tzinfo=timezone.utc)
TIMESTAMP = re.compile(
    r"^(\d{4}-\d{2}-\d{2})T\d{2}:\d{2}:\d{2}(?:\.\d+)?(Z|[+-]\d{2}:\d{2})$"
)


class DeepcopyableCounter:
    """JSF's itertools.count cannot be deep-copied on Python 3.14."""

    def __init__(self, start: int = 1) -> None:
        self.current = start

    def __iter__(self) -> "DeepcopyableCounter":
        return self

    def __next__(self) -> int:
        value = self.current
        self.current += 1
        return value


class FrozenDateTime(datetime):
    """A datetime replacement for schema expressions that call now()/today()."""

    @classmethod
    def now(cls, tz: timezone | None = None) -> datetime:
        if tz is None:
            return FIXED_NOW.replace(tzinfo=None)
        return FIXED_NOW.astimezone(tz)

    @classmethod
    def today(cls) -> datetime:
        return cls.now()


@dataclass(frozen=True)
class FixtureInputs:
    schema_dir: Path
    output: Path
    alf_version: str
    seed: int
    schema_revision: str


def canonical_json(value: Any, *, indent: int | None = 2) -> bytes:
    return (
        json.dumps(
            value,
            ensure_ascii=False,
            sort_keys=True,
            indent=indent,
            separators=(",", ": ") if indent is not None else (",", ":"),
            allow_nan=False,
        )
        + "\n"
    ).encode("utf-8")


def load_schema(schema_dir: Path, filename: str) -> dict[str, Any]:
    with (schema_dir / filename).open(encoding="utf-8") as file:
        return json.load(file)


def generation_context(seed: int) -> dict[str, Any]:
    random.seed(seed)
    Faker.seed(seed)
    faker = Faker()
    faker.seed_instance(seed)
    return {
        "faker": faker,
        "random": random.Random(seed),
        "datetime": FrozenDateTime,
        "__internal__": {"List": list, "Union": __import__("typing").Union, "Tuple": tuple},
    }


def generate_data(schema: dict[str, Any], context: dict[str, Any]) -> Any:
    generator = JSF(schema, context=context)
    generator.base_state["__counter__"] = DeepcopyableCounter(start=1)
    return generator.generate()


def ensure_nonempty(value: Any, fallback: str) -> str:
    return value if isinstance(value, str) and value else fallback


def repair_generated_data(
    manifest: dict[str, Any],
    identity: dict[str, Any],
    principals: dict[str, Any],
    credentials: dict[str, Any],
    attachments: dict[str, Any],
    memory_records: list[dict[str, Any]],
    alf_version: str,
) -> None:
    agent = manifest.setdefault("agent", {})
    agent_id = ensure_nonempty(
        agent.get("id"), "00000000-0000-0000-0000-000000000000"
    )
    agent["id"] = agent_id
    agent["name"] = ensure_nonempty(agent.get("name"), "Synthetic Agent")
    agent["source_runtime"] = ensure_nonempty(
        agent.get("source_runtime"), "test_runtime"
    )
    manifest["created_at"] = FIXED_NOW.isoformat().replace("+00:00", "Z")
    manifest["alf_version"] = alf_version

    identity["agent_id"] = agent_id
    structured = identity.get("structured")
    if isinstance(structured, dict):
        for sub_agent in structured.get("sub_agents", []):
            if isinstance(sub_agent, dict):
                sub_agent["name"] = ensure_nonempty(
                    sub_agent.get("name"), "Synthetic sub-agent"
                )
        for index, capability in enumerate(structured.get("capabilities", [])):
            if isinstance(capability, dict):
                capability["name"] = ensure_nonempty(
                    capability.get("name"), f"capability-{index}"
                )

    for principal in principals.get("principals", []):
        if isinstance(principal, dict) and isinstance(principal.get("profile"), dict):
            principal["profile"]["principal_id"] = principal.get("id")

    for credential in credentials.get("credentials", []):
        if not isinstance(credential, dict):
            continue
        credential["encrypted_payload"] = ensure_nonempty(
            credential.get("encrypted_payload"), "a"
        )
        credential["service"] = ensure_nonempty(credential.get("service"), "test-service")
        encryption = credential.get("encryption")
        if isinstance(encryption, dict):
            encryption["nonce"] = ensure_nonempty(encryption.get("nonce"), "a")
            encryption["algorithm"] = ensure_nonempty(encryption.get("algorithm"), "a")

    for index, record in enumerate(memory_records):
        record["id"] = f"018f8a00-0000-7000-8000-{index + 1:012x}"
        record["agent_id"] = agent_id
        record["namespace"] = ensure_nonempty(record.get("namespace"), "default")
        source = record.get("source")
        if isinstance(source, dict):
            source["runtime"] = ensure_nonempty(source.get("runtime"), "test_runtime")
        for embedding in record.get("embeddings", []):
            if isinstance(embedding, dict):
                dimensions = max(1, int(embedding.get("dimensions", 1)))
                embedding["vector"] = [0.1] * dimensions
                embedding["dimensions"] = dimensions

    layers = manifest.setdefault("layers", {})
    layers.setdefault("identity", {})["file"] = "identity.json"
    layers["identity"]["version"] = identity.get("version", 1)
    layers.setdefault("principals", {})["file"] = "principals.json"
    layers["principals"]["count"] = len(principals.get("principals", []))
    layers.setdefault("credentials", {})["file"] = "credentials.json"
    layers["credentials"]["count"] = len(credentials.get("credentials", []))
    layers.setdefault("attachments", {})["file"] = "attachments.json"
    layers["attachments"]["count"] = len(attachments.get("attachments", []))
    layers["attachments"]["included_count"] = 0
    layers["attachments"]["included_size_bytes"] = 0
    layers["attachments"]["referenced_count"] = 0
    layers["attachments"]["referenced_size_bytes"] = 0
    memory = layers.setdefault("memory", {})
    memory["index_file"] = "memory/index.json"
    memory["partitions"] = [
        {
            "file": PARTITION_FILE,
            "from": "2026-01-01",
            "to": "2026-03-31",
            "record_count": len(memory_records),
            "sealed": False,
        }
    ]
    memory["record_count"] = len(memory_records)


def normalize_timestamps(value: Any) -> Any:
    if isinstance(value, dict):
        for key, nested in value.items():
            value[key] = normalize_timestamps(nested)
        return value
    if isinstance(value, list):
        return [normalize_timestamps(item) for item in value]
    if isinstance(value, str):
        match = TIMESTAMP.match(value)
        if match:
            return f"{match.group(1)}T00:00:00.000000{match.group(2)}"
    return value

def archive_entries(inputs: FixtureInputs) -> dict[str, bytes]:
    context = generation_context(inputs.seed)
    schemas = {
        filename: load_schema(inputs.schema_dir, schema_filename)
        for filename, schema_filename in SCHEMA_FILES.items()
    }
    memory_schema = load_schema(inputs.schema_dir, MEMORY_RECORD_SCHEMA)

    manifest = generate_data(schemas["manifest.json"], context)
    identity = generate_data(schemas["identity.json"], context)
    principals = generate_data(schemas["principals.json"], context)
    credentials = generate_data(schemas["credentials.json"], context)
    attachments = generate_data(schemas["attachments.json"], context)
    memory_records = [generate_data(memory_schema, context) for _ in range(10)]
    repair_generated_data(
        manifest,
        identity,
        principals,
        credentials,
        attachments,
        memory_records,
        inputs.alf_version,
    )
    for generated in (manifest, identity, principals, credentials, attachments, memory_records):
        normalize_timestamps(generated)


    memory_index = {"partitions": manifest["layers"]["memory"]["partitions"]}
    return {
        "artifacts/.keep": b"",
        "attachments.json": canonical_json(attachments),
        "credentials.json": canonical_json(credentials),
        "identity.json": canonical_json(identity),
        "manifest.json": canonical_json(manifest),
        "memory/index.json": canonical_json(memory_index),
        PARTITION_FILE: b"".join(
            canonical_json(record, indent=None) for record in memory_records
        ),
        "principals.json": canonical_json(principals),
        "raw/openclaw/.keep": b"",
    }


def zip_info(name: str) -> zipfile.ZipInfo:
    info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
    info.create_system = 3
    info.create_version = 30
    info.extract_version = 10
    info.external_attr = (stat.S_IFREG | 0o644) << 16
    info.compress_type = zipfile.ZIP_STORED
    return info


def write_archive(entries: dict[str, bytes], output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{output.name}.", suffix=".tmp", dir=output.parent
    )
    os.close(descriptor)
    temporary = Path(temporary_name)
    try:
        with zipfile.ZipFile(temporary, "w", compression=zipfile.ZIP_STORED) as archive:
            for name in sorted(entries):
                archive.writestr(zip_info(name), entries[name])
        os.replace(temporary, output)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise


def validation_errors(value: Any, schema: dict[str, Any]) -> list[str]:
    validator = Draft202012Validator(schema, format_checker=FormatChecker())
    return [
        f"{'/'.join(str(part) for part in error.absolute_path) or '<root>'}: {error.message}"
        for error in sorted(validator.iter_errors(value), key=lambda item: list(item.path))
    ]


def validate_archive(archive_path: Path, schema_dir: Path) -> None:
    schemas = {
        filename: load_schema(schema_dir, schema_filename)
        for filename, schema_filename in SCHEMA_FILES.items()
    }
    memory_schema = load_schema(schema_dir, MEMORY_RECORD_SCHEMA)
    errors: list[str] = []

    with zipfile.ZipFile(archive_path) as archive:
        for filename, schema in schemas.items():
            try:
                value = json.loads(archive.read(filename))
            except KeyError:
                errors.append(f"{filename}: missing")
                continue
            errors.extend(
                f"{filename}: {error}" for error in validation_errors(value, schema)
            )
        try:
            records = archive.read(PARTITION_FILE).decode("utf-8").splitlines()
        except KeyError:
            errors.append(f"{PARTITION_FILE}: missing")
        else:
            for line_number, line in enumerate(records, 1):
                value = json.loads(line)
                errors.extend(
                    f"{PARTITION_FILE}:{line_number}: {error}"
                    for error in validation_errors(value, memory_schema)
                )

    if errors:
        raise ValueError(
            "generated archive does not validate against pinned schemas:\n  - "
            + "\n  - ".join(errors)
        )


def generate_fixture(inputs: FixtureInputs) -> None:
    write_archive(archive_entries(inputs), inputs.output)
    validate_archive(inputs.output, inputs.schema_dir)


def parse_args() -> FixtureInputs:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--schema-dir", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--alf-version", required=True)
    parser.add_argument("--seed", required=True, type=int)
    parser.add_argument("--schema-revision", required=True)
    args = parser.parse_args()
    schema_dir = args.schema_dir.resolve()
    if not (schema_dir / "manifest.schema.json").is_file():
        parser.error(f"{schema_dir} does not contain manifest.schema.json")
    return FixtureInputs(
        schema_dir=schema_dir,
        output=args.output.resolve(),
        alf_version=args.alf_version,
        seed=args.seed,
        schema_revision=args.schema_revision,
    )


def main() -> int:
    inputs = parse_args()
    generate_fixture(inputs)
    print(
        f"Generated {inputs.output} from schema {inputs.schema_revision} "
        f"(ALF format {inputs.alf_version})."
    )
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
