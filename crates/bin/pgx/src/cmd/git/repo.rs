//! Git plumbing: where the repository is, which worktree this is, what HEAD
//! points at. Only read-only plumbing commands are run, always as an argv
//! (never through a shell), with branch names passed as data after `--` or
//! inside a full `refs/heads/…` ref.
use super::config::valid_branch;
use crate::cmd::agent::identity::{fnv1a64, hex16};
use crate::errors::{PgxError, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// What HEAD points at in a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// A local branch (possibly unborn: no commit yet).
    Branch(String),
    /// A detached HEAD at this commit.
    Detached(String),
}

impl Head {
    pub fn branch(&self) -> Option<&str> {
        match self {
            Head::Branch(b) => Some(b),
            Head::Detached(_) => None,
        }
    }
    pub fn label(&self) -> String {
        match self {
            Head::Branch(b) => b.clone(),
            Head::Detached(c) => format!("(detached at {})", &c[..c.len().min(9)]),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Repo {
    /// Canonical top level of this worktree.
    pub toplevel: PathBuf,
    /// Absolute per-worktree Git dir (`.git`, or `.git/worktrees/<name>`).
    pub git_dir: PathBuf,
    /// Canonical common dir, shared by every worktree of the repository.
    pub common_dir: PathBuf,
    /// 16 hex: the repository identity (hash of the common dir).
    pub repo_id: String,
    /// 16 hex: the worktree identity (hash of the canonical top level).
    pub worktree_id: String,
    pub head: Head,
    /// A linked worktree (`git worktree add`), not the main one.
    pub linked: bool,
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// A `git` command for `dir`. Inherited `GIT_DIR`/`GIT_WORK_TREE` (Git sets
/// them for some hooks) are made absolute against our own working directory,
/// so changing the child's directory cannot re-point them.
pub fn git_cmd(dir: &Path) -> Command {
    let mut c = Command::new(std::env::var_os("PGX_GIT").unwrap_or_else(|| "git".into()));
    c.current_dir(dir);
    let here = std::env::current_dir().ok();
    for var in ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR"] {
        if let Some(v) = std::env::var_os(var) {
            let p = PathBuf::from(&v);
            if p.is_relative() {
                if let Some(h) = &here {
                    c.env(var, h.join(p));
                }
            }
        }
    }
    // Never prompt, never page, never run an external diff/merge tool.
    c.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null());
    c
}

/// Run git, returning stdout when it exits 0.
pub fn git_out(dir: &Path, args: &[&str]) -> Option<String> {
    let out = git_cmd(dir).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    git_cmd(dir)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub fn git_available() -> bool {
    Command::new(std::env::var_os("PGX_GIT").unwrap_or_else(|| "git".into()))
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The repository containing `dir`, or None outside any repository (or in a
/// bare repository, which has no worktree to give a database to).
pub fn discover(dir: &Path) -> Result<Option<Repo>> {
    let Some(text) = git_out(
        dir,
        &[
            "rev-parse",
            "--show-toplevel",
            "--absolute-git-dir",
            "--git-common-dir",
        ],
    ) else {
        return Ok(None);
    };
    let mut lines = text.lines();
    let (Some(top), Some(gd), Some(cd)) = (lines.next(), lines.next(), lines.next()) else {
        return Ok(None);
    };
    if top.is_empty() {
        return Ok(None);
    }
    let toplevel = canon(Path::new(top));
    let git_dir = canon(Path::new(gd));
    // --git-common-dir is relative to the command's directory when relative.
    let cdp = PathBuf::from(cd);
    let common_dir = canon(&if cdp.is_absolute() {
        cdp
    } else {
        dir.join(cdp)
    });
    let head = read_head(&toplevel)?;
    let linked = git_dir != common_dir;
    Ok(Some(Repo {
        repo_id: hex16(fnv1a64(common_dir.to_string_lossy().as_bytes())),
        worktree_id: hex16(fnv1a64(toplevel.to_string_lossy().as_bytes())),
        toplevel,
        git_dir,
        common_dir,
        head,
        linked,
    }))
}

fn read_head(dir: &Path) -> Result<Head> {
    if let Some(b) = git_out(dir, &["symbolic-ref", "-q", "--short", "HEAD"]) {
        let b = b.trim().to_string();
        if !valid_branch(&b) {
            return Err(PgxError::usage(format!(
                "the current branch name {b:?} is not one PGX Git accepts"
            )));
        }
        return Ok(Head::Branch(b));
    }
    let sha = git_out(dir, &["rev-parse", "-q", "--verify", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    Ok(Head::Detached(sha))
}

impl Repo {
    /// A short readable name for the repository: the main worktree's folder.
    pub fn display_name(&self) -> String {
        let base = if self.common_dir.file_name().is_some_and(|n| n == ".git") {
            self.common_dir.parent().map(Path::to_path_buf)
        } else {
            Some(self.common_dir.clone())
        };
        base.and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .map(|n| n.trim_end_matches(".git").to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "repo".to_string())
    }

    pub fn branch_exists(&self, branch: &str) -> bool {
        valid_branch(branch)
            && git_ok(
                &self.toplevel,
                &[
                    "show-ref",
                    "--verify",
                    "-q",
                    &format!("refs/heads/{branch}"),
                ],
            )
    }

    /// The commit a local branch points at.
    pub fn branch_rev(&self, branch: &str) -> Option<String> {
        if !valid_branch(branch) {
            return None;
        }
        git_out(
            &self.toplevel,
            &[
                "rev-parse",
                "-q",
                "--verify",
                &format!("refs/heads/{branch}^{{commit}}"),
            ],
        )
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    }

    /// The effective hooks directory: honours `core.hooksPath`, and is the
    /// same for every worktree (hooks live in the common dir).
    pub fn hooks_dir(&self) -> PathBuf {
        let p = git_out(&self.toplevel, &["rev-parse", "--git-path", "hooks"])
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "hooks".to_string());
        let p = PathBuf::from(p);
        if p.is_absolute() {
            p
        } else {
            self.toplevel.join(p)
        }
    }

    pub fn hooks_path_config(&self) -> Option<String> {
        git_out(&self.toplevel, &["config", "--get", "core.hooksPath"])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Whether `path` (inside the worktree) is tracked by Git.
    pub fn is_tracked(&self, path: &Path) -> bool {
        let Ok(rel) = canon(path.parent().unwrap_or(path))
            .join(path.file_name().unwrap_or_default())
            .strip_prefix(&self.toplevel)
            .map(Path::to_path_buf)
        else {
            return false;
        };
        git_ok(
            &self.toplevel,
            &["ls-files", "--error-unmatch", "--", &rel.to_string_lossy()],
        )
    }

    /// Every worktree of the repository: (canonical path, branch or None).
    pub fn worktrees(&self) -> Vec<(PathBuf, Option<String>)> {
        self.worktrees_checked()
            .unwrap_or_else(|| vec![(self.toplevel.clone(), self.head.branch().map(String::from))])
    }

    /// Whether a local branch exists: `None` when Git itself failed (so a
    /// caller deciding to delete something must not read it as "deleted").
    pub fn branch_known(&self, branch: &str) -> Option<bool> {
        if !valid_branch(branch) {
            return None;
        }
        let st = git_cmd(&self.toplevel)
            .args([
                "show-ref",
                "--verify",
                "-q",
                &format!("refs/heads/{branch}"),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        match st.code() {
            Some(0) => Some(true),
            Some(1) => Some(false),
            _ => None,
        }
    }

    /// Every worktree, or `None` when `git worktree list` failed.
    pub fn worktrees_checked(&self) -> Option<Vec<(PathBuf, Option<String>)>> {
        let text = git_out(&self.toplevel, &["worktree", "list", "--porcelain"])?;
        let mut out = Vec::new();
        let mut path: Option<PathBuf> = None;
        let mut branch: Option<String> = None;
        for line in text.lines().chain(std::iter::once("")) {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(canon(Path::new(p)));
                branch = None;
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = b.strip_prefix("refs/heads/").map(String::from);
            } else if line.is_empty() {
                if let Some(p) = path.take() {
                    out.push((p, branch.take()));
                }
            }
        }
        Some(out)
    }

    /// The worktree that has `branch` checked out, if any.
    pub fn worktree_of(&self, branch: &str) -> Option<PathBuf> {
        self.worktrees()
            .into_iter()
            .find(|(_, b)| b.as_deref() == Some(branch))
            .map(|(p, _)| p)
    }

    /// File paths at `rev`, for the migration scan. Read from Git's object
    /// store, so the working tree (another branch's files) does not matter.
    pub fn files_at(&self, rev: &str) -> Vec<String> {
        git_out(
            &self.toplevel,
            &["ls-tree", "-r", "--name-only", "--full-tree", rev],
        )
        .map(|t| t.lines().map(String::from).collect())
        .unwrap_or_default()
    }

    /// `git branch -m old new` leaves "Branch: renamed refs/heads/old to
    /// refs/heads/new" as the newest reflog entry of the new name.
    pub fn renamed_from(&self, branch: &str) -> Option<String> {
        if !valid_branch(branch) {
            return None;
        }
        let msg = git_out(
            &self.toplevel,
            &[
                "reflog",
                "show",
                "-n",
                "1",
                "--format=%gs",
                &format!("refs/heads/{branch}"),
                "--",
            ],
        )?;
        let msg = msg.trim();
        let rest = msg.strip_prefix("Branch: renamed refs/heads/")?;
        let (old, new) = rest.split_once(" to refs/heads/")?;
        (new == branch && valid_branch(old)).then(|| old.to_string())
    }

    /// During `git rebase` HEAD is detached; the branch being rebased is the
    /// one whose database the work belongs to.
    pub fn rebasing_branch(&self) -> Option<String> {
        for d in ["rebase-merge", "rebase-apply"] {
            if let Ok(t) = std::fs::read_to_string(self.git_dir.join(d).join("head-name")) {
                if let Some(b) = t.trim().strip_prefix("refs/heads/") {
                    if valid_branch(b) {
                        return Some(b.to_string());
                    }
                }
            }
        }
        None
    }

    /// The configured main branch, or a sensible guess: `origin/HEAD`, then
    /// `main`, `master`, `trunk`, `develop`, then the current branch.
    pub fn guess_main(&self) -> String {
        if let Some(r) = git_out(
            &self.toplevel,
            &["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"],
        ) {
            if let Some(b) = r.trim().strip_prefix("origin/") {
                if valid_branch(b) {
                    return b.to_string();
                }
            }
        }
        for b in ["main", "master", "trunk", "develop"] {
            if self.branch_exists(b) {
                return b.to_string();
            }
        }
        if let Some(b) = git_out(&self.toplevel, &["config", "--get", "init.defaultBranch"]) {
            let b = b.trim();
            if valid_branch(b) {
                return b.to_string();
            }
        }
        self.head.branch().unwrap_or("main").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pgx-git-repo-{tag}-{}-{}",
            std::process::id(),
            crate::fsutil::now_epoch()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        canon(&d)
    }

    #[test]
    fn discovers_repos_worktrees_heads_and_renames() {
        if !git_available() {
            return;
        }
        let base = tmp("disc");
        let r = base.join("myapp");
        std::fs::create_dir_all(r.join("sub")).unwrap();
        assert!(discover(&r).unwrap().is_none());
        sh(&r, &["init", "-q", "-b", "main"]);
        std::fs::write(r.join("a"), "1").unwrap();
        sh(&r, &["add", "a"]);
        sh(&r, &["commit", "-q", "-m", "one"]);
        let repo = discover(&r.join("sub")).unwrap().unwrap();
        assert_eq!(repo.toplevel, r);
        assert_eq!(repo.head, Head::Branch("main".into()));
        assert!(!repo.linked);
        assert_eq!(repo.display_name(), "myapp");
        assert_eq!(repo.guess_main(), "main");
        assert!(repo.branch_exists("main") && !repo.branch_exists("nope"));
        sh(&r, &["branch", "feature/auth"]);
        sh(&r, &["worktree", "add", "-q", "../wt-auth", "feature/auth"]);
        let wt = discover(&base.join("wt-auth")).unwrap().unwrap();
        assert!(wt.linked);
        assert_eq!(wt.repo_id, repo.repo_id, "one repository");
        assert_ne!(wt.worktree_id, repo.worktree_id, "two worktrees");
        assert_eq!(wt.head, Head::Branch("feature/auth".into()));
        assert!(
            base.join("wt-auth/.git").is_file(),
            "a .git file, not a dir"
        );
        assert_eq!(wt.display_name(), "myapp");
        assert_eq!(wt.hooks_dir(), repo.hooks_dir());
        assert_eq!(
            repo.worktree_of("feature/auth"),
            Some(canon(&base.join("wt-auth")))
        );
        // Rename detection.
        sh(&r, &["branch", "old-name"]);
        sh(&r, &["branch", "-m", "old-name", "new-name"]);
        assert_eq!(repo.renamed_from("new-name").as_deref(), Some("old-name"));
        assert_eq!(repo.renamed_from("main"), None);
        // Detached HEAD.
        sh(&r, &["checkout", "-q", "--detach"]);
        let d = discover(&r).unwrap().unwrap();
        assert!(matches!(d.head, Head::Detached(_)));
        let _ = std::fs::remove_dir_all(&base);
    }
}
