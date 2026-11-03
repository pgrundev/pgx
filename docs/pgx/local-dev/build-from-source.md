# Building PGX from source

The prebuilt distribution (`npx pgxdb@latest` or `curl -fsSL https://pgx.dev/install | sh`, see [`install.md`](install.md)) is the normal way to run PGX. This page is for building the engine yourself, running a bare runtime without the `pgx` CLI, and minting databases from a sealed template by hand.

## Build

You need the Rust toolchain (`rust-toolchain.toml` pins Rust 1.96.0;
rustup fetches it), the RE2 regex library, and the PostgreSQL 18 client
tools for `initdb` and `psql`:

- Debian/Ubuntu: `sudo apt-get install -y build-essential pkg-config libre2-dev`,
  plus `postgresql-18` and `postgresql-client-18` from the
  [PGDG apt repo](https://www.postgresql.org/download/linux/ubuntu/)
  (then `export PATH="/usr/lib/postgresql/18/bin:$PATH"`).
- macOS: `brew install re2 pkg-config postgresql@18`, then
  `export PATH="$(brew --prefix postgresql@18)/bin:$PATH"`.

```bash
cargo build --release --locked --bin postgres
```

The server binary is `target/release/postgres`. Release builds refuse to
compile without RE2 on purpose: the fallback regex engine is much slower.

At run time the server loads two libraries with `dlopen` rather than
linking them: libxml2 for the `xml` type and SQL/XML functions, and ICU for
ICU collations. The PostgreSQL 18 packages above install both. A host
without libxml2 still starts, but XML functions raise `unsupported XML
feature` (SQLSTATE 0A000) with a hint naming the library, and
`pgrust_runtime_status()` reports `libraries.libxml2.available = false`.
Debian/Ubuntu package: `libxml2` (`libxml2-16` from Ubuntu 25.10); macOS
ships libxml2 with the system.

## Run a PGX runtime

```bash
# Create a cluster and apply the PGX profile.
initdb -D /tmp/pgx-data --no-locale --encoding UTF8 -U postgres
cat configs/pgx-ephemeral.conf >> /tmp/pgx-data/postgresql.conf

# The server reads timezone and other data files from a Postgres share dir.
export PGRUST_PGSHAREDIR=/usr/share/postgresql/18      # macOS: "$(brew --prefix postgresql@18)/share/postgresql"
export PGRUST_TZDIR=/usr/share/zoneinfo                # macOS: "$PGRUST_PGSHAREDIR/timezone"

ulimit -s 65520
RUST_MIN_STACK=33554432 target/release/postgres \
  -D /tmp/pgx-data -k /tmp -p 5432 \
  -c listen_addresses= -c io_method=sync -c max_stack_depth=60000 \
  -c pgrust.ephemeral_db_prefix=tdb_ \
  -c pgrust.ephemeral_db_mint_roles=postgres
```

Put the data directory on a ZFS dataset with block cloning enabled to get
copy-on-write branches (the profile sets `file_copy_method = clone`). On a
filesystem without block cloning, remove that line from the profile and
databases are created by copying files.

## Make a template and mint databases

```bash
# Build a template once, then seal it.
psql -h /tmp -U postgres -c "CREATE DATABASE tpl_app"
psql -h /tmp -U postgres -d tpl_app -c "CREATE TABLE users (id bigserial PRIMARY KEY, email text UNIQUE)"
psql -h /tmp -U postgres -c "SELECT pgrust_seal_template('tpl_app')"

# Connecting to <prefix><template>__<token> creates that database.
psql -h /tmp -U postgres -d tdb_tpl_app__agent42 -c "INSERT INTO users (email) VALUES ('a@example.com')"
psql -h /tmp -U postgres -d tdb_tpl_app__agent43 -c "SELECT count(*) FROM users"   # 0: isolated

# Runtime status as JSON.
psql -h /tmp -U postgres -Atc "SELECT pgrust_runtime_status()"
```

Minted databases are dropped `pgrust.ephemeral_db_grace` seconds after
their last session ends, unless they are pinned with
`pgrust_pin_database()`. All control functions are listed in
[`docs/pgx/pgrun-interface.md`](../pgrun-interface.md).
