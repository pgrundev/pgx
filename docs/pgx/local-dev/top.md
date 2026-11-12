# `pgx top`: a live view of PGX, its databases and your agents

`pgx top` is a native terminal monitor (Rust, [Ratatui](https://ratatui.rs) +
Crossterm) for the local PGX runtime. Open it in a second terminal while
coding agents work and it shows, on one dense screen, every database grouped
by project, Git branch and agent session, with live client connections, TTL
countdowns, lifecycle, logical size and — when an agent reports it — the
task phase. It refreshes every second and is **read-only**: no key drops,
resets, extends or creates anything.

```sh
pgx top                            # interactive (needs a terminal)
pgx top --demo                     # a simulated five-agent session; no PGX needed
pgx --json top --snapshot          # one collection as one JSON object
```

## What it looks like

Captured from the real binary on a PTY (`pgx top --demo`, 100×30, after
about 40 s of the simulated timeline). **Everything on this screen is
simulated**, which the `DEMO / SIMULATED` badge says; real mode never shows
that badge.

```text
 PGX TOP   DEMO / SIMULATED                                                   ● LIVE 1s   PGX 0.1.3
 Runtime: RUNNING  PID 73136  Port 5433   Uptime 00:42:59
 MEM [█░░░░░░░░░░░░░░░░░░░░░░░░] 194 MB RSS (whole runtime)   CPU n/a
 DATABASES 6   AGENT DBS 5   SESSIONS 1   CONNS 7   ISSUES 0                         sort:name tree
────────────────────────────────────────────────────────────────────────────────────────────────────
 PROJECT / SESSION / DATABASE                                  DB STATE  TASK      CONN   AGE    TTL
 ▾ remotely                                                    5 DB                   7
   ▾ ⎇ feature-auth  [remotely]                                5 DB                   7
     ▾ Claude Code · session 72ac4f1e  (no session db)         5 DB                   7
       ├─ agent-a · JWT                                        CONNECTED TESTING      2   40s  1h59m
       ├─ agent-b · OAuth                                      CONNECTED TESTING      3   37s  1h59m
       ├─ agent-c · API                                        CONNECTED WORKING      1   34s  1h59m
       ├─ agent-d · Schema                                     IDLE      WORKING      0   31s  1h59m
       └─ agent-e · Billing                                    CONNECTED TESTING      1   28s  1h59m
 ▾ Standalone / unowned                                        1 DB                   0
   └─ remotely_dev                                             IDLE      —            0    1d      ∞
────────────────────────────────────────────────────────────────────────────────────────────────────
 RECENT EVENTS
 12:00:36 ~ agent-claude-72ac4f1e-b reported TESTING (OAuth)
 12:00:32 ~ agent-claude-72ac4f1e-a reported TESTING (JWT)
 12:00:32 ▲ agent-claude-72ac4f1e-a receiving connections (2)
 12:00:30 ~ agent-claude-72ac4f1e-e reported TESTING (Billing)
 ↑↓ move  ⏎ info  t tree  a agents  / find  s sort  p pause  ? help  q quit
```

At 120 columns and wider the table adds LIFECYCLE, HOST, SIZE and TOUCH
(time since the last hook heartbeat). Below 60 columns or 16 lines it
degrades to a one-line header and a three-column table; nothing ever wraps
or prints outside its cell, and resizing is handled live.

## Three states, never conflated

| Column | Values | Comes from | Does **not** mean |
|---|---|---|---|
| DB STATE | `CONNECTED`, `IDLE`, `EXPIRING`, `DEL_RETRY`, `INVALID`, `UNKNOWN` | `pg_stat_activity` client backends (the monitor's own connection excluded), the TTL record's state, `datconnlimit = -2` | `IDLE` is "no connection right now", not "the agent is done" |
| LIFECYCLE | `ACTIVE`, `RETIRED`, `EXPIRED` | the agent's TTL record: `retired` (set by `SessionEnd`/`SubagentStop`/`pgx agent down`) and the deadline | `RETIRED` is "the owner ended", not "the task succeeded" |
| TASK | `WORKING`, `MIGRATING`, `TESTING`, `COMPLETED`, `FAILED`, or `—` | only an explicit `pgx agent activity --state …` report | never inferred from SQL traffic, hooks or session end |

Other values are labelled for what they are: **MEM** is the resident memory
of the whole runtime process (all databases together; there is no
per-database RAM), **SIZE** is the logical `pg_database_size`, not
copy-on-write disk usage (clones share blocks), **TOUCH** is the agent hook's
heartbeat, not query activity, and **CPU** is `n/a` because it is not
measured. Anything unavailable shows `n/a` or `—`, never `0`.

## The tree

```text
project                          (project_id: a hash of the project root)
  ⎇ Git branch  [worktree]       (PGX Git scope, when the session uses PGX Git)
    Host · session <id8>         (host + session id)
      session db <name>          the session's own branch, first
      ├─ <agent id> · <task>     one row per subagent branch
Git · <repository>               PGX Git databases with no agent owner
Standalone / unowned             everything else (templates, `pgx create`, `pgx branch`)
```

Group ids are built from identities (`p:<project>/g:<scope>/s:<host>:<session>`,
`db:<oid>`), never from row positions, so: two projects with the same session
id stay apart; two worktrees on the same Git branch stay apart; the same
session id under two hosts stays apart; selection and collapsed groups
survive refreshes, re-sorting, filtering and deletions. A session whose own
database has expired while subagents live on is shown as a synthetic group
marked `(no session db)` — no parent database is invented.

Ownership is attached only when the TTL record's **cluster identity and
database OID** match the live catalog row. A record for a database that was
dropped and recreated under the same name, or that belongs to another
cluster incarnation, is reported under ISSUES and the database is shown as
standalone; the monitor never deletes or repairs anything.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | move the selection (`PgUp` `PgDn`, `g` `G` first/last) |
| `←` `→` / `-` `+` | collapse / expand a group; `←` on a database selects its group |
| `Space` / `Tab` | toggle the selected group |
| `Enter` | details pane for the selection; `Esc` closes |
| `t` | tree ⇄ flat list |
| `a` | all databases ⇄ agent-owned only |
| `/` | case-insensitive search over database name, project, Git branch, host, session, agent id and task label; `Backspace` edits, `Enter` keeps, `Esc` cancels (another `Esc` clears) |
| `s` | sort: name → TTL deadline → age → connections (siblings sorted, hierarchy kept) |
| `p` | pause the display (collection and history continue; PGX is untouched); `p` again shows the latest |
| `r` | refresh now |
| `?` / `h` | key help |
| `q` / `Ctrl-C` | quit |

The details pane shows the database name and OID, role, project, host and
session id, agent id, Git branch and worktree, the clone source, creation
time, last hook touch, the TTL remaining with its deadline kind (`idle TTL
after last touch`, `grace (session ended)` or `fixed`), lifecycle,
connections, logical size, the reported task phase with its time and source,
and the TTL worker's last error.

## Options

| Option | |
|---|---|
| `--demo` | the simulator (below) instead of the live runtime |
| `--snapshot` | collect once and print (text; one JSON object with `--json`) |
| `--interval-ms N` | refresh interval, `250`..`10000` (default `1000`); anything else is a usage error (exit 2) |
| `--no-color` | monochrome (reverse video and bold only); `NO_COLOR` is honoured too |
| `--project <filter>` | only projects whose name or path contains the filter (or whose id starts with it) |

`pgx top` without `--snapshot` needs a terminal: with stdin or stdout not a
TTY it exits 2 and points to `--snapshot`. `pgx --json top` without
`--snapshot` is a usage error rather than escape codes on stdout. On a home
that was never initialised `pgx top` exits 3 and creates nothing; on a
stopped runtime it opens, shows `STOPPED`, and reconnects by itself when the
runtime starts.

## Reporting a task phase (agents and CI)

```sh
pgx agent activity --state working --task "Implement OAuth"
pgx agent activity --state migrating
pgx agent activity --state testing
pgx agent activity --state completed
pgx agent activity --state failed
```

Run it inside the agent's session (Claude Code and Codex sessions, and
commands routed to a Claude Code subagent, carry their identity in the
environment), or name it with `--id <session> [--host <h>] [--agent <id>]`
as for `pgx agent touch`. The report attaches only to the database this
session or subagent verifiably owns: the TTL record must exist and be
unexpired, and the live database must still have the recorded OID in this
cluster (exit 5 for no such branch, 4 for a mismatch). Inside a host session
an `--id` naming a different session is refused (exit 4). `--task` is
optional plain text, at most 80 characters, without control characters,
escape sequences or bidi overrides (exit 2 otherwise).

A report is stored in `$PGX_HOME/top/phases/` (directory 0700, one atomic
0600 file per agent identity) and **never touches the TTL record**: no
deadline, lifecycle or ownership changes. Parents and subagents report
independently. `pgx top` shows a report only while its database incarnation
and identity still match and the record has not expired. A retired session
may still report (for example `completed` from a CI step after the session
ended).

Nothing reports a phase implicitly. `SessionEnd` and `SubagentStop` retire
the branch (LIFECYCLE `RETIRED`) but never mark a task `COMPLETED`; an agent
that wants TESTING or COMPLETED shown says so. To opt an agent in, tell it
(in `CLAUDE.md`, `AGENTS.md` or the task prompt) to run
`pgx agent activity --state testing` before running the test suite and
`--state completed` or `--state failed` when it is done. Integrations that do
not report keep working unchanged and show `—`.

## The demo

`pgx top --demo` plays a deterministic 150-second timeline: one project
(`remotely`), one Git branch (`feature-auth`), one Claude Code session and
five subagents (JWT, OAuth, API, Schema, Billing) whose databases appear one
by one; explicit `WORKING`, `MIGRATING` and `TESTING` reports; connections
coming and going; one task `COMPLETED`, one `FAILED`; subagents retiring and
finally the whole session ending, while the TTLs count down. It reads no
PGX home, opens no connection and spawns nothing, so it works on a machine
without PGX; `pgx --json top --demo --snapshot` prints the fixed state at
second 40 with a fixed clock, byte-identical on every run, for tests.

## `--snapshot --json`

One object, the same collector and model the screen uses:

```json
{
  "schema_version": 1,
  "mode": "live",
  "simulated": false,
  "collected_at": "2026-11-11T20:00:00Z",
  "pgx_version": "0.1.3",
  "runtime": {"status": "running", "running": true, "pid": 1234, "port": 5433,
              "uptime_seconds": 2538, "rss_bytes": 201326592,
              "rss_scope": "whole runtime process", "cpu_percent": null,
              "runtime_status_available": true},
  "summary": {"databases": 7, "agent_databases": 6, "sessions": 1,
              "client_connections": 3, "issues": 0},
  "databases": [
    {"name": "agent-claude-72ac4f1e-a1b2c3d4", "oid": 16401, "role": "subagent",
     "project_id": "def7b3178476a1c0", "project": "remotely", "project_root": "/src/remotely",
     "git_branch": null, "git_scope": null, "worktree": null,
     "host": "claude", "session_id": "72ac4f1e-…", "agent_id": "agent-a1b2c3d4",
     "parent_database": "myapp_template", "connections": 2, "size_bytes": 7766016,
     "is_template": false, "created_at": "2026-11-11T19:48:00Z", "age_seconds": 720,
     "last_touch": "2026-11-11T19:59:40Z", "ttl": "2h", "ttl_seconds": 7200,
     "expires_at": "2026-11-11T21:59:40Z", "expires_in_seconds": 7180,
     "deadline_kind": "idle_ttl", "ttl_state": "active", "last_error": null,
     "db_state": "connected", "lifecycle": "active",
     "task_phase": {"state": "testing", "label": "Implement OAuth",
                    "reported_at": "2026-11-11T19:58:10Z", "source": "pgx agent activity"}}
  ],
  "warnings": []
}
```

- `databases` is `null` (and the `summary` counts are `null`) when the
  catalog could not be read — runtime stopped, unreachable, or not
  initialised — which is different from `[]`, no databases. Unknown metrics
  are `null`, never `0`.
- `role`: `session`, `subagent`, `git_main`, `git_branch`, `git_snapshot`,
  `standalone`. `parent_database` is the clone source of an agent branch.
- `db_state`: `connected`, `idle`, `expiring`, `delete_retry`, `invalid`,
  `unknown`. `lifecycle`: `active`, `retired`, `expired`, or `null` for
  databases without an agent owner. `deadline_kind`: `idle_ttl`, `grace`,
  `fixed`, or `null` for no deadline.
- `warnings[]`: `{severity: "issue"|"info", code, message, database}`. Codes
  include `ttl_record_unreadable`, `ttl_record_foreign_cluster`,
  `ttl_record_stale_oid`, `ttl_record_orphaned`, `ttl_delete_retry`,
  `ttl_last_error`, `git_state_unreadable`, `runtime_unreachable`,
  `catalog_query_failed` (issues) and `ttl_record_pending_cleanup`,
  `git_state_foreign_cluster`, `phase_records_ignored`,
  `size_sample_failed`, `not_initialized` (info). `summary.issues` counts the
  issues of this collection; it is not a cumulative error counter.
- Exit status: 0 when the runtime is running (and always for `--demo`), 3
  when it is not initialised or not running — the object is printed either
  way. Database order is by name; additive fields keep `schema_version` 1.
- No passwords, connection URLs, environment values or query texts appear in
  the JSON, the text form or the screen.

## Performance and footprint

The collector runs on its own thread with a bounded hand-off (one pending
snapshot), so a slow query never blocks input; the input loop wakes at least
every 100 ms. Per refresh it reads the catalog and client-connection counts
in **one** SQL statement over **one** persistent Unix-socket connection
(`application_name = pgx-top`), plus uptime and `pgrust_runtime_status()`;
logical sizes are sampled in one statement every 10 s, never per database
per second. TTL records, PGX Git state and phase files are read from disk;
no subprocess is spawned per refresh (RSS comes from `pgrust_runtime_status`,
`/proc/<pid>/status` or `proc_pidinfo`). Event history is capped at the last
100 events, in memory only. Measured numbers are in the PR that introduced
the command and in the unit tests' 1,000-database fixtures.

## Security

- Read-only: the monitor's SQL is `SELECT` only; it writes no file (not even
  under `$PGX_HOME`), never changes a TTL, and has no destructive keys.
- Terminal-safe: every label is stripped of control characters (including
  ESC/CSI, so no ANSI injection through a database or branch name) and bidi
  overrides, then clipped by display width.
- The terminal is restored (raw mode off, main screen, cursor visible) on
  `q`, `Ctrl-C`, SIGTERM/SIGHUP/SIGINT, errors and, best effort, panics.
- No network endpoints, daemons or telemetry.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `pgx top is interactive and needs a terminal` | stdin or stdout is a pipe or file: use `pgx --json top --snapshot` |
| `○ STOPPED` / empty table | the runtime is not running: `pgx start`; the view reconnects on its own |
| `○ DISCONNECTED` | the socket or a catalog query failed; the first ISSUE explains, the view retries every refresh |
| `◌ STALE` | no fresh collection for several intervals (a slow catalog read); the screen keeps the last good data |
| an agent's database shows as standalone | its TTL record does not match the live OID/cluster (recreated outside PGX); see ISSUES |
| TASK is always `—` | nothing reported a phase; see [Reporting a task phase](#reporting-a-task-phase-agents-and-ci) |
| garbled glyphs | use a UTF-8 terminal and font; `--no-color` for terminals without colour |
