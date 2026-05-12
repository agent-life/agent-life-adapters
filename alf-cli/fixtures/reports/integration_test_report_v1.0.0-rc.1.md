# ALF CLI Integration Test Report

**Schema Version:** v1.0.0-rc.1
**Timestamp:** 2026-05-11T15:42:31.906208473+00:00
**Status:** SUCCESS

## `alf validate` Output
```
{"ok":true,"valid":true,"errors":[],"warnings":[{"path":"credentials[0].encryption.algorithm","message":"Unknown encryption algorithm 'Hic ut ut architecto Enim nec dolor dui placeat a' — supported: xchacha20-poly1305, aes-256-gcm"},{"path":"credentials[1].encryption.algorithm","message":"Unknown encryption algorithm 'a' — supported: xchacha20-poly1305, aes-256-gcm"},{"path":"credentials[1].encryption.kdf","message":"Unknown KDF 'a repellendus sit magnam, exercitationem ipsum sit' — only argon2id is currently recognized"},{"path":"credentials[2].encryption.algorithm","message":"Unknown encryption algorithm 'tellus. placeat esse repellendus ipsum sit modi a' — supported: xchacha20-poly1305, aes-256-gcm"}]}

```

## `alf validate` Errors
```
Validating /home/johan/wa/personal/agent-life-adapters/alf-cli/fixtures/synthetic-agent.alf...

```
