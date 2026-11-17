//! `pgboxdb doctor`: every check reports ok / warn / fail with a detail and a hint;
//! exit 1 when anything fails. It never aborts early: an incomplete
//! installation or a missing data directory is a finding, not an error.
use crate::cli::ExitCode;
use crate::cmd::init::{port_is_free, socket_path_len, MAX_SOCKET_PATH};
use crate::config::Config;
use crate::errors::Result;
use crate::layout::{Home, Install};
use crate::out::Out;
use crate::{fsdetect, process};
use serde_json::json;
use std::path::Path;

#[derive(Debug)]
struct Check {
    name: &'static str,
    status: &'static str,
    detail: String,
    hint: Option<String>,
}

fn ok(name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        status: "ok",
        detail: detail.into(),
        hint: None,
    }
}
fn warn(name: &'static str, detail: impl Into<String>, hint: impl Into<String>) -> Check {
    Check {
        name,
        status: "warn",
        detail: detail.into(),
        hint: Some(hint.into()),
    }
}
fn fail(name: &'static str, detail: impl Into<String>, hint: impl Into<String>) -> Check {
    Check {
        name,
        status: "fail",
        detail: detail.into(),
        hint: Some(hint.into()),
    }
}

fn disk_free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(st.f_bavail as u64 * st.f_frsize)
}

fn mb(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.1} GB", b as f64 / (1u64 << 30) as f64)
    } else {
        format!("{} MB", b >> 20)
    }
}

/// Directories under base/ that no pg_database row references. Read-only, and
/// conservative: the runtime must be up (so the catalog is current), pgsql_tmp
/// and non-numeric names are ignored, and anything modified in the last
/// 2 minutes is treated as a CREATE DATABASE that may still be in progress.
pub fn orphan_dirs(data: &Path, known_oids: &[String]) -> Vec<(String, u64)> {
    let base = data.join("base");
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let now = std::time::SystemTime::now();
    let mut out = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.chars().all(|c| c.is_ascii_digit()) || known_oids.contains(&name) {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_dir() {
            continue;
        }
        if let Ok(m) = meta.modified() {
            if now
                .duration_since(m)
                .map(|d| d.as_secs() < 120)
                .unwrap_or(true)
            {
                continue;
            }
        }
        let size = dir_size(&e.path());
        out.push((name, size));
    }
    out.sort();
    out
}

fn dir_size(p: &Path) -> u64 {
    std::fs::read_dir(p)
        .map(|rd| {
            rd.flatten()
                .map(|e| {
                    e.metadata()
                        .map(|m| {
                            if m.is_dir() {
                                dir_size(&e.path())
                            } else {
                                m.len()
                            }
                        })
                        .unwrap_or(0)
                })
                .sum()
        })
        .unwrap_or(0)
}

fn orphan_check(data: &Path, known: &[String]) -> Check {
    let orphans = orphan_dirs(data, known);
    if orphans.is_empty() {
        return ok(
            "orphan_dirs",
            "every base/<oid> directory belongs to a database",
        );
    }
    let total: u64 = orphans.iter().map(|(_, s)| *s).sum();
    let names: Vec<String> = orphans
        .iter()
        .map(|(n, s)| format!("{n} ({})", mb(*s)))
        .collect();
    warn(
        "orphan_dirs",
        format!("{} base/<oid> directory(ies) not referenced by pg_database, {} in total: {}", orphans.len(), mb(total), names.join(", ")),
        format!("left by a CREATE DATABASE or clone interrupted by a crash; pgboxdb never deletes them. After confirming nothing is creating a database, remove them by hand from {}/base", data.display()),
    )
}

/// TTL diagnostics: management status, supervisor liveness, metadata validity,
/// databases awaiting cleanup, and failed cleanups. Read-only — never deletes.
fn ttl_checks(home: &Home, running: bool) -> Vec<Check> {
    let store = crate::ttlstore::MetaStore::new(&home.root);
    let (records, bad) = store.list();
    let now = crate::fsutil::now_epoch();
    let mut checks = Vec::new();

    // Supervisor liveness from its pid file (read-only; the lock stays untouched).
    let sup_pid = std::fs::read_to_string(home.root.join("supervisor.pid"))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());
    match sup_pid {
        Some(pid) if process::pid_alive(pid) => checks.push(ok(
            "ttl_worker",
            format!("cleanup supervisor running, pid {pid}"),
        )),
        _ if records.is_empty() => checks.push(ok(
            "ttl_worker",
            "no temporary databases; supervisor idle/not needed",
        )),
        _ if running => checks.push(warn(
            "ttl_worker",
            "temporary databases exist but no cleanup supervisor is running",
            "`pgboxdb start` (or `pgboxdb run`) respawns it",
        )),
        _ => checks.push(ok(
            "ttl_worker",
            "runtime stopped; supervisor exits with it",
        )),
    }

    if records.is_empty() {
        checks.push(ok(
            "ttl",
            format!("no TTL-managed databases ({})", store.dir().display()),
        ));
    } else {
        let expired = records.iter().filter(|r| r.is_expired(now)).count();
        checks.push(ok(
            "ttl",
            format!(
                "{} temporary database(s), {expired} past their deadline awaiting cleanup",
                records.len()
            ),
        ));
    }

    if !bad.is_empty() {
        let names: Vec<String> = bad
            .iter()
            .map(|(p, e)| format!("{}: {e}", p.display()))
            .collect();
        checks.push(warn(
            "ttl_metadata",
            format!(
                "{} unreadable TTL record(s): {}",
                bad.len(),
                names.join("; ")
            ),
            "a corrupt record is never treated as deletable; remove it by hand if stale",
        ));
    }

    let failed: Vec<&crate::ttlstore::TtlRecord> = records
        .iter()
        .filter(|r| r.state == crate::ttlstore::State::RetryPending)
        .collect();
    if !failed.is_empty() {
        let names: Vec<String> = failed
            .iter()
            .map(|r| format!("{} ({})", r.name, r.last_error.as_deref().unwrap_or("?")))
            .collect();
        checks.push(warn(
            "ttl_cleanup",
            format!(
                "{} cleanup(s) failed and will retry: {}",
                failed.len(),
                names.join("; ")
            ),
            "inspect the runtime log; the database is retried with backoff",
        ));
    }
    checks
}

pub fn run(out: &Out) -> Result<ExitCode> {
    let mut checks: Vec<Check> = Vec::new();
    let home = Home::resolve();

    // Installation.
    let install = match Install::discover() {
        Ok(i) => Some(i),
        Err(e) => {
            checks.push(fail("install", e.message.clone(), "reinstall PGBoxDB"));
            None
        }
    };
    if let Some(install) = &install {
        let problems = install.verify();
        if problems.is_empty() {
            checks.push(ok(
                "install",
                format!("complete at {}", install.prefix.display()),
            ));
        } else {
            let list: Vec<&str> = problems.iter().map(|p| p.what.as_str()).collect();
            checks.push(fail(
                "install",
                format!(
                    "incomplete at {}: missing {}",
                    install.prefix.display(),
                    list.join(", ")
                ),
                "reinstall PGBoxDB (the distribution layout is in share/pgboxdb/layout.md)",
            ));
        }
        match install.dist_info() {
            Ok(d) => checks.push(ok(
                "dist",
                format!(
                    "pgboxdb {} engine {} PostgreSQL {} {}/{}",
                    d.pgboxdb_version, d.engine_commit, d.postgres_compat, d.os, d.arch
                ),
            )),
            Err(e) => checks.push(fail("dist", e.message.clone(), "reinstall PGBoxDB")),
        }
        match std::process::Command::new(&install.engine)
            .arg("--version")
            .output()
        {
            Ok(o) if o.status.success() => checks.push(ok(
                "engine",
                String::from_utf8_lossy(&o.stdout).trim().to_string(),
            )),
            Ok(o) => checks.push(fail(
                "engine",
                format!(
                    "{} exited {}: {}",
                    install.engine.display(),
                    o.status,
                    String::from_utf8_lossy(&o.stderr).trim()
                ),
                "the engine binary does not run on this machine; reinstall for this OS and CPU",
            )),
            Err(e) => checks.push(fail(
                "engine",
                format!("cannot run {}: {e}", install.engine.display()),
                "check the file exists and is executable",
            )),
        }
    }

    // Data directory and config.
    let initialized = home.is_initialized();
    if initialized {
        let ver = std::fs::read_to_string(home.data.join("PG_VERSION"))
            .unwrap_or_default()
            .trim()
            .to_string();
        checks.push(ok(
            "data_dir",
            format!("{} (PostgreSQL {ver} format)", home.data.display()),
        ));
    } else if home.data.exists() || home.config_file.exists() {
        checks.push(fail(
            "data_dir",
            format!(
                "{} is incomplete (data dir or config.json missing)",
                home.root.display()
            ),
            "`pgboxdb init --force` recreates it (deletes every database)",
        ));
    } else {
        checks.push(fail(
            "data_dir",
            format!("not initialised at {}", home.root.display()),
            crate::legacy::uninitialised_hint(&home).unwrap_or_else(|| "run `pgboxdb init`".into()),
        ));
    }
    if initialized {
        if let Some(old) = crate::legacy::initialised_home() {
            let canon =
                |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            if canon(&old.root) != canon(&home.root) {
                checks.push(warn(
                    "previous_home",
                    format!(
                        "databases from before the rename are still at {} (not used by this installation)",
                        old.root.display()
                    ),
                    "nothing is lost; to use them, set PGBOXDB_HOME to that path (`pgboxdb migrate` moves a home only into a free location)",
                ));
            }
        }
    }
    let config = if home.config_file.exists() {
        match Config::load(&home.config_file) {
            Ok(c) => {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&home.config_file)
                    .map(|m| m.permissions().mode() & 0o777)
                    .unwrap_or(0);
                if mode & 0o077 != 0 {
                    checks.push(warn(
                        "config",
                        format!(
                            "{} has mode {mode:o}; it holds the superuser password",
                            home.config_file.display()
                        ),
                        format!("chmod 600 {}", home.config_file.display()),
                    ));
                } else {
                    checks.push(ok(
                        "config",
                        format!("port {}, durability {}", c.port, c.durability.as_str()),
                    ));
                }
                Some(c)
            }
            Err(e) => {
                checks.push(fail(
                    "config",
                    e.message.clone(),
                    "`pgboxdb init --force` recreates it",
                ));
                None
            }
        }
    } else {
        None
    };

    // Socket path, runtime, port.
    let sock_dir = config
        .as_ref()
        .and_then(|c| c.run_dir.clone())
        .unwrap_or_else(|| home.run.clone());
    let port = config.as_ref().map(|c| c.port).unwrap_or(5432);
    let len = socket_path_len(&sock_dir, port);
    if len > MAX_SOCKET_PATH {
        checks.push(fail(
            "socket_path",
            format!(
                "{}/.s.PGSQL.{port} is {len} bytes (limit {MAX_SOCKET_PATH})",
                sock_dir.display()
            ),
            "set PGBOXDB_RUN_DIR to a short directory and `pgboxdb init --force`",
        ));
    } else {
        checks.push(ok(
            "socket_path",
            format!("{}/.s.PGSQL.{port} ({len} bytes)", sock_dir.display()),
        ));
    }
    let mut running_pid = None;
    match process::read_pid_file(&home.data) {
        Some(pf) if process::is_our_engine(pf.pid, &home.data) => {
            running_pid = Some(pf.pid);
            checks.push(ok("runtime", format!("running, pid {}", pf.pid)));
        }
        Some(pf) => checks.push(warn(
            "runtime",
            format!(
                "not running; stale postmaster.pid names pid {} ({})",
                pf.pid,
                if process::pid_alive(pf.pid) {
                    "alive, not PGBoxDB"
                } else {
                    "dead"
                }
            ),
            "`pgboxdb start` removes it",
        )),
        None if initialized => checks.push(ok("runtime", "not running")),
        None => checks.push(warn("runtime", "no data directory", "run `pgboxdb init`")),
    }
    if running_pid.is_some() {
        checks.push(ok("port", format!("{port} in use by PGBoxDB")));
    } else if port_is_free(port) {
        checks.push(ok("port", format!("{port} is free")));
    } else {
        checks.push(fail(
            "port",
            format!("{port} is in use by another process"),
            "stop that process or `pgboxdb init --force --port N`",
        ));
    }

    // TTL management.
    if initialized {
        checks.extend(ttl_checks(&home, running_pid.is_some()));
        // Agent branches (pgboxdb agent): counted here, diagnosed by `pgboxdb agent doctor`.
        {
            let (records, _) = crate::ttlstore::MetaStore::new(&home.root).list();
            let now = crate::fsutil::now_epoch();
            let agents: Vec<_> = records.iter().filter(|r| r.agent.is_some()).collect();
            let retired = agents
                .iter()
                .filter(|r| r.agent.as_ref().unwrap().retired)
                .count();
            let expired = agents.iter().filter(|r| r.is_expired(now)).count();
            checks.push(ok(
                "agents",
                format!(
                    "{} agent branch(es): {retired} retired, {expired} expired awaiting cleanup (`pgboxdb agent doctor` checks a project)",
                    agents.len()
                ),
            ));
        }
    }

    // Host resources.
    let probe_dir = if home.data.is_dir() {
        home.data.clone()
    } else if home.root.is_dir() {
        home.root.clone()
    } else {
        std::env::temp_dir()
    };
    match disk_free_bytes(&probe_dir) {
        Some(b) if b < 100 << 20 => checks.push(fail(
            "disk_free",
            format!("{} free at {}", mb(b), probe_dir.display()),
            "free disk space; the engine stops writes at 53100 when the disk is full",
        )),
        Some(b) if b < 1 << 30 => checks.push(warn(
            "disk_free",
            format!("{} free at {}", mb(b), probe_dir.display()),
            "less than 1 GB free",
        )),
        Some(b) => checks.push(ok(
            "disk_free",
            format!("{} free at {}", mb(b), probe_dir.display()),
        )),
        None => checks.push(warn(
            "disk_free",
            format!("cannot statvfs {}", probe_dir.display()),
            "",
        )),
    }
    let mut rl = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_STACK, &mut rl) } == 0 {
        let hard_ok = rl.rlim_max == libc::RLIM_INFINITY || rl.rlim_max >= process::STACK_BYTES;
        if hard_ok {
            checks.push(ok("stack_limit", format!("hard limit {} (engine needs 64 MB; pgboxdb raises the soft limit when it starts the engine)", if rl.rlim_max == libc::RLIM_INFINITY { "unlimited".to_string() } else { mb(rl.rlim_max) })));
        } else {
            checks.push(fail(
                "stack_limit",
                format!("hard stack limit {} is below 64 MB", mb(rl.rlim_max)),
                "raise it (`ulimit -Hs 65520`, or /etc/security/limits.conf) before `pgboxdb start`",
            ));
        }
    }
    let fs_name = fsdetect::fs_type_name(&probe_dir);
    if fsdetect::supports_block_clone(&probe_dir) {
        checks.push(ok(
            "filesystem",
            format!("{fs_name} with block cloning: `pgboxdb clone` shares blocks with the source"),
        ));
    } else {
        checks.push(warn(
            "filesystem",
            format!("{fs_name} without block cloning: `pgboxdb clone` copies files"),
            "APFS, ZFS, btrfs or XFS with reflinks make clones copy-on-write",
        ));
    }

    // Libraries, orphan directories and log, when running.
    if let (Some(pid), Some(cfg)) = (running_pid, config.as_ref()) {
        let _ = pid;
        match crate::pgwire::Client::connect_unix(
            &sock_dir,
            cfg.port,
            "postgres",
            "postgres",
            std::time::Duration::from_secs(10),
        ) {
            Ok(mut client) => {
                let status: serde_json::Value = client
                    .simple_query("SELECT pgrust_runtime_status()")
                    .ok()
                    .and_then(|r| r.scalar().map(String::from))
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                let known: Option<Vec<String>> = client
                    .simple_query("SELECT oid::text FROM pg_database")
                    .ok()
                    .map(|r| r.rows.iter().filter_map(|row| row[0].clone()).collect());
                let invalid: Vec<String> = client
                    .simple_query("SELECT datname FROM pg_database WHERE datconnlimit = -2")
                    .map(|r| r.rows.iter().filter_map(|row| row[0].clone()).collect())
                    .unwrap_or_default();
                client.close();
                if let Some(known) = known {
                    checks.push(orphan_check(&home.data, &known));
                }
                if !invalid.is_empty() {
                    checks.push(warn(
                        "invalid_databases",
                        format!(
                            "{} database(s) marked invalid by an interrupted DROP DATABASE: {}",
                            invalid.len(),
                            invalid.join(", ")
                        ),
                        "drop them again: `pgboxdb drop <name> --force`",
                    ));
                }
                for (lib, feature, pkg) in [
                    ("libxml2", "the xml type and SQL/XML functions", "libxml2"),
                    ("icu", "ICU collations", "libicu"),
                ] {
                    let l = &status["libraries"][lib];
                    let name: &'static str = if lib == "libxml2" { "libxml2" } else { "icu" };
                    if l["available"].as_bool() == Some(true) {
                        let v = l
                            .get("version")
                            .and_then(|v| v.as_str())
                            .or_else(|| l.get("unicode_version").and_then(|v| v.as_str()))
                            .unwrap_or("");
                        checks.push(ok(name, format!("available {v}")));
                    } else {
                        checks.push(warn(
                            name,
                            format!(
                                "not found: {feature} are unavailable ({})",
                                l["error"].as_str().unwrap_or("not probed")
                            ),
                            format!("install the {pkg} package"),
                        ));
                    }
                }
            }
            Err(e) => checks.push(fail(
                "connect",
                format!("cannot connect to the running engine: {e}"),
                "see the log",
            )),
        }
    }
    // PostgreSQL client tools (only `pgboxdb copy` needs them; not bundled).
    match crate::pgtools::locate() {
        Ok(t) => checks.push(ok(
            "client_tools",
            format!(
                "pg_dump/pg_restore {} in {} (for `pgboxdb copy`)",
                t.version,
                t.dir.display()
            ),
        )),
        Err(e) => checks.push(warn(
            "client_tools",
            "pg_dump/pg_restore not found; `pgboxdb copy` (import from PostgreSQL) needs them, nothing else does",
            e.hint.unwrap_or_default(),
        )),
    }
    if home.log_file.exists() {
        let tail = process::log_tail(&home, 200);
        let bad: Vec<&str> = tail
            .lines()
            .filter(|l| l.contains("PANIC") || l.contains("FATAL"))
            .collect();
        if bad.is_empty() {
            checks.push(ok(
                "log",
                format!(
                    "{} (no FATAL/PANIC in the last 200 lines)",
                    home.log_file.display()
                ),
            ));
        } else {
            checks.push(warn(
                "log",
                format!(
                    "{} FATAL/PANIC line(s) in the last 200 lines of {}: {}",
                    bad.len(),
                    home.log_file.display(),
                    bad.last().unwrap_or(&"")
                ),
                "read the log",
            ));
        }
    }

    let any_fail = checks.iter().any(|c| c.status == "fail");
    let mut text = String::new();
    for c in &checks {
        text.push_str(&format!("{:<5} {:<12} {}\n", c.status, c.name, c.detail));
        if c.status != "ok" {
            if let Some(h) = &c.hint {
                if !h.is_empty() {
                    text.push_str(&format!("      {:<12} hint: {h}\n", ""));
                }
            }
        }
    }
    text.push_str(if any_fail {
        "\nproblems found"
    } else {
        "\nno problems found"
    });
    let value = json!({
        "ok": !any_fail,
        "checks": checks.iter().map(|c| json!({"name": c.name, "status": c.status, "detail": c.detail, "hint": c.hint})).collect::<Vec<_>>(),
    });
    out.emit(&text, value);
    Ok(if any_fail {
        ExitCode::Failure
    } else {
        ExitCode::Ok
    })
}
