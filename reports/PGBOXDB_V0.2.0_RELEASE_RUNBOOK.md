# PGBoxDB v0.2.0: release runbook

Exact commands, in order. **Every step is an outward action that needs your
approval.** Nothing here has been run. Run them from the PGBoxDB worktree
(`.worktrees/pgboxdb`), whose remote `pgx` is `pgrundev/pgx` until step 2.

## State when this was written (2026-11-17)

* PR #50 (`rebrand/pgboxdb` → `main`), draft. **CI green**, run
  44017542703: Linux amd64 and arm64, macOS arm64 (fmt, clippy, unit,
  black-box, top, agent, Git, Skill e2e, installer, npm), Docker amd64 and
  arm64, Worker, npm (Linux and macOS), Skill, plugin, and
  `release-workflow`.
* **Release workflow verified on GitHub**, run 44017561323
  (`workflow_dispatch`, version `0.0.0-ci.rebrand`, ref `rebrand/pgboxdb`).
  * All four builds passed, each running the packaged lifecycle, databases
    and clone tests (the experimental `darwin-amd64` leg passed too).
  * `draft-dry-run` downloaded the real artifacts, dropped `darwin-amd64`,
    verified the checksums, and listed exactly the 7 release files.
  * The `draft` job was skipped, and no release or tag was created.
* `release/v0.2.0` (local, not pushed) = `rebrand/pgboxdb` merged into the
  0.2.0 bump, so it contains every rebrand fix. It was tested locally only.
* GitHub environment `npm-publish` has 1 required reviewer. The repository
  has **no secrets**.
* npm `pgboxdb` does not exist yet (registry 404), so this is a first
  publish. npm `pgxdb` stays as it is (latest 0.1.3), pending legal review.

## 0. Before anything (decisions)

- Legal sign-off on the remaining uses of the previous name (compatibility
  matrix, "Legal review").
- Already decided: historical notes stay as PGX v0.1.x; `pgxdb` and pgx.dev
  are unchanged for now; `PGX_AGENT_TOKEN` rotation is a separate
  production task.

## 1. Merge the rebrand: DONE 2026-11-17

PR #50 merged as `75112dad28` (merge commit) after CI run 44025347562 was
green on head `c8d3551a70`. `release/v0.2.0` then merged `pgx/main`
(local). The commands that were run:

```sh
gh pr ready 50 -R pgrundev/pgx
gh pr checks 50 -R pgrundev/pgx            # all green
gh pr merge 50 -R pgrundev/pgx --merge     # merge commit, as for #49
gh run list -R pgrundev/pgx --branch main --limit 3   # CI on main green
```

## 2. Rename the repositories

Do steps 2–6 close together. Between the rename and the 0.2.0 release,
`install.sh` without a version picks v0.1.3, whose assets are named `pgx-…`,
and fails cleanly ("download failed").

```sh
gh repo rename pgboxdb -R pgrundev/pgx --yes
git remote set-url pgx git@github.com:pgrundev/pgboxdb.git
gh repo view pgrundev/pgboxdb --json nameWithOwner,url

gh repo rename pgboxdb-plugin -R pgrundev/pgx-plugin --yes

# Settings carried over from the old name (the homepage waits for the pgx.dev decision):
gh repo edit pgrundev/pgboxdb --description "PGBoxDB — Lightweight Postgres for AI agents. Written in Rust. Thousands of isolated databases in one runtime"
gh repo edit pgrundev/pgboxdb --homepage https://pgboxdb.com          # only when approved

# The redirects the old installs depend on (legal option A):
curl -fsSIL https://github.com/pgrundev/pgx/releases/download/v0.1.3/pgx-0.1.3-darwin-arm64.tar.gz.sha256 | grep -i '^HTTP' | tail -1   # 200
curl -fsSL https://raw.githubusercontent.com/pgrundev/pgx/v0.1.3/dist/install.sh | head -1                                               # #!/bin/sh
```

## 3. Release PR (version 0.2.0)

```sh
git fetch pgx
git switch release/v0.2.0
git merge --no-edit pgx/main        # brings in the merge commit of #50
git diff pgx/main --stat            # the version bump (19 files), 3 report files, tests/lifecycle.rs
git push -u pgx release/v0.2.0
gh pr create -R pgrundev/pgboxdb --base main --head release/v0.2.0 \
  --title "release: PGBoxDB v0.2.0" --body-file reports/pgboxdb-v0.2.0-pr-body.md
gh pr checks <PR> -R pgrundev/pgboxdb --watch
gh pr merge <PR> -R pgrundev/pgboxdb --merge
```

Optional rehearsal on the merged `main`, which builds everything and creates
nothing:

```sh
gh workflow run release.yml -R pgrundev/pgboxdb --ref main -f version=0.2.0
gh run watch -R pgrundev/pgboxdb "$(gh run list -R pgrundev/pgboxdb --workflow release.yml -L1 --json databaseId -q '.[0].databaseId')"
```

## 4. Tag and draft release

```sh
git fetch pgx main
git show pgx/main:crates/bin/pgboxdb/Cargo.toml | grep '^version'   # version = "0.2.0"
git show pgx/main:dist/npm/package.json | grep '"version"'          # "0.2.0"
git tag -a v0.2.0 -m "PGBoxDB v0.2.0" pgx/main
git push pgx v0.2.0
gh run watch -R pgrundev/pgboxdb "$(gh run list -R pgrundev/pgboxdb --workflow release.yml -L1 --json databaseId -q '.[0].databaseId')"

# The draft must hold exactly these 7 files:
gh release view v0.2.0 -R pgrundev/pgboxdb --json isDraft,assets --jq '.isDraft, (.assets[].name)'
#   pgboxdb-0.2.0-{darwin-arm64,linux-amd64,linux-arm64}.tar.gz(.sha256), checksums.txt

# Verify one asset before publishing:
D=$(mktemp -d) && gh release download v0.2.0 -R pgrundev/pgboxdb -D "$D" -p 'pgboxdb-0.2.0-darwin-arm64.tar.gz*' -p checksums.txt
(cd "$D" && shasum -a 256 -c pgboxdb-0.2.0-darwin-arm64.tar.gz.sha256 && grep darwin-arm64 checksums.txt)
```

## 5. npm: first publish of `pgboxdb`

npm can't configure a trusted publisher for a package that doesn't exist
yet. So the first version is staged with a short-lived token:

0. Optional hardening: the environment has 1 required reviewer but no
   deployment-branch policy. Restrict it to `v*` tags:
   ```sh
   gh api -X PUT repos/pgrundev/pgboxdb/environments/npm-publish --input - <<'EOF'
   {"reviewers":[{"type":"User","id":REVIEWER_ID}],
    "deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":true}}
   EOF
   gh api -X POST repos/pgrundev/pgboxdb/environments/npm-publish/deployment-branch-policies -f name='v*' -f type=tag
   ```
   (`REVIEWER_ID` comes from `gh api users/alexshapalov --jq .id`. The
   registry check in step 4 is a `dry_run` and never enters the
   environment.)

1. On npmjs.com, create a granular access token that is **stage-only**
   (read and write, all packages, short expiry). Then add it:
   ```sh
   gh secret set NPM_TOKEN -R pgrundev/pgboxdb        # paste the token
   ```
2. Publish the GitHub release. This triggers `npm-publish.yml`
   (`release: published`):
   ```sh
   gh release edit v0.2.0 -R pgrundev/pgboxdb --draft=false --latest
   gh run watch -R pgrundev/pgboxdb "$(gh run list -R pgrundev/pgboxdb --workflow npm-publish.yml -L1 --json databaseId -q '.[0].databaseId')"
   ```
   * `verify` checks that the release is not a draft, the tag is on
     `main`, and all 3 platform assets exist. It then runs `npm test`,
     packs, and smoke-runs the packed tarball against the real release.
   * `publish` waits for the `npm-publish` environment. Approve it in the
     run page (Review deployments). It runs `npm stage publish`, which
     creates the package and queues 0.2.0.
3. Approve the staged version with 2FA: npmjs.com → `pgboxdb` → Staged
   Packages, or:
   ```sh
   npm stage list pgboxdb
   npm stage approve <stage-id>
   npm view pgboxdb dist-tags          # { latest: '0.2.0' }
   ```
4. Run the clean-machine check from the registry (a fresh ubuntu:24.04
   container: no PostgreSQL, no Docker, an unprivileged user):
   ```sh
   gh workflow run npm-publish.yml -R pgrundev/pgboxdb --ref main -f version=0.2.0 -f dry_run=true -f registry_check=true
   ```
5. Switch to trusted publishing for later releases. On npmjs.com →
   `pgboxdb` → Settings → Trusted publishing, choose GitHub Actions, owner
   `pgrundev`, repo `pgboxdb`, workflow `npm-publish.yml`, environment
   `npm-publish`. Then remove the token:
   ```sh
   gh secret delete NPM_TOKEN -R pgrundev/pgboxdb      # and revoke it on npmjs.com
   ```

## 6. Installer Worker

`INSTALL_REF` is already `v0.2.0`. Deploy it with the Cloudflare account
that owns the `pgboxdb.com` zone:

```sh
cd dist/pgboxdb.com && npx wrangler deploy
curl -fsSI https://pgboxdb.com/install | grep -i x-pgboxdb-install-ref    # v0.2.0
```

## 7. DNS / site

Point `pgboxdb.com` at `dist/pgboxdb.com/site`, and `pgbox.dev/db` at the
documentation.

## 8. Clean-install verification (on your Mac, with a throwaway HOME)

Your live `~/.pgx` runtime stays untouched: every command gets a fresh
`HOME` and a free port.

```sh
T=$(mktemp -d); NODE_DIR=$(dirname "$(command -v node)")

# npm
env -i HOME="$T/npm" PATH="$NODE_DIR:/usr/bin:/bin" PGBOXDB_PORT=55432 sh -euc '
  mkdir -p "$HOME"
  npx -y pgboxdb@0.2.0 --version          # pgboxdb 0.2.0 (engine …)
  npx -y pgboxdb@0.2.0 run                # PGBoxDB is ready
  npx -y pgboxdb@0.2.0 --json ls          # contains "app"
  npx -y pgboxdb@0.2.0 --json top --snapshot | grep -q schema_version
  npx -y pgboxdb@0.2.0 stop
  test ! -e "$HOME/.pgx"                  # nothing under the previous name'

# installer (via the Worker, after step 6)
env -i HOME="$T/curl" PATH="/usr/bin:/bin" PGBOXDB_PORT=55433 sh -euc '
  mkdir -p "$HOME"
  curl -fsSL https://pgboxdb.com/install | sh       # verifies the SHA-256
  "$HOME/.local/bin/pgboxdb" --version
  "$HOME/.local/bin/pgboxdb" run && "$HOME/.local/bin/pgboxdb" stop'

# installer straight from the tag (works before the Worker is deployed)
env -i HOME="$T/raw" PATH="/usr/bin:/bin" sh -euc '
  mkdir -p "$HOME"
  curl -fsSL https://raw.githubusercontent.com/pgrundev/pgboxdb/v0.2.0/dist/install.sh | PGBOXDB_VERSION=0.2.0 sh
  "$HOME/.local/bin/pgboxdb" --version'

# Upgrade path, read-only, on your real machine: a dry run that changes nothing
npx -y pgboxdb@0.2.0 migrate
```

Plugin, in a fresh Claude Code config, after the plugin repository sync
(step 9): `/plugin marketplace add pgrundev/pgboxdb-plugin`, then
`/plugin install pgboxdb@pgboxdb-plugin` (as `plugins/pgboxdb/README.md`
documents).

## 9. After the release

* Plugin repository sync: add `PGBOXDB_PLUGIN_APP_ID` and
  `PGBOXDB_PLUGIN_APP_PRIVATE_KEY` (or `PGBOXDB_PLUGIN_DEPLOY_KEY`), then:
  ```sh
  gh workflow run publish-plugin.yml -R pgrundev/pgboxdb --ref main -f dry_run=true
  gh workflow run publish-plugin.yml -R pgrundev/pgboxdb --ref main -f dry_run=false
  ```
* Docker: publish images only under the name `pgboxdb` (no image publishing
  exists today).
* PGRun developers: install `pgboxdb`, run `reports/pgrun-dev/apply.sh
  <pgrun-checkout>`, then migrate their own home (`pgboxdb migrate`, stop
  the old runtime, `pgboxdb migrate --apply`).

## Rollback

| Step | Command |
|---|---|
| 1 merge | `gh pr create` a revert of merge `75112dad28` |
| 2 rename | `gh repo rename pgx -R pgrundev/pgboxdb --yes` (and the plugin repository) |
| 4 tag / draft | `gh release delete v0.2.0 -R pgrundev/pgboxdb --yes` and `git push pgx :refs/tags/v0.2.0` |
| 5 npm | a staged version that isn't approved never goes live (reject it). Once live: `npm deprecate pgboxdb@0.2.0 "<reason>"`, or unpublish within 72 h |
| 6 Worker | `npx wrangler rollback` |
| user data | `pgboxdb migrate --rollback --apply` |
