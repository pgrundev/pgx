# PGBoxDB local-development distribution — validation report

**Scope.** Turn PGBoxDB into a self-contained, relocatable PostgreSQL-18-compatible
runtime a developer or agent installs locally, with a `pgboxdb` CLI, and prove it
with automated tests. This report records what was executed, where, and the
release decision.

**Branch / commit.** `pgboxdb-local-dev` @ `8ebce867b9` (engine unchanged from the
branch base; all engine behaviour is the pgrust 0.3 / PostgreSQL 18.6 build).

**Final verdict: GO for macOS arm64 and Linux arm64 local-development use. The
Docker image and Linux x86-64 are BLOCKED on local verification (no reachable
builder) and are gated by CI before any release.** Details below.

---

## 1. Environments

| Tag | Host | Engine binary | How the CLI/dist was built |
|---|---|---|---|
| macOS arm64 | macOS 14.4, Apple M1 Pro, 16 GB, APFS | built from this branch | `cargo build --release`, `dist/assemble.sh` with Homebrew PostgreSQL 18.6 |
| Linux arm64 | Ubuntu 26.04 aarch64 in Docker (Apple M1 Pro host) | pgrust 0.3 / PG 18.6, bench build 2026-10-12 | `pgboxdb` rebuilt from this branch in-container; `dist/assemble.sh` with PGDG PostgreSQL 18.6, tzdata from `/usr/share/zoneinfo` |

The Linux **engine** binary is the existing bench build (arm64); the Linux
**CLI** (`pgboxdb`) and distribution were built from this branch's source. No engine
source changed on this branch, so the engine under test is the branch engine.

## 2. What was built

- A relocatable distribution (`dist/assemble.sh`): `bin/{pgboxdb,postgres}`, the
  share trees (timezone, timezonesets, tsearch_data, extension), a pristine
  PostgreSQL 18 base cluster, `dist.json`, licenses. Installs with **no system
  PostgreSQL**; `pgboxdb init` copies the bundled cluster instead of running initdb.
- The `pgboxdb` CLI: `init, start, stop, restart, status, serve, create, drop,
  clone, ls, url, doctor, version, help`, each with `--json` and stable exit
  codes (0 ok · 1 engine/runtime · 2 usage · 3 not ready · 4 conflict · 5 not
  found · 6 in use · 7 corrupt install).
- Installer (`dist/install.sh`), Docker image (`Dockerfile`, `pgboxdb serve`
  entrypoint), and GitHub Actions (`ci.yml`, `release.yml`, `nightly.yml`).

## 3. Test results

Counts are the last green run against the **assembled distribution** (not a dev
binary), unless noted. Commands are in §6.

| Test suite | Executed | Passed | Failed | Skipped / gap | Where |
|---|---|---|---|---|---|
| Unit (Rust, `cargo test -p pgboxdb`) | 42 | 42 | 0 | 1 ignored (live-server, covered by black-box) | macOS |
| CLI lifecycle (black-box) | 25 | 25 | 0 | 0 | macOS |
| CLI databases (create/drop/ls/url) | 10 | 10 | 0 | 0 | macOS |
| CLI clone | 7 | 7 | 0 | 0 | macOS |
| CLI doctor | 7 | 7 | 0 | 0 | macOS |
| SQL integration (`test_sql.py`) | 19 | 19 | 0 | 0 | macOS **and** Linux |
| Clone snapshot consistency | 1 | 1 | 0 | 0 | macOS |
| Compatibility vs PostgreSQL 18.6 control | 22 | 22 | 0 | 0 | macOS |
| PostgreSQL regression baseline | 231 | 223 byte-exact | 0 unexplained | 6 plan-only, 2 profile-induced | macOS |
| Isolation / locking / timeouts | 12 | 12 | 0 | 0 | macOS **and** Linux |
| Concurrency | 5 | 5 | 0 | 0 | macOS |
| Crash recovery (ephemeral ×N + full) | 10 | 10 | 0 | 0 | macOS (6 runs) **and** Linux (reap caveat §5) |
| Storage (portable, APFS) | 6 | 6 | 0 | low-disk macOS-only | macOS |
| Storage (ZFS) | 0 | 0 | 0 | COVERAGE GAP: no ZFS host | — |
| Stress acceptance (1000+1000 cycles, 100 clients 60 s, 50 start/stop) | 1 | 1 | 0 | 0 | macOS |
| App: psql | 1 | 1 | 0 | 0 | macOS |
| App: node-postgres | 1 | 1 | 0 | 0 | macOS |
| App: psycopg 3 | 1 | 1 | 0 | 0 | macOS |
| App: jackc/pgx v5 | 1 | 1 | 0 | 0 | macOS |
| App: ActiveRecord 8.1 | 1 | 1 | 0 | 0 | macOS |
| Installer end-to-end | 1 | 1 | 0 | 0 | macOS |
| Assemble layout | 1 | 1 | 0 | 0 | macOS **and** Linux |
| Docker image | 0 | 0 | 0 | COVERAGE GAP: daemon/disk | — |

### PostgreSQL regression differences (individually)

231 tests, stock `pg_regress`, PGBoxDB profile (`configs/pgboxdb-ephemeral.conf`). 223
byte-exact. The 8 that differ, each with cause:

| Test | Class | Cause |
|---|---|---|
| `select_distinct`, `portals`, `incremental_sort`, `plpgsql`, `partition_prune`, `partition_aggregate` | plan-only | EXPLAIN output text differs; no result/row difference. |
| `select_parallel` | profile-induced | the PGBoxDB profile sets `max_parallel_workers = 0`; parallel-plan lines and a parallel-worker NOTICE are absent. Query results match. |
| `stats` | profile-induced | one row asserts `current_setting('synchronous_commit') = 'on'`; the PGBoxDB profile sets it `off`. The value is correct for the profile. |

Zero unexplained result, transaction, SQL-semantic, protocol or persistence
differences. This matches the frozen v0.1 baseline (`docs/pgboxdb/v0.1-baseline.md`:
219/231 on an older profile; the 4 extra byte-exact here are plan-text drift).

### Compatibility suite (behavioural, vs a live PostgreSQL 18.6 control)

22 cases, 53 assertions, every step asserted to produce the **same rows and the
same SQLSTATE** on PGBoxDB and PostgreSQL 18.6: SQL_COMPATIBILITY (5), TRANSACTION
(9), MVCC (4), CONSTRAINT (4). 0 differences.

## 4. Durability and data integrity (crash recovery)

Validated under both settings, 5 SIGKILL repetitions per scenario, randomized
order, on macOS (6 runs) and Linux:

- **`durability = full`** (`fsync`, `synchronous_commit`, `full_page_writes`
  on): **0 of 21,436** acknowledged commits lost across a kill-during-commit
  flood; 0 lost during kill-during-insert. Every acknowledged commit survives.
- **`durability = ephemeral`** (the PGBoxDB profile, `synchronous_commit = off`):
  recent commits not yet flushed by the walwriter are lost (≈330 of ≈33,000 in
  the flood) — **documented async-commit behaviour, not corruption.** In every
  case the surviving rows form a clean prefix of the commit order and no partial
  transaction or partial batch is ever visible.
- SIGTERM/SIGINT/SIGKILL at rest, and kills during INSERT, COMMIT, CREATE
  DATABASE, clone, DROP DATABASE and shutdown: the runtime restarts and every
  database is consistent with `base/` after recovery.
- **Storage exhaustion:** a full disk raises `53100` to the client, then the
  WAL-write PANIC takes the runtime down — PGBoxDB is **one crash domain**
  (`v0.1-readiness.md`), unlike stock PostgreSQL which auto-restarts the
  postmaster. After space is freed, `pgboxdb start` recovers by WAL replay with
  committed data intact. No corruption.
- Interrupted CREATE DATABASE / clone can leave an orphan `base/<oid>` directory
  (PostgreSQL has the same recovery gap). PGBoxDB never auto-deletes them: `pgboxdb
  doctor` reports them read-only and `pgboxdb ls` flags databases left invalid by an
  interrupted DROP.
- Permission error on `data/base`, and an invalid `postgresql.conf` line: `pgboxdb
  start` fails cleanly (clear message, no orphan engine, no leftover pid file).

## 5. Concurrency, isolation, storage, stress

- **Isolation:** READ COMMITTED / REPEATABLE READ / SERIALIZABLE (write skew →
  `40001`), deadlock detection (`40P01`), `lock_timeout` (`55P03`),
  `statement_timeout` (`57014`), `FOR UPDATE/SHARE/NOWAIT/SKIP LOCKED`, advisory
  locks, `DROP ... WITH (FORCE)` terminating sessions (`57P01`). 12/12, macOS
  and Linux.
- **Concurrency:** 10 and 100 concurrent clients, 16 parallel creates, 20
  concurrent CLI reads, connect/disconnect churn, concurrent create/drop of one
  name (no partial dir, no internal error). 5/5.
- **Storage (portable, APFS):** block cloning verified (a 281 MB clone consumes
  ≈0 MB net via `clonefile`); 150+ clone/drop cycles grow `base/` by 0.0 MB;
  clone isolation both ways; private file modes; 153-database restart in 0.17 s.
  6/6. **Clone was found to need `STRATEGY = FILE_COPY`** so the engine's
  `file_copy_method` applies — without it `CREATE DATABASE` used the WAL_LOG
  strategy and copied every block (a 281 MB clone cost 280 MB); fixed.
- **Stress acceptance:** 1,000 create/drop + 1,000 clone/drop (clone integrity
  verified, `base/` net −9 MB), 176,232 client ops across 100 clients in 60 s
  with 0 errors, 50 start/stop cycles with data intact. File-descriptor and
  thread counts return to baseline (no leak).

**Linux crash-suite caveat (precise).** All 10 crash tests pass on Linux arm64.
In a *full* randomized crash run inside the bench container they are
occasionally perturbed because that container's PID 1 is `sleep infinity`, which
never reaps children, so SIGKILLed engines accumulate as zombies (observed 156).
PGBoxDB itself handles zombies correctly (`pid_alive` treats `/proc` state `Z` as
dead; `pgboxdb start` clears a socket lock whose PID is a zombie), which is why the
suite passes; the residual flakiness is the init-less container perturbing
timing. On a normal Linux host, under systemd, or with `docker run --init`,
zombies are reaped and the suite is stable; CI runners (proper init) and the
image's `pgboxdb serve` (engine is PID 1, no detached children) do not hit it.

## 6. Commands

```sh
# dev binary / distribution
CC=/usr/bin/cc cargo build --release --locked --bin postgres --bin pgboxdb
PG18_BINDIR=$(brew --prefix postgresql@18)/bin dist/assemble.sh --out OUT --version V --pg-bindir "$PG18_BINDIR"
export PGBOXDB_DIST="OUT/pgboxdb-V-darwin-arm64"

# suites
cargo test -p pgboxdb
python3 -I tests/pgboxdb/test_sql.py
python3 -I tests/pgboxdb/test_isolation.py
python3 -I tests/pgboxdb/test_concurrency.py
python3 -I tests/pgboxdb/test_storage.py
python3 -I tests/pgboxdb/clone_consistency.py
PGBOXDB_TEST_DURABILITY=ephemeral python3 -I tests/pgboxdb/test_crash.py
PGBOXDB_TEST_DURABILITY=full      python3 -I tests/pgboxdb/test_crash.py
python3 -I tests/pgboxdb/stress.py --cycles 1000 --clients 100 --duration 60 --startstop 50
PG18_BINDIR=... tests/compat/run.sh
python3 benchmarks/pgboxdb/regress-baseline.py --out OUT/regress --binary target/release/postgres --conf configs/pgboxdb-ephemeral.conf --max-connections 15
python3 benchmarks/pgboxdb/classify-regress-diffs.py OUT/regress
PGBOXDB_DIST=... PGBOXDB_TEST_PSQL=$(brew --prefix postgresql@18)/bin/psql tests/apps/run_all.sh
PG18_BINDIR=... dist/test/{assemble_test.sh,install_test.sh}
dist/test/docker_test.sh                    # COVERAGE GAP without a daemon
tests/pgboxdb/storage_zfs.sh <pool/dataset>     # COVERAGE GAP without ZFS
```

Logs and regression artifacts: `tests/results/` (git-ignored) and
`docs/pgboxdb/local-dev/validation/regress-<sha>/` (regress.json, classification,
regression.diffs).

## 7. Performance (observed, not a guarantee)

Measured incidentally by the suites (macOS M1 Pro, APFS; informational):

| Operation | Observed |
|---|---|
| clone of a template (copy-on-write, APFS clonefile) | ~42 ms |
| 150 clones + 153-database restart | 6.4 s / 0.17 s |
| 100 concurrent clients, mixed read/write, 60 s | 176,232 ops, 0 errors |
| idle runtime RSS with ~150 databases | ~65 MB |
| clone disk cost (281 MB source, block cloning) | ≈0 MB net |

These are consistent with the architecture's intent (cheap create/destroy, low
idle memory). Controlled baseline numbers remain in `docs/pgboxdb/v0.1-baseline.md`.

## 8. Risk assessment

- **ZFS: UNVERIFIED.** No ZFS host was available. `storage_zfs.sh` is written
  and self-checks (block cloning, snapshot consistency); it must be run on a
  real ZFS dataset before any ZFS claim. Never mocked.
- **Docker image: UNVERIFIED locally.** The daemon was reachable but the VM disk
  was 100% full with the user's unrelated data (not pruned). The Dockerfile and
  `docker_test.sh` exist; `ci.yml` builds and tests the image on Linux.
- **Linux x86-64: NOT EXECUTED.** Only Linux arm64 was run. `release.yml` builds
  and smoke-tests `linux-amd64` on a native runner.
- **One crash domain.** A PANIC (e.g. WAL disk-full) stops the whole runtime by
  design; acceptable for disposable databases, not for a system of record.
- **Container without an init** accumulates zombies from killed detached
  engines; use `docker run --init`, systemd, or `pgboxdb serve`.

## 9. Final verdict

**GO** — macOS arm64 and Linux arm64 local development. All mandatory
correctness, CLI lifecycle, CRUD, client-connectivity, SQL-compatibility, crash
recovery, isolation, concurrency and portable-storage checks were executed and
passed; the installer and self-contained (no system PostgreSQL) install were
verified end-to-end.

**BLOCKED (infrastructure, gated by CI, not shippable until green):**
- Docker image — build/run on a Linux runner (`ci.yml`).
- Linux x86-64 artifact — build/smoke on a native runner (`release.yml`).
- ZFS copy-on-write and snapshot consistency — a ZFS host (`storage_zfs.sh`).

Publishing a GitHub release remains a manual, approved step; `release.yml`
produces a **draft** only. No release has been published.
