//! `pgboxdb agent install|uninstall|setup`: hook configuration for Claude Code,
//! Cursor and Codex, the project's `.pgboxdb.toml`, and the one-shot setup the
//! Skill runs. Hook files are merged, never rewritten: entries that are not
//! ours stay byte-for-byte where they were.
use super::hosts::Host;
use super::project::{self, ProjectConfig};
use super::session::{self, ResolveInput};
use super::{InstallOpts, Scope, SetupOpts};
use crate::cli::ExitCode;
use crate::errors::{PgboxdbError, Result};
use crate::layout::Home;
use crate::out::Out;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Every entry we write contains this; `uninstall` and `doctor` look for it.
pub const MARKER: &str = "pgboxdb agent hook";
/// Where the installer and the usual package managers put `pgboxdb`, appended to
/// the hook's PATH so a Dock-launched editor finds it too.
pub const PATH_SUFFIX: &str = "$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin";

/// The guarded command: without `pgboxdb` on the PATH it is a silent no-op, so a
/// committed hook file is harmless for a teammate who has not installed PGBoxDB.
pub fn hook_command(host: Host) -> String {
    // The Claude file stays host-less: Cursor's compat loader reads it too and
    // the payload decides. Cursor's and Codex's own files name their host, so
    // detection never depends on environment variables those hosts may omit.
    let flag = match host {
        Host::Claude => "",
        Host::Cursor => " --host cursor",
        Host::Codex => " --host codex",
    };
    format!(
        "/bin/sh -c 'PATH=\"$PATH:{PATH_SUFFIX}\"; command -v pgboxdb >/dev/null 2>&1 && exec pgboxdb agent hook{flag}; exit 0'"
    )
}

/// The events each host gets, with their timeouts (seconds).
fn events(host: Host) -> Vec<(&'static str, Option<&'static str>, u64)> {
    match host {
        // PreToolUse(Bash) extends the session's TTL and routes a subagent's
        // command to its own branch (ensuring it first, so a clone may be
        // needed: hence the longer timeout). SubagentStart/Stop give each
        // subagent its branch and retire it.
        Host::Claude => vec![
            ("SessionStart", None, 30),
            ("UserPromptSubmit", None, 5),
            ("PreToolUse", Some("Bash"), 15),
            ("SubagentStart", None, 30),
            ("SubagentStop", None, 10),
            ("SessionEnd", None, 10),
        ],
        // Touch hooks sit on non-permission events so they can never influence approvals.
        Host::Cursor => vec![
            ("sessionStart", None, 30),
            ("beforeSubmitPrompt", None, 5),
            ("afterShellExecution", None, 5),
            ("sessionEnd", None, 10),
        ],
        Host::Codex => vec![
            ("SessionStart", None, 30),
            ("UserPromptSubmit", None, 5),
            ("SessionEnd", None, 10),
        ],
    }
}

fn entry(host: Host, matcher: Option<&str>, timeout: u64) -> Value {
    match host {
        Host::Cursor => json!({"command": hook_command(host), "timeout": timeout}),
        _ => {
            let mut e = json!({"hooks": [{"type": "command", "command": hook_command(host), "timeout": timeout}]});
            if let Some(m) = matcher {
                e["matcher"] = json!(m);
            }
            e
        }
    }
}

fn user_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

/// The file a host reads at a scope.
pub fn host_file(host: Host, scope: Scope, root: &Path) -> PathBuf {
    let home = user_home();
    match (host, scope) {
        (Host::Claude, Scope::Project) => root.join(".claude/settings.json"),
        (Host::Claude, Scope::Local) => root.join(".claude/settings.local.json"),
        (Host::Claude, Scope::User) => home.join(".claude/settings.json"),
        (Host::Cursor, Scope::User) => home.join(".cursor/hooks.json"),
        (Host::Cursor, _) => root.join(".cursor/hooks.json"),
        (Host::Codex, Scope::User) => home.join(".codex/hooks.json"),
        (Host::Codex, _) => root.join(".codex/hooks.json"),
    }
}

/// Every file a host might read for this project, for `doctor`.
pub fn host_files(host: Host, root: &Path) -> Vec<(Scope, PathBuf)> {
    let mut v = vec![(Scope::Project, host_file(host, Scope::Project, root))];
    if host == Host::Claude {
        v.push((Scope::Local, host_file(host, Scope::Local, root)));
    }
    v.push((Scope::User, host_file(host, Scope::User, root)));
    v
}

/// Hosts present on this machine or in this project.
pub fn detect_hosts(root: &Path) -> Vec<Host> {
    let home = user_home();
    let mut v = Vec::new();
    for (host, dir) in [
        (Host::Claude, ".claude"),
        (Host::Cursor, ".cursor"),
        (Host::Codex, ".codex"),
    ] {
        if home.join(dir).is_dir() || root.join(dir).is_dir() {
            v.push(host);
        }
    }
    v
}

fn parse_hosts(names: &[String], root: &Path, what: &str) -> Result<Vec<Host>> {
    let mut hosts = Vec::new();
    for n in names {
        let h = Host::parse(n)?;
        if !hosts.contains(&h) {
            hosts.push(h);
        }
    }
    if hosts.is_empty() {
        hosts = detect_hosts(root);
    }
    if hosts.is_empty() {
        return Err(PgboxdbError::usage(format!(
            "no agent host detected (no ~/.claude, ~/.cursor or ~/.codex, and none in the project); name one: pgboxdb agent {what} claude|cursor|codex"
        )));
    }
    Ok(hosts)
}

fn empty_doc(host: Host) -> Value {
    match host {
        Host::Cursor => json!({"version": 1, "hooks": {}}),
        _ => json!({}),
    }
}

fn load_doc(path: &Path, host: Host) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            PgboxdbError::failure(format!(
                "{} is not valid JSON ({e}); will not touch it",
                path.display()
            ))
            .hint("fix the file by hand, then run the command again")
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(empty_doc(host)),
        Err(e) => Err(PgboxdbError::io("cannot read", path, &e)),
    }
}

fn write_doc(path: &Path, doc: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| PgboxdbError::io("cannot create", parent, &e))?;
    }
    let tmp = path.with_extension("json.pgboxdb-tmp");
    let text = serde_json::to_string_pretty(doc).expect("json") + "\n";
    std::fs::write(&tmp, text).map_err(|e| PgboxdbError::io("cannot write", &tmp, &e))?;
    std::fs::rename(&tmp, path).map_err(|e| PgboxdbError::io("cannot rename into place", path, &e))
}

/// Ours: the `command` of a Cursor entry, or any `hooks[].command` of a
/// Claude/Codex group, runs `pgboxdb agent hook`. Messages or comments that merely
/// mention the marker do not count.
pub fn is_our_command(cmd: &Value) -> bool {
    cmd.as_str().is_some_and(|c| c.contains(MARKER))
}

/// A hook command written before the rename (it runs the previous CLI).
/// `install` replaces such entries and `uninstall` removes them; they are
/// never kept beside ours, which would give a session two databases.
pub fn is_legacy_command(cmd: &Value) -> bool {
    cmd.as_str()
        .is_some_and(|c| c.contains(crate::legacy::AGENT_HOOK_MARKER) && !c.contains(MARKER))
}

/// Any hook entry in a settings document written before the rename.
pub fn has_legacy(doc: &Value) -> bool {
    doc["hooks"].as_object().is_some_and(|h| {
        h.values()
            .filter_map(|l| l.as_array())
            .flatten()
            .any(is_legacy)
    })
}

pub fn is_legacy(entry: &Value) -> bool {
    is_legacy_command(&entry["command"])
        || entry["hooks"]
            .as_array()
            .is_some_and(|hs| hs.iter().any(|h| is_legacy_command(&h["command"])))
}

pub fn is_ours(entry: &Value) -> bool {
    if is_our_command(&entry["command"]) {
        return true;
    }
    entry["hooks"]
        .as_array()
        .is_some_and(|hs| hs.iter().any(|h| is_our_command(&h["command"])))
}

/// Add our entries where missing. Returns (added, already present).
fn merge_install(doc: &mut Value, host: Host) -> (usize, usize) {
    if !doc.is_object() {
        *doc = empty_doc(host);
    }
    if host == Host::Cursor && doc.get("version").is_none() {
        doc["version"] = json!(1);
    }
    if !doc.get("hooks").is_some_and(|h| h.is_object()) {
        doc["hooks"] = json!({});
    }
    // Entries written before the rename are replaced by ours below.
    if has_legacy(doc) {
        remove_matching(doc, is_legacy_command, false);
    }
    let (mut added, mut present) = (0, 0);
    for (event, matcher, timeout) in events(host) {
        let list = doc["hooks"]
            .as_object_mut()
            .unwrap()
            .entry(event)
            .or_insert_with(|| json!([]));
        if !list.is_array() {
            *list = json!([]);
        }
        if list.as_array().unwrap().iter().any(is_ours) {
            present += 1;
        } else {
            list.as_array_mut()
                .unwrap()
                .push(entry(host, matcher, timeout));
            added += 1;
        }
    }
    (added, present)
}

/// Remove our hooks everywhere in `hooks`: a Cursor entry whose command is
/// ours, or our `hooks[]` items inside a Claude/Codex group (a foreign hook
/// sharing the group stays). Groups and event arrays left empty are dropped.
/// Returns the number of hook commands removed.
fn merge_uninstall(doc: &mut Value) -> usize {
    remove_matching(doc, |c| is_our_command(c) || is_legacy_command(c), true)
}

/// Remove hook commands for which `matches` holds. Groups emptied by that
/// removal are dropped, and so are event arrays left empty. With
/// `drop_empty_groups` (uninstall, the historical behaviour) every empty
/// group goes; without it (install replacing entries from before the
/// rename) a foreign empty group is left exactly as it was.
fn remove_matching(
    doc: &mut Value,
    matches: impl Fn(&Value) -> bool,
    drop_empty_groups: bool,
) -> usize {
    let mut removed = 0;
    let Some(hooks) = doc.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return 0;
    };
    let mut empty = Vec::new();
    for (event, list) in hooks.iter_mut() {
        let Some(arr) = list.as_array_mut() else {
            continue;
        };
        let removed_before = removed;
        let mut emptied_by_us = vec![false; arr.len()];
        for (i, entry) in arr.iter_mut().enumerate() {
            if let Some(inner) = entry.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                let before = inner.len();
                inner.retain(|h| !matches(&h["command"]));
                removed += before - inner.len();
                emptied_by_us[i] = before > 0 && inner.is_empty();
            }
        }
        let cursor_ours = arr.iter().filter(|e| matches(&e["command"])).count();
        removed += cursor_ours;
        let mut i = 0;
        arr.retain(|e| {
            let emptied = e
                .get("hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|h| h.is_empty())
                && (drop_empty_groups || emptied_by_us[i]);
            i += 1;
            !(matches(&e["command"]) || emptied)
        });
        if arr.is_empty() && (drop_empty_groups || removed > removed_before) {
            empty.push(event.clone());
        }
    }
    for e in empty {
        hooks.remove(&e);
    }
    removed
}

#[derive(Debug, Clone)]
pub struct TomlReport {
    pub path: PathBuf,
    pub written: bool,
    pub source: String,
}

/// Write `.pgboxdb.toml` when absent. An existing file wins; a different
/// `--source` against it is a conflict, never a silent overwrite.
pub fn ensure_toml(
    project: &ProjectConfig,
    source: Option<&str>,
    idle_ttl: Option<&str>,
) -> Result<TomlReport> {
    if let Some(existing) = &project.config_file {
        let current = project
            .source
            .clone()
            .unwrap_or_else(|| project::DEFAULT_SOURCE.to_string());
        if let Some(s) = source {
            if s != current {
                return Err(PgboxdbError::new(
                    ExitCode::Conflict,
                    format!(
                        "{} already names source {current:?}; refusing to change it to {s:?}",
                        existing.display()
                    ),
                )
                .hint("edit .pgboxdb.toml by hand if the source really changed"));
            }
        }
        return Ok(TomlReport {
            path: existing.clone(),
            written: false,
            source: current,
        });
    }
    let source = source.unwrap_or(project::DEFAULT_SOURCE).to_string();
    crate::names::validate(&source)?;
    let ttl = idle_ttl.unwrap_or(project::DEFAULT_IDLE_TTL);
    crate::ttl::parse_ttl(ttl)?;
    let path = project.root.join(project::CONFIG_FILE);
    std::fs::write(
        &path,
        project::render_pgboxdb_toml(&source, ttl, project::DEFAULT_GRACE),
    )
    .map_err(|e| PgboxdbError::io("cannot write", &path, &e))?;
    Ok(TomlReport {
        path,
        written: true,
        source,
    })
}

#[derive(Debug, Clone)]
pub struct Selftest {
    pub ok: bool,
    pub pgboxdb: Option<String>,
    pub version: Option<String>,
    pub hint: Option<String>,
}

/// Resolve `pgboxdb` exactly as the installed hook command will, and ask it for
/// its version, so a PATH problem shows up now instead of in a silent hook.
pub fn selftest() -> Selftest {
    let script = format!("PATH=\"$PATH:{PATH_SUFFIX}\"; command -v pgboxdb");
    let found = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(&script)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    let Some(path) = found else {
        return Selftest {
            ok: false,
            pgboxdb: None,
            version: None,
            hint: Some(format!(
                "the hook could not find `pgboxdb` on PATH (or in {PATH_SUFFIX}); install PGBoxDB with `npm i -g pgboxdb` or `curl -fsSL https://pgboxdb.com/install | sh`, or link the binary into ~/.local/bin"
            )),
        };
    };
    let version = std::process::Command::new(&path)
        .args(["--json", "version"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .and_then(|v| v["pgboxdb_version"].as_str().map(String::from));
    match version {
        Some(v) => Selftest {
            ok: true,
            pgboxdb: Some(path),
            version: Some(v),
            hint: None,
        },
        None => Selftest {
            ok: false,
            pgboxdb: Some(path.clone()),
            version: None,
            hint: Some(format!(
                "{path} did not answer `pgboxdb --json version`; reinstall PGBoxDB"
            )),
        },
    }
}

fn selftest_json(s: &Selftest) -> Value {
    json!({"ok": s.ok, "pgboxdb": s.pgboxdb, "version": s.version, "hint": s.hint})
}

#[derive(Debug, Clone)]
pub struct HostReport {
    pub host: Host,
    pub file: PathBuf,
    pub added: usize,
    pub present: usize,
    pub removed: usize,
}

fn host_json(h: &HostReport, uninstall: bool) -> Value {
    if uninstall {
        json!({"host": h.host.as_str(), "file": h.file, "removed": h.removed})
    } else {
        json!({"host": h.host.as_str(), "file": h.file, "added": h.added, "present": h.present})
    }
}

/// Install hook entries for `hosts` at `scope`.
pub fn install_hosts(hosts: &[Host], scope: Scope, root: &Path) -> Result<Vec<HostReport>> {
    let mut reports = Vec::new();
    for &host in hosts {
        let file = host_file(host, scope, root);
        let mut doc = load_doc(&file, host)?;
        let (added, present) = merge_install(&mut doc, host);
        if added > 0 {
            write_doc(&file, &doc)?;
        }
        reports.push(HostReport {
            host,
            file,
            added,
            present,
            removed: 0,
        });
    }
    Ok(reports)
}

pub fn install(out: &Out, opts: InstallOpts, cwd: &Path) -> Result<ExitCode> {
    let project = project::locate(cwd, &[])?;
    let hosts = parse_hosts(&opts.hosts, &project.root, "install")?;
    let toml = ensure_toml(&project, opts.source.as_deref(), opts.ttl.as_deref())?;
    let reports = install_hosts(&hosts, opts.scope, &project.root)?;
    let st = selftest();
    let mut text = String::new();
    for r in &reports {
        text.push_str(&format!(
            "{:<8} {}  ({} added, {} already present)\n",
            r.host.as_str(),
            r.file.display(),
            r.added,
            r.present
        ));
    }
    text.push_str(&format!(
        "config   {} ({}, source {})\n",
        toml.path.display(),
        if toml.written { "written" } else { "kept" },
        toml.source
    ));
    text.push_str(&selftest_line(&st));
    out.emit(
        text.trim_end(),
        json!({
            "hosts": reports.iter().map(|r| host_json(r, false)).collect::<Vec<_>>(),
            "toml": {"path": toml.path, "written": toml.written, "source": toml.source},
            "selftest": selftest_json(&st),
        }),
    );
    Ok(ExitCode::Ok)
}

fn selftest_line(st: &Selftest) -> String {
    match (&st.ok, &st.pgboxdb, &st.version, &st.hint) {
        (true, Some(p), Some(v), _) => format!("hook     resolves pgboxdb {v} at {p}\n"),
        (_, _, _, Some(h)) => format!("hook     WARNING: {h}\n"),
        _ => String::new(),
    }
}

pub fn uninstall(out: &Out, opts: InstallOpts, cwd: &Path) -> Result<ExitCode> {
    let project = project::locate(cwd, &[])?;
    let hosts = parse_hosts(&opts.hosts, &project.root, "uninstall")?;
    let mut reports = Vec::new();
    for host in hosts {
        let file = host_file(host, opts.scope, &project.root);
        let removed = if file.exists() {
            let mut doc = load_doc(&file, host)?;
            let n = merge_uninstall(&mut doc);
            if n > 0 {
                write_doc(&file, &doc)?;
            }
            n
        } else {
            0
        };
        reports.push(HostReport {
            host,
            file,
            added: 0,
            present: 0,
            removed,
        });
    }
    let text: String = reports
        .iter()
        .map(|r| {
            format!(
                "{:<8} {}  ({} removed)\n",
                r.host.as_str(),
                r.file.display(),
                r.removed
            )
        })
        .collect();
    out.emit(
        text.trim_end(),
        json!({"hosts": reports.iter().map(|r| host_json(r, true)).collect::<Vec<_>>()}),
    );
    Ok(ExitCode::Ok)
}

/// One-shot project setup: runtime up, source ready and idle, `.pgboxdb.toml`
/// written, hooks installed, one real branch created and removed as proof.
pub fn setup(out: &Out, opts: SetupOpts, cwd: &Path) -> Result<ExitCode> {
    let quiet = out.quiet();
    let home = Home::resolve();
    let initialized = if home.is_initialized() {
        false
    } else {
        crate::legacy::guard_new_home(&home)?;
        out.note("initialising a new PGBoxDB cluster…");
        let port = std::env::var("PGBOXDB_PORT")
            .ok()
            .and_then(|p| p.parse().ok());
        let durability = std::env::var("PGBOXDB_DURABILITY").ok();
        crate::cmd::init::run(&quiet, port, durability, false)?;
        true
    };
    let ctx = crate::cmd::Ctx::load()?;
    let started = crate::cmd::start::ensure_running(&quiet, &ctx)?;
    crate::cmd::start::spawn_supervisor(&ctx.home);

    let project = project::locate(cwd, &[])?;
    let source_name = opts
        .source
        .clone()
        .or_else(|| project.source.clone())
        .unwrap_or_else(|| project::DEFAULT_SOURCE.to_string());
    let source = crate::names::validate_user_target(&source_name)?;
    let mut client = ctx.connect("postgres")?;
    let existed = crate::cmd::db::exists(&ctx, &mut client, &source)?;
    if !existed {
        // A named source (flag or committed .pgboxdb.toml) that is missing is an
        // error: creating it empty would make every session branch from
        // nothing while the context line claims a copy of the real template.
        let named = opts.source.is_some() || project.source.is_some();
        if named && source_name != project::DEFAULT_SOURCE {
            return Err(PgboxdbError::new(
                ExitCode::NotFound,
                format!("source database {source} does not exist"),
            )
            .hint(format!(
                "import it with `pgboxdb copy --from \"$DATABASE_URL\" --to {source} --schema-only`, or create it with `pgboxdb create {source}`"
            )));
        }
        out.note(&format!("creating database {source}…"));
        if let Err(e) = client.simple_query(&format!("CREATE DATABASE {}", source.quoted())) {
            let err = crate::cmd::db_error(e, &format!("cannot create database {source}"));
            if err.code != ExitCode::Conflict {
                return Err(err);
            }
        }
    }
    let connections = crate::cmd::db::active_connections(&mut client, source.as_str())?;
    if connections > 0 {
        return Err(PgboxdbError::new(
            ExitCode::InUse,
            format!("source {source} has {connections} active connection(s); a branch source must be idle"),
        )
        .hint("disconnect them, or keep a dedicated template nobody connects to"));
    }
    let size: u64 = client
        .simple_query(&format!(
            "SELECT pg_database_size({})",
            crate::pgwire::quote_literal(source.as_str())
        ))
        .ok()
        .and_then(|r| r.scalar().and_then(|s| s.parse().ok()))
        .unwrap_or(0);
    client.close();

    let toml = ensure_toml(&project, Some(source.as_str()), opts.ttl.as_deref())?;
    let project = project::locate(cwd, &[])?; // re-read: the toml may be new

    let hooks = if opts.no_hooks {
        Vec::new()
    } else {
        let hosts = if opts.hosts.is_empty() {
            detect_hosts(&project.root)
        } else {
            parse_hosts(&opts.hosts, &project.root, "setup --hosts")?
        };
        if hosts.is_empty() {
            out.note("no agent host detected (no ~/.claude, ~/.cursor or ~/.codex); install hooks later with `pgboxdb agent install <host>`");
        }
        install_hosts(&hosts, opts.scope, &project.root)?
    };

    // Proof: one real branch, created and removed.
    let check_id = format!("check-{}", crate::fsutil::now_epoch());
    let r = session::resolve(&ResolveInput {
        id: Some(check_id),
        host: Some("setup".to_string()),
        source: Some(source.as_str().to_string()),
        cwd: Some(cwd.to_path_buf()),
        ..Default::default()
    })?;
    let res = session::up(&quiet, &r)?;
    session::down(&r, true)?;

    let st = if hooks.is_empty() {
        None
    } else {
        Some(selftest())
    };

    let mut text = String::from("PGBoxDB agent setup complete.\n");
    text.push_str(&format!(
        "  source     {} ({}, {})\n",
        source,
        human_size(size),
        if existed { "idle" } else { "created empty" }
    ));
    text.push_str(&format!(
        "  config     {} ({})\n",
        toml.path.display(),
        if toml.written { "written" } else { "kept" }
    ));
    if hooks.is_empty() {
        text.push_str("  hooks      none installed\n");
    }
    for (i, h) in hooks.iter().enumerate() {
        text.push_str(&format!(
            "  {:<10} {:<12} {}  ({} added, {} present)\n",
            if i == 0 { "hooks" } else { "" },
            match h.host {
                Host::Claude => "claude-code",
                Host::Cursor => "cursor",
                Host::Codex => "codex",
            },
            h.file.display(),
            h.added,
            h.present
        ));
    }
    text.push_str(&format!(
        "  runtime    {} on port {}\n",
        if started.already_running {
            "running"
        } else {
            "started"
        },
        ctx.config.port
    ));
    text.push_str(&format!(
        "  check      branch created and removed in {:.0} ms ({})\n",
        res.elapsed_ms, res.method
    ));
    if let Some(st) = &st {
        text.push_str(&format!("  {}", selftest_line(st)));
    }
    text.push_str("Next: start a new agent session; it gets its own branch automatically.\n");
    if toml.written {
        text.push_str(
            "      Commit .pgboxdb.toml so every checkout branches from the same source.\n",
        );
    }
    out.emit(
        text.trim_end(),
        json!({
            "runtime": {"initialized": initialized, "started": started.started, "already_running": started.already_running, "port": ctx.config.port, "pid": started.pid},
            "source": {"name": source.as_str(), "created": !existed, "connections": connections, "size_bytes": size},
            "toml": {"path": toml.path, "written": toml.written, "source": toml.source},
            "hooks": hooks.iter().map(|h| host_json(h, false)).collect::<Vec<_>>(),
            "check": {"branch": res.branch.as_str(), "elapsed_ms": (res.elapsed_ms * 10.0).round() / 10.0, "method": res.method},
            "selftest": st.as_ref().map(selftest_json),
        }),
    );
    Ok(ExitCode::Ok)
}

fn human_size(b: u64) -> String {
    const K: f64 = 1024.0;
    let b = b as f64;
    if b < K * K {
        format!("{:.0} KB", b / K)
    } else if b < K * K * K {
        format!("{:.1} MB", b / K / K)
    } else {
        format!("{:.2} GB", b / K / K / K)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_replaces_entries_written_before_the_rename() {
        let legacy = "/bin/sh -c 'PATH=\"$PATH:~/.local/bin\"; command -v pgx >/dev/null 2>&1 && exec pgx agent hook; exit 0'";
        let mut doc = json!({"hooks": {
            "SessionStart": [
                {"hooks": [{"type": "command", "command": legacy}]},
                {"hooks": [{"type": "command", "command": "other"}]}
            ],
            "Stop": [{"hooks": [{"type": "command", "command": legacy}, {"type": "command", "command": "keep-me"}]}],
            "Unrelated": [{"hooks": []}]
        }});
        assert!(has_legacy(&doc));
        assert_eq!(merge_install(&mut doc, Host::Claude), (6, 0));
        assert!(!has_legacy(&doc), "{doc}");
        let text = doc.to_string();
        assert!(!text.contains("exec pgx agent hook"), "{text}");
        assert_eq!(text.matches(MARKER).count(), 6, "{text}");
        // Foreign hooks stay, including one sharing a group with an old entry,
        // and an empty foreign group is not tidied away by install.
        assert!(
            text.contains("\"other\"") && text.contains("\"keep-me\""),
            "{text}"
        );
        assert!(doc["hooks"]["Unrelated"].is_array(), "{text}");
        // Idempotent afterwards.
        assert_eq!(merge_install(&mut doc, Host::Claude), (0, 6));
    }

    #[test]
    fn uninstall_removes_current_and_previous_entries_only() {
        let legacy = "exec pgx agent hook --host cursor";
        let mut doc = json!({"version": 1, "hooks": {"sessionStart": [
            {"command": legacy}, {"command": "pgboxdb agent hook --host cursor"}, {"command": "other"}
        ]}});
        assert_eq!(merge_uninstall(&mut doc), 2);
        assert_eq!(doc["hooks"]["sessionStart"], json!([{"command": "other"}]));
    }

    #[test]
    fn merge_is_idempotent_and_keeps_foreign_entries() {
        let mut doc = json!({"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "other"}]}]}});
        assert_eq!(merge_install(&mut doc, Host::Claude), (6, 0));
        assert_eq!(merge_install(&mut doc, Host::Claude), (0, 6));
        assert_eq!(doc["hooks"]["SessionStart"].as_array().unwrap().len(), 2);
        assert_eq!(
            doc["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "other"
        );
        assert!(doc["hooks"]["SubagentStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(MARKER));
        assert_eq!(doc["hooks"]["SubagentStop"][0]["hooks"][0]["timeout"], 10);
        assert_eq!(doc["hooks"]["PreToolUse"][0]["matcher"], "Bash");
        // An older installation (four events) is completed, not duplicated.
        let mut old = json!({"hooks": {}});
        merge_install(&mut old, Host::Claude);
        old["hooks"]
            .as_object_mut()
            .unwrap()
            .remove("SubagentStart");
        old["hooks"].as_object_mut().unwrap().remove("SubagentStop");
        assert_eq!(merge_install(&mut old, Host::Claude), (2, 4));
        assert_eq!(merge_uninstall(&mut doc), 6);
        assert_eq!(doc["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
        assert!(doc["hooks"].get("SessionEnd").is_none());
    }

    #[test]
    fn cursor_docs_carry_a_version() {
        let mut doc = json!({});
        merge_install(&mut doc, Host::Cursor);
        assert_eq!(doc["version"], 1);
        assert_eq!(doc["hooks"]["sessionStart"][0]["timeout"], 30);
        assert!(doc["hooks"].get("beforeShellExecution").is_none());
    }

    #[test]
    fn the_hook_command_is_guarded() {
        let c = hook_command(Host::Claude);
        assert!(c.contains("command -v pgboxdb"));
        assert!(c.ends_with("exit 0'"));
        assert!(c.contains(MARKER));
        assert!(!c.contains("--host"));
        assert!(hook_command(Host::Codex).contains("pgboxdb agent hook --host codex"));
        assert!(hook_command(Host::Cursor).contains("pgboxdb agent hook --host cursor"));
    }
}
