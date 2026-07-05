"""Central secret redaction — wired into EVERY output sink (ui, driver.log,
report.md/json, rendered diffs). Secret hygiene is a hard WP2 constraint: no
runtime key may reach a terminal, a log file, or a committed file.

Deliberately NOT redacted: the committed fake scenario markers
(…-FAKE-…, e.g. sk-atlas-r1-FAKE-1A2B) — they are the assertion material and
match none of the patterns below.
"""

from __future__ import annotations

import re

# Order matters: value-bearing patterns first, generic ones after.
_PATTERNS: list[tuple[re.Pattern, str]] = [
    # The service runtime/API key shape: alf_<32 alnum> (repo grep gate shape).
    (re.compile(r"alf_[A-Za-z0-9]{32}"), "alf_[REDACTED]"),
    # HTTP auth headers.
    (re.compile(r"(?i)(Bearer\s+)\S+"), r"\1[REDACTED]"),
    # TOML/INI-style assignments (config.toml, run.env renders, provisioner block).
    (
        re.compile(
            r"(?im)^(\s*(?:api_key|runtime_api_key|password|user_email_password|"
            r"bot_token)\s*=\s*).+$"
        ),
        r"\1[REDACTED]",
    ),
    # env-file style KEY=value lines.
    (
        re.compile(r"(?im)^((?:ALF_API_KEY|RUNTIME_API_KEY|API_KEY)=).*$"),
        r"\1[REDACTED]",
    ),
    # Connection-string passwords (postgres://user:pass@host).
    (re.compile(r"(?i)((?:postgres|postgresql|mysql)://[^:/\s@]+:)[^@\s]+@"), r"\1[REDACTED]@"),
    # AWS-style secrets, defensively.
    (re.compile(r"(?i)(aws_secret_access_key\s*[=:]\s*)\S+"), r"\1[REDACTED]"),
]


# Exact raw secret values seen by this process (the minted runtime key, a
# loaded ALF_API_KEY). Pattern shapes cannot survive truncation of a fragment;
# exact values are additionally scrubbed by prefix so a sliced key still dies.
_SECRET_VALUES: set[str] = set()


def register_secret(value: str) -> None:
    """Register a raw secret for exact-value (and truncated-prefix) scrubbing."""
    if value and len(value) >= 12:
        _SECRET_VALUES.add(value)


def redact(text: str) -> str:
    """Redact known secret shapes from a string. Idempotent."""
    if not text:
        return text
    for secret in _SECRET_VALUES:
        if secret in text:
            text = text.replace(secret, "[REDACTED]")
        else:
            # A truncated line may hold only a prefix of the secret: scrub any
            # prefix long enough to be meaningfully secret (>= 12 chars).
            for n in range(len(secret) - 1, 11, -1):
                prefix = secret[:n]
                if prefix in text:
                    text = text.replace(prefix, "[REDACTED]")
                    break
    for pattern, repl in _PATTERNS:
        text = pattern.sub(repl, text)
    return text


def redact_obj(obj):
    """Recursively redact every string inside a JSON-shaped object."""
    if isinstance(obj, str):
        return redact(obj)
    if isinstance(obj, list):
        return [redact_obj(v) for v in obj]
    if isinstance(obj, tuple):
        return tuple(redact_obj(v) for v in obj)
    if isinstance(obj, dict):
        return {k: redact_obj(v) for k, v in obj.items()}
    return obj
