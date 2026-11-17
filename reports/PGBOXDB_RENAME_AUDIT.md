# PGX → PGBoxDB rename audit

* Repository: `pgrundev/pgx` (local checkout), base `pgx/main` @ `56177328d7`
  (Merge PR #49, `pgx top`).
* Work: isolated worktree `.worktrees/pgboxdb`, branch `rebrand/pgboxdb`.
  Local commits only; nothing pushed, published, tagged or deployed.
* Commits (local, on `rebrand/pgboxdb`, base `56177328d7`):
  * `039453f6ed` rebrand: rename PGX to PGBoxDB (mechanical pass)
  * `f0fd7d81b1` pgboxdb: never fork or silently drop installations made
    before the rename (compatibility layer, `migrate`, tests, docs)
  * `96381d1a50` rebrand: review fixes (Go driver pattern, site metadata,
    site README)
  * the commit that adds `reports/` (this file)

## 1. Method

1. Inventory every case-insensitive `pgx` in tracked files (404,587 files;
   `crates/postgres-18.6-reference` excluded as vendored upstream source).
2. Classify by file area and by token, with a word-bounded matcher (`_`
   counts as a boundary, so `PGX_HOME`, `pgx_version` and
   `CARGO_BIN_EXE_pgx` match, while `spgxlog` and `pgxact` do not).
3. Mask third-party and unrelated tokens before any rewrite (§3).
4. Rewrite contents with ordered rules (URLs → npm/identifiers → env →
   words), then move paths with `git mv` so history follows.
5. Review the residue by hand. This found and fixed: names owned by PGRun,
   test-harness name collisions, help-table alignment, a byte-literal
   startup message, and shell variables.
6. Add the compatibility layer (reports/PGBOXDB_COMPATIBILITY_MATRIX.md),
   then test (reports/PGBOXDB_TEST_REPORT.md).

## 2. Inventory (base tree, case-insensitive `pgx`)

| Area | Files | Occurrences | Class | Action |
|---|---|---|---|---|
| `crates/bin/pgx` (the CLI, ~40k lines) | 79 | 1915 | 2 internal, 3 CLI contract, 4 persisted, 1 user-facing | renamed to `crates/bin/pgboxdb`; persisted names handled per the matrix |
| `docs/pgx` | 53 | 1060 | 1 | moved to `docs/pgboxdb`, rebranded; PGRun-owned names restored |
| `docs/pgx/local-dev/validation/**` | (part of the above) | — | 6 historical record | moved, contents unchanged |
| `plugins/` | 21 | 481 | 1, 5 | `plugins/pgboxdb`, plugin `pgboxdb` |
| `skills/` | 20 | 449 | 1, 3 | `skills/pgboxdb`, skill `pgboxdb` |
| `tests/pgx` | 11 | 300 | 2 | `tests/pgboxdb` |
| `benchmarks/pgx` (scripts) | 33 | 195 | 2, 5 | `benchmarks/pgboxdb`; `pgxbench` / `tank/pgx-bench` kept (infra) |
| `benchmarks/pgx/results/**` | 227 | 2337 | 6 historical | moved to `benchmarks/pgboxdb/results`, contents unchanged |
| `proofs/**` | 62 | 1868 | 6 historical | unchanged |
| `fuzz/**` | 6 | 180 | 6 (`spgxlog`, `pgxact` in corpora) | unchanged |
| `docs/superpowers/**` | 5 | 246 | 6 historical design records | unchanged |
| `dist/npm` | 6 | 186 | 5 | package `pgboxdb`, bin `pgboxdb` only, files renamed |
| `dist/pgx.dev` | 7 | 84 | 1, 5 | moved to `dist/pgboxdb.com`; Worker targets `pgboxdb.com/install` (not deployed) |
| `dist/{install.sh,assemble.sh,layout.md,docker,test,macos,release-notes.md,THIRD-PARTY-LICENSES.md}` | 17 | ~245 | 5 | renamed asset names, prefixes, env |
| `.github/workflows` | 6 | 162 | 5 | renamed jobs, artifacts, guards `github.repository == 'pgrundev/pgboxdb'` |
| `README.md`, `LLM.md`, `CONTRIBUTORS.md`, `Dockerfile`, `.claude-plugin`, `configs/` | 8 | ~255 | 1, 5 | rebranded; `configs/pgx-*.conf` → `configs/pgboxdb-*.conf` |
| `crates/backend`, `crates/_support` (engine) | 34 | 257 | mostly 6; 7 comment lines and 1 message | rewrote comments and the libxml2 error message only; no identifier |
| `tests/apps`, `tests/compat` | 18 | 95 | 2 (+6 `jackc/pgx`) | rebranded; Go driver fixture `tests/apps/go` untouched |

Classes: 1 user-facing branding · 2 internal code identifier · 3 public API
or CLI contract · 4 persisted or serialized data · 5 build or distribution
identifier · 6 third-party, unrelated or historical.

## 3. Masked (never rewritten)

| Token | Count | Reason |
|---|---|---|
| `spgxlog*` / `Spgxlog*` | 1557 | PostgreSQL SP-GiST WAL records |
| `pgxact*`, `*GXact*` | 97 / 36 | PostgreSQL proc array / two-phase internals |
| `PGXS`, `pgxs` | 34 | PostgreSQL extension build system |
| `github.com/jackc/pgx`, `pgx v5`, `pgx.Connect`, "node-postgres / pgx / lib/pq" | 11+ | Go PostgreSQL driver |
| `pgxbench`, `PGXBENCH*` | 136 | benchmark VPS ssh alias and its env (infrastructure) |
| `tank/pgx-bench`, `tank/pgx-rs*` | 105 | ZFS datasets on that host |
| `pgx-agent`, `PGX_ENGINE_SHA`, `PGX_BINARY_SHA256`, `PGX_AGENT_*`, `/opt/pgx/releases`, `pgrun/script/pgx`, `pgrun/docs/pgx` | 10+ | owned by PGRun (another repository) |
| `pgx-v0.1-beta.1` | 2 | existing git tag |

## 4. Public contracts and persisted names (decisions)

| Name | Class | Decision |
|---|---|---|
| binary `pgx` | 3 | `pgboxdb`; no alias |
| `PGX_*` read by the CLI (11) | 3 | `PGBOXDB_*`; old names detected and reported, not read |
| `PGX_*` exported to sessions (8) + `__PGX_*` shell internals | 3 | `PGBOXDB_*` |
| `--json version` `pgx_version`, `dist.json` `pgx_version`, `share/pgx/` | 3, 5 | renamed |
| `~/.pgx` | 4 | `~/.pgboxdb`; refuse to fork; `pgboxdb migrate` |
| `.pgx.toml` | 4 | `.pgboxdb.toml`; refuse the old one; migrate copies |
| `.git/pgx-repo-id`, `pgx-worktree-id` | 4 | new names; old read as fallback (internal) |
| `pgx.conf` in clusters | 4 | new clusters `pgboxdb.conf`; old clusters keep theirs (internal) |
| `pgx_<repo>_…` Git databases | 4 | new prefix for new databases; existing keep names (recorded in state) |
| hook / rc markers, `pgx agent hook` | 4 | new markers; old entries replaced in place by the install commands |
| `pgx-routed` marker, `_pgxcopy_` staging, `application_name` | 4 (transient) | renamed |
| npm `pgxdb` (bins `pgxdb`, `pgx`) | 5 | `pgboxdb` (bin `pgboxdb`) |
| release assets `pgx-<v>-<target>.tar.gz` | 5 | `pgboxdb-<v>-<target>.tar.gz` |
| Docker paths and user | 5, 4 | renamed; same uid; old volume supported |
| `pgx.dev`, `pgx.dev/install` | 1, 5 | `pgboxdb.com`, `pgboxdb.com/install`; docs at `pgbox.dev/db`; no redirect |
| `pgrundev/pgx`, `pgrundev/pgx-plugin` | 5 | `pgrundev/pgboxdb`, `pgrundev/pgboxdb-plugin` (repositories not renamed) |

## 5. Implemented subcommands (help vs code)

Checked against `cli.rs`, `cmd/agent/mod.rs` and `cmd/git/mod.rs`: `run`,
`init`, `start`, `serve`, `stop`, `restart`, `status`, `create`, `drop`,
`clone`, `branch`, `ls`, `ttl`, `url`, `copy`, `top`, `doctor`, `version`,
`help`, `supervise` (internal), **`migrate` (new)**; `agent setup | doctor |
exec | ls | up | touch | down | env | activity | hook | install | uninstall`;
`git init | status | ls | sync | env | exec | reset | prune | doctor |
uninstall | shell | install-shell | hook`. All the examples in the task
(`--version`, `agent up/ls/down`, `git init/status/sync`) exist. `--version`
is accepted as an alias of `version`, unchanged behaviour. No command was
invented other than `migrate`, which the compatibility requirement needs.

## 6. Changed files

Run `git diff --stat 56177328d7..rebrand/pgboxdb` for the full list.
Groups:

* moved and rebranded: `crates/bin/pgx → crates/bin/pgboxdb` (79),
  `docs/pgx → docs/pgboxdb` (61), `skills/pgx → skills/pgboxdb`,
  `skills/tests/pgx → skills/tests/pgboxdb`, `plugins/pgx → plugins/pgboxdb`,
  `plugins/tests/pgx → plugins/tests/pgboxdb`, `tests/pgx → tests/pgboxdb`,
  `benchmarks/pgx → benchmarks/pgboxdb`, `configs/pgx* → configs/pgboxdb*`,
  `dist/pgx.dev → dist/pgboxdb.com`, `dist/npm/{bin,lib}/pgxdb.js →
  pgboxdb.js`
* edited in place: `Cargo.toml`, `Cargo.lock`, `.github/workflows/*.yml`
  (6), `Dockerfile`, `dist/*`, `README.md`, `LLM.md`, `CONTRIBUTORS.md`,
  `.gitignore`, `.claude-plugin/marketplace.json`, `tests/apps/**`,
  `tests/compat/**`, 7 engine files (comments, one message)
* new: `crates/bin/pgboxdb/src/legacy.rs`,
  `crates/bin/pgboxdb/src/cmd/migrate.rs`, `dist/test/migrate_test.sh`,
  `docs/pgboxdb/local-dev/upgrading.md`, `reports/PGBOXDB_*.md`

## 7. Risks

| Risk | Mitigation / status |
|---|---|
| A user's scripts read `PGX_*` exported variables | documented in the upgrade guide; deliberate (no alias) |
| Developers' committed `.claude/settings.json` still call `pgx agent hook` (silent no-op once `pgx` is gone) | `agent doctor` warns; `migrate` lists the files; `agent install` replaces them. PGRun's own copy: PGRUN_MIGRATION B1 |
| A new installer replaces a previous same-version dist under the migrated home | first PGBoxDB release must be ≥ 0.2.0; installers refuse to replace a pre-rename dist (guard added) |
| Release URLs 404 until the repository is renamed | ordering in the release checklist |
| `release.yml` draft job drops assets (open bug from v0.1.3) | not fixed here (out of scope); flagged in the checklist |
| GitHub redirects `pgrundev/pgx` → `pgrundev/pgboxdb` automatically | legal review item |
| Docker image not built or run locally (Docker VM disk full) | CI `docker` job covers the build; the entrypoint snippet was unit-tested |
| Linux not run locally | CI matrix (ubuntu amd64 + arm64) |
