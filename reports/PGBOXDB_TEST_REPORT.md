# PGBoxDB rename: test report

* Date: 2026-11-16/17. Host: Apple M1 Pro, 16 GB, macOS 14.4 (darwin/arm64).
* Code under test: branch `rebrand/pgboxdb`. Binaries were built at
  `f0fd7d81b1` (rename + compatibility). `96381d1a50` adds only Skill scripts,
  site HTML and docs on top. The release-equivalent static build was made at
  `96381d1a50`.
* Toolchains: rustc 1.96.0 (the CI pin) for fmt and clippy, and for the
  release-equivalent static build; Homebrew rustc 1.92.0 for the local suite
  build (`target/release`). PostgreSQL 18.6 client tools (Homebrew), Node
  23.3, Python 3.9.6, Go 1.27.2, Claude Code 2.1.326.
* Isolation: every run used a temporary `HOME` and/or `PGBOXDB_HOME` and a
  `/tmp` run directory. The developer's live `~/.pgx` runtime (pid 44341,
  port 5433) and its supervisor kept running throughout and were never
  touched. `~/.pgboxdb` was never created.
* Logs: `$SCRATCHPAD/logs-final/*.log` (final matrix), `logs/` (first
  matrix), `regress.log`, `static-build.log`, `bench3.out`, where `$SCRATCHPAD`
  is `/private/tmp/claude-501/-Users-alex-GPT-postgresrun-pgrust-pgx/ea79dcf2-eb38-4ebb-9821-cbcbf531f23f/scratchpad`
  (session scratch, not durable).

Legend: **PASS** · **FAIL** (with cause) · **UNAVAILABLE** (could not run
here, with reason) · **N/A** (feature not in this codebase).

## 1. Results against the requested checklist

| # | Requested check | Result | Evidence |
|---|---|---|---|
| 1 | `pgboxdb --version` | **PASS** | `pgboxdb 0.1.3 (engine f0fd7d81b1, PostgreSQL 18.6 compatible, darwin/arm64, built 2026-11-16T23:59:12Z)`; `--version`, `-V`, `version` and `--json version` (`pgboxdb_version`) all work; packaged `version` is checked by install, npm and app tests |
| 2 | New CLI functionality and help | **PASS** | unit 235/0 (1 ignored, as before), incl. `help_mentions_every_command`, which now includes `migrate`; help columns re-aligned; the agent/git/top usage strings say `pgboxdb` |
| 3 | Database startup/shutdown | **PASS** | `lifecycle` 27/27, `run` 8/8, `doctor` 7/7; packaged smoke (static build) clean stop with 0 engine processes left |
| 4 | PostgreSQL wire protocol | **PASS** | app clients over TCP: psql, node-postgres (prepared, 23505), psycopg (server-side cursor, COPY, 42P07), Go `jackc/pgx` v5 (CopyFrom, batch); Python black-box `test_sql` 19/19 through the harness's own wire client; CLI's `pgwire` unit tests |
| 5 | SQL compatibility regression suite | **PASS** | PostgreSQL's own `src/test/regress` vs PG 18.6, this platform: **219/231 byte-exact, 12 plan-only diffs, 0 result diffs**, classifier gate `--fail-on-semantic` rc=0. Identical to the figure documented before the rename (README badge 219/231, 0 result diffs). This is a regression-suite comparison, not a compatibility percentage. |
| 6 | Create / clone / drop database | **PASS** | `databases` 10/10, `clone` 7/7, `branch` 8/8, `ttl` 10/10, `copy` 18/18; `clone_consistency` 10/10 clones identical under a concurrent writer; `test_storage` 6/6 (200 clone/drop cycles, base/ +0.0 MB) |
| 7 | Scale-to-zero and wake-on-connect | **N/A** | Not implemented on `main`. It exists only on the unmerged branch `feat/scale-to-zero` (`af23e52572`, not an ancestor of this branch); `docs/pgboxdb/local-dev/plan.md` states the tree has no suspend/resume. The shipped equivalent, stop/start with persistence, is covered by lifecycle and smoke (data and TTLs survive a restart). |
| 8 | Concurrent database/agent isolation | **PASS** | `agent` 16, `agent_hook` 11, `agent_exec` 8, `agent_install` 12, `agent_subagent` 12; `test_isolation` 12/12, `test_concurrency` 5/5; `stress` (1000 cycles, 100 clients, 60 s, 50 start/stops): no leaks or integrity failures |
| 9 | Git integration | **PASS** | `git` with failpoints 21/21, release configuration 20/20; migration e2e checks 9 and 11 (branch db kept, hooks replaced) |
| 10 | Existing data compatibility | **PASS** | `dist/test/migrate_test.sh` against a real installation made by the published **PGX 0.1.3** release (`~/.pgx/dist/0.1.3`, executed only): **13/13**, on both the hybrid prefix and the final assembled distribution (below) |
| 11 | Installer and npm package | **PASS** | install e2e (download, checksum, relocate, tampered-tarball refusal, pre-rename-dist refusal), install discovery, npm e2e (download, verify, cache, pass-through, exit codes), npm unit 22/22, Worker unit 7/7 |
| 12 | Release build checksums | **PASS** | clean-commit tarball `pgboxdb-0.1.3-darwin-arm64.tar.gz` sha256 `21e6315c98762d1bb4deda949c1b72dcc6110b65324f2f8008b2bc2b85f6a30f`, `shasum -c` OK; 1869 entries, none named after the previous product; `dist.json` `engine_commit` clean (no `-dirty`). Tarballs are not byte-reproducible across runs (by design: `dist.json` records `built_at`). |
| 13 | Cargo metadata and lockfile | **PASS** | `cargo build --release --locked` (twice, clean tree); `cargo metadata` lists package/bin `pgboxdb`; lockfile diff is the package name plus its sort position (11 lines); clippy `-D warnings` on 1.96 with and without `failpoints`; `cargo fmt --check` |

## 2. Full matrix (final run, assembled distribution from `f0fd7d81b1`)

| Suite | Result |
|---|---|
| `cargo fmt -p pgboxdb -- --check` (1.96) | PASS |
| `cargo clippy -p pgboxdb --all-targets -- -D warnings` (1.96), also `--features failpoints` | PASS |
| unit (`--bin pgboxdb`) | 235 passed, 0 failed, 1 ignored |
| integration: lifecycle 27 · databases 10 · clone 7 · branch 8 · doctor 7 · run 8 · ttl 10 · copy 18 · top 14 · top_tty 5 · agent 16 · agent_hook 11 · agent_exec 8 · agent_install 12 · agent_subagent 12 | 173 passed, 0 failed |
| git (failpoints) / git (release config) | 21 / 20 passed |
| Python `test_sql` | 19 passed |
| Skill e2e (real runtime, real PostgreSQL, skills CLI) | 43 passed |
| Skill unit (`skills/tests/pgboxdb/run.sh`) | 42 ok |
| Plugin (`claude plugin validate --strict`, install/enable/disable/update/scopes/uninstall, isolated `CLAUDE_CONFIG_DIR`) | 56 ok |
| install e2e, install discovery, assemble layout | PASS |
| npm e2e, npm unit (22), Worker unit (7) | PASS |
| **migration e2e** (13 checks) | PASS |
| crash recovery, ephemeral (10) | PASS |
| crash recovery, full (10) | **1 FAIL, flaky, pre-existing (§3)**; 8/8 reruns PASS |
| isolation (12), concurrency (5), storage (6), clone consistency, stress | PASS |
| app clients: psql, node, python, go | PASS |
| app client: rails | **UNAVAILABLE**: activerecord/pg gems not installed |
| Ruby compat suite (`tests/compat`) | **UNAVAILABLE**: pg/minitest gems not installed (not installed into system Ruby on purpose) |
| PostgreSQL regression suite + semantic gate | PASS (219/231 byte-exact, 0 result diffs) |
| packaged smoke on the local dynamic build | **FAIL, environment**: the local engine links Homebrew abseil dylibs. Release builds use a static RE2+abseil prefix (`release.yml`), so this was re-run that way (next row). |
| release-equivalent static build (rustc 1.96, `dist/macos/build-static-re2.sh`, `--locked`) → assemble → `macos_selfcontained_test.sh` → `smoke_packaged.sh` → lifecycle/databases/clone on the artifact | PASS (0 Homebrew loads; TTL expiry across a restart; RE2 linked; 0 engines after stop; 27/10/7 tests) |
| Docker image build/run (`dist/test/docker_test.sh`, CI `docker` job) | **UNAVAILABLE**: the local Docker VM disk is full. The entrypoint's new run-dir step was tested in isolation. Not run: CI must cover it. |
| Linux (amd64, arm64) | **UNAVAILABLE locally**: covered by the CI matrix (`ubuntu-24.04`, `ubuntu-24.04-arm`) once pushed |

A first full matrix on the same code (before the Rust identifier tidy-up)
was also green, except `test_sql`, which caught the Python harness
class/global name collision introduced by the mechanical pass. That was
fixed and re-run (19/19).

## 3. Failure analysis: `test_crash.py::test_kill_during_commit_flood` (full durability)

* Observed once: `committed rows are not a prefix of the commit order`, with
  18380 rows present and 18379 acknowledged. **0 acknowledged commits lost.**
* Cause: a race in the test, not the engine. `check_prefix` first explicitly
  allows one in-flight, not-yet-acknowledged id (`acked_ids[-1] + 1`), then
  asserts `sorted(present) == acked_ids[:len(present)]`, which fails exactly
  when that allowed row is present. That case is a commit made durable while
  the client was killed before reading the acknowledgement, which is
  legitimate.
* Not caused by the rename: the file's diff is name-only
  (`self.pgx` → `self.pgboxdb`), and the engine has no logic changes.
  Reruns: **4/4 PASS on the new distribution and 4/4 PASS on a prefix with
  the pre-rename 0.1.3 engine**, each with 0 acknowledged commits lost.
* Not fixed here (unrelated to the rename). Suggested fix: compare `present`
  against `acked_ids + [acked_ids[-1] + 1]`.

## 4. Migration e2e detail (`dist/test/migrate_test.sh`)

Previous CLI: `pgx 0.1.3 (engine ec10a9d61f)`, the published release.

| # | Check |
|---|---|
| 1 | previous CLI builds an installation: `app` with data, a permanent db with data, a 2 h TTL db, `agent setup` (project-scope Claude hooks, `.pgx.toml`), `git init`, a feature-branch db with data, zsh shell integration |
| 2 | `pgboxdb run` refuses (exit 4) and does not create `~/.pgboxdb` |
| 3 | `pgboxdb status` points at `pgboxdb migrate` |
| 4 | a set `PGX_PORT` is reported as not read |
| 5 | `migrate --apply` refuses while the previous runtime runs, and changes nothing (also not the project files: a blocked home step applies nothing) |
| 6 | dry run changes nothing |
| 7 | `--apply` moves the home with byte-identical file contents (checksummed tree), writes `.pgboxdb.toml` with the same source, copies the repo id, keeps `.pgx.toml` and `pgx-repo-id` |
| 8 | data, permanent db, TTL db and port survive; the cluster keeps its `pgx.conf` (no `pgboxdb.conf` appears) |
| 9 | `pgboxdb git status` maps the same branch database; its data is intact |
| 10 | `agent install` replaces the 6 previous hook entries (no duplicates; idempotent) |
| 11 | `git init` and `git install-shell` replace the previous blocks in place |
| 12 | a second `--apply` reports "already migrated" |
| 13 | rollback refused while running; after stop it moves the home back and the **previous CLI starts and sees the data** |

Bugs this test found during development, now fixed: `run` created
`~/.pgboxdb` (its run lock) before refusing; a blocked home step still copied
project files.

## 5. Performance: before vs after

A/B on the same machine, isolated homes, alternating order, 3 rounds. The
baseline is the published **PGX 0.1.3** release artifact. The candidate is
the release-equivalent static PGBoxDB build (rustc 1.96). The third arm,
**hybrid**, is the pre-rename 0.1.3 engine run by the new `pgboxdb` CLI.

| metric (median of 3) | PGX 0.1.3 | hybrid | PGBoxDB | per-round range across all arms |
|---|---|---|---|---|
| cold `run` (init + start + app), ms | 1053 | 1215 | 1184 | 1040–1288 |
| `start`, ms | 400 | 421 | 426 | 396–446 |
| clone of a 50k-row db, ms | 36.2 | 37.8 | 38.0 | 35.0–44.4 |
| drop, ms | 20.5 | 21.3 | 21.2 | 19.7–22.4 |
| pgbench -S, 4 clients, 8 s, tps | 48459 | 41138 | 41568 | 38251–53661 |
| idle RSS of the cluster, MB | 35.8 | 35.4 | 20.8 | bimodal 18.7–21 / 33–38 in **every** arm |

Every between-arm difference is inside the per-round noise. An earlier run
reported idle RSS −24%, which was a measurement error: `pgrep` also matched
the developer's live engine started from the same 0.1.3 binary path. A second
run reported +68%, which is the bimodal noise above. Both were discarded, and
the method was fixed to count only the cluster's postmaster and its children.

The rename's only runtime additions are in the CLI (a few `stat` calls and
environment lookups), so CLI latency was measured directly against a running
cluster, 60 runs each:

| command | PGX 0.1.3 median / p95 ms | PGBoxDB median / p95 ms | Δ median |
|---|---|---|---|
| `status` | 11.1 / 16.0 | 11.2 / 16.9 | +0.1 ms |
| `ls` | 9.0 / 12.2 | 8.8 / 13.5 | −0.2 ms |
| `url app` | 6.1 / 8.2 | 6.3 / 9.3 | +0.2 ms |
| `start` (already running) | 5.1 / 10.8 | 4.8 / 7.0 | −0.3 ms |
| `version` | 2.7 / 3.7 | 2.4 / 3.7 | −0.3 ms |

**Conclusion: no measurable regression.** The Linux benchmark suite
(`benchmarks/pgboxdb/linux`, `pgxbench` host) was not run (remote host, not in
scope).

## 6. Not verified

* Linux and Docker locally (CI covers both after a push).
* GitHub Actions themselves (no push). `release.yml`, `npm-publish.yml` and
  `publish-plugin.yml` were not executed. Their local equivalents were
  (build, assemble, self-contained, artifact suites, npm pack/install via
  `npm_test.sh`, plugin validate).
* Real downloads from `github.com/pgrundev/pgboxdb` and
  `https://pgboxdb.com/install`: neither exists yet. Download paths were tested
  against a local HTTP server.
* Rails app client and the Ruby compat suite (gems missing).
