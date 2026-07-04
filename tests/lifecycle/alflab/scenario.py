"""Round-tagged marker scenario — the single source of truth (D9).

Marker grammar: {PERSONA}-{TYPE}{ROUND}-{NONCE4}; secrets sk-{persona}-r{N}-FAKE-{NONCE4}.
All secrets are OBVIOUSLY FAKE throwaway values (…-FAKE-…), committed on
purpose: a deterministic integration test needs known values to assert
against. The real runtime LLM-proxy key never appears here (and would be
redacted by alflab.redact everywhere anyway).

Round 2+ prompts/seeds are append-shaped: a later round only ADDS memories, so
a delta between round N and N+1 is exactly the new round's rows (delta
exactness; also respects OpenClaw's positional ids later — WP4).

The legacy bash scenario (scripts/multiagent-scenario.sh) stays for the spike
testkits until WP3–5 absorb them; LEGACY_ROUND1_MARKERS below is the frozen
copy the drift-guard (tests/lifecycle/test_scenario_drift.py) compares against
so the two sources can't drift silently while both live.
"""

from __future__ import annotations

from dataclasses import dataclass

TYPES = ("semantic", "episodic", "procedural", "secret")

# type → the category the seeded tier writes into ZeroClaw's REAL taxonomy
# (core/episodic/procedure/conversation/credentials — captured schema).
ZEROCLAW_CATEGORY = {
    "semantic": "core",
    "episodic": "episodic",
    "procedural": "procedure",
    "secret": "credentials",
}

# slot → persona. Slot names are per-framework agent handles; the pilot's
# single implicit agent maps to slot "default".
PERSONAS = {
    "default": "ATLAS",
    "agent_a": "ATLAS",
    "agent_b": "NOVA",
}

# Fixed per-(persona, type, round) nonces — deterministic and committed (fake).
_NONCES = {
    ("ATLAS", "semantic", 1): "7F3A", ("ATLAS", "episodic", 1): "2B9C",
    ("ATLAS", "procedural", 1): "5D1E", ("ATLAS", "secret", 1): "1A2B",
    ("ATLAS", "semantic", 2): "9E4C", ("ATLAS", "episodic", 2): "6D2F",
    ("ATLAS", "procedural", 2): "8A7B", ("ATLAS", "secret", 2): "3C4D",
    ("NOVA", "semantic", 1): "8C2B", ("NOVA", "episodic", 1): "3D7F",
    ("NOVA", "procedural", 1): "6A4B", ("NOVA", "secret", 1): "9Z8Y",
    ("NOVA", "semantic", 2): "5B1A", ("NOVA", "episodic", 2): "7C3E",
    ("NOVA", "procedural", 2): "2F9D", ("NOVA", "secret", 2): "4E5F",
}


@dataclass(frozen=True)
class MarkerTurn:
    slot: str
    persona: str
    turn_type: str   # semantic | episodic | procedural | secret
    round: int
    marker: str
    prompt: str


def marker_for(slot: str, turn_type: str, round: int) -> str:
    persona = PERSONAS[slot]
    nonce = _NONCES[(persona, turn_type, round)]
    if turn_type == "secret":
        return f"sk-{persona.lower()}-r{round}-FAKE-{nonce}"
    abbrev = {"semantic": "SEM", "episodic": "EPI", "procedural": "PROC"}[turn_type]
    return f"{persona}-{abbrev}{round}-{nonce}"


def _prompt(persona: str, turn_type: str, round: int, marker: str) -> str:
    name = persona.capitalize()
    if turn_type == "semantic":
        return (
            f"Permanently remember these durable facts in your long-term (semantic) "
            f"memory. You are {name}, an infrastructure assistant. Your primary user "
            f"is Johan, based in Cape Town, who prefers terse technical answers. "
            f"Record the verbatim reference code {marker} with these facts so they "
            f"can be looked up later."
        )
    if turn_type == "episodic":
        return (
            f"Record an episodic log entry for TODAY: in round {round} you completed "
            f"a database migration and rotated the TLS certificates. Save it as a "
            f"dated/episodic memory and include the verbatim event id {marker}."
        )
    if turn_type == "procedural":
        return (
            f"Save the following as a reusable, named PROCEDURE called "
            f"\"deploy-r{round}\": 1) run the full test suite, 2) tag the release, "
            f"3) push to main, 4) watch the canary for 10 minutes. Store it as "
            f"procedural memory and include the verbatim procedure id {marker}."
        )
    return (
        f"Keep this credential for me — it is a throwaway TEST value, safe to "
        f"store: STAGING_API_TOKEN_R{round} = {marker}-DO-NOT-USE. Store it "
        f"wherever you keep secrets/credentials so you can recall it later."
    )


def turns(slot: str, round: int = 1) -> list[MarkerTurn]:
    """The 4 marker turns for one agent slot and round."""
    persona = PERSONAS[slot]
    out = []
    for turn_type in TYPES:
        marker = marker_for(slot, turn_type, round)
        out.append(MarkerTurn(
            slot=slot, persona=persona, turn_type=turn_type, round=round,
            marker=marker, prompt=_prompt(persona, turn_type, round, marker),
        ))
    return out


def markers(slot: str, round: int = 1) -> list[str]:
    return [t.marker for t in turns(slot, round)]


# ---------------------------------------------------------------------------
# Drift guard against the legacy bash scenario (scripts/multiagent-scenario.sh)
# ---------------------------------------------------------------------------
# Frozen copy of the legacy round-1 markers. If someone edits the bash
# scenario, tests/lifecycle/test_scenario_drift.py fails and the change must
# be mirrored (or the guard consciously updated) here.
LEGACY_ROUND1_MARKERS = {
    "agent_a": {
        "semantic": "CAPETOWN-ATLAS-SEM7F3A",
        "episodic": "ATLAS-EPI-2B9C",
        "procedural": "ATLAS-PROC-5D1E",
        "secret": "sk-atlas-FAKE-1a2b3c",
    },
    "agent_b": {
        "semantic": "BERLIN-NOVA-SEM8C2B",
        "episodic": "NOVA-EPI-3D7F",
        "procedural": "NOVA-PROC-6A4B",
        "secret": "nova-FAKE-9z8y7x",
    },
}
