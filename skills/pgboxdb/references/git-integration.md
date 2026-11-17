# PGBoxDB Git for agents

PGBoxDB Git (`pgboxdb git …`) gives every Git branch, per worktree, its own
copy-on-write database, cloned from the latest validated, immutable snapshot
of the repository's main database. Agent sessions sit one level below:

```text
feature/auth ─ pgboxdb_<repo>_auth_<hash>      the developer's branch database
                 ├─ agent-claude-…          your session
                 ├─ agent-claude-…-<sub>    your subagent
                 └─ agent-codex-…           another session
```

## Version

`pgboxdb git` needs **PGBoxDB 0.1.3 or newer**. On 0.1.2 `pgboxdb git` is an unknown
command (exit 2); the agent features (`pgboxdb agent …`) still work there, so
only offer PGBoxDB Git after `pgboxdb --json version` reports 0.1.3 or newer.

## Detect

- `detect-project.sh`: `git.repository` (inside a Git work tree) and
  `git.pgboxdb_git` (`.pgboxdb.toml` has a `[git]` table).
- `pgboxdb --json git status`: `enabled`, `git_branch`, `database` (the
  developer's branch database; **not yours to write to**), `main_snapshot`,
  `branch_baseline`, `sync.status` (`up_to_date`, `main_changed`,
  `pending_migrations`, `stale`, `failed`), `agents.active`.
- `PGBOXDB_GIT_BRANCH` and `PGBOXDB_WORKTREE` are set next to `DATABASE_URL` in
  commands run through `pgboxdb git exec`.

## Work

- In a PGBoxDB Git repository your session's branch is a copy of the current Git
  branch's database: its migrations and data are what the developer has on
  that branch. Run migrations and tests in your session as usual; they never
  reach the developer's database or another agent's.
- `pgboxdb git exec -- <command>` inside a session runs against **your session's**
  branch (same as `pgboxdb agent exec`). `pgboxdb git env` likewise.
- After `git switch` / `git worktree add`, the next database command gets a
  branch of the new Git branch's database. Never switch and use the database
  in the same command: `git switch x && bin/rails db:migrate` would run on
  the old branch's database, and Claude Code's hook refuses it. Switch in one
  command and work in the next, or wrap the database part:
  `git switch x && pgboxdb git exec -- bin/rails db:migrate`.
- `pgboxdb --json git env` masks the password; never add `--reveal` (you do not
  need credentials: `pgboxdb git exec` passes them to the command).
- If a command is refused with "the Git branch in this worktree is now …",
  the worktree's HEAD moved under you (your own switch, another agent, or the
  developer). Check that the branch is the one you mean to work on; then
  re-run. Parallel agents should each use their own worktree.
- In Cursor, and in any shell PGBoxDB's hooks do not cover, run database
  commands as `pgboxdb git exec -- <command>` in PGBoxDB Git repositories.
- If the developer's branch database is busy (a dev server), your branch is a
  logical copy; on main, a clone of the latest main snapshot. Nothing is
  terminated.

## Set up (only with the user's go-ahead)

```sh
pgboxdb git init                                  # empty main database, first snapshot, hooks
pgboxdb git init --source <pgboxdb template>          # clone an existing PGBoxDB template into main
pgboxdb git init --source "$DEV_DATABASE_URL"     # read-only import, schema-only by default
pgboxdb git doctor
```

`pgboxdb git init` writes the `[git]` tables into `.pgboxdb.toml` (names only, no
secrets; commit it) and inserts a marked block into existing Git hooks
without changing them. It never reads `DATABASE_URL` on its own and never
imports data without `--with-data`, which needs the same explicit approval as
`pgboxdb copy` without `--schema-only`. Agent hosts still need `pgboxdb agent setup`.

## Main changes

`pgboxdb git sync` publishes a new main snapshot when main's schema or migration
bookkeeping changed; new branches then start from it, existing branch
databases are never replaced. `pending` means migrations committed on main
are not applied to the main database: tell the user, do not apply them to
main yourself unless asked (`pgboxdb git sync --migrate` runs the configured
command and records approval). `pgboxdb git reset --from-main` recreates a branch
database from main (destructive: ask first).

## Fail closed

If `pgboxdb git exec` refuses ("PGBoxDB Git is not enabled", "cannot resolve …") or
`PGBOXDB_ISOLATION=failed`, do not fall back to an inherited `DATABASE_URL`.
Run `pgboxdb git doctor` and `pgboxdb agent doctor`, report, stop.
