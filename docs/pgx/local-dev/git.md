# PGX Git: one Git branch, one isolated Postgres

**Every Git branch gets its own database, automatically.** You keep using
plain `git switch`; PGX clones a copy-on-write database for each new branch
from the latest validated snapshot of your main database, gives it back when
you return to the branch, and never touches a branch database when main moves
on.

```sh
cd my-project
pgx git init
```

```text
✓ Git repository detected
✓ PGX runtime ready
✓ Main database configured
✓ Main snapshot v1 ready
✓ Git integration installed
✓ Automatic database branching enabled

PGX Git is ready.
Every Git branch gets its own Postgres.
```

```sh
git switch -c feature/auth
```

```text
✓ PGX database ready: feature/auth
  Cloned from main (v1) in 41 ms
  Database: pgx_myapp_auth_baf8a6
```

`git switch main` gives you the main development database again,
`git switch feature/auth` the same `feature/auth` database with its
migrations and data.

## How it works

```text
main Git branch ─► main development database          (you migrate/seed it as usual)
                         │ pgx git sync, post-merge on main, or lazily before a new clone
                         ▼ fingerprint (schema + migration bookkeeping) changed?
                   staging copy ─► validate ─► rename + ALLOW_CONNECTIONS false
                         ▼
                   main snapshot vN (immutable)  ◄── new Git branches clone from here
```

- **Main database** (`pgx_<repo>_main_<hash>`): the database you use on the
  main branch. Created by `pgx git init` (empty, cloned from a PGX template,
  or imported schema-only from PostgreSQL). Never overwritten.
- **Main snapshot** (`pgx_<repo>_base_v<N>_<hash>`): an immutable, validated
  copy of the main database. Nobody can connect to it (`ALLOW_CONNECTIONS
  false`), so it can always be cloned and never changes. New branches clone
  from the newest one.
- **Branch databases** (`pgx_<repo>_<branch>_<hash>`): one per Git branch per
  worktree, cloned from the snapshot that was current when the branch was
  first checked out. Each records its *baseline generation*.

### Automatic synchronisation

A new snapshot is published when the main database's **schema fingerprint**
(tables, columns, types, defaults, indexes, constraints, functions, enums,
extensions) or its **migration bookkeeping** (`schema_migrations`,
`_prisma_migrations`, `django_migrations`, `knex_migrations`,
`alembic_version`, `goose_db_version`, `flyway_schema_history`,
`__drizzle_migrations`, `SequelizeMeta`) changed. With
`sync_mode = "data"` row counts count too. Fingerprinting is a handful of
catalog queries (milliseconds); nothing is dumped when main is unchanged.

It runs explicitly, on `pgx git sync` (works from any branch, never changes
the branch you are on) and inside `pgx git init`, and automatically, from the
`post-merge` hook when you `git pull` on main and right before a new branch
database is cloned (so a new branch never starts from a stale baseline
because you forgot to sync). No background service is involved.

**Automatic snapshots are conservative.** They never wait and never take a
slow copy: they keep the current baseline (and say why) when another snapshot
is being taken, when main has connections (a migration or `db:reset` may be
running), when main does not exist right now, or when migrations committed
on main are not applied yet: a half-migrated main is never a baseline.
Otherwise they clone the idle main, which takes about a quarter of a second.
`pgx git sync` is the explicit path: it also snapshots a busy main, with a
consistent logical copy.

Publishing is atomic and safe under concurrent hooks, agents and worktrees:

1. copy main into a staging database: a copy-on-write clone when nobody is
   connected to main, otherwise (explicit sync only) a consistent logical
   copy (`pg_dump` → `pg_restore`, the `pgx copy` machinery) — PGX never
   terminates your connections to take a snapshot and never physically copies
   a database in use;
2. validate: the staged copy's fingerprints must equal main's;
3. `VACUUM (FREEZE, ANALYZE)` so clones share pages;
4. rename to the next generation, make it read-only (`ALLOW_CONNECTIONS
   false`: a snapshot that cannot be locked is not published), record it.

Two kernel locks (`flock`, so a crashed process never leaves one held)
coordinate this. The **state lock** covers every change to PGX Git's state
and every short database step (a clone, a rename); it is never held across a
slow copy, so a checkout hook or an agent waits at most for a clone. The
**sync lock** is held for a whole snapshot; the slow logical copy runs with
only it held.

If any step fails, the staging copy is dropped, the previous snapshot stays
the baseline, branch databases are untouched, and `pgx git status` shows the
failure. An interrupted sync (killed process, engine restart) is rolled back
by the next PGX Git command. Recovery does not rely on process ids (which
are reused, especially in containers): a snapshot in progress is dead
exactly when nobody holds the sync lock, and every other multi-step
operation runs inside the state lock. The previous snapshot is kept; older
ones are *retired* and removed by `pgx git prune --apply`.

### Git revision versus database state

A commit is not proof that its migrations ran. PGX Git tracks both: each
snapshot records main's Git revision, and the migration files committed at
main's revision (read from Git's object store: Rails `db/migrate`, Prisma
`prisma/migrations`, Django `*/migrations`, golang-migrate/dbmate
`migrations/*.up.sql`) are compared with the bookkeeping rows in the main
database. After `git pull` brings a new migration, `pgx git status` and
`pgx git sync` say **pending** until it is applied:

```text
Sync: Pending: 1 migration(s) committed on main are not applied to the main database (rails:20240202000000)
```

Apply it (on main: `pgx git exec -- bin/rails db:migrate`), then
`pgx git sync`. Or configure the command once and run
`pgx git sync --migrate`:

```toml
[git.migrations]
command = "bin/rails db:migrate"   # plain words only: no shell, no pipes, no variables
auto_apply_main = false            # true: run it unattended after you approved it once
```

The command runs only on your explicit `--migrate`; that run also records
your approval of that exact text, which `auto_apply_main = true` requires
before it runs the command from a hook. Change the text and it needs
approval again. It runs as an argv (never through a shell) in the worktree
that has main checked out, with the main database's connection in its
environment.

### Existing branches are never replaced

```text
main v1 ─┬─ feature/auth    (custom migrations)   still v1
         └─ feature/search  (custom test data)    still v1
main v2 ─── feature/billing (new)                 v2
```

Git merging code is not merging SQL state, so PGX does not try. A branch
based on an older snapshot shows as `stale`:

```text
$ pgx git status
PGX Git
Project: myapp
Git: feature/auth
Worktree: /work/myapp
Database: pgx_myapp_auth_baf8a6
Main database: pgx_myapp_main_73217d (main)
Main snapshot: v3
Branch baseline: v2
Status: Ready (0 connections)
Sync: Main has newer changes (v3); this branch is based on v2 — `pgx git reset --from-main` gives a fresh copy
Agents: every session and subagent gets its own copy of this branch's database (0 active)
```

To start over from main, explicitly: `pgx git reset --from-main` (asks for
confirmation; `--force` for scripts). It refuses on main and protected
branches. The fresh copy is cloned first, so a failure leaves the old
database in place.

## Using the database: `exec`, `env`, shell integration

A Git hook runs in a child of `git`, which is a child of your shell: **it
cannot change your shell's environment**, and PGX does not pretend it did.
Three honest ways to get the right `DATABASE_URL`:

| | |
|---|---|
| `pgx git exec -- <command>` | Resolves the current branch's database now and runs the command with `DATABASE_URL`, `PGHOST`, `PGPORT`, `PGUSER`, `PGPASSWORD`, `PGDATABASE`, `PGX_BRANCH`, `PGX_GIT_BRANCH`, `PGX_WORKTREE`, `PGX_SOURCE`, `PGX_ISOLATION`. Works anywhere, no setup. Replaces itself with the command, so exit codes and signals are the command's own. |
| `eval "$(pgx git env --shell zsh)"` | The same variables, exported into this shell (`bash`, `zsh`, `fish`: `pgx git env --shell fish \| source`). Plain `pgx git env` shows them with the password masked. |
| `pgx git install-shell` | Adds one marked block to `~/.zshrc` (or `~/.bashrc`, `~/.config/fish/config.fish`) that runs `pgx git shell <sh>`: a prompt hook that refreshes the variables whenever the worktree or branch changed, so the **next** command you type uses the right database. Leaving PGX Git repositories restores what the shell had before. Explicit and idempotent; `pgx git uninstall --shell` removes it. |

```sh
git switch feature/auth
bin/rails db:migrate        # with shell integration: feature/auth's database
pgx git exec -- bin/rails test   # without: always correct
```

**Running processes keep the environment they started with.** A Rails
server, a Next.js dev server or a REPL started on main keeps talking to
main's database after `git switch`: restart it (or start it with
`pgx git exec -- …`).

Connection secrets stay local: no URL or password is ever written to
`.pgx.toml`, a Git hook, a log, or your shell history (`eval` output is not
history).

If PGX Git is enabled but the branch's database cannot be resolved, `exec`
refuses to run the command and the shell integration exports an unreachable
sentinel (`PGX_ISOLATION=failed`): nothing silently falls back to an
inherited `DATABASE_URL`.

### Framework examples

```sh
# Rails
pgx git exec -- bin/rails db:prepare
pgx git exec -- bin/rails test

# Prisma
pgx git exec -- npx prisma migrate dev
pgx git exec -- npm test

# Django
pgx git exec -- python manage.py migrate
pgx git exec -- pytest

# Node.js (pg, Knex, Drizzle, Sequelize: anything reading DATABASE_URL)
pgx git exec -- node scripts/seed.js

# psql
pgx git exec -- psql
```

Rails reads `DATABASE_URL` over `config/database.yml`; Prisma reads it via
`env("DATABASE_URL")`; for Django use `dj-database-url` (or read the `PG*`
variables, which libpq-based drivers pick up automatically).

## Git worktrees

```sh
git worktree add ../app-auth feature/auth
git worktree add ../app-billing feature/billing
```

Each worktree gets its own database, resolved from wherever you (or an agent)
work inside it: a branch database is keyed by repository **and** worktree
(`repository_id + worktree_id + branch + role`). Linked worktrees (with a
`.git` file instead of a directory) and `core.hooksPath` are handled; hooks
live in the repository's common directory, so one installation covers every
worktree. A removed worktree's databases are only candidates for
`pgx git prune` after the grace period (`abandoned_ttl`, default 7 days); a
worktree that is merely missing for a while (an unmounted disk) is not
removed.

Identities survive moves: `pgx git init` writes a repository id
(`.git/pgx-repo-id`) and each worktree's id (`pgx-worktree-id` in its Git
directory). `mv`-ing the repository or `git worktree move` keeps every
database mapping; a *copy* of the repository (its id names a repository that
still exists elsewhere) gets a new id and databases of its own.

A detached HEAD gets one database per worktree (`pgx_<repo>_detached_<hash>`);
during `git rebase` the branch being rebased keeps its own database.

## AI coding agents

PGX Git builds on [`pgx agent`](agents.md). The hierarchy:

```text
feature/auth (Git branch)
   └─ pgx_myapp_auth_…            the developer's branch database
        ├─ agent-claude-…          Claude session A
        ├─ agent-claude-…          Claude session B
        ├─ agent-codex-…           a Codex session
        └─ agent-claude-…-…        a Claude subagent
```

With `[git]` enabled (and `[git.agents] enabled = true`, the default), an
agent session's branch is cloned from **its current Git branch's database**
instead of the `[agent] source`. Every session and every subagent still gets
its own database: two agents on the same Git branch never share mutable
state, and none of them writes to the developer's branch database. TTL,
touch, grace and fail-closed behaviour are exactly those of `pgx agent`.

- If the branch database is in use (your dev server is connected), the agent
  branch is a consistent logical copy instead of a file clone; on main it is
  cloned from the latest main snapshot. Nobody's connection is terminated.
- If an agent switches Git branches, its next database (`pgx agent up`,
  `pgx agent exec`, `pgx git exec`) is a separate branch of the new Git
  branch's database. In Claude Code, the session's Bash commands are
  rerouted automatically once the Git branch changed (a `PreToolUse` rewrite
  to `pgx agent exec --cwd …`). Ending the session ends its branches for
  every Git branch it used.
- **Switching and working in one command is refused.** A command's
  `DATABASE_URL` is fixed when it starts, so in
  `git switch feature/x && bin/rails db:migrate` the migration would run on
  the *previous* branch's database. In Claude Code, the `PreToolUse` hook
  denies such a command before it runs (parent session and subagents; also
  `git checkout <branch>`, `gh pr checkout`, and a `cd` into another
  worktree followed by more commands) and tells the agent the two safe forms:

  ```sh
  git switch feature/x                                 # one Bash call…
  bin/rails db:migrate                                 # …the next one gets feature/x's database
  git switch feature/x && pgx git exec -- bin/rails db:migrate   # or one call: the wrapper resolves after the switch
  ```

  A switch alone, a switch followed only by Git or other non-database
  commands (`git push`, `echo`, output filters such as `| tail -5`),
  redirections (`git switch x 2>&1`), `git checkout -- <paths>` and
  `git checkout <file>` (asked from Git: a ref is a switch, a file is not)
  pass; shell keywords and launchers (`if git switch x; then …`, `time`,
  `env`, `!`) and Git's global options (`-C`, `--git-dir`) are seen through.
  The same check runs inside `pgx agent exec` and `pgx git exec` for a
  `<shell> -c '<script>'` command (exit 2, nothing runs), so Codex, Cursor
  and scripts that use the wrappers get it too. A command this hook itself
  rewrote is checked by what it runs, and only counts as rewritten when it
  matches the rewrite exactly, for that session or subagent.
- **Agents sharing a worktree share its HEAD.** When the branch changes under
  an agent (it switched, or another agent or the developer did), that agent's
  next command is stopped once with the reason before its database changes
  (Claude Code); re-running it continues on a copy of the new branch's
  database. Give parallel agents their own worktrees (`git worktree add`,
  `claude --worktree`) to avoid this altogether.
- `pgx git exec` and `pgx git env` inside an agent session return the
  session's own database, never the developer's.

Setup is the usual one: `pgx git init` for the repository, then
`pgx agent setup` (or the PGX Skill / Claude Code plugin) for the hosts.

```sh
# Claude Code: the hooks from `pgx agent setup` do everything
claude
# Codex: wrap the session (or each command)
pgx agent exec -- codex
# Cursor: hooks from `pgx agent setup --hosts cursor`; in its terminal use
pgx git exec -- npm test
```

Without PGX Git (no `[git]` table), `pgx agent` behaves exactly as before.
`pgx branch` and `pgx clone` are unchanged.

## Commands

All accept the global `--json`.

| Command | |
|---|---|
| `pgx git init [--source <db\|postgres-url>] [--main <branch>] [--schema-only\|--with-data] [--no-hooks]` | Detect the repository, initialise/start PGX, create (never overwrite) the main database, publish the first snapshot, install the hooks, map the current branch. Safe to rerun. `--source` names a PGX database to clone (else `[git] source`, else `[agent] source`, else empty) or a `postgres://` URL to import read-only, **schema-only unless `--with-data`**. PGX never reads `DATABASE_URL` or any database on its own. |
| `pgx git status` | Project, Git branch, worktree, database, readiness, main database, main snapshot, branch baseline, sync status (changed / pending migrations / stale / last failure), agent isolation. Read-only. |
| `pgx git ls` | `BRANCH DATABASE BASE STATUS WORKTREE` with status `current`, `ready`, `stale`, `abandoned`, `missing`; then the snapshots. |
| `pgx git sync [--force] [--migrate]` | Refresh the main snapshot if main changed (`--force`: even if not). Prints the generation. Exit 1 when it failed (the previous snapshot stays). |
| `pgx git env [--shell bash\|zsh\|fish] [--reveal]` | The current branch's connection variables. The password is masked in human and `--json` output unless `--reveal` is given; `--shell` prints complete `export` lines for `eval`. |
| `pgx git exec -- <command> [args…]` | Run a command with them. |
| `pgx git reset --from-main [--force]` | Recreate the current branch database from the latest snapshot. |
| `pgx git prune [--apply] [--grace <dur>]` | Databases of deleted branches and removed worktrees idle longer than `abandoned_ttl`, and retired snapshots. Dry run unless `--apply`. Never touches main, protected branches, agent databases, databases with connections, databases recreated outside PGX Git, or anything PGX Git did not create. |
| `pgx git doctor` | Runtime, repository, `.pgx.toml`, hooks, shell integration, main database, snapshot health and lock, schema/migration sync, mapping collisions and missing databases, unmapped look-alikes, agent integration, `pg_dump` availability. Exit 1 on a failure. |
| `pgx git uninstall [--shell]` | Remove PGX's hook blocks (and files it created), and with `--shell` the shell integration. Databases and `.pgx.toml` are kept. |
| `pgx git shell <sh>` / `pgx git install-shell [<sh>]` | Print / install the prompt integration. |

## Git hooks

`pgx git init` installs `post-checkout`, `post-merge` and `post-rewrite` in
the effective hooks directory (`git rev-parse --git-path hooks`, which
honours `core.hooksPath`):

- a missing hook becomes a small script PGX owns;
- an existing hook gets a marked block inserted right after its shebang (so
  an `exit 0` later in your script cannot skip it); your lines and the file's
  permissions are untouched, the block reads no stdin (Git feeds
  `post-rewrite` on stdin) and always succeeds;
- a hook committed to the repository, a symlink, or a script in another
  language (Ruby, Python, Node) is left alone, with the line to add by hand:
  `pgx git hook post-checkout "$@" </dev/null || true`;
- PGX only writes into the repository's own hooks directory. A
  `core.hooksPath` outside the repository (shared by every repository on the
  machine) or inside the work tree (husky's generated `.husky/_`, a committed
  `.githooks/`) is left alone with the same line to add yourself (to
  `.husky/<hook>` for husky; in a shared directory the line is a no-op for
  repositories without PGX Git). `pgx git uninstall` likewise never touches
  such a directory.

The block runs `pgx git hook <name> "$@"` (Git's arguments as data; nothing
from the repository is evaluated) and never blocks Git: if PGX is missing,
not initialised or broken, you get one line on stderr and Git carries on.
`PGX_GIT_SKIP=1 git switch …` skips it once.

Events no hook sees are reconciled lazily: `git branch -m old new` carries
the database over on the next resolution (from the reflog), a deleted branch
or removed worktree is found by `pgx git prune`, and a database dropped by
hand is recreated from the main snapshot on next use.

## Configuration (`.pgx.toml`)

Safe, committed project settings only; everything mutable (database names,
OIDs, generations, fingerprints, mappings, approvals) lives in
`$PGX_HOME/git/<repository id>/state.json` (0600).

```toml
[git]
enabled = true
main_branch = "main"        # master, trunk, … (detected by `pgx git init`)
source = "app_template"     # optional: the PGX database main was first cloned from
auto_branch = true          # create a branch database on checkout
auto_sync = true            # refresh the snapshot when main changed
sync_mode = "schema"        # or "data" (row counts count too)
branch_source = "main"      # the only supported value
preserve_existing = true    # the only supported value

[git.cleanup]
abandoned_ttl = "7d"
protected_branches = ["main", "master"]

[git.agents]
enabled = true              # agent sessions branch from the Git branch database
isolated_per_session = true # the only supported value
isolated_per_subagent = true

[git.migrations]
auto_apply_main = false
# command = "bin/rails db:migrate"
```

Values are validated on load: connection URLs, invalid branch or database
names, shell syntax in `command` and unsupported settings are refused with a
clear error. Unknown keys are ignored, and a `.pgx.toml` without `[git]`
(older PGX versions, `pgx agent setup`) keeps working unchanged.

## Database names

`pgx_<repo>_<branch>_<6 hex>`, lowercase `[a-z0-9_]`, at most 63 bytes:
`feature/user-auth` → `pgx_myapp_user_auth_a19f3c`. The readable part drops
the first path component and anything that is not a letter or digit; the
hash covers the repository id, worktree id, the complete branch name and the
role, so `feature/x` and `fix/x`, two worktrees, or two repositories never
collide. If a name is already taken by a database PGX Git does not map there
(a manual one), a 12-hex variant is used and the other database is left
alone. The branch name itself is kept as metadata.

## Limits

- Snapshots of a busy main database and agent branches of a busy branch
  database need `pg_dump`/`pg_restore` (PostgreSQL client tools); without
  them PGX waits for the database to be idle rather than terminating
  connections, and says so.
- Pending-migration detection knows the layouts listed above; other tools
  still get schema-fingerprint change detection.
- Hooks and agents wait at most 60 s for the state lock (in practice a
  clone); Git is never blocked longer.
- A running server is not reconfigured on `git switch`; restart it.
- The same-command guard reads the command text: a branch switch hidden in
  a script (`./switch-and-migrate.sh`) or behind a variable is not seen.
- Cursor's terminal environment is set at session start and PGX's Cursor
  hooks deliberately stay off permission events, so a raw terminal command
  in Cursor after a branch switch still uses the session's first database:
  in PGX Git repositories, run database commands in Cursor as
  `pgx git exec -- …` (which re-resolves and carries the guard). Codex runs
  commands through `pgx agent exec`, which re-resolves every time.
- `PGX_GIT_FAILPOINT` (fault injection for the failure tests) exists only in
  builds made with `--features failpoints`; release builds ignore it.
