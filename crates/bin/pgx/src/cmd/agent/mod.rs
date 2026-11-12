//! `pgx agent …`: one isolated database per coding-agent session.
//!
//! The deterministic half of the agent integration. The Skill in
//! `skills/pgx/` decides *when* a task needs a database and how to prepare a
//! template; these commands do the work that must be exact: session identity,
//! idempotent branch creation, idle TTL and grace, hook plumbing for Claude
//! Code, Cursor and Codex, a process wrapper, installation and diagnosis.
use crate::cli::ExitCode;
use crate::errors::{PgxError, Result};
use crate::out::Out;
use std::path::PathBuf;

pub mod activity;
pub mod doctor;
pub mod exec;
pub mod hook;
pub mod hosts;
pub mod identity;
pub mod install;
pub mod project;
pub mod session;

/// Where hook configuration is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The project's shared file (`.claude/settings.json`, `.cursor/hooks.json`, `.codex/hooks.json`).
    Project,
    /// The per-developer project file (`.claude/settings.local.json`; the same as Project for hosts without one).
    Local,
    /// The user's home (`~/.claude/settings.json`, `~/.cursor/hooks.json`, `~/.codex/hooks.json`).
    User,
}

impl Scope {
    fn parse(s: &str) -> Result<Scope> {
        match s {
            "project" => Ok(Scope::Project),
            "local" => Ok(Scope::Local),
            "user" => Ok(Scope::User),
            other => Err(PgxError::usage(format!(
                "--scope must be project, local or user, got {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdOpts {
    pub id: Option<String>,
    pub host: Option<String>,
    /// A subagent of the session.
    pub agent: Option<String>,
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpOpts {
    pub id: Option<String>,
    pub host: Option<String>,
    pub agent: Option<String>,
    pub source: Option<String>,
    pub ttl: Option<String>,
    /// Print `export` lines instead of the human block.
    pub env: bool,
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DownOpts {
    pub id: Option<String>,
    pub host: Option<String>,
    pub agent: Option<String>,
    /// Drop immediately instead of retiring with a grace period.
    pub now: bool,
    pub grace: Option<String>,
    /// Act only if the session's branch is this one (a guard against a
    /// mistyped `--id` retiring somebody else's branch).
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecOpts {
    pub id: Option<String>,
    pub host: Option<String>,
    pub agent: Option<String>,
    pub source: Option<String>,
    pub ttl: Option<String>,
    /// Leave the branch active (idle TTL) instead of dropping it when the
    /// command exits; the next `exec` with the same id attaches to it.
    pub keep: bool,
    /// Resolve the project from here instead of the current directory.
    pub cwd: Option<PathBuf>,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookOpts {
    pub host: Option<String>,
    pub source: Option<String>,
    pub ttl: Option<String>,
    pub grace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOpts {
    /// Empty means "the hosts detected on this machine".
    pub hosts: Vec<String>,
    pub scope: Scope,
    pub source: Option<String>,
    pub ttl: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupOpts {
    pub source: Option<String>,
    pub hosts: Vec<String>,
    pub scope: Scope,
    pub ttl: Option<String>,
    pub no_hooks: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCmd {
    Up(UpOpts),
    Touch(IdOpts),
    Down(DownOpts),
    Exec(ExecOpts),
    Env(IdOpts),
    Ls,
    /// `pgx agent activity --state …`: an explicit task phase for `pgx top`.
    Activity(activity::ActivityOpts),
    Hook(HookOpts),
    Install(InstallOpts),
    Uninstall(InstallOpts),
    Setup(SetupOpts),
    Doctor,
}

const USAGE: &str =
    "usage: pgx agent <setup|doctor|exec|ls|up|touch|down|env|activity|hook|install|uninstall> [options]";

const VALUE_FLAGS: [&str; 12] = [
    "id", "host", "agent", "source", "ttl", "grace", "hosts", "scope", "cwd", "branch", "state",
    "task",
];
const BOOL_FLAGS: [&str; 4] = ["env", "now", "keep", "no-hooks"];

/// Parse everything after `pgx agent`. `--` ends option parsing (for `exec`).
pub fn parse(tail: &[&str]) -> Result<AgentCmd> {
    let Some((&sub, rest)) = tail.split_first() else {
        return Err(PgxError::usage(USAGE));
    };
    let mut flags: Vec<(String, Option<String>)> = Vec::new();
    let mut positional: Vec<String> = Vec::new();
    let mut command: Vec<String> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i];
        if t == "--" {
            command = rest[i + 1..].iter().map(|s| s.to_string()).collect();
            break;
        }
        if let Some(flag) = t.strip_prefix("--") {
            if let Some((k, v)) = flag.split_once('=') {
                flags.push((k.to_string(), Some(v.to_string())));
            } else if VALUE_FLAGS.contains(&flag) {
                let v = rest
                    .get(i + 1)
                    .copied()
                    .ok_or_else(|| PgxError::usage(format!("--{flag} needs a value")))?;
                flags.push((flag.to_string(), Some(v.to_string())));
                i += 1;
            } else if BOOL_FLAGS.contains(&flag) {
                flags.push((flag.to_string(), None));
            } else {
                return Err(PgxError::usage(format!(
                    "pgx agent {sub}: unknown option --{flag}"
                )));
            }
        } else {
            positional.push(t.to_string());
        }
        i += 1;
    }
    let only = |allowed: &[&str]| -> Result<()> {
        for (k, _) in &flags {
            if !allowed.contains(&k.as_str()) {
                return Err(PgxError::usage(format!(
                    "pgx agent {sub}: unknown option --{k}"
                )));
            }
        }
        Ok(())
    };
    let value = |k: &str| -> Option<String> {
        flags
            .iter()
            .find(|(f, _)| f == k)
            .and_then(|(_, v)| v.clone())
    };
    let has = |k: &str| flags.iter().any(|(f, _)| f == k);
    let no_positional = |usage: &str| -> Result<()> {
        if positional.is_empty() {
            Ok(())
        } else {
            Err(PgxError::usage(format!("usage: pgx agent {usage}")))
        }
    };
    let hosts_list = |v: Option<String>| -> Vec<String> {
        v.map(|s| {
            s.split(',')
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
                .collect()
        })
        .unwrap_or_default()
    };
    let scope = match value("scope") {
        Some(s) => Scope::parse(&s)?,
        None => Scope::Project,
    };
    let cwd_flag = value("cwd").map(PathBuf::from);
    let cmd = match sub {
        "up" => {
            only(&["id", "host", "agent", "source", "ttl", "env", "cwd"])?;
            no_positional(
                "up [--id <session>] [--host <h>] [--agent <id>] [--source <db>] [--ttl <dur>] [--env] [--cwd <dir>]",
            )?;
            AgentCmd::Up(UpOpts {
                id: value("id"),
                host: value("host"),
                agent: value("agent"),
                source: value("source"),
                ttl: value("ttl"),
                env: has("env"),
                cwd: cwd_flag,
            })
        }
        "touch" => {
            only(&["id", "host", "agent", "cwd"])?;
            no_positional("touch [--id <session>] [--host <h>] [--agent <id>]")?;
            AgentCmd::Touch(IdOpts {
                id: value("id"),
                host: value("host"),
                agent: value("agent"),
                cwd: cwd_flag,
            })
        }
        "env" => {
            only(&["id", "host", "agent", "cwd"])?;
            no_positional("env [--id <session>] [--host <h>] [--agent <id>] [--cwd <dir>]")?;
            AgentCmd::Env(IdOpts {
                id: value("id"),
                host: value("host"),
                agent: value("agent"),
                cwd: cwd_flag,
            })
        }
        "down" => {
            only(&["id", "host", "agent", "now", "grace", "branch", "cwd"])?;
            no_positional("down [--id <session>] [--host <h>] [--agent <id>] [--branch <name>] [--now | --grace <dur>]")?;
            if has("now") && has("grace") {
                return Err(PgxError::usage(
                    "pgx agent down: --now and --grace exclude each other",
                ));
            }
            AgentCmd::Down(DownOpts {
                id: value("id"),
                host: value("host"),
                agent: value("agent"),
                now: has("now"),
                grace: value("grace"),
                branch: value("branch"),
            })
        }
        "exec" => {
            only(&["id", "host", "agent", "source", "ttl", "keep", "cwd"])?;
            if !positional.is_empty() || command.is_empty() {
                return Err(PgxError::usage(
                    "usage: pgx agent exec [--id <session>] [--agent <id>] [--source <db>] [--ttl <dur>] [--keep] [--cwd <dir>] -- <command> [args…]",
                ));
            }
            AgentCmd::Exec(ExecOpts {
                id: value("id"),
                host: value("host"),
                agent: value("agent"),
                source: value("source"),
                ttl: value("ttl"),
                keep: has("keep"),
                cwd: cwd_flag,
                command,
            })
        }
        "ls" | "list" => {
            only(&[])?;
            no_positional("ls")?;
            AgentCmd::Ls
        }
        "activity" => {
            only(&["id", "host", "agent", "cwd", "state", "task"])?;
            if !positional.is_empty() {
                return Err(PgxError::usage(activity::USAGE));
            }
            let Some(state) = value("state") else {
                return Err(PgxError::usage(format!(
                    "pgx agent activity: --state is required\n{}",
                    activity::USAGE
                )));
            };
            activity::parse_state(&state)?;
            AgentCmd::Activity(activity::ActivityOpts {
                id: value("id"),
                host: value("host"),
                agent: value("agent"),
                cwd: cwd_flag,
                state,
                task: value("task"),
            })
        }
        "doctor" => {
            only(&[])?;
            no_positional("doctor")?;
            AgentCmd::Doctor
        }
        "hook" => {
            only(&["host", "source", "ttl", "grace"])?;
            no_positional(
                "hook [--host claude|cursor|codex] [--source <db>] [--ttl <dur>] [--grace <dur>]",
            )?;
            AgentCmd::Hook(HookOpts {
                host: value("host"),
                source: value("source"),
                ttl: value("ttl"),
                grace: value("grace"),
            })
        }
        "install" | "uninstall" => {
            only(&["scope", "source", "ttl", "hosts"])?;
            let mut hosts = positional.clone();
            hosts.extend(hosts_list(value("hosts")));
            let opts = InstallOpts {
                hosts,
                scope,
                source: value("source"),
                ttl: value("ttl"),
            };
            if sub == "install" {
                AgentCmd::Install(opts)
            } else {
                AgentCmd::Uninstall(opts)
            }
        }
        "setup" => {
            only(&["source", "hosts", "scope", "ttl", "no-hooks"])?;
            no_positional("setup [--source <db>] [--hosts claude,cursor,codex] [--scope project|local|user] [--ttl <dur>] [--no-hooks]")?;
            AgentCmd::Setup(SetupOpts {
                source: value("source"),
                hosts: hosts_list(value("hosts")),
                scope,
                ttl: value("ttl"),
                no_hooks: has("no-hooks"),
            })
        }
        other => {
            return Err(PgxError::usage(format!(
                "unknown agent subcommand {other:?}\n{USAGE}"
            )))
        }
    };
    Ok(cmd)
}

fn cwd() -> Result<PathBuf> {
    std::env::current_dir()
        .map_err(|e| PgxError::failure(format!("cannot read the current directory: {e}")))
}

pub fn run(out: &Out, cmd: AgentCmd) -> Result<ExitCode> {
    use session::ResolveInput;
    match cmd {
        AgentCmd::Up(o) => {
            let r = session::resolve(&ResolveInput {
                id: o.id,
                host: o.host,
                agent_id: o.agent,
                source: o.source,
                ttl: o.ttl,
                cwd: o.cwd,
                ..Default::default()
            })?;
            let res = session::up(out, &r)?;
            if o.env {
                print!("{}", session::env_lines(&res));
            } else {
                for w in &res.guard_warnings {
                    out.note(&format!("guard not applied on {}: {w}", res.branch));
                }
                out.emit(&session::up_text(&res), session::up_json(&res));
            }
            Ok(ExitCode::Ok)
        }
        AgentCmd::Touch(o) => {
            let r = session::resolve_by_id(&ResolveInput {
                id: o.id,
                host: o.host,
                agent_id: o.agent,
                cwd: o.cwd,
                ..Default::default()
            })?;
            let t = session::touch(&r)?;
            let now = crate::fsutil::now_epoch();
            out.emit(
                &match (&t.branch, t.skipped) {
                    (None, _) => "no branch for this session".to_string(),
                    (Some(b), true) => format!("{b}: recently touched, nothing to do"),
                    (Some(b), false) => format!(
                        "{b}: expires in {}",
                        crate::ttl::humanize_remaining(t.expires_at, now)
                    ),
                },
                serde_json::json!({
                    "branch": t.branch,
                    "skipped": t.skipped,
                    "expires_at": t.expires_at.map(crate::fsutil::rfc3339),
                    "expires_in_seconds": t.expires_at.map(|e| e.saturating_sub(now)),
                }),
            );
            Ok(ExitCode::Ok)
        }
        AgentCmd::Down(o) => {
            let r = session::resolve_by_id(&ResolveInput {
                id: o.id,
                host: o.host,
                agent_id: o.agent,
                grace: o.grace,
                ..Default::default()
            })?;
            if let Some(b) = &o.branch {
                session::verify_branch(&r, b)?;
            }
            let d = session::down(&r, o.now)?;
            let Some(branch) = d.branch.clone() else {
                return Err(PgxError::new(
                    ExitCode::NotFound,
                    format!("no branch for {}", r.key.describe()),
                )
                .hint("`pgx agent ls` lists agent branches"));
            };
            let now = crate::fsutil::now_epoch();
            let text = if d.dropped {
                format!("dropped {branch}")
            } else {
                format!(
                    "retired {branch}: removed in {} unless the session resumes",
                    crate::ttl::humanize_remaining(d.retired_until, now)
                )
            };
            out.emit(
                &text,
                serde_json::json!({
                    "branch": branch,
                    "session_id": r.key.session_id,
                    "agent_id": r.key.agent_id,
                    "dropped": d.dropped,
                    "retired_until": d.retired_until.map(crate::fsutil::rfc3339),
                    "retired_until_seconds": d.retired_until.map(|e| e.saturating_sub(now)),
                    "backends_terminated": d.backends_terminated,
                    "backends_remaining": d.backends_remaining,
                }),
            );
            Ok(ExitCode::Ok)
        }
        AgentCmd::Env(o) => {
            let r = session::resolve_by_id(&ResolveInput {
                id: o.id,
                host: o.host,
                agent_id: o.agent,
                cwd: o.cwd,
                ..Default::default()
            })?;
            print!("{}", session::env_for_existing(&r)?);
            Ok(ExitCode::Ok)
        }
        AgentCmd::Ls => session::ls(out),
        AgentCmd::Activity(o) => activity::run(out, o),
        AgentCmd::Exec(o) => exec::run(out, o),
        AgentCmd::Hook(o) => hook::run(out, o),
        AgentCmd::Install(o) => install::install(out, o, &cwd()?),
        AgentCmd::Uninstall(o) => install::uninstall(out, o, &cwd()?),
        AgentCmd::Setup(o) => install::setup(out, o, &cwd()?),
        AgentCmd::Doctor => doctor::run(out, &cwd()?),
    }
}
