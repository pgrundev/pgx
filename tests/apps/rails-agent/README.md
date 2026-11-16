# rails-agent: a minimal Rails 8 + PostgreSQL app for agent-isolation tests

Used by the live validation in `docs/pgboxdb/local-dev/validation/` and by hand:

```sh
pgboxdb agent exec -- bin/rails db:prepare     # schema into this session's branch
pgboxdb agent exec -- bin/rails test           # the second test asserts the branch name
```

`config/database.yml` is the generator's default: Rails reads `DATABASE_URL`
from the environment when it is set, which is exactly what the PGBoxDB hooks and
`pgboxdb agent exec` provide. The `PGBOXDB_FIXTURE_ANY_DB=1` variable relaxes the
branch-name assertion for runs against a plain database.
