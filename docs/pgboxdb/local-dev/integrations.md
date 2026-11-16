# Using PGBoxDB from your application

`pgboxdb url <db>` prints a standard `postgres://` URL. Point `DATABASE_URL` at it;
any PostgreSQL client or ORM works unchanged. Runnable fixtures for each stack
are in `tests/apps/` (all verified against the assembled distribution).

```sh
pgboxdb create myapp
export DATABASE_URL="$(pgboxdb url myapp)"
```

## Rails / ActiveRecord

`config/database.yml` reads `DATABASE_URL`, or set it in the environment. PGBoxDB
supports migrations up/down, `db:schema:load`, seeds, transactions with
savepoints, `RecordNotUnique` / `InvalidForeignKey` from the database, JSONB and
parallel tests. Example: `tests/apps/rails/app.rb`.

## Node.js (`pg`)

```js
const { Client } = require('pg');
const c = new Client({ connectionString: process.env.DATABASE_URL });
await c.connect();
```
Parameterized and named/prepared statements, transactions, JSONB, `COPY`.
Example: `tests/apps/node/app.js`.

## Python (`psycopg` 3)

```python
import psycopg
with psycopg.connect(os.environ["DATABASE_URL"]) as conn: ...
```
`executemany`, `COPY`, server-side cursors, JSONB. Example:
`tests/apps/python/app.py`.

## Go (`jackc/pgx` v5)

```go
conn, _ := pgx.Connect(ctx, os.Getenv("DATABASE_URL"))
```
`CopyFrom`, batches, transactions, JSONB. Example: `tests/apps/go/main.go`.

## psql

```sh
psql "$(pgboxdb url myapp)"
psql "$(pgboxdb url myapp --socket)"   # Unix socket, no password
```

## Agent workflow

With the PGBoxDB Skill installed (`npx skills add pgrundev/pgboxdb --skill pgboxdb`) and
`pgboxdb agent setup` run once in the project, each Claude Code session gets its
own branch and `DATABASE_URL` automatically; Cursor and Codex are covered by
hooks plus `pgboxdb agent exec`. See [agents.md](agents.md).

By hand, any command can run in a branch of its own:

```sh
pgboxdb agent exec -- bin/rails test          # its own branch, dropped when the command exits
pgboxdb agent exec --keep -- python manage.py migrate   # kept for 10 minutes afterwards
```

or, the older pattern, one disposable database per task:

```sh
db="task_$(date +%s)"
pgboxdb branch app "$db" --ttl 2h
DATABASE_URL="$(pgboxdb --json url "$db" | jq -r .url)"  # run migrations / tests
pgboxdb drop "$db" --force
```
