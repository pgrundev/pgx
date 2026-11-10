# PGX release

A self-contained, relocatable, PostgreSQL 18 compatible runtime for local
development, CI and disposable databases. No system PostgreSQL required.

## PGX v0.1.3 — One Git Branch, One Isolated Postgres

`pgx git` gives every Git branch, in every worktree, its own copy-on-write
database, cloned from the latest validated snapshot of your main database.
You keep using plain `git switch`.

```sh
cd my-project
pgx git init                      # main database, first main snapshot, Git hooks
git switch -c feature/auth        # ✓ PGX database ready: feature/auth
pgx git exec -- bin/rails db:migrate
pgx git sync                      # publish main's new schema as the next snapshot
```

- **Commands:** `pgx git init | status | ls | sync | env | exec | reset --from-main | prune | doctor | uninstall | shell | install-shell`.
- **Hooks:** `post-checkout`, `post-merge` and `post-rewrite` create and map
  branch databases. Existing hooks, permissions and stdin are preserved, and
  `uninstall` restores them byte for byte. Shared hook directories
  (`core.hooksPath` outside the repository, husky's `.husky/_`) are never
  written to; `pgx git init` prints the line to add yourself.
- **Main snapshots:** immutable, versioned, validated by schema and
  migration fingerprints, and published only when main is idle and has no
  pending migrations. A branch database is never overwritten when main moves
  on. `pgx git reset --from-main` re-clones a branch on request and is
  crash-safe.
- **Worktrees and repositories:** each worktree gets its own database for
  the same branch. Identity is stored in `.git`, so moving or renaming the
  repository keeps its databases, and a copied repository gets new ones.
- **Agents:** `pgx agent` sessions and subagents now branch from the current
  Git branch's database: Git branch → agent session → subagent. In Claude
  Code, a command that switches branches and then keeps working is refused
  with the fix, and later commands are rerouted to the new branch's database.
- **Shell:** `pgx git install-shell` (zsh, bash, fish) keeps `DATABASE_URL`
  in step with the checked-out branch.
- **Security:** `pgx git env` and `--json` mask the password unless you pass
  `--reveal`. External imports are schema-only by default and the source is
  never written. Branch names never reach a shell. Fault injection exists
  only in test builds compiled with the `failpoints` feature.
- **Config:** `[git]` in `.pgx.toml`. Guide: `docs/pgx/local-dev/git.md`.

Measured on macOS arm64 (M1 Pro, APFS): a branch database clone takes
34–72 ms, `git switch -c` including the hook 486–589 ms, switching to an
existing branch 230–245 ms, and `pgx git sync` on an idle main 204–292 ms.

### Known limitations

1. Cursor's terminal keeps the database from the start of the session after
   a branch switch; start a new session or use `pgx git exec`.
2. The branch-switch guard and rerouting are Claude Code only. Codex and
   Cursor sessions are still isolated, but not rerouted after a switch.
3. The guard cannot see a branch switch hidden inside a script or a
   variable.
4. Copying a database that is in use needs `pg_dump`/`pg_restore`.
5. The fish integration is not tested yet.

## PGX v0.1.2 — Every AI Subagent Gets Its Own Postgres

0.1.1 gave every Claude Code **session** its own branch. Subagents launched
from that session inherited the session's identity and therefore its
database: three parallel subagents, one database. 0.1.2 gives every Claude
Code **subagent** its own branch, with nothing to configure and nothing for
the model to do:

- **One branch per subagent.** Identity is `(project, host, session, agent)`;
  a subagent's branch is `agent-<host>-<session8>-<agent8>`, a copy-on-write
  clone of the same template. The session's branch keeps its name and records.
- **Hooks.** `pgx agent setup` / `pgx agent install claude` now writes six
  Claude Code hooks (an existing four-hook installation is completed in place,
  nothing duplicated): `SubagentStart` creates the subagent's branch and tells
  the model its name, `PreToolUse` (Bash) runs each subagent command through
  `pgx agent exec --agent <id> --cwd <project> -- <shell> -c '<command>'` so
  it sees that branch's `DATABASE_URL`/`PG*`, and `SubagentStop` retires the
  branch after terminating whatever it left connected. If a branch cannot be
  ensured the command is refused with the reason; it never runs against the
  parent's database.
- **Killed agents cannot commit later.** Every agent branch carries
  `client_connection_check_interval` (5 s), `idle_in_transaction_session_timeout`
  (15 min) and `statement_timeout` (1 h), set with `ALTER DATABASE … SET` and
  adjustable or disableable in `.pgx.toml`; the template is never altered.
  `pgx agent down` terminates the branch's backends before retiring or
  dropping and reports `backends_terminated` / `backends_remaining`.
- **CLI.** `pgx agent exec`: explicit-id branches are labelled `exec`, `--keep`
  leaves the branch active for its idle TTL (the next `exec` with the same
  `--id` attaches to it), `--json` adds `agent_id`, `source`, `created`,
  `fresh`, `method`, `elapsed_ms`, `status`; `PGSERVICE`/`PGSERVICEFILE` are
  cleared in the child and the password only ever travels in the environment.
  `pgx agent down --id` finds a branch under any host label, `--branch <name>`
  refuses to act on any other branch, `--agent <id>` on every lifecycle
  command. `pgx agent ls` shows an AGENT column and `deadline_kind`;
  `pgx agent doctor` reports subagent branches and the new hook audit log
  (`~/.pgx/agent/hooks.log`, ids and outcomes only).
- **Skill.** The PGX Agent Skill (Skills CLI and the Claude Code plugin,
  both 0.1.2) requires PGX 0.1.2: with a 0.1.1 binary it refuses database
  work and says why (session isolation only, subagents would share the
  parent's branch), instead of promising isolation it cannot deliver.

### Verified with real Claude Code subagents (2026-10-26)

- 3 and then **20 concurrent subagents**, no ids, no manual provisioning:
  **20/20** branches created by `SubagentStart`, **20/20** workloads in their
  own branch, **zero cross-agent contamination**, 20/20 retired by
  `SubagentStop`; **37 s** end to end for the 20-agent run; **92 MB** peak
  PGX runtime RSS; the template and the developer database unchanged
  (schema md5 identical before and after); temporary storage reclaimed after
  the grace period.
- Provisioning measured **p50 1,231 ms / p95 1,371 ms / max 1,401 ms** when
  all twenty branches started in the same instant. That is end-to-end hook
  time under 20-way concurrency, not raw clone latency: a single branch
  provisions in about 250 ms, and `pgx branch` on an idle runtime clones in
  under 30 ms.
- A subagent killed mid-transaction (`BEGIN … pg_sleep … COMMIT` interrupted
  by TaskStop) left a backend that aborted within the tested five-second
  guard with nothing committed; on 0.1.1 the same scenario committed eight
  minutes after the kill.
- Evidence and method: `docs/pgx/local-dev/validation/2026-10-26-subagent-isolation-live.md`.

### Known limitations

1. Claude Code evaluates permission rules, and in auto mode its classifier,
   on the **rewritten** Bash command. Some subagent commands may therefore
   ask for approval, or be refused by the auto-mode classifier, where the
   unwrapped command would not have. The hook never approves anything.
2. Do **not** add a broad allow rule such as `Bash(pgx agent exec:*)` on an
   agent's say-so: it would approve any command wrapped that way. Whether to
   allow the wrapper, and for which path, is the user's decision.
3. A subagent stopped by force (`TaskStop`) does not always produce
   `SubagentStop`; its branch stays on the idle TTL and the server-side
   guards protect the data until the supervisor removes it.
4. The server-side cancellation is a bounded guard, not an unconditional
   guarantee: a backend already past the point of no return of a COMMIT
   still completes it, and the guards fire within their intervals, not
   instantly. The documented guarantee begins when `pgx agent down` returns.
5. Cursor and Codex subagent auto-isolation is not live-verified in this
   release; the hooks and `pgx agent exec --agent` are available there, and
   session-level isolation is unchanged.

## What is new in 0.1.1: one Skill, every agent gets its own Postgres

```sh
npx skills add pgrundev/pgx --skill pgx     # the PGX Agent Skill for Claude Code, Cursor and Codex
```

Then ask your agent to set up isolated databases for coding agents. It
detects the project, installs PGX if needed, imports your development
database schema-only into a template, runs `pgx agent setup`, and from then
on every Claude Code session works in its own branch with `DATABASE_URL`
already set (Cursor: hooks installed, env propagation pending live
verification; Codex: `pgx agent exec -- codex`). Your PostgreSQL is read once
and never written.

- `pgx agent setup | doctor | exec -- <command> | ls` and the primitives
  `up | touch | down | env | hook | install | uninstall`; idle TTL 2h, 10-minute
  grace after a session, fail-closed sentinel when a branch cannot be made.
- `.pgx.toml`: the committed per-project source template and TTLs.
- `pgx copy --schema-only` keeps migration-bookkeeping rows
  (`schema_migrations`, `django_migrations`, `_prisma_migrations`, …).
- `pgx drop` removes the TTL record; `--json` keeps key order.
- Guide and compatibility matrix: `docs/pgx/local-dev/agents.md`.

## Install

```sh
curl -fsSL https://pgx.dev/install | sh
pgx run                             # a ready `app` database in one step
pgx branch app agent-001            # a disposable 2-hour branch
pgx agent exec -- bin/rails test    # any command in a branch of its own
```

Or download the tarball for your platform below, verify its SHA-256 against
`checksums.txt`, and extract it; `bin/pgx` is relocatable.

## Platforms

| Platform | Artifact |
|---|---|
| Linux x86-64 | `pgx-<version>-linux-amd64.tar.gz` |
| Linux arm64  | `pgx-<version>-linux-arm64.tar.gz` |
| macOS arm64  | `pgx-<version>-darwin-arm64.tar.gz` |

XML functions and ICU collations use the host's libxml2 and ICU when present.

## Verified in this release

See `docs/pgx/local-dev/validation-report.md` for the full matrix (unit,
lifecycle, SQL, compatibility vs PostgreSQL 18.6, crash recovery, isolation,
storage, concurrency, stress, and the application fixtures) and the
GO / NO-GO / BLOCKED decision.
