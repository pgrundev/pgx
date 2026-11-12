//! The normalized snapshot `pgx top` renders and `pgx --json top --snapshot`
//! prints. Built by the live collector (`collect.rs`) or the simulator
//! (`demo.rs`); everything downstream — tree, events, UI, JSON — reads only
//! this model, so it is tested without a terminal or a runtime.
//!
//! Three separate concepts, never conflated:
//! - [`DbState`]: what the catalog says about the database right now
//!   (client connections, TTL deletion in progress). Zero connections is
//!   `Idle`, not "finished".
//! - [`Lifecycle`]: the owning agent session (`AgentMeta.retired`, the
//!   deadline). `Retired` means the owner ended, not that its task succeeded.
//! - [`TaskPhase`]: only what an agent explicitly reported with
//!   `pgx agent activity`; `None` otherwise.
use serde_json::{json, Value};

/// Version of the `--json top --snapshot` object. Additive fields keep the
/// version; a removed or retyped field bumps it.
pub const SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Live,
    Demo,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Live => "live",
            Mode::Demo => "demo",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStatus {
    Running,
    /// Initialised, no engine running.
    Stopped,
    NotInitialized,
    /// A running pid, but the socket or a catalog query failed.
    Disconnected,
    /// The installation or configuration could not be read.
    Error,
}

impl RuntimeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeStatus::Running => "running",
            RuntimeStatus::Stopped => "stopped",
            RuntimeStatus::NotInitialized => "not_initialized",
            RuntimeStatus::Disconnected => "disconnected",
            RuntimeStatus::Error => "error",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            RuntimeStatus::Running => "RUNNING",
            RuntimeStatus::Stopped => "STOPPED",
            RuntimeStatus::NotInitialized => "NOT INITIALISED",
            RuntimeStatus::Disconnected => "DISCONNECTED",
            RuntimeStatus::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtime {
    pub status: RuntimeStatus,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub uptime_seconds: Option<u64>,
    /// Resident memory of the whole engine process (all databases together).
    pub rss_bytes: Option<u64>,
    /// Physical memory of the machine, for the meter's scale.
    pub phys_mem_bytes: Option<u64>,
    /// Whether `pgrust_runtime_status()` answered.
    pub runtime_status_available: bool,
}

impl Runtime {
    pub fn empty(status: RuntimeStatus) -> Runtime {
        Runtime {
            status,
            pid: None,
            port: None,
            uptime_seconds: None,
            rss_bytes: None,
            phys_mem_bytes: None,
            runtime_status_available: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbState {
    /// One or more client connections (the monitor's own excluded).
    Connected,
    /// No client connections. Says nothing about whether an agent is done.
    Idle,
    /// The TTL worker is expiring or deleting it.
    Expiring,
    /// A TTL deletion failed and will be retried.
    DeleteRetry,
    /// `datconnlimit = -2`: an interrupted CREATE/DROP left it unusable.
    Invalid,
    /// Connection counts were not available.
    Unknown,
}

impl DbState {
    pub fn as_str(self) -> &'static str {
        match self {
            DbState::Connected => "connected",
            DbState::Idle => "idle",
            DbState::Expiring => "expiring",
            DbState::DeleteRetry => "delete_retry",
            DbState::Invalid => "invalid",
            DbState::Unknown => "unknown",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            DbState::Connected => "CONNECTED",
            DbState::Idle => "IDLE",
            DbState::Expiring => "EXPIRING",
            DbState::DeleteRetry => "DEL_RETRY",
            DbState::Invalid => "INVALID",
            DbState::Unknown => "UNKNOWN",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// The owning session is alive: the deadline is its idle TTL.
    Active,
    /// The owner ended (SessionEnd/SubagentStop/`down`): the grace period runs.
    Retired,
    /// Past its deadline, waiting for the TTL worker.
    Expired,
}

impl Lifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Lifecycle::Active => "active",
            Lifecycle::Retired => "retired",
            Lifecycle::Expired => "expired",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Lifecycle::Active => "ACTIVE",
            Lifecycle::Retired => "RETIRED",
            Lifecycle::Expired => "EXPIRED",
        }
    }
}

/// A task phase an agent reported explicitly with `pgx agent activity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    Working,
    Migrating,
    Testing,
    Completed,
    Failed,
}

impl Phase {
    pub const ALL: [Phase; 5] = [
        Phase::Working,
        Phase::Migrating,
        Phase::Testing,
        Phase::Completed,
        Phase::Failed,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Working => "working",
            Phase::Migrating => "migrating",
            Phase::Testing => "testing",
            Phase::Completed => "completed",
            Phase::Failed => "failed",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Phase::Working => "WORKING",
            Phase::Migrating => "MIGRATING",
            Phase::Testing => "TESTING",
            Phase::Completed => "COMPLETED",
            Phase::Failed => "FAILED",
        }
    }
    pub fn parse(s: &str) -> Option<Phase> {
        Phase::ALL.into_iter().find(|p| p.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPhase {
    pub phase: Phase,
    pub label: Option<String>,
    pub reported_at: u64,
    /// Who reported it: `pgx agent activity` for real reports, `demo` in the simulator.
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// An agent session's own branch (the parent).
    Session,
    /// A subagent's branch inside a session.
    Subagent,
    /// PGX Git: the main branch's database.
    GitMain,
    /// PGX Git: a feature branch's database.
    GitBranch,
    /// PGX Git: an immutable main snapshot.
    GitSnapshot,
    /// Anything else (`pgx create`, `pgx branch`, the source templates).
    Standalone,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Session => "session",
            Role::Subagent => "subagent",
            Role::GitMain => "git_main",
            Role::GitBranch => "git_branch",
            Role::GitSnapshot => "git_snapshot",
            Role::Standalone => "standalone",
        }
    }
}

/// What the deadline means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineKind {
    /// An agent branch: idle TTL after the last activity.
    IdleTtl,
    /// A retired agent branch: the grace period after the session ended.
    Grace,
    /// `pgx create/clone/branch --ttl`: a fixed deadline.
    Fixed,
}

impl DeadlineKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DeadlineKind::IdleTtl => "idle_ttl",
            DeadlineKind::Grace => "grace",
            DeadlineKind::Fixed => "fixed",
        }
    }
}

/// The owning agent session, from a TTL record that matched this database's
/// OID and cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub host: String,
    pub session_id: String,
    pub agent_id: Option<String>,
    pub project_id: String,
    pub project_root: String,
    /// A readable project name (the project directory's name).
    pub project_name: String,
    pub source: String,
    /// The last hook heartbeat (epoch). Not a claim about SQL or coding activity.
    pub last_touch: Option<u64>,
    pub retired: bool,
    pub git_scope: Option<String>,
    pub git_branch: Option<String>,
    /// The worktree of the Git branch the session works on, when PGX Git knows it.
    pub worktree: Option<String>,
}

/// PGX Git ownership of a Git-managed database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitInfo {
    pub repo_id: String,
    pub repo_name: String,
    pub branch: String,
    pub worktree_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Database {
    pub oid: u64,
    pub name: String,
    pub is_template: bool,
    pub invalid: bool,
    /// Client connections, the monitor's own excluded. `None` = unknown.
    pub connections: Option<u64>,
    /// `pg_database_size`: the logical size, not copy-on-write disk usage.
    pub size_bytes: Option<u64>,
    pub created_at: Option<u64>,
    pub ttl_label: Option<String>,
    pub ttl_seconds: Option<u64>,
    pub expires_at: Option<u64>,
    /// The TTL record's state (`active`, `expiring`, `deleting`, `retry_pending`).
    pub ttl_state: Option<String>,
    pub last_error: Option<String>,
    pub deadline_kind: Option<DeadlineKind>,
    pub agent: Option<Agent>,
    pub git: Option<GitInfo>,
    pub phase: Option<TaskPhase>,
    pub db_state: DbState,
    pub lifecycle: Option<Lifecycle>,
    pub role: Role,
}

impl Database {
    /// The stable identity within one cluster: the OID never repeats for a
    /// live incarnation, and a recreated database gets a new one.
    pub fn id(&self) -> String {
        format!("db:{}", self.oid)
    }

    /// Seconds until the deadline at `now` (0 when past due; `None` = unlimited).
    pub fn remaining(&self, now: u64) -> Option<u64> {
        self.expires_at.map(|e| e.saturating_sub(now))
    }

    pub fn to_json(&self, now: u64) -> Value {
        let a = self.agent.as_ref();
        json!({
            "name": self.name,
            "oid": self.oid,
            "role": self.role.as_str(),
            "project_id": a.map(|a| a.project_id.clone()),
            "project": a.map(|a| a.project_name.clone()).or_else(|| self.git.as_ref().map(|g| g.repo_name.clone())),
            "project_root": a.map(|a| a.project_root.clone()),
            "git_branch": a.and_then(|a| a.git_branch.clone()).or_else(|| self.git.as_ref().map(|g| g.branch.clone())),
            "git_scope": a.and_then(|a| a.git_scope.clone()),
            "worktree": a.and_then(|a| a.worktree.clone()).or_else(|| self.git.as_ref().map(|g| g.worktree_path.clone())),
            "host": a.map(|a| a.host.clone()),
            "session_id": a.map(|a| a.session_id.clone()),
            "agent_id": a.and_then(|a| a.agent_id.clone()),
            "parent_database": a.map(|a| a.source.clone()),
            "connections": self.connections,
            "size_bytes": self.size_bytes,
            "is_template": self.is_template,
            "created_at": self.created_at.map(crate::fsutil::rfc3339),
            "age_seconds": self.created_at.map(|c| now.saturating_sub(c)),
            "last_touch": a.and_then(|a| a.last_touch).map(crate::fsutil::rfc3339),
            "ttl": self.ttl_label,
            "ttl_seconds": self.ttl_seconds,
            "expires_at": self.expires_at.map(crate::fsutil::rfc3339),
            "expires_in_seconds": self.remaining(now),
            "deadline_kind": self.deadline_kind.map(|d| d.as_str()),
            "ttl_state": self.ttl_state,
            "last_error": self.last_error,
            "db_state": self.db_state.as_str(),
            "lifecycle": self.lifecycle.map(|l| l.as_str()),
            "task_phase": self.phase.as_ref().map(|p| json!({
                "state": p.phase.as_str(),
                "label": p.label,
                "reported_at": crate::fsutil::rfc3339(p.reported_at),
                "source": p.source,
            })),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Counted in ISSUES: something observed is wrong or failing.
    Issue,
    /// Context only (a metric is unavailable, stale metadata was ignored).
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    pub database: Option<String>,
}

impl Warning {
    pub fn issue(code: &'static str, message: impl Into<String>) -> Warning {
        Warning {
            severity: Severity::Issue,
            code,
            message: message.into(),
            database: None,
        }
    }
    pub fn info(code: &'static str, message: impl Into<String>) -> Warning {
        Warning {
            severity: Severity::Info,
            code,
            message: message.into(),
            database: None,
        }
    }
    pub fn on(mut self, db: &str) -> Warning {
        self.database = Some(db.to_string());
        self
    }
    pub fn to_json(&self) -> Value {
        json!({
            "severity": match self.severity { Severity::Issue => "issue", Severity::Info => "info" },
            "code": self.code,
            "message": self.message,
            "database": self.database,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub databases: Option<usize>,
    pub agent_databases: Option<usize>,
    pub sessions: Option<usize>,
    pub client_connections: Option<u64>,
    pub issues: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopSnapshot {
    pub mode: Mode,
    pub collected_at: u64,
    pub runtime: Runtime,
    /// `None` when the catalog could not be read (runtime stopped or
    /// unreachable): unknown, which is not the same as "no databases".
    pub databases: Option<Vec<Database>>,
    pub warnings: Vec<Warning>,
}

impl TopSnapshot {
    pub fn summary(&self) -> Summary {
        let issues = self
            .warnings
            .iter()
            .filter(|w| w.severity == Severity::Issue)
            .count();
        match &self.databases {
            None => Summary {
                databases: None,
                agent_databases: None,
                sessions: None,
                client_connections: None,
                issues,
            },
            Some(dbs) => {
                let mut sessions: Vec<(&str, &str, &str)> = dbs
                    .iter()
                    .filter_map(|d| d.agent.as_ref())
                    .map(|a| {
                        (
                            a.project_id.as_str(),
                            a.host.as_str(),
                            a.session_id.as_str(),
                        )
                    })
                    .collect();
                sessions.sort();
                sessions.dedup();
                let conns = if dbs.iter().any(|d| d.connections.is_none()) {
                    None
                } else {
                    Some(dbs.iter().filter_map(|d| d.connections).sum())
                };
                Summary {
                    databases: Some(dbs.len()),
                    agent_databases: Some(dbs.iter().filter(|d| d.agent.is_some()).count()),
                    sessions: Some(sessions.len()),
                    client_connections: conns,
                    issues,
                }
            }
        }
    }

    pub fn to_json(&self) -> Value {
        let s = self.summary();
        let now = self.collected_at;
        json!({
            "schema_version": SCHEMA_VERSION,
            "mode": self.mode.as_str(),
            "simulated": self.mode == Mode::Demo,
            "collected_at": crate::fsutil::rfc3339(now),
            "pgx_version": env!("CARGO_PKG_VERSION"),
            "runtime": {
                "status": self.runtime.status.as_str(),
                "running": self.runtime.status == RuntimeStatus::Running,
                "pid": self.runtime.pid,
                "port": self.runtime.port,
                "uptime_seconds": self.runtime.uptime_seconds,
                "rss_bytes": self.runtime.rss_bytes,
                "rss_scope": "whole runtime process",
                "cpu_percent": Value::Null,
                "runtime_status_available": self.runtime.runtime_status_available,
            },
            "summary": {
                "databases": s.databases,
                "agent_databases": s.agent_databases,
                "sessions": s.sessions,
                "client_connections": s.client_connections,
                "issues": s.issues,
            },
            "databases": self.databases.as_ref().map(|d| d.iter().map(|x| x.to_json(now)).collect::<Vec<_>>()),
            "warnings": self.warnings.iter().map(Warning::to_json).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
pub mod fixtures {
    //! Builders for unit tests across the `top` modules.
    use super::*;

    pub const NOW: u64 = 1_794_427_200; // 2026-11-11T20:00:00Z

    pub fn standalone(oid: u64, name: &str) -> Database {
        Database {
            oid,
            name: name.to_string(),
            is_template: false,
            invalid: false,
            connections: Some(0),
            size_bytes: Some(8_000_000),
            created_at: None,
            ttl_label: None,
            ttl_seconds: None,
            expires_at: None,
            ttl_state: None,
            last_error: None,
            deadline_kind: None,
            agent: None,
            git: None,
            phase: None,
            db_state: DbState::Idle,
            lifecycle: None,
            role: Role::Standalone,
        }
    }

    pub fn agent_db(
        oid: u64,
        name: &str,
        project: &str,
        host: &str,
        session: &str,
        agent: Option<&str>,
    ) -> Database {
        let mut d = standalone(oid, name);
        d.role = if agent.is_some() {
            Role::Subagent
        } else {
            Role::Session
        };
        d.created_at = Some(NOW - 600);
        d.ttl_label = Some("2h".into());
        d.ttl_seconds = Some(7200);
        d.expires_at = Some(NOW + 6600);
        d.ttl_state = Some("active".into());
        d.deadline_kind = Some(DeadlineKind::IdleTtl);
        d.lifecycle = Some(Lifecycle::Active);
        d.agent = Some(Agent {
            host: host.into(),
            session_id: session.into(),
            agent_id: agent.map(String::from),
            project_id: format!("{project}-id"),
            project_root: format!("/work/{project}"),
            project_name: project.into(),
            source: "app".into(),
            last_touch: Some(NOW - 30),
            retired: false,
            git_scope: None,
            git_branch: None,
            worktree: None,
        });
        d
    }

    pub fn snap(dbs: Vec<Database>) -> TopSnapshot {
        TopSnapshot {
            mode: Mode::Live,
            collected_at: NOW,
            runtime: Runtime {
                status: RuntimeStatus::Running,
                pid: Some(4242),
                port: Some(5433),
                uptime_seconds: Some(100),
                rss_bytes: Some(200 << 20),
                phys_mem_bytes: Some(16 << 30),
                runtime_status_available: true,
            },
            databases: Some(dbs),
            warnings: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn json_distinguishes_unknown_from_zero_and_unavailable_from_empty() {
        let mut d = standalone(5, "postgres");
        d.connections = None;
        d.size_bytes = None;
        let v = snap(vec![d]).to_json();
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["databases"][0]["connections"], Value::Null);
        assert_eq!(v["databases"][0]["size_bytes"], Value::Null);
        assert_eq!(v["databases"][0]["task_phase"], Value::Null);
        assert_eq!(v["summary"]["client_connections"], Value::Null);
        let empty = snap(vec![]).to_json();
        assert_eq!(empty["databases"], json!([]));
        assert_eq!(empty["summary"]["databases"], 0);
        let mut down = snap(vec![]);
        down.databases = None;
        down.runtime = Runtime::empty(RuntimeStatus::Stopped);
        let v = down.to_json();
        assert_eq!(v["databases"], Value::Null);
        assert_eq!(v["summary"]["databases"], Value::Null);
        assert_eq!(v["runtime"]["running"], false);
        assert_eq!(v["runtime"]["pid"], Value::Null);
        assert_eq!(v["runtime"]["cpu_percent"], Value::Null);
    }

    #[test]
    fn summary_counts_sessions_per_project_and_host() {
        let dbs = vec![
            agent_db(10, "a1", "p1", "claude", "s1", None),
            agent_db(11, "a2", "p1", "claude", "s1", Some("x")),
            agent_db(12, "a3", "p2", "claude", "s1", None), // same session id, other project
            agent_db(13, "a4", "p1", "codex", "s1", None),  // same session id, other host
            standalone(5, "postgres"),
        ];
        let s = snap(dbs).summary();
        assert_eq!(s.databases, Some(5));
        assert_eq!(s.agent_databases, Some(4));
        assert_eq!(s.sessions, Some(3));
        assert_eq!(s.client_connections, Some(0));
    }

    #[test]
    fn phases_parse_only_known_values() {
        for p in Phase::ALL {
            assert_eq!(Phase::parse(p.as_str()), Some(p));
        }
        for bad in ["", "TESTING", "done", "testing ", "complete"] {
            assert_eq!(Phase::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn remaining_never_goes_negative() {
        let mut d = standalone(1, "x");
        assert_eq!(d.remaining(NOW), None);
        d.expires_at = Some(NOW - 50);
        assert_eq!(d.remaining(NOW), Some(0));
        d.expires_at = Some(NOW + 50);
        assert_eq!(d.remaining(NOW), Some(50));
    }
}
