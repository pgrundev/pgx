# PGX benchmarks

The numbers behind the README's Benchmarks section, with every table and the isolation and query-speed results. The full report is [`linux-results.md`](linux-results.md) and the frozen v0.1 numbers are [`v0.1-baseline.md`](v0.1-baseline.md).

Measured on a 4 vCPU / 8 GB / $8.50-a-month VPS with ZFS block cloning. The
load generator ran on the same CPUs, so these are architecture numbers,
not capacity limits. Every figure has a script and a committed raw result
([`benchmarks/pgx/linux/README.md`](../../benchmarks/pgx/linux/README.md)).

## Density

| | PGX |
|---|---|
| 1000 idle databases in one runtime | **128–155 MB** in total |
| 1000 databases, each queried once | **213 MB** |
| Per idle database | ~70 KB untouched, ~128 KB once queried |
| 1000 databases with 100 active | 283 MB |
| Idle CPU with 1000 databases | 0.33–0.43 % of one core |

For comparison, one idle PostgreSQL 18 server uses 16.6 MB.

## Creating databases

| | p50 | p99 |
|---|---|---|
| Database from the warm pool | 6.2 ms | 9.4 ms |
| Cold database (copy-on-write clone of a 600-file template) | 74 ms | 102 ms |
| 500 simultaneous new-database requests | all served, 0 errors | drained in 12.6–13.8 s |

Times run from the client's connection to its first `SELECT 1` answer,
on the same host.

## Branch storage (ZFS copy-on-write)

| Template | Branch ready in | New physical storage |
|---|---|---|
| 1 GB | 135 ms | 2.3 MB |
| 2 GB | 198 ms | 3.7 MB |
| 5.2 GB | 417 ms | 7.0 MB |
| 10 GB | 674 ms | 13.6 MB |

A branch of a 10 GB database is ready in under a second and initially uses
hundreds of times less physical storage than a full copy.

## Running for a long time

| | Runtime memory |
|---|---|
| After 10,000 create → use → drop lifecycles | 108 MB |
| After 30,000 lifecycles | 111 MB (~0.2 KB retained per lifecycle) |

## Compatibility

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

## Isolation

- **Memory limits.** Session, database and runtime limits refuse runaway
  work with SQLSTATE 53200. The session stays usable, other databases see
  0 errors, and the runtime does not crash.
- **Connection limits.** A neighbour running a 50-connection migration
  mix pushes another database's p99 latency to 9.8 ms. Capped at 1 / 2 / 5
  connections per database, it stays at 1.2 / 2.1 / 4.0 ms.
- **cgroups.** The kernel is the last line: under a cgroup memory limit,
  the worst case is that one runtime is killed. The host survives.

## Query speed

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
[`docs/pgx/linux-results.md`](linux-results.md). The frozen v0.1
numbers: [`docs/pgx/v0.1-baseline.md`](v0.1-baseline.md).

## Reproducing the benchmarks

The harness is Python 3 standard library only and talks the wire protocol
directly. On a Linux host with a ZFS pool:

```bash
PGXBENCH_WORKDIR=/tank/pgx-bench benchmarks/pgx/linux/run-suite.sh scale churn pool cow regress
```

Results land in `benchmarks/pgx/results/<host>-<commit>/`.
[`benchmarks/pgx/linux/README.md`](../../benchmarks/pgx/linux/README.md) maps
every headline number in this document to its step, script and raw result.

The regression suite is PostgreSQL's own `src/test/regress`, vendored
unmodified at `crates/postgres-18.6-reference/src/test/regress`. It runs
with stock `pg_regress` (`benchmarks/pgx/regress-baseline.py`) under the
PGX profile plus `configs/pgx-regress-overlay.conf` (parallel workers and
`synchronous_commit` back on, because the suite asserts on them), and each
difference is classified as plan-only or semantic. The nightly run fails on
any semantic difference (`classify-regress-diffs.py --fail-on-semantic`).
