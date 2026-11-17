//! `pgboxdb migrate`: adopt an installation made before the rename to PGBoxDB.
//!
//! A dry run unless `--apply`. Nothing is ever deleted or overwritten:
//! * the data home is moved with one `rename(2)` on the same filesystem (no
//!   copy, so no half-copied state), only while its runtime is stopped and only
//!   when the target does not exist (an empty directory counts as absent);
//!   a record of the move (`migrated-from.json`) makes `--rollback` possible;
//! * the project file and the Git identity files are copied to their new
//!   names with exclusive creation; the originals stay;
//! * hook and shell entries are reported with the command that replaces them
//!   (those commands already treat the old entries as their own).
//!
//! `pgboxdb migrate --rollback [--apply]` moves the home back while the
//! runtime is stopped and the old location is still free.
use crate::cli::ExitCode;
use crate::cmd::agent::hosts::Host;
use crate::cmd::agent::install;
use crate::errors::{PgboxdbError, Result};
use crate::layout::Home;
use crate::legacy;
use crate::out::Out;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Opts {
    pub apply: bool,
    pub rollback: bool,
}

/// Written into the new home after a move: where it came from.
pub const RECORD: &str = "migrated-from.json";
/// The record's name once the home has been moved back.
pub const ROLLED_BACK: &str = "migration-rolled-back.json";
/// config.json as it was before paths inside it were rewritten.
pub const CONFIG_BACKUP: &str = "config.json.pre-migrate";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// performed in this run (--apply)
    Done,
    /// would be performed by --apply
    Planned,
    /// nothing to do
    Skipped,
    /// cannot be performed safely; nothing was changed for this step
    Blocked,
    /// needs a command the user runs
    Manual,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            State::Done => "done",
            State::Planned => "planned",
            State::Skipped => "skipped",
            State::Blocked => "blocked",
            State::Manual => "manual",
        }
    }
    fn mark(self) -> &'static str {
        match self {
            State::Done => "✓",
            State::Planned => "→",
            State::Skipped => "·",
            State::Blocked => "✗",
            State::Manual => "!",
        }
    }
}

struct Step {
    area: &'static str,
    state: State,
    detail: String,
    hint: Option<String>,
}

fn step(area: &'static str, state: State, detail: impl Into<String>) -> Step {
    Step {
        area,
        state,
        detail: detail.into(),
        hint: None,
    }
}

impl Step {
    fn hint(mut self, h: impl Into<String>) -> Step {
        self.hint = Some(h.into());
        self
    }
}

pub fn run(out: &Out, opts: &Opts) -> Result<ExitCode> {
    let new = Home::resolve();
    let mut steps = Vec::new();
    if opts.rollback {
        steps.push(rollback_home(&new, opts.apply)?);
    } else {
        let home = home_step(&new, opts.apply)?;
        // A blocked home step leaves everything as it was: the other steps
        // are only planned, never half-applied.
        let apply = opts.apply && home.state != State::Blocked;
        steps.push(home);
        let cwd = std::env::current_dir().map_err(|e| {
            PgboxdbError::failure(format!("cannot read the current directory: {e}"))
        })?;
        project_steps(&cwd, apply, &mut steps)?;
        hook_steps(&cwd, &mut steps);
    }
    report(out, opts, &steps);
    Ok(if steps.iter().any(|s| s.state == State::Blocked) {
        ExitCode::Conflict
    } else {
        ExitCode::Ok
    })
}

fn engine_pid(home: &Home) -> Option<u32> {
    let pf = crate::process::read_pid_file(&home.data)?;
    crate::process::pid_alive(pf.pid).then_some(pf.pid)
}

fn same_path(a: &Path, b: &Path) -> bool {
    let c = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    c(a) == c(b)
}

/// Absent, or an empty directory (which `rename(2)` replaces atomically).
fn free(p: &Path) -> bool {
    match std::fs::symlink_metadata(p) {
        Err(_) => true,
        Ok(m) if m.is_dir() => std::fs::read_dir(p).is_ok_and(|mut d| d.next().is_none()),
        Ok(_) => false,
    }
}

fn write_new(path: &Path, text: &str) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()
}

fn rename_error(from: &Path, to: &Path, e: &std::io::Error) -> String {
    if e.raw_os_error() == Some(libc::EXDEV) {
        format!(
            "{} and {} are on different filesystems, so the move cannot be one atomic rename; nothing was changed",
            from.display(),
            to.display()
        )
    } else {
        format!(
            "cannot move {} to {}: {e}; nothing was changed",
            from.display(),
            to.display()
        )
    }
}

fn home_step(new: &Home, apply: bool) -> Result<Step> {
    const A: &str = "home";
    let record = new.root.join(RECORD);
    let Some(old) = legacy::home() else {
        return Ok(step(
            A,
            State::Skipped,
            "HOME is not set; no previous data home to look for",
        ));
    };
    if same_path(&old.root, &new.root) {
        return Ok(step(
            A,
            State::Skipped,
            format!(
                "PGBOXDB_HOME points at {}: it is used in place, nothing to move",
                new.root.display()
            ),
        ));
    }
    if !old.is_initialized() {
        if let Some(from) = read_record(&record).and_then(|r| r["from"].as_str().map(String::from))
        {
            return Ok(step(
                A,
                State::Skipped,
                format!("already migrated: {} came from {from}", new.root.display()),
            ));
        }
        let what = if old.root.exists() {
            format!(
                "{} holds no initialised cluster; left as it is",
                old.root.display()
            )
        } else {
            format!(
                "no data home from before the rename at {}",
                old.root.display()
            )
        };
        return Ok(step(A, State::Skipped, what));
    }
    if !free(&new.root) {
        return Ok(step(
            A,
            State::Blocked,
            format!(
                "{} already exists, so {} was not moved there",
                new.root.display(),
                old.root.display()
            ),
        )
        .hint(format!(
            "keep using the existing databases in place with PGBOXDB_HOME={}, or move {} aside (nothing is ever merged)",
            old.root.display(),
            new.root.display()
        )));
    }
    if let Some(pid) = engine_pid(&old) {
        return Ok(step(
            A,
            State::Blocked,
            format!(
                "the runtime using {} is running (pid {pid})",
                old.root.display()
            ),
        )
        .hint(format!(
            "stop it first with the CLI that started it (or `kill -INT {pid}`), then run this again"
        )));
    }
    let what = format!(
        "move {} to {} (one rename; nothing is copied or deleted)",
        old.root.display(),
        new.root.display()
    );
    if !apply {
        return Ok(step(A, State::Planned, what));
    }
    if let Some(parent) = new.root.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| PgboxdbError::io("cannot create", parent, &e))?;
    }
    if let Err(e) = std::fs::rename(&old.root, &new.root) {
        return Ok(
            step(A, State::Blocked, rename_error(&old.root, &new.root, &e)).hint(format!(
                "use the databases in place with PGBOXDB_HOME={}",
                old.root.display()
            )),
        );
    }
    let rec = json!({
        "from": old.root.to_string_lossy(),
        "to": new.root.to_string_lossy(),
        "at": crate::fsutil::now_rfc3339(),
        "by": format!("pgboxdb {}", env!("CARGO_PKG_VERSION")),
    });
    write_new(&record, &format!("{rec:#}\n"))
        .map_err(|e| PgboxdbError::io("cannot write", &record, &e))?;
    let rewritten = rewrite_config_paths(&new.config_file, &old.root, &new.root)?;
    let mut detail = format!("moved {} to {}", old.root.display(), new.root.display());
    if rewritten {
        detail.push_str(&format!(
            "; paths inside config.json updated (previous copy: {CONFIG_BACKUP})"
        ));
    }
    Ok(step(A, State::Done, detail).hint(format!(
        "`pgboxdb start` uses it now; `pgboxdb migrate --rollback --apply` moves it back to {}",
        old.root.display()
    )))
}

fn read_record(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// config.json may name a directory inside the home (a run_dir set below it).
/// Rewrite such paths, keeping the original as `config.json.pre-migrate`.
fn rewrite_config_paths(config: &Path, from: &Path, to: &Path) -> Result<bool> {
    let Ok(text) = std::fs::read_to_string(config) else {
        return Ok(false);
    };
    let Ok(mut v) = serde_json::from_str::<Value>(&text) else {
        return Ok(false);
    };
    let mut changed = false;
    if let Some(obj) = v.as_object_mut() {
        for (_, val) in obj.iter_mut() {
            if let Some(s) = val.as_str() {
                if let Ok(rest) = Path::new(s).strip_prefix(from) {
                    *val = json!(to.join(rest).to_string_lossy());
                    changed = true;
                }
            }
        }
    }
    if !changed {
        return Ok(false);
    }
    let backup = config.with_file_name(CONFIG_BACKUP);
    write_new(&backup, &text).map_err(|e| PgboxdbError::io("cannot write", &backup, &e))?;
    let tmp = config.with_extension("json.migrate-tmp");
    std::fs::write(&tmp, format!("{v:#}\n"))
        .map_err(|e| PgboxdbError::io("cannot write", &tmp, &e))?;
    std::fs::rename(&tmp, config).map_err(|e| PgboxdbError::io("cannot replace", config, &e))?;
    Ok(true)
}

fn rollback_home(new: &Home, apply: bool) -> Result<Step> {
    const A: &str = "rollback";
    let record = new.root.join(RECORD);
    let Some(rec) = read_record(&record) else {
        return Ok(step(
            A,
            State::Skipped,
            format!(
                "nothing to roll back: {} was not created by `pgboxdb migrate`",
                new.root.display()
            ),
        ));
    };
    let Some(from) = rec["from"].as_str().map(PathBuf::from) else {
        return Ok(step(
            A,
            State::Blocked,
            format!("{} names no origin", record.display()),
        ));
    };
    if !free(&from) {
        return Ok(step(
            A,
            State::Blocked,
            format!(
                "{} exists again, so {} was not moved back",
                from.display(),
                new.root.display()
            ),
        )
        .hint("move it aside first (nothing is ever merged)"));
    }
    if let Some(pid) = engine_pid(new) {
        return Ok(step(
            A,
            State::Blocked,
            format!(
                "the runtime using {} is running (pid {pid})",
                new.root.display()
            ),
        )
        .hint("`pgboxdb stop` first"));
    }
    let what = format!("move {} back to {}", new.root.display(), from.display());
    if !apply {
        return Ok(step(A, State::Planned, what));
    }
    if let Err(e) = std::fs::rename(&new.root, &from) {
        return Ok(step(A, State::Blocked, rename_error(&new.root, &from, &e)));
    }
    // Only after the move succeeded: put back config.json as it was.
    let backup = from.join(CONFIG_BACKUP);
    if backup.is_file() {
        let cfg = from.join("config.json");
        std::fs::rename(&backup, &cfg).map_err(|e| PgboxdbError::io("cannot restore", &cfg, &e))?;
    }
    let _ = std::fs::rename(from.join(RECORD), from.join(ROLLED_BACK));
    Ok(step(A, State::Done, format!("moved {} back to {}", new.root.display(), from.display())).hint(
        "the CLI from before the rename can use it again; files copied into projects stay (they are harmless)",
    ))
}

/// The project root as `pgboxdb agent` resolves it: the first directory up
/// from `cwd` holding a project file (either name) or a `.git`.
fn project_root(cwd: &Path) -> Option<PathBuf> {
    let mut cur = Some(cwd);
    while let Some(d) = cur {
        if d.join(crate::cmd::agent::project::CONFIG_FILE).is_file()
            || d.join(legacy::PROJECT_FILE).is_file()
            || d.join(".git").exists()
        {
            return Some(d.to_path_buf());
        }
        cur = d.parent();
    }
    None
}

/// Comments in the old project file name the old CLI; values are copied as they are.
/// Both whole-line comments and trailing comments after a value (as `agent
/// setup` writes them) are converted; a `#` inside a quoted value is not a comment.
fn convert_project_file(text: &str) -> String {
    text.lines()
        .map(|l| match comment_start(l) {
            Some(i) => format!("{}{}", &l[..i], convert_comment(&l[i..])),
            None => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
        + if text.ends_with('\n') { "\n" } else { "" }
}

fn convert_comment(c: &str) -> String {
    c.replace(
        legacy::PROJECT_FILE,
        crate::cmd::agent::project::CONFIG_FILE,
    )
    .replace("`pgx ", "`pgboxdb ")
    .replace("docs/pgx/", "docs/pgboxdb/")
    .replace("PGX", "PGBoxDB")
}

/// Byte offset of the `#` that starts a TOML comment on this line, if any:
/// the first `#` outside a basic ("…", with \ escapes) or literal ('…') string.
fn comment_start(line: &str) -> Option<usize> {
    let (mut basic, mut literal, mut escaped) = (false, false, false);
    for (i, ch) in line.char_indices() {
        if basic {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => basic = false,
                _ => {}
            }
        } else if literal {
            if ch == '\'' {
                literal = false;
            }
        } else {
            match ch {
                '#' => return Some(i),
                '"' => basic = true,
                '\'' => literal = true,
                _ => {}
            }
        }
    }
    None
}

fn project_steps(cwd: &Path, apply: bool, steps: &mut Vec<Step>) -> Result<()> {
    const P: &str = "project";
    let new_name = crate::cmd::agent::project::CONFIG_FILE;
    match project_root(cwd) {
        None => steps.push(step(
            P,
            State::Skipped,
            format!("{} is not inside a project", cwd.display()),
        )),
        Some(root) => {
            let old = root.join(legacy::PROJECT_FILE);
            let new = root.join(new_name);
            if old.is_file() && new.is_file() {
                steps.push(
                    step(
                        P,
                        State::Manual,
                        format!("{} is no longer read ({} is)", old.display(), new.display()),
                    )
                    .hint(format!(
                        "delete {} once everyone working on the project uses PGBoxDB",
                        legacy::PROJECT_FILE
                    )),
                );
            } else if old.is_file() {
                let what = format!("copy {} to {}", old.display(), new.display());
                if apply {
                    let text = std::fs::read_to_string(&old)
                        .map_err(|e| PgboxdbError::io("cannot read", &old, &e))?;
                    write_new(&new, &convert_project_file(&text))
                        .map_err(|e| PgboxdbError::io("cannot write", &new, &e))?;
                    steps.push(step(P, State::Done, format!("copied {} to {}", old.display(), new.display())).hint(format!(
                        "commit {new_name}; delete {} once everyone working on the project uses PGBoxDB",
                        legacy::PROJECT_FILE
                    )));
                } else {
                    steps.push(step(P, State::Planned, what));
                }
            } else {
                steps.push(step(
                    P,
                    State::Skipped,
                    format!("no {} in {}", legacy::PROJECT_FILE, root.display()),
                ));
            }
        }
    }
    let Ok(Some(repo)) = crate::cmd::git::repo::discover(cwd) else {
        return Ok(());
    };
    use crate::cmd::git::repo::{REPO_ID_FILE, WORKTREE_ID_FILE};
    for (dir, new_name, old_name) in [
        (&repo.common_dir, REPO_ID_FILE, legacy::REPO_ID_FILE),
        (&repo.git_dir, WORKTREE_ID_FILE, legacy::WORKTREE_ID_FILE),
    ] {
        let old = dir.join(old_name);
        let new = dir.join(new_name);
        let Some(id) = crate::cmd::git::repo::read_id(&old) else {
            continue;
        };
        if new.exists() {
            steps.push(step(
                "git",
                State::Skipped,
                format!("{} already exists", new.display()),
            ));
            continue;
        }
        if apply {
            write_new(&new, &format!("{id}\n"))
                .map_err(|e| PgboxdbError::io("cannot write", &new, &e))?;
            steps.push(step(
                "git",
                State::Done,
                format!("copied {} to {}", old.display(), new.display()),
            ));
        } else {
            steps.push(step(
                "git",
                State::Planned,
                format!(
                    "copy {} to {} (the repository keeps its databases)",
                    old.display(),
                    new.display()
                ),
            ));
        }
    }
    let legacy_hooks = crate::cmd::git::hooks::legacy_blocks(&repo);
    if !legacy_hooks.is_empty() {
        let names: Vec<String> = legacy_hooks
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        steps.push(
            step(
                "git",
                State::Manual,
                format!("hooks from before the rename in {}", names.join(", ")),
            )
            .hint("after the home is migrated, run `pgboxdb git init` in this repository: it replaces them"),
        );
    }
    Ok(())
}

fn hook_steps(cwd: &Path, steps: &mut Vec<Step>) {
    let root = project_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    for host in [Host::Claude, Host::Cursor, Host::Codex] {
        for (scope, path) in install::host_files(host, &root) {
            let has = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .is_some_and(|d| install::has_legacy(&d));
            if has {
                let scope = match scope {
                    crate::cmd::agent::Scope::Project => "project",
                    crate::cmd::agent::Scope::Local => "local",
                    crate::cmd::agent::Scope::User => "user",
                };
                steps.push(
                    step(
                        "agent",
                        State::Manual,
                        format!(
                            "{} has {} hooks from before the rename",
                            path.display(),
                            host.as_str()
                        ),
                    )
                    .hint(format!(
                        "run `pgboxdb agent install {} --scope {scope}`: it replaces them",
                        host.as_str()
                    )),
                );
            }
        }
    }
    for p in crate::cmd::git::env::legacy_rc_files() {
        let sh = if p.to_string_lossy().contains("fish") {
            "fish"
        } else if p.to_string_lossy().ends_with(".bashrc") {
            "bash"
        } else {
            "zsh"
        };
        steps.push(
            step(
                "shell",
                State::Manual,
                format!(
                    "{} has shell integration from before the rename",
                    p.display()
                ),
            )
            .hint(format!(
                "run `pgboxdb git install-shell {sh}`: it replaces it"
            )),
        );
    }
}

fn report(out: &Out, opts: &Opts, steps: &[Step]) {
    let mut text = String::new();
    for s in steps {
        text.push_str(&format!("{} {:<8} {}\n", s.state.mark(), s.area, s.detail));
        if let Some(h) = &s.hint {
            text.push_str(&format!("           {h}\n"));
        }
    }
    let planned = steps.iter().any(|s| s.state == State::Planned);
    if !opts.apply && planned {
        text.push_str(&format!(
            "Dry run: nothing was changed. `pgboxdb migrate{} --apply` performs the planned steps.",
            if opts.rollback { " --rollback" } else { "" }
        ));
    } else if steps.iter().all(|s| s.state == State::Skipped) {
        text.push_str("Nothing to migrate.");
    }
    out.emit(
        text.trim_end(),
        json!({
            "apply": opts.apply,
            "rollback": opts.rollback,
            "steps": steps.iter().map(|s| json!({
                "area": s.area,
                "state": s.state.as_str(),
                "detail": s.detail,
                "hint": s.hint,
            })).collect::<Vec<_>>(),
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_file_comments_are_converted_and_values_kept() {
        let old = "# Written by `pgx agent setup`; see docs/pgx/local-dev/agents.md.\n[agent]\nsource = \"pgx_app\"\n";
        let new = convert_project_file(old);
        assert_eq!(
            new,
            "# Written by `pgboxdb agent setup`; see docs/pgboxdb/local-dev/agents.md.\n[agent]\nsource = \"pgx_app\"\n"
        );
    }

    #[test]
    fn trailing_comments_are_converted_and_quoted_hashes_kept() {
        // What `pgx agent setup` wrote (PGRun's own .pgx.toml has this shape).
        let old = "[agent]\nsource = \"pgrun_template\"        # the PGX database every agent session branches from\nidle_ttl = \"2h\"\n";
        assert_eq!(
            convert_project_file(old),
            "[agent]\nsource = \"pgrun_template\"        # the PGBoxDB database every agent session branches from\nidle_ttl = \"2h\"\n"
        );
        // A `#` (or "PGX") inside a value is data, not a comment.
        let old = "source = \"PGX#1\" # PGX\nname = 'a#PGX' # see `pgx ls`\nesc = \"q\\\"#PGX\"\n";
        assert_eq!(
            convert_project_file(old),
            "source = \"PGX#1\" # PGBoxDB\nname = 'a#PGX' # see `pgboxdb ls`\nesc = \"q\\\"#PGX\"\n"
        );
    }

    #[test]
    fn free_means_absent_or_empty_dir() {
        let d = std::env::temp_dir().join(format!("pgboxdb-migrate-free-{}", std::process::id()));
        assert!(free(&d));
        std::fs::create_dir_all(&d).unwrap();
        assert!(free(&d));
        std::fs::write(d.join("x"), "1").unwrap();
        assert!(!free(&d));
        std::fs::remove_file(d.join("x")).unwrap();
        std::fs::remove_dir(&d).unwrap();
    }
}
