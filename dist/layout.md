# PGBoxDB distribution layout

A PGBoxDB distribution is one directory. It can live anywhere (`pgboxdb` locates the
engine and the share trees relative to its own path), it has no dependency on
a system PostgreSQL, and every file in it is a regular file.

```
pgboxdb-<version>-<os>-<arch>/
  bin/pgboxdb                     the CLI
  bin/postgres                the engine (PostgreSQL 18 compatible, pgrust-based)
  share/timezone/             zoneinfo compiled by PostgreSQL 18 (tzdata, public domain)
  share/timezonesets/         timezone abbreviation sets (PostgreSQL)
  share/tsearch_data/         text-search stop words, synonyms, thesaurus, unaccent rules
  share/extension/            vendored contrib extension scripts (*.control, *.sql), plpgsql
  share/pgboxdb/base-cluster/     a pristine `initdb --no-locale -E UTF8 -U postgres` cluster
                              without postgresql.conf / pg_hba.conf (pgboxdb writes them)
  share/pgboxdb/dist.json         {"pgboxdb_version","engine_commit","postgres_compat","initdb_version","os","arch","built_at"}
  share/pgboxdb/layout.md         this file
  LICENSE                     AGPL-3.0 (PGBoxDB / pgrust)
  NOTICE
  THIRD-PARTY-LICENSES.md     PostgreSQL License (share trees, base cluster), tzdata, Rust crates
```

`<os>` is `darwin` or `linux`; `<arch>` is `arm64` or `amd64`.

## How the engine finds its files

The engine resolves its share directory as `PGRUST_PGSHAREDIR` (environment),
then the build-time value, then `<dir of the binary>/../share` — the
PostgreSQL rule. `pgboxdb start` sets `PGRUST_PGSHAREDIR=<prefix>/share` and
`PGRUST_TZDIR=<prefix>/share/timezone` explicitly, so a relocated or
symlinked installation never depends on build-time paths.

## Why a base cluster instead of initdb

PGBoxDB has no `initdb` of its own: a cluster is created by stock PostgreSQL 18
`initdb` and the engine opens that stock data directory. Running `initdb` on
the user's machine would need PostgreSQL 18 installed. The distribution
therefore ships the output of `initdb` (about 39 MB, 7 MB compressed) built on
the release machine for the same OS and architecture, and `pgboxdb init` copies
it. The data directory format is PostgreSQL 18's; `pg_control` records the
build parameters (`maxalign`, block size, float format…) and the engine
verifies them on start.
