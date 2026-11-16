# `pgboxdb` CLI reference

`pgboxdb [--json] <command> [arguments]`. `--json` makes a command print one JSON
object (or array) on stdout; errors print `{"error":{...}}`. Data lives under
`$PGBOXDB_HOME` (default `~/.pgboxdb`).

## Quick start

| Command | What it does |
|---|---|
| `pgboxdb run` | One command to a ready database. Initialise `$PGBOXDB_HOME` if needed, start the runtime if it is not already up, ensure a default `app` database, and print its connection URL. Idempotent and safe to repeat; it never resets or overwrites an existing database. `--json` prints one object (for agents). The first run reads `PGBOXDB_PORT` / `PGBOXDB_DURABILITY` / `PGBOXDB_PASSWORD`; afterwards they come from the saved config. |

```console
$ pgboxdb run
PGBoxDB is ready.

  postgres://postgres:…@127.0.0.1:5432/app

  database   app (created)
  runtime    started (pid 23469, port 5432)
  home       /home/you/.pgboxdb

Connect:
  psql "postgres://postgres:…@127.0.0.1:5432/app"
```

## Runtime

| Command | What it does |
|---|---|
| `pgboxdb init [--port N] [--durability ephemeral\|full] [--force]` | Create the cluster under `$PGBOXDB_HOME` from the bundled base cluster, pick a free port, set a random superuser password, write the config. `--force` recreates (deletes every database). |
| `pgboxdb start` | Start the runtime in the background (idempotent: a second start reports "already running"). |
| `pgboxdb serve` | Run the engine in the **foreground** (init first if needed); the Docker/systemd entrypoint. |
| `pgboxdb stop [--force]` | Fast shutdown (SIGINT); `--force` escalates to SIGKILL after the timeout. |
| `pgboxdb restart` | Stop then start. |
| `pgboxdb status` | Running?, pid, port, databases, connections, uptime, memory, libraries. Exit 3 when stopped. |

## Databases

| Command | What it does |
|---|---|
| `pgboxdb create <db> [--ttl <dur>]` | Create an empty database. `--ttl` makes it temporary (auto-expires); default is permanent. |
| `pgboxdb drop <db> [--force]` | Drop it; without `--force`, exit 6 if sessions are connected (data untouched); `--force` terminates them first. |
| `pgboxdb clone <src> <dst> [--ttl <dur>]` | Copy-on-write clone (`CREATE DATABASE … STRATEGY = FILE_COPY`); shares blocks on APFS/ZFS/reflink filesystems. TTL is never inherited from the source. |
| `pgboxdb branch <source> <name> [--ttl <dur>]` | A disposable database for one agent: the same clone, with a **2h default TTL** (`--ttl 30m`, or `--ttl unlimited` for a permanent branch) and the connection URL printed as the last line, so `DATABASE_URL="$(pgboxdb branch myapp agent-1 \| tail -1)"` works. Refuses an existing name (exit 4) and a missing source (exit 5) without changing anything. `--json` returns `branch`, `source`, `url`, `socket_url`, `ttl`, `ttl_seconds`, `expires_at`, `expires_in_seconds`, `method`, `elapsed_ms`. |
| `pgboxdb ls` | List databases with status, TTL, remaining time and size. |
| `pgboxdb ttl <db> [<dur>\|unlimited]` | Show a database's TTL, or set/extend/disable it. See [ttl.md](ttl.md). |
| `pgboxdb url <db> [--socket]` | Print the connection URL (TCP with password, or Unix socket). |

```console
$ pgboxdb branch myapp agent-1
✓ Branch created: agent-1
✓ Source: myapp
✓ Expires in: 2 hours

DATABASE_URL:
postgres://postgres:…@127.0.0.1:5433/agent-1
```

## Agents: one isolated database per coding-agent session

The `pgboxdb agent` group is what the [PGBoxDB Skill](../../../skills/pgboxdb/SKILL.md)
and the host hooks call; it is also a complete manual interface. Details and
the per-host compatibility matrix are in [agents.md](agents.md).

| Command | What it does |
|---|---|
| `pgboxdb agent setup [--source <db>] [--hosts claude,cursor,codex] [--scope project\|local\|user] [--ttl <dur>] [--no-hooks]` | One-time project setup: initialise and start PGBoxDB if needed, make sure the source template exists and is idle, write `.pgboxdb.toml`, install lifecycle hooks for the detected hosts, create and remove one real branch as proof, and self-test that the hook can find `pgboxdb`. Idempotent. |
| `pgboxdb agent doctor` | Check `.pgboxdb.toml`, the runtime and supervisor, the source (exists, idle), each host's hook files (missing, partial, duplicated), the current session's branch and whether `DATABASE_URL` names it, and the agent branches overall. Exit 1 on any failure. |
| `pgboxdb agent exec [--source <db>] [--ttl <dur>] [--keep] [--id <s>] [--agent <id>] [--host <h>] -- <command> [args…]` | Run any command with its own branch in `DATABASE_URL` (and `PGHOST`/`PGPORT`/`PGUSER`/`PGPASSWORD`/`PGDATABASE`, `PGBOXDB_BRANCH`, `PGBOXDB_SOURCE`, `PGBOXDB_AGENT_SESSION`; the password only ever travels in the environment). Forwards SIGINT/SIGTERM/SIGHUP, keeps the idle TTL alive, exits with the command's status (128+signal if killed), then drops the branch; `--keep` leaves it active for its idle TTL and the next `exec` with the same `--id` attaches to it. Explicit-id branches are labelled `exec` whatever the command. Inside a Claude Code or Codex session (or a routed subagent command, which carries `PGBOXDB_AGENT_ID`) it attaches to that branch instead and leaves it alone. `--json`: `branch`, `session_id`, `agent_id`, `host`, `source`, `created`, `fresh`, `method`, `elapsed_ms` (null unless created), `exit_code`, `attached`, `dropped`, `status` (`attached`, `active`, `dropped`). |
| `pgboxdb agent ls` | Agent branches: name, host, session, agent, source, state (active, retired, expired), expires-in, size; `--json` adds `deadline_kind` (`idle_ttl` or `grace`). |
| `pgboxdb agent up [--id <s>] [--host <h>] [--agent <id>] [--source <db>] [--ttl <dur>] [--env] [--cwd <dir>]` | Ensure this session's (or subagent's) branch: create it (a clone of the source with the idle TTL and the `.pgboxdb.toml` guards applied) or extend the existing one. `--env` prints `export` lines. Session id and host default to the surrounding agent's variables (`CLAUDE_CODE_SESSION_ID`, `CODEX_SESSION_ID`), the agent id to `PGBOXDB_AGENT_ID`. Refuses a busy source (exit 6) and a missing named source (exit 5); the default source `app` is created empty when absent. |
| `pgboxdb agent touch [--id <s>] [--host <h>] [--agent <id>]` | Activity: move the deadline to now + idle TTL (at most once per 30 s; no connection). |
| `pgboxdb agent down [--id <s>] [--host <h>] [--agent <id>] [--branch <name>] [--now \| --grace <dur>]` | Session end: terminate the branch's backends (the cancellation barrier; reported as `backends_terminated`/`backends_remaining`), then retire the branch (grace period, default 10m) so a resume finds it, or `--now` drop it. Only a branch recorded for that exact session (and subagent) is ever touched; `--branch` refuses to act unless that is the branch. `--id` without `--host` finds the branch under whichever host label it has. |
| `pgboxdb agent activity --state working\|migrating\|testing\|completed\|failed [--task <label>] [--id <s>] [--host <h>] [--agent <id>] [--cwd <dir>]` | Report this session's (or subagent's) task phase for `pgboxdb top`'s TASK column. Attaches only to the branch the identity verifiably owns (unexpired record, live OID in this cluster: exit 5 / 4 otherwise); inside a host session an `--id` for another session is refused (exit 4). `--task`: ≤ 80 characters of plain text. Stored in `$PGBOXDB_HOME/top/phases/` (0600); never changes the branch, its TTL or its lifecycle. Hooks never report a phase. See [top.md](top.md#reporting-a-task-phase-agents-and-ci). |
| `pgboxdb agent env [--id <s>] [--host <h>] [--agent <id>] [--cwd <dir>]` | The `export` lines for an existing session's branch; `eval "$(pgboxdb agent env)"`. |
| `pgboxdb agent hook [--host claude\|cursor\|codex]` | The command the hosts' hook files run: reads the host payload on stdin, detects the host and event, and answers in its dialect (Claude: `CLAUDE_ENV_FILE` exports plus a context line on `SessionStart`; `additionalContext` on `SubagentStart`; `updatedInput` (the command rewritten to `<pgboxdb> agent exec --agent <id> --cwd <project> -- <shell> -c '<command>'`) or `permissionDecision: deny` on a subagent's `PreToolUse` for Bash; Cursor: `{"env":…,"additional_context":…}`; Codex: `additionalContext`). Never exits 2. Fails closed with an unreachable sentinel `DATABASE_URL` (session) or a refusal (subagent command) when the project has `.pgboxdb.toml` and no branch could be made. Logs every delivery to `~/.pgboxdb/agent/hooks.log`. |
| `pgboxdb agent install [claude] [cursor] [codex] [--scope …] [--source <db>]` / `uninstall` | Merge our entries into (or remove them from) `.claude/settings.json`, `.cursor/hooks.json`, `.codex/hooks.json` (or the `--scope local`/`user` files), leaving every other entry untouched. |

## Git: one isolated database per Git branch

`pgboxdb git` connects the Git lifecycle to PGBoxDB databases: hooks give every
branch (per worktree) a copy-on-write database cloned from the latest
validated snapshot of the main database. Full guide, configuration and
semantics: [git.md](git.md).

| Command | What it does |
|---|---|
| `pgboxdb git init [--source <db\|postgres-url>] [--main <branch>] [--schema-only\|--with-data] [--no-hooks]` | Set up the repository: initialise/start PGBoxDB, create (never overwrite) the main database (empty, a clone of a PGBoxDB database, or a read-only import that is schema-only unless `--with-data`), publish main snapshot v1, add the `[git]` tables to `.pgboxdb.toml`, install `post-checkout`/`post-merge`/`post-rewrite` (non-destructively), map the current branch. Safe to rerun. No repository: exit 5. |
| `pgboxdb git status` | Project, Git branch, worktree, database and readiness, main database, main snapshot vs branch baseline, sync state (main changed, pending migrations, stale branch, last failure), agent isolation. Read-only. |
| `pgboxdb git ls` | `BRANCH DATABASE BASE STATUS WORKTREE`; status `current`, `ready`, `stale`, `abandoned`, `missing`; the snapshots. `--json`: `branches`, `snapshots`, `current_generation`. |
| `pgboxdb git sync [--force] [--migrate]` | Publish a new immutable main snapshot when main's schema or migration bookkeeping changed (validated, atomic; a busy main is copied logically, never terminated). `--migrate` first runs `[git.migrations] command`. Status `published`, `unchanged`, `pending` (migrations committed on main but not applied), `failed` (exit 1; the previous snapshot stays). Existing branch databases are never changed. |
| `pgboxdb git env [--shell bash\|zsh\|fish] [--reveal]` | The current branch's `DATABASE_URL`, `PG*`, `PGBOXDB_BRANCH`, `PGBOXDB_GIT_BRANCH`, `PGBOXDB_WORKTREE`, `PGBOXDB_SOURCE`, `PGBOXDB_ISOLATION`. The password is masked in human and `--json` output (`"redacted": true`) unless `--reveal`; `--shell` prints complete `export` lines for `eval`. Inside an agent session: the session's own branch. |
| `pgboxdb git exec -- <command> [args…]` | Run a command with those variables (the process is replaced: exit status and signals are the command's). Refuses to run when the database cannot be resolved. Inside an agent session: `pgboxdb agent exec` semantics. |
| `pgboxdb git reset --from-main [--force]` | Recreate the current branch's database from the latest main snapshot (asks; `--force` for scripts). Refuses main and protected branches (exit 2). |
| `pgboxdb git prune [--apply] [--grace <dur>]` | Databases of deleted branches and removed worktrees idle past `abandoned_ttl` (default 7d), and retired snapshots. Dry run unless `--apply`. Never main, protected branches, agent databases, busy or recreated databases, or anything PGBoxDB Git did not create. |
| `pgboxdb git doctor` | Repository, config, runtime, state, hooks, shell integration, main database, snapshot (exists, OID, locked), schema/migration sync, mapping collisions, missing and unmapped databases, agent integration, live-copy tools. Exit 1 on a failure. |
| `pgboxdb git uninstall [--shell]` | Remove PGBoxDB's hook blocks (and hook files it created); `--shell` also the shell integration. Databases and `.pgboxdb.toml` are kept. |
| `pgboxdb git shell <bash\|zsh\|fish>` / `pgboxdb git install-shell [<sh>]` | Print the prompt integration / add it (one marked block) to the shell's rc file. |
| `pgboxdb git hook <name> [args…]` | What the installed hooks run. Never fails Git. |

## Monitor: `pgboxdb top`

A live, read-only terminal view of the runtime, its databases and the agent
sessions that own them. Full guide, semantics and JSON schema: [top.md](top.md).

| Command | What it does |
|---|---|
| `pgboxdb top [--interval-ms N] [--no-color] [--project <filter>]` | Interactive monitor (needs a TTY; exit 2 otherwise, pointing to `--snapshot`): project → Git branch → session → subagent tree with DB state (connections), lifecycle, explicit task phase, connections, logical size, age and TTL countdown; details pane, search, sort, flat/agents-only views, pause. Refresh 250–10000 ms (default 1000). No key changes anything. Exit 3 (and nothing created) when PGBoxDB is not initialised. |
| `pgboxdb top --demo` | The same view on a deterministic simulated five-agent session, labelled `DEMO / SIMULATED`; needs no PGBoxDB home and touches no runtime. |
| `pgboxdb [--json] top --snapshot [--demo] [--project <filter>]` | One collection: plain text, or with `--json` one object (`schema_version` 1: `runtime`, `summary`, `databases` — `null` when the catalog is unavailable — and `warnings`). Exit 0 running (or demo), 3 not initialised/not running. `pgboxdb --json top` without `--snapshot` is a usage error. |

## Import from PostgreSQL

| Command | What it does |
|---|---|
| `pgboxdb copy --from <postgres-url> --to <db> [--schema-only] [--jobs N] [--verbose] [--no-optimize]` | Copy an existing PostgreSQL database into PGBoxDB as a template to clone from: `pg_dump` (read-only on the source) → compatibility gate → `pg_restore --exit-on-error` into a staging database → validate → `VACUUM (FREEZE, ANALYZE)` → atomic rename to `<db>`. Refuses an existing destination (exit 4) and an incompatible source (exit 1) without creating anything. Needs `pg_dump`/`pg_restore` (`PGBOXDB_PG_BINDIR`, `PATH`, or the usual install dirs). See [copy.md](copy.md). |

Temporary databases (`--ttl`) are deleted automatically by a background worker;
permanent databases are never auto-deleted. Full semantics: [ttl.md](ttl.md).

## Tools

`pgboxdb doctor` checks the install, data directory, config mode, socket-path
length, port, runtime, disk, stack limit, filesystem/block-cloning, libxml2/ICU,
the PostgreSQL client tools `pgboxdb copy` would use, and orphan `base/<oid>`
directories (read-only; never deletes). `pgboxdb version`
reports the engine commit and PostgreSQL compatibility. `pgboxdb help`.

## Database names

1–63 bytes, `^[A-Za-z_][A-Za-z0-9_-]*$`, always sent quoted; `postgres`,
`template0`, `template1` cannot be created, dropped or cloned onto.

## Exit codes

`0` ok · `1` engine/runtime error · `2` usage · `3` not initialised / not
running · `4` conflict (exists, running, port in use) · `5` not found · `6`
database in use · `7` corrupted or incomplete installation.

## Environment

`PGBOXDB_HOME` (data home), `PGBOXDB_RUN_DIR` (socket directory, for a long `$PGBOXDB_HOME`),
`PGBOXDB_PORT` / `PGBOXDB_DURABILITY` / `PGBOXDB_LISTEN` / `PGBOXDB_PASSWORD` (read by `init` and
`serve`, used by the Docker entrypoint), `PGBOXDB_PG_BINDIR` (where `pgboxdb copy` finds
`pg_dump` and `pg_restore`).
