# PGBoxDB for AI coding agents

**Every AI agent gets its own Postgres.** Install one Skill, and each Claude
Code, Cursor or Codex session that touches your database works in its own
disposable branch of a template, created in milliseconds, never shared, and
cleaned up on its own. Your real PostgreSQL is read once and never written.

```text
/plugin marketplace add pgrundev/pgboxdb        # Claude Code: the Skill as a native plugin, inside a session
/plugin install pgboxdb@pgrundev
```

```sh
npx skills add pgrundev/pgboxdb --skill pgboxdb     # any host (Claude Code, Cursor, Codex): the Skills CLI
npx pgboxdb@latest                            # PGBoxDB itself, if you prefer to install it by hand
```

Three channels, one Skill: the Claude Code plugin (native install, `/plugin`
manages updates and removal, skill name `/pgboxdb:pgboxdb`), the Agent Skills CLI
(copies the Skill into the project for every supported host, skill name
`/pgboxdb` in Claude Code) and the PGBoxDB CLI itself for manual use. The plugin and
the Skills CLI copy are the same files; see
[claude-plugin.md](claude-plugin.md) for the differences and for running
both.

Then open your agent in the project and ask it to set PGBoxDB up:

> Use PGBoxDB to give this project isolated databases for coding agents. Set everything up.

The agent (through the Skill) detects the project's database setup, installs
PGBoxDB if it is missing, imports your development database **schema-only** into
a template, runs `pgboxdb agent setup`, and verifies the result. From the next
session on, isolation is automatic on the hosts marked verified below. A
Skill installation on its own executes nothing; the agent performs setup when
the Skill is activated and the host's permissions allow it.

## Versions

`pgboxdb agent` and the Skill need **PGBoxDB 0.2.0 or newer**
([release](https://github.com/pgrundev/pgboxdb/releases/tag/v0.2.0): linux-amd64,
linux-arm64, darwin-arm64 with SHA-256 checksums), the first release under
the PGBoxDB name. It includes per-session branches, separate branches for
Claude Code subagents, and the Git integration. The Skill's
`scripts/check-pgboxdb.sh` verifies the installed binary from
`pgboxdb --json version` and refuses older ones with a hint naming what they
lack. npm `pgboxdb@0.2.0` follows the GitHub release through the
repository's staged publishing process.

## What you get

| | |
|---|---|
| Template | `<app>_template`, a schema-only (or, with your approval, full) copy of your PostgreSQL database, or an empty `app` for a new project. Nothing connects to it; it only gets branched. |
| Branch per session | `agent-<host>-<8 chars of the session id>`, a copy-on-write clone. Two sessions, two branches; a `claude --worktree` session gets its own. |
| Branch per subagent | `agent-<host>-<session8>-<agent8>`: every Claude Code subagent gets its own clone of the same source the moment it starts, and its Bash commands are routed to it. Twenty parallel subagents, twenty branches; the parent keeps its own. |
| Environment | `DATABASE_URL`, `PGHOST`, `PGPORT`, `PGUSER`, `PGPASSWORD`, `PGDATABASE`, `PGBOXDB_BRANCH`, `PGBOXDB_SOURCE`, `PGBOXDB_ISOLATION=ok`, present before the agent's first command (a subagent's commands additionally see `PGBOXDB_AGENT_ID` and `PGBOXDB_ROUTED=1`). |
| Lifetime | Idle TTL of 2 hours, extended by every prompt and shell command; a 10-minute grace period after the session or subagent ends, so a resume finds its data. |
| Cleanup | The PGBoxDB supervisor removes expired branches. `pgboxdb agent ls` shows them; `pgboxdb agent down --now` drops the current one. Ending a session or subagent terminates what it left connected to its branch, and every branch carries server-side guards (`client_connection_check_interval`, `idle_in_transaction_session_timeout`, `statement_timeout`) so a killed client's transaction aborts instead of committing later. |
| Fail closed | If a project has `.pgboxdb.toml` and a branch cannot be made, the session's `DATABASE_URL` is pointed at an unreachable sentinel (`PGBOXDB_ISOLATION=failed`) instead of silently inheriting another database; a subagent's Bash command in that situation is refused with the reason. |

## Host compatibility

| Host | Automatic isolation | How | Verified |
|---|---|---|---|
| **Claude Code** | Yes, sessions and subagents | `SessionStart` hook writes the exports to `CLAUDE_ENV_FILE`; `UserPromptSubmit`/`PreToolUse` extend the TTL; `SessionEnd` retires the branch. `SubagentStart` clones a branch per subagent, `PreToolUse` rewrites each subagent Bash command to run through `pgboxdb agent exec --agent <id>` (`updatedInput`; refused with `deny` when the branch cannot be ensured; the rewritten command still passes the host's permission flow), `SubagentStop` retires it. | **Live, 2026-10-25**: setup from an existing PostgreSQL, feature work in a fresh session, resume within grace, two concurrent sessions. [validation note](validation/2026-10-25-agent-live-claude.md). **2026-10-26**: the same through the native plugin (`/plugin install pgboxdb@pgrundev`). [plugin note](validation/2026-10-26-claude-plugin-live.md). **2026-10-26, subagents**: 3 and then 20 concurrent subagents, each in its own hook-created branch, no ids, no contamination, killed-agent transaction aborted. [subagent note](validation/2026-10-26-subagent-isolation-live.md) |
| **Cursor** (IDE agent, `agent` CLI) | Hooks installed; env injection pending live verification | `sessionStart` hook answers `{"env": …}`, which Cursor applies to that conversation's shell per its docs. Env is not re-applied to a reopened chat and not inherited by Cursor subagents; cloud agents are skipped. | Payload-level tests only; live run needs `agent login`. [note](validation/2026-10-25-agent-live-cursor.md). Use `pgboxdb agent exec -- <command>` whenever `PGBOXDB_ISOLATION` is unset. |
| **Codex** | With the wrapper | Codex hooks cannot set environment. `pgboxdb agent exec -- codex` puts the branch in Codex's environment; inside any Codex session `pgboxdb agent exec -- <command>` attaches to the session's branch. Lifecycle hooks still create the branch and tell the model. The `workspace-write` sandbox blocks loopback TCP until `sandbox_workspace_write.network_access = true`. | Wrapper verified by tests; native hooks pending a logged-in Codex. [note](validation/2026-10-25-agent-live-codex.md) |
| Anything else | With the wrapper | `pgboxdb agent exec -- <command>` | `tests/agent_exec.rs`, e2e scenarios C and D |

## The pieces

- **The Skill** (`skills/pgboxdb/`, [SKILL.md](../../../skills/pgboxdb/SKILL.md)):
  what an agent reads. It decides when a task needs a database, classifies
  the project's existing setup (`local`, `pgboxdb`, `none`, `remote`,
  `sensitive`), chooses schema-only import by default, never imports
  production data without an explicit yes, runs the commands below, and
  explains failures. Installed with the `skills` CLI into `.claude/skills/pgboxdb`
  (Claude Code) and `.agents/skills/pgboxdb` (Codex, Cursor and others).
- **`pgboxdb agent`** (the CLI, [cli.md](cli.md#agents-one-isolated-database-per-coding-agent-session)):
  the deterministic part. `setup`, `doctor`, `exec`, `ls`, and the
  primitives `up`, `touch`, `down`, `env`, `hook`, `install`, `uninstall`.
  Everything takes `--json`.
- **`.pgboxdb.toml`** in the project root, written by `setup`, safe to commit:
  `source`, `idle_ttl`, `grace_period` in `[agent]` (and the `[git]` tables
  `pgboxdb git init` adds, see [git.md](git.md#configuration-pgboxdbtoml)). Names and
  durations only; URLs are rejected. PGBoxDB is one runtime per user, so the project is part of a
  session's identity and two projects can both call their template `app`.
- **Hook files**, merged into `.claude/settings.json`, `.cursor/hooks.json`,
  `.codex/hooks.json` (or the `--scope local`/`user` equivalents). The
  command is guarded (`command -v pgboxdb … || exit 0`), so a committed hook is a
  silent no-op for a teammate without PGBoxDB. Existing entries are never
  touched; `pgboxdb agent uninstall` removes only ours.

## With PGBoxDB Git

When the repository uses [PGBoxDB Git](git.md) (`[git]` in `.pgboxdb.toml`,
`[git.agents] enabled = true` by default), a session's branch is cloned from
the database of **the Git branch checked out where the session runs**, not
from the `[agent] source`. Each session and each subagent still gets its own
branch, so agents on the same Git branch never share mutable state and none
of them touches the developer's branch database:

```text
feature/auth ─ pgboxdb_myapp_auth_…  (developer)
                 ├─ agent-claude-<A>     ├─ agent-claude-<B>
                 ├─ agent-codex-<C>      └─ agent-claude-<A>-<subagent>
```

- The session identity gains the worktree and Git branch: switching Git
  branches gives the session a separate branch of the new Git branch's
  database (Claude Code's Bash commands are rerouted to it automatically);
  `SessionEnd`/`pgboxdb agent down` ends all of them.
- A branch database with connections (your dev server) is copied logically
  (`pg_dump`), never terminated; on main the agent branches from the latest
  validated main snapshot instead.
- `pgboxdb git exec`/`pgboxdb git env` inside a session return the session's
  branch. An explicit `--source`/`PGBOXDB_SOURCE` still wins.
- Without `[git]`, nothing here changes.

## Manual use

```sh
pgboxdb copy --from "$DATABASE_URL" --to myapp_template --schema-only   # import once
pgboxdb agent setup --source myapp_template                            # config + hooks + proof
pgboxdb agent doctor                                                   # is this project ready? is this session isolated?
pgboxdb agent exec -- bin/rails test                                   # any command in its own branch
pgboxdb agent ls                                                       # who has a branch, until when
pgboxdb agent down --now                                               # drop this session's branch
pgboxdb agent uninstall claude cursor codex                            # remove the hooks (keeps .pgboxdb.toml)
pgboxdb branch myapp_template feature-123                              # a hand-made 2h branch, as before
```

## Watching the agents: `pgboxdb top`

`pgboxdb top` in a second terminal shows every session and subagent branch live:
project, Git branch, session, connections, lifecycle (active, retired,
expired), TTL countdown and size, read-only. Its TASK column shows only
what an agent reported explicitly:

```sh
pgboxdb agent activity --state testing --task "Implement OAuth"   # working|migrating|testing|completed|failed
```

The hooks never report a phase — `SessionEnd`/`SubagentStop` mark the branch
retired, which is not success. To opt an agent in, ask it (in `CLAUDE.md`,
`AGENTS.md` or the prompt) to run `pgboxdb agent activity --state testing` before
the test suite and `--state completed`/`failed` at the end. A report never
changes the branch or its TTL. Details: [top.md](top.md).

## Semantics worth knowing

- **Identity.** A session is `(project, host, session id)`; a subagent adds
  the host's agent id; the branch name is derived from the identity. The
  same session id in another project is another branch; the same agent id
  in another session is another branch. Hand-picked ids (`pgboxdb agent exec
  --id`) shorter than 8 characters or with other characters are hashed into
  the name; `exec` branches are always labelled `exec`, so `pgboxdb agent down
  --id` finds them.
- **`--keep`.** `pgboxdb agent exec --keep` leaves the branch active for its
  idle TTL after the command exits; the next `exec` with the same `--id`
  attaches to it with its data. Without `--keep` the branch is dropped at
  exit. "Retired" is reserved for a session or subagent that ended: its
  branch lives on for the grace period and is revived by a resume.
- **The source must be idle.** `CREATE DATABASE … TEMPLATE` refuses a busy
  template; `up` fails with exit 6 and says so. Keep `<app>_template` for
  branching only.
- **Schema-only keeps migration bookkeeping.** `pgboxdb copy --schema-only`
  carries the rows of `schema_migrations`, `ar_internal_metadata`,
  `django_migrations`, `_prisma_migrations`, `alembic_version` and the other
  common bookkeeping tables, so a framework sees its migrations as applied.
- **Framework purges are survived.** If a session drops and recreates its
  branch under the same name (Rails `db:reset`), `pgboxdb agent` adopts the new
  incarnation: same TTL, same cleanup. Hand-made databases keep the strict
  rule that a reused name is never touched.
- **Concurrency.** A per-session lock serialises duplicate hook deliveries
  (Cursor also loads Claude hook files); two simultaneous starts for one
  session produce one branch.
- **Crash recovery.** Every operation is safe to repeat; a vanished branch is
  replaced by a fresh one and the context line says so.

## Security

The Skill's full rules are in [skills/pgboxdb/references/security.md](../../../skills/pgboxdb/references/security.md).
In short: no credentials in the conversation (the detection script prints
host and database name only, and the Skill is told not to read credential
files); schema-only by default and no production data without explicit
approval; nothing leaves the machine; no edits to application config; no
deletion of developer databases; official installers only; existing hooks
preserved; sandboxes never weakened; fail closed.

## Troubleshooting

`pgboxdb agent doctor` first. The common cases are in the Skill's
[troubleshooting reference](../../../skills/pgboxdb/references/troubleshooting.md)
and in [troubleshooting.md](troubleshooting.md#coding-agents).
