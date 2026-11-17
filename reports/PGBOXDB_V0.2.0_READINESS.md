# PGBoxDB v0.2.0: release readiness

Date 2026-11-16. Local work only: nothing was pushed, tagged, published,
renamed or deployed.

## Verdict

**Ready for review, not ready to tag.** The code, packaging and release
automation are prepared and pass every check that can run on this machine.
Releasing still needs: a legal decision, CI on Linux and Docker (never run
here), the repository rename, and the account-side steps (npm publisher,
Worker deploy, DNS). Each is an approval-gated action in
`PGBOXDB_RELEASE_CHECKLIST.md`.

## What v0.2.0 is

The first release under the PGBoxDB name, cut from `main` after the rebrand
merges. Compared with v0.1.3 (the last PGX release):

| Change | Where |
|---|---|
| Rename PGX → PGBoxDB: CLI `pgboxdb`, npm `pgboxdb`, `PGBOXDB_*`, `~/.pgboxdb`, `.pgboxdb.toml`, assets `pgboxdb-<v>-<os>-<arch>.tar.gz`, Docker, Skill, plugin, docs, site | `rebrand/pgboxdb` (`039453f6ed`, `96381d1a50`) |
| Existing installations never forked or lost: refusal to fork, `pgboxdb migrate` (dry run, `--apply`, `--rollback`), hooks replaced in place | `f0fd7d81b1`, `66e5b67e37` (trailing comments) |
| `pgboxdb top`, a live read-only monitor (PR #49, merged after v0.1.3) | already on `main` (`56177328d7`) |
| Release workflow: draft keeps its assets, one checked version, replay test | `3891435c18` |
| Installer Worker pinned to `v0.2.0`; one-name consistency test | `130d8a56ba` |
| Crash test harness: in-flight commit accepted correctly | `a24f804b33` |
| Version 0.2.0 everywhere, release notes, Skill floor 0.2.0 | **`release/v0.2.0` (`5fbbfd079f`)**, a separate branch on top of `130d8a56ba` |

The engine is unchanged apart from comments and one libxml2 error message
(the rename audit, §2).

## Why 0.2.0

* No `pgboxdb-0.1.x` asset exists, so a 0.1.x `pgboxdb` package would have
  nothing to download.
* A migrated home holds the previous dists under `dist/0.1.x`. The
  installers refuse to overwrite them, and a new version avoids the
  collision entirely.
* The release branch makes every channel agree. `dist/release/version.sh`
  refuses a tag that doesn't match the crate and npm versions, and the
  consistency test requires the Worker pin to equal `v<version>`.

## Gates

| Gate | Status | Evidence |
|---|---|---|
| Build, fmt, clippy (rustc 1.96) | ✅ local | test report §7 |
| Unit and integration suites, crash, migrate e2e | ✅ local (macOS arm64) | test report §2, §7 |
| PostgreSQL regression suite | ✅ 219/231 byte-exact, 0 result diffs (same as before) | test report §2 |
| Release-equivalent static macOS build, self-contained, packaged smoke | ✅ local, at `96381d1a50` (no engine change since) | test report §2 |
| npm package (`pgboxdb`, bin `pgboxdb`, 4 files), installer, Worker | ✅ local tests | test report §7 |
| One name in every channel | ✅ `dist/test/dist_consistency_test.sh` (also in CI) | §7 below |
| Release workflow produces a complete draft | ✅ replayed locally; ⏳ never run on GitHub | `dist/test/release_workflow_test.rb` |
| Linux amd64/arm64 | ⏳ CI only | CI `cli` matrix |
| Docker image build and run | ⏳ CI only (local daemon down, VM disk full) | CI `docker` job |
| Legal review of the remaining uses of the previous name | ⏳ decision | compatibility matrix, "Legal review" 1–6 |
| PGRun impact | ✅ production unaffected; dev change prepared | `PGBOXDB_PGRUN_MIGRATION.md` §B |
| Security: `PGX_AGENT_TOKEN` | ✅ not exposed (not in git, ignored file, mode 0600); rotation optional, procedure written | `PGBOXDB_PGRUN_MIGRATION.md` §F |

## Channel consistency (task 7)

Checked by `dist/test/dist_consistency_test.sh`, then by behaviour:

| Channel | Name | Behavioural check |
|---|---|---|
| npm | package `pgboxdb`, single bin `pgboxdb`, downloads `github.com/pgrundev/pgboxdb/releases/download/v<v>/pgboxdb-<v>-<target>.tar.gz` + `.sha256`, installs `~/.pgboxdb/dist/<v>`, reads `PGBOXDB_VERSION/INSTALL_DIR/BASE_URL` | `npm test` 22/22, `npm_test.sh` against a real assembled 0.2.0 tarball |
| Release artifacts | `pgboxdb-<v>-{darwin-arm64,linux-amd64,linux-arm64}.tar.gz` + `.sha256` + `checksums.txt`; top directory `pgboxdb-<v>-<target>/`, `bin/pgboxdb`, `bin/postgres`, `share/pgboxdb/dist.json` (`pgboxdb_version`) | `assemble_test.sh`, replay test, tarball listing, `shasum -c` |
| Installer (`install.sh`) | same URL and asset, `~/.pgboxdb/dist/<v>`, links `~/.local/bin/pgboxdb`, `PGBOXDB_*` only, refuses to replace a pre-rename dist | `install_test.sh`, `install_discovery_test.sh` |
| Worker | `pgboxdb-install`, route `pgboxdb.com/install`, serves `raw.githubusercontent.com/pgrundev/pgboxdb/<INSTALL_REF>/dist/install.sh`, `INSTALL_REF = v0.2.0` | `node --test` 7/7 |
| Docker | user `pgboxdb` (uid 10001), `/opt/pgboxdb`, volume `/var/lib/pgboxdb`, `PGBOXDB_HOME/RUN_DIR/LISTEN/PORT`, entrypoint `pgboxdb-entrypoint` | static only here; CI `docker` job |
| CI guards | `npm-publish.yml` and `publish-plugin.yml` run only in `pgrundev/pgboxdb` | consistency test |

The only previous-name strings left in these channels are the reviewed
detection lines: `share/pgx/dist.json` in the npm wrapper and installer,
and `/var/run/pgx` in the Docker entrypoint.

Fixed during this check:
* **Worker pin.** The Worker was pinned to `v0.1.0-beta.1`. After the
  repository rename, that tag's `install.sh` installs the PGX product, so
  `pgboxdb.com/install` would have served it. It is now `v0.2.0`, and it
  answers 502 until that tag exists (fail closed).
* **Skill version gates** (release branch). They required "PGBoxDB 0.1.2 /
  0.1.3 or newer", which are versions that only ever shipped as PGX. They
  now require 0.2.0.

## Risks and open points

1. **The release workflow has never run on GitHub since the fix.** The
   replay test emulates `actions/checkout`, `download-artifact` and
   `action-gh-release`. Run `actionlint` in CI or locally (it is not
   installed here). Watch the first tag run, and use the draft (never
   auto-published) as the safety net.
2. **Docker and Linux** were verified only by CI design, not locally.
3. **Release-notes history.** The older sections were renamed by the
   mechanical pass ("PGBoxDB v0.1.3 — …"), but they shipped as PGX. The
   options are: keep them, mark them "released as PGX", or trim them from
   the 0.2.0 body. This is a brand and legal choice.
4. **Install discovery before 0.2.0 exists.** After the rename, `install.sh`
   without a version picks the newest release, v0.1.3, whose assets are
   named `pgx-…`. It fails cleanly with "download failed", not with a
   partial install. Rename the repository and release 0.2.0 close
   together.
5. **Intel Macs** stay unsupported. Their `darwin-amd64` build is an actions
   artifact only. npm and the installer say so explicitly.
6. **Docker image version.** The image's `dist.json` always reports
   `0.1.0-docker`, because the Dockerfile has no `ARG PGBOXDB_VERSION`. This
   predates the rename. It is cosmetic unless images are published, and
   none are today.
7. **Rails app test and Ruby compatibility suite**: not run (gems not
   installed).

## Branches to review

* `rebrand/pgboxdb`: the rebrand plus this pass, then the reports.
* `release/v0.2.0`: one commit (`5fbbfd079f`) on top of `130d8a56ba`, the
  release PR, to open after the rebrand merges. It does not contain the
  report commit, and its diff touches no report file.
