//! The project a session runs in, and its committed `.pgboxdb.toml`.
//!
//! PGBoxDB is one runtime per user, so database names are global across projects.
//! The branch source therefore has to be a per-project fact; `.pgboxdb.toml` in
//! the project root carries it, and the project root's canonical path gives
//! every project a stable id that keeps identical session ids apart.
use super::identity::{fnv1a64, hex16};
use crate::errors::{PgboxdbError, Result};
use std::path::{Path, PathBuf};

pub const CONFIG_FILE: &str = ".pgboxdb.toml";
pub const DEFAULT_SOURCE: &str = "app";
pub const DEFAULT_IDLE_TTL: &str = "2h";
pub const DEFAULT_GRACE: &str = "10m";

/// Server-side guards set on every agent branch (`ALTER DATABASE … SET`), so
/// a client that dies mid-transaction (a killed agent) cannot leave a backend
/// running to its COMMIT: the backend notices the dead client within
/// `connection_check_interval`, an idle transaction is aborted after
/// `idle_in_transaction_timeout`, and no single statement runs longer than
/// `statement_timeout`. `0`/`unlimited` disables one.
pub const DEFAULT_CONNECTION_CHECK: &str = "5s";
pub const DEFAULT_IDLE_IN_TRANSACTION: &str = "15m";
pub const DEFAULT_STATEMENT_TIMEOUT: &str = "1h";

/// The `[agent]` guard settings a project may override.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Guards {
    pub connection_check_interval: Option<String>,
    pub idle_in_transaction_timeout: Option<String>,
    pub statement_timeout: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConfig {
    /// Canonical project root.
    pub root: PathBuf,
    /// 16 hex digits of FNV-1a 64 over the canonical root path.
    pub project_id: String,
    pub source: Option<String>,
    pub idle_ttl: Option<String>,
    pub grace_period: Option<String>,
    pub guards: Guards,
    /// The `.pgboxdb.toml` that was read, if any.
    pub config_file: Option<PathBuf>,
}

fn is_repo_root(dir: &Path) -> bool {
    // A `.git` directory, or the `.git` file a worktree carries.
    dir.join(".git").exists()
}

/// Walk up from `dir` to the first directory that holds `.pgboxdb.toml` or is a
/// repository root, whichever comes first. One walk, so a stray `.pgboxdb.toml`
/// above a repository (say in `$HOME`) can never claim the repository's
/// projects. Returns `(root, config_file)`.
fn find_root(dir: &Path) -> Option<(PathBuf, Option<PathBuf>)> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        let f = d.join(CONFIG_FILE);
        if f.is_file() {
            return Some((d.to_path_buf(), Some(f)));
        }
        if is_repo_root(d) {
            return Some((d.to_path_buf(), None));
        }
        cur = d.parent();
    }
    None
}

/// Resolve the project for a session. `start` is where the session runs (the
/// hook's `cwd`); `fallbacks` are the host's idea of the project root
/// (`CLAUDE_PROJECT_DIR`, `CURSOR_PROJECT_DIR`, `workspace_roots[0]`), tried
/// when `start` yields neither a `.pgboxdb.toml` nor a repository. With nothing
/// found anywhere, the project is `start` itself.
pub fn locate(start: &Path, fallbacks: &[PathBuf]) -> Result<ProjectConfig> {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let start = canon(start);
    let found = find_root(&start).or_else(|| {
        fallbacks
            .iter()
            .filter(|f| f.is_dir())
            .find_map(|f| find_root(&canon(f)))
    });
    let (root, config_file) = found.unwrap_or((start.clone(), None));
    let root = canon(&root);
    let parsed = match &config_file {
        Some(f) => {
            let text =
                std::fs::read_to_string(f).map_err(|e| PgboxdbError::io("cannot read", f, &e))?;
            parse_pgboxdb_toml_full(&text)
                .map_err(|e| PgboxdbError::usage(format!("{}: {}", f.display(), e.message)))?
        }
        None => Parsed::default(),
    };
    Ok(ProjectConfig {
        project_id: hex16(fnv1a64(root.to_string_lossy().as_bytes())),
        root,
        source: parsed.source,
        idle_ttl: parsed.idle_ttl,
        grace_period: parsed.grace_period,
        guards: parsed.guards,
        config_file: config_file.map(|f| canon(&f)),
    })
}

/// Everything `.pgboxdb.toml`'s `[agent]` table may carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    pub source: Option<String>,
    pub idle_ttl: Option<String>,
    pub grace_period: Option<String>,
    pub guards: Guards,
}

/// The `[agent]` table of `.pgboxdb.toml`: `source`, `idle_ttl`, `grace_period`
/// (the guard keys are returned by [`parse_pgboxdb_toml_full`]).
#[cfg(test)]
pub fn parse_pgboxdb_toml(text: &str) -> Result<(Option<String>, Option<String>, Option<String>)> {
    let p = parse_pgboxdb_toml_full(text)?;
    Ok((p.source, p.idle_ttl, p.grace_period))
}

/// The `[agent]` table of `.pgboxdb.toml`: `source`, `idle_ttl`, `grace_period`,
/// `connection_check_interval`, `idle_in_transaction_timeout`,
/// `statement_timeout`. A deliberately tiny TOML subset: `[section]` headers,
/// `key = "value"` or `key = 'value'` lines, `#` comments. Unknown keys and
/// other sections are ignored. Values are validated here so a hostile or
/// mistaken file cannot smuggle a connection URL or an invalid name into a
/// command.
pub fn parse_pgboxdb_toml_full(text: &str) -> Result<Parsed> {
    let mut in_agent = false;
    let mut source = None;
    let mut idle_ttl = None;
    let mut grace = None;
    let mut guards = Guards::default();
    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_agent = section.trim() == "agent";
            continue;
        }
        if !in_agent {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        let Some(v) = unquote(v.trim()) else {
            continue;
        };
        if v.contains("://") {
            return Err(PgboxdbError::usage(format!(
                "{CONFIG_FILE}: {k} must be a PGBoxDB database name, not a connection URL (connection URLs do not belong in {CONFIG_FILE})"
            )));
        }
        match k {
            "source" => {
                crate::names::validate(&v).map_err(|e| {
                    PgboxdbError::usage(format!("{CONFIG_FILE}: source: {}", e.message))
                })?;
                source = Some(v);
            }
            "idle_ttl" | "grace_period" => {
                match crate::ttl::parse_ttl(&v) {
                    Ok(crate::ttl::Ttl::Seconds(_)) => {}
                    Ok(crate::ttl::Ttl::Unlimited) => {
                        return Err(PgboxdbError::usage(format!(
                            "{CONFIG_FILE}: {k} must be a finite duration such as 2h; agent branches always expire"
                        )))
                    }
                    Err(e) => {
                        return Err(PgboxdbError::usage(format!("{CONFIG_FILE}: {k}: {}", e.message)))
                    }
                }
                if k == "idle_ttl" {
                    idle_ttl = Some(v);
                } else {
                    grace = Some(v);
                }
            }
            "connection_check_interval" | "idle_in_transaction_timeout" | "statement_timeout" => {
                // A duration, or 0/unlimited to switch the guard off.
                if v.trim() != "0" {
                    crate::ttl::parse_ttl(&v).map_err(|e| {
                        PgboxdbError::usage(format!("{CONFIG_FILE}: {k}: {}", e.message))
                    })?;
                }
                match k {
                    "connection_check_interval" => guards.connection_check_interval = Some(v),
                    "idle_in_transaction_timeout" => guards.idle_in_transaction_timeout = Some(v),
                    _ => guards.statement_timeout = Some(v),
                }
            }
            _ => {}
        }
    }
    Ok(Parsed {
        source,
        idle_ttl,
        grace_period: grace,
        guards,
    })
}

/// A guard value as seconds for `ALTER DATABASE … SET`: `None` when the
/// guard is off (`0`/`unlimited`).
pub fn guard_seconds(value: Option<&str>, default: &str) -> Option<u64> {
    let v = value.unwrap_or(default).trim();
    if v == "0" {
        return None;
    }
    crate::ttl::parse_ttl(v).ok().and_then(|t| t.as_seconds())
}

fn strip_comment(line: &str) -> &str {
    // A `#` outside quotes starts a comment.
    let mut quote: Option<char> = None;
    for (i, c) in line.char_indices() {
        match (quote, c) {
            (None, '#') => return &line[..i],
            (None, '"') | (None, '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            _ => {}
        }
    }
    line
}

fn unquote(v: &str) -> Option<String> {
    let inner = v
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))?;
    Some(inner.to_string())
}

/// The file `pgboxdb agent setup` writes. No secrets, no URLs: names and durations only.
pub fn render_pgboxdb_toml(source: &str, idle_ttl: &str, grace: &str) -> String {
    format!(
        "# .pgboxdb.toml — PGBoxDB agent configuration for this project (safe to commit).\n\
# Written by `pgboxdb agent setup`; see docs/pgboxdb/local-dev/agents.md.\n\
[agent]\n\
source = \"{source}\"        # the PGBoxDB database every agent session branches from\n\
idle_ttl = \"{idle_ttl}\"           # a branch expires this long after the agent's last activity\n\
grace_period = \"{grace}\"      # how long a branch survives after its session ends\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pgboxdb-agent-project-{tag}-{}-{}",
            std::process::id(),
            crate::fsutil::now_epoch()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn toml_subset_reads_the_agent_table() {
        let (s, t, g) = parse_pgboxdb_toml(
            "# PGBoxDB agent config\n[agent]\nsource = \"myapp_template\"\nidle_ttl = \"3h\"\n",
        )
        .unwrap();
        assert_eq!(s.as_deref(), Some("myapp_template"));
        assert_eq!(t.as_deref(), Some("3h"));
        assert!(g.is_none());
        // Keys outside [agent] and unknown keys are ignored.
        let (s, _, g) = parse_pgboxdb_toml(
            "[other]\nsource = \"nope\"\n[agent]\nextra = 1\ngrace_period = \"5m\"\n",
        )
        .unwrap();
        assert!(s.is_none());
        assert_eq!(g.as_deref(), Some("5m"));
        // Single quotes and inline comments.
        let (s, _, _) = parse_pgboxdb_toml("[agent]\nsource = 'tpl' # the template\n").unwrap();
        assert_eq!(s.as_deref(), Some("tpl"));
    }

    #[test]
    fn toml_rejects_connection_urls_and_bad_names() {
        assert!(parse_pgboxdb_toml("[agent]\nsource = \"postgres://u:p@h/db\"\n").is_err());
        assert!(parse_pgboxdb_toml("[agent]\nsource = \"bad name\"\n").is_err());
        assert!(parse_pgboxdb_toml("[agent]\nidle_ttl = \"soon\"\n").is_err());
        assert!(parse_pgboxdb_toml("[agent]\nidle_ttl = \"unlimited\"\n").is_err());
    }

    #[test]
    fn toml_guards_are_optional_validated_and_switchable_off() {
        let p = parse_pgboxdb_toml_full("[agent]\nsource = \"tpl\"\n").unwrap();
        assert_eq!(p.guards, Guards::default());
        let p = parse_pgboxdb_toml_full(
            "[agent]\nconnection_check_interval = \"10s\"\nidle_in_transaction_timeout = \"0\"\nstatement_timeout = \"unlimited\"\n",
        )
        .unwrap();
        assert_eq!(p.guards.connection_check_interval.as_deref(), Some("10s"));
        assert_eq!(
            guard_seconds(
                p.guards.connection_check_interval.as_deref(),
                DEFAULT_CONNECTION_CHECK
            ),
            Some(10)
        );
        assert_eq!(
            guard_seconds(p.guards.idle_in_transaction_timeout.as_deref(), "15m"),
            None
        );
        assert_eq!(
            guard_seconds(p.guards.statement_timeout.as_deref(), "1h"),
            None
        );
        assert_eq!(guard_seconds(None, DEFAULT_STATEMENT_TIMEOUT), Some(3600));
        assert!(parse_pgboxdb_toml_full("[agent]\nstatement_timeout = \"soon\"\n").is_err());
    }

    #[test]
    fn render_roundtrips() {
        let text = render_pgboxdb_toml("myapp_template", "2h", "10m");
        assert!(text.starts_with("# .pgboxdb.toml"));
        let (s, t, g) = parse_pgboxdb_toml(&text).unwrap();
        assert_eq!(s.as_deref(), Some("myapp_template"));
        assert_eq!(t.as_deref(), Some("2h"));
        assert_eq!(g.as_deref(), Some("10m"));
        assert!(!text.contains("://"));
    }

    #[test]
    fn locate_prefers_pgboxdb_toml_then_git_then_start() {
        let root = tmp("locate");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("sub/deep")).unwrap();
        // No .pgboxdb.toml anywhere: the git top level wins.
        let p = locate(&root.join("sub/deep"), &[]).unwrap();
        assert_eq!(p.root, std::fs::canonicalize(&root).unwrap());
        assert!(p.config_file.is_none());
        assert!(p.source.is_none());
        assert_eq!(p.project_id.len(), 16);
        // A .pgboxdb.toml below the git root wins over it.
        std::fs::write(root.join("sub/.pgboxdb.toml"), "[agent]\nsource = \"tpl\"\n").unwrap();
        let p = locate(&root.join("sub/deep"), &[]).unwrap();
        assert_eq!(p.root, std::fs::canonicalize(root.join("sub")).unwrap());
        assert_eq!(p.source.as_deref(), Some("tpl"));
        assert_eq!(
            p.config_file,
            Some(
                std::fs::canonicalize(root.join("sub"))
                    .unwrap()
                    .join(".pgboxdb.toml")
            )
        );
        // A worktree `.git` file counts as a repository root too.
        let wt = tmp("wt");
        std::fs::write(wt.join(".git"), "gitdir: /elsewhere\n").unwrap();
        std::fs::create_dir_all(wt.join("a/b")).unwrap();
        assert_eq!(
            locate(&wt.join("a/b"), &[]).unwrap().root,
            std::fs::canonicalize(&wt).unwrap()
        );
        // Fallback directories are searched when the start has nothing.
        let bare = tmp("bare");
        let p = locate(&bare, &[root.join("sub")]).unwrap();
        assert_eq!(p.source.as_deref(), Some("tpl"));
        // Nothing at all: the start directory itself.
        let p = locate(&bare, &[]).unwrap();
        assert_eq!(p.root, std::fs::canonicalize(&bare).unwrap());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(wt);
        let _ = std::fs::remove_dir_all(bare);
    }

    #[test]
    fn a_pgboxdb_toml_above_the_repository_does_not_hijack_the_project() {
        // ~/.pgboxdb.toml left by an accidental setup must not turn every repo
        // under ~ into one project with someone else's source.
        let outer = tmp("outer");
        std::fs::write(outer.join(".pgboxdb.toml"), "[agent]\nsource = \"stray\"\n").unwrap();
        std::fs::create_dir_all(outer.join("repo/.git")).unwrap();
        std::fs::create_dir_all(outer.join("repo/src")).unwrap();
        let p = locate(&outer.join("repo/src"), &[]).unwrap();
        assert_eq!(p.root, std::fs::canonicalize(outer.join("repo")).unwrap());
        assert!(p.source.is_none(), "{p:?}");
        assert!(p.config_file.is_none());
        // A toml inside the repository root still wins.
        std::fs::write(outer.join("repo/.pgboxdb.toml"), "[agent]\nsource = \"mine\"\n").unwrap();
        assert_eq!(
            locate(&outer.join("repo/src"), &[])
                .unwrap()
                .source
                .as_deref(),
            Some("mine")
        );
        let _ = std::fs::remove_dir_all(outer);
    }

    #[test]
    fn project_ids_are_stable_and_distinct() {
        let a = tmp("ida");
        let b = tmp("idb");
        let ia = locate(&a, &[]).unwrap().project_id;
        assert_eq!(ia, locate(&a, &[]).unwrap().project_id);
        assert_ne!(ia, locate(&b, &[]).unwrap().project_id);
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }
}
