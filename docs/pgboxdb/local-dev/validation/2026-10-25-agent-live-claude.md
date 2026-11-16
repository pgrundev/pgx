# Live validation: Claude Code (2026-10-25)

Host: Claude Code 2.1.310 (`claude -p`, headless), macOS 14 arm64, PGX built
from `feat/pgx-agent` at the commit noted in the PR. Lab: a copy of
`tests/apps/rails-agent` (Rails 8.1.4, PostgreSQL adapter) with the Skill
installed by `npx skills add <path> -a claude-code -y`, a scratch PostgreSQL 18
on port 54351 holding the app schema and two users as "the developer's
database" (`.env` pointed `DATABASE_URL` at it), a scratch `PGX_HOME`, and a
scratch `pgx` prefix on `PATH`. Allowed tools: Bash, Read, Edit, Write, Glob,
Grep, Skill; `--max-turns 40`. No Claude settings were changed; the user's
own `~/.claude/settings.json` hooks ran as usual.

## Runs

| Prompt | Session | Turns | Cost | Result |
|---|---|---|---|---|
| Setup: "Use PGX to give this project isolated databases for coding agents. Set everything up." | `3053fe60` | 31 | $3.32 | ok |
| Feature (new session): "Add a last_login_at timestamp column to users with a migration and run the tests." | `82842350` | 12 | $1.26 | ok |
| Resume (`--resume <feature session>`): "Run the test suite again and print the current database name." | `82842350` | 2 | $1.33 | ok |
| Concurrent A (`--session-id 11111111-…`): create concurrent-a@example.com, print count and database | `11111111` | 3 | $0.66 | ok |
| Concurrent B (`--session-id 22222222-…`, started 20 s later): print count, whether that user exists, database | `22222222` | 2 | $0.63 | ok |

Total: $7.20.

## What happened

**Setup.** Claude invoked the `pgx` skill unprompted, ran
`scripts/detect-project.sh` (classification `local`, framework `rails`),
read `installation.md` and `postgres-import.md`, found the dev URL in `.env`
without printing it (it redacted URLs in every command it ran), imported the
schema with `pgx copy --from "$DATABASE_URL" --to rails_agent_template
--schema-only`, ran `pgx agent setup --source rails_agent_template` (which
wrote `.pgx.toml` and installed hooks for Claude Code, Cursor and Codex, the
last two because `~/.cursor` and `~/.codex` exist on this machine), then
verified with `pgx agent exec -- bin/rails db:prepare` and `bin/rails test`
and `pgx agent doctor`. The developer's PostgreSQL fingerprint
(`count:md5(emails)`) was identical before and after. It correctly told the
user that this session predates the hooks and that new sessions are automatic.

Two defects surfaced and were fixed in commit `61bb1e922f` before the next
runs: (1) a schema-only import carried no `schema_migrations` rows, so Rails
re-ran the migration on the first branch, failed on the existing table and
purged the database; `pgx copy --schema-only` now keeps the rows of known
migration-bookkeeping tables (Rails, Django, Prisma, Alembic, Flyway, Knex,
goose, …). (2) The purge recreated the branch under the same name with a new
OID, which the old code treated as a foreign database and orphaned without a
TTL; `pgx agent up`, `down` and the supervisor now adopt a recreated
incarnation of an agent branch. Claude diagnosed both itself during the run
(hence 31 turns) and worked around them by inserting the bookkeeping rows and
dropping the orphan.

One behaviour to flag: while diagnosing, Claude ran `cat ~/.codex/config.toml`
and `cat ~/.pgx/config.json`, which put a Neon API token and the default PGX
home's local password into that session's transcript (local file under
`~/.claude/projects/`, and the model request). The Skill now says explicitly
never to read credential-bearing files; the token should be rotated.

**Feature, fresh session.** No manual step. The `SessionStart` hook created
`agent-claude-82842350` and wrote nine exports to
`~/.claude/session-env/<session>/sessionstart-hook-1.sh`; Claude's first Bash
command already had `DATABASE_URL`. It wrote the migration, ran
`bin/rails db:migrate` and `bin/rails test` (3 runs, 5 assertions, 0 failures,
including the fixture's test that asserts the database name starts with
`agent-`). Verified from outside: the branch has `last_login_at`, the template
does not, the dev PostgreSQL does not and its fingerprint is unchanged. Claude
noticed Rails' `ActiveRecord::EnvironmentMismatchError` from the spawned
`db:test:prepare` (the branch carries `environment = development` in
`ar_internal_metadata`) and correctly left it alone.

**Resume.** `claude --resume <feature session>` 2 minutes after it ended:
the hook found the retired branch inside its 10-minute grace, revived it, and
`bin/rails runner` printed `agent-claude-82842350`. 20 seconds, 2 turns.

**Concurrent.** Two sessions with fixed ids 20 seconds apart: A created a
user in `agent-claude-11111111` (count 1); B saw count 0 and no such user in
`agent-claude-22222222`. Both reported their database name from Rails
itself.

## Runs after the review fixes (2026-10-26)

| Prompt | Turns | Cost | Result |
|---|---|---|---|
| Fail-closed: `.pgx.toml` names a missing template, PGX stopped, the developer's PostgreSQL `DATABASE_URL` inherited; "run the pending migrations now" | 5 | $0.75 | The hook wrote the sentinel (`DATABASE_URL=postgres://pgx-isolation-failed@127.0.0.1:1/…`, `PGX_ISOLATION=failed`) and exited 0 so Claude Code kept it; the agent read the reason, ran `pgx agent doctor`, and refused ("no database was touched"). The developer database's `schema_migrations` and columns were unchanged. |
| Re-validation on the release code: "add a unique index on users.email via a migration, run it and the tests" | 14 | $1.05 | Fresh session, branch `agent-claude-4f689f41`, migration and suite green (4 runs, 0 failures); the template kept one index, the branch had two; the dev PostgreSQL fingerprint unchanged. |
| **Public artifact**: Skill installed from public `main`, PGX from the published `v0.1.1` darwin-arm64 tarball (checksum verified), setup by the documented commands, then "Add a last_login_at timestamp to users and run the tests." | 7 | $0.8 | Branch `agent-claude-90c67043` created by the hook (`PGX_ISOLATION=ok`), migration and tests green (3 runs, 5 assertions), the branch has the column, template and dev PostgreSQL do not, fingerprint unchanged, branch retired after the session. 53 seconds end to end. |

Seven runs in total: $9.00 for the first six plus $0.8.

## Verdict

Automatic isolation on Claude Code is verified end to end, including with the public 0.1.1 artifact: Skill discovery
and activation, PGX setup from an existing PostgreSQL, hook-driven branch
creation with environment injection, real migrations and tests, resume
within grace, and concurrent isolation, with the developer's database
untouched throughout.

## Not covered here

Cursor and Codex live runs (see their notes), Linux hosts (CI runs the
synthetic suites there), and interactive `claude` sessions (only `-p` was
used; the hook mechanism is the same).
