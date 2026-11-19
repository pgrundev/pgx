# PGBoxDB branding audit (before v0.2.0)

Date: 2026-11-19. The official brand is **PGBoxDB**: CLI and npm `pgboxdb`,
env `PGBOXDB_*`, repository `pgrundev/pgboxdb`, site `pgboxdb.com`, docs
`pgbox.dev/db`.

Searched: `release/v0.2.0` (every tracked file, case-insensitive `pgx`,
`pgxdb`, `pgx.dev` and `pgbox` not followed by `db`, plus wrong-cased brand
variants); the CLI's `--version` and `--help` output; the 0.2.0 release
artifacts; GitHub metadata of both repositories, their releases and the org;
the npm registry; the live `pgboxdb.com` and `pgx.dev`.

## Fixed in this audit

| Where | Was | Now |
|---|---|---|
| GitHub `pgrundev/pgboxdb` description | "PGBox — Lightweight Postgres…" | "PGBoxDB — Lightweight Postgres…" |
| GitHub `pgrundev/pgboxdb-plugin` | no description or homepage | PGBoxDB description, `https://pgboxdb.com`, topics |
| Docs, Skill, plugin README, npm README, `install.sh` example | version floors 0.1.x (only ever shipped as PGX); "`pgboxdb` and `pgboxdb`" (a leftover of "`pgx` and `pgxdb`"); 0.1.0 samples | 0.2.0; one command; 0.2.0 samples (`4cef79193a`) |

## Clean (no change needed)

* **CLI**: `--version` and every `--help` print PGBoxDB/`pgboxdb` only; the
  upper-case hits are `PGBOXDB_*` variables.
* **Release artifacts (0.2.0 rehearsal, run 44603543571)**: the asset names are
  `pgboxdb-0.2.0-<target>.tar.gz`. Each holds `bin/pgboxdb` and
  `share/pgboxdb/dist.json` (`pgboxdb_version`), with no `pgx` paths. The
  release title will be "PGBoxDB v0.2.0" (`release.yml`).
* **npm `pgboxdb`**: name, bin, description, repository, homepage, bugs and
  README are all PGBoxDB. Not yet published (the registry answers 404).
* **Installer, Worker `pgboxdb.com/install`, Docker** (user `pgboxdb`,
  `/opt/pgboxdb`, `PGBOXDB_*`), **workflows, Skill, plugin package**: checked by
  `dist/test/dist_consistency_test.sh` in CI.
* **Repository site source** `dist/pgboxdb.com/site/`: titled "PGBoxDB — …".
* **GitHub org** `pgrundev` ("PGRun"): no product name.

## Remaining references, and why they stay

### 1. History (preserved by decision)

* GitHub releases `v0.1.0-beta.1`, "PGX v0.1.0" … "PGX v0.1.3", and the
  "Earlier releases (as PGX)" section of `dist/release-notes.md`.
* Git history and tags, including the PGRun production engine tag
  `pgx-v0.1-beta.1`, which is immutable and referenced by PGRun.
* Dated records: `docs/**/validation/**`, `docs/superpowers/**`,
  `benchmarks/**/results/**`, `reports/**`.

### 2. Compatibility during the approved migration

* Detection of an installation from before the rename, which is never an
  alias: `crates/bin/pgboxdb/src/legacy.rs`, `pgboxdb migrate`
  (`~/.pgx`, `.pgx.toml`, `PGX_*`), `share/pgx/dist.json` in `install.sh`
  and the npm wrapper, `/var/run/pgx` in the Docker entrypoint, replacement
  of old `pgx agent hook` / `pgx git` hook blocks, and their tests.
  `docs/pgboxdb/local-dev/upgrading.md` explains these.
* GitHub's automatic redirects from `pgrundev/pgx` and `pgrundev/pgx-plugin`
  (option A).
* `pgx.dev`: a 301 to `pgboxdb.com`, while `/install` keeps serving the PGX
  0.1.3 installer for existing instructions. Both answer 410 from
  2027-02-17 19:58:29 UTC (`dist/legacy-domain-redirect/`).

### 3. Technical identifiers, not branding

* The Go driver `jackc/pgx` (docs, test app, Skill detection).
* PostgreSQL's SP-GiST box opclass `spgbox` (engine, fuzzers), and the engine's
  Rust type `PgBox` (a memory-context `Box`).
* PGRun production names: `pgx-agent`, Go package `pgxdb`, `/opt/pgx`,
  `script/pgx/`, `PGX_ENGINE_SHA`, `PGX_BINARY_SHA256` (out of scope by rule).
* Lab infrastructure: the ZFS dataset `tank/pgx-bench`, `PGXBENCH_WORKDIR`.
* The Cloudflare Worker names `pgx-install` and `pgx-dev-redirect` belong to the
  `pgx.dev` zone and are not shown to users.
* npm `pgxdb` (PGX, latest 0.1.3): it stays published and unchanged pending
  legal review, and no new version is published under it.

## Requires manual action

| # | Item | Why I could not or should not do it | Severity |
|---|---|---|---|
| M1 | **`pgbox.dev` is not registered** (the `.dev` registry RDAP answers 404). The README links the docs to `pgbox.dev/db`, and the live `pgboxdb.com/llms.txt` tells agents to run `curl -fsSL https://pgbox.dev/install \| sh`. Whoever registers the domain controls that script. | Registering a domain is an account action | **Blocker**: register `pgbox.dev` now, or remove every `pgbox.dev` reference |
| M2 | **The live `pgboxdb.com` is branded "PGBOX"** (22× on the page, all of `llms.txt`), and `llms.txt` gives wrong commands: `pgbox run`, `--skill pgbox`, `/plugin install pgbox@pgbox-plugin`, `PGBOX_PASSWORD`, `docker build -t pgbox`. `pgx.dev` now redirects there. | Its source is not in this repository, and I couldn't find the Worker that serves it. Replacing a live site needs your approval | **Blocker** for the announcement: fix its source, or deploy `dist/pgboxdb.com/site` (PGBoxDB-branded) instead |
| M3 | `pgrundev/pgboxdb-plugin` still contains the PGX plugin (README "PGX — Postgres for Agents", `pgx@pgx-plugin`). | Syncing before `pgboxdb` is on npm would break new installs. It needs a plugin-sync secret (runbook step 9) | Right after the npm publish |
| M4 | The npm name `pgbox` belongs to a third party (created 2026-03-20, "PostgreSQL emulator"). Using "PGBox" anywhere invites confusion with it. | A legal and brand matter | Note for legal |
| M5 | PGRun PR pgrundev/pgrun#49 (GitHub and site links → PGBoxDB) | Merging and deploying PGRun needs your approval | Before 2027-02-17 |
| M6 | The old site is still deployed as the Cloudflare Worker `pgx`. It is unreachable while the `pgx.dev/*` route exists. | Removing its domain or route is a separate approval | After the transition |
| M7 | The repository's social-preview image can't be read through the API. | Check it in Settings → Social preview | Check |
| M8 | npm `pgxdb`: optionally `npm deprecate pgxdb "renamed to pgboxdb"` after 0.2.0 | Needs your approval (and legal) | Optional |
