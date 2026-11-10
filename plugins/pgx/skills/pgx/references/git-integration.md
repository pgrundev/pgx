# PGX Git for agents

PGX Git (`pgx git …`) gives every Git branch, per worktree, its own
copy-on-write database, cloned from the latest validated, immutable snapshot
of the repository's main database. Agent sessions sit one level below:

```text
feature/auth ─ pgx_<repo>_auth_<hash>      the developer's branch database
                 ├─ agent-claude-…          your session
                 ├─ agent-claude-…-<sub>    your subagent
                 └─ agent-codex-…           another session
```

## Detect

- `detect-project.sh`: `git.repository` (inside a Git work tree) and
  `git.pgx_git` (`.pgx.toml` has a `[git]` table).
- `pgx --json git status`: `enabled`, `git_branch`, `database` (the
  developer's branch database; **not yours to write to**), `main_snapshot`,
  `branch_baseline`, `sync.status` (`up_to_date`, `main_changed`,
  `pending_migrations`, `stale`, `failed`), `agents.active`.
- `PGX_GIT_BRANCH` and `PGX_WORKTREE` are set next to `DATABASE_URL` in
  commands run through `pgx git exec`.

## Work

- In a PGX Git repository your session's branch is a copy of the current Git
  branch's database: its migrations and data are what the developer has on
  that branch. Run migrations and tests in your session as usual; they never
  reach the developer's database or another agent's.
- `pgx git exec -- <command>` inside a session runs against **your session's**
  branch (same as `pgx agent exec`). `pgx git env` likewise.
- After `git switch` / `git worktree add`, the next database command gets a
  branch of the new Git branch's database. Never switch and use the database
  in the same command: `git switch x && bin/rails db:migrate` would run on
  the old branch's database, and Claude Code's hook refuses it. Switch in one
  command and work in the next, or wrap the database part:
  `git switch x && pgx git exec -- bin/rails db:migrate`.
- `pgx --json git env` masks the password; never add `--reveal` (you do not
  need credentials: `pgx git exec` passes them to the command).
- If a command is refused with "the Git branch in this worktree is now …",
  the worktree's HEAD moved under you (your own switch, another agent, or the
  developer). Check that the branch is the one you mean to work on; then
  re-run. Parallel agents should each use their own worktree.
- In Cursor, and in any shell PGX's hooks do not cover, run database
  commands as `pgx git exec -- <command>` in PGX Git repositories.
- If the developer's branch database is busy (a dev server), your branch is a
  logical copy; on main, a clone of the latest main snapshot. Nothing is
  terminated.

## Set up (only with the user's go-ahead)

```sh
pgx git init                                  # empty main database, first snapshot, hooks
pgx git init --source <pgx template>          # clone an existing PGX template into main
pgx git init --source "$DEV_DATABASE_URL"     # read-only import, schema-only by default
pgx git doctor
```

`pgx git init` writes the `[git]` tables into `.pgx.toml` (names only, no
secrets; commit it) and inserts a marked block into existing Git hooks
without changing them. It never reads `DATABASE_URL` on its own and never
imports data without `--with-data`, which needs the same explicit approval as
`pgx copy` without `--schema-only`. Agent hosts still need `pgx agent setup`.

## Main changes

`pgx git sync` publishes a new main snapshot when main's schema or migration
bookkeeping changed; new branches then start from it, existing branch
databases are never replaced. `pending` means migrations committed on main
are not applied to the main database: tell the user, do not apply them to
main yourself unless asked (`pgx git sync --migrate` runs the configured
command and records approval). `pgx git reset --from-main` recreates a branch
database from main (destructive: ask first).

## Fail closed

If `pgx git exec` refuses ("PGX Git is not enabled", "cannot resolve …") or
`PGX_ISOLATION=failed`, do not fall back to an inherited `DATABASE_URL`.
Run `pgx git doctor` and `pgx agent doctor`, report, stop.
