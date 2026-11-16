# Getting started: the three scenarios

Every scenario ends the same way: a committed `.pgboxdb.toml`, hooks installed
for the agents on the machine, and each new session working in its own
branch. What differs is where the template comes from.

## A. The project already uses a local PostgreSQL

Typical signals: `config/database.yml` with `host: localhost`, a `.env` with
`DATABASE_URL=postgres://…@localhost/…`, a `compose.yml` with a `postgres`
image. Classification `local`.

```sh
pgboxdb version || <install, see installation.md>
pgboxdb copy --from "$DEV_URL" --to myapp_template --schema-only   # $DEV_URL from the user's env or .env; never typed into chat
pgboxdb agent setup --source myapp_template
pgboxdb agent doctor
pgboxdb agent exec -- bin/rails db:migrate          # in this session; new sessions are automatic
```

The PostgreSQL server is only read, once. Developers keep using it; agents
use PGBoxDB branches of the template. Say in one line that data was not copied
(schema only) and that a data copy needs their approval.

## B. The project already uses PGBoxDB

`pgboxdb ls` shows a template (often `app` or `<name>_template`), `.pgboxdb.toml`
may exist. Classification `pgboxdb`.

```sh
pgboxdb agent setup            # reads .pgboxdb.toml; installs hooks if missing; proves a branch
pgboxdb agent doctor
```

Do not run `pgboxdb copy` again. If `.pgboxdb.toml` names a source that no longer
exists, ask which database to use; never guess by picking the largest.

## C. A new project with no database

Classification `none`.

```sh
pgboxdb agent setup            # creates an empty `app`, writes .pgboxdb.toml, installs hooks
pgboxdb agent exec -- bin/rails db:prepare     # or the framework's schema setup, inside the branch
```

The template stays empty; every session branches it and builds the schema
with the framework's own migrations. When the schema is stable and the user
wants faster branches, migrate the *template* once, deliberately:
`DATABASE_URL="$(pgboxdb url app)" bin/rails db:migrate` while nothing else is
connected to `app`, then say that you did.

## What the user sees afterwards

```
$ claude
PGBoxDB: this session's database is branch agent-claude-9b1f3c2d (a copy of myapp_template).
DATABASE_URL and PG* are already set in your shell; do not point them elsewhere.
The branch is disposable and expires 2 hours after your last activity.
```

A second session gets `agent-claude-4e77a0c1`. Neither can see the other's
rows. `pgboxdb agent ls` shows both; both vanish on their own.
