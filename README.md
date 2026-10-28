<h1 align="center">PGX</h1>

<p align="center">
  <strong>PGX. Postgres for agents. 🐘</strong><br>
  One runtime. Thousands of Postgres databases.
</p>

<p align="center">
  <img alt="Postgres 18.6" src="https://img.shields.io/badge/Postgres-18.6-336791">
  <img alt="Version: v0.1" src="https://img.shields.io/badge/version-v0.1-blue">
  <img alt="PostgreSQL regression suite: 219 of 231 byte-exact, 0 result differences" src="https://img.shields.io/badge/src%2Ftest%2Fregress-219%2F231_byte--exact%2C_0_result_diffs-green">
  <a href="LICENSE"><img alt="License: AGPL-3.0" src="https://img.shields.io/badge/license-AGPL--3.0-blue"></a>
</p>

```text
# Claude Code: install inside a session (native plugin, lightweight repository)
/plugin marketplace add pgrundev/pgx-plugin
/plugin install pgx@pgx-plugin
```

```bash
# Any agent host (Claude Code, Cursor, Codex): the Agent Skills CLI
npx skills add pgrundev/pgx --skill pgx

# Manual PGX installation
npx pgxdb@latest
```

<p align="center">
  <strong>Every AI agent gets its own Postgres.</strong><br>
  Install one Skill. Your coding agent handles the database.<br>
  No Docker required · Native Mac &amp; Linux · Automatic cleanup
</p>
<!-- skills.sh badge: add [![skills.sh](https://skills.sh/b/pgrundev/pgx)](https://skills.sh/pgrundev/pgx) once https://skills.sh/pgrundev/pgx lists the skill -->

<p align="center">
  <sub>Open source · Free · v0.1.2 · For disposable databases, not production data</sub>
</p>

PGX is a database runtime for short-lived Postgres databases: agent
sandboxes, CI and per-PR databases, test and preview environments. One PGX
process holds many isolated databases. Each costs tens of kilobytes of
memory while idle, is created in milliseconds from a sealed template, and
is backed, on a copy-on-write storage backend such as ZFS, by a clone
that starts at a few megabytes whatever the template's size (on a
filesystem without block cloning a branch is a full copy).

PGX speaks the Postgres wire protocol and SQL dialect. Rails, Django,
Prisma, SQLAlchemy and node-postgres run their full migrate → test →
schema-change lifecycles on it unchanged.

PGX is the database engine behind PGRun branches.

> A database should be cheap enough to allocate like an object, not
> provision like a server.

## Why PGX

Agent workflows, test suites, CI jobs and preview environments create
databases constantly and throw them away minutes or hours later. Giving
each one its own PostgreSQL server costs a process, tens of megabytes and
a cold start every time. Traditional PostgreSQL is built to keep one
database running for years; PGX is built to create one in milliseconds,
use it briefly and delete it.

Instead of

```text
1 database → 1 PostgreSQL server
```

PGX runs

```text
1 PGX runtime → hundreds or thousands of isolated databases
```

Immutable state is shared. Each database pays mainly for what makes it
different.

## At a glance

```text
~6.2 ms      warm database mint, p50
~70 KB       memory per untouched idle database
~128 KB      memory per idle database that has been queried
1,000        isolated databases on a 4 vCPU / 8 GB VM
128–155 MB   total runtime memory with 1,000 untouched databases
~213 MB      after all 1,000 have been used

10 GB        template
~674 ms      copy-on-write branch ready
~13.6 MB     initial additional physical storage

219 / 231    PostgreSQL src/test/regress byte-exact against PG 18.6 expected output
0            result differences (the other 12 differ in EXPLAIN plan text only)
```

These describe measured configurations and workloads, not a claim that
PGX is faster than PostgreSQL in general. The regression figure is
PostgreSQL's own test suite on one platform under the regress profile
(see Benchmarks), not a compatibility percentage for arbitrary SQL. The detail and the scripts are
under Benchmarks.

## Disposable Postgres

PGX is built around a different database lifecycle:

```text
sealed template
      ↓
   mint database        milliseconds, copy-on-write
      ↓
agent / CI / test       minutes or hours
      ↓
    discard             dropped a grace period after the last session
```

A database is created from a sealed template without copying it. On
Linux, ZFS block cloning lets a branch share every unchanged block with
its template, so a branch of a 10 GB database is ready in under a second
and starts at about 14 MB of its own storage.

## Built for agents

A coding agent's database workload looks like this, and many agents do it
at once:

```text
create database → run migrations → change schema → load fixtures
→ run tests → inspect results → throw the database away
```

PGX optimizes for that lifecycle rather than for a long-running
production primary. Every agent, every pull request and every test run
can have its own real Postgres-compatible database.

## Benchmarks

Measured on a 4 vCPU / 8 GB / $8.50-a-month VPS with ZFS block cloning. The
load generator ran on the same CPUs, so these are architecture numbers,
not capacity limits. Every figure has a script and a committed raw result
([`benchmarks/pgx/linux/README.md`](benchmarks/pgx/linux/README.md)).

### Density

| | PGX |
|---|---|
| 1000 idle databases in one runtime | **128–155 MB** in total |
| 1000 databases, each queried once | **213 MB** |
| Per idle database | ~70 KB untouched, ~128 KB once queried |
| 1000 databases with 100 active | 283 MB |
| Idle CPU with 1000 databases | 0.33–0.43 % of one core |

For comparison, one idle PostgreSQL 18 server uses 16.6 MB.

### Creating databases

| | p50 | p99 |
|---|---|---|
| Database from the warm pool | 6.2 ms | 9.4 ms |
| Cold database (copy-on-write clone of a 600-file template) | 74 ms | 102 ms |
| 500 simultaneous new-database requests | all served, 0 errors | drained in 12.6–13.8 s |

Times run from the client's connection to its first `SELECT 1` answer,
on the same host.

### Branch storage (ZFS copy-on-write)

| Template | Branch ready in | New physical storage |
|---|---|---|
| 1 GB | 135 ms | 2.3 MB |
| 2 GB | 198 ms | 3.7 MB |
| 5.2 GB | 417 ms | 7.0 MB |
| 10 GB | 674 ms | 13.6 MB |

A branch of a 10 GB database is ready in under a second and initially uses
hundreds of times less physical storage than a full copy.

### Running for a long time

| | Runtime memory |
|---|---|
| After 10,000 create → use → drop lifecycles | 108 MB |
| After 30,000 lifecycles | 111 MB (~0.2 KB retained per lifecycle) |

### Compatibility

| | Result |
|---|---|
| PostgreSQL regression suite (`src/test/regress`, PG 18.6 expected output) | 219 / 231 byte-exact; 12 differ only in EXPLAIN plan text; **0 result differences**. Measured under the PGX profile with parallel workers and `synchronous_commit` restored (`configs/pgx-regress-overlay.conf`), the two settings the suite asserts on; under the shipped ephemeral profile those two settings alone make `select_parallel` and `stats` differ. One suite, one platform: not a compatibility score. |
| Real Rails 8.1 app (migrations, fixtures, model tests, schema change, `db:schema:load`) | passes; within 0–5 % of PostgreSQL 18 per phase |
| Django 5.2, Prisma 6, SQLAlchemy 2, node-postgres, Rails-shaped SQL | pass |
| All framework runs | **36 / 36** |

Transactional behaviour follows PostgreSQL, including transactional DDL
and temporary tables:

```sql
BEGIN;
CREATE TEMP TABLE pgx_test (id integer PRIMARY KEY, value text);
INSERT INTO pgx_test VALUES (1, 'PGX works');
SAVEPOINT before_update;
UPDATE pgx_test SET value = 'changed' WHERE id = 1;
ROLLBACK TO SAVEPOINT before_update;
SELECT value FROM pgx_test WHERE id = 1;   -- 'PGX works'
ROLLBACK;
SELECT to_regclass('pg_temp.pgx_test');    -- NULL: the table was rolled back
```

### Isolation

- **Memory limits.** Session, database and runtime limits refuse runaway
  work with SQLSTATE 53200. The session stays usable, other databases see
  0 errors, and the runtime does not crash.
- **Connection limits.** A neighbour running a 50-connection migration
  mix pushes another database's p99 latency to 9.8 ms. Capped at 1 / 2 / 5
  connections per database, it stays at 1.2 / 2.1 / 4.0 ms.
- **cgroups.** The kernel is the last line: under a cgroup memory limit,
  the worst case is that one runtime is killed. The host survives.

### Query speed

PGX is not trying to beat Postgres on single queries.

- **Short statements:** about 0.05–0.15 ms slower than PostgreSQL 18; a
  point SELECT takes 0.25 vs 0.13 ms.
- **Large single-threaded scans:** 1.5–2.2× slower.
- **Equal:** COPY and 4-client throughput.

Whole framework lifecycles take 0–15 % longer than on PostgreSQL 18.

PGX concentrates on database creation latency, density, memory
efficiency, copy-on-write storage, high churn and isolation between
tenants. For application lifecycle workloads it stays close enough to
PostgreSQL while making large numbers of isolated databases dramatically
cheaper to keep alive.

The full report:
[`docs/pgx/linux-results.md`](docs/pgx/linux-results.md). The frozen v0.1
numbers: [`docs/pgx/v0.1-baseline.md`](docs/pgx/v0.1-baseline.md).

## How it works

The central idea: **share everything immutable, pay for divergence.** It
applies to memory and to storage alike.

- **Shared runtime.** Databases are isolated by name, roles and limits
  inside one process with a thread per connection. A database costs its
  catalog caches, not a server.
- **Shared catalog state.** Databases cloned from the same template share
  identical relation-cache entries. Idle databases are skipped by
  autovacuum.
- **Mint-on-connect.** Connecting to `<prefix><template>__<token>` creates
  that database from a sealed template and connects you to it. A warm pool
  of pre-created spares serves bursts. A janitor drops databases a grace
  period after their last session.
- **Copy-on-write storage.** `file_copy_method = clone` turns database
  creation into ZFS block clones, so a branch shares every unchanged block
  with its template and its cost grows with divergence:

  ```text
  template
  ├── branch A → only changed blocks
  ├── branch B → only changed blocks
  └── branch C → only changed blocks
  ```
- **Limits at every level.** Session, database and runtime memory limits
  inside the engine, connection limits per database, and a Linux cgroup
  around the runtime. Memory-heavy work fails with a PostgreSQL-style
  error instead of taking the host down:

  ```text
  session → database / runtime → Linux cgroup
  ```
- **Machine-readable status.** `pgrust_runtime_status()` returns JSON with
  databases, sessions, memory against limits, and warm-pool and mint
  counters.

## Status

PGX is at **v0.1.2**, an early release with known limitations listed in the
[release notes](https://github.com/pgrundev/pgx/releases/tag/v0.1.2). It
already powers PGX branches in PGRun. The current focus is PostgreSQL compatibility,
Rails/application compatibility, concurrency semantics, lifecycle
correctness, predictable resource containment and real agent workloads.
Correctness and reproducibility come before benchmark marketing: if PGX
behaves differently from PostgreSQL for a supported workload, we want a
reproducible test case (see Contributing).

**PGX v0.1 is for disposable databases:** agent, CI, PR, test and preview
databases built from sanitized copies of production. Use it where losing
a branch is acceptable.

It is **not** a production primary, a system of record, or an HA or PITR
database:

- **Not crash-safe.** The PGX profile runs with `fsync = off`.
- **One process.** A runtime is one crash domain.
- **No C extensions.** Existing PostgreSQL C extensions cannot load; only
  the built-in contrib modules exist, pgvector among them. PL/Python,
  PL/Perl and PL/Tcl are not available.

Keep Postgres as the source of truth for data you can't afford to lose.
If you need a long-lived production database, use PostgreSQL or a managed
PostgreSQL service. If you need hundreds or thousands of databases that
live for minutes or hours, PGX is the workload we are building for. The
full list is in [`docs/pgx/v0.1-readiness.md`](docs/pgx/v0.1-readiness.md).

## PGX + PGRun

PGX is the database engine behind PGX branches in [PGRun](https://pgrun.dev).
PGRun adds the control plane around it:

```text
production / source database
        ↓
safe copy with masking
        ↓
sealed template
        ↓
PGX
        ↓
one branch per agent, PR or CI job
        ↓
automatic expiry
```

PGRun handles orchestration, routing, templates, lifecycle, access and
branch management. PGX is the runtime. They are separate projects.

## Quickstart

One command installs PGX, starts it, creates an `app` database and prints its
connection URL. No Docker, Rust or system PostgreSQL; the native distribution
is self-contained and relocatable.

```bash
npx pgxdb@latest
```

```console
PGX is ready.

  postgres://postgres:…@127.0.0.1:5432/app
```

Point `DATABASE_URL` at that URL. `npx pgxdb` with no arguments is `pgx run`;
every other `pgx` command works the same way through npx:

```bash
npx pgxdb url app                      # its connection URL
npx pgxdb ls                           # databases, TTL, expiry, size
npx pgxdb branch app agent-001         # a disposable 2-hour branch for one agent
npx pgxdb clone app pr-123 --ttl 5m    # a 5-minute copy-on-write clone
npx pgxdb --json ls                    # machine-readable output for agents
npx pgxdb stop
```

The binary is downloaded once (SHA-256 verified) into `~/.pgx/dist/<version>`
and reused. `npm install -g pgxdb` puts `pgx` on your PATH for good. Supported:
macOS (Apple silicon and Intel) and Linux (arm64, x86-64). Intel Macs are
supported from the first release after v0.1.2.

Without Node, the shell installer does the same install:

```bash
curl -fsSL https://pgx.dev/install | sh
pgx run
```

Any PostgreSQL client or ORM works against `pgx url <db>`. The full CLI,
install options, integrations (Rails, Node, Python, Go, psql), storage model
and troubleshooting are under [`docs/pgx/local-dev/`](docs/pgx/local-dev/); the
test matrix and release decision are in
[`docs/pgx/local-dev/validation-report.md`](docs/pgx/local-dev/validation-report.md).

### Branching and temporary databases

Clones are fast branches: copy-on-write on a backend with block cloning
(such as ZFS), a full copy elsewhere. Any database can also carry a TTL so
it cleans itself up — a branch per agent, pull request or CI job that you
never have to remember to delete.

```bash
pgx branch myapp agent-1           # an agent's own database; expires in 2 hours
pgx branch myapp agent-2 --ttl 30m # same, with a shorter TTL
pgx clone myapp pr-123 --ttl 30m   # a clone; permanent unless you give a TTL
pgx create scratch --ttl 2h        # a throwaway database, gone in 2 hours
pgx ls                             # shows each database's TTL and expiry
pgx ttl pr-123 unlimited           # changed your mind — keep it for good
```

`pgx branch` is `pgx clone` for agents: a 2-hour TTL by default and the
connection URL as the last line of output, so an agent can be handed
`DATABASE_URL="$(pgx branch myapp agent-1 | tail -1)"` or read `--json`.

Databases created without `--ttl` are permanent. A background supervisor
drops each temporary database shortly after its deadline, never touches
permanent or system databases, and resumes cleanup automatically after a
`pgx stop` / `pgx start`. Details and the identity-safety model are in
[`docs/pgx/local-dev/ttl.md`](docs/pgx/local-dev/ttl.md).

### Every AI agent gets its own Postgres

Install the Skill, then ask your agent. In Claude Code, from inside a session:

```text
/plugin marketplace add pgrundev/pgx-plugin
/plugin install pgx@pgx-plugin
/pgx:pgx Set up isolated Postgres databases for this project.
```

[`pgrundev/pgx-plugin`](https://github.com/pgrundev/pgx-plugin) holds only
the plugin (about 110 KB, generated from this repository on every change to
`main`), so the install takes seconds. Adding this repository itself as the
marketplace still works (`/plugin marketplace add pgrundev/pgx`, then
`/plugin install pgx@pgrundev`) but clones about 2 GB. Install from one of
the two, not both.

For Cursor, Codex or several hosts at once, the Agent Skills CLI installs the
same Skill into the project:

```bash
npx skills add pgrundev/pgx --skill pgx
```

> Use PGX to give this project isolated databases for coding agents. Set everything up.

The plugin is the Skill packaged for Claude Code's plugin manager (`/plugin`
lists, updates and removes it; the skill is `/pgx:pgx` and Claude also picks
it up on its own when a task touches the database). The Skills CLI copies
the Skill into `.claude/skills/pgx` and `.agents/skills/pgx` for every host.
Both contain no hooks and install no database engine: the `pgx` binary is
installed when the Skill first needs it, with your approval. Details and the
differences: [`docs/pgx/local-dev/claude-plugin.md`](docs/pgx/local-dev/claude-plugin.md).

The agent detects your database setup, installs PGX if needed, imports your
development database **schema-only** into a template (`pgx copy`), writes a
committed `.pgx.toml`, installs lifecycle hooks (`pgx agent setup`) and
proves a branch. From then on each Claude Code session gets its own branch
with `DATABASE_URL` already set, extended while the agent works and removed
after it stops, and every subagent it launches gets a branch of its own,
with its shell commands routed there; two agents never see each other's
changes and your PostgreSQL is never written. Installing the Skill executes
nothing by itself: the agent does the setup when the Skill is activated and
your permissions allow it.

| Host | Automatic isolation | Status |
|---|---|---|
| Claude Code | yes, through hooks: one branch per session and one per subagent | verified live |
| Cursor | hooks installed; `pgx agent exec --` when `PGX_ISOLATION` is unset | env propagation pending live verification |
| Codex | `pgx agent exec -- codex` (Codex hooks cannot set environment) | wrapper verified |

Requires PGX 0.1.2 or newer ([release v0.1.2](https://github.com/pgrundev/pgx/releases/tag/v0.1.2); 0.1.1 isolates sessions but lets subagents share the parent's branch); the Skill checks the installed version before it does anything.
Details, the lifecycle, security rules and the validation notes:
[`docs/pgx/local-dev/agents.md`](docs/pgx/local-dev/agents.md). The Skill
itself: [`skills/pgx/SKILL.md`](skills/pgx/SKILL.md).

### Keep your Postgres, branch it in PGX (by hand)

The same thing without an agent. Copy PostgreSQL into PGX once, then give
every agent its own branch. The source is only read; PostgreSQL stays your
source of truth.

```bash
PGX_PORT=5433 npx pgxdb@latest                 # PGX beside PostgreSQL on 5432

# Import PostgreSQL once (pg_dump → pg_restore, read-only on the source)
pgx copy --from "$DATABASE_URL" --to myapp --schema-only

# Give each AI agent its own database (isolated, self-expiring after 2h)
pgx branch myapp agent-1
pgx branch myapp agent-2
pgx agent exec -- bin/rails test               # or: any command in a branch of its own
```

```console
$ pgx branch myapp agent-1
✓ Branch created: agent-1
✓ Source: myapp
✓ Expires in: 2 hours

DATABASE_URL:
postgres://postgres:…@127.0.0.1:5433/agent-1
```

`--schema-only` copies the structure without the rows (migration bookkeeping
such as `schema_migrations` is kept, so frameworks see their migrations as
applied). The copy needs `pg_dump`/`pg_restore` on the machine (a PostgreSQL
install has them); it refuses sources that use extensions PGX does not ship
(PostGIS, TimescaleDB, …) and never overwrites an existing database. Details,
limitations and measurements: [`docs/pgx/local-dev/copy.md`](docs/pgx/local-dev/copy.md).

### Build from source

You need the Rust toolchain (`rust-toolchain.toml` pins Rust 1.96.0;
rustup fetches it), the RE2 regex library, and the PostgreSQL 18 client
tools for `initdb` and `psql`:

- Debian/Ubuntu: `sudo apt-get install -y build-essential pkg-config libre2-dev`,
  plus `postgresql-18` and `postgresql-client-18` from the
  [PGDG apt repo](https://www.postgresql.org/download/linux/ubuntu/)
  (then `export PATH="/usr/lib/postgresql/18/bin:$PATH"`).
- macOS: `brew install re2 pkg-config postgresql@18`, then
  `export PATH="$(brew --prefix postgresql@18)/bin:$PATH"`.

```bash
cargo build --release --locked --bin postgres
```

The server binary is `target/release/postgres`. Release builds refuse to
compile without RE2 on purpose: the fallback regex engine is much slower.

At run time the server loads two libraries with `dlopen` rather than
linking them: libxml2 for the `xml` type and SQL/XML functions, and ICU for
ICU collations. The PostgreSQL 18 packages above install both. A host
without libxml2 still starts, but XML functions raise `unsupported XML
feature` (SQLSTATE 0A000) with a hint naming the library, and
`pgrust_runtime_status()` reports `libraries.libxml2.available = false`.
Debian/Ubuntu package: `libxml2` (`libxml2-16` from Ubuntu 25.10); macOS
ships libxml2 with the system.

### Run a PGX runtime

```bash
# Create a cluster and apply the PGX profile.
initdb -D /tmp/pgx-data --no-locale --encoding UTF8 -U postgres
cat configs/pgx-ephemeral.conf >> /tmp/pgx-data/postgresql.conf

# The server reads timezone and other data files from a Postgres share dir.
export PGRUST_PGSHAREDIR=/usr/share/postgresql/18      # macOS: "$(brew --prefix postgresql@18)/share/postgresql"
export PGRUST_TZDIR=/usr/share/zoneinfo                # macOS: "$PGRUST_PGSHAREDIR/timezone"

ulimit -s 65520
RUST_MIN_STACK=33554432 target/release/postgres \
  -D /tmp/pgx-data -k /tmp -p 5432 \
  -c listen_addresses= -c io_method=sync -c max_stack_depth=60000 \
  -c pgrust.ephemeral_db_prefix=tdb_ \
  -c pgrust.ephemeral_db_mint_roles=postgres
```

Put the data directory on a ZFS dataset with block cloning enabled to get
copy-on-write branches (the profile sets `file_copy_method = clone`). On a
filesystem without block cloning, remove that line from the profile and
databases are created by copying files.

### Make a template and mint databases

```bash
# Build a template once, then seal it.
psql -h /tmp -U postgres -c "CREATE DATABASE tpl_app"
psql -h /tmp -U postgres -d tpl_app -c "CREATE TABLE users (id bigserial PRIMARY KEY, email text UNIQUE)"
psql -h /tmp -U postgres -c "SELECT pgrust_seal_template('tpl_app')"

# Connecting to <prefix><template>__<token> creates that database.
psql -h /tmp -U postgres -d tdb_tpl_app__agent42 -c "INSERT INTO users (email) VALUES ('a@example.com')"
psql -h /tmp -U postgres -d tdb_tpl_app__agent43 -c "SELECT count(*) FROM users"   # 0: isolated

# Runtime status as JSON.
psql -h /tmp -U postgres -Atc "SELECT pgrust_runtime_status()"
```

Minted databases are dropped `pgrust.ephemeral_db_grace` seconds after
their last session ends, unless they are pinned with
`pgrust_pin_database()`. All control functions are listed in
[`docs/pgx/pgrun-interface.md`](docs/pgx/pgrun-interface.md).

## Reproducing the benchmarks

The harness is Python 3 standard library only and talks the wire protocol
directly. On a Linux host with a ZFS pool:

```bash
PGXBENCH_WORKDIR=/tank/pgx-bench benchmarks/pgx/linux/run-suite.sh scale churn pool cow regress
```

Results land in `benchmarks/pgx/results/<host>-<commit>/`.
[`benchmarks/pgx/linux/README.md`](benchmarks/pgx/linux/README.md) maps
every headline number above to its step, script and raw result.

The regression suite is PostgreSQL's own `src/test/regress`, vendored
unmodified at `crates/postgres-18.6-reference/src/test/regress`. It runs
with stock `pg_regress` (`benchmarks/pgx/regress-baseline.py`) under the
PGX profile plus `configs/pgx-regress-overlay.conf` (parallel workers and
`synchronous_commit` back on, because the suite asserts on them), and each
difference is classified as plan-only or semantic. The nightly run fails on
any semantic difference (`classify-regress-diffs.py --fail-on-semantic`).

## Documentation

| Document | What's in it |
|---|---|
| [`v0.1-baseline.md`](docs/pgx/v0.1-baseline.md) | the frozen v0.1 numbers and the build each came from |
| [`v0.1-readiness.md`](docs/pgx/v0.1-readiness.md) | what is ready, known limitations, approved use |
| [`linux-results.md`](docs/pgx/linux-results.md) | the full benchmark report |
| [`compatibility-gate.md`](docs/pgx/compatibility-gate.md) | how to decide whether a database can run on PGX |
| [`local-dev/claude-plugin.md`](docs/pgx/local-dev/claude-plugin.md) | the PGX plugin for Claude Code: `/plugin marketplace add pgrundev/pgx-plugin`, `/plugin install pgx@pgx-plugin`, the lightweight `pgrundev/pgx-plugin` repository and how it is generated, how it relates to the Skills CLI install, updates, removal, the Anthropic directory submission |
| [`local-dev/agents.md`](docs/pgx/local-dev/agents.md) | the PGX Skill and `pgx agent`: automatic per-session isolation for Claude Code, Cursor and Codex, with the compatibility matrix |
| [`local-dev/copy.md`](docs/pgx/local-dev/copy.md) | `pgx copy`: import an existing PostgreSQL database and branch it per agent |
| [`pgrun-integration.md`](docs/pgx/pgrun-integration.md) | the host-agent API PGRun builds against |
| [`lifecycle-memory.md`](docs/pgx/lifecycle-memory.md), [`linux-churn.md`](docs/pgx/linux-churn.md) | memory under long churn, and what was fixed |
| [`zfs-cow.md`](docs/pgx/zfs-cow.md) | copy-on-write branch cost and the recordsize trade-off |
| [`linux-limits.md`](docs/pgx/linux-limits.md) | memory limits and the cgroup backstop |

## Contributing

PGX is early, and the most useful contribution is a concrete
compatibility case. If something works on PostgreSQL 18 and behaves
differently on PGX, open an issue with:

```text
PostgreSQL version
PGX version
minimal SQL reproduction
expected result
actual result
```

The areas where a case helps most: MVCC, locking, deadlocks, isolation
levels, transactional DDL, ORMs (Rails, Django, Prisma, SQLAlchemy),
schema migrations and client compatibility. Every real compatibility bug
becomes a permanent regression test.

## Origin

PGX is built on [pgrust](https://github.com/malisper/pgrust), a
PostgreSQL-compatible database written in Rust. We are grateful to the
pgrust project and its contributors for the foundation PGX builds on.
PGX narrows that foundation to one problem, high-density disposable
Postgres-compatible databases for agents and tests, and its own changes
concentrate on shared database state, lifecycle and churn, density,
memory containment, copy-on-write branching, fast minting and agent/test
compatibility.

## License

PGX is licensed under [AGPL-3.0](LICENSE). Portions derived from PostgreSQL
remain under the PostgreSQL License; see [`NOTICE`](NOTICE).

---

Production PostgreSQL is optimized to live for years. PGX is optimized to
live for minutes.

**One runtime. Thousands of isolated databases. Postgres for agents.**
