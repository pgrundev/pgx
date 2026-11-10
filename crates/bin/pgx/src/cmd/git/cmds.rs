//! The `pgx git` commands.
use super::config::{self, GitConfig};
use super::core::{self, Create, MainSource, Resolution, SyncOpts};
use super::env::{self, Shell};
use super::repo::Repo;
use super::state::{self, BranchDb, RepoState, Role, SnapState};
use super::{GitCmd, InitOpts};
use crate::cli::ExitCode;
use crate::cmd::Ctx;
use crate::errors::{PgxError, Result};
use crate::out::Out;
use serde_json::json;
use std::path::Path;

pub fn run(out: &Out, cmd: GitCmd) -> Result<ExitCode> {
    let cwd = super::cwd()?;
    match cmd {
        GitCmd::Init(o) => init(out, &cwd, o),
        GitCmd::Status => status(out, &cwd),
        GitCmd::Ls => ls(out, &cwd),
        GitCmd::Sync { force, migrate } => sync(out, &cwd, force, migrate),
        GitCmd::Env {
            shell,
            hook,
            reveal,
        } => env_cmd(out, &cwd, shell, hook, reveal),
        GitCmd::Exec { command } => exec(out, &cwd, command),
        GitCmd::Reset { force, .. } => reset(out, &cwd, force),
        GitCmd::Prune { apply, grace } => prune(out, &cwd, apply, grace),
        GitCmd::Doctor => doctor(out, &cwd),
        GitCmd::Uninstall { shell } => uninstall(out, &cwd, shell),
        GitCmd::Shell { shell } => {
            let exe = std::env::current_exe().unwrap_or_else(|_| "pgx".into());
            print!("{}", env::snippet(&shell, &exe)?);
            Ok(ExitCode::Ok)
        }
        GitCmd::InstallShell { shell } => install_shell(out, shell),
        GitCmd::Hook { name, args } => Ok(hook(&name, &args)),
    }
}

/// The snapshot generation a mapping was based on, as `v3`.
fn gen_label(g: Option<u64>) -> String {
    g.map(|g| format!("v{g}"))
        .unwrap_or_else(|| "-".to_string())
}

fn source_label(st: &RepoState, db: &BranchDb) -> String {
    match db.role {
        Role::Main => st.main_branch.clone(),
        _ => format!("{} {}", st.main_branch, gen_label(db.base_generation)),
    }
}

/// Inside a coding-agent session the agent's own database wins: a session
/// never works on the developer's branch database (or another session's).
fn agent_session_active(cfg: &GitConfig) -> bool {
    cfg.agents_enabled
        && [
            "CLAUDE_CODE_SESSION_ID",
            "CODEX_SESSION_ID",
            "PGX_AGENT_SESSION",
        ]
        .iter()
        .any(|v| std::env::var(v).is_ok_and(|s| !s.is_empty()))
}

// ------------------------------------------------------------------ init

fn init(out: &Out, cwd: &Path, o: InitOpts) -> Result<ExitCode> {
    let t0 = std::time::Instant::now();
    let mut repo = core::require_repo(cwd)?;
    core::ensure_identity(&crate::layout::Home::resolve().root, &mut repo)?;
    let mut steps: Vec<(String, String)> = vec![(
        "Git repository detected".into(),
        format!(
            "{}{}",
            repo.toplevel.display(),
            if repo.linked {
                " (linked worktree)"
            } else {
                ""
            }
        ),
    )];
    let ctx = core::runtime(out, true)?;
    steps.push((
        "PGX runtime ready".into(),
        format!("port {}", ctx.config.port),
    ));
    let lock = state::lock(&ctx.home.root, &repo.repo_id, core::LOCK_WAIT)?;
    let (cfg0, file) = core::load_config_file(&repo)?;
    let main_branch = o
        .main
        .clone()
        .or_else(|| cfg0.main_branch.clone())
        .unwrap_or_else(|| repo.guess_main());

    // Where the main database comes from (only when it does not exist yet).
    let agent_source = file
        .as_ref()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|t| crate::cmd::agent::project::parse_pgx_toml_full(&t).ok())
        .and_then(|p| p.source);
    let source = match &o.source {
        Some(s) if s.contains("://") => MainSource::Url {
            url: s.clone(),
            schema_only: !o.with_data,
        },
        Some(s) => MainSource::Pgx(crate::names::validate_user_target(s)?),
        None => match cfg0.source.clone().or(agent_source) {
            Some(s) => MainSource::Pgx(crate::names::validate(&s)?),
            None => MainSource::Empty,
        },
    };
    if o.with_data && !matches!(source, MainSource::Url { .. }) {
        out.note("--with-data applies to imports from a postgres:// URL; a PGX source is cloned as it is");
    }
    if o.schema_only && !matches!(source, MainSource::Url { .. }) {
        out.note("--schema-only applies to imports from a postgres:// URL (the default for them); a PGX source is cloned as it is");
    }

    let mut st = state::load(&ctx.home.root, &repo.repo_id)?.unwrap_or_else(|| RepoState {
        repo_id: repo.repo_id.clone(),
        repo_name: repo.display_name(),
        repo_slug: super::naming::repo_slug(&repo.display_name()),
        common_dir: repo.common_dir.to_string_lossy().into_owned(),
        initialized_at: core::now(),
        ..Default::default()
    });
    st.main_branch = main_branch.clone();
    st.cluster_id = ctx.cluster_id();
    st.common_dir = repo.common_dir.to_string_lossy().into_owned();
    let mut client = ctx.connect("postgres")?;
    for n in core::recover(&ctx, &mut client, &mut st)? {
        out.note(&format!("pgx git: {n}"));
    }
    let (main_db, how) = core::ensure_main(out, &ctx, &mut client, &mut st, &source)?;
    state::save(&ctx.home.root, &st)?;
    steps.push((
        "Main database configured".into(),
        format!("{main_db} ({how}, for {main_branch})"),
    ));

    // .pgx.toml: add the [git] tables, or update the keys we were told to.
    let toml_path = file
        .clone()
        .unwrap_or_else(|| repo.toplevel.join(config::FILE));
    let committed_source = match &source {
        MainSource::Pgx(s) => Some(s.as_str().to_string()),
        _ => None,
    };
    let old = std::fs::read_to_string(&toml_path).unwrap_or_default();
    let new = if !cfg0.present {
        let mut t = old.clone();
        if !t.is_empty() && !t.ends_with('\n') {
            t.push('\n');
        }
        if t.is_empty() {
            t.push_str("# .pgx.toml — PGX configuration for this project (safe to commit).\n");
        }
        t.push_str(&config::render_block(
            &main_branch,
            committed_source.as_deref(),
        ));
        t
    } else {
        let mut t = old.clone();
        if !cfg0.enabled {
            t = config::set_key(&t, "git", "enabled", "true");
        }
        if cfg0.main_branch.as_deref() != Some(main_branch.as_str()) {
            t = config::set_key(&t, "git", "main_branch", &config::lit(&main_branch));
        }
        if let (Some(s), true) = (&committed_source, o.source.is_some()) {
            t = config::set_key(&t, "git", "source", &config::lit(s));
        }
        t
    };
    let toml_written = new != old;
    if toml_written {
        std::fs::write(&toml_path, &new)
            .map_err(|e| PgxError::io("cannot write", &toml_path, &e))?;
    }
    let (cfg, _) = core::load_config(&repo)?;

    // First (or refreshed) snapshot: the explicit path takes its own locks.
    client.close();
    drop(lock);
    let so = core::sync_explicit(out, &ctx, &repo, &cfg, &SyncOpts::default())?;
    if !so.ok() {
        return Err(PgxError::failure(so.message.clone())
            .hint("fix the cause and rerun `pgx git init` (safe to repeat)"));
    }
    let _lock = state::lock(&ctx.home.root, &repo.repo_id, core::LOCK_WAIT)?;
    let mut st = core::require_state(&ctx, &repo)?;
    let mut client = ctx.connect("postgres")?;
    steps.push((
        format!("Main snapshot {} ready", gen_label(so.generation)),
        match st.current() {
            Some(s)
                if so.status == "unchanged" || so.status == "pending" && so.method.is_empty() =>
            {
                format!(
                    "{} (up to date; change detection: {})",
                    s.database,
                    cfg.sync_mode.as_str()
                )
            }
            Some(s) => format!(
                "{} ({} in {:.0} ms; change detection: {})",
                s.database,
                if s.method == "dump" {
                    "logical copy"
                } else {
                    "copy-on-write clone"
                },
                so.elapsed_ms,
                cfg.sync_mode.as_str()
            ),
            None => so.message.clone(),
        },
    ));

    // Hooks.
    let mut hook_reports = Vec::new();
    if o.no_hooks {
        steps.push((
            "Git integration skipped".into(),
            "--no-hooks: run `pgx git exec` / `pgx git env` yourself".into(),
        ));
    } else {
        let (reports, records) = super::hooks::install(&repo)?;
        st.hooks = records;
        let skipped: Vec<_> = reports.iter().filter(|r| r.action == "skipped").collect();
        for r in &skipped {
            out.note(&format!(
                "hook {} not installed: {}",
                r.name,
                r.detail.clone().unwrap_or_default()
            ));
        }
        steps.push((
            "Git integration installed".into(),
            format!(
                "{} ({})",
                repo.hooks_dir().display(),
                reports
                    .iter()
                    .map(|r| format!("{} {}", r.name, r.action))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
        hook_reports = reports;
    }

    // The current branch's database (main's if on main).
    let res = core::resolve_head(
        out,
        &ctx,
        &mut client,
        &repo,
        &mut st,
        &cfg,
        Create::Yes,
        false,
    )?;
    client.close();
    state::save(&ctx.home.root, &st)?;
    steps.push((
        "Automatic database branching enabled".into(),
        format!("{} → {}", res.git_label, res.db.database),
    ));

    let shell_on = env::any_shell_installed();
    let mut text = String::new();
    for (k, v) in &steps {
        text.push_str(&format!("✓ {k}\n    {v}\n"));
    }
    text.push_str("\nPGX Git is ready.\nEvery Git branch gets its own Postgres.\n");
    if !shell_on {
        text.push_str("\nNext:\n  pgx git exec -- <command>      run anything against this branch's database\n  pgx git install-shell          refresh DATABASE_URL in new shell commands automatically\n");
    }
    if toml_written {
        text.push_str(&format!(
            "\n{} updated (commit it to share the setup).\n",
            toml_path.display()
        ));
    }
    out.emit(
        text.trim_end(),
        json!({
            "ready": true,
            "repository": repo.toplevel,
            "repo_id": repo.repo_id,
            "worktree_id": repo.worktree_id,
            "main_branch": main_branch,
            "main_database": main_db,
            "main_database_how": how,
            "snapshot": so.generation,
            "sync": so.status,
            "hooks": hook_reports.iter().map(|r| json!({"name": r.name, "path": r.path, "action": r.action, "detail": r.detail})).collect::<Vec<_>>(),
            "config": toml_path,
            "config_written": toml_written,
            "branch": res.git_label,
            "database": res.db.database,
            "shell_integration": shell_on,
            "elapsed_ms": (t0.elapsed().as_secs_f64() * 10000.0).round() / 10.0,
        }),
    );
    Ok(ExitCode::Ok)
}

// ---------------------------------------------------------------- status

struct Live {
    exists: bool,
    connections: u64,
}

fn live(ctx: &Ctx, name: &str) -> Option<Live> {
    ctx.running_pid()?;
    let mut c = ctx.connect("postgres").ok()?;
    let oid = crate::cmd::db::database_oid(&mut c, name).ok()?;
    let conns = crate::cmd::db::active_connections(&mut c, name).unwrap_or(0);
    c.close();
    Some(Live {
        exists: oid.is_some(),
        connections: conns,
    })
}

/// Agent databases branched from `db` (any session, any host).
fn agent_dbs(ctx: &Ctx, db: &str) -> Vec<crate::ttlstore::TtlRecord> {
    let now = core::now();
    crate::ttlstore::MetaStore::new(&ctx.home.root)
        .list()
        .0
        .into_iter()
        .filter(|r| {
            !r.is_expired(now)
                && r.agent
                    .as_ref()
                    .is_some_and(|m| m.source == db && !m.retired)
        })
        .collect()
}

fn sync_summary(
    ctx: &Ctx,
    repo: &Repo,
    st: &RepoState,
    cfg: &GitConfig,
    db: Option<&BranchDb>,
) -> (String, serde_json::Value) {
    let cur = st.current();
    let mut lines: Vec<String> = Vec::new();
    let mut code = "up_to_date";
    let mut pending: Vec<String> = Vec::new();
    let mut main_changed = None;
    if let Some(ls) = &st.last_sync {
        if ls.status == "failed" {
            lines.push(format!("Last sync failed: {}", ls.message));
            code = "failed";
        }
    }
    if ctx.running_pid().is_some() && st.main.is_some() {
        if let Ok(p) = core::probe_main(ctx, repo, st, cfg) {
            pending = p.pending.clone();
            if let Some(c) = cur {
                let same = p.fp.same_as(
                    &c.schema_fingerprint,
                    &c.migration_fingerprint,
                    c.data_fingerprint.as_deref(),
                );
                main_changed = Some(!same);
                if !same {
                    lines.push(format!(
                        "Main database changed since {} (run `pgx git sync`)",
                        gen_label(c.generation)
                    ));
                    if code == "up_to_date" {
                        code = "main_changed";
                    }
                }
            }
            if let (Some(c), Some(rev)) = (cur, &p.rev) {
                if c.source_git_revision.as_deref().is_some_and(|r| r != rev) {
                    lines.push(format!(
                        "{} moved in Git since {} ({} → {})",
                        st.main_branch,
                        gen_label(c.generation),
                        &c.source_git_revision.as_deref().unwrap_or("")
                            [..7.min(c.source_git_revision.as_deref().unwrap_or("").len())],
                        &rev[..7.min(rev.len())]
                    ));
                }
            }
        }
    }
    if !pending.is_empty() {
        lines.push(format!(
            "Pending: {} migration(s) committed on {} are not applied to the main database ({}{})",
            pending.len(),
            st.main_branch,
            pending
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
            if pending.len() > 3 { ", …" } else { "" }
        ));
        if code == "up_to_date" {
            code = "pending_migrations";
        }
    }
    let stale = match (db, cur) {
        (Some(d), Some(c)) if d.role != Role::Main => {
            d.base_generation.is_some() && d.base_generation < c.generation
        }
        _ => false,
    };
    if stale {
        lines.push(format!(
            "Main has newer changes ({}); this branch is based on {} — `pgx git reset --from-main` gives a fresh copy",
            gen_label(cur.and_then(|c| c.generation)),
            gen_label(db.and_then(|d| d.base_generation))
        ));
        if code == "up_to_date" {
            code = "stale";
        }
    }
    if lines.is_empty() {
        lines.push("Up to date".to_string());
    }
    (
        lines.join("\n      "),
        json!({
            "status": code,
            "main_changed": main_changed,
            "branch_stale": stale,
            "pending_migrations": pending,
            "last_sync": st.last_sync.as_ref().map(|s| s.to_json()),
        }),
    )
}

fn status(out: &Out, cwd: &Path) -> Result<ExitCode> {
    let repo = core::require_repo(cwd)?;
    let (cfg, file) = core::load_config(&repo)?;
    let home = crate::layout::Home::resolve();
    if !cfg.is_on() || !home.is_initialized() {
        let why = if !cfg.present {
            "not initialised (no [git] table in .pgx.toml)"
        } else if !cfg.enabled {
            "disabled ([git] enabled = false)"
        } else {
            "PGX is not initialised on this machine"
        };
        out.emit(
            &format!("PGX Git\nProject: {}\nGit: {}\nStatus: {why}\n  run `pgx git init`", repo.display_name(), repo.head.label()),
            json!({"enabled": false, "reason": why, "project": repo.display_name(), "git_branch": repo.head.branch()}),
        );
        return Ok(ExitCode::Ok);
    }
    let ctx = Ctx::load()?;
    let st = core::require_state(&ctx, &repo)?;
    let running = ctx.running_pid().is_some();
    // Read-only: find the mapping without creating anything.
    let branch = repo
        .head
        .branch()
        .map(String::from)
        .or_else(|| repo.rebasing_branch());
    let db: Option<BranchDb> = match &branch {
        Some(b) if *b == st.main_branch => st.main.clone(),
        Some(b) => st
            .find_branch(&repo.worktree_id, b, Role::Branch)
            .map(|i| st.branches[i].clone()),
        None => st
            .find_branch(&repo.worktree_id, "HEAD", Role::Detached)
            .map(|i| st.branches[i].clone()),
    };
    let lv = db.as_ref().and_then(|d| live(&ctx, &d.database));
    let readiness = match (&db, &lv, running) {
        (None, _, _) => {
            "no database yet (created on the next checkout, `pgx git env` or `pgx git exec`)"
                .to_string()
        }
        (Some(_), _, false) => {
            "runtime stopped (`pgx start`, or any pgx git command starts it)".to_string()
        }
        (Some(_), Some(l), true) if l.exists => format!(
            "Ready ({} connection{})",
            l.connections,
            if l.connections == 1 { "" } else { "s" }
        ),
        (Some(_), _, true) => {
            "missing (dropped outside PGX Git; recreated from main on next use)".to_string()
        }
    };
    let (sync_text, sync_json) = sync_summary(&ctx, &repo, &st, &cfg, db.as_ref());
    let agents = db
        .as_ref()
        .map(|d| agent_dbs(&ctx, &d.database))
        .unwrap_or_default();
    let agent_line = if !cfg.agents_enabled {
        "disabled ([git.agents] enabled = false): agents use the [agent] source".to_string()
    } else {
        format!(
            "every session and subagent gets its own copy of this branch's database ({} active)",
            agents.len()
        )
    };
    let cur = st.current();
    let text = format!(
        "PGX Git\nProject: {}\nGit: {}\nWorktree: {}{}\nDatabase: {}\nMain database: {}\nMain snapshot: {}\nBranch baseline: {}\nStatus: {}\nSync: {}\nAgents: {}",
        st.repo_name,
        repo.head.label(),
        repo.toplevel.display(),
        if repo.linked { " (linked)" } else { "" },
        db.as_ref().map(|d| d.database.clone()).unwrap_or_else(|| "-".into()),
        st.main.as_ref().map(|m| format!("{} ({})", m.database, st.main_branch)).unwrap_or_else(|| "-".into()),
        cur.map(|c| format!("{}{}", gen_label(c.generation), if c.locked { "" } else { " (unlocked)" })).unwrap_or_else(|| "none".into()),
        match &db {
            Some(d) if d.role == Role::Main => "main database".to_string(),
            Some(d) => gen_label(d.base_generation),
            None => "-".to_string(),
        },
        readiness,
        sync_text,
        agent_line,
    );
    out.emit(
        &text,
        json!({
            "enabled": true,
            "project": st.repo_name,
            "repo_id": st.repo_id,
            "git_branch": branch,
            "head": repo.head.label(),
            "worktree": repo.toplevel,
            "worktree_id": repo.worktree_id,
            "linked_worktree": repo.linked,
            "database": db.as_ref().map(|d| d.database.clone()),
            "role": db.as_ref().map(|d| d.role.as_str()),
            "ready": lv.as_ref().is_some_and(|l| l.exists) && running,
            "connections": lv.as_ref().map(|l| l.connections),
            "runtime_running": running,
            "main_branch": st.main_branch,
            "main_database": st.main.as_ref().map(|m| m.database.clone()),
            "main_snapshot": cur.and_then(|c| c.generation),
            "branch_baseline": db.as_ref().and_then(|d| d.base_generation),
            "sync": sync_json,
            "agents": {"enabled": cfg.agents_enabled, "active": agents.iter().map(|r| r.name.clone()).collect::<Vec<_>>()},
            "config": file,
        }),
    );
    Ok(ExitCode::Ok)
}

// -------------------------------------------------------------------- ls

fn ls(out: &Out, cwd: &Path) -> Result<ExitCode> {
    let repo = core::require_repo(cwd)?;
    let ctx = Ctx::load()?;
    let st = core::require_state(&ctx, &repo)?;
    let cur = st.current_generation();
    let live_ids = repo.live_worktree_ids().unwrap_or_default();
    let missing_wts = repo.missing_worktrees();
    let existing: std::collections::HashMap<String, u64> = if ctx.running_pid().is_some() {
        ctx.connect("postgres")
            .ok()
            .and_then(|mut c| {
                let r = c.simple_query("SELECT datname, oid FROM pg_database").ok();
                c.close();
                r
            })
            .map(|r| {
                r.rows
                    .into_iter()
                    .filter_map(|row| Some((row[0].clone()?, row[1].as_deref()?.parse().ok()?)))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Default::default()
    };
    let here_branch = repo.head.branch().map(String::from);
    let mut rows: Vec<(String, String, String, String, String)> = Vec::new();
    let mut items = Vec::new();
    let mut all: Vec<BranchDb> = st.main.iter().cloned().collect();
    all.extend(st.branches.iter().filter(|b| !b.creating).cloned());
    for b in &all {
        let is_current = match b.role {
            Role::Main => here_branch.as_deref() == Some(st.main_branch.as_str()),
            Role::Branch => {
                b.worktree_id == repo.worktree_id
                    && here_branch.as_deref() == Some(b.branch.as_str())
            }
            Role::Detached => b.worktree_id == repo.worktree_id && here_branch.is_none(),
        };
        let wt_alive = b.role == Role::Main
            || live_ids.iter().any(|(id, _)| *id == b.worktree_id)
            || missing_wts
                .iter()
                .any(|p| p.to_string_lossy() == b.worktree_path);
        let branch_alive = b.role != Role::Branch || repo.branch_exists(&b.branch);
        let status = if !existing.is_empty() && !existing.contains_key(&b.database) {
            "missing"
        } else if is_current {
            "current"
        } else if !wt_alive || !branch_alive {
            "abandoned"
        } else if b.role != Role::Main && b.base_generation.is_some() && b.base_generation < cur {
            "stale"
        } else {
            "ready"
        };
        let base = if b.role == Role::Main {
            gen_label(cur)
        } else {
            gen_label(b.base_generation)
        };
        let wt = if b.role == Role::Main {
            "-".to_string()
        } else {
            Path::new(&b.worktree_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let label = if b.role == Role::Detached {
            "(detached)".to_string()
        } else {
            b.branch.clone()
        };
        rows.push((
            label.clone(),
            b.database.clone(),
            base.clone(),
            status.to_string(),
            wt.clone(),
        ));
        items.push(json!({
            "branch": label, "database": b.database, "role": b.role.as_str(), "base_generation": if b.role == Role::Main { cur } else { b.base_generation },
            "status": status, "worktree": b.worktree_path, "worktree_id": b.worktree_id,
            "created_at": crate::fsutil::rfc3339(b.created_at), "last_used": crate::fsutil::rfc3339(b.last_used),
            "source": b.source, "method": b.method, "elapsed_ms": b.elapsed_ms, "renamed_from": b.renamed_from,
        }));
    }
    let wb = rows.iter().map(|r| r.0.len()).max().unwrap_or(6).max(6);
    let wd = rows.iter().map(|r| r.1.len()).max().unwrap_or(8).max(8);
    let mut text = format!(
        "{:<wb$}  {:<wd$}  {:<6}  {:<10}  WORKTREE\n",
        "BRANCH", "DATABASE", "BASE", "STATUS"
    );
    for r in &rows {
        text.push_str(&format!(
            "{:<wb$}  {:<wd$}  {:<6}  {:<10}  {}\n",
            r.0, r.1, r.2, r.3, r.4
        ));
    }
    let snaps: Vec<_> = st
        .snapshots
        .iter()
        .filter(|s| matches!(s.state, SnapState::Ready | SnapState::Retired))
        .collect();
    if !snaps.is_empty() {
        text.push_str("\nMain snapshots: ");
        text.push_str(
            &snaps
                .iter()
                .rev()
                .map(|s| {
                    format!(
                        "{} {}",
                        gen_label(s.generation),
                        if s.generation == cur {
                            "(current)"
                        } else if s.state == SnapState::Retired {
                            "(retired)"
                        } else {
                            "(previous)"
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    out.emit(
        text.trim_end(),
        json!({
            "branches": items,
            "snapshots": st.snapshots.iter().map(|s| s.to_json()).collect::<Vec<_>>(),
            "current_generation": cur,
        }),
    );
    Ok(ExitCode::Ok)
}

// ------------------------------------------------------------------ sync

fn sync(out: &Out, cwd: &Path, force: bool, migrate: bool) -> Result<ExitCode> {
    let repo = core::require_repo(cwd)?;
    let (cfg, _) = core::load_config(&repo)?;
    if !cfg.is_on() {
        return Err(PgxError::new(
            ExitCode::NotReady,
            "PGX Git is not enabled for this repository",
        )
        .hint("run `pgx git init`"));
    }
    let ctx = core::runtime(out, false)?;
    let o = core::sync_explicit(out, &ctx, &repo, &cfg, &SyncOpts { force, migrate })?;
    let st = core::require_state(&ctx, &repo)?;
    let mut text = format!("{} {}", if o.ok() { "✓" } else { "✗" }, o.message);
    if o.status == "published" || o.status == "pending" && o.generation != o.previous {
        text.push_str(&format!(
            "\n  {} in {:.0} ms ({}); new branches start from {}; existing branch databases are unchanged",
            gen_label(o.generation),
            o.elapsed_ms,
            if o.method == "dump" { "logical copy: main was in use" } else { "copy-on-write clone" },
            gen_label(o.generation)
        ));
    }
    if !o.pending.is_empty() {
        text.push_str(&format!(
            "\n  The snapshot does not include them. Apply them to the main database ({}), then run `pgx git sync` again.",
            match &cfg.migrate_command {
                Some(c) => format!("`pgx git sync --migrate` runs `{c}`"),
                None => format!("check out {} and run your migrations with `pgx git exec -- <migrate command>`", st.main_branch),
            }
        ));
    }
    out.emit(
        &text,
        json!({
            "status": o.status, "generation": o.generation, "previous": o.previous, "message": o.message,
            "pending_migrations": o.pending, "method": o.method, "elapsed_ms": (o.elapsed_ms * 10.0).round() / 10.0,
            "migrated": o.migrated, "main_database": st.main.as_ref().map(|m| m.database.clone()),
        }),
    );
    Ok(if o.ok() {
        ExitCode::Ok
    } else {
        ExitCode::Failure
    })
}

// ------------------------------------------------------------- env / exec

fn pairs(ctx: &Ctx, st: &RepoState, res: &Resolution, repo: &Repo) -> Vec<(&'static str, String)> {
    env::pairs_for(
        ctx,
        &res.db,
        &res.git_label,
        &repo.toplevel,
        &source_label(st, &res.db),
    )
}

/// The agent session's environment (its own branch of the current Git
/// branch's database) for `env`/`exec` run inside an agent session.
fn agent_pairs(cwd: &Path) -> Result<Vec<(&'static str, String)>> {
    use crate::cmd::agent::session;
    let r = session::resolve(&session::ResolveInput {
        cwd: Some(cwd.to_path_buf()),
        ..Default::default()
    })?;
    let res = session::up(&Out::new(false).quiet(), &r)?;
    let mut v = session::env_pairs(&res);
    if let Some(g) = &r.git {
        v.push(("PGX_GIT_BRANCH", g.label.clone()));
        v.push(("PGX_WORKTREE", g.worktree.clone()));
    }
    Ok(v)
}

fn print_hook_feedback(res: &Resolution, st: &RepoState) {
    for n in &res.notes {
        eprintln!("pgx git: {n}");
    }
    if let Some(s) = &res.sync {
        if matches!(s.status, "published" | "failed" | "skipped" | "pending") {
            eprintln!("pgx git: {}", s.message);
        }
    }
    if res.created {
        eprintln!(
            "✓ PGX database ready: {}\n  Cloned from {} ({}) in {:.0} ms{}\n  Database: {}",
            res.git_label,
            st.main_branch,
            gen_label(res.db.base_generation),
            res.db.elapsed_ms,
            if res.recreated {
                " (the previous one was gone)"
            } else {
                ""
            },
            res.db.database
        );
    }
}

fn env_cmd(
    out: &Out,
    cwd: &Path,
    shell: Option<String>,
    hook: bool,
    reveal: bool,
) -> Result<ExitCode> {
    let sh = shell.as_deref().map(Shell::parse).transpose()?;
    if hook {
        // The prompt integration: always print something `eval` can run.
        let sh = sh.unwrap();
        let enabled = super::repo::discover(cwd)
            .ok()
            .flatten()
            .and_then(|r| core::load_config(&r).ok().map(|(c, _)| (r, c)))
            .filter(|(_, c)| c.is_on());
        let Some((_, cfg)) = enabled else {
            print!("{}", env::restore_lines(sh));
            return Ok(ExitCode::Ok);
        };
        let r = if agent_session_active(&cfg) {
            agent_pairs(cwd)
        } else {
            core::resolve_here(&out.quiet(), cwd, Create::Yes, core::HOOK_LOCK_WAIT).map(
                |(ctx, repo, _cfg, st, res)| {
                    print_hook_feedback(&res, &st);
                    pairs(&ctx, &st, &res, &repo)
                },
            )
        };
        match r {
            Ok(p) => print!("{}", env::lines(sh, &p, true)),
            Err(e) => {
                eprintln!(
                    "pgx git: isolation failed, DATABASE_URL points at an unreachable sentinel: {}",
                    e.message
                );
                print!("{}", env::lines(sh, &env::sentinel(&e.message), true));
            }
        }
        return Ok(ExitCode::Ok);
    }
    let repo = core::require_repo(cwd)?;
    let (cfg, _) = core::load_config(&repo)?;
    let (p, label, db) = if cfg.is_on() && agent_session_active(&cfg) {
        let p = agent_pairs(cwd)?;
        let db = p
            .iter()
            .find(|(k, _)| *k == "PGX_BRANCH")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        (p, repo.head.label(), db)
    } else {
        let (ctx, repo, _cfg, st, res) =
            core::resolve_here(out, cwd, Create::Yes, core::LOCK_WAIT)?;
        print_hook_feedback(&res, &st);
        (
            pairs(&ctx, &st, &res, &repo),
            res.git_label.clone(),
            res.db.database.clone(),
        )
    };
    if let Some(sh) = sh {
        print!("{}", env::lines(sh, &p, false));
        return Ok(ExitCode::Ok);
    }
    let masked = |k: &str, v: &str| -> String {
        if k == "PGPASSWORD" {
            "********".to_string()
        } else if k == "DATABASE_URL" {
            match (v.find("://"), v.rfind('@')) {
                (Some(a), Some(b)) if b > a => {
                    let user_end = v[a + 3..b].find(':').map(|i| a + 3 + i).unwrap_or(b);
                    format!("{}:********{}", &v[..user_end], &v[b..])
                }
                _ => v.to_string(),
            }
        } else {
            v.to_string()
        }
    };
    let mut text = format!("PGX Git database for {label}: {db}\n");
    for (k, v) in &p {
        text.push_str(&format!("  {k}={}\n", masked(k, v)));
    }
    text.push_str("\nThis shell is not changed. Use one of:\n  eval \"$(pgx git env --shell zsh)\"   (bash, zsh, fish)\n  pgx git exec -- <command>\n  pgx git install-shell               (refresh automatically at every prompt)");
    let mut m = serde_json::Map::new();
    for (k, v) in &p {
        // Redacted by default, for tools as for humans; `--reveal` is the
        // explicit opt-in (`--shell` output is for `eval` and always complete).
        let v = if reveal { v.clone() } else { masked(k, v) };
        m.insert((*k).to_string(), json!(v));
    }
    out.emit(
        &text,
        json!({"branch": label, "database": db, "env": m, "redacted": !reveal}),
    );
    Ok(ExitCode::Ok)
}

fn exec(out: &Out, cwd: &Path, command: Vec<String>) -> Result<ExitCode> {
    let repo = core::require_repo(cwd)?;
    let (cfg, _) = core::load_config(&repo)?;
    if cfg.is_on() && agent_session_active(&cfg) {
        // Inside an agent session: the session's own branch, never the developer's.
        return crate::cmd::agent::exec::run(
            out,
            crate::cmd::agent::ExecOpts {
                cwd: Some(cwd.to_path_buf()),
                command,
                ..Default::default()
            },
        );
    }
    if let Some(script) = super::guard::shell_script(&command) {
        if let Some(reason) = super::guard::check(script, cwd) {
            return Err(PgxError::usage(format!(
                "PGX refused the command: {reason}"
            )));
        }
    }
    // Fail closed: no database, no command (never the inherited DATABASE_URL).
    let (ctx, repo, _cfg, st, res) =
        core::resolve_here(&out.quiet(), cwd, Create::Yes, core::LOCK_WAIT)?;
    print_hook_feedback(&res, &st);
    let mut c = std::process::Command::new(&command[0]);
    c.args(&command[1..]);
    for (k, v) in pairs(&ctx, &st, &res, &repo) {
        c.env(k, v);
    }
    for k in ["PGSERVICE", "PGSERVICEFILE", "PGX_ISOLATION_REASON"] {
        c.env_remove(k);
    }
    // Replace this process: the command's exit status and signals are its own.
    use std::os::unix::process::CommandExt;
    let e = c.exec();
    Err(PgxError::new(
        ExitCode::NotFound,
        format!("cannot run {}: {e}", command[0]),
    ))
}

// ----------------------------------------------------------------- reset

fn confirm(question: &str) -> bool {
    if unsafe { libc::isatty(0) } != 1 {
        return false;
    }
    eprint!("{question} [y/N] ");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).is_ok() && matches!(line.trim(), "y" | "Y" | "yes")
}

fn reset(out: &Out, cwd: &Path, force: bool) -> Result<ExitCode> {
    let repo = core::require_repo(cwd)?;
    let (cfg, _) = core::load_config(&repo)?;
    if !cfg.is_on() {
        return Err(PgxError::new(
            ExitCode::NotReady,
            "PGX Git is not enabled for this repository",
        ));
    }
    let ctx = core::runtime(out, false)?;
    let _lock = state::lock(&ctx.home.root, &repo.repo_id, core::LOCK_WAIT)?;
    let mut st = core::require_state(&ctx, &repo)?;
    let mut client = ctx.connect("postgres")?;
    let res = core::resolve_head(
        out,
        &ctx,
        &mut client,
        &repo,
        &mut st,
        &cfg,
        Create::Yes,
        false,
    )?;
    if res.db.role == Role::Main || cfg.is_protected(&res.db.branch) {
        return Err(PgxError::usage(format!(
            "{} is protected: `pgx git reset` never resets the main database or a protected branch",
            res.git_label
        )));
    }
    // Reset onto the latest main state.
    let so = core::sync_auto(out, &ctx, &mut client, &repo, &mut st, &cfg)?;
    if so.status == "failed" || so.status == "skipped" {
        out.note(&format!("pgx git: {}", so.message));
    }
    let snap = st.current().cloned().ok_or_else(|| {
        PgxError::new(
            ExitCode::NotReady,
            "no validated main snapshot to reset from",
        )
        .hint("run `pgx git sync`")
    })?;
    let conns = crate::cmd::db::active_connections(&mut client, &res.db.database)?;
    if !force {
        let q = format!(
            "Recreate {} ({}) from main {}? Every change in this database is lost{}.",
            res.db.database,
            res.git_label,
            gen_label(snap.generation),
            if conns > 0 {
                format!(" and {conns} connection(s) are terminated")
            } else {
                String::new()
            }
        );
        if !confirm(&q) {
            return Err(PgxError::usage(format!(
                "refusing to reset {} without confirmation: rerun with --force",
                res.db.database
            )));
        }
    }
    let target = crate::names::validate_user_target(&res.db.database)?;
    let token = format!("{:x}", core::now());
    let fresh = crate::names::validate_user_target(&super::naming::staging_db(
        &st.repo_slug,
        &format!("r{token}"),
    ))?;
    let old = crate::names::validate_user_target(&super::naming::staging_db(
        &st.repo_slug,
        &format!("o{token}"),
    ))?;
    let snap_db = crate::names::validate(&snap.database)?;
    // Intent first: an interrupted reset is finished or undone by recovery
    // (`core::recover`), never leaked.
    st.reset = Some(state::ResetIntent {
        target: target.as_str().to_string(),
        old: old.as_str().to_string(),
        old_oid: res.db.oid,
        fresh: fresh.as_str().to_string(),
        fresh_oid: None,
    });
    state::save(&ctx.home.root, &st)?;
    let steps = (|| -> Result<crate::cmd::db::Cloned> {
        // 1. The fresh copy, while the original is still in place.
        let c = crate::cmd::db::clone_database(&ctx, &mut client, &snap_db, &fresh, None)?;
        if let Some(r) = st.reset.as_mut() {
            r.fresh_oid = crate::cmd::db::database_oid(&mut client, fresh.as_str())?;
        }
        state::save(&ctx.home.root, &st)?;
        // 2. Move the original aside (its sessions end: confirmed above).
        let _ = client.simple_query(&format!(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = {} AND pid <> pg_backend_pid()",
            crate::pgwire::quote_literal(target.as_str())
        ));
        let mut moved = Err(PgxError::failure("not attempted"));
        for _ in 0..40 {
            moved = client
                .simple_query(&format!(
                    "ALTER DATABASE {} RENAME TO {}",
                    target.quoted(),
                    old.quoted()
                ))
                .map(|_| ())
                .map_err(|e| crate::cmd::db_error(e, &format!("cannot move {target} aside")));
            if moved.is_ok() {
                break;
            }
            let _ = client.simple_query(&format!(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = {} AND pid <> pg_backend_pid()",
                crate::pgwire::quote_literal(target.as_str())
            ));
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        moved?;
        core::failpoint("reset")?;
        // 3. The fresh copy takes the branch's name; 4. the original goes.
        client
            .simple_query(&format!(
                "ALTER DATABASE {} RENAME TO {}",
                fresh.quoted(),
                target.quoted()
            ))
            .map_err(|e| crate::cmd::db_error(e, "cannot put the fresh copy in place"))?;
        let _ = client.simple_query(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            old.quoted()
        ));
        Ok(c)
    })();
    let c = match steps {
        Ok(c) => c,
        Err(e) => {
            // Undo what was done; the branch keeps its original database.
            if let Some(r) = st.reset.take() {
                let _ = core::recover_reset(&mut client, &r);
            }
            state::save(&ctx.home.root, &st)?;
            return Err(e);
        }
    };
    st.reset = None;
    let oid = crate::cmd::db::database_oid(&mut client, target.as_str())?.unwrap_or(0);
    client.close();
    if let Some(i) = st
        .branches
        .iter()
        .position(|b| b.database == res.db.database)
    {
        let b = &mut st.branches[i];
        b.oid = oid;
        b.base_generation = snap.generation;
        b.source = snap.database.clone();
        b.created_at = core::now();
        b.last_used = core::now();
        b.method = c.method.to_string();
        b.elapsed_ms = c.elapsed_ms;
    }
    state::save(&ctx.home.root, &st)?;
    out.emit(
        &format!(
            "✓ {} reset from main {} in {:.0} ms\n  Database: {} (agent databases branched from it are unchanged)",
            res.git_label,
            gen_label(snap.generation),
            c.elapsed_ms,
            target
        ),
        json!({"branch": res.git_label, "database": target.as_str(), "generation": snap.generation, "elapsed_ms": c.elapsed_ms, "terminated_connections": conns}),
    );
    Ok(ExitCode::Ok)
}

// ----------------------------------------------------------------- prune

fn prune(out: &Out, cwd: &Path, apply: bool, grace: Option<String>) -> Result<ExitCode> {
    let repo = core::require_repo(cwd)?;
    let (cfg, _) = core::load_config(&repo)?;
    let ctx = core::runtime(out, false)?;
    let _lock = state::lock(&ctx.home.root, &repo.repo_id, core::LOCK_WAIT)?;
    let mut st = core::require_state(&ctx, &repo)?;
    let mut client = ctx.connect("postgres")?;
    for n in core::recover(&ctx, &mut client, &mut st)? {
        out.note(&format!("pgx git: {n}"));
    }
    let grace_secs = match &grace {
        Some(g) => crate::ttl::parse_ttl(g)?.as_seconds().unwrap_or(u64::MAX),
        None => cfg.abandoned_secs(),
    };
    let now = core::now();
    // Deletion decisions need Git's real answer: refuse rather than guess.
    // Worktrees are matched by identity (a moved worktree keeps its id); a
    // listed worktree that is missing from disk right now is never "removed".
    let live_ids = repo
        .live_worktree_ids()
        .ok_or_else(|| PgxError::failure("`git worktree list` failed; nothing was pruned"))?;
    let missing_wts = repo.missing_worktrees();
    let agent_names: std::collections::HashSet<String> =
        crate::ttlstore::MetaStore::new(&ctx.home.root)
            .list()
            .0
            .into_iter()
            .filter(|r| r.agent.is_some())
            .map(|r| r.name)
            .collect();
    let mut actions: Vec<serde_json::Value> = Vec::new();
    let mut text = String::new();
    let mut keep = Vec::new();
    for b in std::mem::take(&mut st.branches) {
        let wt_listed = live_ids.iter().any(|(id, _)| *id == b.worktree_id)
            || missing_wts
                .iter()
                .any(|p| p.to_string_lossy() == b.worktree_path);
        let reason = match b.role {
            Role::Branch if repo.branch_known(&b.branch).is_none() => None,
            Role::Branch if repo.branch_known(&b.branch) == Some(false) => {
                Some(format!("branch {} was deleted", b.branch))
            }
            _ if !wt_listed => Some(format!("worktree {} was removed", b.worktree_path)),
            Role::Detached => live_ids
                .iter()
                .find(|(id, _)| *id == b.worktree_id)
                .and_then(|(_, br)| br.as_ref())
                .map(|_| "the worktree is back on a branch".to_string()),
            _ => None,
        };
        let Some(reason) = reason else {
            keep.push(b);
            continue;
        };
        let idle = now.saturating_sub(b.last_used);
        let skip = if cfg.is_protected(&b.branch) {
            Some("protected branch".to_string())
        } else if agent_names.contains(&b.database) {
            Some("an agent database".to_string())
        } else if idle < grace_secs {
            Some(format!(
                "within the grace period ({} left)",
                crate::ttl::humanize_long(grace_secs - idle)
            ))
        } else {
            match crate::cmd::db::database_oid(&mut client, &b.database)? {
                None => None,
                Some(oid) if oid != b.oid => {
                    Some("recreated outside PGX Git (OID changed)".to_string())
                }
                Some(_) => {
                    let c = crate::cmd::db::active_connections(&mut client, &b.database)?;
                    (c > 0).then(|| format!("{c} active connection(s)"))
                }
            }
        };
        let label = if b.role == Role::Detached {
            "(detached)".to_string()
        } else {
            b.branch.clone()
        };
        if let Some(s) = skip {
            text.push_str(&format!(
                "  keep    {:<40} {label}: {reason}; kept: {s}\n",
                b.database
            ));
            actions.push(json!({"database": b.database, "branch": label, "action": "keep", "reason": reason, "kept_because": s}));
            keep.push(b);
            continue;
        }
        let exists = crate::cmd::db::database_oid(&mut client, &b.database)?.is_some();
        if apply {
            if exists {
                let n = crate::names::validate_user_target(&b.database)?;
                client
                    .simple_query(&format!("DROP DATABASE {}", n.quoted()))
                    .map_err(|e| crate::cmd::db_error(e, &format!("cannot drop {n}")))?;
            }
            text.push_str(&format!(
                "  {} {:<40} {label}: {reason}\n",
                if exists { "dropped" } else { "forgot " },
                b.database
            ));
        } else {
            text.push_str(&format!(
                "  {} {:<40} {label}: {reason}\n",
                if exists { "drop   " } else { "forget " },
                b.database
            ));
            keep.push(b.clone());
        }
        actions.push(json!({"database": b.database, "branch": label, "action": if exists { "drop" } else { "forget" }, "reason": reason, "applied": apply}));
    }
    st.branches = keep;
    // Retired snapshots (never the current or the previous one).
    let mut snaps = Vec::new();
    for s in std::mem::take(&mut st.snapshots) {
        if s.state == SnapState::Retired {
            if apply {
                let n = crate::names::validate_user_target(&s.database)?;
                let _ = client.simple_query(&format!("DROP DATABASE IF EXISTS {}", n.quoted()));
                text.push_str(&format!(
                    "  dropped {:<40} retired main snapshot {}\n",
                    s.database,
                    gen_label(s.generation)
                ));
            } else {
                text.push_str(&format!(
                    "  drop    {:<40} retired main snapshot {}\n",
                    s.database,
                    gen_label(s.generation)
                ));
                snaps.push(s.clone());
            }
            actions.push(json!({"database": s.database, "snapshot": s.generation, "action": "drop", "reason": "retired snapshot", "applied": apply}));
        } else {
            snaps.push(s);
        }
    }
    st.snapshots = snaps;
    client.close();
    if apply {
        state::save(&ctx.home.root, &st)?;
    }
    let head = if actions.is_empty() {
        "Nothing to prune.".to_string()
    } else if apply {
        "Pruned:".to_string()
    } else {
        "Would prune (dry run; `pgx git prune --apply` does it):".to_string()
    };
    out.emit(
        format!("{head}\n{text}").trim_end(),
        json!({"applied": apply, "grace_seconds": grace_secs, "actions": actions}),
    );
    Ok(ExitCode::Ok)
}

// ---------------------------------------------------------------- doctor

fn doctor(out: &Out, cwd: &Path) -> Result<ExitCode> {
    let mut checks: Vec<(&'static str, &'static str, String)> = Vec::new();
    let mut add = |name: &'static str, level: &'static str, detail: String| {
        checks.push((name, level, detail))
    };
    if !super::repo::git_available() {
        add("git", "fail", "git is not on PATH".into());
        return finish_doctor(out, checks);
    }
    let Some(repo) = super::repo::discover(cwd)? else {
        add(
            "repository",
            "fail",
            format!("{} is not inside a Git repository", cwd.display()),
        );
        return finish_doctor(out, checks);
    };
    add(
        "repository",
        "ok",
        format!(
            "{} ({}; common dir {})",
            repo.toplevel.display(),
            if repo.linked {
                "linked worktree, .git file"
            } else {
                "main worktree"
            },
            repo.common_dir.display()
        ),
    );
    let cfg = match core::load_config(&repo) {
        Ok((c, f)) => {
            match (&f, c.is_on()) {
                (None, _) => add("config", "fail", "no .pgx.toml (run `pgx git init`)".into()),
                (Some(f), false) => add(
                    "config",
                    "fail",
                    format!("{}: [git] missing or enabled = false", f.display()),
                ),
                (Some(f), true) => add(
                    "config",
                    "ok",
                    format!(
                        "{} (main {}, sync {}, auto_branch {}, agents {})",
                        f.display(),
                        c.main(),
                        c.sync_mode.as_str(),
                        c.auto_branch,
                        c.agents_enabled
                    ),
                ),
            }
            c
        }
        Err(e) => {
            add("config", "fail", e.message);
            GitConfig::default()
        }
    };
    let home = crate::layout::Home::resolve();
    if !home.is_initialized() {
        add(
            "runtime",
            "fail",
            format!("PGX is not initialised at {}", home.root.display()),
        );
        return finish_doctor(out, checks);
    }
    let ctx = Ctx::load()?;
    let running = ctx.running_pid().is_some();
    add(
        "runtime",
        if running { "ok" } else { "warn" },
        if running {
            format!("running on port {}", ctx.config.port)
        } else {
            "stopped (pgx git commands start it)".into()
        },
    );
    let st = match state::load(&ctx.home.root, &repo.repo_id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            add(
                "state",
                "fail",
                "no PGX Git state for this repository (run `pgx git init`)".into(),
            );
            return finish_doctor(out, checks);
        }
        Err(e) => {
            add("state", "fail", e.message);
            return finish_doctor(out, checks);
        }
    };
    add(
        "state",
        "ok",
        state::dir(&ctx.home.root, &repo.repo_id)
            .join("state.json")
            .display()
            .to_string(),
    );
    if st.cluster_id != ctx.cluster_id() {
        add("cluster", "warn", "the PGX cluster was re-initialised since `pgx git init`; databases are recreated on use".into());
    }
    // Hooks.
    if let Some(hp) = repo.hooks_path_config() {
        add("hooks path", "ok", format!("core.hooksPath = {hp}"));
    }
    for (name, path, ok) in super::hooks::status(&repo) {
        add(
            "hook",
            if ok { "ok" } else { "warn" },
            format!(
                "{name}: {}{}",
                path.display(),
                if ok {
                    ""
                } else {
                    " (no PGX block; `pgx git init` installs it)"
                }
            ),
        );
    }
    let sh = env::default_shell();
    add(
        "shell",
        if env::shell_installed(&sh) {
            "ok"
        } else {
            "warn"
        },
        if env::shell_installed(&sh) {
            format!(
                "{sh} integration installed ({})",
                env::rc_file(&sh)
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            )
        } else {
            format!("no {sh} integration: use `pgx git exec -- <cmd>`, or `pgx git install-shell`")
        },
    );
    let mut client = if running {
        ctx.connect("postgres").ok()
    } else {
        None
    };
    let oid_of = |c: &mut Option<core::Client>, n: &str| -> Option<Option<u64>> {
        c.as_mut()
            .and_then(|c| crate::cmd::db::database_oid(c, n).ok())
    };
    match &st.main {
        None => add("main database", "fail", "none recorded".into()),
        Some(m) => match oid_of(&mut client, &m.database) {
            Some(Some(_)) => add("main database", "ok", format!("{} ({})", m.database, st.main_branch)),
            Some(None) => add("main database", "fail", format!("{} is missing (recreated empty on next use; `pgx git init --source …` re-imports)", m.database)),
            None => add("main database", "warn", format!("{} (not checked: runtime stopped)", m.database)),
        },
    }
    match st.current() {
        None => add(
            "snapshot",
            "fail",
            "no validated main snapshot (run `pgx git sync`)".into(),
        ),
        Some(s) => match oid_of(&mut client, &s.database) {
            Some(Some(oid)) if oid == s.oid => add(
                "snapshot",
                "ok",
                format!(
                    "{} {} ({}{})",
                    gen_label(s.generation),
                    s.database,
                    s.method,
                    if s.locked {
                        ", locked read-only"
                    } else {
                        ", NOT locked"
                    }
                ),
            ),
            Some(Some(_)) => add(
                "snapshot",
                "fail",
                format!("{} was replaced outside PGX Git (OID changed)", s.database),
            ),
            Some(None) => add(
                "snapshot",
                "fail",
                format!("{} is missing; run `pgx git sync --force`", s.database),
            ),
            None => add(
                "snapshot",
                "warn",
                format!("{} {} (not checked)", gen_label(s.generation), s.database),
            ),
        },
    }
    let failed = st
        .snapshots
        .iter()
        .filter(|s| s.state == SnapState::Failed)
        .count();
    if let Some(ls) = &st.last_sync {
        add(
            "sync",
            if ls.status == "failed" { "warn" } else { "ok" },
            format!(
                "last {} at {}: {}",
                ls.status,
                crate::fsutil::rfc3339(ls.at),
                ls.message
            ),
        );
    }
    if failed > 0 {
        add(
            "sync history",
            "ok",
            format!("{failed} failed attempt(s) recorded (their staging copies were removed)"),
        );
    }
    if running {
        if let Ok(p) = core::probe_main(&ctx, &repo, &st, &cfg) {
            if let Some(c) = st.current() {
                let same = p.fp.same_as(
                    &c.schema_fingerprint,
                    &c.migration_fingerprint,
                    c.data_fingerprint.as_deref(),
                );
                add(
                    "schema",
                    if same { "ok" } else { "warn" },
                    if same {
                        format!("main matches {}", gen_label(c.generation))
                    } else {
                        "main changed since the snapshot (`pgx git sync`)".into()
                    },
                );
            }
            if !p.pending.is_empty() {
                add(
                    "migrations",
                    "warn",
                    format!(
                        "{} committed on {} but not applied to the main database",
                        p.pending.len(),
                        st.main_branch
                    ),
                );
            } else if !p.fp.frameworks.is_empty() {
                add(
                    "migrations",
                    "ok",
                    format!("bookkeeping found: {}", p.fp.frameworks.join(", ")),
                );
            }
        }
    }
    // Mappings.
    let mut seen = std::collections::HashMap::new();
    let mut broken = 0;
    for b in &st.branches {
        if let Some(prev) = seen.insert(b.database.clone(), b.branch.clone()) {
            add(
                "mapping",
                "fail",
                format!(
                    "collision: {} is mapped to both {prev} and {}",
                    b.database, b.branch
                ),
            );
        }
        if let Some(None) = oid_of(&mut client, &b.database) {
            broken += 1;
        }
        if b.creating {
            add(
                "mapping",
                "warn",
                format!(
                    "{} is still being created (pid {})",
                    b.database, b.owner_pid
                ),
            );
        }
    }
    add(
        "mappings",
        if broken == 0 { "ok" } else { "warn" },
        format!(
            "{} branch database(s){}",
            st.branches.len(),
            if broken > 0 {
                format!(", {broken} missing (recreated on next use)")
            } else {
                String::new()
            }
        ),
    );
    if let Some(c) = client.as_mut() {
        let prefix = format!("pgx_{}_", st.repo_slug);
        if let Ok(r) = c.simple_query(&format!(
            "SELECT datname FROM pg_database WHERE starts_with(datname, {})",
            crate::pgwire::quote_literal(&prefix)
        )) {
            let unmapped: Vec<String> = r
                .rows
                .into_iter()
                .filter_map(|row| row[0].clone())
                .filter(|n| !st.owns_database(n))
                .collect();
            if !unmapped.is_empty() {
                add(
                    "unmapped",
                    "warn",
                    format!(
                        "databases with this repository's prefix but no mapping (left alone): {}",
                        unmapped.join(", ")
                    ),
                );
            }
        }
    }
    if let Some(c) = client.take() {
        c.close();
    }
    add(
        "agents",
        "ok",
        if cfg.agents_enabled {
            "agent sessions branch from the current Git branch's database (one per session and subagent)".to_string()
        } else {
            "[git.agents] enabled = false: agent sessions keep using the [agent] source".to_string()
        },
    );
    add(
        "live copies",
        if crate::pgtools::locate().is_ok() {
            "ok"
        } else {
            "warn"
        },
        if crate::pgtools::locate().is_ok() {
            "pg_dump/pg_restore found: a busy database can still be snapshotted or branched".into()
        } else {
            "pg_dump/pg_restore not found: snapshots and agent branches need an idle source database".into()
        },
    );
    finish_doctor(out, checks)
}

fn finish_doctor(out: &Out, checks: Vec<(&'static str, &'static str, String)>) -> Result<ExitCode> {
    let fail = checks.iter().any(|c| c.1 == "fail");
    let mut text = String::from("PGX Git doctor\n");
    for (n, l, d) in &checks {
        let mark = match *l {
            "ok" => "✓",
            "warn" => "!",
            _ => "✗",
        };
        text.push_str(&format!("  {mark} {n:<13} {d}\n"));
    }
    out.emit(
        text.trim_end(),
        json!({"ok": !fail, "checks": checks.iter().map(|(n, l, d)| json!({"check": n, "level": l, "detail": d})).collect::<Vec<_>>()}),
    );
    Ok(if fail {
        ExitCode::Failure
    } else {
        ExitCode::Ok
    })
}

// ------------------------------------------------------ uninstall / shell

fn uninstall(out: &Out, cwd: &Path, shell: bool) -> Result<ExitCode> {
    let repo = core::require_repo(cwd)?;
    let reports = super::hooks::uninstall(&repo)?;
    let home = crate::layout::Home::resolve();
    if home.is_initialized() {
        if let Ok(_l) = state::lock(&home.root, &repo.repo_id, core::LOCK_WAIT) {
            if let Ok(Some(mut st)) = state::load(&home.root, &repo.repo_id) {
                st.hooks.clear();
                state::save(&home.root, &st)?;
            }
        }
    }
    let mut shells = Vec::new();
    if shell {
        for sh in ["zsh", "bash", "fish"] {
            let p = env::rc_file(sh)?;
            if let Ok(t) = std::fs::read_to_string(&p) {
                if let Some(rest) = env::strip_block(&t, env::RC_BEGIN, env::RC_END) {
                    std::fs::write(&p, rest).map_err(|e| PgxError::io("cannot write", &p, &e))?;
                    shells.push(p.display().to_string());
                }
            }
        }
    }
    let mut text = String::from("✓ PGX Git integration removed\n");
    for r in &reports {
        text.push_str(&format!("  {} {}\n", r.action, r.path.display()));
    }
    if reports.is_empty() {
        text.push_str("  no PGX hooks were installed\n");
    }
    for s in &shells {
        text.push_str(&format!("  shell integration removed from {s}\n"));
    }
    text.push_str("  Databases are kept (`pgx git ls`); the [git] table in .pgx.toml is kept (it is shared with your team — set enabled = false there to turn PGX Git off for everyone).");
    if !shell && env::any_shell_installed() {
        text.push_str("\n  Shell integration is still installed (shared by every repository): `pgx git uninstall --shell` removes it.");
    }
    out.emit(
        &text,
        json!({"hooks": reports.iter().map(|r| json!({"name": r.name, "path": r.path, "action": r.action})).collect::<Vec<_>>(), "shell_files": shells, "databases_kept": true}),
    );
    Ok(ExitCode::Ok)
}

fn install_shell(out: &Out, shell: Option<String>) -> Result<ExitCode> {
    let sh = shell.unwrap_or_else(env::default_shell);
    Shell::parse(&sh)?;
    let p = env::rc_file(&sh)?;
    let old = std::fs::read_to_string(&p).unwrap_or_default();
    let block = env::rc_block(&sh);
    let base = env::strip_block(&old, env::RC_BEGIN, env::RC_END).unwrap_or_else(|| old.clone());
    let mut new = base.clone();
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new.push_str(&block);
    let changed = new != old;
    if changed {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| PgxError::io("cannot create", d, &e))?;
        }
        std::fs::write(&p, &new).map_err(|e| PgxError::io("cannot write", &p, &e))?;
    }
    out.emit(
        &format!(
            "{} {sh} integration {} {}\n  Open a new shell (or `source {}`). From then on, every prompt refreshes DATABASE_URL and PG* for the current Git branch.\n  Running servers keep their old environment: restart them after switching branches.",
            "✓",
            if changed { "installed in" } else { "already in" },
            p.display(),
            p.display()
        ),
        json!({"shell": sh, "file": p, "changed": changed}),
    );
    Ok(ExitCode::Ok)
}

// ------------------------------------------------------------------ hook

/// The Git hook entry point. Never fails Git: every problem becomes a
/// one-line notice and exit 0. Does nothing in repositories without PGX Git.
fn hook(name: &str, args: &[String]) -> ExitCode {
    if std::env::var("PGX_GIT_SKIP").is_ok_and(|v| !v.is_empty() && v != "0") {
        return ExitCode::Ok;
    }
    let Ok(cwd) = super::cwd() else {
        return ExitCode::Ok;
    };
    let Ok(Some(repo)) = super::repo::discover(&cwd) else {
        return ExitCode::Ok;
    };
    let cfg = match core::load_config(&repo) {
        Ok((c, _)) => c,
        Err(e) => {
            eprintln!("pgx git: {} (hook skipped)", e.message);
            return ExitCode::Ok;
        }
    };
    if !cfg.is_on() {
        return ExitCode::Ok;
    }
    match name {
        "post-checkout" => {
            // args: <previous HEAD> <new HEAD> <1 = branch checkout, 0 = file checkout>
            if args.get(2).map(String::as_str) != Some("1") {
                return ExitCode::Ok;
            }
        }
        "post-merge" | "post-rewrite" => {}
        _ => return ExitCode::Ok,
    }
    let home = crate::layout::Home::resolve();
    if !home.is_initialized() {
        eprintln!("pgx git: PGX is not initialised on this machine; skipped (run `pgx git init`)");
        return ExitCode::Ok;
    }
    let out = Out::new(false);
    let on_main = repo.head.branch() == Some(cfg.main());
    let result = (|| -> Result<()> {
        if name == "post-merge" && on_main {
            // `git pull` on main: the code moved; check the database.
            let ctx = core::runtime(&out.quiet(), false)?;
            let _lock = state::lock(&ctx.home.root, &repo.repo_id, core::HOOK_LOCK_WAIT)?;
            let mut st = core::require_state(&ctx, &repo)?;
            let mut client = ctx.connect("postgres")?;
            let o = if cfg.auto_sync {
                core::sync_auto(&out.quiet(), &ctx, &mut client, &repo, &mut st, &cfg)?
            } else {
                client.close();
                return Ok(());
            };
            client.close();
            match o.status {
                "published" => eprintln!("✓ PGX: {}", o.message),
                "failed" | "pending" | "skipped" => eprintln!("! PGX: {}", o.message),
                _ => {}
            }
            if !o.pending.is_empty() {
                eprintln!(
                    "  Run your migrations on {} (`pgx git exec -- <migrate>`{}), then `pgx git sync`.",
                    cfg.main(),
                    if cfg.migrate_command.is_some() { " or `pgx git sync --migrate`" } else { "" }
                );
            }
            return Ok(());
        }
        if !cfg.auto_branch && repo.head.branch() != Some(cfg.main()) {
            return Ok(());
        }
        let (_ctx, _repo, _cfg, st, res) = core::resolve_here(
            &out.quiet(),
            &repo.toplevel,
            Create::Yes,
            core::HOOK_LOCK_WAIT,
        )?;
        print_hook_feedback(&res, &st);
        if !res.created && name == "post-checkout" {
            let cur = st.current_generation();
            let stale = res.db.role != Role::Main
                && res.db.base_generation.is_some()
                && res.db.base_generation < cur;
            eprintln!(
                "✓ PGX database: {} → {}{}",
                res.git_label,
                res.db.database,
                if stale {
                    format!(
                        " (based on {}; main is at {})",
                        gen_label(res.db.base_generation),
                        gen_label(cur)
                    )
                } else {
                    String::new()
                }
            );
            if let Some(old) = &res.renamed_from {
                eprintln!("  (carried over from the renamed branch {old})");
            }
        }
        if name == "post-checkout" {
            if env::any_shell_installed() {
                eprintln!("  New commands in shells with `pgx git shell` integration use it; restart running servers.");
            } else {
                eprintln!("  This shell's environment is unchanged: run commands with `pgx git exec -- <command>`.");
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!(
            "pgx git: could not prepare the database for {}: {}{}",
            repo.head.label(),
            e.message,
            e.hint.map(|h| format!(" ({h})")).unwrap_or_default()
        );
    }
    ExitCode::Ok
}
