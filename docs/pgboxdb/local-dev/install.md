# Installing PGBoxDB

PGBoxDB ships as one self-contained, relocatable directory. It needs **no system
PostgreSQL**; the only runtime dependencies are libc and, for XML and ICU
collations, the host's libxml2 and ICU (present by default on macOS and in the
Docker image).

## One command with npm

```sh
npx pgboxdb@latest
```

`pgboxdb` is a small npm wrapper around the `pgboxdb` CLI (source: `dist/npm/`). It
downloads the release tarball for your OS and CPU into `~/.pgboxdb/dist/<version>`
(the same place the shell installer uses), **verifies the checksum**, and then
runs `pgboxdb run`: initialise if needed, start, ensure the `app` database, print
its URL. Any arguments are passed to `pgboxdb` unchanged (`npx pgboxdb ls`,
`npx pgboxdb --json url app`, …). The download happens once per version; later
runs start in well under a second.

`pgboxdb@X.Y.Z` installs PGBoxDB `X.Y.Z`, so `@latest` is the latest PGBoxDB release.
`PGBOXDB_VERSION`, `PGBOXDB_INSTALL_DIR` and `PGBOXDB_BASE_URL` override the version,
cache directory and download base. `npm install -g pgboxdb` provides both
`pgboxdb` and `pgboxdb` on your PATH. Node.js 18+ is required. Supported: Apple
silicon Macs and Linux (arm64, x86-64); Intel Macs and Windows are not
supported (see the platform table below).

## One-line install (shell)

```sh
curl -fsSL https://pgboxdb.com/install | sh
```

The installer detects your OS and CPU, downloads the matching release tarball
and its `.sha256`, **verifies the checksum before installing**, unpacks into
`~/.pgboxdb/dist/<version>` and links `pgboxdb` into `~/.local/bin`. It never uses root
and refuses an unsupported platform or a tampered download.

Environment overrides: `PGBOXDB_VERSION`, `PGBOXDB_INSTALL_DIR`, `PGBOXDB_BIN_DIR`,
`PGBOXDB_BASE_URL`. Ensure `PGBOXDB_BIN_DIR` (default `~/.local/bin`) is on your `PATH`.

Then get a database in one step:

```sh
pgboxdb run
```

`pgboxdb run` initialises PGBoxDB if needed, starts it, ensures a default `app`
database, and prints its connection URL. It is idempotent — run it again any
time; it never resets your data.

## From a release tarball

Download `pgboxdb-<version>-<os>-<arch>.tar.gz`, verify it against `checksums.txt`,
extract anywhere, and run `bin/pgboxdb`.

```sh
shasum -a 256 -c pgboxdb-<version>-<os>-<arch>.tar.gz.sha256
tar -xzf pgboxdb-<version>-<os>-<arch>.tar.gz
./pgboxdb-<version>-<os>-<arch>/bin/pgboxdb version
```

## Importing an existing PostgreSQL database

`pgboxdb copy --from "$DATABASE_URL" --to myapp_template` copies a PostgreSQL
database into PGBoxDB as a template to branch from. This one command needs the
PostgreSQL client tools `pg_dump` and `pg_restore` on the machine (not
bundled; a PostgreSQL installation provides them, otherwise `brew install
libpq` / `apt install postgresql-client-18`). Everything else in PGBoxDB is
self-contained. See [copy.md](copy.md).

## Docker

```sh
docker run --init -p 5432:5432 -e PGBOXDB_PASSWORD=secret -v pgboxdbdata:/var/lib/pgboxdb ghcr.io/pgrundev/pgboxdb
psql "postgres://postgres:secret@localhost:5432/postgres"
```

`-v pgboxdbdata:/var/lib/pgboxdb` persists data; `--init` gives the container an init
that reaps processes.

## From source

See [`build-from-source.md`](build-from-source.md): the Rust toolchain (pinned by
`rust-toolchain.toml`), RE2, and PostgreSQL 18 client tools, then
`cargo build --release --bin postgres --bin pgboxdb` and `dist/assemble.sh`.

## Supported platforms

| Platform | Status |
|---|---|
| macOS arm64 (Apple silicon) | supported, verified |
| Linux arm64 | supported, verified |
| Linux x86-64 | supported, built and smoke-tested in CI |
| macOS x86-64 (Intel) | **not supported yet**: no binary is published. `npx pgboxdb` and the shell installer both refuse with a clear message. The release workflow has an experimental, non-blocking Intel build leg whose artifact is kept out of releases until it has been validated |
| Windows | not supported |
