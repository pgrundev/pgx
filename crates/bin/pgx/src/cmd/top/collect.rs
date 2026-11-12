//! Read-only collection of live PGX state for `pgx top`.
//!
//! [`build`] is the pure join: catalog rows + TTL records + PGX Git state +
//! phase reports → [`TopSnapshot`]. Metadata is attached to a database only
//! when its cluster identity and OID match the live catalog row, so stale,
//! foreign or recycled-name metadata is reported as a warning and never
//! shown as authoritative (and nothing here can delete anything).
//!
//! [`LiveCollector`] gathers the inputs with one persistent connection
//! (application_name `pgx-top`, excluded from the connection counts), bounded
//! queries per refresh, and throttled size sampling. It never writes: no SQL
//! besides SELECTs, no file it creates or rewrites.
use super::model::*;
use super::phase::{self, PhaseRecord};
use super::text::sanitize;
use crate::cmd::git::state::{self as gitstate, RepoState};
use crate::ttlstore::{MetaStore, State as TtlState, TtlRecord};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

type Client = crate::pgwire::Client<std::os::unix::net::UnixStream>;

/// One `pg_database` row with its client-connection count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRow {
    pub oid: u64,
    pub name: String,
    pub is_template: bool,
    pub invalid: bool,
    pub connections: Option<u64>,
}

/// Everything [`build`] joins. Assembled by the live collector or by tests.
#[derive(Debug, Clone)]
pub struct Inputs {
    pub now: u64,
    pub cluster_id: String,
    pub runtime: Runtime,
    /// `None`: the catalog could not be read.
    pub catalog: Option<Vec<CatalogRow>>,
    /// Logical sizes by OID (sampled less often than the catalog).
    pub sizes: HashMap<u64, u64>,
    pub ttl_records: Vec<TtlRecord>,
    pub ttl_bad: Vec<(PathBuf, String)>,
    pub git: Vec<RepoState>,
    pub git_bad: Vec<String>,
    pub phases: Vec<PhaseRecord>,
    pub phase_bad: usize,
    /// Collector problems observed this refresh (already sanitized).
    pub errors: Vec<Warning>,
}

impl Inputs {
    pub fn new(now: u64, cluster_id: &str, runtime: Runtime) -> Inputs {
        Inputs {
            now,
            cluster_id: cluster_id.to_string(),
            runtime,
            catalog: None,
            sizes: HashMap::new(),
            ttl_records: Vec::new(),
            ttl_bad: Vec::new(),
            git: Vec::new(),
            git_bad: Vec::new(),
            phases: Vec::new(),
            phase_bad: 0,
            errors: Vec::new(),
        }
    }
}

/// The databases a user never thinks of as theirs.
fn is_system(name: &str) -> bool {
    matches!(name, "template0" | "template1")
}

/// `remotely` for `/home/me/src/remotely`; the id prefix when there is no root.
pub fn project_name(root: &str, project_id: &str) -> String {
    let base = Path::new(root)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty());
    match base {
        Some(b) => sanitize(&b),
        None => {
            let id: String = project_id.chars().take(8).collect();
            if id.is_empty() {
                "(unknown project)".into()
            } else {
                format!("project {}", sanitize(&id))
            }
        }
    }
}

/// Where PGX Git puts a database, by name: (repo, branch db record) for
/// states of *this* cluster.
struct GitEntry<'a> {
    st: &'a RepoState,
    role: Role,
    branch: String,
    worktree: String,
    oid: u64,
    /// Only a snapshot's staging database has no recorded OID.
    oid_optional: bool,
}

impl GitEntry<'_> {
    fn verifies(&self, live_oid: u64) -> bool {
        (self.oid_optional && self.oid == 0) || (self.oid != 0 && self.oid == live_oid)
    }
}

struct GitIndex<'a> {
    by_db: HashMap<&'a str, GitEntry<'a>>,
}

fn git_index<'a>(
    states: &'a [RepoState],
    cluster_id: &str,
    warnings: &mut Vec<Warning>,
) -> GitIndex<'a> {
    let mut by_db = HashMap::new();
    for st in states {
        if st.cluster_id != cluster_id {
            warnings.push(Warning::info(
                "git_state_foreign_cluster",
                format!(
                    "PGX Git state for {} belongs to another cluster incarnation; ignored",
                    sanitize(&st.repo_name)
                ),
            ));
            continue;
        }
        let entry = |role, branch: &str, worktree: &str, oid, oid_optional| GitEntry {
            st,
            role,
            branch: branch.to_string(),
            worktree: worktree.to_string(),
            oid,
            oid_optional,
        };
        if let Some(m) = &st.main {
            by_db.insert(
                m.database.as_str(),
                entry(Role::GitMain, &m.branch, &m.worktree_path, m.oid, false),
            );
        }
        for b in &st.branches {
            let role = match b.role {
                gitstate::Role::Main => Role::GitMain,
                _ => Role::GitBranch,
            };
            by_db.insert(
                b.database.as_str(),
                entry(role, &b.branch, &b.worktree_path, b.oid, false),
            );
        }
        for s in &st.snapshots {
            let label = match s.generation {
                Some(g) => format!("{} snapshot g{g}", st.main_branch),
                None => format!("{} snapshot (staging)", st.main_branch),
            };
            by_db.insert(
                s.database.as_str(),
                entry(Role::GitSnapshot, &label, "", s.oid, false),
            );
            if !s.staging.is_empty() {
                by_db.insert(
                    s.staging.as_str(),
                    entry(Role::GitSnapshot, &label, "", 0, true),
                );
            }
        }
    }
    GitIndex { by_db }
}

/// The pure join. Deterministic: rows sorted by name, warnings in a fixed order.
pub fn build(inp: Inputs) -> TopSnapshot {
    let mut warnings: Vec<Warning> = inp.errors.clone();
    let now = inp.now;
    for (path, msg) in &inp.ttl_bad {
        let file = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        warnings.push(Warning::issue(
            "ttl_record_unreadable",
            format!(
                "TTL record {} is unreadable ({}); it is ignored and never treated as expired",
                sanitize(&file),
                sanitize(msg)
            ),
        ));
    }
    for msg in &inp.git_bad {
        warnings.push(Warning::issue("git_state_unreadable", sanitize(msg)));
    }
    if inp.phase_bad > 0 {
        warnings.push(Warning::info(
            "phase_records_ignored",
            format!(
                "{} task-phase file(s) unreadable and ignored",
                inp.phase_bad
            ),
        ));
    }
    let gi = git_index(&inp.git, &inp.cluster_id, &mut warnings);
    let Some(catalog) = inp.catalog else {
        return TopSnapshot {
            mode: Mode::Live,
            collected_at: now,
            runtime: inp.runtime,
            databases: None,
            warnings,
        };
    };
    let rec_by_name: HashMap<&str, &TtlRecord> = inp
        .ttl_records
        .iter()
        .map(|r| (r.name.as_str(), r))
        .collect();
    let live: HashMap<&str, &CatalogRow> = catalog.iter().map(|r| (r.name.as_str(), r)).collect();
    // Metadata whose database is gone (or was recreated): report, never attach.
    for rec in &inp.ttl_records {
        let shown = sanitize(&rec.name);
        if rec.cluster_id != inp.cluster_id {
            warnings.push(
                Warning::issue(
                    "ttl_record_foreign_cluster",
                    format!(
                        "TTL record for {shown} belongs to another cluster incarnation; ignored"
                    ),
                )
                .on(&rec.name),
            );
            continue;
        }
        match live.get(rec.name.as_str()) {
            None if rec.state == TtlState::Deleting || rec.is_expired(now) => warnings.push(
                Warning::info(
                    "ttl_record_pending_cleanup",
                    format!("{shown}: database gone, TTL record awaiting cleanup"),
                )
                .on(&rec.name),
            ),
            None => warnings.push(
                Warning::issue(
                    "ttl_record_orphaned",
                    format!("TTL record for {shown} has no database (dropped outside PGX?)"),
                )
                .on(&rec.name),
            ),
            Some(row) if row.oid != rec.oid => warnings.push(
                Warning::issue(
                    "ttl_record_stale_oid",
                    format!(
                        "TTL record for {shown} was written for OID {} but the database is OID {}: stale, not attached",
                        rec.oid, row.oid
                    ),
                )
                .on(&rec.name),
            ),
            Some(_) => {}
        }
    }
    let mut dbs = Vec::new();
    for row in &catalog {
        if is_system(&row.name) {
            continue;
        }
        let rec = rec_by_name
            .get(row.name.as_str())
            .copied()
            .filter(|r| r.cluster_id == inp.cluster_id && r.oid == row.oid);
        let mut d = Database {
            oid: row.oid,
            name: row.name.clone(),
            is_template: row.is_template,
            invalid: row.invalid,
            connections: row.connections,
            size_bytes: inp.sizes.get(&row.oid).copied(),
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
            db_state: DbState::Unknown,
            lifecycle: None,
            role: Role::Standalone,
        };
        if let Some(r) = rec {
            d.created_at = Some(r.created_at).filter(|c| *c > 0);
            d.ttl_label = Some(r.ttl_label.clone());
            d.ttl_seconds = r.ttl_seconds;
            d.expires_at = r.expires_at;
            d.ttl_state = Some(r.state.as_str().to_string());
            d.last_error = r.last_error.as_deref().map(sanitize);
            if r.state == TtlState::RetryPending {
                warnings.push(
                    Warning::issue(
                        "ttl_delete_retry",
                        format!(
                            "{}: TTL deletion is being retried{}",
                            sanitize(&r.name),
                            r.last_error
                                .as_deref()
                                .map(|e| format!(" ({})", sanitize(e)))
                                .unwrap_or_default()
                        ),
                    )
                    .on(&r.name),
                );
            } else if let Some(e) = &r.last_error {
                warnings.push(
                    Warning::issue(
                        "ttl_last_error",
                        format!("{}: {}", sanitize(&r.name), sanitize(e)),
                    )
                    .on(&r.name),
                );
            }
            d.deadline_kind = r.expires_at.map(|_| DeadlineKind::Fixed);
            if let Some(m) = &r.agent {
                d.role = if m.agent_id.is_some() {
                    Role::Subagent
                } else {
                    Role::Session
                };
                d.lifecycle = Some(if r.is_expired(now) {
                    Lifecycle::Expired
                } else if m.retired {
                    Lifecycle::Retired
                } else {
                    Lifecycle::Active
                });
                d.deadline_kind = Some(if m.retired {
                    DeadlineKind::Grace
                } else {
                    DeadlineKind::IdleTtl
                });
                // The source's Git entry counts only if it verifies against
                // the source database's live OID (a reused name does not).
                let worktree = gi
                    .by_db
                    .get(m.source.as_str())
                    .filter(|g| {
                        live.get(m.source.as_str())
                            .is_some_and(|r| g.verifies(r.oid))
                    })
                    .map(|g| g.worktree.clone())
                    .filter(|w| !w.is_empty());
                d.agent = Some(Agent {
                    host: m.host.clone(),
                    session_id: m.session_id.clone(),
                    agent_id: m.agent_id.clone(),
                    project_id: m.project_id.clone(),
                    project_root: m.project_root.clone(),
                    project_name: project_name(&m.project_root, &m.project_id),
                    source: m.source.clone(),
                    last_touch: Some(m.last_touch).filter(|t| *t > 0),
                    retired: m.retired,
                    git_scope: m.git_scope.clone(),
                    git_branch: m.git_branch.clone(),
                    worktree,
                });
                // An explicit report shows only while the record is current:
                // same incarnation, same identity, metadata not expired.
                if d.lifecycle != Some(Lifecycle::Expired) {
                    d.phase = inp
                        .phases
                        .iter()
                        .filter(|p| p.matches(&inp.cluster_id, row.oid, &row.name, m))
                        .max_by_key(|p| p.reported_at)
                        .map(|p| TaskPhase {
                            phase: p.phase,
                            label: p.label.clone(),
                            reported_at: p.reported_at,
                            source: "pgx agent activity".into(),
                        });
                }
            }
        }
        if d.agent.is_none() {
            if let Some(g) = gi.by_db.get(row.name.as_str()) {
                // Recorded OIDs must match; only staging may lack one.
                if g.verifies(row.oid) {
                    d.role = g.role;
                    d.git = Some(GitInfo {
                        repo_id: g.st.repo_id.clone(),
                        repo_name: sanitize(&g.st.repo_name),
                        branch: g.branch.clone(),
                        worktree_path: g.worktree.clone(),
                    });
                }
            }
        }
        d.db_state = if row.invalid {
            DbState::Invalid
        } else {
            match d.ttl_state.as_deref() {
                Some("retry_pending") => DbState::DeleteRetry,
                Some("expiring") | Some("deleting") => DbState::Expiring,
                _ => match row.connections {
                    None => DbState::Unknown,
                    Some(0) => DbState::Idle,
                    Some(_) => DbState::Connected,
                },
            }
        };
        dbs.push(d);
    }
    dbs.sort_by(|a, b| a.name.cmp(&b.name).then(a.oid.cmp(&b.oid)));
    TopSnapshot {
        mode: Mode::Live,
        collected_at: now,
        runtime: inp.runtime,
        databases: Some(dbs),
        warnings,
    }
}

/// Every `$PGX_HOME/git/<repo>/state.json`, read-only.
pub fn load_git_states(home_root: &Path) -> (Vec<RepoState>, Vec<String>) {
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    let Ok(entries) = std::fs::read_dir(gitstate::root(home_root)) else {
        return (ok, bad);
    };
    for e in entries.flatten().take(1000) {
        if !e.path().is_dir() {
            continue;
        }
        let id = e.file_name().to_string_lossy().into_owned();
        match gitstate::load(home_root, &id) {
            Ok(Some(st)) => ok.push(st),
            Ok(None) => {}
            Err(err) => bad.push(format!("PGX Git state {}: {}", sanitize(&id), err.message)),
        }
    }
    ok.sort_by(|a, b| a.repo_id.cmp(&b.repo_id));
    (ok, bad)
}

/// Resident set size of a process, without spawning anything.
pub fn process_rss(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let line = s.lines().find(|l| l.starts_with("VmRSS:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        let n = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTASKINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if n == size {
            Some(info.pti_resident_size)
        } else {
            None
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

pub fn physical_memory() -> Option<u64> {
    let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if pages > 0 && size > 0 {
        Some(pages as u64 * size as u64)
    } else {
        None
    }
}

/// The catalog query: every database with its client connections, the
/// monitor's own backend excluded. One statement, no per-database round trip.
pub const CATALOG_SQL: &str = "SELECT d.oid, d.datname, d.datistemplate, d.datconnlimit = -2, coalesce(a.n, 0) \
     FROM pg_database d LEFT JOIN (SELECT datid, count(*) AS n FROM pg_stat_activity \
     WHERE backend_type = 'client backend' AND pid <> pg_backend_pid() GROUP BY datid) a ON a.datid = d.oid \
     ORDER BY d.datname";
/// Logical sizes, sampled every [`LiveCollector::size_every`].
pub const SIZES_SQL: &str = "SELECT d.oid, pg_database_size(d.oid) FROM pg_database d \
     WHERE d.datallowconn AND d.datconnlimit <> -2";
pub const UPTIME_SQL: &str =
    "SELECT extract(epoch FROM now() - pg_postmaster_start_time())::bigint";

/// All or nothing: a result that is not exactly the catalog query's shape
/// (five columns, every row well-formed) is rejected, so a response that
/// belongs to another statement can never pass for an empty catalog.
pub fn parse_catalog(r: &crate::pgwire::QueryResult) -> Option<Vec<CatalogRow>> {
    if r.columns.len() != 5 {
        return None;
    }
    r.rows
        .iter()
        .map(|r| {
            if r.len() != 5 {
                return None;
            }
            Some(CatalogRow {
                oid: r[0].as_deref()?.parse().ok()?,
                name: r[1].clone()?,
                is_template: r[2].as_deref() == Some("t"),
                invalid: r[3].as_deref() == Some("t"),
                connections: Some(r[4].as_deref()?.parse().ok()?),
            })
        })
        .collect()
}

/// An I/O or protocol error can leave part of a response unread on the
/// socket; the connection must not be reused after one.
fn wire_broken(e: &crate::pgwire::WireError) -> bool {
    matches!(
        e,
        crate::pgwire::WireError::Io(_) | crate::pgwire::WireError::Protocol(_)
    )
}

/// The live, read-only collector. One instance per `pgx top` process; the UI
/// runs it on a worker thread.
pub struct LiveCollector {
    client: Option<(u32, Client)>,
    sizes: HashMap<u64, u64>,
    sizes_at: Option<Instant>,
    pub size_every: Duration,
    runtime_fn: Option<bool>,
    pub query_timeout: Duration,
    phys: Option<u64>,
}

impl Default for LiveCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveCollector {
    pub fn new() -> LiveCollector {
        LiveCollector {
            client: None,
            sizes: HashMap::new(),
            sizes_at: None,
            size_every: Duration::from_secs(10),
            runtime_fn: None,
            query_timeout: Duration::from_secs(5),
            phys: physical_memory(),
        }
    }

    /// One refresh. Never fails: problems become runtime states and warnings.
    pub fn collect(&mut self) -> TopSnapshot {
        let now = crate::fsutil::now_epoch();
        let home = crate::layout::Home::resolve();
        if !home.is_initialized() {
            self.client = None;
            let mut s = build(Inputs::new(
                now,
                "",
                Runtime::empty(RuntimeStatus::NotInitialized),
            ));
            s.warnings.push(Warning::info(
                "not_initialized",
                format!(
                    "PGX is not initialised at {} (run `pgx init`)",
                    sanitize(&home.root.display().to_string())
                ),
            ));
            return s;
        }
        let ctx = match crate::cmd::Ctx::load() {
            Ok(c) => c,
            Err(e) => {
                self.client = None;
                let mut inp = Inputs::new(now, "", Runtime::empty(RuntimeStatus::Error));
                inp.errors
                    .push(Warning::issue("config_unreadable", sanitize(&e.message)));
                return build(inp);
            }
        };
        let cluster_id = ctx.cluster_id();
        let mut runtime = Runtime::empty(RuntimeStatus::Stopped);
        runtime.port = Some(ctx.config.port);
        runtime.phys_mem_bytes = self.phys;
        let mut inp = Inputs::new(now, &cluster_id, runtime);
        let store = MetaStore::new(&ctx.home.root);
        let (recs, bad) = store.list();
        inp.ttl_records = recs;
        inp.ttl_bad = bad;
        let (git, git_bad) = load_git_states(&ctx.home.root);
        inp.git = git;
        inp.git_bad = git_bad;
        let (phases, phase_bad) = phase::list(&ctx.home.root);
        inp.phases = phases;
        inp.phase_bad = phase_bad;

        let Some(pid) = ctx.running_pid() else {
            self.client = None;
            return build(inp);
        };
        inp.runtime.pid = Some(pid);
        inp.runtime.status = RuntimeStatus::Disconnected;
        // A restarted engine (new pid) needs a new connection and new capabilities.
        if self.client.as_ref().is_some_and(|(p, _)| *p != pid) {
            self.client = None;
            self.runtime_fn = None;
            self.sizes.clear();
            self.sizes_at = None;
        }
        if self.client.is_none() {
            match crate::pgwire::Client::connect_unix_app(
                &ctx.sock_dir(),
                ctx.config.port,
                "postgres",
                "postgres",
                "pgx-top",
                self.query_timeout,
            ) {
                Ok(c) => self.client = Some((pid, c)),
                Err(e) => {
                    inp.errors.push(Warning::issue(
                        "runtime_unreachable",
                        format!(
                            "cannot connect to the runtime: {}",
                            sanitize(&e.to_string())
                        ),
                    ));
                    return build(inp);
                }
            }
        }
        let (_, client) = self.client.as_mut().expect("connected above");
        let catalog = match client.simple_query(CATALOG_SQL) {
            Ok(r) => match parse_catalog(&r) {
                Some(c) => c,
                None => {
                    inp.errors.push(Warning::issue(
                        "catalog_query_failed",
                        "catalog query returned an unexpected result; reconnecting",
                    ));
                    self.client = None;
                    return build(inp);
                }
            },
            Err(e) => {
                inp.errors.push(Warning::issue(
                    "catalog_query_failed",
                    format!("catalog query failed: {}", sanitize(&e.to_string())),
                ));
                if wire_broken(&e) {
                    self.client = None;
                }
                return build(inp);
            }
        };
        inp.runtime.status = RuntimeStatus::Running;
        // The catalog above is complete and valid; a failure below only
        // costs this refresh its secondary metrics, and a broken
        // connection is replaced before the next one.
        let mut broken = false;
        match client.simple_query(UPTIME_SQL) {
            Ok(r) => inp.runtime.uptime_seconds = r.scalar().and_then(|s| s.parse().ok()),
            Err(e) => broken |= wire_broken(&e),
        }
        if !broken && self.runtime_fn != Some(false) {
            match client.simple_query("SELECT pgrust_runtime_status()") {
                Ok(r) => {
                    self.runtime_fn = Some(true);
                    inp.runtime.runtime_status_available = true;
                    let v: Option<serde_json::Value> =
                        r.scalar().and_then(|s| serde_json::from_str(s).ok());
                    inp.runtime.rss_bytes = v
                        .as_ref()
                        .and_then(|v| v.pointer("/memory/rss_bytes"))
                        .and_then(|x| x.as_u64());
                }
                Err(crate::pgwire::WireError::Db(_)) => self.runtime_fn = Some(false),
                Err(e) => broken |= wire_broken(&e),
            }
        }
        if inp.runtime.rss_bytes.is_none() {
            inp.runtime.rss_bytes = process_rss(pid);
        }
        let due = self.sizes_at.is_none_or(|t| t.elapsed() >= self.size_every);
        if due && !broken {
            match client.simple_query(SIZES_SQL) {
                Ok(r) if r.columns.len() == 2 => {
                    self.sizes = r
                        .rows
                        .iter()
                        .filter_map(|row| {
                            Some((
                                row.first()?.as_deref()?.parse().ok()?,
                                row.get(1)?.as_deref()?.parse().ok()?,
                            ))
                        })
                        .collect();
                }
                Ok(_) => broken = true,
                Err(e) => {
                    broken |= wire_broken(&e);
                    inp.errors.push(Warning::info(
                        "size_sample_failed",
                        format!("database sizes not refreshed: {}", sanitize(&e.to_string())),
                    ));
                }
            }
            self.sizes_at = Some(Instant::now());
        }
        if broken {
            inp.errors.push(Warning::info(
                "runtime_query_interrupted",
                "a runtime query timed out or broke the connection; reconnecting next refresh",
            ));
            self.client = None;
        }
        inp.sizes = self.sizes.clone();
        inp.catalog = Some(catalog);
        build(inp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ttlstore::AgentMeta;

    const NOW: u64 = 1_794_427_200;
    const CL: &str = "/data#2026-11-11";

    fn running() -> Runtime {
        Runtime {
            status: RuntimeStatus::Running,
            pid: Some(10),
            port: Some(5433),
            uptime_seconds: Some(5),
            rss_bytes: Some(1),
            phys_mem_bytes: None,
            runtime_status_available: true,
        }
    }

    fn row(oid: u64, name: &str, conns: u64) -> CatalogRow {
        CatalogRow {
            oid,
            name: name.into(),
            is_template: false,
            invalid: false,
            connections: Some(conns),
        }
    }

    fn rec(name: &str, oid: u64, agent: Option<AgentMeta>) -> TtlRecord {
        TtlRecord {
            name: name.into(),
            oid,
            cluster_id: CL.into(),
            created_at: NOW - 600,
            ttl_label: "2h".into(),
            ttl_seconds: Some(7200),
            expires_at: Some(NOW + 3600),
            state: TtlState::Active,
            last_attempt: None,
            last_error: None,
            agent,
        }
    }

    fn meta(project: &str, session: &str, agent: Option<&str>) -> AgentMeta {
        AgentMeta {
            host: "claude".into(),
            session_id: session.into(),
            project_id: format!("{project}-id"),
            project_root: format!("/work/{project}"),
            source: "app".into(),
            last_touch: NOW - 20,
            retired: false,
            agent_id: agent.map(String::from),
            git_scope: None,
            git_branch: None,
        }
    }

    fn inputs(catalog: Vec<CatalogRow>, recs: Vec<TtlRecord>) -> Inputs {
        let mut i = Inputs::new(NOW, CL, running());
        i.catalog = Some(catalog);
        i.ttl_records = recs;
        i
    }

    fn by_name<'a>(s: &'a TopSnapshot, n: &str) -> &'a Database {
        s.databases
            .as_ref()
            .unwrap()
            .iter()
            .find(|d| d.name == n)
            .unwrap_or_else(|| panic!("{n} missing"))
    }

    #[test]
    fn zero_one_five_hundred_thousand_databases() {
        for n in [0usize, 1, 5, 100, 1000] {
            let mut cat = vec![row(1, "template1", 0), row(5, "postgres", 0)];
            let mut recs = Vec::new();
            for i in 0..n {
                let name = format!("agent-claude-s{i:04}");
                cat.push(row(20_000 + i as u64, &name, (i % 3) as u64));
                recs.push(rec(
                    &name,
                    20_000 + i as u64,
                    Some(meta("p", &format!("s{i}"), None)),
                ));
            }
            let s = build(inputs(cat, recs));
            let sum = s.summary();
            assert_eq!(
                sum.databases,
                Some(n + 1),
                "n={n}: postgres + agents, no template1"
            );
            assert_eq!(sum.agent_databases, Some(n));
            assert_eq!(sum.sessions, Some(n));
            let conns: u64 = (0..n).map(|i| (i % 3) as u64).sum();
            assert_eq!(sum.client_connections, Some(conns));
            assert_eq!(sum.issues, 0);
        }
    }

    #[test]
    fn metadata_joins_only_on_cluster_and_oid() {
        let cat = vec![row(100, "a", 1), row(101, "b", 0), row(102, "c", 0)];
        let mut foreign = rec("b", 101, Some(meta("p", "s2", None)));
        foreign.cluster_id = "/other#x".into();
        let recs = vec![
            rec("a", 100, Some(meta("p", "s1", None))),
            foreign,
            rec("c", 999, Some(meta("p", "s3", None))), // recycled name, new OID
            rec("gone", 103, Some(meta("p", "s4", None))),
        ];
        let s = build(inputs(cat, recs));
        assert!(by_name(&s, "a").agent.is_some());
        assert_eq!(by_name(&s, "a").db_state, DbState::Connected);
        let b = by_name(&s, "b");
        assert!(
            b.agent.is_none() && b.ttl_label.is_none(),
            "foreign cluster not attached"
        );
        assert_eq!(b.role, Role::Standalone);
        let c = by_name(&s, "c");
        assert!(
            c.agent.is_none() && c.expires_at.is_none(),
            "stale OID not attached"
        );
        assert_eq!(c.db_state, DbState::Idle);
        let codes: Vec<&str> = s.warnings.iter().map(|w| w.code).collect();
        assert!(codes.contains(&"ttl_record_foreign_cluster"));
        assert!(codes.contains(&"ttl_record_stale_oid"));
        assert!(codes.contains(&"ttl_record_orphaned"));
        assert!(
            s.databases
                .as_ref()
                .unwrap()
                .iter()
                .all(|d| d.name != "gone"),
            "metadata without a database is a warning, not a live row"
        );
        assert_eq!(s.summary().issues, 3);
    }

    #[test]
    fn corrupt_and_truncated_records_are_warnings_only() {
        let mut i = inputs(vec![row(5, "postgres", 0)], vec![]);
        i.ttl_bad = vec![
            (PathBuf::from("/h/ttl/6162.json"), "ttl record: EOF".into()),
            (PathBuf::from("/h/ttl/\x1b[2J.json"), "bad\x1b[0m".into()),
        ];
        i.git_bad = vec!["PGX Git state r1: bad json".into()];
        i.phase_bad = 3;
        let s = build(i);
        assert_eq!(s.summary().databases, Some(1));
        assert_eq!(s.summary().issues, 3);
        for w in &s.warnings {
            assert!(!w.message.contains('\x1b'), "{:?}", w.message);
        }
    }

    #[test]
    fn ttl_states_deadlines_and_lifecycles() {
        let mut expired = rec("exp", 3, Some(meta("p", "s3", None)));
        expired.expires_at = Some(NOW - 10);
        let mut retired = rec("ret", 4, Some(meta("p", "s4", None)));
        retired.agent.as_mut().unwrap().retired = true;
        retired.expires_at = Some(NOW + 300);
        let mut retry = rec("retry", 5, None);
        retry.state = TtlState::RetryPending;
        retry.last_error = Some("database is in use".into());
        let mut deleting = rec("del", 6, None);
        deleting.state = TtlState::Deleting;
        let mut unlimited = rec("unl", 7, None);
        unlimited.ttl_label = "unlimited".into();
        unlimited.ttl_seconds = None;
        unlimited.expires_at = None;
        let cat = vec![
            row(3, "exp", 0),
            row(4, "ret", 0),
            row(5, "retry", 2),
            row(6, "del", 0),
            row(7, "unl", 0),
            CatalogRow {
                invalid: true,
                ..row(8, "broken", 0)
            },
            CatalogRow {
                connections: None,
                ..row(9, "unknown", 0)
            },
        ];
        let s = build(inputs(
            cat,
            vec![expired, retired, retry, deleting, unlimited],
        ));
        let e = by_name(&s, "exp");
        assert_eq!(e.lifecycle, Some(Lifecycle::Expired));
        assert_eq!(e.remaining(NOW), Some(0), "never negative");
        let r = by_name(&s, "ret");
        assert_eq!(r.lifecycle, Some(Lifecycle::Retired));
        assert_eq!(r.deadline_kind, Some(DeadlineKind::Grace));
        assert_eq!(
            r.db_state,
            DbState::Idle,
            "retired is a lifecycle, not a db state"
        );
        let rt = by_name(&s, "retry");
        assert_eq!(rt.db_state, DbState::DeleteRetry);
        assert_eq!(rt.last_error.as_deref(), Some("database is in use"));
        assert_eq!(rt.deadline_kind, Some(DeadlineKind::Fixed));
        assert_eq!(by_name(&s, "del").db_state, DbState::Expiring);
        assert_eq!(by_name(&s, "unl").expires_at, None);
        assert_eq!(by_name(&s, "unl").remaining(NOW), None);
        assert_eq!(by_name(&s, "broken").db_state, DbState::Invalid);
        assert_eq!(by_name(&s, "unknown").db_state, DbState::Unknown);
        assert_eq!(
            s.summary().client_connections,
            None,
            "one unknown count = unknown total"
        );
        assert!(s.warnings.iter().any(|w| w.code == "ttl_delete_retry"));
    }

    #[test]
    fn last_touch_is_not_activity_and_retired_is_not_success() {
        let mut m = meta("p", "s", Some("agent-x"));
        m.retired = true;
        let s = build(inputs(vec![row(10, "x", 3)], vec![rec("x", 10, Some(m))]));
        let d = by_name(&s, "x");
        assert_eq!(d.db_state, DbState::Connected);
        assert_eq!(d.lifecycle, Some(Lifecycle::Retired));
        assert_eq!(
            d.phase, None,
            "no report, no phase — retirement never implies COMPLETED"
        );
        assert_eq!(d.agent.as_ref().unwrap().last_touch, Some(NOW - 20));
        let mut m0 = meta("p", "s2", None);
        m0.last_touch = 0;
        let s = build(inputs(vec![row(11, "y", 0)], vec![rec("y", 11, Some(m0))]));
        assert_eq!(by_name(&s, "y").agent.as_ref().unwrap().last_touch, None);
    }

    #[test]
    fn phases_attach_only_to_the_exact_incarnation_and_identity() {
        let m = meta("p", "s", Some("agent-a"));
        let other = meta("p", "s", Some("agent-b"));
        let mut i = inputs(
            vec![row(10, "a", 0), row(11, "b", 0)],
            vec![
                rec("a", 10, Some(m.clone())),
                rec("b", 11, Some(other.clone())),
            ],
        );
        let mk = |meta: &AgentMeta, oid: u64, name: &str, phase: Phase, at: u64| PhaseRecord {
            cluster_id: CL.into(),
            database: name.into(),
            database_oid: oid,
            project_id: meta.project_id.clone(),
            host: meta.host.clone(),
            session_id: meta.session_id.clone(),
            agent_id: meta.agent_id.clone(),
            git_scope: meta.git_scope.clone(),
            phase,
            label: Some("JWT".into()),
            reported_at: at,
        };
        i.phases = vec![
            mk(&m, 10, "a", Phase::Working, 1),
            mk(&m, 10, "a", Phase::Testing, 2),
            mk(&m, 9, "a", Phase::Failed, 3), // previous incarnation
        ];
        let s = build(i.clone());
        let a = by_name(&s, "a");
        assert_eq!(a.phase.as_ref().map(|p| p.phase), Some(Phase::Testing));
        assert_eq!(by_name(&s, "b").phase, None, "sibling subagent unaffected");
        // Expired metadata hides the phase.
        i.ttl_records[0].expires_at = Some(NOW - 1);
        assert_eq!(by_name(&build(i), "a").phase, None);
    }

    #[test]
    fn git_state_attaches_by_oid_and_cluster() {
        let st = RepoState::from_json(
            &serde_json::json!({
                "schema": 1, "repo_id": "r1", "repo_name": "remotely", "repo_slug": "remotely",
                "common_dir": "/w/.git", "main_branch": "main", "cluster_id": CL,
                "main": {"role": "main", "worktree_id": "w0", "worktree_path": "/w", "branch": "main",
                         "database": "remotely_main", "oid": 50, "source": "", "created_at": 1, "last_used": 1},
                "branches": [
                    {"role": "branch", "worktree_id": "w0", "worktree_path": "/w", "branch": "feature/auth",
                     "database": "remotely_feature_auth", "oid": 51, "source": "s", "created_at": 1, "last_used": 1},
                    {"role": "branch", "worktree_id": "w1", "worktree_path": "/w2", "branch": "stale",
                     "database": "remotely_stale", "oid": 77, "source": "s", "created_at": 1, "last_used": 1}
                ],
                "snapshots": [{"generation": 1, "database": "remotely_snap_g1", "staging": "", "oid": 52, "state": "ready"}]
            })
            .to_string(),
        )
        .unwrap();
        let mut i = inputs(
            vec![
                row(50, "remotely_main", 0),
                row(51, "remotely_feature_auth", 1),
                row(52, "remotely_snap_g1", 0),
                row(53, "remotely_stale", 0),
            ],
            vec![],
        );
        i.git = vec![st.clone()];
        let s = build(i);
        assert_eq!(by_name(&s, "remotely_main").role, Role::GitMain);
        let f = by_name(&s, "remotely_feature_auth");
        assert_eq!(f.role, Role::GitBranch);
        assert_eq!(f.git.as_ref().unwrap().branch, "feature/auth");
        assert_eq!(by_name(&s, "remotely_snap_g1").role, Role::GitSnapshot);
        assert_eq!(
            by_name(&s, "remotely_stale").role,
            Role::Standalone,
            "OID mismatch"
        );
        let mut foreign = st;
        foreign.cluster_id = "other".into();
        let mut i = inputs(vec![row(50, "remotely_main", 0)], vec![]);
        i.git = vec![foreign];
        let s = build(i);
        assert_eq!(by_name(&s, "remotely_main").role, Role::Standalone);
        assert!(s
            .warnings
            .iter()
            .any(|w| w.code == "git_state_foreign_cluster"));
    }

    #[test]
    fn unavailable_catalog_is_not_an_empty_one() {
        let mut i = Inputs::new(NOW, CL, Runtime::empty(RuntimeStatus::Stopped));
        i.ttl_records = vec![rec("a", 1, Some(meta("p", "s", None)))];
        let s = build(i);
        assert_eq!(s.databases, None);
        assert!(
            s.warnings.iter().all(|w| w.code != "ttl_record_orphaned"),
            "a stopped runtime does not make metadata orphaned"
        );
    }

    #[test]
    fn project_names_are_readable_and_safe() {
        assert_eq!(project_name("/home/me/src/remotely", "abcd"), "remotely");
        assert_eq!(project_name("", "0123456789abcdef"), "project 01234567");
        assert_eq!(project_name("", ""), "(unknown project)");
        assert_eq!(project_name("/x/\x1b[31mevil", "id"), "?[31mevil");
    }

    fn qr(cols: usize, rows: Vec<Vec<Option<&str>>>) -> crate::pgwire::QueryResult {
        crate::pgwire::QueryResult {
            columns: (0..cols).map(|i| format!("c{i}")).collect(),
            rows: rows
                .into_iter()
                .map(|r| r.into_iter().map(|v| v.map(String::from)).collect())
                .collect(),
            tags: vec![],
        }
    }

    #[test]
    fn catalog_parse_is_all_or_nothing() {
        let good = vec![Some("5"), Some("postgres"), Some("f"), Some("f"), Some("2")];
        let p = parse_catalog(&qr(5, vec![good.clone()])).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].connections, Some(2));
        assert_eq!(
            parse_catalog(&qr(5, vec![])),
            Some(vec![]),
            "a real empty catalog"
        );
        // A leftover response of another statement (sizes: 2 columns,
        // runtime status: 1) must never read as an empty catalog.
        assert_eq!(
            parse_catalog(&qr(2, vec![vec![Some("5"), Some("7000")]])),
            None
        );
        assert_eq!(parse_catalog(&qr(1, vec![vec![Some("{}")]])), None);
        assert_eq!(
            parse_catalog(&qr(2, vec![])),
            None,
            "shape is checked even with no rows"
        );
        let bad_oid = vec![Some("x"), Some("bad"), Some("f"), Some("f"), Some("0")];
        assert_eq!(parse_catalog(&qr(5, vec![good.clone(), bad_oid])), None);
        assert_eq!(parse_catalog(&qr(5, vec![good, vec![Some("6")]])), None);
    }

    #[test]
    fn git_entries_without_a_recorded_oid_and_reused_source_names_do_not_verify() {
        let st = RepoState::from_json(
            &serde_json::json!({
                "schema": 1, "repo_id": "r1", "repo_name": "remotely", "repo_slug": "remotely",
                "common_dir": "/w/.git", "main_branch": "main", "cluster_id": CL,
                "branches": [
                    {"role": "branch", "worktree_id": "w0", "worktree_path": "/w", "branch": "no-oid",
                     "database": "remotely_no_oid", "source": "s", "created_at": 1, "last_used": 1},
                    {"role": "branch", "worktree_id": "w0", "worktree_path": "/w/feature", "branch": "feature",
                     "database": "remotely_feature", "oid": 51, "source": "s", "created_at": 1, "last_used": 1}
                ],
                "snapshots": [{"generation": null, "database": "remotely_snap_g2", "staging": "remotely_snap_stage",
                               "oid": 0, "state": "creating"}]
            })
            .to_string(),
        )
        .unwrap();
        let mut m = meta("p", "s", None);
        m.source = "remotely_feature".into();
        m.git_scope = Some("sc".into());
        m.git_branch = Some("feature".into());
        let mut i = inputs(
            vec![
                row(60, "remotely_no_oid", 0),
                row(61, "remotely_snap_stage", 0),
                row(99, "remotely_feature", 0), // the name now belongs to another incarnation
                row(70, "agent-x", 0),
            ],
            vec![rec("agent-x", 70, Some(m))],
        );
        i.git = vec![st];
        let s = build(i);
        assert_eq!(
            by_name(&s, "remotely_no_oid").role,
            Role::Standalone,
            "missing OID is unverified"
        );
        assert_eq!(
            by_name(&s, "remotely_snap_stage").role,
            Role::GitSnapshot,
            "staging may lack one"
        );
        assert_eq!(by_name(&s, "remotely_feature").role, Role::Standalone);
        let a = by_name(&s, "agent-x").agent.clone().unwrap();
        assert_eq!(
            a.worktree, None,
            "worktree of a reused source name is not shown"
        );
        assert_eq!(
            a.git_branch.as_deref(),
            Some("feature"),
            "the record's own label stays"
        );
    }

    #[test]
    fn process_memory_is_readable_for_ourselves() {
        assert!(process_rss(std::process::id()).is_some_and(|b| b > 0));
        assert!(physical_memory().is_some_and(|b| b > 0));
    }
}
