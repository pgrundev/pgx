//! Explicit task phases: the optional sidecar `pgx agent activity` writes and
//! `pgx top` reads. One atomic 0600 JSON file per agent identity under
//! `$PGX_HOME/top/phases/`, separate from the TTL store so a phase report can
//! never move a deadline or change a lifecycle.
//!
//! A record names the exact database incarnation it was reported for
//! (cluster identity + database OID) and the full agent identity. The
//! collector shows it only when all of that still matches the live database
//! and its TTL record, so a phase can never leak to a recreated database, to
//! another project, worktree, session or subagent.
use super::model::Phase;
use crate::errors::{PgxError, Result};
use crate::ttlstore::AgentMeta;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Longest task label, in characters.
pub const MAX_LABEL_CHARS: usize = 80;
/// A phase file larger than this is not a phase file.
const MAX_FILE_BYTES: u64 = 4096;
/// The collector reads at most this many phase files per refresh.
pub const MAX_FILES: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseRecord {
    pub cluster_id: String,
    pub database: String,
    pub database_oid: u64,
    pub project_id: String,
    pub host: String,
    pub session_id: String,
    pub agent_id: Option<String>,
    pub git_scope: Option<String>,
    pub phase: Phase,
    pub label: Option<String>,
    pub reported_at: u64,
}

pub fn dir(home_root: &Path) -> PathBuf {
    home_root.join("top").join("phases")
}

/// A task label as given on the command line: trimmed, at most
/// [`MAX_LABEL_CHARS`] characters, no control/escape/bidi characters.
/// Rejected rather than silently cleaned, so the reporter learns.
pub fn validate_label(s: &str) -> Result<Option<String>> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(None);
    }
    if !super::text::is_clean(t) {
        return Err(PgxError::usage(
            "--task must be plain text: control characters, escape sequences and bidi overrides are not allowed",
        ));
    }
    let n = t.chars().count();
    if n > MAX_LABEL_CHARS || super::text::width(t) > MAX_LABEL_CHARS {
        return Err(PgxError::usage(format!(
            "--task is {n} characters; keep it to {MAX_LABEL_CHARS}"
        )));
    }
    Ok(Some(t.to_string()))
}

impl PhaseRecord {
    pub fn to_json(&self) -> Value {
        json!({
            "schema": 1,
            "cluster_id": self.cluster_id,
            "database": self.database,
            "database_oid": self.database_oid,
            "project_id": self.project_id,
            "host": self.host,
            "session_id": self.session_id,
            "agent_id": self.agent_id,
            "git_scope": self.git_scope,
            "state": self.phase.as_str(),
            "label": self.label,
            "reported_at": self.reported_at,
            "source": "pgx agent activity",
        })
    }

    /// Strict: any missing field, unknown state or unclean label rejects the record.
    pub fn from_json(text: &str) -> Option<PhaseRecord> {
        let v: Value = serde_json::from_str(text).ok()?;
        if v.get("schema").and_then(Value::as_u64) != Some(1) {
            return None;
        }
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(String::from);
        let opt = |k: &str| -> Option<Option<String>> {
            match v.get(k) {
                None | Some(Value::Null) => Some(None),
                Some(Value::String(x)) if !x.is_empty() => Some(Some(x.clone())),
                _ => None,
            }
        };
        let label = opt("label")?;
        if let Some(l) = &label {
            validate_label(l).ok()??;
        }
        Some(PhaseRecord {
            cluster_id: s("cluster_id")?,
            database: s("database")?,
            database_oid: v.get("database_oid").and_then(Value::as_u64)?,
            project_id: s("project_id")?,
            host: s("host")?,
            session_id: s("session_id")?,
            agent_id: opt("agent_id")?,
            git_scope: opt("git_scope")?,
            phase: Phase::parse(&s("state")?)?,
            label,
            reported_at: v.get("reported_at").and_then(Value::as_u64)?,
        })
    }

    /// Whether this report belongs to the database `oid` in `cluster_id`
    /// owned by `meta` — every identity field must agree.
    pub fn matches(&self, cluster_id: &str, oid: u64, name: &str, meta: &AgentMeta) -> bool {
        self.cluster_id == cluster_id
            && self.database_oid == oid
            && self.database == name
            && self.project_id == meta.project_id
            && self.host == meta.host
            && self.session_id == meta.session_id
            && self.agent_id == meta.agent_id
            && self.git_scope == meta.git_scope
    }
}

/// The file for one agent identity (the session key's material).
pub fn path_for(home_root: &Path, key_material: &str) -> PathBuf {
    use crate::cmd::agent::identity::{fnv1a64, hex16};
    dir(home_root).join(format!("{}.json", hex16(fnv1a64(key_material.as_bytes()))))
}

/// Write a record atomically (unique temp file, fsync, rename) with 0600
/// permissions in a 0700 directory. Concurrent writers never interleave:
/// the last rename wins and every reader sees one whole record.
pub fn save(home_root: &Path, key_material: &str, rec: &PhaseRecord) -> Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let d = dir(home_root);
    std::fs::create_dir_all(&d).map_err(|e| PgxError::io("cannot create", &d, &e))?;
    for p in [home_root.join("top"), d.clone()] {
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700));
    }
    let path = path_for(home_root, key_material);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = d.join(format!(
        ".{}.{}.{nanos}.{seq}.tmp",
        path.file_stem().and_then(|s| s.to_str()).unwrap_or("phase"),
        std::process::id()
    ));
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| PgxError::io("cannot write", &tmp, &e))?;
        let body = serde_json::to_string_pretty(&rec.to_json()).expect("phase serialises");
        f.write_all(body.as_bytes())
            .map_err(|e| PgxError::io("cannot write", &tmp, &e))?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        PgxError::io("cannot rename into place", &path, &e)
    })?;
    Ok(path)
}

/// Every readable record, plus the number of files that were not (corrupt,
/// truncated, oversized, foreign). Read-only; never repairs or deletes.
pub fn list(home_root: &Path) -> (Vec<PhaseRecord>, usize) {
    let mut ok = Vec::new();
    let mut bad = 0;
    let Ok(entries) = std::fs::read_dir(dir(home_root)) else {
        return (ok, bad);
    };
    let mut seen = 0usize;
    for entry in entries.flatten() {
        seen += 1;
        if seen > MAX_FILES {
            // Counted, not read: the warning says some reports were skipped.
            bad += 1;
            continue;
        }
        let p = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let too_big = entry
            .metadata()
            .map(|m| !m.is_file() || m.len() > MAX_FILE_BYTES)
            .unwrap_or(true);
        if too_big {
            bad += 1;
            continue;
        }
        match std::fs::read_to_string(&p)
            .ok()
            .and_then(|t| PhaseRecord::from_json(&t))
        {
            Some(r) => ok.push(r),
            None => bad += 1,
        }
    }
    (ok, bad)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn meta(project: &str, host: &str, session: &str, agent: Option<&str>) -> AgentMeta {
        AgentMeta {
            host: host.into(),
            session_id: session.into(),
            project_id: project.into(),
            project_root: format!("/w/{project}"),
            source: "app".into(),
            last_touch: 1,
            retired: false,
            agent_id: agent.map(String::from),
            git_scope: None,
            git_branch: None,
        }
    }

    pub fn rec_for(m: &AgentMeta, oid: u64, name: &str, phase: Phase) -> PhaseRecord {
        PhaseRecord {
            cluster_id: "c1".into(),
            database: name.into(),
            database_oid: oid,
            project_id: m.project_id.clone(),
            host: m.host.clone(),
            session_id: m.session_id.clone(),
            agent_id: m.agent_id.clone(),
            git_scope: m.git_scope.clone(),
            phase,
            label: Some("Implement OAuth".into()),
            reported_at: 100,
        }
    }

    pub fn tmp_home(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "pgx-top-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn labels_are_bounded_and_plain() {
        assert_eq!(validate_label("").unwrap(), None);
        assert_eq!(validate_label("   ").unwrap(), None);
        assert_eq!(
            validate_label("  Implement OAuth ").unwrap().as_deref(),
            Some("Implement OAuth")
        );
        assert!(validate_label(&"x".repeat(80)).unwrap().is_some());
        assert!(validate_label(&"x".repeat(81)).is_err());
        assert!(validate_label(&"漢".repeat(41)).is_err(), "82 columns");
        assert!(validate_label("ünïcødé ok").unwrap().is_some());
        for bad in [
            "\x1b[31mred",
            "line\nbreak",
            "tab\there",
            "bell\x07",
            "rtl\u{202E}x",
            "c1\u{9b}x",
        ] {
            assert!(validate_label(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn record_round_trips_and_strict_parse_rejects_damage() {
        let m = meta("p1", "claude", "s1", Some("agent-a"));
        let r = rec_for(&m, 16384, "agent-claude-s1-a", Phase::Testing);
        let text = r.to_json().to_string();
        assert_eq!(PhaseRecord::from_json(&text), Some(r.clone()));
        // Truncated, wrong schema, unknown state, missing oid, unclean label.
        assert!(PhaseRecord::from_json(&text[..text.len() / 2]).is_none());
        let mut v = r.to_json();
        v["schema"] = json!(2);
        assert!(PhaseRecord::from_json(&v.to_string()).is_none());
        let mut v = r.to_json();
        v["state"] = json!("done");
        assert!(PhaseRecord::from_json(&v.to_string()).is_none());
        let mut v = r.to_json();
        v.as_object_mut().unwrap().remove("database_oid");
        assert!(PhaseRecord::from_json(&v.to_string()).is_none());
        let mut v = r.to_json();
        v["label"] = json!("\x1b]0;pwned\x07");
        assert!(PhaseRecord::from_json(&v.to_string()).is_none());
        let mut v = r.to_json();
        v["agent_id"] = json!(42);
        assert!(PhaseRecord::from_json(&v.to_string()).is_none());
    }

    #[test]
    fn matching_requires_every_identity_field() {
        let m = meta("p1", "claude", "s1", Some("agent-a"));
        let r = rec_for(&m, 16384, "db1", Phase::Working);
        assert!(r.matches("c1", 16384, "db1", &m));
        assert!(!r.matches("c2", 16384, "db1", &m), "other cluster");
        assert!(!r.matches("c1", 16385, "db1", &m), "recreated database");
        assert!(!r.matches("c1", 16384, "db2", &m), "renamed database");
        for other in [
            meta("p2", "claude", "s1", Some("agent-a")),
            meta("p1", "codex", "s1", Some("agent-a")),
            meta("p1", "claude", "s2", Some("agent-a")),
            meta("p1", "claude", "s1", Some("agent-b")),
            meta("p1", "claude", "s1", None),
        ] {
            assert!(!r.matches("c1", 16384, "db1", &other), "{other:?}");
        }
        let mut scoped = m.clone();
        scoped.git_scope = Some("abc".into());
        assert!(!r.matches("c1", 16384, "db1", &scoped), "other worktree");
    }

    #[test]
    fn save_is_0600_atomic_and_list_skips_damage() {
        use std::os::unix::fs::PermissionsExt;
        let home = tmp_home("phase");
        let m = meta("p1", "claude", "s1", None);
        let r = rec_for(&m, 1, "db1", Phase::Working);
        let p = save(&home, "p1|claude|s1", &r).unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(dir(&home)).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::write(dir(&home).join("deadbeefdeadbeef.json"), "{\"schema\":1,").unwrap();
        std::fs::write(dir(&home).join("big.json"), "x".repeat(10_000)).unwrap();
        std::fs::write(dir(&home).join(".partial.tmp"), "junk").unwrap();
        let (ok, bad) = list(&home);
        assert_eq!(ok, vec![r]);
        assert_eq!(bad, 2);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn concurrent_writers_leave_one_whole_record() {
        let home = tmp_home("race");
        let m = meta("p1", "claude", "s1", None);
        let threads: Vec<_> = (0..16)
            .map(|i| {
                let home = home.clone();
                let m = m.clone();
                std::thread::spawn(move || {
                    for j in 0..20 {
                        let mut r = rec_for(&m, 1, "db1", Phase::ALL[(i + j) % 5]);
                        r.reported_at = (i * 100 + j) as u64;
                        save(&home, "p1|claude|s1", &r).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let (ok, bad) = list(&home);
        assert_eq!(ok.len(), 1, "one identity, one file");
        assert_eq!(bad, 0, "no torn writes");
        let leftovers: Vec<_> = std::fs::read_dir(dir(&home))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files cleaned by rename");
        let _ = std::fs::remove_dir_all(home);
    }
}
