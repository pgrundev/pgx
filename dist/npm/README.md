# pgxdb

**PGX. Postgres for agents.** One Postgres-compatible runtime, thousands of
disposable databases. No Docker, no Rust, no system PostgreSQL.

```bash
npx pgxdb@latest
```

That one command downloads the PGX native binary for your machine (Apple
silicon macOS, or Linux arm64/x86-64), verifies its SHA-256, starts a local PGX runtime, creates
a default `app` database and prints its connection URL:

```console
$ npx pgxdb@latest
pgxdb: downloading PGX 0.1.0 (darwin-arm64) from https://github.com/pgrundev/pgx/releases/download/v0.1.0/pgx-0.1.0-darwin-arm64.tar.gz
pgxdb: installed PGX 0.1.0 to /Users/you/.pgx/dist/0.1.0
PGX is ready.

  postgres://postgres:…@127.0.0.1:5432/app

  database   app (created)
  runtime    started (pid 23469, port 5432)
  home       /Users/you/.pgx

Connect:
  psql "postgres://postgres:…@127.0.0.1:5432/app"
```

Point `DATABASE_URL` at that URL. Any PostgreSQL client or ORM works.

## Every pgx command

With no arguments `npx pgxdb` is `pgx run`. Anything else is passed straight to
the `pgx` CLI:

```bash
npx pgxdb url app                      # connection URL
npx pgxdb clone app agent-001 --ttl 5m # a 5-minute copy-on-write branch
npx pgxdb ls                           # databases, TTL, expiry, size
npx pgxdb --json ls                    # machine-readable (for agents)
npx pgxdb stop
```

`npm install -g pgxdb` puts both `pgxdb` and `pgx` on your PATH.

## How it works

- The binary is downloaded once per version into `~/.pgx/dist/<version>` — the
  same place `curl -fsSL https://pgx.dev/install | sh` installs to — and reused
  by every later run. Nothing is compiled; nothing needs root.
- `pgxdb@X.Y.Z` installs PGX `X.Y.Z`, so the wrapper and the binary are always
  a tested pair. `npx pgxdb@latest` therefore means the latest PGX release.
- Data lives under `~/.pgx` (`PGX_HOME`). `pgx run` is idempotent and never
  resets an existing database.

Environment overrides:

| Variable | Meaning |
|---|---|
| `PGX_VERSION` | install a different PGX release than the package pins |
| `PGX_INSTALL_DIR` | where the distribution tree goes (default `~/.pgx/dist/<version>`) |
| `PGX_BASE_URL` | release download base (default GitHub releases) |
| `PGX_HOME`, `PGX_PORT`, `PGX_PASSWORD`, `PGX_DURABILITY` | passed through to `pgx` (see `npx pgxdb help`) |

Requires Node.js 18 or newer and `tar` (present on every supported OS).
Intel Macs are supported from the first release after 0.1.2 (older releases
publish no darwin-amd64 binary; `npx pgxdb` says so). Windows is not supported.

## Links

- Source and docs: https://github.com/pgrundev/pgx
- CLI reference: https://github.com/pgrundev/pgx/blob/main/docs/pgx/local-dev/cli.md
- License: AGPL-3.0
