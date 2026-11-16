# Live validation: one branch per Claude Code subagent (2026-10-26)

Branch `feat/subagent-isolation`, release build, Claude Code 2.1.310, macOS 14.4
arm64, PGX engine 33414b6f90 (PostgreSQL 18.6 compat). Project: a test
directory with `.pgx.toml` (`source = "car_center_template"`, a 7.7 MB
schema-only template), `pgx agent install claude` run inside an already
running session (the settings watcher picked the two new hooks up without a
restart). No subagent received an id, a database name, a connection string
or a `pgx` command; the hooks did everything.

## Before (PGX 0.1.1, same session, earlier the same day)

Three concurrent subagents using the documented `pgx agent exec -- …` path
all attached to the parent's branch `agent-claude-f775e29e`: three markers,
nine tables and three migrations in one database. Root cause: the branch was
keyed on `CLAUDE_CODE_SESSION_ID`, which every subagent inherits, and the
hooks did not handle `SubagentStart`/`SubagentStop` or the `agent_id` that
Claude Code puts in a subagent's `PreToolUse` payload. Full report:
`testpgx/PGX_AGENT_BENCHMARK.md` (Part 1) in the benchmark repository.

## After

| Phase | Result |
|---|---|
| 3 subagents (users / account / booking workloads) | `SubagentStart` created `agent-claude-f775e29e-{a64ccc14,a1317f9f,a45ff869}` in the same second; every Bash command routed to its own branch; each branch ended with exactly its own marker, migration and tables; all three retired by `SubagentStop` with the backend barrier |
| 20 subagents launched in one message | 20/20 created by `SubagentStart` (clone included: min 241, p50 1231, p95 1371, max 1401 ms, all twenty in the same instant); 20/20 workloads ran in their own branch (`current_database()`, `PGX_ROUTED=1`), 0 migration/seed failures, 0 contamination; 20/20 retired by `SubagentStop`; 37 s end to end; runtime RSS peak 92 MB; +161 MB disk, reclaimed after grace |
| Template / base | schema md5 identical to the pre-run baseline, 0 rows, after every phase |
| Killed subagent mid-transaction (TaskStop while in `pg_sleep` inside `BEGIN … COMMIT`) | backend gone within the 5 s `client_connection_check_interval` guard (`FATAL: connection to client lost`), 0 rows committed; before the fix the same scenario committed 2 rows 8 minutes later |

## Hook log excerpt (`~/.pgx/agent/hooks.log`)

```
22:39:36Z claude SubagentStart session=f775e29e-e73 agent=a64ccc148ea2ecf2e tool=- -> created agent-claude-f775e29e-a64ccc14
22:39:40Z claude PreToolUse session=f775e29e-e73 agent=a64ccc148ea2ecf2e tool=Bash -> routed to agent-claude-f775e29e-a64ccc14
22:51:10Z claude SubagentStop session=f775e29e-e73 agent=a64ccc148ea2ecf2e tool=- -> retired agent-claude-f775e29e-a64ccc14 (terminated 0 backend(s), 0 remaining)
22:52:33Z claude SubagentStart session=f775e29e-e73 agent=a59efc045bebc0c48 tool=- -> created agent-claude-f775e29e-a59efc04 in 1231 ms
22:52:36Z claude PreToolUse session=f775e29e-e73 agent=a59efc045bebc0c48 tool=Bash -> routed to agent-claude-f775e29e-a59efc04 in 190 ms
```

## Limitations observed

- **Permissions.** Claude Code evaluates permission rules and, in auto mode,
  its classifier on the rewritten command. In this auto-mode session the
  classifier refused 12 of the 18 commands of the three-agent phase (none of
  the twenty in the scale phase). The commands never ran anywhere. The
  remedy is the user's: an allow rule such as
  `Bash(/path/to/pgx agent exec:*)`, where `/path/to/pgx` is the absolute path the rewritten command starts with (for the installer, `~/.local/bin/pgx` written out; `pgx agent doctor` shows it); the rewrite starts with
  the `pgx` invocation so a prefix rule matches. PGX never answers `allow`.
- **A forcibly terminated subagent emits no `SubagentStop`.** Its branch
  stays active on the idle TTL; the guards protect the data; `pgx agent
  down --agent <id> --now` cleans it up by hand.
- **Helper agents.** Two short-lived internal Claude Code agents ran a Bash
  command and were given branches (created on first command). TTL-bounded;
  an `agent_type` filter would avoid the clones.
