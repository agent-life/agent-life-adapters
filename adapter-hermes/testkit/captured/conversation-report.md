# Hermes — multi-agent memory conversation

_Model: us.amazon.nova-lite-v1:0. Each profile is a fully isolated HERMES_HOME (own state.db)._
_Detailed transcripts: captured/home/<agent>/logs/conversation/ (gitignored)._

## Hermes

Memory-type coverage (✓ = the turn's unique marker is present in that agent's stored memory):

| agent | semantic | episodic | procedural | secret |
|-------|:--------:|:--------:|:----------:|:------:|
| agent_a (Atlas) | ✓ | ✓ | ✓ | ✓ |
| agent_b (Nova) | ✓ | ✓ | ✓ | ✓ |

Isolation (the *other* agent's markers must NOT appear in this agent's memory):

- agent_a: clean (no foreign markers)
- agent_b: clean (no foreign markers)

**Verdict:** coverage 8/8 memory markers · isolation clean

<!-- VERDICT coverage=8/8 isolation=clean -->

Where agent_a's markers landed:
```
CAPETOWN-ATLAS-SEM7F3A -> agent_a/memories/USER.md state.db(messages)
ATLAS-EPI-2B9C -> agent_a/memories/MEMORY.md state.db(messages)
ATLAS-PROC-5D1E -> state.db(messages)
sk-atlas-FAKE-1a2b3c -> agent_a/memories/MEMORY.md state.db(messages)
```
Where agent_b's markers landed:
```
BERLIN-NOVA-SEM8C2B -> agent_b/memories/MEMORY.md 
NOVA-EPI-3D7F -> agent_b/memories/MEMORY.md 
NOVA-PROC-6A4B -> 
nova-FAKE-9z8y7x -> state.db(messages)
```

