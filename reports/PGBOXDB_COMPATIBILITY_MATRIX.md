# PGBoxDB compatibility matrix

How each name or format from before the rename is treated. The rule, from the
task and from the trademark constraint:

* **No public alias under the previous name.** PGBoxDB installs no `pgx`
  command, publishes no `pgx` npm bin, and reads none of the previous
  environment variables or project files as configuration.
* **No silent breakage, no data loss.** Existing installations are
  *detected*, never forked or ignored silently. They are adopted only on
  request (`pgboxdb migrate`), with a tested rollback.
* **Internal persisted identifiers stay where renaming would detach data.**
  These are read in place and listed under "Legal review" so counsel can
  confirm that internal, non-public use is acceptable.

Every previous name the program knows is in one file,
`crates/bin/pgboxdb/src/legacy.rs`.

Treatment codes: **R** renamed, no fallback · **D** renamed; the previous
form is detected and reported, never used · **M** adopted by
`pgboxdb migrate` (explicit, dry run first, rollback) · **I** internal
identifier read in place (legal review) · **K** kept unchanged (not ours, or
a historical record).

## Runtime, CLI and configuration

| Item | Before | After | Treatment | Existing installation | Evidence |
|---|---|---|---|---|---|
| CLI executable | `pgx` | `pgboxdb` | R | the old binary keeps working where it is installed until its home is migrated; no `pgx` alias is shipped | `dist/npm/package.json` `bin`, `dist/assemble.sh`, `Dockerfile` |
| Subcommands, flags, exit codes, human/JSON output | — | unchanged | — | — | unit 235, all integration suites |
| `--json version` key | `pgx_version` | `pgboxdb_version` | R | consumers must read the new key (PGRun does not use it) | `cmd/version.rs` |
| `dist.json` | `share/pgx/dist.json`, key `pgx_version` | `share/pgboxdb/dist.json`, key `pgboxdb_version` | R | a new install is a new tree | `layout.rs`, `dist/assemble.sh` |
| Data home | `~/.pgx` (`$PGX_HOME`) | `~/.pgboxdb` (`$PGBOXDB_HOME`) | D + M | `init`/`run`/`serve`/`agent setup`/`git init` refuse (exit 4) to create a new home beside an initialised previous one unless `PGBOXDB_HOME` is set; `status` and friends point at `pgboxdb migrate`; `doctor` reports a previous home that is still present | `legacy::guard_new_home`, `migrate_test.sh` checks 2, 3 |
| Data home move | — | `pgboxdb migrate --apply`: one `rename(2)`, refused while running, when the target exists, or across filesystems; writes `migrated-from.json` | M | byte-identical tree after the move (checksummed) | `migrate_test.sh` checks 5–7 |
| Rollback | — | `pgboxdb migrate --rollback --apply` | M | the previous CLI starts again and sees the data | `migrate_test.sh` check 13 |
| `config.json` | absolute paths into the old home (when set) | rewritten on migrate | M | original kept as `config.json.pre-migrate`, restored on rollback | `migrate::rewrite_config_paths` |
| Cluster settings file | `postgresql.conf` → `include 'pgx.conf'` | new clusters: `include 'pgboxdb.conf'` | I | `start` regenerates whichever file the cluster's `postgresql.conf` includes; `postgresql.conf` is never rewritten | `legacy::profile_conf_name`, unit test, `migrate_test.sh` check 8 |
| Cluster data (`PG_VERSION`, catalogs, databases, OIDs) | — | unchanged | — | same engine format; no conversion | `migrate_test.sh` check 8 |
| Port, password, durability, listen | in `config.json` | unchanged | — | kept | `migrate_test.sh` check 8 (port) |
| Sockets / run dir | `$PGX_HOME/run` or `$PGX_RUN_DIR` | `$PGBOXDB_HOME/run` or `$PGBOXDB_RUN_DIR` | R | a run dir recorded in `config.json` keeps being used; inside the home it is rewritten by migrate | `cmd/mod.rs::sock_dir` |
| Environment: `HOME PORT DURABILITY LISTEN PASSWORD PG_BINDIR RUN_DIR SOURCE TTL_INTERVAL GIT GIT_SKIP` | `PGX_*` | `PGBOXDB_*` | D | not read; `init`, `run`, `start`, `serve`, `status`, `doctor` and `migrate` print `note: PGX_… is set but not read; PGBoxDB reads PGBOXDB_…` | `legacy::ignored_env_note`, `migrate_test.sh` check 4 |
| Test/fault env (`PGX_TEST_*`, `PGX_GIT_FAILPOINT`, `PGX_COPY_FAULT`) | `PGX_*` | `PGBOXDB_*` | R | test-only | — |
| Environment exported into sessions (`git exec`, `agent env`, hooks) | `PGX_AGENT_ID`, `PGX_AGENT_SESSION`, `PGX_BRANCH`, `PGX_SOURCE`, `PGX_GIT_BRANCH`, `PGX_WORKTREE`, `PGX_ISOLATION`, `PGX_ISOLATION_REASON`, `__PGX_*` | `PGBOXDB_*`, `__PGBOXDB_*` | R | scripts reading the old names must change (exporting both would be a public alias) | `cmd/agent/session.rs`, `cmd/git/env.rs` |
| TTL metadata, agent registry, copy staging, Git state | under the home | unchanged format | — | move with the home | `migrate_test.sh` checks 8–9 |
| Generated database names (Git) | `pgx_<repo>_main_<hash>` etc. | `pgboxdb_<repo>_…` for databases created from now on | I | existing names are recorded in Git state and keep resolving; nothing is renamed in the cluster | `cmd/git/naming.rs`, `migrate_test.sh` check 9 |
| Copy staging databases | `_pgxcopy_…` | `_pgboxdbcopy_…` | R | transient, dropped when a copy finishes or fails. Known gap: a `_pgxcopy_…` left by a copy that was *killed* before the rename is not recognised by the new cleanup; it is visible in `pgboxdb ls` and can be dropped by hand | `cmd/copy.rs::STAGING_PREFIX` |
| `application_name` of the CLI's connections | `pgx`, `pgx-top` | `pgboxdb`, `pgboxdb-top` | R | visible in `pg_stat_activity`; `top` excludes its own name | `pgwire.rs`, `cmd/top/collect.rs` |
| Agent branch names | `agent-<host>-<id>` | unchanged | — | — | `identity.rs` |

## Projects and repositories

| Item | Before | After | Treatment | Existing installation | Evidence |
|---|---|---|---|---|---|
| Project file | `.pgx.toml` | `.pgboxdb.toml` | D + M | a project with only `.pgx.toml` is refused (sessions fail closed instead of silently branching from `app`; Git hooks skip, they never block Git); `migrate --apply` copies it (comments updated, values unchanged), the original stays | `legacy::project_file_error`, `migrate_test.sh` check 7 |
| Git identity files | `.git/pgx-repo-id`, `.git/pgx-worktree-id` | `pgboxdb-repo-id`, `pgboxdb-worktree-id` | I + M | read as a fallback and written under the new name on first use (the old file stays), so a repository keeps its databases even before `migrate` | `repo::read_id_in`, `migrate_test.sh` checks 7, 9 |
| Git hook block | `# >>> pgx git hook >>>` | `# >>> pgboxdb git hook >>>` | D | `pgboxdb git init` replaces the old block in place, `pgboxdb git uninstall` removes either; the old block never blocks Git (it skips when its CLI is gone) | `hooks::strip_ours`, `migrate_test.sh` check 11 |
| Shell integration block | `# >>> pgx git shell integration >>>` | `# >>> pgboxdb git shell integration >>>` | D | `pgboxdb git install-shell` replaces, `uninstall --shell` removes either | `env::strip_rc_blocks`, `migrate_test.sh` check 11 |
| Agent hooks (Claude Code, Cursor, Codex; project, local and user scope) | commands containing `pgx agent hook` | `pgboxdb agent hook` | D | `pgboxdb agent install` replaces the old entries (never two hooks per event: a session would get two databases); `uninstall` removes both; `agent doctor` warns about old entries; `migrate` lists each file with the command to run | `install::remove_matching`, `migrate_test.sh` check 10 |
| Routed-command marker (in-flight Claude Bash rewrite) | `# pgx-routed` | `# pgboxdb-routed` | R | transient per command | `hook.rs` |
| Claude Code plugin and Skill | plugin `pgx`, skill `pgx`, marketplace `pgrundev`; standalone `pgx@pgx-plugin` | plugin `pgboxdb`, skill `pgboxdb`; standalone `pgboxdb@pgboxdb-plugin` | R | users install the new plugin and uninstall the old one (no alias plugin) | `.claude-plugin/marketplace.json`, `plugins/` |

## Distribution

| Item | Before | After | Treatment | Note |
|---|---|---|---|---|
| Crate / package | `pgx` (crates/bin/pgx) | `pgboxdb` (crates/bin/pgboxdb), `publish = false` (workspace) | R | not published to crates.io |
| npm package | `pgxdb` (bins `pgxdb`, `pgx`) | `pgboxdb` (bin `pgboxdb` only) | R | `pgxdb` on npm is untouched (no deprecation published) |
| npm cache / install dir | `~/.pgx/dist/<v>` | `~/.pgboxdb/dist/<v>` | R | the previous dists move with a migrated home; they are never replaced by the new installers (see the release checklist: first release ≥ 0.2.0, plus installer guard) |
| Release assets | `pgx-<v>-<os>-<arch>.tar.gz(.sha256)` | `pgboxdb-<v>-<os>-<arch>.tar.gz(.sha256)` | R | old releases keep their assets |
| Download URL | `github.com/pgrundev/pgx/releases/download/v<v>/…` | `github.com/pgrundev/pgboxdb/releases/download/v<v>/…` | R | resolves only after the repository rename |
| Installer | `https://pgx.dev/install` | `https://pgboxdb.com/install` (Worker prepared, not deployed) | R | no redirect from pgx.dev (legal) |
| Docker image | `pgx:dev`, user `pgx` (uid 10001), `/opt/pgx`, volume `/var/lib/pgx`, run `/var/run/pgx`, entrypoint `pgx-entrypoint` | `pgboxdb:dev`, user `pgboxdb` (uid 10001), `/opt/pgboxdb`, `/var/lib/pgboxdb`, `/var/run/pgboxdb`, `pgboxdb-entrypoint` | R + I | mount an existing volume at `/var/lib/pgboxdb`. Same uid, so ownership matches. The entrypoint rewrites exactly `"run_dir": "/var/run/pgx"` (the previous image's socket dir, no data) to `$PGBOXDB_RUN_DIR`, keeping `config.json.pre-migrate`. `pgx.conf` is kept by `start`. Not run locally (Docker VM disk full). |
| Platforms | macOS arm64, Linux amd64/arm64; darwin-amd64 not published; no Windows | unchanged | — | `platformTarget`, `install.sh` |

## Unchanged on purpose

| Item | Why |
|---|---|
| Engine binary `postgres`, `PGRUST_*` env, `pgrust.*` GUCs, `pgrust_*` SQL functions | PGRun production depends on them (reports/PGBOXDB_PGRUN_MIGRATION.md) |
| PostgreSQL wire protocol, SQL semantics, catalog format | no engine change (engine diff: comments plus one error message) |
| PostgreSQL internals `spgxlog*` (1557), `pgxact*` (97), `*GXact*` (36), `PGXS`/`pgxs` (34) | unrelated PostgreSQL identifiers |
| Go driver `github.com/jackc/pgx` and "pgx" in driver lists | third party |
| `pgxbench` host alias (136), ZFS dataset `tank/pgx-bench` (105) | benchmark infrastructure names; renaming breaks the documented commands |
| PGRun-owned names in docs (`pgx-agent`, `PGX_ENGINE_SHA`, `PGX_BINARY_SHA256`, `/opt/pgx/releases`, `pgrun/script/pgx`, `pgrun/docs/pgx`) | another repository's identifiers |
| Git tag `pgx-v0.1-beta.1`, tags `v0.1.0`…`v0.1.3`, GitHub releases titled PGX | history is not rewritten |
| Historical records: `benchmarks/pgboxdb/results/**` (moved, contents unchanged), `docs/pgboxdb/local-dev/validation/**` (moved, contents unchanged), `proofs/**`, `fuzz/**`, `docs/superpowers/**` | evidence must stay as recorded |

## Legal review: every remaining use of the previous name

None of these is a public alias. Each is detection, an internal identifier,
a third-party or other-owner name, or a historical record. Counsel should
confirm each category.

1. **Detection constants in source** (`crates/bin/pgboxdb/src/legacy.rs`):
   `.pgx`, `PGX_` + 11 names, `.pgx.toml`, `pgx-repo-id`,
   `pgx-worktree-id`, `pgx.conf`, `pgx agent hook`, the two marker blocks.
   They are used only to find and replace or adopt previous state.
2. **User-visible messages that name previous state** (only the user's own
   paths and variables): "found databases … at /Users/you/.pgx", "PGX_PORT is
   set but not read", ".pgx.toml is a project file from before the rename",
   hook files with "hooks from before the rename". No message uses the
   previous product name as a brand. They say "before the rename to
   PGBoxDB".
3. **Internal identifiers read in place**: `.git/pgx-repo-id` and
   `pgx-worktree-id` (fallback read), `pgx.conf` inside clusters created
   before the rename, Git database names `pgx_*` created before the rename,
   and the Docker entrypoint's match on `/var/run/pgx`.
4. **The upgrade guide** (`docs/pgboxdb/local-dev/upgrading.md`) and the
   release notes name the previous command, package and variables in a
   before/after table. This is nominative use, needed to tell users what to
   change.
5. **Historical records, infrastructure and third-party names** listed under
   "Unchanged on purpose".
6. **Outside this repository, not implemented, decision needed**: whether to
   publish a deprecation notice on npm `pgxdb`; whether GitHub's automatic
   redirect from `pgrundev/pgx` after the rename is acceptable (it cannot be
   disabled except by creating a new repository at the old name); pgx.dev
   (no redirect was implemented); the standalone repository
   `pgrundev/pgx-plugin`; PGRun's own `pgx` names
   (reports/PGBOXDB_PGRUN_MIGRATION.md §D).
