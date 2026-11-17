//! `pgboxdb agent doctor`: is this project ready for automatic isolation, and is
//! the current session actually using its branch?
use super::hosts::Host;
use super::install;
use super::project;
use super::session;
use super::Scope;
use crate::cli::ExitCode;
use crate::errors::Result;
use crate::layout::Home;
use crate::out::Out;
use crate::process;
use serde_json::{json, Value};
use std::path::Path;

struct Check {
    name: String,
    status: &'static str,
    detail: String,
    hint: Option<String>,
}

fn ok(name: impl Into<String>, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        status: "ok",
        detail: detail.into(),
        hint: None,
    }
}
fn warn(name: impl Into<String>, detail: impl Into<String>, hint: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        status: "warn",
        detail: detail.into(),
        hint: Some(hint.into()),
    }
}
fn fail(name: impl Into<String>, detail: impl Into<String>, hint: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        status: "fail",
        detail: detail.into(),
        hint: Some(hint.into()),
    }
}

/// Our entries per event in one hook file: (events covered, duplicate events).
fn count_ours(path: &Path) -> Option<(usize, usize, bool)> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc: Value = match serde_json::from_str(&text) {
        Ok(d) => d,
        Err(_) => return Some((0, 0, true)),
    };
    let mut covered = 0;
    let mut dup = 0;
    if let Some(hooks) = doc.get("hooks").and_then(|h| h.as_object()) {
        for (_, list) in hooks {
            let n = list
                .as_array()
                .map(|a| a.iter().filter(|e| install::is_ours(e)).count())
                .unwrap_or(0);
            if n > 0 {
                covered += 1;
            }
            if n > 1 {
                dup += 1;
            }
        }
    }
    Some((covered, dup, false))
}

fn expected_events(host: Host) -> usize {
    match host {
        Host::Claude => 6,
        Host::Cursor => 4,
        Host::Codex => 3,
    }
}

fn hooks_check(host: Host, root: &Path) -> Check {
    let name = format!("hooks:{}", host.as_str());
    let expected = expected_events(host);
    let mut found = Vec::new();
    let mut partial = Vec::new();
    let mut dups = Vec::new();
    let mut broken = Vec::new();
    let mut legacy = Vec::new();
    for (scope, path) in install::host_files(host, root) {
        let Some((covered, dup, invalid)) = count_ours(&path) else {
            continue;
        };
        let label = format!(
            "{} ({})",
            path.display(),
            match scope {
                Scope::Project => "project",
                Scope::Local => "local",
                Scope::User => "user",
            }
        );
        let has_legacy = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .is_some_and(|d| install::has_legacy(&d));
        if has_legacy {
            legacy.push(label.clone());
        }
        if invalid {
            broken.push(label);
        } else if dup > 0 {
            dups.push(label);
        } else if covered == expected {
            found.push(label);
        } else if covered > 0 {
            partial.push(label);
        }
    }
    if !broken.is_empty() {
        return fail(
            name,
            format!("not valid JSON: {}", broken.join(", ")),
            "fix the file by hand",
        );
    }
    if !legacy.is_empty() {
        return warn(
            name,
            format!(
                "hooks from before the rename to PGBoxDB in {} (they do not run pgboxdb)",
                legacy.join(", ")
            ),
            format!(
                "run `pgboxdb agent install {}` for that scope: it replaces them",
                host.as_str()
            ),
        );
    }
    if !dups.is_empty() {
        return warn(
            name,
            format!("duplicate pgboxdb entries in {}", dups.join(", ")),
            "run `pgboxdb agent uninstall` then `pgboxdb agent install` for that scope",
        );
    }
    if found.len() > 1 {
        return warn(
            name,
            format!("installed at more than one scope: {}", found.join(", ")),
            "keep one; the host would run the hook twice (harmless, but noisy)",
        );
    }
    if let Some(f) = found.first() {
        return ok(name, format!("{f}: {expected} hooks"));
    }
    if let Some(p) = partial.first() {
        return warn(
            name,
            format!("partial installation in {p}"),
            if host == Host::Claude {
                "run `pgboxdb agent install claude` to complete it (installations before 0.1.2 lack the SubagentStart/SubagentStop hooks that give subagents their own branches)"
            } else {
                "run `pgboxdb agent install` to complete it"
            },
        );
    }
    warn(
        name,
        "not installed",
        format!(
            "run `pgboxdb agent setup` (or `pgboxdb agent install {}`)",
            host.as_str()
        ),
    )
}

pub fn run(out: &Out, cwd: &Path) -> Result<ExitCode> {
    let mut checks: Vec<Check> = Vec::new();
    let home = Home::resolve();
    let project = project::locate(cwd, &[]);
    let (root, config) = match &project {
        Ok(p) => (p.root.clone(), p.config_file.clone()),
        Err(e) => {
            checks.push(fail("config", e.message.clone(), "fix .pgboxdb.toml"));
            (cwd.to_path_buf(), None)
        }
    };
    if let Ok(p) = &project {
        match &config {
            Some(f) => checks.push(ok(
                "config",
                format!(
                    "{} (source {}, idle TTL {}, grace {})",
                    f.display(),
                    p.source
                        .clone()
                        .unwrap_or_else(|| project::DEFAULT_SOURCE.into()),
                    p.idle_ttl
                        .clone()
                        .unwrap_or_else(|| project::DEFAULT_IDLE_TTL.into()),
                    p.grace_period
                        .clone()
                        .unwrap_or_else(|| project::DEFAULT_GRACE.into()),
                ),
            )),
            None => checks.push(warn(
                "config",
                format!(
                    "no .pgboxdb.toml found from {} (sessions would branch from `app`)",
                    cwd.display()
                ),
                "run `pgboxdb agent setup --source <db>`",
            )),
        }
    }

    // Runtime.
    let ctx = if home.is_initialized() {
        crate::cmd::Ctx::load().ok()
    } else {
        None
    };
    let running = ctx.as_ref().and_then(|c| c.running_pid());
    match (&ctx, running) {
        (None, _) => checks.push(fail(
            "runtime",
            format!("PGBoxDB is not initialised at {}", home.root.display()),
            "run `pgboxdb run` or `pgboxdb agent setup`",
        )),
        (Some(c), Some(pid)) => checks.push(ok(
            "runtime",
            format!("running, pid {pid}, port {}", c.config.port),
        )),
        (Some(_), None) => checks.push(warn(
            "runtime",
            "initialised but stopped",
            "run `pgboxdb start` (hooks start it on demand too)",
        )),
    }
    let sup = std::fs::read_to_string(home.root.join("supervisor.pid"))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());
    match (running, sup) {
        (Some(_), Some(pid)) if process::pid_alive(pid) => {
            checks.push(ok("supervisor", format!("running, pid {pid}")))
        }
        (Some(_), _) => checks.push(warn(
            "supervisor",
            "not running; expired branches are not being removed",
            "run `pgboxdb restart`",
        )),
        _ => {}
    }

    // Source.
    // The same precedence the hook uses: PGBOXDB_SOURCE, then .pgboxdb.toml, then app.
    let source_name = std::env::var("PGBOXDB_SOURCE")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| project.as_ref().ok().and_then(|p| p.source.clone()))
        .unwrap_or_else(|| project::DEFAULT_SOURCE.to_string());
    if let (Some(c), Some(_)) = (&ctx, running) {
        match c.connect("postgres") {
            Ok(mut client) => {
                let exists = crate::names::validate(&source_name)
                    .ok()
                    .and_then(|n| crate::cmd::db::exists(c, &mut client, &n).ok())
                    .unwrap_or(false);
                if !exists {
                    checks.push(fail(
                        "source",
                        format!("{source_name} does not exist"),
                        format!("pgboxdb copy --from \"$DATABASE_URL\" --to {source_name} --schema-only, or pgboxdb create {source_name}"),
                    ));
                } else {
                    let conns =
                        crate::cmd::db::active_connections(&mut client, &source_name).unwrap_or(0);
                    let size: u64 = client
                        .simple_query(&format!(
                            "SELECT pg_database_size({})",
                            crate::pgwire::quote_literal(&source_name)
                        ))
                        .ok()
                        .and_then(|r| r.scalar().and_then(|s| s.parse().ok()))
                        .unwrap_or(0);
                    if conns > 0 {
                        checks.push(fail(
                            "source",
                            format!("{source_name} has {conns} active connection(s)"),
                            "a branch source must be idle: disconnect, or keep a dedicated template nobody connects to",
                        ));
                    } else {
                        checks.push(ok(
                            "source",
                            format!("{source_name} ({:.1} MB, idle)", size as f64 / 1048576.0),
                        ));
                    }
                }
                client.close();
            }
            Err(e) => checks.push(fail(
                "source",
                e.message,
                "is the runtime healthy? `pgboxdb doctor`",
            )),
        }
    } else {
        checks.push(warn(
            "source",
            format!("{source_name}: not checked (runtime down)"),
            "start PGBoxDB",
        ));
    }

    // Hooks per host.
    let mut hosts = install::detect_hosts(&root);
    for h in [Host::Claude, Host::Cursor, Host::Codex] {
        if !hosts.contains(&h)
            && install::host_files(h, &root)
                .iter()
                .any(|(_, p)| p.exists())
        {
            hosts.push(h);
        }
    }
    if hosts.is_empty() {
        hosts = vec![Host::Claude, Host::Cursor, Host::Codex];
    }
    for h in hosts {
        checks.push(hooks_check(h, &root));
    }

    // The current session, when we are inside one.
    let host_var = [
        ("claude", "CLAUDE_CODE_SESSION_ID"),
        ("codex", "CODEX_SESSION_ID"),
    ]
    .into_iter()
    .find_map(|(h, v)| {
        std::env::var(v)
            .ok()
            .filter(|s| !s.is_empty())
            .map(|id| (h, id))
    });
    if let Some((host, id)) = host_var {
        let resolved = session::resolve(&session::ResolveInput {
            id: Some(id.clone()),
            host: Some(host.to_string()),
            cwd: Some(cwd.to_path_buf()),
            ..Default::default()
        });
        match resolved {
            Err(e) => checks.push(fail(
                "session",
                e.message,
                "fix the session id or .pgboxdb.toml",
            )),
            Ok(r) => {
                let store = crate::ttlstore::MetaStore::new(&home.root);
                match session::find_record(&store, &r.key) {
                    Ok(Some(rec)) => {
                        let branch = rec.name.clone();
                        match std::env::var("DATABASE_URL").ok().filter(|s| !s.is_empty()) {
                            Some(url) => {
                                let path = url.split('?').next().unwrap_or("");
                                if path.ends_with(&format!("/{branch}")) {
                                    checks.push(ok("session", format!("{host} session {id}: DATABASE_URL is branch {branch}")));
                                } else {
                                    checks.push(fail(
                                        "session",
                                        format!("{host} session {id}: branch {branch} exists but DATABASE_URL names another database"),
                                        "do not run database commands in this session; `eval \"$(pgboxdb agent env)\"` or start a new session so the hook sets it",
                                    ));
                                }
                            }
                            None => checks.push(warn(
                                "session",
                                format!("{host} session {id}: branch {branch} exists but DATABASE_URL is not set here"),
                                "eval \"$(pgboxdb agent env)\"",
                            )),
                        }
                    }
                    Ok(None) => checks.push(warn(
                        "session",
                        format!("{host} session {id}: no branch yet"),
                        "the SessionStart hook creates one; `pgboxdb agent up` does it by hand",
                    )),
                    Err(e) => checks.push(fail("session", e.message, "see `pgboxdb doctor`")),
                }
            }
        }
    }

    // Agent branches overall.
    let store = crate::ttlstore::MetaStore::new(&home.root);
    let (records, _) = store.list();
    let now = crate::fsutil::now_epoch();
    let agents: Vec<_> = records.iter().filter(|r| r.agent.is_some()).collect();
    let retired = agents
        .iter()
        .filter(|r| r.agent.as_ref().unwrap().retired)
        .count();
    let expired = agents.iter().filter(|r| r.is_expired(now)).count();
    let subagents = agents
        .iter()
        .filter(|r| r.agent.as_ref().unwrap().agent_id.is_some())
        .count();
    checks.push(ok(
        "branches",
        format!(
            "{} agent branch(es) ({} for subagents): {} retired, {} expired awaiting cleanup",
            agents.len(),
            subagents,
            retired,
            expired
        ),
    ));
    // The hook audit log: the last subagent events, so "did the hooks fire?"
    // has an answer without a debugger.
    let log = super::hook::audit_log_path(&home);
    if let Ok(text) = std::fs::read_to_string(&log) {
        let sub: Vec<&str> = text
            .lines()
            .filter(|l| {
                l.contains(" Subagent") || l.contains("-> routed") || l.contains("-> denied")
            })
            .collect();
        let denied = sub.iter().filter(|l| l.contains("-> denied")).count();
        if let Some(last) = sub.last() {
            let detail = format!(
                "{} subagent event(s) logged, {} denied; last: {}",
                sub.len(),
                denied,
                last.split_once(' ').map(|(_, rest)| rest).unwrap_or(last)
            );
            checks.push(if denied > 0 {
                warn("subagents", detail, format!("see {}", log.display()))
            } else {
                ok("subagents", detail)
            });
        }
    }

    let any_fail = checks.iter().any(|c| c.status == "fail");
    let mut text = String::new();
    for c in &checks {
        text.push_str(&format!("{:<5} {:<14} {}\n", c.status, c.name, c.detail));
        if c.status != "ok" {
            if let Some(h) = &c.hint {
                text.push_str(&format!("      {:<14} hint: {h}\n", ""));
            }
        }
    }
    text.push_str(if any_fail {
        "\nproblems found"
    } else {
        "\nno problems found"
    });
    out.emit(
        &text,
        json!({
            "ok": !any_fail,
            "checks": checks.iter().map(|c| json!({"name": c.name, "status": c.status, "detail": c.detail, "hint": c.hint})).collect::<Vec<_>>(),
        }),
    );
    Ok(if any_fail {
        ExitCode::Failure
    } else {
        ExitCode::Ok
    })
}
