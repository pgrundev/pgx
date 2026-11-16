# Temporary databases with TTL

A PGBoxDB database can be given a **time-to-live**: after it expires, a background
worker deletes it automatically. This makes disposable branches for agents, CI
jobs and feature work safe to create freely — they clean themselves up.

```bash
pgboxdb create agent-001 --ttl 5m      # expires 5 minutes after creation
pgboxdb clone myapp feature-x --ttl 1h # a disposable clone
pgboxdb branch myapp agent-1           # a clone for one agent: 2h TTL unless --ttl says otherwise
pgboxdb ttl agent-001 30m              # change the TTL
pgboxdb ttl agent-001 unlimited        # make it permanent
pgboxdb ttl agent-001                  # show the current TTL
pgboxdb ls                             # NAME, STATUS, TTL, EXPIRES IN, SIZE
```

## Idle TTL for agent branches

Branches made by `pgboxdb agent` (the Skill, the hooks, `pgboxdb agent exec`) use the
TTL as an **idle** timeout: every prompt and shell command the agent runs
moves the deadline to now + TTL (default `2h`, `idle_ttl` in `.pgboxdb.toml`), so
a three-hour session stays alive while a forgotten one still disappears. When
the session ends the branch is *retired*: its deadline becomes now + the
grace period (default `10m`, `grace_period`), long enough for a resume to find
its data. `pgboxdb agent ls` shows `active`, `retired` and `expired` states.

## Durations

`30s`, `5m`, `15m`, `1h`, `6h`, `24h`, `7d` — any positive whole number with a
unit of `s` (seconds), `m` (minutes), `h` (hours) or `d` (days), up to a maximum
of `3650d`. `unlimited` (also `none`/`never`) means no expiry. Zero, negative,
fractional, unit-less and over-cap values are rejected.

## Unlimited is the default — and never inherited

`pgboxdb create myapp` and `pgboxdb clone myapp x` create **permanent** databases: no
TTL, never auto-deleted. A TTL applies only when you pass `--ttl`. The one
exception is `pgboxdb branch`, whose default is `--ttl 2h` (pass `--ttl unlimited`
for a permanent branch). A clone never
inherits its source's TTL — you set it explicitly or it is unlimited. There is
no metadata record for a permanent database, so nothing can ever mark it for
deletion.

The default `app` database created by `pgboxdb run` is permanent unless you set a
TTL on it yourself.

## When the clock starts

- `pgboxdb create x --ttl 5m` → expires at **creation time + 5m**.
- `pgboxdb ttl x 1h` → expires at **the time you run it + 1h** (extends or shortens
  from now, not from creation).

Deadlines are stored as UTC Unix timestamps, so they are timezone-independent.

## While PGBoxDB is stopped

Deadlines keep advancing while PGBoxDB is stopped — wall-clock time still passes.
Automatic deletion does not happen while the runtime is down (there is nothing
running to do it), but **cleanup resumes automatically the next time PGBoxDB
starts**: the worker's first sweep removes anything already past its deadline.

## The cleanup worker

`pgboxdb start`, `pgboxdb run` and `pgboxdb serve` launch a small background supervisor
(one per runtime, enforced by a lock). It polls every 5 seconds by default
(`PGBOXDB_TTL_INTERVAL` seconds to change it), so an expired database is removed
within roughly its TTL plus one poll interval. The worker exits when the
runtime stops and is relaunched on the next start. It uses negligible resources
(about 2 MB RSS, idle between polls).

`pgboxdb run` exits after starting everything — the worker keeps running without a
terminal open.

## Active connections

A TTL deadline is authoritative. When a temporary database expires, the worker
terminates **that database's** sessions (never any other database's, never the
shared runtime), then drops it with `DROP DATABASE ... WITH (FORCE)`. Treat a
TTL-managed branch as ephemeral: do not keep long-lived work in it past its
deadline. Extending the TTL (`pgboxdb ttl x <longer>`) before expiry cancels the
deletion; setting it to `unlimited` makes the database permanent.

## Safety

A database is deleted automatically only when **all** of these hold: it had an
explicit TTL, the deadline has passed, the metadata is valid, the recorded
database OID still matches (a dropped-and-recreated name is a different
incarnation and is never touched), it belongs to the current cluster, and it is
not a protected system database (`postgres`, `template0`, `template1`). Missing
or corrupt metadata is never treated as permission to delete. Cleanup is
idempotent and crash-safe: interrupted at any point, the next sweep either
finishes the drop or finds the database already gone and finalizes.

## Inspecting and diagnosing

- `pgboxdb ls` / `pgboxdb ls --json` show each database's TTL and remaining time.
- `pgboxdb ttl <db>` shows one database's TTL (JSON with `--json`:
  `{ "name", "ttl", "ttl_seconds", "expires_at", "expires_in_seconds" }`;
  `expires_at` is `null` for unlimited).
- `pgboxdb doctor` reports whether the supervisor is running, how many temporary
  databases exist, how many are past their deadline awaiting cleanup, any
  unreadable TTL records, and any failed cleanups (with the last error).
