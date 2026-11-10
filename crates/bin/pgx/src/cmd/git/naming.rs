//! Deterministic database names for Git-managed databases.
//!
//! A name is readable (`pgx_myapp_user_auth_a19f3c`) but its identity is the
//! hash suffix, computed over the full key: repository id, worktree id, the
//! complete branch name and the role. Two repositories with the same branch,
//! two worktrees of one repository, `feature/x` and `fix/x`: all different
//! names. Only `[a-z0-9_]` reaches SQL, the result is always a valid
//! [`crate::names`] name, and it never exceeds PostgreSQL's 63 bytes.
use crate::cmd::agent::identity::{fnv1a64, hex16};

/// Lowercase `[a-z0-9]` runs joined by `_`, at most `max` bytes.
pub fn slug(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in s.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if gap && !out.is_empty() {
                out.push('_');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
        if out.len() >= max {
            break;
        }
    }
    out.truncate(max);
    out.trim_end_matches('_').to_string()
}

/// The readable part of a branch: everything after the first path component
/// when there are several (`feature/user-auth` → `user_auth`), else the name.
pub fn branch_slug(branch: &str) -> String {
    let visible = match branch.split_once('/') {
        Some((_, rest)) if !rest.is_empty() => rest,
        _ => branch,
    };
    let s = slug(visible, 24);
    if s.is_empty() {
        "branch".to_string()
    } else {
        s
    }
}

pub fn repo_slug(name: &str) -> String {
    let s = slug(name, 16);
    if s.is_empty() {
        "repo".to_string()
    } else {
        s
    }
}

fn suffix(material: &str, long: bool) -> String {
    let h = hex16(fnv1a64(material.as_bytes()));
    if long {
        h[..12].to_string()
    } else {
        h[..6].to_string()
    }
}

/// The main development database of a repository.
pub fn main_db(repo_slug: &str, repo_id: &str, long: bool) -> String {
    format!(
        "pgx_{repo_slug}_main_{}",
        suffix(&format!("{repo_id}|main"), long)
    )
}

/// A feature branch's database in one worktree.
pub fn branch_db(
    repo_slug: &str,
    repo_id: &str,
    worktree_id: &str,
    branch: &str,
    long: bool,
) -> String {
    format!(
        "pgx_{repo_slug}_{}_{}",
        branch_slug(branch),
        suffix(&format!("{repo_id}|{worktree_id}|{branch}|branch"), long)
    )
}

/// The database a detached HEAD uses in one worktree.
pub fn detached_db(repo_slug: &str, repo_id: &str, worktree_id: &str, long: bool) -> String {
    format!(
        "pgx_{repo_slug}_detached_{}",
        suffix(&format!("{repo_id}|{worktree_id}|detached"), long)
    )
}

/// A published main snapshot.
pub fn snapshot_db(repo_slug: &str, repo_id: &str, generation: u64) -> String {
    format!(
        "pgx_{repo_slug}_base_v{generation}_{}",
        suffix(&format!("{repo_id}|snapshot|{generation}"), false)
    )
}

/// A snapshot under construction (renamed to [`snapshot_db`] on publish).
pub fn staging_db(repo_slug: &str, token: &str) -> String {
    format!("pgx_{repo_slug}_stg_{}", slug(token, 12))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("Feature/User-Auth.v2", 64), "feature_user_auth_v2");
        assert_eq!(slug("--__..", 64), "");
        assert_eq!(slug("ÜberApp", 64), "berapp");
        assert_eq!(branch_slug("feature/user-auth"), "user_auth");
        assert_eq!(branch_slug("main"), "main");
        assert_eq!(branch_slug("team/feat/x"), "feat_x");
        assert_eq!(branch_slug("ü/ö"), "branch");
        assert_eq!(repo_slug("My App"), "my_app");
    }

    #[test]
    fn names_are_valid_bounded_and_collision_resistant() {
        let rid = "0123456789abcdef";
        let wt1 = "1111111111111111";
        let wt2 = "2222222222222222";
        let n = branch_db("myapp", rid, wt1, "feature/user-auth", false);
        assert!(n.starts_with("pgx_myapp_user_auth_"), "{n}");
        assert_eq!(
            n,
            branch_db("myapp", rid, wt1, "feature/user-auth", false),
            "deterministic"
        );
        // Different worktree, repo, or a branch with the same slug: different names.
        assert_ne!(n, branch_db("myapp", rid, wt2, "feature/user-auth", false));
        assert_ne!(
            n,
            branch_db("myapp", "fedcba9876543210", wt1, "feature/user-auth", false)
        );
        assert_ne!(n, branch_db("myapp", rid, wt1, "fix/user-auth", false));
        assert_ne!(n, branch_db("myapp", rid, wt1, "feature/user_auth", false));
        let long_branch = format!("feature/{}", "x".repeat(250));
        for name in [
            branch_db(&repo_slug(&"r".repeat(80)), rid, wt1, &long_branch, true),
            branch_db("myapp", rid, wt1, "feature/'; DROP DATABASE app; --", false),
            main_db(&repo_slug("Ünïcödé"), rid, true),
            detached_db("myapp", rid, wt1, true),
            snapshot_db(&repo_slug(&"r".repeat(80)), rid, u64::MAX),
            staging_db("myapp", "a1b2c3d4e5f6"),
        ] {
            assert!(name.len() <= 63, "{name}");
            crate::names::validate_user_target(&name).unwrap();
            assert!(
                name.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "{name}"
            );
        }
        assert_ne!(main_db("a", rid, false), main_db("a", rid, true));
        assert_ne!(snapshot_db("a", rid, 1), snapshot_db("a", rid, 2));
    }
}
