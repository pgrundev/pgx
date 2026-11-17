# Keep your Postgres. Give every AI agent its own database.

`pgboxdb copy` imports an existing PostgreSQL database into your local PGBoxDB runtime
once. From that template, `pgboxdb branch` hands each coding agent, pull request or
test run its own isolated, disposable database in milliseconds. Your
PostgreSQL installation is never modified and stays the source of truth.

```
Local PostgreSQL (port 5432)          PGBoxDB runtime (port 5433)
┌──────────────┐    pgboxdb copy          ┌───────────────────────────┐
│    myapp     │ ───────────────────▶ │ myapp                     │
└──────────────┘  pg_dump│pg_restore  │   ├─ agent-1  (pgboxdb branch)│
                  read-only            │   ├─ agent-2    2h TTL    │
                                       │   ├─ agent-3  --ttl 30m   │
                                       │   └─ pr-123               │
                                       └───────────────────────────┘
```

## Existing PostgreSQL + PGBoxDB

```sh
# 1. Start PGBoxDB on a port that does not collide with PostgreSQL (first run only;
#    afterwards the saved configuration is used).
PGBOXDB_PORT=5433 npx pgboxdb@latest

# 2. Import your database into PGBoxDB once. The URL is only read.
pgboxdb copy --from "$DATABASE_URL" --to myapp

# 3. Give each agent its own database. Each branch is independent and expires
#    after 2 hours unless you say otherwise; its last output line is the URL.
pgboxdb branch myapp agent-1
pgboxdb branch myapp agent-2
pgboxdb branch myapp agent-3 --ttl 30m

pgboxdb ls               # name, status, TTL, size
pgboxdb drop agent-1     # or just let the TTL expire
```

```console
$ pgboxdb branch myapp agent-1
✓ Branch created: agent-1
✓ Source: myapp
✓ Expires in: 2 hours

DATABASE_URL:
postgres://postgres:…@127.0.0.1:5433/agent-1
```

For scripts and agents: `DATABASE_URL="$(pgboxdb branch myapp agent-1 | tail -1)"`,
or `pgboxdb --json branch myapp agent-1` for the same fields as one JSON object.
`pgboxdb branch` is `pgboxdb clone` with a 2-hour default TTL and this output; `pgboxdb
clone` is unchanged (permanent by default).

What `pgboxdb copy` shows:

```console
$ pgboxdb copy --from "$DATABASE_URL" --to myapp_template
copying PostgreSQL database app@127.0.0.1:5432/myapp into PGBoxDB as myapp_template
  ✓ client tools  pg_dump 18.6 (/opt/homebrew/opt/postgresql@18/bin)
  ✓ source        PostgreSQL 18.6 · app@127.0.0.1:5432/myapp
  ✓ compatibility 2 extensions (citext, pgcrypto), 26 objects
  ✓ export        schema and data, 592.3 kB in 0.3 s
  ✓ staging       database _pgboxdbcopy_myapp_template_xcaucn
  ✓ restore       32 objects in 0.5 s
  ✓ validate      2 tables, 3 sequences, 1 view, 1 materialized view, 5 indexes, 2 functions
  ✓ optimize      VACUUM (FREEZE, ANALYZE) in 0.2 s
  ✓ publish       myapp_template (31.0 MB)

Database ready: myapp_template (31.0 MB)
  url  postgres://postgres:…@127.0.0.1:5433/myapp_template

Next:
  pgboxdb branch myapp_template agent-1
```

Progress lines go to stderr; the summary (or, with `--json`, one JSON object)
goes to stdout.

## PGBoxDB only

No PostgreSQL server needed. `pgboxdb run` creates an `app` database; migrate and
seed it, then branch it.

```sh
npx pgboxdb@latest
DATABASE_URL="$(pgboxdb url app)" bin/rails db:migrate db:seed
pgboxdb branch app agent-1
```

## `pgboxdb copy` versus `pgboxdb branch`

| | `pgboxdb copy` | `pgboxdb branch` (and `pgboxdb clone`) |
|---|---|---|
| From → to | external PostgreSQL → PGBoxDB | PGBoxDB database → PGBoxDB database |
| Mechanism | logical: `pg_dump` and `pg_restore` | physical: `CREATE DATABASE … STRATEGY = FILE_COPY`, block-cloned on APFS/ZFS/reflink filesystems |
| Cost | seconds to minutes, proportional to the source | tens of milliseconds, independent of size |
| How often | once per template, and again to refresh it | once per agent, PR or test run |
| Source | only read, never modified | not modified; must have no open sessions during the clone |

Copy once, branch many times. `pgboxdb branch` and `pgboxdb clone` share one
implementation: `branch` defaults to a 2-hour TTL and prints the URL last,
`clone` defaults to permanent. `pgboxdb copy` changes neither.

## Command

```
pgboxdb copy --from <postgres-url> --to <database> [--schema-only] [--jobs N] [--verbose] [--no-optimize]
```

| Option | Meaning |
|---|---|
| `--from` | The source: a `postgres://user:password@host:port/db?sslmode=…` URL, a libpq `key=value` string, or a bare database name (libpq's `PG*` variables fill the rest). Usually `"$DATABASE_URL"`. |
| `--to` | The new PGBoxDB database. Must not exist (`pgboxdb copy` never overwrites). Same name rules as `pgboxdb create`. |
| `--schema-only` | Tables, indexes, constraints, sequences, views, functions, triggers, types and extensions, but no table rows, except the rows of migration-bookkeeping tables (`schema_migrations`, `ar_internal_metadata`, `django_migrations`, `_prisma_migrations`, `alembic_version`, Flyway, Knex, goose, …) so the framework sees its migrations as applied. For a production-sized source this is what you want; agents then run new migrations and load fixtures. |
| `--jobs N` | Parallel restore workers (`pg_restore --jobs`). Helps on large sources with many tables and indexes. |
| `--no-optimize` | Skip the final `VACUUM (FREEZE, ANALYZE)`. Not recommended; see "Why the template is frozen". |
| `--verbose` | Show `pg_dump` / `pg_restore` output (passwords masked). |
| `--json` (global) | One JSON object on stdout, for agents and scripts. |

`--data-only` is not offered: it would need an existing, schema-compatible
destination and a validation of that compatibility, which this release does
not do. Copy the whole database or the schema only.

### Exit codes

`0` done · `1` tools missing, source unreachable or unreadable, incompatible,
dump or restore failed, interrupted · `2` bad URL, name or option · `3` PGBoxDB not
initialised or not running · `4` the destination already exists.

### JSON

```json
{
  "status": "success", "operation": "copy",
  "source": {"engine": "postgresql", "version": "18.6", "host": "127.0.0.1", "port": 5432, "user": "app", "database": "myapp"},
  "destination": {"engine": "pgboxdb", "database": "myapp_template", "url": "postgres://postgres:…@127.0.0.1:5433/myapp_template", "size_bytes": 32505856, "port": 5433},
  "schema_only": false, "optimized": true,
  "tools": {"pg_dump": "18.6", "dir": "/opt/homebrew/opt/postgresql@18/bin"},
  "extensions": ["citext", "pgcrypto"],
  "objects": {"tables": 2, "sequences": 3, "views": 1, "materialized_views": 1, "indexes": 5, "functions": 2},
  "stages": [{"name": "export", "detail": "schema and data, 592.3 kB in 0.3 s", "ms": 301}, "…"],
  "warnings": [],
  "duration_ms": 1990
}
```

Errors use the CLI's standard `{"error": {"code", "exit_code", "message",
"hint"}}` plus a `details` object where there is structure to give, for
example `{"unsupported_extensions": ["timescaledb"], "reasons": [...]}`. The
source password never appears in any output.

## How the import works

1. **Client tools.** `pg_dump` and `pg_restore` are located (see below).
2. **Source.** A schema-only `pg_dump` connects to the source. Its table of
   contents tells PGBoxDB the server version, the extensions, the procedural
   languages and every object.
3. **Compatibility gate.** Extensions must exist in the engine's
   `pg_available_extensions`; functions must be PL/pgSQL or SQL; logical
   replication subscriptions are refused; publications are skipped with a
   warning. An incompatible source stops here with the reasons listed and
   nothing created.
4. **Export.** One full custom-format `pg_dump` (a single snapshot, so the copy
   is consistent even while the application keeps writing to the source) into
   `$PGBOXDB_HOME/tmp/copy-<pid>-<random>/`, mode 0700. With `--schema-only` the
   first dump is the only dump.
5. **Staging.** A database named `_pgboxdbcopy_<destination>_<random>` is created.
   `pgboxdb ls` shows it as `importing`.
6. **Restore.** `pg_restore --exit-on-error --no-owner --no-privileges` into
   the staging database over PGBoxDB's Unix socket. Every object is created by
   PGBoxDB's `postgres` superuser; source roles and grants are not needed and not
   copied. The first error aborts the import. There is no best-effort mode.
7. **Validate.** The restored catalog is compared with the archive's inventory
   (tables, sequences, views, materialized views, functions must match exactly;
   indexes at least), and the database must answer queries.
8. **Optimize.** `VACUUM (FREEZE, ANALYZE)` and a checkpoint.
9. **Publish.** `ALTER DATABASE _pgboxdbcopy_… RENAME TO <destination>`, an atomic
   catalog operation. Until it happens the destination name does not exist.

On failure, interruption (Ctrl-C) or a kill, the staging database and the
temporary dump are removed. If the process dies before it can clean up, the
next `pgboxdb copy` removes the leftovers (it records which pid owns which staging
database under `$PGBOXDB_HOME/copy/`), and `pgboxdb ls` shows such a database as
`abandoned` in the meantime.

### Why the template is frozen

A PostgreSQL row keeps "hint bits" that record whether its transaction
committed. Freshly loaded rows have none; the first `SELECT` that touches them
sets the bits and rewrites the page. On a copy-on-write clone every rewritten
page stops being shared with the template. Measured on APFS (`benchmarks.md`):
with an unfrozen template, the first full read of 100 clones of a 31 MB
template cost 3.0 GB of real disk (30 MB per clone) and took 2.6× longer;
with the frozen template the same reads cost 13 MB. For a 1.5 GB template,
100 clones read every table once for 1.8 MB of disk. `VACUUM (FREEZE,
ANALYZE)` on the template sets every hint bit once, so clones read without
writing, and `ANALYZE` gives the planner statistics from the start.

## PostgreSQL client tools

`pgboxdb copy` runs the PostgreSQL client programs `pg_dump` and `pg_restore`. They
are **not bundled** with PGBoxDB (the distribution stays a self-contained engine
with no libpq); if you have PostgreSQL on the machine, you already have them.
PGBoxDB looks in `PGBOXDB_PG_BINDIR`, then `PATH`, then the usual places (Homebrew
`postgresql@N` and `libpq`, Postgres.app, `/usr/lib/postgresql/N/bin`,
`/usr/pgsql-N/bin`) and uses the newest version it finds. `pgboxdb doctor` reports
which. Without PostgreSQL installed:

| OS | Install |
|---|---|
| macOS | `brew install libpq` (then put `$(brew --prefix libpq)/bin` on `PATH`, or set `PGBOXDB_PG_BINDIR` to it) or `brew install postgresql@18` |
| Debian / Ubuntu | `apt install postgresql-client-18` (from apt.postgresql.org) |
| Fedora / RHEL | `dnf install postgresql18` |
| Alpine | `apk add postgresql18-client` |

`pg_dump` must be at least as new as the source server (PostgreSQL's own
rule). The archive is restored into a PostgreSQL 18 compatible engine; using
the PostgreSQL 18 tools is the tested configuration.

## Supported sources

- **Versions.** Sources from PostgreSQL 14 through 18 are expected to work
  (`pg_dump` 18 dumps them all). PGBoxDB is PostgreSQL 18 compatible; a source
  newer than 18 produces a warning and may fail on new features.
- **Extensions.** PGBoxDB ships a fixed set of built-in extensions and cannot load
  C extensions. Available in this build: amcheck, auto_explain, bloom,
  btree_gin, btree_gist, citext, cube, dblink, earthdistance, file_fdw,
  fuzzystrmatch, hstore, intarray, isn, lo, ltree, pageinspect, passwordcheck,
  pg_buffercache, pg_freespacemap, pg_logicalinspect, pg_overexplain,
  pg_prewarm, pg_stat_statements, pg_surgery, pg_trgm, pg_visibility,
  pg_walinspect, pgcrypto, pgrowlocks, pgstattuple, plpgsql, postgres_fdw, seg,
  sslinfo, tablefunc, tcn, tsm_system_rows, tsm_system_time, unaccent,
  uuid-ossp, vector (pgvector). `SELECT name FROM pg_available_extensions` on
  your PGBoxDB is authoritative. **PostGIS, TimescaleDB, Citus, pg_cron,
  pg_partman and other C extensions are not available**; a source that uses
  one is refused before anything is created.
- **Languages.** PL/pgSQL and SQL. PL/Python, PL/Perl, PL/v8 are refused.
- **Not copied, by design:** ownership, grants and default privileges (every
  object belongs to PGBoxDB's `postgres`), tablespaces, publications and
  subscriptions, security labels. Roles are not part of a database dump; a
  schema object that names a role (a row-level-security policy's `TO role`)
  fails to restore until you create the role in PGBoxDB
  (`psql "$(pgboxdb url postgres)" -c 'CREATE ROLE name'`) and retry.
- **Large objects** are copied.

## Limitations

- The copy is logical: time grows with the source's size and complexity.
  Clones afterwards are what is fast. On a laptop a 157 MB source took 4 s
  and a 1.5 GB source 33–49 s (28 s with `--jobs 4`); a 10 GB source will
  take proportionally longer and needs that much free disk under `$PGBOXDB_HOME`
  (the compressed dump plus the restored data; the 1.5 GB source needed a
  99 MB dump and 1.7 GB of data).
- Row counts are not compared against the source (PGBoxDB never connects to the
  source itself; only `pg_dump` does). What is verified: `pg_restore`
  succeeded with `--exit-on-error`, the object inventory matches, the database
  answers.
- `--data-only`, refreshing an existing template in place (`pgboxdb refresh`),
  and overwriting a destination are not offered. To refresh: copy to a new
  name, then `pgboxdb drop` the old one when its branches are gone.
- The dump goes through a temporary file (streaming would need
  `pg_dump | pg_restore` without a table of contents and without the
  integrity check); it is removed when the copy finishes.
- Cloning is copy-on-write only on filesystems that clone blocks (APFS, ZFS,
  btrfs, XFS with reflink). Elsewhere `pgboxdb clone` is a full file copy and
  `pgboxdb clone --json` reports `"method": "copy"`. See [`storage.md`](storage.md).

## Sensitive data

`pgboxdb copy` copies what the source holds. Nothing is masked, nothing is sent
anywhere: the dump is written to `$PGBOXDB_HOME/tmp` (mode 0700) and removed, the
data lands in `$PGBOXDB_HOME/data`, and PGBoxDB sends no telemetry. The source password
is handed to `pg_dump` through the `PGPASSWORD` environment variable, never on
`pg_dump`'s command line, and is masked in every message. The `pgboxdb copy`
command line itself is visible to other local users while it runs (`ps`) and
to your shell history; if that matters, leave the password out of the URL and
let libpq read it from `PGPASSWORD` or `~/.pgpass`, which `pg_dump` honours. If the source holds personal
data you do not want on a developer machine, copy `--schema-only` and seed the
branches with fixtures, or point `--from` at an anonymised copy. A remote
`--from` URL moves that database onto this machine; honour your data policies.

## Refreshing a template

A template is a point-in-time copy. To pick up new schema or data:

```sh
pgboxdb copy --from "$DATABASE_URL" --to myapp_template_v2
# new agents clone from myapp_template_v2; existing branches keep their data
pgboxdb drop myapp_template        # once no branch needs the old one
```

A built-in `pgboxdb refresh <template> --from <url>` that does this with an atomic
swap is planned; the staging-and-rename design above is what it will reuse.
Real-time synchronisation is a separate project (PGSync), not a template
feature.

## Cleaning up

`pgboxdb drop agent-1` removes a branch; a branch created with `--ttl` removes
itself. `pgboxdb ls` shows every database with its TTL and size. A template is
permanent until you `pgboxdb drop` it. An interrupted copy leaves nothing; if a
copy process was killed, the next `pgboxdb copy` removes its staging database and
temp files, and `pgboxdb doctor` lists the client tools it would use.

## Measured on this implementation

macOS M1 Pro, APFS, engine v0.1.0, PostgreSQL 18.6 source on the same
machine (`benchmarks/pgboxdb/copy_bench.py`; method and all rows in
[`benchmarks.md`](benchmarks.md)):

| Source | `pgboxdb copy` | clone p50 (100 clones) | disk after 100 clones read every table |
|---|---|---|---|
| 31 MB | 1.9–3.8 s | 40 ms | 13 MB (3.0 GB without `--no-optimize`'s freeze) |
| 157 MB | 3.9 s | 44 ms | 22 MB |
| 1.5 GB | 33–49 s (28 s with `--jobs 4`; schema-only 0.5 s) | 47 ms | 1.8 MB |

The CLI and the tools use about 17 MB of memory throughout; the engine peaked
at 86–190 MB during the restores. Disk per branch afterwards follows what the
branch writes: a contiguous 2 % change of the largest table cost 3–19 MB per
clone, while scattered row updates across a large table can un-share most of
it (worst case measured: 1.35 GB per clone of a 1.5 GB table).
