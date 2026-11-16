# Skill tests

The Skill is the product, so it is tested as one. Nothing here needs a paid
agent call.

- `run.sh`: the official Agent Skills validator (`skills-ref` via pipx/uvx),
  the frontmatter contract (six fields, `name: pgboxdb`, description length,
  under 500 lines), every reference file present and linked, no credentials
  or machine paths in the text, and `scripts/detect-project.sh` against the
  fixture projects (framework, migrations, classification, no secret leaks).
- `e2e.sh`: scenarios A fresh, B existing PostgreSQL (a scratch PG 18 is
  started and must stay unchanged), C PGBoxDB-only, D six concurrent agents,
  E lifecycle (reuse, grace, revive, expiry, restart), F security (no
  credential output, URL in `.pgboxdb.toml` rejected, foreign database never
  dropped, hostile session id, fail closed), G installation UX (merge and
  uninstall keep foreign hooks; the `skills` CLI installs, lists and removes
  the skill for claude-code, cursor and codex). Needs `PGBOXDB_DIST` (an assembled
  distribution), the `pgboxdb` binary under test and `PG_BINDIR`; `SKIP_NPX=1`
  for an offline run.
- `fixtures/`: small projects for `detect-project.sh`. Credentials in them
  are fake and must never appear in the script's output.

Live host validation (Claude Code, Cursor, Codex) is recorded under
`docs/pgboxdb/local-dev/validation/`.
