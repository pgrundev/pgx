//! PGX-managed local state for one repository:
//! `$PGX_HOME/git/<repo_id>/state.json` (0600, atomic rename) guarded by
//! `$PGX_HOME/git/<repo_id>/lock` (`flock`). Everything mutable lives here,
//! never in the committed `.pgx.toml`: database names and OIDs, snapshot
//! generations and fingerprints, branch mappings, the last sync result.
//!
//! The lock is a kernel `flock`, so a crashed or killed PGX process can never
//! leave it held: there are no stale-lock deadlocks to break. Operations that
//! outlive a crash (a snapshot being staged, a branch database being created)
//! record their intent with the owner's pid first; `recover` finishes or
//! rolls back intents whose owner is gone.
use crate::cli::ExitCode;
use crate::errors::{PgxError, Result};
use serde_json::{json, Value};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapState {
    Preparing,
    Validating,
    Ready,
    Failed,
    Retired,
}

impl SnapState {
    pub fn as_str(self) -> &'static str {
        match self {
            SnapState::Preparing => "preparing",
            SnapState::Validating => "validating",
            SnapState::Ready => "ready",
            SnapState::Failed => "failed",
            SnapState::Retired => "retired",
        }
    }
    fn parse(s: &str) -> SnapState {
        match s {
            "preparing" => SnapState::Preparing,
            "validating" => SnapState::Validating,
            "ready" => SnapState::Ready,
            "retired" => SnapState::Retired,
            _ => SnapState::Failed,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// Assigned at publication; `None` for attempts that never published.
    pub generation: Option<u64>,
    pub database: String,
    pub staging: String,
    pub oid: u64,
    pub state: SnapState,
    pub owner_pid: u32,
    pub source_database: String,
    pub source_git_revision: Option<String>,
    pub schema_fingerprint: String,
    pub migration_fingerprint: String,
    pub data_fingerprint: Option<String>,
    pub migrations_applied: u64,
    pub tables: u64,
    pub created_at: u64,
    pub method: String,
    pub elapsed_ms: f64,
    /// `ALLOW_CONNECTIONS false` was applied: nobody can modify it.
    pub locked: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Main,
    Branch,
    Detached,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Main => "main",
            Role::Branch => "branch",
            Role::Detached => "detached",
        }
    }
    fn parse(s: &str) -> Role {
        match s {
            "main" => Role::Main,
            "detached" => Role::Detached,
            _ => Role::Branch,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BranchDb {
    pub role: Role,
    /// Empty for the main database (it belongs to the repository, not a worktree).
    pub worktree_id: String,
    pub worktree_path: String,
    /// The Git branch, exactly as Git names it (human-readable metadata).
    pub branch: String,
    pub database: String,
    pub oid: u64,
    /// The main snapshot generation it was cloned from (`None`: not from a snapshot).
    pub base_generation: Option<u64>,
    pub source: String,
    pub created_at: u64,
    pub last_used: u64,
    /// `creating`: an intent written before the clone; `ready` otherwise.
    pub creating: bool,
    pub owner_pid: u32,
    pub method: String,
    pub elapsed_ms: f64,
    pub renamed_from: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SyncRecord {
    pub at: u64,
    /// `published`, `unchanged`, `pending`, `failed`.
    pub status: String,
    pub generation: Option<u64>,
    pub message: String,
    pub pending_migrations: Vec<String>,
    pub elapsed_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRecord {
    pub name: String,
    pub path: String,
    /// `file`: PGX wrote the whole hook. `block`: PGX inserted its marked
    /// block into a hook that already existed.
    pub mode: String,
}

/// A `pgx git reset` in progress: the branch database `target` is moved
/// aside to `old` and replaced by `fresh`. Recorded before the first step so
/// an interrupted reset is finished or undone, never leaked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetIntent {
    pub target: String,
    pub old: String,
    pub old_oid: u64,
    pub fresh: String,
    pub fresh_oid: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RepoState {
    pub repo_id: String,
    pub repo_name: String,
    pub repo_slug: String,
    pub common_dir: String,
    pub main_branch: String,
    pub main: Option<BranchDb>,
    pub snapshots: Vec<Snapshot>,
    pub branches: Vec<BranchDb>,
    pub last_sync: Option<SyncRecord>,
    pub hooks: Vec<HookRecord>,
    /// The migration command text the developer approved for unattended runs.
    pub approved_migrate_command: Option<String>,
    pub cluster_id: String,
    pub initialized_at: u64,
    pub reset: Option<ResetIntent>,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}
fn os(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(String::from)
}
fn u(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}
fn f(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}
fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}

impl Snapshot {
    pub fn to_json(&self) -> Value {
        json!({
            "generation": self.generation, "database": self.database, "staging": self.staging,
            "oid": self.oid, "state": self.state.as_str(), "owner_pid": self.owner_pid,
            "source_database": self.source_database, "source_git_revision": self.source_git_revision,
            "schema_fingerprint": self.schema_fingerprint, "migration_fingerprint": self.migration_fingerprint,
            "data_fingerprint": self.data_fingerprint, "migrations_applied": self.migrations_applied,
            "tables": self.tables, "created_at": self.created_at, "method": self.method,
            "elapsed_ms": self.elapsed_ms, "locked": self.locked, "error": self.error,
        })
    }
    fn from_json(v: &Value) -> Snapshot {
        Snapshot {
            generation: v.get("generation").and_then(Value::as_u64),
            database: s(v, "database"),
            staging: s(v, "staging"),
            oid: u(v, "oid"),
            state: SnapState::parse(&s(v, "state")),
            owner_pid: u(v, "owner_pid") as u32,
            source_database: s(v, "source_database"),
            source_git_revision: os(v, "source_git_revision"),
            schema_fingerprint: s(v, "schema_fingerprint"),
            migration_fingerprint: s(v, "migration_fingerprint"),
            data_fingerprint: os(v, "data_fingerprint"),
            migrations_applied: u(v, "migrations_applied"),
            tables: u(v, "tables"),
            created_at: u(v, "created_at"),
            method: s(v, "method"),
            elapsed_ms: f(v, "elapsed_ms"),
            locked: b(v, "locked"),
            error: os(v, "error"),
        }
    }
}

impl BranchDb {
    pub fn to_json(&self) -> Value {
        json!({
            "role": self.role.as_str(), "worktree_id": self.worktree_id, "worktree_path": self.worktree_path,
            "branch": self.branch, "database": self.database, "oid": self.oid,
            "base_generation": self.base_generation, "source": self.source,
            "created_at": self.created_at, "last_used": self.last_used, "creating": self.creating,
            "owner_pid": self.owner_pid, "method": self.method, "elapsed_ms": self.elapsed_ms,
            "renamed_from": self.renamed_from,
        })
    }
    fn from_json(v: &Value) -> BranchDb {
        BranchDb {
            role: Role::parse(&s(v, "role")),
            worktree_id: s(v, "worktree_id"),
            worktree_path: s(v, "worktree_path"),
            branch: s(v, "branch"),
            database: s(v, "database"),
            oid: u(v, "oid"),
            base_generation: v.get("base_generation").and_then(Value::as_u64),
            source: s(v, "source"),
            created_at: u(v, "created_at"),
            last_used: u(v, "last_used"),
            creating: b(v, "creating"),
            owner_pid: u(v, "owner_pid") as u32,
            method: s(v, "method"),
            elapsed_ms: f(v, "elapsed_ms"),
            renamed_from: os(v, "renamed_from"),
        }
    }
}

impl SyncRecord {
    pub fn to_json(&self) -> Value {
        json!({
            "at": crate::fsutil::rfc3339(self.at), "at_epoch": self.at, "status": self.status,
            "generation": self.generation, "message": self.message,
            "pending_migrations": self.pending_migrations, "elapsed_ms": self.elapsed_ms,
        })
    }
    fn from_json(v: &Value) -> SyncRecord {
        SyncRecord {
            at: u(v, "at_epoch"),
            status: s(v, "status"),
            generation: v.get("generation").and_then(Value::as_u64),
            message: s(v, "message"),
            pending_migrations: v
                .get("pending_migrations")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            elapsed_ms: f(v, "elapsed_ms"),
        }
    }
}

impl RepoState {
    pub fn to_json(&self) -> Value {
        json!({
            "schema": 1,
            "repo_id": self.repo_id, "repo_name": self.repo_name, "repo_slug": self.repo_slug,
            "common_dir": self.common_dir, "main_branch": self.main_branch,
            "main": self.main.as_ref().map(BranchDb::to_json),
            "snapshots": self.snapshots.iter().map(Snapshot::to_json).collect::<Vec<_>>(),
            "branches": self.branches.iter().map(BranchDb::to_json).collect::<Vec<_>>(),
            "last_sync": self.last_sync.as_ref().map(SyncRecord::to_json),
            "hooks": self.hooks.iter().map(|h| json!({"name": h.name, "path": h.path, "mode": h.mode})).collect::<Vec<_>>(),
            "approved_migrate_command": self.approved_migrate_command,
            "cluster_id": self.cluster_id, "initialized_at": self.initialized_at,
            "reset": self.reset.as_ref().map(|r| json!({"target": r.target, "old": r.old, "old_oid": r.old_oid, "fresh": r.fresh, "fresh_oid": r.fresh_oid})),
        })
    }

    pub fn from_json(text: &str) -> Result<RepoState> {
        let bad = |m: &str| PgxError::new(ExitCode::Corrupt, format!("PGX Git state: {m}"));
        let v: Value = serde_json::from_str(text).map_err(|e| bad(&e.to_string()))?;
        if v.get("schema").and_then(Value::as_u64) != Some(1) {
            return Err(bad("missing or unsupported schema version"));
        }
        let arr = |k: &str| {
            v.get(k)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        Ok(RepoState {
            repo_id: s(&v, "repo_id"),
            repo_name: s(&v, "repo_name"),
            repo_slug: s(&v, "repo_slug"),
            common_dir: s(&v, "common_dir"),
            main_branch: s(&v, "main_branch"),
            main: v
                .get("main")
                .filter(|m| m.is_object())
                .map(BranchDb::from_json),
            snapshots: arr("snapshots").iter().map(Snapshot::from_json).collect(),
            branches: arr("branches").iter().map(BranchDb::from_json).collect(),
            last_sync: v
                .get("last_sync")
                .filter(|m| m.is_object())
                .map(SyncRecord::from_json),
            hooks: arr("hooks")
                .iter()
                .map(|h| HookRecord {
                    name: s(h, "name"),
                    path: s(h, "path"),
                    mode: s(h, "mode"),
                })
                .collect(),
            approved_migrate_command: os(&v, "approved_migrate_command"),
            cluster_id: s(&v, "cluster_id"),
            initialized_at: u(&v, "initialized_at"),
            reset: v
                .get("reset")
                .filter(|r| r.is_object())
                .map(|r| ResetIntent {
                    target: s(r, "target"),
                    old: s(r, "old"),
                    old_oid: u(r, "old_oid"),
                    fresh: s(r, "fresh"),
                    fresh_oid: r.get("fresh_oid").and_then(Value::as_u64),
                }),
        })
    }

    /// The published baseline new branches clone from.
    pub fn current(&self) -> Option<&Snapshot> {
        self.snapshots
            .iter()
            .filter(|s| s.state == SnapState::Ready && s.generation.is_some())
            .max_by_key(|s| s.generation)
    }

    pub fn current_generation(&self) -> Option<u64> {
        self.current().and_then(|s| s.generation)
    }

    pub fn next_generation(&self) -> u64 {
        self.snapshots
            .iter()
            .filter_map(|s| s.generation)
            .max()
            .unwrap_or(0)
            + 1
    }

    pub fn find_branch(&self, worktree_id: &str, branch: &str, role: Role) -> Option<usize> {
        self.branches.iter().position(|b| {
            b.worktree_id == worktree_id && b.branch == branch && b.role == role && !b.creating
        })
    }

    /// Every database name this repository's state refers to.
    pub fn owns_database(&self, name: &str) -> bool {
        self.main.as_ref().is_some_and(|m| m.database == name)
            || self.branches.iter().any(|b| b.database == name)
            || self
                .snapshots
                .iter()
                .any(|s| s.database == name || s.staging == name)
    }
}

/// `$PGX_HOME/git`.
pub fn root(home: &Path) -> PathBuf {
    home.join("git")
}

pub fn dir(home: &Path, repo_id: &str) -> PathBuf {
    root(home).join(repo_id)
}

fn private_dir(d: &Path) -> Result<()> {
    std::fs::create_dir_all(d).map_err(|e| PgxError::io("cannot create", d, &e))?;
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
    Ok(())
}

pub fn load(home: &Path, repo_id: &str) -> Result<Option<RepoState>> {
    let p = dir(home, repo_id).join("state.json");
    match std::fs::read_to_string(&p) {
        Ok(t) => Ok(Some(RepoState::from_json(&t).map_err(|e| {
            PgxError::new(e.code, format!("{} ({})", e.message, p.display()))
                .hint("the file is left untouched; move it aside to start over (`pgx git doctor` explains)")
        })?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(PgxError::io("cannot read", &p, &e)),
    }
}

pub fn save(home: &Path, st: &RepoState) -> Result<()> {
    let d = dir(home, &st.repo_id);
    private_dir(&root(home))?;
    private_dir(&d)?;
    let p = d.join("state.json");
    let tmp = d.join(format!("state.json.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(&st.to_json()).expect("state serialises");
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| PgxError::io("cannot write", &tmp, &e))?;
        f.write_all(text.as_bytes())
            .and_then(|_| f.sync_all())
            .map_err(|e| PgxError::io("cannot write", &tmp, &e))?;
    }
    std::fs::rename(&tmp, &p).map_err(|e| PgxError::io("cannot replace", &p, &e))
}

/// A held repository lock. Released when dropped (or when the process dies).
pub struct Lock(#[allow(dead_code)] std::fs::File);

fn open_lock(home: &Path, repo_id: &str, name: &str) -> Result<(std::fs::File, PathBuf)> {
    let d = dir(home, repo_id);
    private_dir(&root(home))?;
    private_dir(&d)?;
    let p = d.join(name);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&p)
        .map_err(|e| PgxError::io("cannot open", &p, &e))?;
    Ok((file, p))
}

/// One non-blocking attempt: `Ok(None)` when somebody else holds it.
fn try_flock(file: &std::fs::File, p: &Path) -> Result<bool> {
    // SAFETY: flock on a descriptor we own.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
        return Err(PgxError::failure(format!(
            "cannot lock {}: {err}",
            p.display()
        )));
    }
    Ok(false)
}

fn lock_named(home: &Path, repo_id: &str, name: &str, wait: Duration, what: &str) -> Result<Lock> {
    let (file, p) = open_lock(home, repo_id, name)?;
    let deadline = Instant::now() + wait;
    loop {
        if try_flock(&file, &p)? {
            return Ok(Lock(file));
        }
        if Instant::now() >= deadline {
            return Err(PgxError::new(
                ExitCode::InUse,
                format!("{what} (waited {}s)", wait.as_secs()),
            )
            .hint("it finishes on its own; retry, or see `pgx git status`"));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The state lock: every read-modify-write of `state.json` and every short
/// database step (a clone, a rename, a drop) happens under it. It is never
/// held across a slow copy, so hooks and agents only ever wait briefly.
pub fn lock(home: &Path, repo_id: &str, wait: Duration) -> Result<Lock> {
    lock_named(
        home,
        repo_id,
        "lock",
        wait,
        "another PGX Git operation on this repository is still running",
    )
}

/// The sync lock: held for the whole of one snapshot (staging, copy,
/// validation, publication). Taken *before* the state lock, never after.
pub fn sync_lock(home: &Path, repo_id: &str, wait: Duration) -> Result<Lock> {
    lock_named(
        home,
        repo_id,
        "sync.lock",
        wait,
        "another main snapshot of this repository is being taken",
    )
}

/// The sync lock if it is free right now, else `None` (automatic syncs skip
/// instead of waiting).
pub fn try_sync_lock(home: &Path, repo_id: &str) -> Result<Option<Lock>> {
    let (file, p) = open_lock(home, repo_id, "sync.lock")?;
    Ok(try_flock(&file, &p)?.then_some(Lock(file)))
}

/// Is a snapshot being taken by a live process? Under the state lock this
/// is the whole truth for crash recovery: with the sync lock free, any
/// in-progress snapshot record belongs to a process that is gone (the kernel
/// released its lock), whatever its pid is now used for.
pub fn sync_running(home: &Path, repo_id: &str) -> Result<bool> {
    Ok(try_sync_lock(home, repo_id)?.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RepoState {
        RepoState {
            repo_id: "0123456789abcdef".into(),
            repo_name: "myapp".into(),
            repo_slug: "myapp".into(),
            common_dir: "/w/myapp/.git".into(),
            main_branch: "main".into(),
            main: Some(BranchDb {
                role: Role::Main,
                worktree_id: String::new(),
                worktree_path: String::new(),
                branch: "main".into(),
                database: "pgx_myapp_main_abc123".into(),
                oid: 16384,
                base_generation: None,
                source: "app".into(),
                created_at: 1,
                last_used: 2,
                creating: false,
                owner_pid: 0,
                method: "clone".into(),
                elapsed_ms: 1.5,
                renamed_from: None,
            }),
            snapshots: vec![
                Snapshot {
                    generation: Some(1),
                    database: "s1".into(),
                    staging: "stg1".into(),
                    oid: 1,
                    state: SnapState::Ready,
                    owner_pid: 0,
                    source_database: "m".into(),
                    source_git_revision: Some("abc".into()),
                    schema_fingerprint: "f1".into(),
                    migration_fingerprint: "m1".into(),
                    data_fingerprint: None,
                    migrations_applied: 3,
                    tables: 4,
                    created_at: 10,
                    method: "clone".into(),
                    elapsed_ms: 2.0,
                    locked: true,
                    error: None,
                },
                Snapshot {
                    generation: Some(2),
                    database: "s2".into(),
                    staging: "stg2".into(),
                    oid: 2,
                    state: SnapState::Ready,
                    owner_pid: 0,
                    source_database: "m".into(),
                    source_git_revision: None,
                    schema_fingerprint: "f2".into(),
                    migration_fingerprint: "m2".into(),
                    data_fingerprint: Some("d".into()),
                    migrations_applied: 4,
                    tables: 5,
                    created_at: 20,
                    method: "dump".into(),
                    elapsed_ms: 3.0,
                    locked: true,
                    error: None,
                },
                Snapshot {
                    generation: None,
                    database: String::new(),
                    staging: "stg3".into(),
                    oid: 0,
                    state: SnapState::Failed,
                    owner_pid: 9,
                    source_database: "m".into(),
                    source_git_revision: None,
                    schema_fingerprint: String::new(),
                    migration_fingerprint: String::new(),
                    data_fingerprint: None,
                    migrations_applied: 0,
                    tables: 0,
                    created_at: 30,
                    method: String::new(),
                    elapsed_ms: 0.0,
                    locked: false,
                    error: Some("validation failed".into()),
                },
            ],
            branches: Vec::new(),
            last_sync: Some(SyncRecord {
                at: 5,
                status: "failed".into(),
                generation: Some(2),
                message: "boom".into(),
                pending_migrations: vec!["2024".into()],
                elapsed_ms: 1.0,
            }),
            hooks: vec![HookRecord {
                name: "post-checkout".into(),
                path: "/w/.git/hooks/post-checkout".into(),
                mode: "block".into(),
            }],
            approved_migrate_command: None,
            cluster_id: "c".into(),
            initialized_at: 1,
            reset: Some(ResetIntent {
                target: "t".into(),
                old: "o".into(),
                old_oid: 3,
                fresh: "f".into(),
                fresh_oid: None,
            }),
        }
    }

    #[test]
    fn state_roundtrips_and_picks_the_newest_ready_snapshot() {
        let st = sample();
        let back = RepoState::from_json(&st.to_json().to_string()).unwrap();
        assert_eq!(back, st);
        assert_eq!(st.current_generation(), Some(2));
        assert_eq!(st.next_generation(), 3);
        assert!(st.owns_database("stg3") && st.owns_database("pgx_myapp_main_abc123"));
        assert!(!st.owns_database("app"));
        assert!(RepoState::from_json("{\"schema\": 9}").is_err());
        assert!(RepoState::from_json("not json").is_err());
    }

    #[test]
    fn save_is_private_and_lock_times_out_while_held() {
        let home = std::env::temp_dir().join(format!("pgx-git-state-{}", std::process::id()));
        let st = sample();
        save(&home, &st).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let p = dir(&home, &st.repo_id).join("state.json");
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(load(&home, &st.repo_id).unwrap().unwrap(), st);
        assert!(load(&home, "ffffffffffffffff").unwrap().is_none());
        let held = lock(&home, &st.repo_id, Duration::from_millis(10)).unwrap();
        // flock is per open file description: a second open in this process contends.
        let e = lock(&home, &st.repo_id, Duration::from_millis(50))
            .err()
            .unwrap();
        assert_eq!(e.code, ExitCode::InUse);
        drop(held);
        // A child forked by a parallel test may briefly share the descriptor
        // until it execs (close-on-exec): allow for that window.
        assert!(lock(&home, &st.repo_id, Duration::from_secs(5)).is_ok());
        let _ = std::fs::remove_dir_all(&home);
    }
}
