# Architecture of the local distribution

```
pgboxdb (CLI)  ──spawns / execs──>  postgres (engine, pgrust / PostgreSQL 18 compatible)
   │                                   │
   │ Unix socket (trust) + built-in    │ reads share/ (timezone, tsearch, extension)
   │ wire-protocol client              │ data in $PGBOXDB_HOME/data
   └── $PGBOXDB_HOME/config.json (0600)    └── one process, one crash domain
```

- The **engine** is unchanged from PGBoxDB: a single-process, PostgreSQL-18-wire
  and SQL-compatible database written in Rust, with mint-on-connect, sealed
  templates and copy-on-write clones.
- The **CLI** manages one cluster under `$PGBOXDB_HOME`: it copies the bundled base
  cluster (no `initdb` at runtime), writes `postgresql.conf` / `pgboxdb.conf` /
  `pg_hba.conf`, starts the engine beside itself, and talks to it over the Unix
  socket with a small built-in wire-protocol client (no driver dependency).
- The **distribution** (`dist/assemble.sh`, layout in `share/pgboxdb/layout.md`)
  bundles the engine, the CLI, the PostgreSQL share trees and a pristine base
  cluster, so it installs with no system PostgreSQL and relocates freely (the
  engine finds its share dir relative to the binary).
- **Security:** the engine binds `127.0.0.1` and `::1` only; the Unix socket
  uses trust, TCP uses scram-sha-256 with a per-install random password stored
  mode 0600. `PGBOXDB_LISTEN` can widen the listen set (the Docker image listens on
  `0.0.0.0`, still password-protected).

See `pgrun-interface.md` for how PGRun drives the same engine as a control
plane.
