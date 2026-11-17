# PGBoxDB compatibility suite

Behavioural checks run against a live PGBoxDB database and, when a PostgreSQL 18
control is given, against PostgreSQL too: every step must return the same
rows and the same SQLSTATE on both engines. Minitest + the `pg` gem only.

Origin: PGRun's `test/pgboxdb_compat` (same author), carried here so the engine
repository gates on it.

```sh
PGBOXDB_DIST=<prefix> PG18_BINDIR=/opt/homebrew/opt/postgresql@18/bin tests/compat/run.sh
```

`run.sh` initialises a throw-away PGBoxDB home, creates the `compat` database,
starts a throw-away PostgreSQL 18 control from `PG18_BINDIR`, runs every
`*_test.rb`, prints the per-category summary and cleans up. Without
`PG18_BINDIR` it prints `COVERAGE GAP: no PostgreSQL control` and asserts
PostgreSQL's documented behaviour only.

Categories: SQL_COMPATIBILITY, TRANSACTION, MVCC, CONSTRAINT (see
`compat_helper.rb`). Add a case for every compatibility bug found in the wild.
