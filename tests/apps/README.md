# Application fixtures

Each fixture connects a real driver to a database on the assembled PGBoxDB
distribution and exercises the operations that matter for that stack. The
orchestrator creates a database with `pgboxdb`, hands the fixture its
`DATABASE_URL`, runs it, and drops the database.

```sh
PGBOXDB_DIST=<prefix> tests/apps/run_all.sh            # all five
PGBOXDB_DIST=<prefix> tests/apps/run_all.sh rails node # a subset
```

| Fixture | Driver | Exercises |
|---|---|---|
| psql   | psql 18            | DDL, DML, `\copy` in/out, transaction + rollback, JSONB |
| node   | node-postgres 8    | pool, 500 parameterized inserts, rollback, named/prepared statement, 23505, JSONB |
| python | psycopg 3          | `executemany`, COPY, server-side cursor, rollback, 42P07, JSONB |
| go     | jackc/pgx v5       | CopyFrom 2000 rows, transaction + rollback, batch, unique violation, JSONB |
| rails  | ActiveRecord 8.1   | migrations up/down, seed, transaction + savepoint rollback, RecordNotUnique, InvalidForeignKey, JSONB, 8 parallel threads, schema dump, cleanup |

Each prints `APP <name>: PASS | FAIL | COVERAGE GAP (<reason>)`. A missing
driver or toolchain is a reported coverage gap, not a pass. `run_all.sh`
exits non-zero only if a fixture FAILED. node, python and go install their
driver on first run (npm / pip / go get); those downloads need network and
are reported as coverage gaps if they fail.
