//! The PGX Git engine: the main snapshot manager (`sync`), branch database
//! resolution (`resolve_head`), crash recovery, and the main database.
//!
//! ```text
//! main Git branch ─► main development database ─► fingerprint ─► staging copy
//!   ─► validate ─► rename + ALLOW_CONNECTIONS false ─► published snapshot vN
//!                                                          │
//!                         new Git branches clone from ◄────┘
//! ```
//!
//! Every step that changes the cluster runs under the repository lock and
//! records its intent first, so concurrent hooks, agents and worktrees take
//! turns and an interrupted step is rolled back on the next run.
use super::config::GitConfig;
use super::fingerprint::{self, Fingerprint};
use super::naming;
use super::repo::{Head, Repo};
use super::state::{self, BranchDb, RepoState, Role, SnapState, Snapshot, SyncRecord};
use crate::cli::ExitCode;
use crate::cmd::db::{self, database_oid, tcp_url};
use crate::cmd::{db_error, Ctx};
use crate::errors::{PgxError, Result};
use crate::names::{self, DbName};
use crate::out::Out;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub type Client = crate::pgwire::Client<std::os::unix::net::UnixStream>;

/// How long a command waits for the repository lock; hooks wait less.
pub const LOCK_WAIT: Duration = Duration::from_secs(300);
pub const HOOK_LOCK_WAIT: Duration = Duration::from_secs(60);

/// Test-only fault injection (`PGX_GIT_FAILPOINT=copy|validate|publish|hang`):
/// the deterministic way to prove a failed sync leaves the previous snapshot
/// in place. Compiled only with the `failpoints` Cargo feature (the test
/// suites enable it); release builds contain a no-op and ignore the variable.
#[cfg(not(feature = "failpoints"))]
pub(crate) fn failpoint(_name: &str) -> Result<()> {
    Ok(())
}

#[cfg(feature = "failpoints")]
pub(crate) fn failpoint(name: &str) -> Result<()> {
    let Ok(v) = std::env::var("PGX_GIT_FAILPOINT") else {
        return Ok(());
    };
    if (v == "hang" && name == "validate") || (v == "hang-reset" && name == "reset") {
        // Simulate a crash window: the caller kills us here.
        std::thread::sleep(Duration::from_secs(600));
    }
    if v == name {
        return Err(PgxError::failure(format!(
            "injected failure at {name} (PGX_GIT_FAILPOINT)"
        )));
    }
    Ok(())
}

/// Where a repository's `.pgx.toml` is: this worktree's top level, else the
/// main worktree's (an uncommitted `.pgx.toml` still covers linked worktrees).
pub fn config_file(repo: &Repo) -> Option<PathBuf> {
    let here = repo.toplevel.join(super::config::FILE);
    if here.is_file() {
        return Some(here);
    }
    let main_wt = repo.worktrees().into_iter().next().map(|(p, _)| p)?;
    let there = main_wt.join(super::config::FILE);
    there.is_file().then_some(there)
}

/// The effective configuration. A branch whose checked-out `.pgx.toml` has
/// no `[git]` table (older than the PGX Git setup, or no file at all) is
/// still part of a repository that ran `pgx git init`: isolation stays on,
/// with the main branch recorded in local state. An explicit
/// `enabled = false` is respected.
pub fn load_config(repo: &Repo) -> Result<(GitConfig, Option<PathBuf>)> {
    let (mut cfg, file) = load_config_file(repo)?;
    let home = crate::layout::Home::resolve();
    if home.is_initialized() {
        if let Ok(Some(st)) = state::load(&home.root, &repo.repo_id) {
            if !cfg.present {
                cfg.present = true;
                cfg.enabled = true;
            }
            // The engine's main branch is the recorded one; hooks, reset and
            // prune must agree with it whatever this checkout's file says.
            if !st.main_branch.is_empty() {
                cfg.main_branch = Some(st.main_branch.clone());
            }
        }
    }
    Ok((cfg, file))
}

/// Exactly what the checked-out `.pgx.toml` says.
pub fn load_config_file(repo: &Repo) -> Result<(GitConfig, Option<PathBuf>)> {
    match config_file(repo) {
        Some(f) => {
            let text =
                std::fs::read_to_string(&f).map_err(|e| PgxError::io("cannot read", &f, &e))?;
            let cfg = super::config::parse(&text)
                .map_err(|e| PgxError::usage(format!("{}: {}", f.display(), e.message)))?;
            Ok((cfg, Some(f)))
        }
        None => Ok((GitConfig::default(), None)),
    }
}

/// `pgx git …` outside a repository.
pub fn no_repo(dir: &Path) -> PgxError {
    PgxError::new(
        ExitCode::NotFound,
        format!("{} is not inside a Git repository", dir.display()),
    )
    .hint("run `git init` first (PGX Git gives every branch of a repository its own database)")
}

pub fn require_repo(dir: &Path) -> Result<Repo> {
    if !super::repo::git_available() {
        return Err(PgxError::new(
            ExitCode::NotReady,
            "git is not installed or not on PATH",
        ));
    }
    super::repo::discover(dir)?.ok_or_else(|| no_repo(dir))
}

/// The state of an initialised repository, or a "run pgx git init" error.
pub fn require_state(ctx: &Ctx, repo: &Repo) -> Result<RepoState> {
    state::load(&ctx.home.root, &repo.repo_id)?.ok_or_else(|| {
        PgxError::new(
            ExitCode::NotReady,
            "PGX Git is not initialised for this repository",
        )
        .hint("run `pgx git init`")
    })
}

pub fn now() -> u64 {
    crate::fsutil::now_epoch()
}

fn random_token() -> String {
    let mut buf = [0u8; 6];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut buf);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn connect_db(ctx: &Ctx, name: &str) -> Result<Client> {
    crate::pgwire::Client::connect_unix(
        &ctx.sock_dir(),
        ctx.config.port,
        "postgres",
        name,
        Duration::from_secs(30),
    )
    .map_err(|e| PgxError::failure(format!("cannot connect to {name}: {e}")))
}

/// Does `name` exist with this OID?
fn live_oid(client: &mut Client, name: &str) -> Result<Option<u64>> {
    database_oid(client, name)
}

fn drop_if_exists(client: &mut Client, name: &str) {
    if let Ok(n) = names::validate_user_target(name) {
        let _ = client.simple_query(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            n.quoted()
        ));
    }
}

/// Drop `name` only if it is still the database we staged (same OID): a
/// rename that failed must never cost a database someone else created.
fn drop_if_ours(client: &mut Client, name: &str, oid: u64) {
    if name.is_empty() || oid == 0 {
        return;
    }
    if let Ok(Some(live)) = live_oid(client, name) {
        if live == oid {
            drop_if_exists(client, name);
        }
    }
}

/// Finish or roll back operations whose owner died (killed hook, crash,
/// engine restart mid-copy). Idempotent; call with the state lock held.
///
/// No pid checks (a pid can be reused, especially in containers): branch
/// creations and resets run entirely under the state lock, so one seen in
/// progress here is dead; a snapshot in progress is dead exactly when nobody
/// holds the sync lock.
pub fn recover(ctx: &Ctx, client: &mut Client, st: &mut RepoState) -> Result<Vec<String>> {
    recover_with(ctx, client, st, false)
}

/// [`recover`] for a caller that holds the sync lock itself (a sync about to
/// stage): then no other snapshot can be in progress.
fn recover_with(
    ctx: &Ctx,
    client: &mut Client,
    st: &mut RepoState,
    sync_held: bool,
) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    let sync_alive = !sync_held && state::sync_running(&ctx.home.root, &st.repo_id)?;
    for s in st.snapshots.iter_mut() {
        if !matches!(s.state, SnapState::Preparing | SnapState::Validating) || sync_alive {
            continue;
        }
        drop_if_exists(client, &s.staging);
        drop_if_ours(client, &s.database, s.oid);
        s.state = SnapState::Failed;
        s.generation = None;
        s.error =
            Some("interrupted: the process taking it exited; staging copy removed".to_string());
        notes.push(format!("recovered an interrupted snapshot ({})", s.staging));
    }
    let mut keep = Vec::new();
    for mut b in std::mem::take(&mut st.branches) {
        if b.creating {
            // CREATE DATABASE is atomic: if the database exists it is complete.
            match live_oid(client, &b.database)? {
                Some(oid) => {
                    b.oid = oid;
                    b.creating = false;
                    notes.push(format!(
                        "finished the interrupted creation of {}",
                        b.database
                    ));
                    keep.push(b);
                }
                None => notes.push(format!(
                    "dropped the intent for {} (never created)",
                    b.database
                )),
            }
            continue;
        }
        keep.push(b);
    }
    st.branches = keep;
    if let Some(r) = st.reset.take() {
        notes.push(recover_reset(client, &r)?);
    }
    // Bound the history of failed attempts.
    let failed: Vec<usize> = st
        .snapshots
        .iter()
        .enumerate()
        .filter(|(_, s)| s.state == SnapState::Failed)
        .map(|(i, _)| i)
        .collect();
    if failed.len() > 5 {
        let drop_n = failed.len() - 5;
        let remove: std::collections::HashSet<usize> = failed.into_iter().take(drop_n).collect();
        let mut i = 0;
        st.snapshots.retain(|_| {
            let k = !remove.contains(&i);
            i += 1;
            k
        });
    }
    Ok(notes)
}

/// Finish an interrupted `pgx git reset` (state lock held): put a database
/// back under the branch's name and remove the leftovers. Every name is
/// OID-checked against the intent, so nothing else is ever touched.
pub fn recover_reset(client: &mut Client, r: &state::ResetIntent) -> Result<String> {
    // Undo rather than finish: an interrupted reset never costs the branch
    // its data. Every database is identified by the OID the intent recorded.
    let target = live_oid(client, &r.target)?;
    let old = live_oid(client, &r.old)?.filter(|o| *o == r.old_oid);
    let fresh_oid = r.fresh_oid.filter(|o| *o != 0);
    let fresh = live_oid(client, &r.fresh)?.filter(|o| Some(*o) == fresh_oid);
    let rename = |client: &mut Client, from: &str, to: &str| -> Result<()> {
        let (f, t) = (
            names::validate_user_target(from)?,
            names::validate_user_target(to)?,
        );
        client
            .simple_query(&format!(
                "ALTER DATABASE {} RENAME TO {}",
                f.quoted(),
                t.quoted()
            ))
            .map_err(|e| db_error(e, "cannot undo an interrupted reset"))?;
        Ok(())
    };
    let what = if target == Some(r.old_oid) {
        // Never moved: drop the unused fresh copy.
        if let Some(f) = fresh {
            drop_if_ours(client, &r.fresh, f);
        }
        "the original database was never touched"
    } else if let Some(o) = old {
        // Moved aside: take a fresh copy out of the branch's name if it got
        // there, then put the original back.
        if let (Some(t), Some(fo)) = (target, fresh_oid) {
            if t == fo {
                drop_if_ours(client, &r.target, t);
            }
        }
        if let Some(f) = fresh {
            drop_if_ours(client, &r.fresh, f);
        }
        let _ = o;
        rename(client, &r.old, &r.target)?;
        "the original database was put back"
    } else if target.is_some() && target == fresh_oid {
        "the reset had completed"
    } else {
        "nothing was left to do"
    };
    Ok(format!(
        "recovered an interrupted reset of {}: {what}",
        r.target
    ))
}

/// Where the main development database starts from on `pgx git init`.
#[derive(Debug, Clone)]
pub enum MainSource {
    /// An empty database.
    Empty,
    /// Clone of an existing PGX database (a template, `app`, …).
    Pgx(DbName),
    /// Import from an external PostgreSQL URL (schema-only unless `with_data`).
    Url { url: String, schema_only: bool },
}

/// Copy `src` into the new database `dst`: a copy-on-write clone when `src`
/// is idle, else a consistent logical copy (`pg_dump` → `pg_restore`, the
/// `pgx copy` machinery) so a busy database is never physically copied and
/// nobody's connection is terminated.
pub fn copy_database(
    out: &Out,
    ctx: &Ctx,
    client: &mut Client,
    src: &DbName,
    dst: &DbName,
) -> Result<(&'static str, f64)> {
    copy_database_with(out, ctx, client, src, dst, true)
}

/// [`copy_database`]; with `allow_logical = false` a busy source is an
/// `InUse` error instead of a slow logical copy.
pub fn copy_database_with(
    out: &Out,
    ctx: &Ctx,
    client: &mut Client,
    src: &DbName,
    dst: &DbName,
    allow_logical: bool,
) -> Result<(&'static str, f64)> {
    let t0 = Instant::now();
    let busy = db::active_connections(client, src.as_str())?;
    if busy == 0 {
        match db::clone_database(ctx, client, src, dst, None) {
            Ok(c) => return Ok((c.method, c.elapsed_ms)),
            Err(e) if e.code == ExitCode::InUse => {}
            Err(e) => return Err(e),
        }
    }
    if !allow_logical {
        return Err(PgxError::new(
            ExitCode::InUse,
            format!("{src} has {busy} active connection(s)"),
        ));
    }
    if crate::pgtools::locate().is_err() {
        return Err(PgxError::new(
            ExitCode::InUse,
            format!(
                "{src} has {busy} active connection(s), so it cannot be cloned file by file, and pg_dump/pg_restore (needed for a live copy) were not found"
            ),
        )
        .hint("disconnect the clients of that database (stop the app server) and retry, or install the PostgreSQL client tools"));
    }
    out.note(&format!(
        "{src} is in use: taking a consistent logical copy instead of a file clone"
    ));
    crate::cmd::copy::run(
        &out.quiet(),
        crate::cmd::copy::Opts {
            from: tcp_url(ctx, src.as_str()),
            to: dst.as_str().to_string(),
            schema_only: false,
            jobs: None,
            verbose: false,
            optimize: true,
        },
    )?;
    Ok(("dump", t0.elapsed().as_secs_f64() * 1000.0))
}

/// Ensure the main development database exists. Never overwrites: an
/// existing mapping (or a database already carrying the derived name) is
/// kept as is.
pub fn ensure_main(
    out: &Out,
    ctx: &Ctx,
    client: &mut Client,
    st: &mut RepoState,
    source: &MainSource,
) -> Result<(String, &'static str)> {
    if let Some(m) = st.main.clone() {
        match live_oid(client, &m.database)? {
            Some(oid) => {
                if oid != m.oid {
                    // Recreated under the same name (rails db:reset): adopt.
                    st.main.as_mut().unwrap().oid = oid;
                }
                return Ok((m.database, "kept"));
            }
            None => out.note(&format!(
                "the main database {} is gone; creating it again",
                m.database
            )),
        }
    }
    let mut name = naming::main_db(&st.repo_slug, &st.repo_id, false);
    let short = names::validate_user_target(&name)?;
    if let Some(oid) = live_oid(client, short.as_str())? {
        // Our derived name: a previous `pgx git init` whose state was lost.
        st.main = Some(main_entry(
            st,
            short.as_str(),
            oid,
            "adopted",
            "adopted",
            0.0,
        ));
        return Ok((name, "adopted"));
    }
    let _ = &mut name;
    let target = short;
    let (how, method, ms) = match source {
        MainSource::Empty => {
            let t0 = Instant::now();
            client
                .simple_query(&format!("CREATE DATABASE {}", target.quoted()))
                .map_err(|e| db_error(e, &format!("cannot create {target}")))?;
            (
                "created empty",
                "create",
                t0.elapsed().as_secs_f64() * 1000.0,
            )
        }
        MainSource::Pgx(src) => {
            if !db::exists(ctx, client, src)? {
                return Err(PgxError::new(ExitCode::NotFound, format!("source database {src} does not exist"))
                    .hint(format!("create it (`pgx create {src}`), import it (`pgx copy --from <url> --to {src} --schema-only`), or pass --source <postgres-url>")));
            }
            let (m, ms) = copy_database(out, ctx, client, src, &target)?;
            ("cloned", m, ms)
        }
        MainSource::Url { url, schema_only } => {
            out.note(&format!(
                "importing the main database from PostgreSQL ({}; read-only on the source)…",
                if *schema_only {
                    "schema only"
                } else {
                    "schema and data"
                }
            ));
            let t0 = Instant::now();
            crate::cmd::copy::run(
                &out.quiet(),
                crate::cmd::copy::Opts {
                    from: url.clone(),
                    to: target.as_str().to_string(),
                    schema_only: *schema_only,
                    jobs: None,
                    verbose: false,
                    optimize: true,
                },
            )?;
            (
                if *schema_only {
                    "imported (schema only)"
                } else {
                    "imported (with data)"
                },
                "copy",
                t0.elapsed().as_secs_f64() * 1000.0,
            )
        }
    };
    let oid = live_oid(client, target.as_str())?
        .ok_or_else(|| PgxError::failure(format!("{target} vanished after it was created")))?;
    let src_label = match source {
        MainSource::Empty => "empty".to_string(),
        MainSource::Pgx(s) => s.as_str().to_string(),
        MainSource::Url { .. } => "postgres import".to_string(),
    };
    st.main = Some(main_entry(st, target.as_str(), oid, &src_label, method, ms));
    Ok((target.as_str().to_string(), how))
}

fn main_entry(
    st: &RepoState,
    name: &str,
    oid: u64,
    source: &str,
    method: &str,
    ms: f64,
) -> BranchDb {
    BranchDb {
        role: Role::Main,
        worktree_id: String::new(),
        worktree_path: String::new(),
        branch: st.main_branch.clone(),
        database: name.to_string(),
        oid,
        base_generation: None,
        source: source.to_string(),
        created_at: now(),
        last_used: now(),
        creating: false,
        owner_pid: 0,
        method: method.to_string(),
        elapsed_ms: ms,
        renamed_from: None,
    }
}

/// How to recreate a main database that disappeared: from the current
/// snapshot when there is one (its latest validated state), else empty.
fn repair_source(client: &mut Client, st: &RepoState) -> Result<MainSource> {
    if let Some(m) = &st.main {
        if live_oid(client, &m.database)?.is_none() {
            if let Some(s) = st.current() {
                if live_oid(client, &s.database)? == Some(s.oid) {
                    return Ok(MainSource::Pgx(names::validate(&s.database)?));
                }
            }
        }
    }
    Ok(MainSource::Empty)
}

#[derive(Debug, Clone, Default)]
pub struct SyncOpts {
    pub force: bool,
    /// Run the configured migration command against main first (explicit
    /// request; records approval for unattended runs).
    pub migrate: bool,
}

#[derive(Debug, Clone)]
pub struct SyncOutcome {
    /// `published`, `unchanged`, `pending`, `failed`.
    pub status: &'static str,
    pub generation: Option<u64>,
    pub previous: Option<u64>,
    pub message: String,
    pub pending: Vec<String>,
    pub method: String,
    pub elapsed_ms: f64,
    pub migrated: bool,
}

impl SyncOutcome {
    pub fn ok(&self) -> bool {
        self.status != "failed"
    }
}

/// Main's state right now: its fingerprint, the migrations committed at
/// main's Git revision, and which of them the database has not applied.
pub struct MainProbe {
    pub fp: Fingerprint,
    pub rev: Option<String>,
    pub pending: Vec<String>,
}

pub fn probe_main(ctx: &Ctx, repo: &Repo, st: &RepoState, cfg: &GitConfig) -> Result<MainProbe> {
    let main = st.main.as_ref().ok_or_else(|| {
        PgxError::new(ExitCode::NotReady, "no main database").hint("run `pgx git init`")
    })?;
    let mut c = connect_db(ctx, &main.database)?;
    let fp = fingerprint::compute(&mut c, cfg.sync_mode == super::config::SyncMode::Data)?;
    c.close();
    let rev = repo.branch_rev(&st.main_branch);
    let committed = rev
        .as_deref()
        .map(|r| fingerprint::committed_migrations(&repo.files_at(r)))
        .unwrap_or_default();
    let pending = fingerprint::pending(&committed, &fp);
    Ok(MainProbe { fp, rev, pending })
}

/// Run the configured migration command against the main database, from
/// the worktree that has main checked out. argv only (no shell), the main
/// database's connection in the environment, output to the terminal.
fn run_migrations(ctx: &Ctx, repo: &Repo, st: &RepoState, cfg: &GitConfig) -> Result<()> {
    let cmd_text = cfg.migrate_command.as_deref().ok_or_else(|| {
        PgxError::usage("no migration command is configured")
            .hint("set [git.migrations] command = \"bin/rails db:migrate\" in .pgx.toml")
    })?;
    let argv = super::config::split_words(cmd_text)?;
    let wt = repo.worktree_of(&st.main_branch).ok_or_else(|| {
        PgxError::new(
            ExitCode::NotReady,
            format!(
                "{} is not checked out in any worktree, so its migrations cannot be run",
                st.main_branch
            ),
        )
        .hint(format!(
            "check out {} somewhere (git worktree add ../main {}) and retry",
            st.main_branch, st.main_branch
        ))
    })?;
    let main = st.main.as_ref().unwrap();
    let pairs = super::env::pairs_for(ctx, main, &st.main_branch, &wt, "main");
    eprintln!(
        "pgx git: running `{cmd_text}` against {} in {}",
        main.database,
        wt.display()
    );
    let mut c = std::process::Command::new(&argv[0]);
    c.args(&argv[1..])
        .current_dir(&wt)
        .stdin(std::process::Stdio::null());
    // stdout → stderr so `--json` stays parseable.
    c.stdout(std::io::stderr());
    for (k, v) in pairs {
        c.env(k, v);
    }
    for k in ["PGSERVICE", "PGSERVICEFILE"] {
        c.env_remove(k);
    }
    let status = c
        .status()
        .map_err(|e| PgxError::failure(format!("cannot run {}: {e}", argv[0])))?;
    if !status.success() {
        return Err(PgxError::failure(format!(
            "the migration command failed ({status})"
        )));
    }
    Ok(())
}

fn record(st: &mut RepoState, o: &SyncOutcome) {
    st.last_sync = Some(SyncRecord {
        at: now(),
        status: o.status.to_string(),
        generation: o.generation,
        message: o.message.clone(),
        pending_migrations: o.pending.clone(),
        elapsed_ms: o.elapsed_ms,
    });
}

/// Refresh the immutable main snapshot when main changed. Two entry points:
///
/// - [`sync_auto`] (Git hooks, lazily before a new branch database, `reset`):
///   called with the state lock held. It never waits and never takes a slow
///   copy: it keeps the current baseline and says why when another snapshot
///   is being taken, when main is in use (it may be mid-migration or
///   mid-`db:reset`), when main is missing, or when migrations committed on
///   main are not applied yet. Otherwise it clones the idle main (fast).
/// - [`sync_explicit`] (`pgx git sync`, `pgx git init`): takes the locks
///   itself. A busy main is copied logically, and that slow copy runs with
///   only the sync lock held, so hooks and agents are never stuck behind it.
///
/// Failures never touch the published snapshot or any branch database: they
/// are recorded (`pgx git status` shows them) and returned as `failed`.
pub fn sync_auto(
    out: &Out,
    ctx: &Ctx,
    client: &mut Client,
    repo: &Repo,
    st: &mut RepoState,
    cfg: &GitConfig,
) -> Result<SyncOutcome> {
    let t0 = Instant::now();
    let Some(_sync) = state::try_sync_lock(&ctx.home.root, &st.repo_id)? else {
        let g = st.current_generation();
        return Ok(outcome(
            "skipped",
            g,
            g,
            format!(
                "another main snapshot is being taken; {} stays the baseline for now",
                gen_name(g)
            ),
            Vec::new(),
            t0,
            false,
        ));
    };
    match plan(out, ctx, client, repo, st, cfg, &SyncOpts::default(), true)? {
        Plan::Done(o) => Ok(o),
        Plan::Stage(p) => {
            let r = copy_validate(out, ctx, client, &p, cfg, false)
                .and_then(|(m, got)| publish(ctx, client, st, &p, m, &got));
            finish(ctx, client, st, &p, r, t0)
        }
    }
}

pub fn sync_explicit(
    out: &Out,
    ctx: &Ctx,
    repo: &Repo,
    cfg: &GitConfig,
    opts: &SyncOpts,
) -> Result<SyncOutcome> {
    let t0 = Instant::now();
    let _sync = state::sync_lock(&ctx.home.root, &repo.repo_id, LOCK_WAIT)?;
    let p = {
        let _l = state::lock(&ctx.home.root, &repo.repo_id, LOCK_WAIT)?;
        let mut st = require_state(ctx, repo)?;
        let mut client = ctx.connect("postgres")?;
        let planned = plan(out, ctx, &mut client, repo, &mut st, cfg, opts, false);
        client.close();
        match planned? {
            Plan::Done(o) => return Ok(o),
            Plan::Stage(p) => p,
        }
    };
    // The copy: only the sync lock is held.
    let mut client = ctx.connect("postgres")?;
    let copied = copy_validate(out, ctx, &mut client, &p, cfg, true);
    let _l = state::lock(&ctx.home.root, &repo.repo_id, LOCK_WAIT)?;
    let mut st = require_state(ctx, repo)?;
    let r = copied.and_then(|(m, got)| publish(ctx, &mut client, &mut st, &p, m, &got));
    let o = finish(ctx, &mut client, &mut st, &p, r, t0);
    client.close();
    o
}

fn gen_name(g: Option<u64>) -> String {
    g.map(|g| format!("v{g}"))
        .unwrap_or_else(|| "no snapshot".to_string())
}

fn outcome(
    status: &'static str,
    generation: Option<u64>,
    previous: Option<u64>,
    message: String,
    pending: Vec<String>,
    t0: Instant,
    migrated: bool,
) -> SyncOutcome {
    SyncOutcome {
        status,
        generation,
        previous,
        message,
        pending,
        method: String::new(),
        elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
        migrated,
    }
}

/// Record a final outcome and save (state lock held).
fn settle(ctx: &Ctx, st: &mut RepoState, o: SyncOutcome) -> Result<SyncOutcome> {
    record(st, &o);
    state::save(&ctx.home.root, st)?;
    Ok(o)
}

/// What a sync will do once main has been examined.
enum Plan {
    Done(SyncOutcome),
    Stage(Prepared),
}

struct Prepared {
    staging: DbName,
    main: DbName,
    probe: MainProbe,
    previous: Option<u64>,
    migrated: bool,
    pending_note: String,
}

/// Examine main (state lock held): recover, repair, fingerprint, migrations,
/// decide; for a new snapshot, record the staging intent and save.
#[allow(clippy::too_many_arguments)]
fn plan(
    out: &Out,
    ctx: &Ctx,
    client: &mut Client,
    repo: &Repo,
    st: &mut RepoState,
    cfg: &GitConfig,
    opts: &SyncOpts,
    auto: bool,
) -> Result<Plan> {
    let t0 = Instant::now();
    for n in recover_with(ctx, client, st, true)? {
        out.note(&format!("pgx git: {n}"));
    }
    // A published snapshot whose database disappeared (dropped by hand) is
    // retired, so the baseline falls back to one that exists or is rebuilt.
    for s in st.snapshots.iter_mut() {
        if s.state == SnapState::Ready && live_oid(client, &s.database)? != Some(s.oid) {
            s.state = SnapState::Retired;
            s.error = Some("the snapshot database disappeared".to_string());
            out.note(&format!(
                "pgx git: snapshot {} is missing; it no longer counts as a baseline",
                s.database
            ));
        }
    }
    let previous = st.current_generation();
    let failed = |st: &mut RepoState, message: String, migrated: bool| -> Result<Plan> {
        let o = outcome(
            "failed",
            previous,
            previous,
            format!(
                "main snapshot not refreshed: {message}{}",
                match previous {
                    Some(p) => format!(" (v{p} is still the baseline)"),
                    None => String::new(),
                }
            ),
            Vec::new(),
            t0,
            migrated,
        );
        Ok(Plan::Done(settle(ctx, st, o)?))
    };
    let skip = |st: &mut RepoState,
                why: String,
                pending: Vec<String>,
                status: &'static str|
     -> Result<Plan> {
        let o = outcome(
            status,
            previous,
            previous,
            format!("{why}; {} stays the baseline", gen_name(previous)),
            pending,
            t0,
            false,
        );
        Ok(Plan::Done(settle(ctx, st, o)?))
    };
    let main_missing = match &st.main {
        Some(m) => live_oid(client, &m.database)?.is_none(),
        None => true,
    };
    if auto && main_missing && previous.is_some() {
        // Possibly mid-`db:reset`: never recreate it behind the developer's back.
        return skip(
            st,
            "the main database does not exist right now".to_string(),
            Vec::new(),
            "skipped",
        );
    }
    let repair = repair_source(client, st)?;
    let (main_name, how) = ensure_main(out, ctx, client, st, &repair)?;
    if how == "created empty" && previous.is_some() {
        // Never publish an empty main as the new baseline.
        return failed(
            st,
            format!("the main database was missing and is now an empty {main_name}"),
            false,
        );
    }
    let mut probe = match probe_main(ctx, repo, st, cfg) {
        Ok(p) => p,
        Err(e) => return failed(st, e.message, false),
    };
    let mut migrated = false;
    if !probe.pending.is_empty() && cfg.migrate_command.is_some() {
        let approved = st.approved_migrate_command.as_deref() == cfg.migrate_command.as_deref();
        if opts.migrate || (cfg.auto_apply_main && approved) {
            if let Err(e) = run_migrations(ctx, repo, st, cfg) {
                if opts.migrate {
                    return Err(e);
                }
                return failed(
                    st,
                    format!("the migration command failed: {}", e.message),
                    false,
                );
            }
            if opts.migrate {
                st.approved_migrate_command = cfg.migrate_command.clone();
            }
            migrated = true;
            probe = match probe_main(ctx, repo, st, cfg) {
                Ok(p) => p,
                Err(e) => return failed(st, e.message, true),
            };
        }
    }
    let pending_note = if probe.pending.is_empty() {
        String::new()
    } else {
        format!(
            "; {} migration(s) committed on {} are not applied to the main database",
            probe.pending.len(),
            st.main_branch
        )
    };
    if let Some(cur) = st.current() {
        if !opts.force
            && probe.fp.same_as(
                &cur.schema_fingerprint,
                &cur.migration_fingerprint,
                cur.data_fingerprint.as_deref(),
            )
        {
            let o = outcome(
                if probe.pending.is_empty() {
                    "unchanged"
                } else {
                    "pending"
                },
                cur.generation,
                previous,
                format!(
                    "main snapshot v{} is up to date{pending_note}",
                    cur.generation.unwrap_or(0)
                ),
                probe.pending.clone(),
                t0,
                migrated,
            );
            return Ok(Plan::Done(settle(ctx, st, o)?));
        }
    }
    let main = names::validate(&main_name)?;
    if auto && previous.is_some() {
        // Automatic snapshots only of a settled main: no unapplied
        // migrations (a half-migrated main is not a baseline) and nobody
        // connected (a migration or reset may be running right now).
        if !probe.pending.is_empty() {
            return skip(
                st,
                format!(
                    "main changed but {} migration(s) committed on {} are not applied yet (run them, then `pgx git sync`)",
                    probe.pending.len(),
                    st.main_branch
                ),
                probe.pending.clone(),
                "pending",
            );
        }
        let busy = db::active_connections(client, main.as_str())?;
        if busy > 0 {
            return skip(
                st,
                format!(
                    "main changed but has {busy} active connection(s) (a migration may be running); `pgx git sync` takes a consistent copy"
                ),
                Vec::new(),
                "skipped",
            );
        }
    }

    let staging = names::validate_user_target(&naming::staging_db(&st.repo_slug, &random_token()))?;
    st.snapshots.push(Snapshot {
        generation: None,
        database: String::new(),
        staging: staging.as_str().to_string(),
        oid: 0,
        state: SnapState::Preparing,
        owner_pid: std::process::id(),
        source_database: main_name.clone(),
        source_git_revision: probe.rev.clone(),
        schema_fingerprint: probe.fp.schema.clone(),
        migration_fingerprint: probe.fp.migrations.clone(),
        data_fingerprint: probe.fp.data.clone(),
        migrations_applied: probe.fp.applied.len() as u64,
        tables: probe.fp.tables,
        created_at: now(),
        method: String::new(),
        elapsed_ms: 0.0,
        locked: false,
        error: None,
    });
    state::save(&ctx.home.root, st)?;
    Ok(Plan::Stage(Prepared {
        staging,
        main,
        probe,
        previous,
        migrated,
        pending_note,
    }))
}

/// Copy main into the staging database and prove the copy is main's state.
/// Needs no state lock (the sync lock protects the staging database).
fn copy_validate(
    out: &Out,
    ctx: &Ctx,
    client: &mut Client,
    p: &Prepared,
    cfg: &GitConfig,
    allow_logical: bool,
) -> Result<(&'static str, Fingerprint)> {
    let (method, _) = copy_database_with(out, ctx, client, &p.main, &p.staging, allow_logical)?;
    failpoint("copy")?;
    let data = cfg.sync_mode == super::config::SyncMode::Data;
    let mut sc = connect_db(ctx, p.staging.as_str())?;
    let got = fingerprint::compute(&mut sc, data)?;
    failpoint("validate")?;
    let matches = |fp: &Fingerprint| fp.same_as(&got.schema, &got.migrations, got.data.as_deref());
    if !matches(&p.probe.fp) {
        // A logical copy is a snapshot at dump time; main may have moved on since.
        let mut mc = connect_db(ctx, p.main.as_str())?;
        let again = fingerprint::compute(&mut mc, data)?;
        mc.close();
        if !matches(&again) {
            sc.close();
            return Err(PgxError::failure(
                "validation failed: the staged copy does not match the main database (main changed during the copy?); retry `pgx git sync`",
            ));
        }
    }
    // Frozen pages stay shared by every clone (no hint-bit rewrites on first read).
    let _ = sc.simple_query("VACUUM (FREEZE, ANALYZE)");
    sc.close();
    Ok((method, got))
}

/// Publish atomically (state lock held): the generation exists only once the
/// renamed, read-only database exists and the state file names it.
fn publish(
    ctx: &Ctx,
    client: &mut Client,
    st: &mut RepoState,
    p: &Prepared,
    method: &'static str,
    got: &Fingerprint,
) -> Result<u64> {
    let idx = st
        .snapshots
        .iter()
        .position(|s| s.staging == p.staging.as_str() && s.state == SnapState::Preparing)
        .ok_or_else(|| {
            PgxError::failure("the staging record disappeared from the PGX Git state")
        })?;
    {
        let s = &mut st.snapshots[idx];
        s.schema_fingerprint = got.schema.clone();
        s.migration_fingerprint = got.migrations.clone();
        s.data_fingerprint = got.data.clone();
        s.migrations_applied = got.applied.len() as u64;
        s.tables = got.tables;
        s.method = method.to_string();
    }
    let generation = st.next_generation();
    let final_name =
        names::validate_user_target(&naming::snapshot_db(&st.repo_slug, &st.repo_id, generation))?;
    if live_oid(client, final_name.as_str())?.is_some() {
        return Err(PgxError::new(
            ExitCode::Conflict,
            format!("{final_name} already exists and is not a snapshot this repository published"),
        ));
    }
    // Record the staged copy's OID with the final name: recovery may drop
    // the final name only while it is still this copy.
    st.snapshots[idx].oid = live_oid(client, p.staging.as_str())?.unwrap_or(0);
    st.snapshots[idx].database = final_name.as_str().to_string();
    state::save(&ctx.home.root, st)?;
    failpoint("publish")?;
    client
        .simple_query(&format!(
            "ALTER DATABASE {} RENAME TO {}",
            p.staging.quoted(),
            final_name.quoted()
        ))
        .map_err(|e| db_error(e, "cannot publish the snapshot"))?;
    // A snapshot that could not be made read-only is not published.
    failpoint("lock")?;
    client
        .simple_query(&format!(
            "ALTER DATABASE {} ALLOW_CONNECTIONS false",
            final_name.quoted()
        ))
        .map_err(|e| {
            db_error(
                e,
                "cannot make the snapshot read-only (ALLOW_CONNECTIONS false)",
            )
        })?;
    let oid = live_oid(client, final_name.as_str())?
        .ok_or_else(|| PgxError::failure("the published snapshot vanished"))?;
    let prev = st.current_generation();
    {
        let s = &mut st.snapshots[idx];
        s.generation = Some(generation);
        s.oid = oid;
        s.locked = true;
        s.state = SnapState::Ready;
        s.owner_pid = 0;
    }
    // Keep the new baseline and the one before it; older ones are retired
    // (still present until `pgx git prune --apply` removes them).
    for s in st.snapshots.iter_mut() {
        if s.state == SnapState::Ready
            && s.generation.is_some()
            && s.generation < prev
            && s.generation != Some(generation)
        {
            s.state = SnapState::Retired;
        }
    }
    state::save(&ctx.home.root, st)?;
    Ok(generation)
}

/// Turn the result of copy + publish into an outcome (state lock held):
/// on failure, remove what was staged and keep the previous baseline.
fn finish(
    ctx: &Ctx,
    client: &mut Client,
    st: &mut RepoState,
    p: &Prepared,
    r: Result<u64>,
    t0: Instant,
) -> Result<SyncOutcome> {
    let pos = st
        .snapshots
        .iter()
        .position(|s| s.staging == p.staging.as_str());
    let o = match r {
        Ok(generation) => {
            let method = pos
                .map(|i| st.snapshots[i].method.clone())
                .unwrap_or_default();
            if let Some(i) = pos {
                st.snapshots[i].elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
            }
            SyncOutcome {
                status: if p.probe.pending.is_empty() {
                    "published"
                } else {
                    "pending"
                },
                generation: Some(generation),
                previous: p.previous,
                message: format!(
                    "published main snapshot v{generation}{}{}",
                    match p.previous {
                        Some(v) => format!(" (previous v{v} kept)"),
                        None => String::new(),
                    },
                    p.pending_note
                ),
                pending: p.probe.pending.clone(),
                method,
                elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
                migrated: p.migrated,
            }
        }
        Err(e) => {
            drop_if_exists(client, p.staging.as_str());
            if let Some(i) = pos {
                if st.snapshots[i].state != SnapState::Ready {
                    let (name, oid) = (st.snapshots[i].database.clone(), st.snapshots[i].oid);
                    drop_if_ours(client, &name, oid);
                    let s = &mut st.snapshots[i];
                    s.state = SnapState::Failed;
                    s.generation = None;
                    s.error = Some(e.message.clone());
                }
            }
            SyncOutcome {
                status: "failed",
                generation: p.previous,
                previous: p.previous,
                message: format!(
                    "main snapshot not refreshed: {}{}",
                    e.message,
                    match p.previous {
                        Some(v) => format!(" (v{v} is still the baseline)"),
                        None => String::new(),
                    }
                ),
                pending: p.probe.pending.clone(),
                method: String::new(),
                elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
                migrated: p.migrated,
            }
        }
    };
    settle(ctx, st, o)
}

/// What `resolve_head` found or made for the current worktree.
#[derive(Debug, Clone)]
pub struct Resolution {
    pub db: BranchDb,
    pub git_label: String,
    pub created: bool,
    /// Recreated because the mapped database had disappeared.
    pub recreated: bool,
    pub renamed_from: Option<String>,
    pub sync: Option<SyncOutcome>,
    pub notes: Vec<String>,
}

/// Should a database be created for a missing mapping?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Create {
    /// Create it (checkout hooks with `auto_branch`, agents, `exec`, `env`).
    Yes,
    /// Report "not found" instead (`status`).
    No,
}

/// The database for the worktree's HEAD: main's for the main branch, the
/// branch's own otherwise (created from the latest validated main snapshot
/// on first use), a per-worktree one for a detached HEAD. Call with the lock
/// held; saves `st`.
#[allow(clippy::too_many_arguments)]
pub fn resolve_head(
    out: &Out,
    ctx: &Ctx,
    client: &mut Client,
    repo: &Repo,
    st: &mut RepoState,
    cfg: &GitConfig,
    create: Create,
    allow_sync: bool,
) -> Result<Resolution> {
    for n in recover(ctx, client, st)? {
        out.note(&format!("pgx git: {n}"));
    }
    let (role, branch) = match &repo.head {
        Head::Branch(b) => (
            if *b == st.main_branch {
                Role::Main
            } else {
                Role::Branch
            },
            b.clone(),
        ),
        Head::Detached(_) => match repo.rebasing_branch() {
            Some(b) if b == st.main_branch => (Role::Main, b),
            Some(b) => (Role::Branch, b),
            None => (Role::Detached, "HEAD".to_string()),
        },
    };
    let label = repo.head.label();
    if role == Role::Main {
        let repair = repair_source(client, st)?;
        let (name, how) = ensure_main(out, ctx, client, st, &repair)?;
        let m = st.main.as_mut().unwrap();
        m.last_used = now();
        let db = m.clone();
        state::save(&ctx.home.root, st)?;
        return Ok(Resolution {
            db,
            git_label: label,
            created: how != "kept",
            recreated: how == "created empty" || how == "cloned",
            renamed_from: None,
            sync: None,
            notes: match how {
                "created empty" => vec![format!(
                    "main database {name} was missing; created it empty"
                )],
                "cloned" => vec![format!(
                    "main database {name} was missing; restored it from the latest main snapshot"
                )],
                _ => vec![],
            },
        });
    }

    let mut recreated = false;
    if let Some(i) = st.find_branch(&repo.worktree_id, &branch, role) {
        let rec = st.branches[i].clone();
        match live_oid(client, &rec.database)? {
            Some(oid) => {
                let b = &mut st.branches[i];
                if oid != b.oid {
                    // Dropped and recreated under its name by the app (db:reset): adopt.
                    b.oid = oid;
                }
                b.last_used = now();
                b.worktree_path = repo.toplevel.to_string_lossy().into_owned();
                let db = b.clone();
                state::save(&ctx.home.root, st)?;
                return Ok(Resolution {
                    db,
                    git_label: label,
                    created: false,
                    recreated: false,
                    renamed_from: None,
                    sync: None,
                    notes: vec![],
                });
            }
            None => {
                if create == Create::No {
                    return Err(PgxError::new(
                        ExitCode::NotFound,
                        format!("the database {} of {branch} no longer exists", rec.database),
                    )
                    .hint(
                        "`pgx git env` or the next checkout recreates it from the main snapshot",
                    ));
                }
                st.branches.remove(i);
                recreated = true;
            }
        }
    } else if role == Role::Branch {
        // `git branch -m old new`: no hook fires; carry the mapping over.
        if let Some(old) = repo.renamed_from(&branch) {
            if !repo.branch_exists(&old) {
                if let Some(i) = st.find_branch(&repo.worktree_id, &old, Role::Branch) {
                    let b = &mut st.branches[i];
                    b.branch = branch.clone();
                    b.renamed_from = Some(old.clone());
                    b.last_used = now();
                    let db = b.clone();
                    state::save(&ctx.home.root, st)?;
                    return Ok(Resolution {
                        db,
                        git_label: label,
                        created: false,
                        recreated: false,
                        renamed_from: Some(old),
                        sync: None,
                        notes: vec![],
                    });
                }
            }
        }
    }
    if create == Create::No {
        return Err(PgxError::new(
            ExitCode::NotFound,
            format!("{label} has no PGX database yet"),
        )
        .hint(
            "`git switch` to it again, or run `pgx git env`, to create it from the main snapshot",
        ));
    }

    // Lazy reconciliation: before cloning, make sure the baseline is current.
    let mut sync = None;
    if allow_sync && (cfg.auto_sync || st.current().is_none()) {
        let o = sync_auto(out, ctx, client, repo, st, cfg)?;
        sync = Some(o);
    }
    let snap = st.current().cloned().ok_or_else(|| {
        PgxError::new(
            ExitCode::NotReady,
            "there is no validated main snapshot to branch from",
        )
        .hint(match &sync {
            Some(o) => format!("{} — fix it and run `pgx git sync`", o.message),
            None => "run `pgx git sync`".to_string(),
        })
    })?;
    let snap_db = names::validate(&snap.database)?;

    let short = match role {
        Role::Detached => naming::detached_db(&st.repo_slug, &st.repo_id, &repo.worktree_id, false),
        _ => naming::branch_db(
            &st.repo_slug,
            &st.repo_id,
            &repo.worktree_id,
            &branch,
            false,
        ),
    };
    let long = match role {
        Role::Detached => naming::detached_db(&st.repo_slug, &st.repo_id, &repo.worktree_id, true),
        _ => naming::branch_db(&st.repo_slug, &st.repo_id, &repo.worktree_id, &branch, true),
    };
    let mut name = names::validate_user_target(&short)?;
    if live_oid(client, name.as_str())?.is_some() {
        // Taken by a database this repository does not map here (a manual
        // one, or another worktree's): never reuse or overwrite it.
        name = names::validate_user_target(&long)?;
        if live_oid(client, name.as_str())?.is_some() {
            return Err(PgxError::new(
                ExitCode::Conflict,
                format!("databases {short} and {long} both exist and are not mapped to {branch} in this worktree"),
            )
            .hint("`pgx git doctor` lists mapping collisions"));
        }
    }
    // Intent first: a crash after the clone finishes the mapping on recovery.
    st.branches.push(BranchDb {
        role,
        worktree_id: repo.worktree_id.clone(),
        worktree_path: repo.toplevel.to_string_lossy().into_owned(),
        branch: branch.clone(),
        database: name.as_str().to_string(),
        oid: 0,
        base_generation: snap.generation,
        source: snap.database.clone(),
        created_at: now(),
        last_used: now(),
        creating: true,
        owner_pid: std::process::id(),
        method: String::new(),
        elapsed_ms: 0.0,
        renamed_from: None,
    });
    let idx = st.branches.len() - 1;
    state::save(&ctx.home.root, st)?;
    let cloned = match db::clone_database(ctx, client, &snap_db, &name, None) {
        Ok(c) => c,
        Err(e) => {
            st.branches.remove(idx);
            state::save(&ctx.home.root, st)?;
            return Err(e);
        }
    };
    let oid = live_oid(client, name.as_str())?
        .ok_or_else(|| PgxError::failure(format!("{name} vanished after the clone")))?;
    {
        let b = &mut st.branches[idx];
        b.oid = oid;
        b.creating = false;
        b.owner_pid = 0;
        b.method = cloned.method.to_string();
        b.elapsed_ms = cloned.elapsed_ms;
    }
    let db = st.branches[idx].clone();
    state::save(&ctx.home.root, st)?;
    Ok(Resolution {
        db,
        git_label: label,
        created: true,
        recreated,
        renamed_from: None,
        sync,
        notes: vec![],
    })
}

/// Give the repository and this worktree durable identities (id files in
/// the Git directory), so moving the repository or `git worktree move` keeps
/// its databases. A copied repository (its id file names a repository whose
/// recorded directory still exists elsewhere) gets a new identity instead of
/// sharing the original's databases. Only called for repositories that use
/// PGX Git: nothing is ever written into other repositories.
pub fn ensure_identity(home: &Path, repo: &mut Repo) -> Result<()> {
    use super::repo::{read_id, REPO_ID_FILE, WORKTREE_ID_FILE};
    let write = |p: &Path, id: &str| -> Result<()> {
        std::fs::write(p, format!("{id}\n")).map_err(|e| PgxError::io("cannot write", p, &e))
    };
    let rid_file = repo.common_dir.join(REPO_ID_FILE);
    match read_id(&rid_file) {
        None => write(&rid_file, &repo.repo_id)?,
        Some(id) => {
            if let Ok(Some(st)) = state::load(home, &id) {
                let recorded = PathBuf::from(&st.common_dir);
                let elsewhere = !st.common_dir.is_empty()
                    && std::fs::canonicalize(&recorded).unwrap_or(recorded.clone())
                        != repo.common_dir;
                if elsewhere
                    && read_id(&recorded.join(REPO_ID_FILE)).as_deref() == Some(id.as_str())
                {
                    // Both copies exist: this one is the copy.
                    use crate::cmd::agent::identity::{fnv1a64, hex16};
                    let fresh = hex16(fnv1a64(
                        format!("{}|{}|{}", repo.common_dir.display(), now(), random_token())
                            .as_bytes(),
                    ));
                    write(&rid_file, &fresh)?;
                    repo.repo_id = fresh;
                }
            }
        }
    }
    let wid_file = repo.git_dir.join(WORKTREE_ID_FILE);
    if read_id(&wid_file).is_none() {
        write(&wid_file, &repo.worktree_id)?;
    }
    Ok(())
}

/// Start (and if needed initialise) the runtime, the way `pgx agent setup` does.
pub fn runtime(out: &Out, init_if_needed: bool) -> Result<Ctx> {
    let home = crate::layout::Home::resolve();
    if !home.is_initialized() {
        if !init_if_needed {
            return Err(PgxError::new(
                ExitCode::NotReady,
                format!("PGX is not initialised at {}", home.root.display()),
            )
            .hint("run `pgx git init` (or `pgx run`) once"));
        }
        out.note("initialising a new PGX cluster…");
        let port = std::env::var("PGX_PORT").ok().and_then(|p| p.parse().ok());
        let durability = std::env::var("PGX_DURABILITY").ok();
        crate::cmd::init::run(&out.quiet(), port, durability, false)?;
    }
    let ctx = Ctx::load()?;
    crate::cmd::start::ensure_running(&out.quiet(), &ctx)?;
    crate::cmd::start::spawn_supervisor(&ctx.home);
    Ok(ctx)
}

/// Load config + state and resolve the current HEAD's database in one go,
/// under the lock. Used by `env`, `exec`, hooks and the agent integration.
pub fn resolve_here(
    out: &Out,
    dir: &Path,
    create: Create,
    lock_wait: Duration,
) -> Result<(Ctx, Repo, GitConfig, RepoState, Resolution)> {
    let mut repo = require_repo(dir)?;
    let (cfg, _file) = load_config(&repo)?;
    if cfg.is_on() {
        ensure_identity(&crate::layout::Home::resolve().root, &mut repo)?;
    }
    if !cfg.is_on() {
        return Err(PgxError::new(
            ExitCode::NotReady,
            "PGX Git is not enabled for this repository",
        )
        .hint("run `pgx git init`"));
    }
    let ctx = runtime(out, false)?;
    let _lock = state::lock(&ctx.home.root, &repo.repo_id, lock_wait)?;
    let mut st = require_state(&ctx, &repo)?;
    // The repository moved (same identity, new place): follow it.
    let here = repo.common_dir.to_string_lossy().into_owned();
    if st.common_dir != here {
        st.common_dir = here;
    }
    let mut client = ctx.connect("postgres")?;
    let res = resolve_head(out, &ctx, &mut client, &repo, &mut st, &cfg, create, true);
    client.close();
    let res = res?;
    Ok((ctx, repo, cfg, st, res))
}

// ------------------------------------------------------------ agents

/// What the agent integration needs to know about a session's Git context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentGit {
    /// Where the session runs (resolution starts here).
    pub dir: PathBuf,
    pub repo_id: String,
    pub worktree_id: String,
    /// `feature/auth`, or `(detached at …)`.
    pub label: String,
    pub worktree: String,
    /// 16 hex over worktree + branch: part of the session key.
    pub scope: String,
}

/// The PGX Git binding for an agent session started in `dir`, when the
/// repository opted in (`[git]` enabled and `[git.agents] enabled`). A
/// broken `.pgx.toml` is an error (the session must fail closed, not fall
/// back to a default source).
pub fn agent_binding(dir: &Path) -> Result<Option<AgentGit>> {
    if !super::repo::git_available() {
        // Without git we cannot tell the branch: refuse if the project opted in.
        let mut cur = Some(dir);
        while let Some(d) = cur {
            if let Ok(t) = std::fs::read_to_string(d.join(super::config::FILE)) {
                if super::config::parse(&t).is_ok_and(|c| c.is_on() && c.agents_enabled) {
                    return Err(PgxError::new(
                        ExitCode::NotReady,
                        "this project uses PGX Git but git is not available, so the session's Git branch is unknown",
                    ));
                }
                break;
            }
            cur = d.parent();
        }
        return Ok(None);
    }
    let Some(repo) = super::repo::discover(dir)? else {
        return Ok(None);
    };
    let (cfg, _) = load_config(&repo)?;
    if !cfg.is_on() || !cfg.agents_enabled {
        return Ok(None);
    }
    let branch_key = match &repo.head {
        Head::Branch(b) => format!("branch:{b}"),
        Head::Detached(_) => match repo.rebasing_branch() {
            Some(b) => format!("branch:{b}"),
            None => "detached".to_string(),
        },
    };
    use crate::cmd::agent::identity::{fnv1a64, hex16};
    Ok(Some(AgentGit {
        dir: dir.to_path_buf(),
        scope: hex16(fnv1a64(
            format!("{}|{}|{branch_key}", repo.repo_id, repo.worktree_id).as_bytes(),
        )),
        repo_id: repo.repo_id.clone(),
        worktree_id: repo.worktree_id.clone(),
        label: repo.head.label(),
        worktree: repo.toplevel.to_string_lossy().into_owned(),
    }))
}

pub struct AgentSource {
    /// The Git branch database to clone the agent's branch from.
    pub database: DbName,
    /// The current validated main snapshot (an always-idle fallback).
    pub snapshot: Option<DbName>,
    /// The Git branch is main (its database is the main development database).
    pub main: bool,
}

/// Resolve (creating if needed) the database of the Git branch the agent
/// works on. Same path as a checkout hook, so an agent that starts on a
/// branch nobody checked out yet with hooks still gets a correct source.
pub fn agent_source(out: &Out, g: &AgentGit) -> Result<AgentSource> {
    // State-lock holds are short now (no slow copy runs under it), so an
    // agent never waits long; a genuinely stuck lock fails closed.
    let (_ctx, _repo, _cfg, st, res) = resolve_here(out, &g.dir, Create::Yes, HOOK_LOCK_WAIT)?;
    Ok(AgentSource {
        database: names::validate(&res.db.database)?,
        snapshot: st.current().and_then(|s| names::validate(&s.database).ok()),
        main: res.db.role == Role::Main,
    })
}
