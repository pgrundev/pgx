# The `pgboxdb agent` lifecycle and `.pgboxdb.toml`

## Identity

A session is `(project, host, session id)`. The project is the canonical root
holding `.pgboxdb.toml` (or the git top level, or the directory itself); the host
is `claude`, `cursor`, `codex` or `exec`; the session id comes from the host
(`session_id` in hook payloads, `CLAUDE_CODE_SESSION_ID` / `CODEX_SESSION_ID`
in commands) or from `--id`. Two projects on one machine can both use a
source called `app` without their sessions colliding, because the project is
part of the identity.

A subagent is `(project, host, session id, agent id)`: the host's own id for
it (Claude Code's `agent_id`, present in `SubagentStart`, `SubagentStop` and
the tool hooks a subagent triggers; `--agent` or `PGBOXDB_AGENT_ID` in commands).
Every subagent of a session is its own identity and its own branch; the
session itself (no agent id) keeps the identity it always had, so branches
and records from before subagents existed still match. Names:
`agent-<host>-<session8>` for a session, `agent-<host>-<session8>-<agent8>`
for a subagent (eight readable characters of each id; Claude's `agent-`
prefix is skipped); when a short name is taken by something else, a 16-hex
hash of the full identity is appended, so uniqueness never rests on the
visible characters. The longest name is 57 bytes.

## Commands

| Command | Role |
|---|---|
| `pgboxdb agent setup [--source <db>] [--hosts …] [--scope project\|local\|user] [--no-hooks]` | One-time: runtime, template check, `.pgboxdb.toml`, hooks, proof branch, hook self-test. Idempotent. |
| `pgboxdb agent doctor` | Config, runtime, supervisor, source idle, hooks per host (missing/partial/duplicate), the current session's branch and `DATABASE_URL`, branch counts (subagents included), the hook log. Exit 1 on failure. |
| `pgboxdb agent up [--env] [--agent <id>]` | Ensure this session's (or subagent's) branch (create or extend). The hook calls it. |
| `pgboxdb agent touch [--agent <id>]` | Activity: deadline = now + idle TTL (max once per 30 s, no connection). |
| `pgboxdb agent down [--now] [--agent <id>] [--branch <name>]` | Session end: terminate the branch's backends, then retire (grace) or drop now. Only this session's branch; with `--branch`, only if that is the branch. |
| `pgboxdb agent env [--agent <id>]` | `export` lines for an existing branch: `eval "$(pgboxdb agent env)"`. |
| `pgboxdb agent exec [--id <s>] [--agent <id>] [--keep] -- cmd` | Wrapper: own branch for the command (dropped at exit; `--keep`: left active for the idle TTL, reattached by the next `exec` with the same id), or attach to the surrounding Claude/Codex session or routed subagent command. |
| `pgboxdb agent ls` | Agent branches, session and subagent, with `state` and `deadline_kind` (`idle_ttl` or `grace`). |
| `pgboxdb agent hook` | What hook files run; detects the host and event from the payload. |
| `pgboxdb agent install/uninstall [hosts] [--scope …]` | Merge/remove our hook entries; other entries untouched. |

`--id` without `--host` (for `touch`, `down`, `env`) finds the branch under
whichever host label it was made with; an id that exists under two hosts
needs `--host`.

All commands take `--json` (`pgboxdb --json agent …`) and use the stable exit
codes: 2 usage, 3 not initialised/stopped, 4 conflict, 5 not found, 6 source
busy.

## `.pgboxdb.toml`

```toml
# .pgboxdb.toml — PGBoxDB agent configuration for this project (safe to commit).
[agent]
source = "myapp_template"   # the PGBoxDB database every agent session branches from
idle_ttl = "2h"             # a branch expires this long after the agent's last activity
grace_period = "10m"        # how long a branch survives after its session ends
# Optional server-side guards every agent branch gets (ALTER DATABASE … SET);
# "0" switches one off. Defaults shown.
# connection_check_interval = "5s"     # a backend whose client died aborts within this
# idle_in_transaction_timeout = "15m"  # an open transaction nobody drives is aborted
# statement_timeout = "1h"             # no single statement runs longer
```

Names and durations only. A connection URL in it is rejected. Resolution
order for the source everywhere: `--source` > `PGBOXDB_SOURCE` > `.pgboxdb.toml`
(walking up from the session's directory, then the host's project root) >
`app`. The default `app` is created empty when missing; a named source that
is missing is an error with the `pgboxdb copy` hint.

The guards exist because a client and its backend have independent
lifetimes: when an agent is killed in the middle of `BEGIN; …; SELECT
pg_sleep(…); COMMIT;` PostgreSQL by default runs the statement string to its
end and commits minutes later. With `client_connection_check_interval` set,
the backend notices the dead client inside the running statement and aborts
the transaction instead. They are set on the branch only; the source is
never altered.

## Hook files `setup`/`install` write

The command is guarded, so a committed hook file is a silent no-op for a
teammate without PGBoxDB:

```
/bin/sh -c 'PATH="$PATH:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin"; command -v pgboxdb >/dev/null 2>&1 && exec pgboxdb agent hook; exit 0'
```

- Claude Code `.claude/settings.json` (`--scope local`: `settings.local.json`;
  `user`: `~/.claude/settings.json`): `SessionStart` (30 s), `UserPromptSubmit`
  (5 s), `PreToolUse` matcher `Bash` (15 s: a subagent's first command may
  clone), `SubagentStart` (30 s), `SubagentStop` (10 s), `SessionEnd` (10 s).
  An older four-entry installation is completed by `pgboxdb agent install
  claude` (nothing is duplicated).
- Cursor `.cursor/hooks.json` (`user`: `~/.cursor/hooks.json`): `sessionStart`,
  `beforeSubmitPrompt`, `afterShellExecution`, `sessionEnd`. Touch hooks sit on
  non-permission events so they never influence approvals.
- Codex `.codex/hooks.json` (`user`: `~/.codex/hooks.json`): `SessionStart`,
  `UserPromptSubmit`, `SessionEnd`. Codex project hooks load for trusted
  projects and must be trust-pinned once (`/hooks` in Codex).

Existing entries (other tools' hooks, the user's own) are preserved
byte-for-byte; `install` adds ours where missing, `uninstall` removes only
ours and never deletes `.pgboxdb.toml`.

## Timing and failure

- Idle TTL default 2h, grace 10m. A session that runs for hours stays alive
  because every prompt and shell command touches it.
- Hooks never exit 2 (blocking). On failure they print one line to stderr
  and, when `.pgboxdb.toml` exists, fail closed: `DATABASE_URL` becomes
  `postgres://pgboxdb-isolation-failed@127.0.0.1:1/pgboxdb_isolation_failed` and
  `PGBOXDB_ISOLATION=failed` with the reason in `PGBOXDB_ISOLATION_REASON`. For a
  subagent's Bash command the equivalent is a `deny` answer with the reason;
  and the wrapper a routed command runs through (`pgboxdb agent exec --agent`)
  exits without running anything if it cannot attach to the branch.
- Lifecycle states: `active` (an owner is alive; the deadline is the idle TTL
  after its last activity), `retired` (the owner ended; the deadline is the
  grace period, a resume revives it), `expired` (past the deadline, awaiting
  the supervisor). `down` terminates the branch's backends before retiring or
  dropping and reports `backends_terminated`/`backends_remaining`: after it
  returns, nothing that was running on the branch commits. A backend already
  past the point of no return of a COMMIT (WAL flushed) still completes it,
  which is why the guarantee starts when `down` returns, not when the client
  died; the `.pgboxdb.toml` guards cover the window before that.
- Events a host never delivers (a hard-killed session emits no `SessionEnd`,
  a cancelled subagent may emit no `SubagentStop`): the idle TTL still
  expires the branch and the supervisor drops it with `WITH (FORCE)`; the
  guards abort what the dead client left running long before that.
- Concurrency: a per-session lock serialises duplicate deliveries (Cursor
  loads Claude hook files too); the database's own uniqueness check is the
  last line. Two simultaneous starts for one session yield one branch.
- Crash recovery: every operation is safe to repeat. A record whose database
  vanished is forgotten and a fresh branch made (the context line says
  "fresh"); a database whose record is gone is just a database with no TTL
  owner and is never dropped by `down`.
