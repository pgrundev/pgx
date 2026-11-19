The first release under the PGBoxDB name. The rebrand itself is #50; this PR
only sets the version.

## Changes
- Version 0.2.0 in `crates/bin/pgboxdb/Cargo.toml`, `Cargo.lock`,
  `dist/npm/package.json`, the plugin manifest, Skill metadata and README.
- Release notes: "PGBoxDB v0.2.0 — the first release under the PGBoxDB
  name". The v0.1.x sections stay as published, under "Earlier releases (as
  PGX)".
- The Skill requires PGBoxDB 0.2.0 or newer (`check-pgboxdb.sh`, SKILL.md,
  references, docs). 0.1.x only ever shipped as PGX.
- `dist_consistency_test.sh` also checks that the Worker `INSTALL_REF`
  equals `v<crate version>` (`v0.2.0`).
- `tests/lifecycle.rs`: three init tests now retry a transient spawn failure
  (ETXTBSY on the arm64 runner), like the rest of the harness.
- Reports: the final release checklist and the runbook.

## Checks
- CI on this PR.
- Locally: release replay 31/31, consistency 42/42,
  `dist/release/version.sh push v0.2.0` → `0.2.0`, npm 22/22, Skill suite.

## After merge (separately approved)
Tag `v0.2.0` on the merge commit, then follow
`reports/PGBOXDB_V0.2.0_RELEASE_RUNBOOK.md` step 4 onward. The release is
created as a draft and is not published automatically.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
