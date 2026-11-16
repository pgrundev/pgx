# Branches: what they are and what a framework sees

A branch is a real PostgreSQL database created with `CREATE DATABASE …
TEMPLATE <source> STRATEGY = FILE_COPY`. On APFS, ZFS and reflink-capable
filesystems the files are cloned copy-on-write, so a branch of a multi-GB
template takes tens of milliseconds and almost no space until it writes.
Everything a database can do, a branch can do: DDL, extensions already in the
template, `TRUNCATE`, `DROP TABLE`, parallel test workers creating their own
schemas. None of it reaches the template or any other branch.

## Agent branches versus hand-made branches

| | `pgboxdb branch <src> <name>` | `pgboxdb agent …` branches |
|---|---|---|
| Name | yours | `agent-<host>-<session8>` |
| TTL | 2h from creation (`--ttl`) | 2h **idle**: every prompt/command resets it |
| End of session | nothing | retired: 10 minutes grace, then removed |
| Identity | the name | `(project, host, session id)`; the name is derived |
| Who connects | you | that one session |

`pgboxdb ls` shows all databases; `pgboxdb agent ls` only agent branches with host,
session, state (`active`, `retired`, `expired`) and expiry.

## The environment a session gets

```
DATABASE_URL=postgres://postgres:…@127.0.0.1:<port>/agent-claude-9b1f3c2d
PGHOST=127.0.0.1  PGPORT=<port>  PGUSER=postgres  PGPASSWORD=…  PGDATABASE=agent-claude-9b1f3c2d
PGBOXDB_BRANCH=agent-claude-9b1f3c2d  PGBOXDB_SOURCE=myapp_template  PGBOXDB_ISOLATION=ok
```

Both the URL and the libpq variables are set, so frameworks that read either
work. `PGBOXDB_ISOLATION` is `ok`, `failed` (sentinel in place, see
troubleshooting.md), or unset (no branch for this shell: use
`pgboxdb agent exec --`).

## Framework notes

- **Rails**: `DATABASE_URL` overrides `config/database.yml` when set, for
  every environment, so `bin/rails test` runs against the branch too. A branch
  of a template imported from a development database carries
  `environment = development` in `ar_internal_metadata`; Rails' spawned
  `db:test:prepare` then aborts with `ActiveRecord::EnvironmentMismatchError`
  after the tests have already passed. That abort is harmless (the parent run
  exits 0) and it keeps Rails from purging the branch; do not run
  `db:environment:set` to silence it. Parallel
  tests create `agent-…-0`, `agent-…-1` databases in PGBoxDB; they are ordinary
  databases without a TTL, so run `PARALLEL_WORKERS=1` or drop them with
  `pgboxdb drop` afterwards, or accept them (they are small). `db:prepare`,
  `db:schema:load`, `db:reset` act on the branch only.
- **Django**: needs settings that read `DATABASE_URL` (dj-database-url,
  django-environ) or the `PG*` variables through psycopg; `manage.py test`
  creates `test_<branch>` as a sibling database, again without a TTL, and
  drops it itself.
- **Prisma**: reads `env("DATABASE_URL")` from `schema.prisma`; shadow
  databases for `migrate dev` are created in PGBoxDB and removed by Prisma.
- **psycopg / asyncpg / SQLAlchemy / Go pgboxdb / node-postgres**: all honour
  `PG*` and most accept `DATABASE_URL` directly.
- **Seeds and fixtures**: run them; a branch of a schema-only template is
  empty, which is usually what tests want. For a branch of a data template
  remember that fixture loading can truncate real tables; that is still
  only the branch.

## Cleanup

Nothing to do in the normal case. The runtime's supervisor removes expired
branches (idle TTL passed, or grace passed after the session ended) and
terminates their sessions first. `pgboxdb agent down --now` drops the current
session's branch immediately; `pgboxdb ttl <branch> 4h` extends by hand;
`pgboxdb ttl <branch> unlimited` keeps one forever (then it is the user's).
