# PGBoxDB two-branch release strategy

Two long-lived branches with different audiences and different release paths.

| Branch | Audience | Contents | Released as |
|---|---|---|---|
| `main` | public open-source developers | the engine plus the `pgboxdb` CLI and local-dev distribution | public GitHub release artifacts (`release.yml`, built from `main` only) |
| `prod` | PGRun's control plane | the exact engine PGRun deploys, frozen at a verified commit | internal builds staged to PGBoxDB hosts by PGRun's own scripts, pinned by SHA-256 — never the public artifacts |

## Verified commits (as of this proposal)

| Role | Commit | Evidence |
|---|---|---|
| **Production engine (prod baseline)** | `913630e11d` | tag `pgx-v0.1-beta.1`; `pgrun/docs/pgx/beta-v1-report.md` names it the Beta v1 engine commit, with `727f31e49f` kept under `/opt/pgx/releases` for rollback |
| Rollback engine | `727f31e49f` | same report; `pgrun/docs/pgx/host-runbook.md` (2026-10-09 install) |
| `pgboxdb/main` tip | `69a0a4c8d3` | README docs commit |
| `pgboxdb/pgboxdb-experimental-plan` | `69a0a4c8d3` | identical to `pgboxdb/main` |
| Proposed `main` after this work | `pgboxdb-local-dev` tip `b456c8ecc1` | the local-dev branch (CLI + tests + dist) |

### Branch relationship (linear, no divergence)

```
ad021e4c63 …
  └ 913630e11d  tag pgx-v0.1-beta.1   ← prod baseline (PGRun production engine)
      └ 80899f9c08  docs: post-freeze fixes (v0.1-readiness.md)   ┐ docs only —
      └ 69a0a4c8d3  docs: README                                  ┘ pgboxdb/main tip, engine == prod
          └ b2ac5558e8  fix: libxml2 soname probe (engine)        ← the only engine change above prod
              └ …20 local-dev commits (crates/bin/pgboxdb, tests, dist, docs)  ← pgboxdb-local-dev
```

`prod` is an **ancestor** of `main`; the two do not diverge. The two commits
`main` already has above `prod` are documentation only, so **`main`'s engine
binary is byte-identical to the deployed prod engine (`913630e11d`)**. The only
engine source change introduced above prod by the local-dev work is the
libxml2-soname fix (`b2ac5558e8`, 3 files); every other local-dev commit adds
the separate `crates/bin/pgboxdb` CLI crate and tests, which do not affect the
`postgres` engine binary.

## How `prod` is created and kept clean

- `prod` is created to point at exactly `913630e11d` (`git branch prod
  913630e11d`). It is **never** branched from `main` or `pgboxdb-local-dev`, so it
  cannot inherit unverified local-development changes (tasks 2, 3).
- `prod` moves only by an explicit, reviewed cherry-pick PR (below). There is no
  workflow, schedule or automation that merges or fast-forwards `main` into
  `prod` (task 8). `main → prod` synchronization does not exist; the only flow
  into `prod` is a human-selected backport.

## Release paths

### Public (`main`)

`release.yml` builds the per-platform artifacts (`linux-amd64`, `linux-arm64`,
`darwin-arm64`) with SHA-256 checksums and uploads a **draft** GitHub release.
A `guard` job refuses any tag that is not an ancestor of `origin/main`, so a tag
pushed from `prod` or a feature branch cannot produce a public release (task 5).
Publishing the draft stays a manual, approved step.

### PGRun production (`prod`)

PGRun builds the engine from a `prod` commit on its benchmark VPS and stages the
binary to each PGBoxDB host over ssh, pinned by `PGX_ENGINE_SHA` and
`PGX_BINARY_SHA256` (`pgrun/script/pgx/{bootstrap_remote,host_install}.sh`). It
does **not** download the public release artifacts. To pin the deployed commit,
PGRun's deploy must set `PGX_ENGINE_SHA` to a `prod` commit SHA and record it;
the current value is `913630e11d` (task 6). *(Changing PGRun's deploy
configuration is out of scope for this repository and awaits approval.)*

## Backporting a fix from `main` to `prod`

Fixes flow one way, selectively, never as a branch merge:

```sh
git checkout prod
git checkout -b backport/<short-name> prod
git cherry-pick <commit-on-main>         # one engine fix, e.g. the libxml2 soname fix
# resolve, build the engine, run the engine gates (below)
# open a PR into prod; after review and an engine rebuild + redeploy by PGRun, prod advances
```

Rules:
- Backport **engine** fixes only. The `pgboxdb` CLI, `dist/`, `tests/pgboxdb/` and the
  local-dev docs do not belong on `prod` (PGRun does not use them).
- One fix per PR, cherry-picked by SHA, with the originating `main` commit named
  in the PR. No `git merge main`.
- After `prod` advances, PGRun rebuilds the engine from the new `prod` SHA and
  redeploys deliberately (keeping the prior SHA under `/opt/pgx/releases` for
  rollback). This repository never deploys.

Candidate first backport: the libxml2-soname fix `b2ac5558e8` (XML on hosts with
libxml2 ≥ 2.14) — an additive, defensive engine fix.

## Branch protection and required checks (to apply in GitHub settings)

These are repository settings, applied by an admin after approval — not code in
this repo:

- **`main`**: require a PR (no direct pushes), require the `CI` workflow to
  pass (the `cli` matrix and `docker` jobs), require branches up to date, no
  force-push, no deletion.
- **`prod`**: require a PR, restrict who can push to the release owners, require
  an engine-only CI check (see below), no force-push, no deletion. Explicitly
  **do not** enable any auto-merge or branch-sync automation.
- Required status checks should be the engine-relevant jobs; the CLI/dist jobs
  are not meaningful on `prod` (it has no `crates/bin/pgboxdb`).

### CI on `prod`

`prod` (frozen at `913630e11d`) predates these workflows, so it has no
`.github/workflows`. Rather than fast-forward CI onto it (which would import
non-engine history), add an **engine-only** workflow to `prod` by a dedicated
reviewed PR: `cargo build --release --locked --bin postgres` plus the
PostgreSQL regression baseline (`benchmarks/pgboxdb/regress-baseline.py`) with the
PGBoxDB profile. This keeps `prod`'s required check about the engine PGRun ships,
and nothing else. `ci.yml` on `main` already runs on `push` to `main` and
`prod`; the engine-only check is the piece `prod` additionally needs.

## Artifact-source validation (task 10)

- Public installer `dist/install.sh` downloads from
  `https://github.com/pgrundev/pgboxdb/releases/download/v<version>/...` and verifies
  the SHA-256 before installing. It never references an internal PGRun build.
- PGRun deploys an internally-built binary staged over ssh and checksum-pinned;
  it never fetches the public release artifacts.
- The two paths are already disjoint; this document fixes that separation as
  policy.

## Compatibility risks

- **Low.** `main` and `prod` share history; `main`'s engine equals the deployed
  prod engine plus the libxml2-soname fix. No engine behaviour on `prod` changes
  unless a fix is deliberately backported and PGRun redeploys.
- The local-dev `pgboxdb` CLI and distribution are new and do not alter the engine
  binary; they are absent from `prod`.
- A tag accidentally cut from `prod` cannot publish a public release (the
  `guard` job). A fix landing on `main` does not reach `prod` until cherry-picked
  and redeployed.
