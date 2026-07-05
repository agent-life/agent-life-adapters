# OpenClaw — multi-agent memory conversation

_Model: us.amazon.nova-lite-v1:0. Detailed transcripts: captured/home/logs/conversation/ (gitignored)._

## OpenClaw

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
CAPETOWN-ATLAS-SEM7F3A -> workspace-agent_a/MEMORY.md agents/agent_a/sessions/conv-agent_a.trajectory.jsonl agents/agent_a/sessions/conv-agent_a.jsonl 
ATLAS-EPI-2B9C -> workspace-agent_a/memory/2026-07-02.md agents/agent_a/sessions/conv-agent_a.trajectory.jsonl agents/agent_a/sessions/conv-agent_a.jsonl 
ATLAS-PROC-5D1E -> workspace-agent_a/procedures/deploy.md agents/agent_a/sessions/conv-agent_a.trajectory.jsonl agents/agent_a/sessions/conv-agent_a.jsonl 
sk-atlas-FAKE-1a2b3c -> workspace-agent_a/secrets/STAGING_API_TOKEN.md 
```
Where agent_b's markers landed:
```
BERLIN-NOVA-SEM8C2B -> workspace-agent_b/MEMORY.md agents/agent_b/sessions/conv-agent_b.jsonl agents/agent_b/sessions/conv-agent_b.trajectory.jsonl 
NOVA-EPI-3D7F -> workspace-agent_b/memory/2026-07-02.md agents/agent_b/sessions/conv-agent_b.jsonl agents/agent_b/sessions/conv-agent_b.trajectory.jsonl 
NOVA-PROC-6A4B -> workspace-agent_b/procedural_memory/NOVA-PROC-6A4B.md agents/agent_b/sessions/conv-agent_b.jsonl agents/agent_b/sessions/conv-agent_b.trajectory.jsonl 
nova-FAKE-9z8y7x -> workspace-agent_b/secrets/RESEARCH_DB_PASSWORD.md 
```

