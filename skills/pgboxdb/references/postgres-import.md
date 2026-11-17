# Importing from PostgreSQL: `pgboxdb copy`

```sh
pgboxdb copy --from "<url>" --to <name>_template --schema-only [--jobs N] [--verbose]
```

`pgboxdb copy` runs `pg_dump` against the source (read-only, no locks beyond a
normal dump) and `pg_restore` into a fresh PGBoxDB database, checks the dump's
table of contents for extensions and features PGBoxDB cannot restore before
writing anything, restores into a staging name and renames atomically, then
runs `VACUUM (FREEZE, ANALYZE)` so branches cloned from it share pages
instead of rewriting them on first read. Output redacts the URL.

## Source rules

1. The connection string comes from the user's environment or an existing
   file (`"$DATABASE_URL"`, `"$(grep ^DATABASE_URL .env | cut -d= -f2-)"`),
   never from the conversation. If it has to be typed, the user types it into
   their own shell, not into the chat.
2. Passwords on a command line are visible in `ps` and shell history for the
   duration of the command; `pgboxdb copy` moves the password into `PGPASSWORD`
   for the tools it runs and never prints it. Prefer `PGPASSWORD`/`.pgpass`
   when the user has them.
3. `local` sources: proceed schema-only without asking. `remote`: ask first.
   `sensitive`: ask, say it looks like production, never copy data.
4. Data copies (`pgboxdb copy` without `--schema-only`) need an explicit yes in
   this conversation, every time, and a sentence about what will be copied
   (database name, size if known). Real data in an agent branch is still real
   data on the developer's disk.

## Naming

`<app>_template` (for example `myapp_template`). Never a name that exists:
`pgboxdb copy` refuses to overwrite and so should you. The template is for
branching only; nothing connects to it, so it stays idle, which `CREATE
DATABASE … TEMPLATE` requires.

## Compatibility

PGBoxDB restores plain PostgreSQL schemas: tables, constraints, indexes, views,
functions, triggers, sequences, enums, domains, partitions, row-level
security, the bundled extensions (`pgcrypto`, `uuid-ossp`, `citext`,
`hstore`, `pg_trgm`, `btree_gist`, `btree_gin`, `unaccent`, `ltree`,
`intarray`, `tablefunc`, `pg_stat_statements` and the other contrib modules
that ship with it). It refuses up front, naming the object, when the dump
needs an extension it does not have (PostGIS, TimescaleDB, pgvector unless
bundled in this release), a role that is not `postgres` as an owner it
cannot map, or tablespaces. In that case tell the user exactly what was
refused and offer: a schema without that extension (`--schema-only` plus
editing the dump is manual work; suggest it only if the user wants), or
keeping that project on its PostgreSQL. Never silently drop objects.

Roles: everything restored is owned by `postgres`; grants to other roles are
skipped with a note in `--verbose` output. Application roles are not
recreated; `DATABASE_URL` for branches uses the PGBoxDB superuser.

## After the import

```sh
pgboxdb ls                       # the template, permanent, with its size
pgboxdb agent setup --source <name>_template
```

Refreshing later: import into a new name, test a branch of it, then change
`source` in `.pgboxdb.toml` (ask; the old template can be dropped by the user).
