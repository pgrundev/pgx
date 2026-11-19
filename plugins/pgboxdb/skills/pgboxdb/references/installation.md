# Installing and checking PGBoxDB

## Is it there?

```sh
command -v pgboxdb && pgboxdb --json version      # {"pgboxdb_version":"0.1.x", …}
pgboxdb --json status                         # exit 3 when initialised but stopped; hooks start it on demand
```

The skill needs **PGBoxDB 0.2.0 or newer**
(https://github.com/pgrundev/pgboxdb/releases/tag/v0.2.0): the first release
under the PGBoxDB name. Every PGBoxDB release has `pgboxdb agent` (sessions
and Claude Code subagents get their own branches) and `pgboxdb git`.
`scripts/check-pgboxdb.sh` decides: exit 0 compatible, 3 missing, 2 too old, 4
malformed, always with one JSON line. It compares `pgboxdb --json version` →
`pgboxdb_version` as semver (`PGBOXDB_MIN_VERSION` overrides the floor). Do not use
`pgboxdb agent --help` as the test: `--help` prints the global help on every
version. An older binary: upgrade with the method it was installed with
(`npm i -g pgboxdb@latest`, or the installer again) and run the check again;
if the upgrade still reports an older version, the newer package is not yet on that
channel and the user should install from the GitHub release.

## Install, in order of preference

1. **A binary that is already on the machine** (a previous install under
   `~/.local/bin/pgboxdb`, Homebrew, or `~/.pgboxdb/dist/<version>/bin/pgboxdb`). Prefer
   it; do not download twice.
2. **npm**, when Node.js 18+ is present: `npm i -g pgboxdb` installs the
   `pgboxdb` command; the first run downloads the native release
   tarball into `~/.pgboxdb/dist/<version>`. `npx pgboxdb@latest` works without a
   global install but gives you `npx pgboxdb …` instead of `pgboxdb …`.
3. **The official installer**: `curl -fsSL https://pgboxdb.com/install | sh`.
   Verified release assets from github.com/pgrundev/pgboxdb; installs under
   `~/.pgboxdb/dist/<version>` and links `pgboxdb` into `~/.local/bin` (override with
   `PGBOXDB_BIN_DIR`). No root.

Nothing else. No third-party scripts, no building from source for a user who
just wants a database (that is for contributors).

Installing downloads from the network and writes to the user's home. Follow
the host's approval flow: say which command you will run, what it installs,
and where. If approval is denied or no method is possible, print the command
for the user to run themselves and stop.

## Platforms

macOS arm64 (Apple silicon), Linux amd64, Linux arm64. Not supported: Intel
macOS, Windows (WSL2 counts as Linux). Do not claim otherwise; on an
unsupported platform explain and stop.

Docker is not needed. PGBoxDB is a single runtime process started by `pgboxdb start`
(or by the hooks on demand).

## Ports and an existing PostgreSQL

An existing PostgreSQL on 5432 is never touched. PGBoxDB picks a free port on
its first `pgboxdb run`/`pgboxdb agent setup` (5432-5500, or `PGBOXDB_PORT`) and saves it
in its config; afterwards every URL carries that port. Do not assume 5433.
Never re-initialise (`pgboxdb init --force`) to change a port: that deletes every
PGBoxDB database. If a port conflict appears, `pgboxdb doctor` explains it.

## Prerequisites for importing a PostgreSQL database

`pgboxdb copy` needs `pg_dump` and `pg_restore` (PostgreSQL 16+ client tools) on
PATH or in `PGBOXDB_PG_BINDIR`. macOS: `brew install libpq` then
`PGBOXDB_PG_BINDIR=$(brew --prefix libpq)/bin`; Debian/Ubuntu:
`postgresql-client`. The tools must be at least as new as the source server.
