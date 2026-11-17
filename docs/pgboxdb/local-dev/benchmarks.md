# Benchmark methodology

The headline figures come from two harnesses, both Python-3-stdlib and talking
the wire protocol directly (no driver start-up in any number):

- `benchmarks/pgboxdb/pgxbench.py` — startup, idle memory, memory per connection.
- The local-dev suites — clone latency, clone/drop throughput, restart time and
  per-client op rate, sampled while running real CLI commands.

Record, per run: PGBoxDB commit, PostgreSQL version, OS, CPU, RAM, filesystem,
storage backend, dataset, repetitions, and p50/p95/p99 where applicable
(`benchmarks/pgboxdb/README.md`). Numbers in `validation-report.md` §7 were observed
on macOS M1 Pro / APFS and are informational, not guarantees; reproduce on
controlled hardware for a baseline. The frozen v0.1 baseline (idle memory,
density, cold/warm mint, ZFS clone) is in `docs/pgboxdb/v0.1-baseline.md`.

Never trade SQL correctness, isolation or durability for latency without
documenting it; the durability setting is explicit (`pgboxdb init --durability`).

## `pgboxdb copy` and branching from a copied template

Harness: `benchmarks/pgboxdb/copy_bench.py` (Python 3 stdlib). Host: macOS 14.4,
M1 Pro, 16 GB, APFS; engine v0.1.0 (`~/.pgboxdb/dist/0.1.0`); PostgreSQL 18.6
source on the same machine; `pg_dump`/`pg_restore` 18.6; measured 2026-10-24.
Physical disk is the `df` used delta on the data volume, settled 2 s after each
step (APFS purgeable space makes single readings noisy to a few MB).

### Import

| Source (pg_database_size) | `pgboxdb copy` | export | restore | optimize | peak RSS pgboxdb+pg_dump+pg_restore | peak engine RSS | peak temp dump |
|---|---|---|---|---|---|---|---|
| 31 MB (2 tables, 70k rows, 2 extensions) | 1.9–3.8 s | 0.5 s | 0.3–1.6 s | 0.2 s | 17 MB | 86–94 MB | 0.6 MB |
| 157 MB (800k rows, 4 indexes) | 3.8–3.9 s | 0.8 s | 2.3 s | 0.3 s | 17 MB | 116–136 MB | 9.4 MB |
| 1.5 GB (8M rows, 4 indexes) | 32.9–48.9 s | 7.7–11.1 s | 22.9–30.9 s | 2.0–5.0 s | 16 MB | 138–190 MB | 99 MB |
| 1.5 GB, `--jobs 4` | 27.7 s | 11.4 s | 14.5 s | 1.6 s | | | |
| 1.5 GB, `--schema-only` | 0.5 s | 0.0 s | 0.0 s | 0.2 s | | | 5 kB |

Ranges are two runs. The import is a logical copy and scales with the
source; the CLI itself stays at ~17 MB because the data streams through
`pg_dump` to a file and from the file through `pg_restore`.

### Clones of the copied template

Each round: create N clones (`pgboxdb clone … --ttl 2h`), read every table in
every clone once (`SELECT count(*)`), then change a contiguous 2 % of the
largest table in every clone, measuring disk after each step. All clones were
block clones (`"method": "clone"`).

| Template | N | clone p50 / p95 | disk after N clones | first full read (all clones) | disk after first read | disk after the 2 % write |
|---|---|---|---|---|---|---|
| 31 MB, frozen | 100 | 40 / 45 ms | 10.5 MB | 7.1 s | 13 MB | 344 MB (3.4 MB per clone) |
| 31 MB, `--no-optimize` | 100 | 46 / 210 ms | 6 MB | 18.2 s | **3.0 GB** (30 MB per clone) | 3.3 GB |
| 157 MB, frozen | 10 | 45 / 53 ms | ~0 | 0.8 s | 0.7 MB | 222 MB (22 MB per clone) |
| 157 MB, `--no-optimize` | 10 | 69 / 151 ms | 0.6 MB | 2.8 s | **2.0 GB** (204 MB per clone) | 2.5 GB |
| 157 MB, frozen | 100 | 44 / 60 ms | 12.8 MB | 7.8 s | 22 MB | 11.6 GB (scattered-update run, see below) |
| 1.5 GB, frozen | 10 | 44 / 46 ms | ~0 | 3.1 s | ~0 | 18 MB |
| 1.5 GB, frozen | 100 | 47 / 239 ms | 2.6 MB | 29.6 s | 1.8 MB | 1.9 GB (19 MB per clone) |

What this shows:

- A frozen template (`VACUUM (FREEZE, ANALYZE)`, the default) keeps the first
  read of a clone free of writes: 100 clones of a 1.5 GB template cost 1.8 MB
  of disk after every table was read once. Without it, every clone's first
  read rewrites its pages and un-shares them from the template: 30 MB per
  31 MB clone, 204 MB per 157 MB clone, with the read itself 2.6–3.5× slower.
- Clone creation does not depend on template size: p50 40–47 ms from 31 MB
  to 1.5 GB. The p95 outliers (210–240 ms) are checkpoint noise.
- Disk then follows what each branch writes. The 157 MB / 100 clones row and
  the first attempt at the 1.5 GB round used `UPDATE … WHERE random() < 0.02`,
  a *scattered* update of 2 % of the rows: it touches nearly every page and
  index leaf, un-sharing almost the whole table (116 MB per 157 MB clone,
  1.35 GB per 1.5 GB clone), which filled the machine's disk. That is the
  honest worst case for a branch that rewrites rows all over a large table;
  the harness now updates a contiguous 2 % by physical order, which is what a
  fixture load or a backfill of recent rows does.

Not measured: Linux/ZFS (see `zfs-cow.md` for ZFS clone costs), a 10 GB
source, concurrent clone creation.
