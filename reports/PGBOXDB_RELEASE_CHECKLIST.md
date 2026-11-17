# PGBoxDB v0.2.0: final release checklist

Updated 2026-11-17. Exact commands: `PGBOXDB_V0.2.0_RELEASE_RUNBOOK.md`.
Readiness and risks: `PGBOXDB_V0.2.0_READINESS.md`.

## Done

- [x] Rebrand, compatibility layer, `pgboxdb migrate`, release-workflow fix
      and regression test, consistency test, crash-test fix, PGRun dev
      change, reports (`rebrand/pgboxdb`)
- [x] CI green on the final head `c8d3551a70` (run 44025347562): Linux
      amd64 and arm64, macOS, Docker amd64 and arm64, npm, Worker, Skill,
      plugin, `release-workflow`
- [x] Release workflow verified on GitHub as a dry run (run 44017561323):
      7 files, no release, no tag
- [x] **PR #50 merged** (approved 2026-11-17): `pgx/main` = `75112dad28`, a
      merge commit, so the GitHub history is preserved
- [x] `release/v0.2.0` brought up to date with `pgx/main` (merge, local, not
      pushed). Its diff against `main` is only the version bump (19 files)
      plus 3 report files (checklist, runbook, PR body)
- [x] Re-tested on `release/v0.2.0`: release replay 31/31, consistency 42/42,
      `version.sh push v0.2.0` → `0.2.0`, npm 22/22, Skill suite
- [x] **`npm-publish` environment**: 1 required reviewer (`alexshapalov`).
      `publish` is the only job that uses it, and it runs only in
      `pgrundev/pgboxdb`. See "Open points" for the branch policy
- [x] **Repository references**: every workflow, the installer, the npm
      wrapper, the Worker, the Docker files, the Skill and the plugin
      reference `pgrundev/pgboxdb` (or `pgrundev/pgboxdb-plugin`). The old
      slug remains only in history: the v0.1.x release notes (kept as
      published), dated validation logs and plans, and these reports
- [x] Historical release notes preserved as "Earlier releases (as PGX)"

## Stopped here: legal decision on GitHub's redirects

Renaming `pgrundev/pgx` → `pgrundev/pgboxdb` keeps the history, the 718
stars, 37 forks, issues, PRs and releases. GitHub then redirects every old
URL (web, `git clone`, release downloads, raw files) to the new name, with
no expiry. Only creating a new repository named `pgrundev/pgx` ends the
redirect. Facts that bear on the decision:

* **What depends on the redirect today:**
  * npm `pgxdb@0.1.3` downloads from `github.com/pgrundev/pgx/releases`;
  * the live pgx.dev Worker serves `raw.githubusercontent.com/pgrundev/pgx/…/install.sh`;
  * PGRun's dev `engine.pin` clones `git@github.com:pgrundev/pgx.git`;
  * existing clones and the Claude Code marketplace `pgrundev/pgx-plugin`.
* **The options:**
  * **A. Rename and keep the redirects** (the runbook's plan). Nothing
    breaks. The old name keeps resolving to PGBoxDB on github.com.
  * **B. Rename, then end the redirects** by creating placeholder
    repositories `pgrundev/pgx` and `pgrundev/pgx-plugin`, with an empty
    README or a notice. Everything in the list above breaks: `pgxdb` and
    pgx.dev installs fail, and old clones point at the placeholder. Do it
    only together with deprecating `pgxdb` and retiring pgx.dev, which are
    currently held for legal.
  * **C. Do not rename.** Create `pgrundev/pgboxdb` fresh and push the full
    git history to it. The commits and tags are preserved; the issues, PRs,
    stars and releases stay on `pgrundev/pgx`. No redirect exists.
* **Repository settings carried over by a rename:** homepage
  `https://pgx.dev` and the description "PGX — Lightweight Postgres…". Both
  need updating (`gh repo edit`). The homepage change is part of the pgx.dev
  decision.

## Requires approval: in this order

1. [ ] **Legal**: choose A, B or C above, and sign off on the remaining
       uses of the previous name (compatibility matrix, "Legal review")
2. [ ] **Rename** `pgrundev/pgx` → `pgrundev/pgboxdb` and `pgrundev/pgx-plugin`
       → `pgrundev/pgboxdb-plugin`; update the description and homepage;
       `git remote set-url`. Check the redirects (runbook step 2)
3. [ ] **Release PR**: push `release/v0.2.0`, open the PR, CI green, merge
4. [ ] **Tag** `v0.2.0` on the merged `main`; check the draft (7 files,
       `shasum -c`)
5. [ ] **Publish the GitHub Release** (separate approval). This starts
       `npm-publish.yml`
6. [ ] **npm**: stage-only `NPM_TOKEN`; approve the `npm-publish`
       environment; **approve the staged package with 2FA** (separate
       approval); registry clean-machine check; switch to the trusted
       publisher; delete and revoke the token
7. [ ] **Worker** `wrangler deploy` (separate approval)
8. [ ] **DNS / site** (separate approval). pgx.dev is not redirected
9. [ ] Clean-install verification (runbook step 8)
10. [ ] Plugin repository sync secrets and `publish-plugin.yml`; Docker
        naming; PGRun developers run `reports/pgrun-dev/apply.sh` (PGRun
        production is not touched)

## Open points (no blocker)

* The `npm-publish` environment has **no deployment-branch policy**, so a
  run from any branch could request it. The reviewer gate and npm's 2FA
  approval still hold. Suggest restricting it to tags `v*` after the rename:
  `gh api -X PUT repos/pgrundev/pgboxdb/environments/npm-publish` with
  `deployment_branch_policy`, plus a tag rule (runbook step 5.0).
* `prevent_self_review` is off. You are the only reviewer, so it must stay
  off, or nobody could approve.
* `plugin-publish` has no reviewers.
* Branch protection on `main` requires the Linux and macOS `cli` checks and
  Docker amd64. `release-workflow` is not required; consider adding it.
* `SECURITY.md` and issue templates are still missing for the public launch.

## Post-release verification

- [ ] `npx pgboxdb@latest` on a machine with no previous install: `run`,
      `ls`, `top --demo`, `stop`
- [ ] On a machine with `~/.pgx`: `pgboxdb` refuses with the migrate hint;
      `migrate`, `migrate --apply` and `start` keep every database;
      `migrate --rollback --apply` restores the old state
- [ ] `curl -fsSL https://pgboxdb.com/install | sh` verifies the checksum
- [ ] `/plugin install pgboxdb@pgboxdb-plugin` in a fresh Claude Code config
- [ ] `check-pgboxdb.sh` reports `{"ok":true,"version":"0.2.0"…}`
- [ ] `pgxdb@0.1.3` and the pgx.dev install still work through the redirect
      (option A)

## Rollback

| Step | Rollback |
|---|---|
| merge to `main` (done) | revert merge `75112dad28` with a PR; `prod` and PGRun are unaffected |
| repository rename | rename it back (works while nobody has created the old name) |
| release / tag | set the release back to draft or delete it, and delete the tag |
| npm `pgboxdb` | reject an unapproved staged version; once live, `npm deprecate`, or unpublish within 72 h |
| Worker | `wrangler rollback` |
| user data | `pgboxdb migrate --rollback --apply` (tested) |
| PGRun dev files | `reports/pgrun-dev/apply.sh <checkout> --rollback` (tested) |
