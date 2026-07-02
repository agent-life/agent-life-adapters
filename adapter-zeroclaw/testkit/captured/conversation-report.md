# ZeroClaw — multi-agent memory conversation

_Model: us.amazon.nova-lite-v1:0. ONE shared brain.db; rows scoped only by agent_id._
_Detailed transcripts: captured/home/logs/conversation/ (gitignored)._

## ZeroClaw

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
conversation | user_msg_2864a9b9-2eb1-4c4c-865d-f711ce25a19b | Record an episodic log entry for TODAY: at 14:00 you co
conversation | user_msg_3a6c3abf-ea13-42d0-a1ba-c2e8bc7b2904 | Keep this credential for me — it is a throwaway TEST va
conversation | user_msg_d56eb51d-d495-4045-a620-bce21d096554 | Save the following as a reusable, named PROCEDURE calle
conversation | user_msg_e279449b-c158-42f6-b5f9-220e19ab431d | Permanently remember these durable facts in your long-t
core | ATLAS-PROC-5D1E | 1) run the full test suite 2) tag the release 3) push t
core | CAPETOWN-ATLAS-SEM7F3A | You are Atlas, an infrastructure assistant. Your primar
core | STAGING_API_TOKEN | sk-atlas-FAKE-1a2b3c-DO-NOT-USE
daily | log_entry_2026-07-02_14:00 | At 14:00 on 2026-07-02, completed the Q2 Postgres-to-Ne
```
Where agent_b's markers landed:
```
conversation | user_msg_5378f4b3-3c0c-42d0-8b73-ca7c5178607c | Save the following as a reusable, named PROCEDURE calle
conversation | user_msg_67f61f51-e87d-40f3-bf1c-aae63241db60 | Keep this credential for me — it is a throwaway TEST va
conversation | user_msg_a0aa5cbc-ff77-4744-ae02-f1af85f0d7cf | Permanently remember these durable facts in your long-t
conversation | user_msg_ace08c2b-9401-4146-8269-3ee8de8c5a5f | Record an episodic log entry for TODAY: you reviewed 12
core | BERLIN-NOVA-SEM8C2B | Nova is a research assistant. Primary user is Mara, bas
core | NOVA-PROC-6A4B | 1) gather the week's notes, 2) cluster them by theme, 3
core | RESEARCH_DB_PASSWORD | nova-FAKE-9z8y7x-DO-NOT-USE
episodic | episodic_log_2026_07_02 | Reviewed 12 papers on retrieval-augmented generation an
```

