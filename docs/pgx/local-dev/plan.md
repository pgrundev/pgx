# PGX local development distribution — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A self-contained, relocatable PGX distribution with a `pgx` CLI (`init`, `start`, `stop`, `restart`, `status`, `create`, `drop`, `clone`, `ls`, `url`, `doctor`, `version`) that installs without a system PostgreSQL and is proven by automated lifecycle, compatibility, crash, isolation, storage and stress tests.

**Architecture:** The engine (`postgres`, the pgrust-based server) is unchanged. A new Rust binary crate `crates/bin/pgx` manages one cluster under `$PGX_HOME` (default `~/.pgx`) by spawning the engine beside itself and talking to it over the Unix socket with a minimal built-in wire-protocol client. The distribution is a directory tree (`bin/`, `share/`) assembled by `dist/assemble.sh` from the release build, the PostgreSQL 18 share trees the engine reads (timezone, timezonesets, tsearch_data) and a pre-initialised PostgreSQL 18 cluster (`share/pgx/base-cluster/`) that `pgx init` copies instead of running `initdb` at install time.

**Tech Stack:** Rust 1.96 (workspace toolchain), `serde_json` and `libc` (already in the lock file); Python 3 standard library for black-box test suites (the wire client in `benchmarks/pgx/pgxbench.py` is the precedent); Ruby `pg` + minitest for the behavioural compatibility suite; GitHub Actions for CI and releases; POSIX `sh` for the installer.

**Spec:** the task brief "Make PGX Production-Quality for Local Development" (pasted 2026-10-17); the audit below is section 1 of that brief.

## Audit findings (brief §1)

- **Engine binary:** `crates/backend/main/main_main` builds `postgres` (57 MB release, `target/release/postgres`, last built 2026-10-11 from the libxml2 fix tree). Single process, threads per session. No `--boot`/`initdb` mode exists in this tree: clusters are created by stock PostgreSQL 18 `initdb` and the engine opens the resulting stock data directory.
- **Share-directory consumers:** `pgtz` (timezone), `tzparser` (timezonesets), tsearch (`tsearch_data`), `commands/extension` (`extension/`). Each resolves `PGRUST_PGSHAREDIR` (runtime env) → `option_env!` (build) → `pg_path::get_share_path(my_exec_path)`, which with the default build constants resolves to `<bindir>/../share`. `PGRUST_TZDIR` overrides the timezone dir alone. Contrib extension scripts and `tsearch_data` files are staged by `main_main/build.rs` into `target/<profile>/share/{extension,tsearch_data}` and are searched before the system dir (`control.rs:staged_extension_dir`), so they are vendored, not taken from the system PostgreSQL.
- **Runtime libraries:** libxml2 and ICU are `dlopen`ed, never linked; absence degrades (0A000 for XML, status reports `libraries.*.available=false`). macOS ships both; Debian/Ubuntu need `libxml2`/`libicu` packages. Linux hosts with libxml2 ≥ 2.14 need the soname probe fix (commit b2ac5558e8, on this branch).
- **Lifecycle primitives (docs/pgx/pgrun-interface.md):** `CREATE DATABASE d TEMPLATE t` (with `file_copy_method = clone`: APFS `clonefile`, Linux `copy_file_range`, ZFS block cloning), `DROP DATABASE … WITH (FORCE)`, `pgrust_seal_template()`, mint-on-connect `<prefix><template>__<token>`, `pgrust_pin_database()`, `pgrust_runtime_status()` (JSON; `/proc` reads for memory, Linux-only fields). Janitor crate: `crates/backend/postmaster/janitor`.
- **Storage:** `configs/pgx-ephemeral.conf` is the PGX profile: `fsync = off`, `synchronous_commit = off`, `full_page_writes = off`, `wal_level = minimal`, `max_connections = 20`, `shared_buffers = 16MB`, `file_copy_method = clone`, autovacuum skip-idle. ZFS specifics in `docs/pgx/zfs-cow.md`. There is no suspend/resume feature in this tree (no doc or code mentions it); "suspend/resume" in the brief maps to stop/start with persistence.
- **Existing tests:** per-crate Rust unit tests; `crash-simulator/` and `xlogrecovery/tests/sim_crash_sweep.rs` (deterministic simulated crash recovery); PostgreSQL 18.6 regression suite vendored at `crates/postgres-18.6-reference/src/test/regress`, run by `benchmarks/pgx/regress-baseline.py` with stock `pg_regress` and classified by `classify-regress-diffs.py`; agent/framework workloads `benchmarks/pgx/linux/agent-workload.py` (Linux only); `benchmarks/pgx/pgxbench.py` (startup, idle, conns). PGRun's `test/pgx_compat` (Ruby, minitest + pg) compares PGX against a PostgreSQL control. **No CI workflows exist** (`.github/` absent on the public main). No Dockerfile, no release artifacts, no installer.
- **Platform assumptions:** macOS and Linux, 64-bit; stack needs `ulimit -s 65520` + `RUST_MIN_STACK=33554432`; Unix socket path ≤ 103 bytes; `/proc` for memory figures (absent on macOS, status fields degrade); `io_method=sync` recommended at launch. Windows is out of scope.
- **Known limitations (docs/pgx/v0.1-readiness.md, README "Status"):** not crash-safe against OS crash/power loss under the profile; one process = one crash domain; no C extensions, PL/Python/Perl/Tcl; residual lifecycle memory retention (~0.2 KB per create→drop); XML needs libxml2 on the host.
- **Baseline numbers (docs/pgx/v0.1-baseline.md):** ~0.6–0.7 MB per idle database, ~659 MB for 1,000 idle databases, warm mint 3–12 ms, 674 MB template clone ≈ 85 ms on ZFS.

## Global Constraints

- Engine behaviour and the measured baseline numbers are preserved; this work adds a CLI and packaging, it does not change the engine except where a defect is found (each such fix is its own commit with a regression test).
- `pgx` installs and runs with **no system PostgreSQL** on the target machine; the only runtime dependencies are libc and, optionally, libxml2/ICU for XML and ICU collations.
- Installation is relocatable; no Homebrew or Linux FHS path is hard-coded; executable and home paths may contain spaces.
- The server binds to `127.0.0.1` and `::1` only; the Unix socket uses trust auth, TCP uses a generated per-install password stored mode 0600 in `$PGX_HOME/config.json`.
- Durability is a documented, explicit setting: `ephemeral` (the PGX profile: survives server crash, not OS crash) is the default; `full` (`fsync = on`, `synchronous_commit = on`, `full_page_writes = on`) is `pgx init --durability full`. Neither is changed silently.
- Database names: 1–63 bytes, `^[A-Za-z_][A-Za-z0-9_-]*$`, always sent as quoted identifiers; `postgres`, `template0`, `template1` cannot be created, dropped or cloned onto.
- Exit codes: 0 ok · 1 engine/runtime error · 2 usage · 3 not initialised or not running · 4 conflict (already exists, already running, port in use) · 5 not found · 6 in use (sessions hold the database) · 7 corrupted or incomplete installation.
- No pushes of tags, no package publishing, no GitHub Release, no deploy, no change to PGRun production anything.
- Tests are never skipped silently: a suite that cannot run in an environment prints the exact gap and exits non-zero unless invoked with its documented `--allow-missing-<dep>` flag, which prints `COVERAGE GAP:` lines.

## Review Focus

1. `HOME` with spaces or a long path (socket path > 103 bytes): `pgx init` must refuse with a clear message and `--run-dir` escape hatch rather than fail at `pgx start` (Task 4 test `init_refuses_long_socket_path`).
2. Two `pgx start` races (agent runs it from two shells): exactly one server, the loser reports "already running" with exit 0 (Task 5 test `concurrent_start_single_server`).
3. Stale `postmaster.pid` whose pid now belongs to an unrelated process: `pgx start` must verify the pid is our engine (command line contains the data dir) before treating it as running (Task 5 test `stale_pid_reused_by_other_process`).
4. `pgx drop` while clients are connected: without `--force` it fails with exit 6 and the data is untouched; with `--force` sessions are terminated first (Task 6 test `drop_in_use_requires_force`).
5. A dropped or failed clone leaves no directory under `base/` (Task 7 test `failed_clone_leaves_no_partial_dir`).

---

### Task 1: Distribution layout and assembly script

**Files:**
- Create: `dist/assemble.sh`, `dist/layout.md`, `dist/THIRD-PARTY-LICENSES.md`
- Test: `dist/test/assemble_test.sh`

**Interfaces:**
- Produces: `dist/assemble.sh [--out DIR] [--pg-bindir DIR] [--engine PATH] [--pgx PATH] [--version V]` → `DIR/pgx-<V>-<os>-<arch>/{bin/pgx,bin/postgres,share/{timezone,timezonesets,tsearch_data,extension},share/pgx/base-cluster/,share/pgx/dist.json,LICENSE,NOTICE,THIRD-PARTY-LICENSES.md}` plus `pgx-<V>-<os>-<arch>.tar.gz` and `.sha256`. `dist.json`: `{"pgx_version","engine_commit","postgres_compat":"18.6","initdb_version","os","arch","built_at"}`.
- `base-cluster` = `initdb --no-locale --encoding UTF8 -U postgres -A trust` output minus `postgresql.conf`, `pg_hba.conf`, `pg_ident.conf`, `postmaster.opts` (pgx writes these), with `postgresql.auto.conf` emptied.

- [ ] Write `dist/test/assemble_test.sh`: runs assemble into a temp dir and asserts every path above exists, `dist.json` parses, the tarball lists `bin/postgres`, the sha256 file verifies.
- [ ] Run it: FAIL (no script).
- [ ] Implement `dist/assemble.sh` (POSIX sh + tar + shasum/sha256sum; os/arch from `uname`; refuses to run if `initdb --version` is not 18.x).
- [ ] Run the test: PASS. Commit `dist: relocatable distribution layout and assembly script`.

### Task 2: `pgx` crate skeleton, layout resolution, config

**Files:**
- Create: `crates/bin/pgx/Cargo.toml`, `src/main.rs`, `src/cli.rs` (arg parsing, help, exit codes), `src/layout.rs` (install prefix + home), `src/config.rs`, `src/errors.rs`
- Modify: `Cargo.toml` workspace members (after `crates/bin/psql`)

**Interfaces:**
- `layout::Install::discover() -> Result<Install>`: from `current_exe().canonicalize()`; fields `prefix, engine: PathBuf (bin/postgres), share: PathBuf, base_cluster: PathBuf, dist_json: PathBuf`; `Install::verify(&self) -> Vec<Problem>` (each missing file is a `Problem`; `exit 7` when fatal).
- `layout::Home::resolve(env) -> Home` (`$PGX_HOME` else `$HOME/.pgx`): `root, data, run, log_file, config_file`.
- `config::Config { port: u16, password: String, durability: Durability, listen: Vec<String>, run_dir: Option<PathBuf>, created_at: String }` JSON round trip; written with mode 0600.
- `cli::ExitCode` enum with the 8 codes; `cli::parse(args) -> Command` (hand-rolled: subcommands, `--json`, `--help`, `--version`).

- [ ] Unit tests in `cli.rs`, `layout.rs`, `config.rs`: `parse_global_json_flag`, `unknown_subcommand_is_usage_error`, `home_defaults_to_dot_pgx`, `home_env_override`, `install_verify_reports_each_missing_file`, `config_roundtrip`, `config_file_mode_0600`.
- [ ] Implement; `cargo test -p pgx`: PASS. Commit `pgx: CLI skeleton, layout and config`.

### Task 3: Minimal wire-protocol client

**Files:**
- Create: `crates/bin/pgx/src/pgwire.rs`
- Test: unit tests in the module (message encoding; decoding against byte fixtures recorded from a live server)

**Interfaces:**
- `pgwire::Client::connect_unix(sock_dir: &Path, port: u16, user: &str, db: &str, timeout: Duration) -> Result<Client>`; `Client::simple_query(&mut self, sql: &str) -> Result<Vec<Row>>` where `Row = Vec<Option<String>>`; `Client::columns() -> &[String]`; errors carry `sqlstate: String, message: String`.
- Trust auth only (socket); any `AuthenticationRequest` other than Ok is an error "unexpected authentication request".

- [ ] Tests: `startup_message_bytes`, `parse_error_response_fields`, `parse_rows_with_nulls`, `query_against_live_server` (ignored unless `PGX_TEST_SOCKET` is set).
- [ ] Implement; PASS. Commit `pgx: built-in wire client`.

### Task 4: `pgx init` and `pgx version`

**Files:**
- Create: `crates/bin/pgx/src/cmd/init.rs`, `src/cmd/version.rs`, `src/conf.rs` (renders `pgx.conf`, `pg_hba.conf`)
- Test: `crates/bin/pgx/tests/lifecycle.rs` (black-box, needs `PGX_DIST` pointing at an assembled prefix; fails loudly when unset)

**Interfaces:**
- `init` steps: verify install (exit 7) → refuse if `data/PG_VERSION` exists (exit 4, `--force` re-inits only if server not running) → check socket path length → choose port (`--port`, else 5432 if free else first free in 5433..5500) → copy `base-cluster` to `data` (0700, atomic: copy to `data.tmp` then rename) → write `postgresql.conf` (base + `include 'pgx.conf'`), `pgx.conf` (profile + `listen_addresses`, `port`, `unix_socket_directories`, `max_connections = 120`, `timezone = 'UTC'`, durability lines), `pg_hba.conf` (local trust; `127.0.0.1/32`, `::1/128` scram-sha-256) → start socket-only, `ALTER ROLE postgres PASSWORD` → stop → write `config.json`.
- `conf::render_pgx_conf(&Config) -> String`, `conf::render_hba() -> String`.

- [ ] Tests: `init_creates_cluster_and_config`, `init_twice_is_conflict`, `init_refuses_long_socket_path`, `init_picks_free_port_when_5432_busy` (bind 5432 in the test first), `version_prints_engine_and_compat`.
- [ ] Implement; PASS. Commit `pgx: init and version`.

### Task 5: `pgx start`, `stop`, `restart`, `status`

**Files:**
- Create: `src/cmd/start.rs`, `stop.rs`, `status.rs`, `src/process.rs` (pid file, liveness, spawn, signals)
- Test: `tests/lifecycle.rs`

**Interfaces:**
- `process::read_pid(data) -> Option<PidFile{pid, data_dir, port, sock_dir}>`; `process::is_our_engine(pid, data_dir) -> bool` (Linux `/proc/<pid>/cmdline`, macOS `ps -o command= -p`); `process::spawn_engine(&Install, &Home, &Config) -> Result<u32>` (setsid, RLIMIT_STACK ≥ 64 MB soft, env `RUST_MIN_STACK=33554432 PGRUST_PGSHAREDIR=<share> PGRUST_TZDIR=<share>/timezone`, args `-D data -k run -p port -c io_method=sync -c max_stack_depth=60000`, stdout+stderr appended to `log/pgx.log`); `process::wait_ready(sock, port, timeout) -> Result<()>`; `process::stop(pid, mode: Fast|Immediate, timeout) -> Result<()>`.
- Start is idempotent (running → exit 0, prints status); a start lock file `run/start.lock` (flock) serialises concurrent starts.
- `status` JSON: `{running, pid, port, socket, uptime_s, data_dir, durability, databases: n, memory_rss_bytes, engine_commit, libraries}` from `pgrust_runtime_status()` where available.

- [ ] Tests: `start_stop_roundtrip_select_1`, `start_twice_is_noop`, `stop_when_not_running_is_ok`, `stale_pid_reused_by_other_process`, `port_conflict_is_exit_4`, `concurrent_start_single_server` (4 parallel starts → one pid), `restart_preserves_data`, `stop_leaves_no_orphan` (no `postgres` with our data dir after stop).
- [ ] Implement; PASS. Commit `pgx: start, stop, restart, status`.

### Task 6: `pgx create`, `drop`, `ls`, `url`

**Files:**
- Create: `src/cmd/db.rs`, `src/names.rs`
- Test: `tests/databases.rs`

**Interfaces:**
- `names::validate(name) -> Result<DbName>`; `DbName::quoted() -> String`; reserved list.
- `url`: `postgres://postgres:<pw>@127.0.0.1:<port>/<db>`; `--socket` → `postgres://postgres@/<db>?host=<run dir>&port=<port>`; `url` fails exit 5 if the database does not exist (`--no-check` skips).
- `ls --json`: `[{name, size_bytes, is_template, connections}]`.

- [ ] Tests: `create_then_connect_with_psql_or_client`, `create_duplicate_is_exit_4`, `invalid_name_is_usage_error` (`"a;drop"`, 64-char, leading digit), `reserved_names_refused`, `drop_in_use_requires_force`, `drop_missing_is_exit_5`, `url_has_password_and_port`, `ls_json_shape`.
- [ ] Implement; PASS. Commit `pgx: create, drop, ls, url`.

### Task 7: `pgx clone`

**Files:**
- Create: `src/cmd/clone.rs`
- Test: `tests/clone.rs`, `tests/pgx/clone_consistency.py`

**Interfaces:**
- `clone <src> <dst>` → `CREATE DATABASE dst TEMPLATE src`; 55006 → exit 6 with hint "disconnect clients from <src> (or pgx seal is not needed for clone)"; `--json` prints `{source, target, elapsed_ms, method: "clone"|"copy"}` (method from `pg_settings file_copy_method` + filesystem detection in `doctor`).

- [ ] Tests: `clone_is_isolated` (write in clone, source unchanged, and vice versa), `clone_source_in_use_is_exit_6`, `failed_clone_leaves_no_partial_dir` (clone onto an existing name, then list `base/`), `concurrent_clones_8` (8 threads, all succeed, all isolated), `clone_consistency.py`: fill source with 100k rows + checksum, clone under a concurrent writer on another database, verify clone checksum = source checksum at clone time (source frozen by PG's connection rule).
- [ ] Implement; PASS. Commit `pgx: clone`.

### Task 8: `pgx doctor`

**Files:**
- Create: `src/cmd/doctor.rs`, `src/fsdetect.rs`
- Test: unit tests + `tests/doctor.rs`

**Interfaces:**
- Checks (each `{name, status: ok|warn|fail, detail, hint}`): install files; `dist.json` present; `data` present/version; stale pid; port free or ours; socket dir length and permissions; disk free ≥ 1 GB (warn); stack rlimit raisable; filesystem of `data` and whether it supports block cloning (`apfs` → yes; Linux `statfs` f_type btrfs/xfs/zfs → yes/likely, ext4 → no); libxml2/ICU from `pgrust_runtime_status()` when running; `~/.pgx/config.json` mode.
- Exit 0 if no `fail`, else 1.

- [ ] Tests: `doctor_reports_missing_share_dir_as_fail` (fixture prefix missing `share/timezone`), `doctor_ok_on_fresh_init`, `fsdetect_apfs_is_clone_capable`.
- [ ] Implement; PASS. Commit `pgx: doctor`.

### Task 9: Black-box SQL integration suite

**Files:**
- Create: `tests/pgx/lib/pgclient.py` (stdlib wire client, extended from `benchmarks/pgx/pgxbench.py`, adds scram-sha-256 over TCP), `tests/pgx/lib/harness.py` (temp `PGX_HOME`, runs the `pgx` CLI, cleanup trap), `tests/pgx/test_sql.py`, `tests/pgx/run.sh`

**Interfaces:**
- `harness.Pgx(dist_prefix)`: `.init()`, `.start()`, `.stop()`, `.cli(*args) -> (code, stdout, stderr)`, `.connect(db) -> Client`, context-managed cleanup that kills a leftover engine.
- Tests (unittest): DDL, DML, joins/GROUP BY, transactions + rollback, PK/FK/unique violations with SQLSTATEs, indexes used (`EXPLAIN` mentions Index Scan), JSONB, arrays, sequences/identity, views, prepared statements (extended protocol: Parse/Bind/Execute), `COPY FROM STDIN`/`TO STDOUT`, isolation levels (RC vs RR snapshot), 10 concurrent clients, error recovery (aborted txn → 25P02 → ROLLBACK recovers). Each asserts result content.

- [ ] Write tests, run against the assembled dist: all pass or each failure is filed as an engine defect in the report. Commit `tests: black-box SQL integration suite`.

### Task 10: Compatibility suite (behavioural, against a PostgreSQL 18 control)

**Files:**
- Create: `tests/compat/` — port of PGRun `test/pgx_compat/*.rb` (same author) with `PGX_COMPAT_URL` + `PGX_COMPAT_CONTROL_URL`; `tests/compat/README.md`; `tests/compat/run.sh` (starts a control PostgreSQL 18 from `PG18_BINDIR` into a temp dir when the control URL is unset and `PG18_BINDIR` is set).
- Modify: `benchmarks/pgx/regress-baseline.py` only if needed to accept `--socket-dir/--port` of a `pgx`-managed server (keep `pg_regress` path as is).

- [ ] Run the regression baseline against the `pgx`-started server, classify with `classify-regress-diffs.py`, record counts (byte-exact / order-only / semantic) in the report; compare with `docs/pgx/v0.1-baseline.md`.
- [ ] Run the ported suite: PGX == PostgreSQL 18 for every step, or each difference listed with evidence. Commit `tests: compatibility suite and regression baseline runner`.

### Task 11: Crash recovery, isolation and storage tests (explicitly requested)

**Files:**
- Create: `tests/pgx/test_crash.py`, `tests/pgx/test_isolation.py`, `tests/pgx/test_storage.py`, `tests/pgx/storage_zfs.sh`

**Interfaces / cases:**
- Crash: SIGTERM, SIGINT, SIGKILL at rest; SIGKILL during a 200k-row INSERT loop (committed batches survive, in-flight batch absent); SIGKILL between `COMMIT` sent and acknowledged (row either fully present or absent, never partial); SIGKILL during `CREATE DATABASE`, during clone, during `DROP DATABASE`, during shutdown; restart succeeds every time; `pg_database` consistent with `base/` (no orphan dirs, no missing dirs); each scenario × 5 repetitions; both `ephemeral` and `full` durability. Storage exhaustion via a small tmpfs/ram disk (macOS `hdiutil`, Linux `tmpfs`) → 53100, server stays up, after space returns writes resume. Permission error: `chmod 000 data/base` → `pgx start` fails with a clear message, exit 1, no pid left. Invalid config line in `postgresql.conf` → start fails with the engine's message, stale state cleaned.
- Isolation: READ COMMITTED vs REPEATABLE READ vs SERIALIZABLE (40001 on write skew), uncommitted invisibility, row locks `FOR UPDATE` block/unblock, deadlock detected (40P01) within `deadlock_timeout`, `lock_timeout` 55P03, `statement_timeout` 57014, long transaction does not block unrelated writers, drop-with-FORCE terminates with 57P01 on the client.
- Storage: portable backend (APFS here, ext4/tmpfs in CI): data integrity after 200 clone/drop cycles (checksums), disk usage of a clone ≪ source on clone-capable fs, cleanup after drop (`base/<oid>` gone, `du` returns), failed clone recovery, low-disk clone fails cleanly. `storage_zfs.sh`: requires a ZFS dataset argument; takes `zfs snapshot` of the data dataset mid-workload, mounts the snapshot clone read-only, starts a second engine on it, verifies checksums and that no uncommitted row is visible; `pgx clone` on ZFS uses block cloning (`zfs get written` of the clone ≪ source). Exits 2 with `COVERAGE GAP: ZFS` when no dataset is given.

- [ ] Write and run; record per-case results. Commit `tests: crash recovery, isolation and storage suites`.

### Task 12: Concurrency and stress suites

**Files:**
- Create: `tests/pgx/test_concurrency.py`, `tests/pgx/stress.py` (`--cycles N --clients N --duration S --soak`), `tests/pgx/leakcheck.py` (RSS, fd count via `lsof`/`/proc`, thread count sampled every 5 s)

- [ ] Cases: 10 and 100 concurrent connections doing reads/writes; 16 parallel creates; concurrent create+drop of the same name (exactly one winner, no partial dir); 8 concurrent clones; drop while connected; 20 concurrent `pgx` CLI invocations (`ls`/`status`/`create`); randomized operation sequences (seeded) with invariant checks after each run.
- [ ] Acceptance: 1,000 create/drop cycles, 1,000 clone/drop cycles, 100 clients for 60 s mixed load, 50 start/stop cycles; RSS growth across the run reported, fd/thread counts return to baseline after cycles. `--soak 3h` for the extended suite (documented, not run by default).
- [ ] Commit `tests: concurrency and stress suites`.

### Task 13: Real application fixtures

**Files:**
- Create: `tests/apps/{psql,node,python,go,rails}/` each with `run.sh` reading `DATABASE_URL`, and `tests/apps/run_all.sh` that creates a database per app with `pgx create`, runs, drops.

- [ ] psql: `\copy`, DDL/DML script, `\d`; node (`pg` 8.x): parameterised inserts, transactions, LISTEN/NOTIFY smoke; python (`psycopg` 3 in a venv): COPY, server-side cursors, JSONB adapt; go (`jackc/pgx/v5`): batch, tx, `CopyFrom`; rails (Rails 8, ActiveRecord): `db:create`, `db:schema:load`, migrations up/down (`db:rollback`), `db:seed`, transactions with savepoints, `parallelize(workers: 4)` tests, `db:drop`.
- [ ] Each run prints `APP <name>: PASS|FAIL|COVERAGE GAP (<missing tool>)`. Commit `tests: application fixtures`.

### Task 14: Installer, Docker, CI and release workflows

**Files:**
- Create: `dist/install.sh`, `dist/test/install_test.sh` (serves a local artifact over `python3 -m http.server`, installs into a temp dir, verifies), `Dockerfile`, `dist/docker/entrypoint.sh`, `dist/test/docker_test.sh`, `.github/workflows/ci.yml`, `.github/workflows/release.yml`, `.github/workflows/nightly.yml`, `dist/release-notes.md`

**Interfaces:**
- `install.sh`: env `PGX_VERSION` (default latest via GitHub API or pinned), `PGX_INSTALL_DIR` (default `$HOME/.pgx/dist/<version>`), `PGX_BIN_DIR` (default `$HOME/.local/bin`, symlink `pgx`), `PGX_BASE_URL` (default `https://github.com/pgrundev/pgx/releases/download`); detects `uname -s/-m` → `linux-amd64|linux-arm64|darwin-arm64`; downloads tarball + `.sha256` to a temp dir, verifies, extracts to `<dir>.partial`, renames atomically, runs `pgx version`, prints PATH hint; never uses sudo; exits 1 on unsupported platform with the supported list.
- Docker: `ubuntu:24.04` + `libxml2 libicu74`, dist tarball, `VOLUME /var/lib/pgx`, `EXPOSE 5432`, entrypoint `pgx init` on first run (`PGX_LISTEN=0.0.0.0`, `PGX_PASSWORD` env), then runs the engine in the foreground.
- `ci.yml` (PR): fmt+clippy of the `pgx` crate, `cargo test -p pgx`, release build with cache, assemble, SQL + databases + clone + doctor suites, installer test, Docker build+test on Linux. `release.yml` (tag `v*`): matrix `ubuntu-24.04`, `ubuntu-24.04-arm`, `macos-15`; assemble; run the lifecycle suite against the packaged artifact; upload artifacts + `checksums.txt` as a **draft** release. `nightly.yml`: stress, crash × repetitions, compatibility suite, agent workloads.

- [ ] Local verification: `install_test.sh` PASS on macOS; `docker_test.sh` if the daemon is available (else COVERAGE GAP); `actionlint` or `python -c yaml.safe_load` on workflows. Commit `dist: installer, Docker image, CI and release workflows`.

### Task 15: Documentation and validation report

**Files:**
- Modify: `README.md` (Quickstart → install one-liner + 6 commands; keep "build from source" below)
- Create: `docs/pgx/local-dev/{install.md,cli.md,storage.md,integrations.md,troubleshooting.md,architecture.md,benchmarks.md,validation-report.md}`

- [ ] Write; every command in the docs is one that a test in Tasks 4–13 executes.
- [ ] Final report per brief §12 with the suite table, commands, logs under `tests/results/<date>/`, commit SHA, environment, performance before/after (`pgxbench startup idle conns` on the same binary with `max_connections` 20 vs 120), risks, verdict.
- [ ] Commit `docs: local development distribution`.
