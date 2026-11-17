# Discovering a project's database setup

Run `scripts/detect-project.sh <dir>` first. It reads only well-known files
and prints one JSON object with no secrets. Then confirm by reading the files
it names, still without echoing credentials.

## What it reports

- `framework`: `rails`, `django`, `prisma`, `node-pg`, `sqlalchemy`, `go`,
  `unknown`; `migrations`: `rails`, `django`, `prisma`, `alembic`,
  `golang-migrate`, `unknown`.
- `database_config.files`: which of `config/database.yml`, `.env`,
  `.env.local`, `.env.development`, `.env.test`, `prisma/schema.prisma`,
  `docker-compose.yml`/`compose.yml`, `settings.py` exist.
- `database_config.url_sources`: where a connection was found
  (`env:DATABASE_URL`, `file:.env`, `file:config/database.yml`, `PGHOST`).
- `database_config.host`, `port`, `db`: host and database name only. The
  user, password and query string are never printed.
- `classification`:
  - `local`: localhost, 127.0.0.1, ::1, a Unix socket, `host.docker.internal`,
    or a `postgres` service in a compose file.
  - `pgboxdb`: local and on the running PGBoxDB runtime's port.
  - `none`: no database configuration found.
  - `remote`: any other host.
  - `sensitive`: the host looks like production or a managed cloud database
    (`prod`, `production`, RDS, Neon, Supabase, Render, Railway, Heroku,
    Azure, Google, Fly, Timescale, Aiven, Crunchy, `cloud`).
- `pgboxdb.installed`, `pgboxdb.version`, `pgboxdb.toml` (a `.pgboxdb.toml` up the tree),
  `pgboxdb.hooks.{claude,cursor,codex}` (our hook entries present in the
  project's files).

## Decide, then say it in one line

| Classification | Decision |
|---|---|
| `local` | Import schema-only into a template; keep the developer's server as is. |
| `pgboxdb` | Reuse the existing PGBoxDB template. |
| `none` | New project: empty `app` template, framework migrations inside branches. |
| `remote` | Ask before copying anything from another machine; offer schema-only. |
| `sensitive` | Ask. Say plainly that it looks like production. Never import data; schema-only only with explicit approval; prefer a staging/dev source if the user has one. |

Ambiguous cases (several `.env` files with different hosts, a `DATABASE_URL`
in the environment that differs from the files): list the candidates by host
and database name and ask which one is the development database.

## What else to look at

- Extensions: `CREATE EXTENSION` in migrations or `schema.sql`. PGBoxDB bundles
  the common ones; `pgboxdb copy` refuses an unsupported extension up front and
  names it. See postgres-import.md.
- A separate test database (`myapp_test`, `DATABASE_URL_TEST`): irrelevant
  inside PGBoxDB; the branch is the test database. Mention it so the user is not
  surprised that tests ran against the branch.
- Framework overrides: Rails `config/database.yml` reads `DATABASE_URL`
  automatically when set; Django needs `dj-database-url`/`environ` or a
  settings file that reads `DATABASE_URL`; Prisma reads `env("DATABASE_URL")`
  from its schema. If a settings file hard-codes host and database, the
  `PG*` variables usually still win for libpq-based drivers (psycopg, `pg`,
  Go `pgboxdb`), but not for Prisma or JDBC-style URLs. Say what applies.
