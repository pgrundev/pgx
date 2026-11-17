# PGRun 2.0 ⇄ PGBoxDB: required changes

Scope: what PGRun must change, later and in its own repository, because this
repository renamed PGX to PGBoxDB. PGRun itself was **not modified**. The
inventory below was taken read-only from `pgrun/`, `pgrun-cli/` and
`ci-action/` on 2026-11-16. Paths are relative to `pgrun/` unless shown
otherwise.

## Summary

PGRun production does **not** consume anything this rename changed.

* PGRun runs only the engine binary `postgres`. It is built from this
  repository's `prod` branch on a build host
  (`pgxbench:~/pgrust/target/release/postgres`), copied with scp, checked by
  SHA-256, and installed under `/opt/pgx/releases/<sha>/bin/postgres`.
* PGRun reaches the engine through `PGRUST_*` environment variables
  (`PGRUST_PGSHAREDIR`, `PGRUST_TZDIR`), `pgrust.*` GUCs, and the SQL
  functions `pgrust_runtime_status()` and `pgrust_seal_template()`. None of
  these contain "pgx", and the rename did not touch them. Verified: the whole
  engine-side diff is 12 lines in 7 files, all of them comments except one
  libxml2 error message ("PGX binds libxml2…" → "PGBoxDB binds libxml2…").
* PGRun never uses the public `pgx` CLI, the `pgxdb` npm package, `dist.json`
  or `share/pgx`, the `https://pgx.dev/install` installer, or the GitHub
  release assets in production.
* `/opt/pgx`, `/etc/pgx`, `/var/lib/pgx`, `/run/pgx`, the `pgx` and
  `pgx-agent` users and units, `PGX_ENGINE_SHA`, `PGX_AGENT_TOKEN`,
  `PGRUN_PGX_*`, the roles `pgx_restore` and `pgx_template_owner`, the
  `branch_hosts.pgx_*` columns and the API engine value `"pgx"` are names
  **PGRun chose for itself**. This repository does not ship them. They keep
  working unchanged, and renaming them is a separate PGRun decision.
* The rename is on `main`. `prod`, the branch PGRun deploys from, is
  untouched. Engine commit `913630e11d` (tag `pgx-v0.1-beta.1`) stays what it
  is.

**Nothing in PGRun production breaks when this branch merges.** What breaks is
dev-only and marketing (sections B and C below).

## A. Production integration points: no change required

| Integration | PGRun file | Depends on | Status after the rename |
|---|---|---|---|
| Engine binary invocation | `script/pgx/pgx-runtime.service:18` (`ExecStart=/opt/pgx/current/bin/postgres -D /var/lib/pgx/runtime/data …`) | binary name `postgres` | unchanged: the engine binary is still `postgres` |
| Runtime startup / shutdown | `script/pgx/pgx-runtime.service` (SIGINT fast shutdown, cgroup limits), `script/pgx/host_install.sh` | `postgres`, `PGRUST_PGSHAREDIR`, `PGRUST_TZDIR` | unchanged |
| Data directories | `/var/lib/pgx/runtime/data`, `/var/lib/pgx/templates`, ZFS `$ZPOOL/pgx{,/runtime,/templates}` | PGRun's own layout | unchanged (not shipped by this repo) |
| Configuration | `script/pgx/pgx-runtime.conf` (`port = 6499`, `unix_socket_directories = '/run/pgx'`, `pgrust.*` memory limits, `pgrust.ephemeral_db_prefix`), `script/pgx/pgx-ephemeral.conf` (checksummed by `bootstrap_host.sh:58`) | `pgrust.*` GUCs | unchanged. This repo's `configs/pgx-ephemeral.conf` was renamed to `configs/pgboxdb-ephemeral.conf`, but PGRun keeps a frozen copy and checks its own SHA-256, so it is not affected |
| Environment variables | `PGRUST_*` in the unit; PGRun's own `PGX_ENGINE_SHA`, `PGX_BINARY_SHA256`, `PGX_AGENT_*`, `PGRUN_PGX_*` | engine reads `PGRUST_*` only | unchanged. The CLI's `PGX_*` → `PGBOXDB_*` rename does not apply to the engine |
| Version detection | `script/pgx/engine.pin` (`PGX_ENGINE_SHA=913630e11d`), `app/services/branching/pgx.rb:13` (`PGX_VERSION = "0.1"`, constant) | operator-set SHA | unchanged; no `pgx --version` call, no `dist.json` read |
| Release downloads | none (built from source on the build host) | — | unchanged |
| Health checks | `pgx-agent/internal/pgxdb/runtime.go:113` (`SELECT pgrust_runtime_status()::text`), `config/recurring.yml:85` (`pgx_health_sweep`), `host_install.sh` (`psql -h /run/pgx -p 6499`) | `pgrust_runtime_status()` | unchanged |
| Project runtime isolation | `pgx-agent` `/databases`, `/postgres/branches`, cgroup `pgx-runtime.service` | engine SQL + PGRun agent | unchanged |
| Golden Copy / template operations | `pgx-agent/internal/pgxdb/template.go:23-26,248`, `citemplate.go:162,175`, `bootstrap_host.sh:96-101` (`pgrust_seal_template`) | `pgrust_seal_template()` | unchanged |
| Crash recovery | `script/pgx/{failure_tests,agent_drills,smoke12,own2}.sh`, `script/branching/host_memory_safety.sh:22` | `postgres` under systemd | unchanged |

**Invariant to preserve (for this repository):** do not rename the engine
binary `postgres`, the `PGRUST_*` environment variables, the `pgrust.*` GUCs
or the `pgrust_*` SQL functions without a coordinated PGRun change. The
rebrand followed this rule.

## B. Dev-only integration points: must change before developers upgrade

These break **silently** once a developer machine has `pgboxdb` instead of
`pgx`.

| # | File | Today | Required change | Why |
|---|---|---|---|---|
| B1 | `.claude/settings.json` (6 entries, lines 8–64) | `command -v pgx … && exec pgx agent hook; exit 0` | Run `pgboxdb agent install claude --scope project` in the PGRun checkout and commit the result. It replaces the six entries in place with `… pgboxdb agent hook …` and adds no duplicates (verified by `dist/test/migrate_test.sh`). | Without `pgx` on PATH the hook exits 0 and agent sessions silently lose branch isolation. |
| B2 | `.pgx.toml` (`source = "pgrun_template"`) | read by `pgx agent` | Run `pgboxdb migrate --apply` in the PGRun checkout (copies it to `.pgboxdb.toml` with comments updated), commit `.pgboxdb.toml`, and remove `.pgx.toml` once every developer has upgraded. | `pgboxdb` refuses a project that has only `.pgx.toml` (fail-closed), and it does not read the old file as an alias. |
| B3 | Developer machines with a `~/.pgx` home | `pgx` runtime | `pgboxdb migrate` (dry run), stop the old runtime, `pgboxdb migrate --apply`. Rollback: `pgboxdb migrate --rollback --apply`. | `pgboxdb` refuses to create a second, empty home beside an existing one. |
| B4 | `.pgx.toml` comment link to `docs/pgx/local-dev/agents.md` | stale | Point at `https://pgbox.dev/db` (or the repository's `docs/pgboxdb/local-dev/agents.md`). | Documentation path moved. |

## C. Marketing / UI: change when the new domain and repository go live

| # | File | Today | Required change |
|---|---|---|---|
| C1 | `app/helpers/application_helper.rb:166` | `github_url = "https://github.com/pgrundev/pgx"` | `https://github.com/pgrundev/pgboxdb` (after the repository rename) |
| C2 | `test/integration/marketing_nav_test.rb:102`, `test/integration/marketing_test.rb:127-133` | assert the URL above | update together with C1 |
| C3 | `app/views/shared/_site_nav.html.erb:42` | `"https://pgx.dev"`, label `"pgx.dev"` | `https://pgboxdb.com`, label `pgboxdb.com` |
| C4 | `test/integration/marketing_nav_test.rb:97` | asserts C3 | update together with C3 |
| C5 | `app/views/pages/home.html.erb:537` | `<a href="https://pgx.dev">pgx</a>` | `https://pgboxdb.com`, `PGBoxDB` |
| C6 | `app/views/pages/home.html.erb:31,522,555-561` | comments, hand-entered npm download count for `pgxdb`, hidden testimonial column | update the copy; count `pgboxdb` downloads once the package is published |
| C7 | `docs/pgx/*.md`, `docs/superpowers/**`, `LLM.md:53,501-511`, `test/pgx/compatibility/*.md`, `pgrun-cli/README.md`, `pgrun-cli/skills/pgrun-branching/SKILL.md`, `ci-action/README.md`, `ci-action/docs/*.md` | "PGX Beta" prose, links to pgx.dev and pgrundev/pgx | rebrand prose and links; keep PGRun's own identifiers (the next section) |

Not PGX branding, so leave unchanged: `app/views/pages/agents.text.erb:83`,
`ci-action/docs/database.md:160` and `insert_3d.html:2726` use "pgx" for the
Go driver or as a psql-style prompt. `pgx-agent/go.mod:5` is
`github.com/jackc/pgx/v5`.

## D. PGRun-owned names: optional, separate decision

PGRun can keep these indefinitely, because nothing in this repository depends
on them. Renaming them is a PGRun product decision with its own migration:

| Name | Where | Cost of renaming |
|---|---|---|
| API engine value `"pgx"`, label `"PGX Beta"`, reason codes `host_pgx_disabled`, `template_requires_pgx` | `app/models/branch.rb:96-130`, `app/helpers/branch_engine_helper.rb`, `pgrun-cli/internal/{api,cli,mcpserver}`, `ci-action/action.yml`, `ci-action/database/action.yml` (`default: pgx`), `lib/pgrun-ci` | **Breaking public API change** for pgrun-cli and ci-action users; needs a dual-accept period (`"pgx"` and `"pgboxdb"`) and a legal view on keeping `"pgx"` as an accepted value |
| DB columns `branch_hosts.pgx_*`, `branches.pgx_database`, `pgx_template*`, tables `pgx_host_samples` and `pgx_template_builds`, check constraints on `'pgx'` | `db/schema.rb` | Rails migrations plus a backfill |
| Host layout `/opt/pgx`, `/etc/pgx`, `/var/lib/pgx`, `/run/pgx`, users `pgx` and `pgx-agent`, units `pgx-runtime.service` and `pgx-agent.service`, ZFS `tank/pgx`, port 6499 | `script/pgx/*`, `pgx-agent/deploy/*`, `pgx-agent/cmd/pgx-agent/main.go:35-40` | host migration with downtime per host; ZFS rename; systemd unit rename |
| Env `PGRUN_PGX_ENABLED`, `PGX_AGENT_TOKEN`, `PGX_AGENT_CERT_SHA256`, `PGRUN_PGX_ALLOWLIST`, `PGRUN_PGX_TEMPLATE_WAIT_MS`, `PGX_ENGINE_SHA` | `config/deploy.yml`, `.kamal/secrets`, `app/services/branching/pgx.rb` | coordinated deploy; dual-read period |
| Roles `pgx_restore`, `pgx_template_owner`, `pgx_seed_<hex>` databases | `pgx-agent/internal/pgxdb/*` | role rename on every host |
| Rake tasks `pgx:*`, `PGX_COMPAT_URL`, `PGX_VALIDATION_OUT` | `lib/tasks/pgx*.rake` | low |
| Go package path `pgx-agent/internal/pgxdb` | `pgx-agent/` | low (internal) |

## E. Sequencing that keeps PGRun working

1. Merge this branch to `main` of `pgrundev/pgx`. PGRun is unaffected
   because it builds from `prod`.
2. Rename the GitHub repository to `pgrundev/pgboxdb`. GitHub redirects
   `git@github.com:pgrundev/pgx.git`, which keeps `script/pgx/engine.pin`
   resolving. Update its comment to the new URL at the next pin bump.
3. Before developers install `pgboxdb`: apply B1 and B2 in the PGRun
   repository (one PR).
4. When `pgboxdb.com` serves: apply C1–C7 (one PR, with the tests).
5. Optional, later: section D, each item behind its own migration plan.

## F. Security note found during the inventory

`pgrun/.kamal/secrets:114` contains a literal `PGX_AGENT_TOKEN` value
committed to the repository (not repeated here). This is unrelated to the
rename, but should be rotated and moved to a secret store.
