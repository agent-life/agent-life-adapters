"""LLM model catalog + alias resolution for the lifecycle harness.

The LLM proxy validates the requested model against a server-side allowlist
(`LLM_MODEL_ALLOWLIST`); this catalog mirrors that allowlist and adds short
aliases so `--model claude-haiku` works instead of only the full Bedrock id.
It is the Python port of the bash catalog in
`scripts/multiagent-walkthrough.sh` (kept in sync by hand — same six ids).

`default_for(framework)` is the model the harness mints with when `--model` is
omitted. It is deliberately explicit (the harness always passes `--llm-model`)
so the test path never inherits the *production* default in the service's
`llm_allowlist.rs` (that constant also governs real Fly spawns).

The default is **per-framework** (`TEST_DEFAULT_MODELS`), because coverage is not
equally model-sensitive across runtimes (A5 sweep, see
docs/multi-agent-support/wp6-live-tier-results.md): ZeroClaw (`auto_save`) and
Hermes (session store) auto-capture memory and pass on the cheapest model
(nova-lite), while OpenClaw's coverage needs the agent to *actively write*
`MEMORY.md` — real tool use that nova-lite fails, so it uses a capable model.
"""

from __future__ import annotations

from typing import Optional

# (full Bedrock id, alias, human label) — cheapest first. Mirrors the deployed
# LLM_MODEL_ALLOWLIST (agent-life-service infra/deploy.sh) and
# scripts/multiagent-walkthrough.sh:49-56.
MODEL_CATALOG: list[tuple[str, str, str]] = [
    ("us.amazon.nova-lite-v1:0", "nova-lite", "Amazon Nova Lite"),
    ("us.amazon.nova-2-lite-v1:0", "nova2-lite", "Amazon Nova 2 Lite"),
    ("global.anthropic.claude-haiku-4-5-20251001-v1:0", "claude-haiku", "Claude 4.5 Haiku"),
    ("minimax.minimax-m2.5", "minimax", "MiniMax M2.5"),
    ("moonshotai.kimi-k2.5", "kimi", "Kimi K2.5"),
    ("deepseek.v3.2", "deepseek", "DeepSeek V3.2"),
]

# Per-framework default proxy-tier model (A5 sweep, verified live 2026-07-05):
#   zeroclaw: nova-lite PASS 13/13 (194238Z) — auto_save, no tool-use test
#   hermes:   nova-lite PASS 13/13 (194522Z) — session store, no tool-use test
#   openclaw: nova-lite FAIL 9/10 (194929Z) → minimax PASS 14/14 (195811Z) —
#             needs the agent to actively write MEMORY.md (real tool use)
# A cheaper OpenClaw-passing model may exist (nova2-lite/claude-haiku untested
# there); minimax is the known-good choice.
TEST_DEFAULT_MODELS = {
    "zeroclaw": "us.amazon.nova-lite-v1:0",
    "hermes": "us.amazon.nova-lite-v1:0",
    "openclaw": "minimax.minimax-m2.5",
}
# For any framework not listed: the capable, known-good model (never the cheapest,
# since an unswept framework may exercise tool use).
FALLBACK_DEFAULT_MODEL = "minimax.minimax-m2.5"


def default_for(framework: str) -> str:
    """The default proxy-tier model for `framework` (see `TEST_DEFAULT_MODELS`)."""
    return TEST_DEFAULT_MODELS.get(framework, FALLBACK_DEFAULT_MODEL)


def resolve(token: str) -> Optional[str]:
    """Map a 1-based index, alias, or full id to a full Bedrock id.

    Returns None for an unknown token (the caller decides whether to forward it
    verbatim as a possibly-future id or to error). Mirrors `model_id_for` in
    scripts/multiagent-walkthrough.sh:62-66.
    """
    token = token.strip()
    for i, (model_id, alias, _label) in enumerate(MODEL_CATALOG, start=1):
        if token in (str(i), alias, model_id):
            return model_id
    return None


def label_for(model_id: str) -> str:
    """Human label for a full id (falls back to the id itself if off-catalog)."""
    for cid, _alias, label in MODEL_CATALOG:
        if cid == model_id:
            return label
    return model_id


def catalog_lines() -> list[str]:
    """`# alias label id` rows for --list-models / help output."""
    lines = ["  #  alias          label                  id"]
    for i, (model_id, alias, label) in enumerate(MODEL_CATALOG, start=1):
        marks = sorted(fw for fw, mid in TEST_DEFAULT_MODELS.items() if mid == model_id)
        mark = f"  <- default: {', '.join(marks)}" if marks else ""
        lines.append(f"  {i:<2} {alias:<14} {label:<22} {model_id}{mark}")
    return lines
