//! The lifecycle primitives: `up` (ensure this session's branch), `touch`
//! (activity: extend the idle TTL), `down` (session end: retire with a grace
//! period, or drop), `env` (the exports for an existing session), `ls`.
use super::identity::{self, SessionKey};
use super::project::{self, ProjectConfig};
use crate::cli::ExitCode;
use crate::cmd::db::{self, database_oid, socket_url, tcp_url};
use crate::cmd::Ctx;
use crate::errors::{PgxError, Result};
use crate::layout::Home;
use crate::names::{self, DbName};
use crate::out::Out;
use crate::ttl::Ttl;
use crate::ttlstore::{AgentMeta, MetaStore, TtlRecord};
use serde_json::json;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

type Client = crate::pgwire::Client<std::os::unix::net::UnixStream>;

/// `touch` rewrites the record at most this often.
pub const TOUCH_INTERVAL_SECS: u64 = 30;

/// What a command needs to know about the session before touching PGX.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub key: SessionKey,
    pub project: ProjectConfig,
    pub source: DbName,
    /// Whether the source came from `--source`/`PGX_SOURCE`/`.pgx.toml`
    /// (as opposed to the default `app`).
    pub explicit_source: bool,
    pub idle_ttl: Ttl,
    /// The idle TTL as the user wrote it (`2h`, `90s`): `Ttl::label()` can
    /// produce composite forms the parser rejects, so the original is kept.
    pub idle_label: String,
    pub grace: Ttl,
    /// PGX Git: the session branches from its current Git branch's database.
    pub git: Option<crate::cmd::git::core::AgentGit>,
}

impl Resolved {
    /// A project that opted into isolation: a `.pgx.toml` exists or a source was named.
    pub fn isolation_enabled(&self) -> bool {
        self.project.config_file.is_some() || self.explicit_source
    }
    pub fn idle_secs(&self) -> u64 {
        self.idle_ttl.as_seconds().unwrap_or(7200)
    }
    pub fn grace_secs(&self) -> u64 {
        self.grace.as_seconds().unwrap_or(600)
    }
}

/// Inputs to `resolve`, from flags or from a host payload.
#[derive(Debug, Clone, Default)]
pub struct ResolveInput {
    pub id: Option<String>,
    pub host: Option<String>,
    /// A subagent of the session (Claude Code's `agent_id`); `None` for the
    /// session itself. Falls back to `PGX_AGENT_ID`, which routed commands carry.
    pub agent_id: Option<String>,
    pub source: Option<String>,
    pub ttl: Option<String>,
    pub grace: Option<String>,
    /// Where the session runs; the current directory when None.
    pub cwd: Option<PathBuf>,
    /// The host's project root(s), tried after `cwd`.
    pub fallbacks: Vec<PathBuf>,
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn finite_ttl(label: &str, what: &str) -> Result<Ttl> {
    match crate::ttl::parse_ttl(label)? {
        Ttl::Unlimited => Err(PgxError::usage(format!(
            "{what} must be a finite duration such as 2h: agent branches always expire (use `pgx branch … --ttl unlimited` for a permanent copy)"
        ))),
        t => Ok(t),
    }
}

pub fn resolve(input: &ResolveInput) -> Result<Resolved> {
    let host = match input.host.clone().or_else(|| env("PGX_AGENT_HOST")) {
        Some(h) => identity::validate_host(&h)?,
        None if env("CLAUDE_CODE_SESSION_ID").is_some() => "claude".to_string(),
        None if env("CODEX_SESSION_ID").is_some() => "codex".to_string(),
        None => "exec".to_string(),
    };
    let session_id = match input
        .id
        .clone()
        .or_else(|| identity::session_env_var(&host).and_then(env))
    {
        Some(id) => identity::validate_session_id(&id)?,
        None => {
            return Err(PgxError::usage(
                "no session id: pass --id <session>, or run inside a supported agent (Claude Code sets CLAUDE_CODE_SESSION_ID, Codex sets CODEX_SESSION_ID)",
            ))
        }
    };
    let agent_id = match input.agent_id.clone().or_else(|| env("PGX_AGENT_ID")) {
        Some(a) => Some(identity::validate_agent_id(&a)?),
        None => None,
    };
    let cwd = match &input.cwd {
        Some(c) => c.clone(),
        None => std::env::current_dir()
            .map_err(|e| PgxError::failure(format!("cannot read the current directory: {e}")))?,
    };
    let mut fallbacks = input.fallbacks.clone();
    for var in ["CLAUDE_PROJECT_DIR", "CURSOR_PROJECT_DIR"] {
        if let Some(d) = env(var) {
            fallbacks.push(PathBuf::from(d));
        }
    }
    let project = project::locate(&cwd, &fallbacks)?;
    // PGX Git (opted in with `[git]` + `[git.agents] enabled`): the source is
    // the database of the Git branch checked out where the session runs. An
    // explicit --source / PGX_SOURCE still wins.
    let git = if input.source.is_none() && env("PGX_SOURCE").is_none() {
        crate::cmd::git::core::agent_binding(&cwd)?
    } else {
        None
    };
    let (source_name, explicit_source) = match input
        .source
        .clone()
        .or_else(|| env("PGX_SOURCE"))
        .or_else(|| git.as_ref().map(|_| "pgx_git_branch".to_string()))
        .or_else(|| project.source.clone())
    {
        Some(s) => (s, true),
        None => (project::DEFAULT_SOURCE.to_string(), false),
    };
    let source = names::validate(&source_name)?;
    let idle_label = input
        .ttl
        .clone()
        .or_else(|| project.idle_ttl.clone())
        .unwrap_or_else(|| project::DEFAULT_IDLE_TTL.to_string());
    let grace_label = input
        .grace
        .clone()
        .or_else(|| project.grace_period.clone())
        .unwrap_or_else(|| project::DEFAULT_GRACE.to_string());
    let idle_ttl = finite_ttl(&idle_label, "--ttl")?;
    let grace = finite_ttl(&grace_label, "--grace")?;
    Ok(Resolved {
        key: SessionKey {
            host,
            session_id,
            project_id: project.project_id.clone(),
            agent_id,
            scope: git.as_ref().map(|g| g.scope.clone()),
        },
        project,
        source,
        explicit_source,
        idle_ttl,
        idle_label,
        grace,
        git,
    })
}

/// `resolve` for a command that names a session (`--id`) but not its host:
/// find the host from the records of this project. One match decides;
/// several (the same id under two hosts) need `--host`; none falls back to
/// the ordinary resolution (the surrounding host, or `exec`).
pub fn resolve_by_id(input: &ResolveInput) -> Result<Resolved> {
    if input.host.is_some() || input.id.is_none() {
        return resolve(input);
    }
    let home = Home::resolve();
    if !home.is_initialized() {
        return resolve(input);
    }
    // The project is what `resolve` would use; the id and agent are validated there.
    let probe = resolve(input)?;
    let store = MetaStore::new(&home.root);
    let (records, _) = store.list();
    let mut hosts: Vec<String> = records
        .iter()
        .filter_map(|r| r.agent.as_ref())
        .filter(|m| {
            m.project_id == probe.key.project_id
                && m.session_id == probe.key.session_id
                && m.agent_id == probe.key.agent_id
        })
        .map(|m| m.host.clone())
        .collect();
    hosts.sort();
    hosts.dedup();
    match hosts.as_slice() {
        [] => Ok(probe),
        [one] => resolve(&ResolveInput {
            host: Some(one.clone()),
            ..input.clone()
        }),
        many => Err(PgxError::usage(format!(
            "session {} has branches under several hosts ({}); pass --host",
            probe.key.session_id,
            many.join(", ")
        ))),
    }
}

/// Refuse to act when the branch a user named is not the one this session
/// owns: a typo in `--id` can then never retire or drop somebody else's.
pub fn verify_branch(r: &Resolved, expected: &str) -> Result<()> {
    let home = Home::resolve();
    let store = MetaStore::new(&home.root);
    match find_record(&store, &r.key)? {
        Some(rec) if rec.name == expected => Ok(()),
        Some(rec) => Err(PgxError::new(
            ExitCode::Conflict,
            format!(
                "{} owns branch {}, not {expected}; refusing to touch {expected}",
                r.key.describe(),
                rec.name
            ),
        )
        .hint("`pgx agent ls` shows which session owns which branch")),
        None => Err(PgxError::new(
            ExitCode::NotFound,
            format!(
                "{} has no branch (so not {expected} either)",
                r.key.describe()
            ),
        )),
    }
}

/// Serialise every lifecycle operation on one session. Two hook deliveries for
/// the same session (Cursor's native file plus its Claude-compat loader, or a
/// racing resume) take turns instead of cloning twice.
pub fn lock_session(home: &Home, key: &SessionKey) -> Result<std::fs::File> {
    let dir = home.root.join("agent").join("locks");
    std::fs::create_dir_all(&dir).map_err(|e| PgxError::io("cannot create", &dir, &e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let path = dir.join(format!(
        "{}.lock",
        identity::hex16(identity::fnv1a64(key.material().as_bytes()))
    ));
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| PgxError::io("cannot open", &path, &e))?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(PgxError::failure(format!(
            "cannot lock {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(f)
}

fn meta_matches(m: &AgentMeta, key: &SessionKey) -> bool {
    meta_matches_any_scope(m, key) && m.git_scope == key.scope
}

/// The same session (or subagent) under any PGX Git scope.
fn meta_matches_any_scope(m: &AgentMeta, key: &SessionKey) -> bool {
    m.host == key.host
        && m.session_id == key.session_id
        && m.project_id == key.project_id
        && m.agent_id == key.agent_id
}

/// The record owned by this session, if any.
pub fn find_record(store: &MetaStore, key: &SessionKey) -> Result<Option<TtlRecord>> {
    let (records, _bad) = store.list();
    Ok(records
        .into_iter()
        .find(|r| r.agent.as_ref().is_some_and(|m| meta_matches(m, key))))
}

/// `Ctx::load` with the agent-friendly message when PGX was never initialised.
pub fn load_ctx() -> Result<Ctx> {
    let home = Home::resolve();
    if !home.is_initialized() {
        return Err(PgxError::new(
            ExitCode::NotReady,
            format!("PGX is not initialised at {}", home.root.display()),
        )
        .hint("run `pgx run` once (or `pgx agent setup`), then start the agent session again"));
    }
    Ctx::load()
}

/// What `up` produced.
#[derive(Debug, Clone)]
pub struct UpResult {
    pub branch: DbName,
    pub url: String,
    pub socket_url: String,
    pub created: bool,
    /// The previous branch for this session had vanished; this is a new one.
    pub fresh: bool,
    /// The session dropped and recreated its branch under the same name (a
    /// framework purge); the new incarnation was adopted.
    pub adopted: bool,
    pub source: String,
    pub source_created: bool,
    pub expires_at: u64,
    pub now: u64,
    pub idle_ttl: Ttl,
    pub key: SessionKey,
    pub project_root: PathBuf,
    pub method: &'static str,
    pub elapsed_ms: f64,
    pub port: u16,
    pub password: String,
    /// Guards that could not be set on a new branch (never fatal: the branch
    /// works, the kill-safety is weaker; `doctor` and the JSON say so).
    pub guard_warnings: Vec<String>,
}

fn new_meta(r: &Resolved, now: u64) -> AgentMeta {
    AgentMeta {
        host: r.key.host.clone(),
        session_id: r.key.session_id.clone(),
        project_id: r.key.project_id.clone(),
        project_root: r.project.root.to_string_lossy().into_owned(),
        source: r.source.as_str().to_string(),
        last_touch: now,
        retired: false,
        agent_id: r.key.agent_id.clone(),
        git_scope: r.key.scope.clone(),
        git_branch: r.git.as_ref().map(|g| g.label.clone()),
    }
}

/// The server-side guards every agent branch carries (`ALTER DATABASE … SET`,
/// so every new connection to the branch inherits them):
///
/// - `client_connection_check_interval`: a backend whose client died (a
///   killed agent) notices within this interval even inside a long statement
///   and aborts its transaction, instead of finishing the statement string and
///   committing minutes later.
/// - `idle_in_transaction_session_timeout`: an open transaction nobody drives
///   is aborted.
/// - `statement_timeout`: no single statement runs forever.
///
/// Returns the settings that could not be applied (one message each).
pub fn apply_guards(client: &mut Client, branch: &DbName, guards: &project::Guards) -> Vec<String> {
    let specs = [
        (
            "client_connection_check_interval",
            project::guard_seconds(
                guards.connection_check_interval.as_deref(),
                project::DEFAULT_CONNECTION_CHECK,
            ),
        ),
        (
            "idle_in_transaction_session_timeout",
            project::guard_seconds(
                guards.idle_in_transaction_timeout.as_deref(),
                project::DEFAULT_IDLE_IN_TRANSACTION,
            ),
        ),
        (
            "statement_timeout",
            project::guard_seconds(
                guards.statement_timeout.as_deref(),
                project::DEFAULT_STATEMENT_TIMEOUT,
            ),
        ),
    ];
    let mut warnings = Vec::new();
    for (name, secs) in specs {
        let value = match secs {
            Some(s) => format!("'{s}s'"),
            None => "0".to_string(),
        };
        let sql = format!("ALTER DATABASE {} SET {name} = {value}", branch.quoted());
        if let Err(e) = client.simple_query(&sql) {
            warnings.push(format!("{name}: {e}"));
        }
    }
    warnings
}

/// Ensure this session's branch exists and is current. Idempotent.
pub fn up(out: &Out, r: &Resolved) -> Result<UpResult> {
    let ctx = load_ctx()?;
    let _lock = lock_session(&ctx.home, &r.key)?;
    let quiet = out.quiet();
    crate::cmd::start::ensure_running(&quiet, &ctx)?;
    crate::cmd::start::spawn_supervisor(&ctx.home);
    let store = MetaStore::new(&ctx.home.root);
    let mut client = ctx.connect("postgres")?;
    let now = crate::fsutil::now_epoch();
    let idle = r.idle_secs();

    // An existing branch for this session: extend it and hand it back.
    let mut fresh = false;
    if let Some(mut rec) = find_record(&store, &r.key)? {
        match database_oid(&mut client, &rec.name)? {
            Some(oid) => {
                // The same OID is simply our branch. A different OID under our
                // derived name means the session dropped and recreated it (a
                // framework purge such as Rails db:reset): adopt the new
                // incarnation rather than orphaning it without a TTL.
                let adopted = oid != rec.oid;
                rec.oid = oid;
                rec.expires_at = Some(now + idle);
                rec.ttl_label = r.idle_label.clone();
                rec.ttl_seconds = Some(idle);
                rec.state = crate::ttlstore::State::Active;
                if let Some(m) = rec.agent.as_mut() {
                    m.last_touch = now;
                    m.retired = false;
                }
                store.save(&rec)?;
                let branch = names::validate(&rec.name)?;
                // A recreated incarnation lost the guards with the old database.
                let guard_warnings = if adopted {
                    apply_guards(&mut client, &branch, &r.project.guards)
                } else {
                    Vec::new()
                };
                client.close();
                return Ok(UpResult {
                    url: tcp_url(&ctx, branch.as_str()),
                    socket_url: socket_url(&ctx, branch.as_str()),
                    branch,
                    created: false,
                    fresh: false,
                    adopted,
                    source: rec
                        .agent
                        .as_ref()
                        .map(|m| m.source.clone())
                        .unwrap_or_default(),
                    source_created: false,
                    expires_at: now + idle,
                    now,
                    idle_ttl: r.idle_ttl,
                    key: r.key.clone(),
                    project_root: r.project.root.clone(),
                    method: "existing",
                    elapsed_ms: 0.0,
                    port: ctx.config.port,
                    password: ctx.config.password.clone(),
                    guard_warnings,
                });
            }
            None => {
                // Dropped by hand or expired: forget it and make a fresh one.
                store.remove(&rec.name)?;
                fresh = true;
            }
        }
    }

    // PGX Git: the current Git branch's database (created from the main
    // snapshot on first use, exactly as a checkout would).
    let mut source = r.source.clone();
    let mut git_snapshot: Option<DbName> = None;
    let mut git_main = false;
    if let Some(g) = &r.git {
        let s = crate::cmd::git::core::agent_source(&quiet, g)?;
        source = s.database;
        git_snapshot = s.snapshot;
        git_main = s.main;
    }
    let r = &Resolved {
        source,
        ..r.clone()
    };

    // The source must exist and be idle.
    let mut source_created = false;
    if !db::exists(&ctx, &mut client, &r.source)? {
        if r.explicit_source {
            return Err(PgxError::new(
                ExitCode::NotFound,
                format!("source database {} does not exist", r.source),
            )
            .hint(format!(
                "import it with `pgx copy --from \"$DATABASE_URL\" --to {} --schema-only`, or create it with `pgx create {}`",
                r.source, r.source
            )));
        }
        // The default `app`, exactly as `pgx run` would create it.
        if let Err(e) = client.simple_query(&format!("CREATE DATABASE {}", r.source.quoted())) {
            let err = crate::cmd::db_error(e, &format!("cannot create database {}", r.source));
            if err.code != ExitCode::Conflict {
                return Err(err);
            }
        }
        source_created = true;
    }
    let mut busy = db::active_connections(&mut client, r.source.as_str())?;
    let mut live_copy = false;
    let mut r_owned = r.clone();
    if busy > 0 && r.git.is_some() {
        // A developer's server is connected to the branch database. Never
        // terminate it: on main, branch from the validated main snapshot
        // (immutable, always idle); on a feature branch, take a consistent
        // logical copy instead of a file clone.
        match (&git_snapshot, git_main) {
            (Some(snap), true) => r_owned.source = snap.clone(),
            _ => live_copy = true,
        }
        busy = 0;
    }
    let r = &r_owned;
    if busy > 0 {
        return Err(PgxError::new(
            ExitCode::InUse,
            format!(
                "source {} has {busy} active connection{}; a branch source must be idle",
                r.source,
                if busy == 1 { "" } else { "s" }
            ),
        )
        .hint(format!(
            "keep it as a template nobody connects to (pgx copy --to {}_template) or disconnect first",
            r.source
        )));
    }

    // Pick the name: readable unless a foreign database already has it.
    let short = names::validate(&identity::branch_name(&r.key, false))?;
    let branch = if db::exists(&ctx, &mut client, &short)? {
        let long = names::validate(&identity::branch_name(&r.key, true))?;
        if db::exists(&ctx, &mut client, &long)? {
            return Err(PgxError::new(
                ExitCode::Conflict,
                format!("databases {short} and {long} both exist and belong to no agent session"),
            )
            .hint("drop or rename them, or pick another --id"));
        }
        long
    } else {
        short
    };

    let make = |client: &mut Client, target: &DbName| -> Result<(&'static str, f64)> {
        if live_copy {
            let (m, ms) =
                crate::cmd::git::core::copy_database(&quiet, &ctx, client, &r.source, target)?;
            db::record_ttl(&ctx, client, target, Some(&r.idle_label))?;
            Ok((m, ms))
        } else {
            let c = db::clone_database(&ctx, client, &r.source, target, Some(&r.idle_label))?;
            Ok((c.method, c.elapsed_ms))
        }
    };
    let (branch, cloned) = match make(&mut client, &branch) {
        Ok(c) => (branch, c),
        Err(e) if e.code == ExitCode::Conflict => {
            // Another session took this name between the check and the
            // clone (42P04): fall back to the long, project-qualified name.
            let long = names::validate(&identity::branch_name(&r.key, true))?;
            if long == branch {
                return Err(e);
            }
            let c = make(&mut client, &long)?;
            (long, c)
        }
        Err(e) => return Err(e),
    };
    // Attach the session to the record `clone_database` wrote.
    let mut rec = store
        .load(branch.as_str())?
        .ok_or_else(|| PgxError::failure(format!("no TTL record for {branch} after cloning")))?;
    rec.agent = Some(new_meta(r, now));
    rec.expires_at = Some(now + idle);
    store.save(&rec)?;
    let guard_warnings = apply_guards(&mut client, &branch, &r.project.guards);
    client.close();
    Ok(UpResult {
        url: tcp_url(&ctx, branch.as_str()),
        socket_url: socket_url(&ctx, branch.as_str()),
        branch,
        created: true,
        fresh,
        adopted: false,
        source: r.source.as_str().to_string(),
        source_created,
        expires_at: now + idle,
        now,
        idle_ttl: r.idle_ttl,
        key: r.key.clone(),
        project_root: r.project.root.clone(),
        method: cloned.0,
        elapsed_ms: cloned.1,
        port: ctx.config.port,
        password: ctx.config.password.clone(),
        guard_warnings,
    })
}

/// Outcome of `touch`.
#[derive(Debug, Clone)]
pub struct Touched {
    pub branch: Option<String>,
    pub skipped: bool,
    pub expires_at: Option<u64>,
}

/// Activity: move the deadline to now + idle TTL, at most once per
/// `TOUCH_INTERVAL_SECS`. No database connection; the supervisor reads the file.
pub fn touch(r: &Resolved) -> Result<Touched> {
    let home = Home::resolve();
    if !home.is_initialized() {
        return Ok(Touched {
            branch: None,
            skipped: true,
            expires_at: None,
        });
    }
    let _lock = lock_session(&home, &r.key)?;
    let store = MetaStore::new(&home.root);
    let Some(mut rec) = find_record(&store, &r.key)? else {
        return Ok(Touched {
            branch: None,
            skipped: true,
            expires_at: None,
        });
    };
    let now = crate::fsutil::now_epoch();
    let last = rec.agent.as_ref().map(|m| m.last_touch).unwrap_or(0);
    let idle = rec.ttl_seconds.unwrap_or(r.idle_secs());
    // Rate-limit writes, but never so much that a short TTL cannot be kept alive.
    let min_gap = TOUCH_INTERVAL_SECS.min(idle / 4).max(1);
    if now.saturating_sub(last) < min_gap {
        return Ok(Touched {
            branch: Some(rec.name),
            skipped: true,
            expires_at: rec.expires_at,
        });
    }
    let idle = rec.ttl_seconds.unwrap_or(r.idle_secs());
    rec.expires_at = Some(now + idle);
    rec.state = crate::ttlstore::State::Active;
    if let Some(m) = rec.agent.as_mut() {
        m.last_touch = now;
        m.retired = false;
    }
    store.save(&rec)?;
    Ok(Touched {
        branch: Some(rec.name),
        skipped: false,
        expires_at: rec.expires_at,
    })
}

/// Outcome of `down`.
#[derive(Debug, Clone)]
pub struct DownResult {
    pub branch: Option<String>,
    pub dropped: bool,
    pub retired_until: Option<u64>,
    /// Backends still connected to the branch that `down` terminated: the
    /// cancellation barrier. After `down` returns, nothing that was running
    /// on the branch can commit (see [`terminate_backends`]).
    pub backends_terminated: u64,
    /// Backends that were still alive when the barrier gave up waiting
    /// (runtime unreachable, or a backend that ignored SIGTERM for 3 s).
    pub backends_remaining: u64,
}

/// Session end: retire the branch (grace period) or drop it now. Only a branch
/// whose record names this exact session is ever touched. Either way every
/// backend connected to the branch is terminated first: the session is over,
/// so a statement it left running must not commit later. A resumed session
/// reconnects on its own.
pub fn down(r: &Resolved, now_flag: bool) -> Result<DownResult> {
    let res = down_one(r, now_flag)?;
    // PGX Git: a session that switched Git branches owns one branch per Git
    // branch; ending the session ends all of them.
    {
        let home = Home::resolve();
        if home.is_initialized() {
            let store = MetaStore::new(&home.root);
            let (records, _) = store.list();
            for rec in records {
                let Some(m) = rec.agent.as_ref() else {
                    continue;
                };
                if meta_matches_any_scope(m, &r.key) && m.git_scope != r.key.scope {
                    let mut other = r.clone();
                    other.key.scope = m.git_scope.clone();
                    if let Err(e) = down_one(&other, now_flag) {
                        eprintln!("pgx agent: could not end {}: {}", rec.name, e.message);
                    }
                }
            }
        }
    }
    Ok(res)
}

fn down_one(r: &Resolved, now_flag: bool) -> Result<DownResult> {
    let home = Home::resolve();
    let none = DownResult {
        branch: None,
        dropped: false,
        retired_until: None,
        backends_terminated: 0,
        backends_remaining: 0,
    };
    if !home.is_initialized() {
        return Ok(none);
    }
    let _lock = lock_session(&home, &r.key)?;
    let store = MetaStore::new(&home.root);
    let Some(mut rec) = find_record(&store, &r.key)? else {
        return Ok(none);
    };
    let mut terminated = 0;
    let mut remaining = 0;
    let mut client = None;
    if let Ok(ctx) = Ctx::load() {
        if ctx.running_pid().is_some() {
            if let Ok(c) = ctx.connect("postgres") {
                client = Some((ctx, c));
            }
        }
    }
    if let Some((_, c)) = client.as_mut() {
        // Adopt a recreated incarnation (same derived name, new OID) so the
        // grace period and the drop apply to what the session actually left.
        if let Ok(Some(oid)) = database_oid(c, &rec.name) {
            let live = rec.agent.as_ref().is_some_and(|m| !m.retired);
            if oid != rec.oid && live {
                rec.oid = oid;
                store.save(&rec)?;
            }
        }
        let (t, rem) = terminate_backends(c, &rec);
        terminated = t;
        remaining = rem;
    }
    let now = crate::fsutil::now_epoch();
    if !now_flag {
        rec.expires_at = Some(now + r.grace_secs());
        if let Some(m) = rec.agent.as_mut() {
            m.retired = true;
            m.last_touch = now;
        }
        store.save(&rec)?;
        if let Some((_, c)) = client.take() {
            c.close();
        }
        return Ok(DownResult {
            branch: Some(rec.name),
            dropped: false,
            retired_until: Some(now + r.grace_secs()),
            backends_terminated: terminated,
            backends_remaining: remaining,
        });
    }
    let (_, mut c) = match client {
        Some(c) => c,
        None => {
            let ctx = Ctx::load()?;
            let c = ctx.connect("postgres")?;
            (ctx, c)
        }
    };
    drop_branch(&mut c, &rec)?;
    c.close();
    store.remove(&rec.name)?;
    Ok(DownResult {
        branch: Some(rec.name),
        dropped: true,
        retired_until: None,
        backends_terminated: terminated,
        backends_remaining: 0,
    })
}

/// The cancellation barrier: terminate every backend connected to this
/// branch (OID-gated, so a reused name is never touched) and wait until none
/// is left, up to 3 s. Returns `(terminated, still alive)`. PostgreSQL's
/// `pg_terminate_backend` sends SIGTERM; a backend past the point of no
/// return of a COMMIT (WAL flushed) still completes it, which is why the
/// guarantee is "nothing that was running when `down` returned commits
/// afterwards", not "nothing that was running when the client died".
pub fn terminate_backends(client: &mut Client, rec: &TtlRecord) -> (u64, u64) {
    match database_oid(client, &rec.name) {
        Ok(Some(oid)) if oid == rec.oid => {}
        _ => return (0, 0),
    }
    let lit = crate::pgwire::quote_literal(&rec.name);
    let count = |client: &mut Client| -> u64 {
        client
            .simple_query(&format!(
                "SELECT count(*) FROM pg_stat_activity WHERE datname = {lit} AND pid <> pg_backend_pid()"
            ))
            .ok()
            .and_then(|r| r.scalar().and_then(|s| s.parse().ok()))
            .unwrap_or(0)
    };
    let before = count(client);
    if before == 0 {
        return (0, 0);
    }
    let _ = client.simple_query(&format!(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = {lit} AND pid <> pg_backend_pid()"
    ));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut left = count(client);
    while left > 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
        left = count(client);
    }
    (before, left)
}

/// Terminate sessions on this branch only, then drop it. The OID gate keeps a
/// reused name (a different incarnation) safe.
fn drop_branch(client: &mut Client, rec: &TtlRecord) -> Result<()> {
    match database_oid(client, &rec.name)? {
        None => return Ok(()),
        Some(oid) if oid != rec.oid => return Ok(()),
        Some(_) => {}
    }
    let name = names::validate(&rec.name)?;
    let _ = terminate_backends(client, rec);
    client
        .simple_query(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            name.quoted()
        ))
        .map_err(|e| crate::cmd::db_error(e, &format!("cannot drop {}", rec.name)))?;
    Ok(())
}

fn sq(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

/// The nine variables a session receives (a subagent gets `PGX_AGENT_ID` too).
pub fn env_pairs(res: &UpResult) -> Vec<(&'static str, String)> {
    let mut v = vec![
        ("DATABASE_URL", res.url.clone()),
        ("PGX_BRANCH", res.branch.as_str().to_string()),
        ("PGX_SOURCE", res.source.clone()),
        ("PGHOST", "127.0.0.1".to_string()),
        ("PGPORT", res.port.to_string()),
        ("PGUSER", "postgres".to_string()),
        ("PGPASSWORD", res.password.clone()),
        ("PGDATABASE", res.branch.as_str().to_string()),
        ("PGX_ISOLATION", "ok".to_string()),
    ];
    if let Some(a) = &res.key.agent_id {
        v.push(("PGX_AGENT_ID", a.clone()));
    }
    v
}

/// `export K='v'` lines for `env_pairs`.
pub fn env_lines(res: &UpResult) -> String {
    env_pairs(res)
        .iter()
        .map(|(k, v)| format!("export {k}={}\n", sq(v)))
        .collect()
}

/// Fail closed: a URL that cannot connect, so a session whose isolation failed
/// never falls back to an inherited database.
pub fn sentinel_pairs(reason: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "DATABASE_URL",
            "postgres://pgx-isolation-failed@127.0.0.1:1/pgx_isolation_failed".to_string(),
        ),
        ("PGX_ISOLATION", "failed".to_string()),
        ("PGX_ISOLATION_REASON", reason.to_string()),
        ("PGHOST", "127.0.0.1".to_string()),
        ("PGPORT", "1".to_string()),
        ("PGUSER", "pgx-isolation-failed".to_string()),
        ("PGDATABASE", "pgx_isolation_failed".to_string()),
        ("PGPASSWORD", String::new()),
        ("PGX_BRANCH", String::new()),
        ("PGX_SOURCE", String::new()),
    ]
}

pub fn sentinel_env_lines(reason: &str) -> String {
    sentinel_pairs(reason)
        .iter()
        .map(|(k, v)| format!("export {k}={}\n", sq(v)))
        .collect()
}

pub fn up_json(res: &UpResult) -> serde_json::Value {
    let mut v = json!({
        "branch": res.branch.as_str(),
        "source": res.source,
        "source_created": res.source_created,
        "created": res.created,
        "fresh": res.fresh,
        "adopted": res.adopted,
        "url": res.url,
        "socket_url": res.socket_url,
        "host": res.key.host,
        "session_id": res.key.session_id,
        "agent_id": res.key.agent_id,
        "project_id": res.key.project_id,
        "project_root": res.project_root.to_string_lossy(),
        "idle_ttl": res.idle_ttl.label(),
        "idle_ttl_seconds": res.idle_ttl.as_seconds(),
        "expires_at": crate::fsutil::rfc3339(res.expires_at),
        "expires_in_seconds": res.expires_at.saturating_sub(res.now),
        "method": res.method,
        "elapsed_ms": (res.elapsed_ms * 10.0).round() / 10.0,
        "guard_warnings": res.guard_warnings,
    });
    v["port"] = json!(res.port);
    v
}

pub fn up_text(res: &UpResult) -> String {
    let verb = if res.created { "created" } else { "ready" };
    let mut src = res.source.clone();
    if res.source_created {
        src.push_str(" (created empty: no .pgx.toml names a source)");
    } else if res.fresh {
        src.push_str(" (fresh: the previous branch was gone)");
    } else if res.adopted {
        src.push_str(" (adopted: the session recreated this database)");
    }
    format!(
        "✓ Branch {verb}: {}\n✓ Source: {src}\n✓ Expires in: {} after the last activity\n\nDATABASE_URL:\n{}",
        res.branch,
        crate::ttl::humanize_long(res.idle_ttl.as_seconds().unwrap_or(0)),
        res.url
    )
}

/// `pgx agent ls`.
pub fn ls(out: &Out) -> Result<ExitCode> {
    let ctx = load_ctx()?;
    let store = MetaStore::new(&ctx.home.root);
    let (records, _bad) = store.list();
    let now = crate::fsutil::now_epoch();
    let mut sizes = std::collections::HashMap::new();
    if ctx.running_pid().is_some() {
        if let Ok(mut client) = ctx.connect("postgres") {
            if let Ok(r) =
                client.simple_query("SELECT datname, pg_database_size(oid) FROM pg_database")
            {
                for row in r.rows {
                    if let (Some(n), Some(s)) = (
                        row[0].clone(),
                        row[1].as_deref().and_then(|s| s.parse::<u64>().ok()),
                    ) {
                        sizes.insert(n, s);
                    }
                }
            }
            client.close();
        }
    }
    let mut text = format!(
        "{:<34} {:<8} {:<14} {:<10} {:<16} {:<8} {:<12} {:>9}\n",
        "NAME", "HOST", "SESSION", "AGENT", "SOURCE", "STATE", "EXPIRES IN", "SIZE"
    );
    let mut items = Vec::new();
    for rec in records.iter().filter(|r| r.agent.is_some()) {
        let m = rec.agent.as_ref().unwrap();
        // `active`: an owner is alive, the deadline is the idle TTL after its
        // last activity. `retired`: the owner is gone, the deadline is the
        // grace period. `expired`: past its deadline, awaiting the supervisor.
        let state = if rec.is_expired(now) {
            "expired"
        } else if m.retired {
            "retired"
        } else {
            "active"
        };
        let deadline_kind = if m.retired { "grace" } else { "idle_ttl" };
        let sid: String = m.session_id.chars().take(12).collect();
        let aid: String = m
            .agent_id
            .as_deref()
            .map(|a| {
                a.strip_prefix("agent-")
                    .unwrap_or(a)
                    .chars()
                    .take(10)
                    .collect()
            })
            .unwrap_or_else(|| "-".to_string());
        let size = sizes.get(&rec.name).copied();
        text.push_str(&format!(
            "{:<34} {:<8} {:<14} {:<10} {:<16} {:<8} {:<12} {:>9}\n",
            rec.name,
            m.host,
            sid,
            aid,
            m.source,
            state,
            crate::ttl::humanize_remaining(rec.expires_at, now),
            size.map(human_size).unwrap_or_else(|| "-".into()),
        ));
        items.push(json!({
            "name": rec.name,
            "state": state,
            "deadline_kind": deadline_kind,
            "ttl": rec.ttl_label,
            "ttl_seconds": rec.ttl_seconds,
            "expires_at": rec.expires_at.map(crate::fsutil::rfc3339),
            "expires_in_seconds": rec.expires_at.map(|e| e.saturating_sub(now)),
            "created_at": crate::fsutil::rfc3339(rec.created_at),
            "age_seconds": now.saturating_sub(rec.created_at),
            "size_bytes": size,
            "agent": m.to_json(),
        }));
    }
    if items.is_empty() {
        out.emit("no agent branches", json!(items));
    } else {
        out.emit(text.trim_end(), json!(items));
    }
    Ok(ExitCode::Ok)
}

fn human_size(b: u64) -> String {
    const K: f64 = 1024.0;
    let b = b as f64;
    if b < K {
        format!("{b:.0} B")
    } else if b < K * K {
        format!("{:.0} KB", b / K)
    } else if b < K * K * K {
        format!("{:.1} MB", b / K / K)
    } else {
        format!("{:.2} GB", b / K / K / K)
    }
}

/// `pgx agent env`: the exports for an existing session, creating nothing.
pub fn env_for_existing(r: &Resolved) -> Result<String> {
    Ok(env_lines(&existing(r)?))
}

/// The `UpResult` an existing branch would answer with, without touching it.
pub fn existing(r: &Resolved) -> Result<UpResult> {
    let ctx = load_ctx()?;
    let store = MetaStore::new(&ctx.home.root);
    let Some(rec) = find_record(&store, &r.key)? else {
        return Err(PgxError::new(
            ExitCode::NotFound,
            format!("no branch for {}", r.key.describe()),
        )
        .hint("`pgx agent up` creates one"));
    };
    let branch = names::validate(&rec.name)?;
    let res = UpResult {
        url: tcp_url(&ctx, branch.as_str()),
        socket_url: socket_url(&ctx, branch.as_str()),
        branch,
        created: false,
        fresh: false,
        adopted: false,
        source: rec
            .agent
            .as_ref()
            .map(|m| m.source.clone())
            .unwrap_or_default(),
        source_created: false,
        expires_at: rec.expires_at.unwrap_or(0),
        now: crate::fsutil::now_epoch(),
        idle_ttl: Ttl::Seconds(rec.ttl_seconds.unwrap_or(0)),
        key: r.key.clone(),
        project_root: r.project.root.clone(),
        method: "existing",
        elapsed_ms: 0.0,
        port: ctx.config.port,
        password: ctx.config.password.clone(),
        guard_warnings: Vec::new(),
    };
    Ok(res)
}

#[allow(dead_code)]
fn _assert_path(_: &Path) {}
