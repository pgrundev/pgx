//! Durable per-database TTL metadata: one atomic JSON file per database under
//! `$PGX_HOME/ttl/`. Independent files mean concurrent CLI operations on
//! different databases never contend, and each write is crash-safe (a temp
//! file renamed into place). A record carries enough identity (the database
//! OID and the cluster's system identifier) that stale metadata can never
//! delete a same-named replacement or a database in another cluster.
use crate::errors::{PgxError, Result};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Lifecycle state of a TTL-managed database (section 3 state machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Active,
    Expiring,
    Deleting,
    RetryPending,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Active => "active",
            State::Expiring => "expiring",
            State::Deleting => "deleting",
            State::RetryPending => "retry_pending",
        }
    }
    fn parse(s: &str) -> State {
        match s {
            "expiring" => State::Expiring,
            "deleting" => State::Deleting,
            "retry_pending" => State::RetryPending,
            _ => State::Active,
        }
    }
}

/// Who owns an agent branch: the coding-agent session it was created for.
/// `pgx agent` reads and writes it; the TTL supervisor ignores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMeta {
    /// `claude`, `cursor`, `codex`, or a free label for `pgx agent exec`.
    pub host: String,
    pub session_id: String,
    /// Hash of the project root, so identical session ids in two projects stay apart.
    pub project_id: String,
    pub project_root: String,
    /// The database the branch was cloned from.
    pub source: String,
    /// Last activity (epoch seconds); `touch` moves `expires_at` from here.
    pub last_touch: u64,
    /// Set by `down` without `--now`: the session ended and the grace period runs.
    pub retired: bool,
    /// The host's id for a subagent of the session; `None` for the session itself.
    pub agent_id: Option<String>,
    /// PGX Git scope (worktree + Git branch hash) the branch belongs to.
    pub git_scope: Option<String>,
    /// The Git branch, for humans (`pgx agent ls`, `pgx git status`).
    pub git_branch: Option<String>,
}

impl AgentMeta {
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "host": self.host,
            "session_id": self.session_id,
            "agent_id": self.agent_id,
            "project_id": self.project_id,
            "project_root": self.project_root,
            "source": self.source,
            "last_touch": self.last_touch,
            "retired": self.retired,
            "git_scope": self.git_scope,
            "git_branch": self.git_branch,
        })
    }

    /// A missing or malformed object is `None`, never an error: the TTL record
    /// stays readable and its safety gates keep working.
    fn from_json(v: Option<&serde_json::Value>) -> Option<AgentMeta> {
        let v = v?;
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
        Some(AgentMeta {
            host: s("host")?,
            session_id: s("session_id")?,
            project_id: s("project_id")?,
            project_root: s("project_root").unwrap_or_default(),
            source: s("source")?,
            last_touch: v.get("last_touch").and_then(|x| x.as_u64()).unwrap_or(0),
            retired: v.get("retired").and_then(|x| x.as_bool()).unwrap_or(false),
            // Absent in records written before subagents existed: the session itself.
            agent_id: s("agent_id").filter(|a| !a.is_empty()),
            // Absent in records written before PGX Git existed.
            git_scope: s("git_scope").filter(|a| !a.is_empty()),
            git_branch: s("git_branch").filter(|a| !a.is_empty()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TtlRecord {
    pub name: String,
    /// pg_database.oid at creation — the database incarnation identity.
    pub oid: u64,
    /// pg_control_system().system_identifier — the cluster identity.
    pub cluster_id: String,
    pub created_at: u64,
    /// The user's original TTL string, e.g. "5m", "24h", "unlimited".
    pub ttl_label: String,
    /// None for unlimited.
    pub ttl_seconds: Option<u64>,
    /// None for unlimited.
    pub expires_at: Option<u64>,
    pub state: State,
    pub last_attempt: Option<u64>,
    pub last_error: Option<String>,
    /// Present only on branches created by `pgx agent`.
    pub agent: Option<AgentMeta>,
}

impl TtlRecord {
    pub fn is_expired(&self, now: u64) -> bool {
        matches!(self.expires_at, Some(e) if now >= e)
    }

    fn to_json(&self) -> String {
        let v = json!({
            "schema": 1,
            "name": self.name,
            "oid": self.oid,
            "cluster_id": self.cluster_id,
            "created_at": self.created_at,
            "ttl_label": self.ttl_label,
            "ttl_seconds": self.ttl_seconds,
            "expires_at": self.expires_at,
            "state": self.state.as_str(),
            "last_attempt": self.last_attempt,
            "last_error": self.last_error,
            "agent": self.agent.as_ref().map(AgentMeta::to_json),
        });
        serde_json::to_string_pretty(&v).expect("ttl record serialises")
    }

    fn from_json(text: &str) -> Result<TtlRecord> {
        let bad =
            |m: &str| PgxError::new(crate::cli::ExitCode::Corrupt, format!("ttl record: {m}"));
        let v: serde_json::Value = serde_json::from_str(text).map_err(|e| bad(&e.to_string()))?;
        // Unknown future schema versions are treated as unreadable, not deletable.
        match v.get("schema").and_then(|s| s.as_u64()) {
            Some(1) => {}
            _ => return Err(bad("missing or unsupported schema version")),
        }
        let name = v
            .get("name")
            .and_then(|x| x.as_str())
            .ok_or_else(|| bad("name missing"))?
            .to_string();
        let oid = v
            .get("oid")
            .and_then(|x| x.as_u64())
            .ok_or_else(|| bad("oid missing"))?;
        let cluster_id = v
            .get("cluster_id")
            .and_then(|x| x.as_str())
            .ok_or_else(|| bad("cluster_id missing"))?
            .to_string();
        let created_at = v.get("created_at").and_then(|x| x.as_u64()).unwrap_or(0);
        let ttl_label = v
            .get("ttl_label")
            .and_then(|x| x.as_str())
            .unwrap_or("unlimited")
            .to_string();
        let ttl_seconds = v.get("ttl_seconds").and_then(|x| x.as_u64());
        let expires_at = v.get("expires_at").and_then(|x| x.as_u64());
        let state = State::parse(v.get("state").and_then(|x| x.as_str()).unwrap_or("active"));
        let last_attempt = v.get("last_attempt").and_then(|x| x.as_u64());
        let last_error = v
            .get("last_error")
            .and_then(|x| x.as_str())
            .map(String::from);
        let agent = AgentMeta::from_json(v.get("agent"));
        Ok(TtlRecord {
            name,
            oid,
            cluster_id,
            created_at,
            ttl_label,
            ttl_seconds,
            expires_at,
            state,
            last_attempt,
            last_error,
            agent,
        })
    }
}

/// The on-disk store rooted at `$PGX_HOME/ttl/`.
pub struct MetaStore {
    dir: PathBuf,
}

/// Hex-encode the database name for the filename so it is safe on every
/// filesystem (including case-insensitive APFS, where `App` and `app` are
/// distinct database names but would otherwise collide).
fn file_stem(name: &str) -> String {
    let mut s = String::with_capacity(name.len() * 2);
    for b in name.bytes() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

impl MetaStore {
    pub fn new(home_root: &Path) -> MetaStore {
        MetaStore {
            dir: home_root.join("ttl"),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.json", file_stem(name)))
    }

    /// Write (or replace) a record atomically with 0600 permissions.
    pub fn save(&self, rec: &TtlRecord) -> Result<()> {
        use std::io::Write;
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| PgxError::io("cannot create", &self.dir, &e))?;
        let path = self.path_for(&rec.name);
        let tmp = self.dir.join(format!(".{}.tmp", file_stem(&rec.name)));
        {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts
                .open(&tmp)
                .map_err(|e| PgxError::io("cannot write", &tmp, &e))?;
            f.write_all(rec.to_json().as_bytes())
                .map_err(|e| PgxError::io("cannot write", &tmp, &e))?;
            f.sync_all().ok();
        }
        std::fs::rename(&tmp, &path)
            .map_err(|e| PgxError::io("cannot rename into place", &path, &e))
    }

    /// Load one record, or None if it does not exist. A corrupt record is an
    /// error (callers must fail safe, never treat it as deletable).
    pub fn load(&self, name: &str) -> Result<Option<TtlRecord>> {
        let path = self.path_for(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(TtlRecord::from_json(&text)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(PgxError::io("cannot read", &path, &e)),
        }
    }

    /// All readable records. Corrupt or unreadable files are returned separately
    /// (as names) so a diagnostic can surface them — they are never silently
    /// dropped, and never treated as expired.
    pub fn list(&self) -> (Vec<TtlRecord>, Vec<(PathBuf, String)>) {
        let mut ok = Vec::new();
        let mut bad = Vec::new();
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(_) => return (ok, bad), // no dir yet = no managed databases
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(text) => match TtlRecord::from_json(&text) {
                    Ok(rec) => ok.push(rec),
                    Err(e) => bad.push((path, e.message)),
                },
                Err(e) => bad.push((path, e.to_string())),
            }
        }
        ok.sort_by(|a, b| a.name.cmp(&b.name));
        (ok, bad)
    }

    /// Remove a record (finalizing a deletion). Missing is success (idempotent).
    pub fn remove(&self, name: &str) -> Result<()> {
        let path = self.path_for(name);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(PgxError::io("cannot remove", &path, &e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: &str, expires: Option<u64>) -> TtlRecord {
        TtlRecord {
            name: name.to_string(),
            oid: 16384,
            cluster_id: "7311111111111111111".to_string(),
            created_at: 1_000_000,
            ttl_label: expires.map(|_| "5m").unwrap_or("unlimited").to_string(),
            ttl_seconds: expires.map(|_| 300),
            expires_at: expires,
            state: State::Active,
            last_attempt: None,
            last_error: None,
            agent: None,
        }
    }

    fn tmpstore() -> (MetaStore, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "pgx-ttlstore-{}-{}",
            std::process::id(),
            fastrand_like()
        ));
        std::fs::create_dir_all(&base).unwrap();
        (MetaStore::new(&base), base)
    }

    fn fastrand_like() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as u64
    }

    #[test]
    fn roundtrip_record() {
        let r = rec("agent-001", Some(1_000_300));
        let back = TtlRecord::from_json(&r.to_json()).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn save_load_list_remove() {
        let (s, base) = tmpstore();
        s.save(&rec("permanent", None)).unwrap();
        s.save(&rec("agent-001", Some(1_000_300))).unwrap();
        assert_eq!(s.load("permanent").unwrap().unwrap().name, "permanent");
        assert!(s.load("nope").unwrap().is_none());
        let (ok, bad) = s.list();
        assert_eq!(ok.len(), 2);
        assert!(bad.is_empty());
        s.remove("agent-001").unwrap();
        s.remove("agent-001").unwrap(); // idempotent
        assert_eq!(s.list().0.len(), 1);
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn records_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (s, base) = tmpstore();
        s.save(&rec("secretdb", Some(1_000_300))).unwrap();
        let p = s.path_for("secretdb");
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode {mode:o}");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn case_distinct_names_do_not_collide() {
        let (s, base) = tmpstore();
        s.save(&rec("App", Some(1_000_300))).unwrap();
        s.save(&rec("app", None)).unwrap();
        // Two files, two records, each with its own name preserved.
        let (ok, _) = s.list();
        assert_eq!(ok.len(), 2, "case-distinct names must not share a file");
        assert_ne!(s.path_for("App"), s.path_for("app"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn corrupt_and_unknown_schema_are_reported_not_deletable() {
        let (s, base) = tmpstore();
        std::fs::create_dir_all(s.dir()).unwrap();
        std::fs::write(
            s.dir().join(format!("{}.json", file_stem("broken"))),
            "{not json",
        )
        .unwrap();
        std::fs::write(
            s.dir().join(format!("{}.json", file_stem("future"))),
            r#"{"schema":99,"name":"future"}"#,
        )
        .unwrap();
        let (ok, bad) = s.list();
        assert!(ok.is_empty(), "no valid records");
        assert_eq!(
            bad.len(),
            2,
            "both surfaced as bad, not parsed as expirable"
        );
        // load of a corrupt record errors (caller fails safe), never returns a deletable record.
        assert!(s.load("broken").is_err());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn expiry_predicate() {
        assert!(rec("x", Some(100)).is_expired(100));
        assert!(rec("x", Some(100)).is_expired(101));
        assert!(!rec("x", Some(100)).is_expired(99));
        assert!(!rec("x", None).is_expired(u64::MAX)); // unlimited never expires
    }

    #[test]
    fn agent_metadata_round_trips_and_is_optional() {
        let mut r = rec("agent-claude-9b1f3c2d", Some(1_007_200));
        r.agent = Some(AgentMeta {
            host: "claude".into(),
            session_id: "9b1f3c2d-0000".into(),
            project_id: "a1b2c3d4e5f60718".into(),
            project_root: "/p".into(),
            source: "myapp".into(),
            last_touch: 1_000_000,
            retired: false,
            agent_id: None,
            git_scope: None,
            git_branch: None,
        });
        assert_eq!(TtlRecord::from_json(&r.to_json()).unwrap(), r);
        // A subagent record round-trips its agent id; a pre-subagent record
        // (no `agent_id` key at all) reads back as the session itself.
        let mut sub = r.clone();
        sub.agent.as_mut().unwrap().agent_id = Some("agent-abc123".into());
        assert_eq!(TtlRecord::from_json(&sub.to_json()).unwrap(), sub);
        let legacy = r.to_json().replace("\"agent_id\": null,\n", "");
        assert!(!legacy.contains("agent_id"), "{legacy}");
        assert_eq!(TtlRecord::from_json(&legacy).unwrap(), r);
        assert!(TtlRecord::from_json(&rec("x", Some(1)).to_json())
            .unwrap()
            .agent
            .is_none());
        // A malformed agent object is ignored, never an error: the record stays
        // readable and the TTL safety gates keep working.
        let text = rec("y", Some(1))
            .to_json()
            .replace("\"schema\": 1", "\"agent\": 42, \"schema\": 1");
        assert!(TtlRecord::from_json(&text).unwrap().agent.is_none());
    }
}
