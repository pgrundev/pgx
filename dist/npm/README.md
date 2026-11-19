# pgboxdb

**PGBoxDB. Postgres for agents.** One Postgres-compatible runtime, thousands of
disposable databases. No Docker, no Rust, no system PostgreSQL.

```bash
npx pgboxdb@latest
```

That one command downloads the PGBoxDB native binary for your machine (Apple
silicon macOS, or Linux arm64/x86-64), verifies its SHA-256, starts a local PGBoxDB runtime, creates
a default `app` database and prints its connection URL:

```console
$ npx pgboxdb@latest
pgboxdb: downloading PGBoxDB 0.2.0 (darwin-arm64) from https://github.com/pgrundev/pgboxdb/releases/download/v0.2.0/pgboxdb-0.2.0-darwin-arm64.tar.gz
pgboxdb: installed PGBoxDB 0.2.0 to /Users/you/.pgboxdb/dist/0.2.0
PGBoxDB is ready.

  postgres://postgres:…@127.0.0.1:5432/app

  database   app (created)
  runtime    started (pid 23469, port 5432)
  home       /Users/you/.pgboxdb

Connect:
  psql "postgres://postgres:…@127.0.0.1:5432/app"
```

Point `DATABASE_URL` at that URL. Any PostgreSQL client or ORM works.

## Every pgboxdb command

With no arguments `npx pgboxdb` is `pgboxdb run`. Anything else is passed straight to
the `pgboxdb` CLI:

```bash
npx pgboxdb url app                      # connection URL
npx pgboxdb clone app agent-001 --ttl 5m # a 5-minute copy-on-write branch
npx pgboxdb ls                           # databases, TTL, expiry, size
npx pgboxdb --json ls                    # machine-readable (for agents)
npx pgboxdb stop
```

`npm install -g pgboxdb` puts `pgboxdb` on your PATH.

## How it works

- The binary is downloaded once per version into `~/.pgboxdb/dist/<version>` — the
  same place `curl -fsSL https://pgboxdb.com/install | sh` installs to — and reused
  by every later run. Nothing is compiled; nothing needs root.
- `pgboxdb@X.Y.Z` installs PGBoxDB `X.Y.Z`, so the wrapper and the binary are always
  a tested pair. `npx pgboxdb@latest` therefore means the latest PGBoxDB release.
- Data lives under `~/.pgboxdb` (`PGBOXDB_HOME`). `pgboxdb run` is idempotent and never
  resets an existing database.

Environment overrides:

| Variable | Meaning |
|---|---|
| `PGBOXDB_VERSION` | install a different PGBoxDB release than the package pins |
| `PGBOXDB_INSTALL_DIR` | where the distribution tree goes (default `~/.pgboxdb/dist/<version>`) |
| `PGBOXDB_BASE_URL` | release download base (default GitHub releases) |
| `PGBOXDB_HOME`, `PGBOXDB_PORT`, `PGBOXDB_PASSWORD`, `PGBOXDB_DURABILITY` | passed through to `pgboxdb` (see `npx pgboxdb help`) |

Requires Node.js 18 or newer and `tar` (present on every supported OS).
Intel Macs are not supported yet (no binary is published; `npx pgboxdb` says
so), and Windows is not supported.

## Links

- Source and docs: https://github.com/pgrundev/pgboxdb
- CLI reference: https://github.com/pgrundev/pgboxdb/blob/main/docs/pgboxdb/local-dev/cli.md
- License: AGPL-3.0
