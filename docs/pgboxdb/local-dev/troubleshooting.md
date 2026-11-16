# Troubleshooting

Run `pgboxdb doctor` first; it names most problems and the fix.

| Symptom | Cause / fix |
|---|---|
| `pgboxdb init` → exit 2, "socket path would be … bytes" | `$PGBOXDB_HOME` is too deep for a Unix socket (limit 103 bytes). Set `PGBOXDB_RUN_DIR=/tmp/pgboxdb` and re-run, or use a shorter `$PGBOXDB_HOME`. |
| `pgboxdb start` → exit 4, "port … is in use" | Another process holds the port. Stop it, or `pgboxdb init --force --port N`. |
| `pgboxdb start` → exit 3, "not initialised" | Run `pgboxdb init`. |
| exit 7, "incomplete PGBoxDB installation" | A file is missing from the distribution; reinstall. `pgboxdb doctor` lists which. |
| `pgboxdb drop` → exit 6, "in use" | Clients are connected. Disconnect them, or `pgboxdb drop <db> --force`. |
| XML functions raise `0A000 unsupported XML feature` | The host has no libxml2. Install it (`libxml2` on Debian/Ubuntu); macOS and the Docker image include it. `pgboxdb doctor` shows library status. |
| `pgboxdb ls` shows a database as `invalid (interrupted drop)` | A `DROP DATABASE` was interrupted. Run `pgboxdb drop <db> --force` again. |
| `pgboxdb doctor` warns about orphan `base/<oid>` directories | A CREATE DATABASE or clone was interrupted by a crash. PGBoxDB never deletes them automatically; after confirming nothing is creating a database, remove them by hand from `$PGBOXDB_HOME/data/base`. |
| In a container, `pgboxdb stop` seems slow or restart fails after a kill | The container has no init to reap processes. Run it with `docker run --init`, or use `pgboxdb serve` (the engine is PID 1). |
| disk full → `53100` then the runtime stops | Expected: PGBoxDB is one crash domain. Free space and `pgboxdb start`; committed data is recovered. |
| `pgboxdb copy` → exit 1, "pg_dump and pg_restore were not found" | `pgboxdb copy` runs the PostgreSQL client tools, which PGBoxDB does not bundle. Install them (`brew install libpq`, `apt install postgresql-client-18`, …) or set `PGBOXDB_PG_BINDIR`. `pgboxdb doctor` shows what it finds. See [copy.md](copy.md). |
| `pgboxdb copy` → exit 1, "unsupported extension: …" | The source uses an extension PGBoxDB does not have (PostGIS, TimescaleDB, …). PGBoxDB cannot load C extensions; keep that database on PostgreSQL or drop the extension in a copy of the source. |
| `pgboxdb copy` → exit 1, "restore into PGBoxDB failed … role … does not exist" | A policy or similar object names a role; roles are not part of a database dump. `psql "$(pgboxdb url postgres)" -c 'CREATE ROLE <name>'` and retry. |
| `pgboxdb copy` → exit 4, "database … already exists" | `pgboxdb copy` never overwrites. Use another `--to` name or `pgboxdb drop` the old one first. |
| `pgboxdb ls` shows `_pgboxdbcopy_… abandoned` | A `pgboxdb copy` process was killed. The next `pgboxdb copy` removes it, or `pgboxdb drop _pgboxdbcopy_… --force`. |

Logs: `$PGBOXDB_HOME/log/pgboxdb.log`. Status as JSON: `pgboxdb --json status`.

## Coding agents

`pgboxdb agent doctor` checks the project (`.pgboxdb.toml`, hooks per host, the
source template) and, inside a session, whether `DATABASE_URL` names the
session's branch. The Skill's own list is in
[skills/pgboxdb/references/troubleshooting.md](../../../skills/pgboxdb/references/troubleshooting.md).

| Symptom | Cause / fix |
|---|---|
| `PGBOXDB_ISOLATION=failed`, `DATABASE_URL` points at port 1 | The SessionStart hook could not create a branch and failed closed; `PGBOXDB_ISOLATION_REASON` says why (missing source, busy source, PGBoxDB not initialised). Fix it, start a new session. |
| No "PGBoxDB: this session's database is …" line at session start | Hooks not installed, or the hook's shell cannot find `pgboxdb`. `pgboxdb agent doctor` → `hooks:<host>`; `pgboxdb agent install <host>` reports the self-test and the `pgboxdb` it resolved; link the binary into `~/.local/bin` for Dock-launched editors. |
| `pgboxdb agent up` → exit 6, "a branch source must be idle" | Something is connected to the template. Keep `<app>_template` for branching only. |
| Rails prints `ActiveRecord::EnvironmentMismatchError` after a green test run | The branch's `ar_internal_metadata` says `development`; the spawned `db:test:prepare` refuses to purge. Harmless. Never run `db:environment:set` in a branch. |
| Rails re-ran every migration on a fresh branch | The template was imported schema-only by a `pgboxdb copy` older than 0.1.1. Re-import (bookkeeping rows are now kept). |
| Codex commands cannot connect; `CODEX_SANDBOX_NETWORK_DISABLED=1` | The `workspace-write` sandbox blocks loopback TCP. `sandbox_workspace_write.network_access = true` in `.codex/config.toml` (the user's decision). |
| Cursor: variables missing after reopening a stopped chat | Cursor does not re-run `sessionStart` on reopen. Start a new chat, or `pgboxdb agent exec -- <command>`. |
| A branch outlived its session | Grace period (10 min) or idle TTL (2 h) not yet reached; the supervisor removes it. `pgboxdb agent down --now` drops it at once. |
