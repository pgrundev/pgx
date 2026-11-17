# PGBoxDB v0.2.0: final release checklist

Everything under "Requires approval" is an external or irreversible action.
None of it was performed, and each item needs its own explicit go-ahead.
Readiness and risks: `PGBOXDB_V0.2.0_READINESS.md`.

## Done locally (not pushed)

Branch `rebrand/pgboxdb` (base `pgx/main` `56177328d7`):

- [x] Rename implemented: crate, binary, npm package, env, paths, docs, Skill,
      plugin, workflows, Docker, installer, Worker (`PGBOXDB_RENAME_AUDIT.md`)
- [x] Compatibility layer and `pgboxdb migrate` with rollback
      (`PGBOXDB_COMPATIBILITY_MATRIX.md`); trailing project-file comments
      converted too (`66e5b67e37`)
- [x] **`release.yml` asset-wipe bug fixed** (`3891435c18`). The draft job
      checks out before downloading, `dist/release/collect.sh` requires and
      verifies every asset, `fail_on_unmatched_files` is set, and the
      version is resolved once and must match crate and npm. The
      dispatch-run bug (version `main<v>`) is fixed too.
- [x] **Regression coverage**: `dist/test/release_workflow_test.rb`
      replays the draft job, fails on the old workflow, and runs in CI
      (`release-workflow` job)
- [x] **One-name consistency** across npm, release assets, installer,
      Worker and Docker: `dist/test/dist_consistency_test.sh` (in CI). The
      Worker is pinned to `v0.2.0` (`130d8a56ba`).
- [x] **Crash test harness** false failure fixed (`a24f804b33`)
- [x] **PGRun dev change prepared**: `reports/pgrun-dev/` (before/after
      files and a tested `apply.sh` with rollback). No PGRun repository
      change is needed.
- [x] **`PGX_AGENT_TOKEN` investigated**: not in git, in an ignored `0600`
      file. Rotation is optional, and the procedure is in PGRun migration §F.
- [x] Local test matrix green on macOS arm64 (`PGBOXDB_TEST_REPORT.md` §2, §7)

Branch `release/v0.2.0` (`5fbbfd079f`, on top of `130d8a56ba`):

- [x] Version 0.2.0 in crate, `Cargo.lock`, npm, plugin, Skill and README;
      release notes `v0.2.0` (with `pgboxdb top`); Skill floor 0.2.0.
      Tested: unit, lifecycle, databases, clone, agent_install, git,
      Skill e2e and unit, plugin, npm, and the consistency test (42/42)
      against an assembled 0.2.0 distribution.

## Decisions

- [ ] **Legal review** of the remaining uses of the previous name
      (compatibility matrix, "Legal review" items 1–6)
- [x] Release notes: v0.1.x kept as published (PGX), under "Earlier releases
      (as PGX)"; PGBoxDB starts at v0.2.0 (`1fc2afb8b5`)
- [x] npm `pgxdb` and pgx.dev: unchanged, pending legal review
- [x] `PGX_AGENT_TOKEN` rotation: a separate production maintenance task
- [ ] `SECURITY.md` / issue templates for the public launch (they need a
      reporting contact)

## CI validation (done, 2026-11-17)

- [x] `rebrand/pgboxdb` pushed; draft PR #50. CI run 44017542703 green:
      Linux amd64 and arm64, macOS, Docker amd64 and arm64, npm, Worker,
      Skill, plugin, release-workflow
- [x] Release workflow on GitHub: run 44017561323 (`workflow_dispatch`).
      All 4 builds passed with packaged tests; `draft-dry-run` listed
      exactly 7 files; no release or tag was created
- [x] `release/v0.2.0` contains every rebrand commit (merge, local)

**The exact release commands are in `PGBOXDB_V0.2.0_RELEASE_RUNBOOK.md`.**

## Requires approval: in this order

1. [ ] **Push `rebrand/pgboxdb`** to the `pgx` remote and open the PR against
       `main`. CI must be green, including the jobs that never ran
       locally: Linux `cli` (amd64, arm64), `docker` (amd64, arm64), and
       the new `release-workflow` job. Review, then merge. `prod` is
       untouched.
2. [ ] **Rename the GitHub repository** `pgrundev/pgx` → `pgrundev/pgboxdb`,
       then `git remote set-url pgx git@github.com:pgrundev/pgboxdb.git`
       locally. Do it before step 5: every download URL points at
       `pgrundev/pgboxdb`. Do steps 2–6 close together (readiness, risk 4).
3. [ ] **Rename `pgrundev/pgx-plugin`** → `pgrundev/pgboxdb-plugin` and sync
       it from `plugins/pgboxdb` (`publish-plugin.yml`, dry run first).
4. [ ] **Release PR**: push `release/v0.2.0`, open it against `main`, then
       wait for CI and merge.
5. [ ] **Tag and release**: push tag `v0.2.0` on the merged `main` commit,
       then watch `release.yml`. The guard prints `0.2.0`. The draft must
       list `pgboxdb-0.2.0-{darwin-arm64,linux-amd64,linux-arm64}.tar.gz`,
       their `.sha256` files and `checksums.txt` (7 files, no darwin-amd64).
       Download one tarball, run `shasum -a 256 -c`, then publish the draft.
6. [ ] **npm**: create the trusted publisher for the **new** package
       `pgboxdb` (OIDC, `npm-publish.yml`, environment `npm`). It is a first
       publish of a new name, so use staged publishing (the default) and
       approve with 2FA. Then check `npx pgboxdb@0.2.0 --version` from a
       clean HOME.
7. [ ] **Installer Worker**: `wrangler deploy` (it is already pinned to
       `v0.2.0`) on the account that owns the `pgboxdb.com` zone. Check
       `curl -fsSL https://pgboxdb.com/install | sh`.
8. [ ] **DNS / site**: point `pgboxdb.com` at `dist/pgboxdb.com/site`, and
       `pgbox.dev/db` at the documentation.
9. [ ] **Docker**: if images are published, use the `pgboxdb` name only (and
       first add `ARG PGBOXDB_VERSION`; readiness, risk 6).
10. [ ] **Claude Code marketplace**: announce installing `pgboxdb` and
        uninstalling `pgx`.
11. [ ] **PGRun**: each developer runs `reports/pgrun-dev/apply.sh
        <pgrun-checkout>` after installing `pgboxdb`, then migrates their
        home by hand (§B3). Update the marketing links (§C) once
        `pgboxdb.com` serves.

## Post-release verification

- [ ] `npx pgboxdb@latest` on a machine with no previous install: `run`,
      `ls`, `top --demo`, `stop`
- [ ] On a machine with `~/.pgx`: `pgboxdb` refuses with the migrate hint.
      Then `pgboxdb migrate`, `migrate --apply` and `start` must keep every
      database, and `migrate --rollback --apply` must return to the old
      state.
- [ ] `curl -fsSL https://pgboxdb.com/install | sh` verifies the checksum
- [ ] `/plugin install pgboxdb@pgboxdb-plugin` in a fresh Claude Code config
- [ ] The Skill's `check-pgboxdb.sh` reports `{"ok":true,"version":"0.2.0"…}`

## Rollback

| Step | Rollback |
|---|---|
| merge to `main` | revert the merge commit (a PR); `prod` and PGRun are unaffected |
| repository rename | rename it back (GitHub keeps redirects until the name is reused) |
| release / tag | set the release back to draft or delete it, and delete the tag (both outward, need approval) |
| npm `pgboxdb` | `npm deprecate pgboxdb@0.2.0`, or unpublish within 72 h (npm policy) |
| Worker | `wrangler rollback` |
| user data | `pgboxdb migrate --rollback --apply` (tested) |
| PGRun dev files | `reports/pgrun-dev/apply.sh <checkout> --rollback` (tested) |
| these branches locally | `git worktree remove .worktrees/pgboxdb`, `git branch -D rebrand/pgboxdb release/v0.2.0` (deletes local work; ask first) |
