# Live validation: the plugin from `pgrundev/pgx-plugin` (2026-10-26)

Host: Claude Code 2.1.311, macOS 14 arm64. PGX 0.1.2 (the published npm
`pgxdb@0.1.2`, engine b04e5045c2). Plugin: `pgrundev/pgx-plugin` at
`cc5c211` (plugin 0.1.2, generated from this repository's
`feat/pgx-plugin-standalone`). Project: a scratch directory with plain SQL
migrations and a `.env` whose `DATABASE_URL` names `pgxplugin_demo`, a
disposable database on the developer's local PostgreSQL 15 (two tables,
3 customers, 4 orders). The plugin's cache was isolated with
`CLAUDE_CODE_PLUGIN_CACHE_DIR` and the marketplace and plugin were installed
at local scope, so `~/.claude` was not touched. Headless sessions ran with
`--permission-mode acceptEdits` and an explicit `--allowedTools` list (no
`--dangerously-skip-permissions`).

## Installation

| Step | Result |
|---|---|
| `claude plugin marketplace add pgrundev/pgx-plugin` (fresh `CLAUDE_CONFIG_DIR`) | 4.0 s; clone 316 KB on disk (`.git` 172 KB); `Successfully added marketplace: pgx-plugin` |
| `claude plugin install pgx@pgx-plugin` | under 1 s; installed copy 19 files, 110 KB |
| `claude plugin install pgx --marketplace pgrundev/pgx-plugin` (fresh config, both steps) | 4.0 s |
| `claude plugin install pgrundev/pgx-plugin` / `… https://github.com/pgrundev/pgx-plugin` | `not found in any configured marketplace`: Claude Code has no marketplace-less install, which is why the repository carries its own one-entry marketplace |
| `claude plugin details pgx` | `Skills (1) pgx`, `Hooks (0)`, `MCP servers (0)`, always-on ~191 tokens |
| For comparison, `claude plugin marketplace add pgrundev/pgx` (this repository) | 65 s; 2.2 GB on disk (`.git` 178 MB); installed copy 16 files, 96 KB |

## Session 1: setup through `/pgx:pgx`

Prompt: use the PGX plugin skill to set up isolated Postgres databases for
this project; schema-only template; Claude Code isolation. 36 turns, 141 s,
$1.63.

- Loaded `pgx:pgx` through the Skill tool and ran the plugin's own
  `detect-project.sh` and `check-pgx.sh` from
  `<plugin cache>/pgx-plugin/pgx/0.1.2/skills/pgx/scripts/` (the
  `${CLAUDE_PLUGIN_ROOT}` path, substituted); read the skill's references
  from the same copy. `check-pgx.sh` reported PGX 0.1.2 compatible.
- Classified the project `local`, ran `pgx copy --from <URL from .env>
  --to pgxplugin_demo_template --schema-only` (2 tables, 0 rows; the URL was
  read by a throwaway script and never printed), then `pgx agent setup
  --source pgxplugin_demo_template`: `.pgx.toml` written, six hooks
  (`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `SubagentStart`,
  `SubagentStop`, `SessionEnd`) installed into `.claude/settings.json`,
  one entry each. Setup had also installed Cursor and Codex hooks; the
  session removed those because the prompt asked for Claude Code only.
- Verified with `pgx agent exec -- psql` in a branch created for the
  session, and `pgx agent doctor`: no problems found.
- Four compound commands (with `$(…)`, `source`, `; echo`) were refused by
  the allow-list and the session found an allowed form each time; nothing
  ran that was not allowed.

## Session 2: three concurrent subagents

New session in the same project. Prompt: launch exactly three subagents in
one message, each adding one migration (`reviews`, `coupons`, `shipments`),
applying it with `psql "$DATABASE_URL" -f …`, inserting a marker row and
reporting `current_database()`, `PGX_BRANCH`, `PGX_ROUTED` and `\dt`. No
subagent was given an id, a database name or a connection string. 2 turns,
125 s, $1.76, no permission denials.

Hook log (`~/.pgx/agent/hooks.log`): `SessionStart` → `agent-claude-72072592`;
three `SubagentStart` → `agent-claude-72072592-{a731ed68,ad59615c,a87c95a6}`
created in 356, 1957 and 820 ms; 22 `PreToolUse` events, every one `routed
to` the subagent's own branch (185–394 ms); three `SubagentStop` → retired;
`SessionEnd` → the session branch retired.

Queried directly afterwards (`pgx url <branch>`, within the grace period):

| Branch | Tables | Rows |
|---|---|---|
| `agent-claude-72072592-a731ed68` (reviews) | customers, orders, reviews | 1 customer, 0 orders, 1 review |
| `agent-claude-72072592-ad59615c` (coupons) | coupons, customers, orders | 0, 0, 1 coupon |
| `agent-claude-72072592-a87c95a6` (shipments) | customers, orders, shipments | 1 customer, 1 order, 1 shipment |
| `agent-claude-72072592` (the session's own) | customers, orders | 0, 0 |
| `pgxplugin_demo_template` | customers, orders | 0, 0 |

Each branch holds exactly its own migration and marker; no branch saw
another's table; the parent branch and the template are unchanged. The
three migration files were each written by one subagent.

## The developer database

`pgxplugin_demo` before, after session 1 and after session 2: schema dump
md5 `351ff52d1a4b86c09f943626edd0dfe5` all three times (ignoring pg_dump's
per-run `\restrict` token); 3 customers, 4 orders; `pg_stat_user_tables`
inserts/updates/deletes 7/0/0 throughout (the 7 inserts are the fixture
rows). Nothing in either session connected to the URL in `.env` except the
one `pg_dump --schema-only` that built the template.

## Cleanup

The five agent branches were `retired` at the end of their sessions and
the supervisor drops them after the 10-minute grace period. The scratch
project, `pgxplugin_demo` and `pgxplugin_demo_template` were disposable test
data created for this run and were dropped afterwards. Nothing was deleted
by hand before the sessions ended.
