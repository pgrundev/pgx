# PGBoxDB first-release checklist

Everything below the line "Requires approval" is an external or
irreversible action. None of it was performed. Each item needs its own
explicit go-ahead.

## Done locally (branch `rebrand/pgboxdb`, not pushed)

- [x] Rename implemented: crate, binary, npm package, env, paths, docs, Skill,
      plugin, workflows, Docker, installer, Worker (reports/PGBOXDB_RENAME_AUDIT.md)
- [x] Compatibility layer and `pgboxdb migrate` with rollback
      (reports/PGBOXDB_COMPATIBILITY_MATRIX.md)
- [x] Local test matrix green on macOS arm64 (reports/PGBOXDB_TEST_REPORT.md)
- [x] Release build from a clean commit with `--locked` (also the
      release-equivalent static macOS build on rustc 1.96); distribution
      assembled; `.sha256` verified; self-contained check passed. Tarballs are
      not byte-reproducible across runs, because `dist.json` records `built_at`.
- [x] PGRun impact documented (reports/PGBOXDB_PGRUN_MIGRATION.md):
      production is unaffected

## Decisions needed before release

- [ ] **Legal review** of the remaining uses of the previous name
      (compatibility matrix, "Legal review" section, items 1–6)
- [ ] **Version.** The first PGBoxDB release must use a version that was
      never published before the rename, recommended **0.2.0**. Reasons:
      (a) release assets were renamed, so `pgboxdb@0.1.x` has nothing to
      download; (b) a migrated home contains the previous dists under
      `dist/0.1.x`. The installers now refuse to replace those, but a new
      version number avoids the collision entirely.
- [ ] npm `pgxdb`: leave as is, or publish a deprecation message (an outward
      change; legal input, since the message would name the old package)
- [ ] pgx.dev: what it serves after the rename (no redirect was implemented)
- [ ] The repository has **no `SECURITY.md`, issue templates or PR template**
      (only `.github/workflows/`), so there was nothing to rebrand. If you
      want them for the public launch, a security policy needs a reporting
      contact, which is a maintainer decision. None was invented.
      `CONTRIBUTORS.md` was rebranded.

## Requires approval: suggested order

1. [ ] **Review and merge.** Push `rebrand/pgboxdb` to the `pgx` remote,
       open the PR against `main`, wait for CI (3 OS legs + Docker x2 +
       worker + npm + skill + plugin), then merge. `prod` is untouched.
2. [ ] **Rename the GitHub repository** `pgrundev/pgx` → `pgrundev/pgboxdb`
       (Settings → General). GitHub redirects the old URLs. Then update the
       local remotes (`git remote set-url pgx git@github.com:pgrundev/pgboxdb.git`).
       Do this **before** step 4, because every download URL in the release
       points at `pgrundev/pgboxdb`.
3. [ ] **Rename `pgrundev/pgx-plugin`** → `pgrundev/pgboxdb-plugin` and sync it
       from `plugins/pgboxdb` (`publish-plugin.yml` runs only in
       `pgrundev/pgboxdb`; dry run first).
4. [ ] **Release PR**: bump `crates/bin/pgboxdb/Cargo.toml` and
       `dist/npm/package.json` to 0.2.0 (or the chosen version), update the
       plugin, Skill and README version references, and replace `vNEXT` in
       `dist/release-notes.md`. Merge it.
5. [ ] **Fix the open `release.yml` bug first.** The `draft` job runs
       `actions/checkout` after `download-artifact`, which wipes `artifacts/`,
       so the v0.1.3 draft was created with no assets and they were uploaded
       by hand. This is a separate, small PR.
6. [ ] **Tag and release**: push tag `v0.2.0`, then check the `release.yml`
       run. The draft must list `pgboxdb-0.2.0-{darwin-arm64,linux-amd64,linux-arm64}.tar.gz`,
       their `.sha256` files and `checksums.txt` (darwin-amd64 is
       intentionally removed). Publish the draft.
7. [ ] **npm**: create the trusted publisher for the **new** package name
       `pgboxdb` (OIDC, `npm-publish.yml`, environment `npm`). First publish of
       a new name: staged publishing (`stage=true`, the default), then approve
       with 2FA. Verify `npx pgboxdb@0.2.0 --version` from a clean HOME.
8. [ ] **Installer Worker**: set `INSTALL_REF` in
       `dist/pgboxdb.com/wrangler.toml` to `v0.2.0`. It currently pins
       `v0.1.0-beta.1`, whose `install.sh` installs the previous assets. Then
       `wrangler deploy` on the account that owns the `pgboxdb.com` zone.
       Verify `curl -fsSL https://pgboxdb.com/install | sh`.
9. [ ] **DNS / site**: point `pgboxdb.com` at the site (`dist/pgboxdb.com/site`)
       and `pgbox.dev/db` at the documentation.
10. [ ] **Docker**: if images are published, publish them under the
        `pgboxdb` name only.
11. [ ] **Claude Code marketplace**: the plugin is now `pgboxdb`. Announce
        that users should install `pgboxdb` and uninstall `pgx`.
12. [ ] **PGRun follow-ups** (in the PGRun repository): PGRUN_MIGRATION §B
        (dev hooks, `.pgx.toml`) before developers switch; §C (links) when
        `pgboxdb.com` serves.

## Post-release verification

- [ ] `npx pgboxdb@latest` on a machine with no previous install: `run`, `ls`,
      `stop`
- [ ] On a machine with a `~/.pgx` home: `npx pgboxdb@latest` refuses with
      the migrate hint; `pgboxdb migrate`, then `--apply`, then `start` keeps
      every database; `pgboxdb migrate --rollback --apply` returns
- [ ] `curl -fsSL https://pgboxdb.com/install | sh` verifies the checksum
- [ ] `/plugin install pgboxdb@pgboxdb-plugin` in a fresh Claude Code config

## Rollback

| Step | Rollback |
|---|---|
| merge to `main` | revert the merge commit (a PR); `prod` and PGRun are unaffected |
| repository rename | rename back (GitHub keeps redirects both ways until the name is reused) |
| release / tag | mark the release as draft or delete it; delete the tag (both outward, need approval) |
| npm `pgboxdb` | `npm deprecate pgboxdb@<v>`, or unpublish within 72 h (npm policy) |
| Worker | `wrangler rollback`, or re-deploy the previous `INSTALL_REF` |
| user data | `pgboxdb migrate --rollback --apply` (tested: reports/PGBOXDB_TEST_REPORT.md) |
| this branch locally | `git worktree remove .worktrees/pgboxdb` and `git branch -D rebrand/pgboxdb` (both delete local work, so ask first) |
