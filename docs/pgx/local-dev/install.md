# Installing PGX

PGX ships as one self-contained, relocatable directory. It needs **no system
PostgreSQL**; the only runtime dependencies are libc and, for XML and ICU
collations, the host's libxml2 and ICU (present by default on macOS and in the
Docker image).

## One command with npm

```sh
npx pgxdb@latest
```

`pgxdb` is a small npm wrapper around the `pgx` CLI (source: `dist/npm/`). It
downloads the release tarball for your OS and CPU into `~/.pgx/dist/<version>`
(the same place the shell installer uses), **verifies the checksum**, and then
runs `pgx run`: initialise if needed, start, ensure the `app` database, print
its URL. Any arguments are passed to `pgx` unchanged (`npx pgxdb ls`,
`npx pgxdb --json url app`, …). The download happens once per version; later
runs start in well under a second.

`pgxdb@X.Y.Z` installs PGX `X.Y.Z`, so `@latest` is the latest PGX release.
`PGX_VERSION`, `PGX_INSTALL_DIR` and `PGX_BASE_URL` override the version,
cache directory and download base. `npm install -g pgxdb` provides both
`pgxdb` and `pgx` on your PATH. Node.js 18+ is required. Supported: macOS
(Apple silicon and Intel) and Linux (arm64, x86-64); Windows is not
supported (see the platform table below).

## One-line install (shell)

```sh
curl -fsSL https://pgx.dev/install | sh
```

The installer detects your OS and CPU, downloads the matching release tarball
and its `.sha256`, **verifies the checksum before installing**, unpacks into
`~/.pgx/dist/<version>` and links `pgx` into `~/.local/bin`. It never uses root
and refuses an unsupported platform or a tampered download.

Environment overrides: `PGX_VERSION`, `PGX_INSTALL_DIR`, `PGX_BIN_DIR`,
`PGX_BASE_URL`. Ensure `PGX_BIN_DIR` (default `~/.local/bin`) is on your `PATH`.

Then get a database in one step:

```sh
pgx run
```

`pgx run` initialises PGX if needed, starts it, ensures a default `app`
database, and prints its connection URL. It is idempotent — run it again any
time; it never resets your data.

## From a release tarball

Download `pgx-<version>-<os>-<arch>.tar.gz`, verify it against `checksums.txt`,
extract anywhere, and run `bin/pgx`.

```sh
shasum -a 256 -c pgx-<version>-<os>-<arch>.tar.gz.sha256
tar -xzf pgx-<version>-<os>-<arch>.tar.gz
./pgx-<version>-<os>-<arch>/bin/pgx version
```

## Importing an existing PostgreSQL database

`pgx copy --from "$DATABASE_URL" --to myapp_template` copies a PostgreSQL
database into PGX as a template to branch from. This one command needs the
PostgreSQL client tools `pg_dump` and `pg_restore` on the machine (not
bundled; a PostgreSQL installation provides them, otherwise `brew install
libpq` / `apt install postgresql-client-18`). Everything else in PGX is
self-contained. See [copy.md](copy.md).

## Docker

```sh
docker run --init -p 5432:5432 -e PGX_PASSWORD=secret -v pgxdata:/var/lib/pgx ghcr.io/pgrundev/pgx
psql "postgres://postgres:secret@localhost:5432/postgres"
```

`-v pgxdata:/var/lib/pgx` persists data; `--init` gives the container an init
that reaps processes.

## From source

See the repository README "Quickstart → Build": the Rust toolchain (pinned by
`rust-toolchain.toml`), RE2, and PostgreSQL 18 client tools, then
`cargo build --release --bin postgres --bin pgx` and `dist/assemble.sh`.

## Supported platforms

| Platform | Status |
|---|---|
| macOS arm64 (Apple silicon) | supported, verified |
| Linux arm64 | supported, verified |
| Linux x86-64 | supported, built and smoke-tested in CI |
| macOS x86-64 (Intel) | supported from the first release after v0.1.2, built and smoke-tested in CI; verified on macOS 15.8 (Core i5-8279U): lifecycle, databases and clone suites and `smoke_packaged.sh` pass. Releases up to v0.1.2 publish no Intel binary |
| Windows | not supported |
