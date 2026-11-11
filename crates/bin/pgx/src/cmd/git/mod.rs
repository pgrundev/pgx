//! `pgx git …`: one Git branch, one isolated Postgres database, automatically.
//!
//! - `config`: the committed `[git]` tables of `.pgx.toml`
//! - `repo`: Git plumbing (repository, worktree, HEAD)
//! - `naming`: deterministic, collision-resistant database names
//! - `state`: PGX-managed local state and the repository lock
//! - `fingerprint`: cheap schema/migration change detection
//! - `core`: the main snapshot manager and branch database resolution
//! - `env`: connection environment and shell integration
//! - `hooks`: non-destructive Git hook installation
//! - `cmds`: the commands
use crate::errors::{PgxError, Result};
use std::path::PathBuf;

pub mod cmds;
pub mod config;
pub mod core;
pub mod env;
pub mod fingerprint;
pub mod guard;
pub mod hooks;
pub mod naming;
pub mod repo;
pub mod state;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InitOpts {
    pub source: Option<String>,
    pub main: Option<String>,
    pub schema_only: bool,
    pub with_data: bool,
    pub no_hooks: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitCmd {
    Init(InitOpts),
    Status,
    Ls,
    Sync {
        force: bool,
        migrate: bool,
    },
    Env {
        shell: Option<String>,
        hook: bool,
        /// `--reveal`: include the password in `--json` output.
        reveal: bool,
    },
    Exec {
        command: Vec<String>,
    },
    Reset {
        from_main: bool,
        force: bool,
    },
    Prune {
        apply: bool,
        grace: Option<String>,
    },
    Doctor,
    Uninstall {
        shell: bool,
    },
    Shell {
        shell: String,
    },
    InstallShell {
        shell: Option<String>,
    },
    /// Called by the installed Git hooks: `pgx git hook <name> [git args…]`.
    Hook {
        name: String,
        args: Vec<String>,
    },
}

const USAGE: &str = "usage: pgx git <init|status|ls|sync|env|exec|reset|prune|doctor|uninstall|shell|install-shell> [options]";

const VALUE_FLAGS: [&str; 4] = ["source", "main", "shell", "grace"];
const BOOL_FLAGS: [&str; 10] = [
    "reveal",
    "schema-only",
    "with-data",
    "no-hooks",
    "force",
    "migrate",
    "hook",
    "from-main",
    "apply",
    "dry-run",
];

pub fn parse(tail: &[&str]) -> Result<GitCmd> {
    let Some((&sub, rest)) = tail.split_first() else {
        return Err(PgxError::usage(USAGE));
    };
    if sub == "hook" {
        // Git's own arguments, verbatim: never parsed as options.
        let Some((name, args)) = rest.split_first() else {
            return Err(PgxError::usage(
                "usage: pgx git hook <post-checkout|post-merge|post-rewrite> [args…]",
            ));
        };
        return Ok(GitCmd::Hook {
            name: name.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        });
    }
    let mut flags: Vec<(String, Option<String>)> = Vec::new();
    let mut positional: Vec<String> = Vec::new();
    let mut command: Option<Vec<String>> = None;
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i];
        if t == "--" {
            command = Some(rest[i + 1..].iter().map(|s| s.to_string()).collect());
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
                    "pgx git {sub}: unknown option --{flag}"
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
                    "pgx git {sub}: unknown option --{k}"
                )));
            }
        }
        Ok(())
    };
    let value = |k: &str| {
        flags
            .iter()
            .find(|(f, _)| f == k)
            .and_then(|(_, v)| v.clone())
    };
    let has = |k: &str| flags.iter().any(|(f, _)| f == k);
    let no_pos = |u: &str| -> Result<()> {
        if positional.is_empty() && command.is_none() {
            Ok(())
        } else {
            Err(PgxError::usage(format!("usage: pgx git {u}")))
        }
    };
    let cmd = match sub {
        "init" => {
            only(&["source", "main", "schema-only", "with-data", "no-hooks"])?;
            no_pos("init [--source <database|postgres-url>] [--main <branch>] [--schema-only|--with-data] [--no-hooks]")?;
            if has("schema-only") && has("with-data") {
                return Err(PgxError::usage(
                    "pgx git init: --schema-only and --with-data exclude each other",
                ));
            }
            if let Some(m) = value("main") {
                if !config::valid_branch(&m) {
                    return Err(PgxError::usage(format!(
                        "--main {m:?} is not a valid Git branch name"
                    )));
                }
            }
            GitCmd::Init(InitOpts {
                source: value("source").filter(|s| !s.is_empty()),
                main: value("main"),
                schema_only: has("schema-only"),
                with_data: has("with-data"),
                no_hooks: has("no-hooks"),
            })
        }
        "status" => {
            only(&[])?;
            no_pos("status")?;
            GitCmd::Status
        }
        "ls" | "list" => {
            only(&[])?;
            no_pos("ls")?;
            GitCmd::Ls
        }
        "sync" => {
            only(&["force", "migrate"])?;
            no_pos("sync [--force] [--migrate]")?;
            GitCmd::Sync {
                force: has("force"),
                migrate: has("migrate"),
            }
        }
        "env" => {
            only(&["shell", "hook", "reveal"])?;
            no_pos("env [--shell bash|zsh|fish] [--reveal]")?;
            let shell = value("shell");
            if let Some(s) = &shell {
                env::Shell::parse(s)?;
            }
            if has("hook") && shell.is_none() {
                return Err(PgxError::usage("pgx git env --hook needs --shell"));
            }
            GitCmd::Env {
                shell,
                hook: has("hook"),
                reveal: has("reveal"),
            }
        }
        "exec" => {
            only(&[])?;
            match command {
                Some(c) if !c.is_empty() && positional.is_empty() => GitCmd::Exec { command: c },
                _ => return Err(PgxError::usage("usage: pgx git exec -- <command> [args…]")),
            }
        }
        "reset" => {
            only(&["from-main", "force"])?;
            no_pos("reset --from-main [--force]")?;
            if !has("from-main") {
                return Err(PgxError::usage(
                    "usage: pgx git reset --from-main [--force] (recreates this branch's database from the latest main snapshot)",
                ));
            }
            GitCmd::Reset {
                from_main: true,
                force: has("force"),
            }
        }
        "prune" => {
            only(&["apply", "dry-run", "grace"])?;
            no_pos("prune [--apply] [--grace <dur>]")?;
            if has("apply") && has("dry-run") {
                return Err(PgxError::usage(
                    "pgx git prune: --apply and --dry-run exclude each other",
                ));
            }
            if let Some(g) = value("grace") {
                crate::ttl::parse_ttl(&g)?;
            }
            GitCmd::Prune {
                apply: has("apply"),
                grace: value("grace"),
            }
        }
        "doctor" => {
            only(&[])?;
            no_pos("doctor")?;
            GitCmd::Doctor
        }
        // `uninstall --shell` is a switch; `parse_args` handles it.
        "uninstall" => {
            only(&[])?;
            no_pos("uninstall [--shell]")?;
            GitCmd::Uninstall { shell: false }
        }
        "shell" => {
            only(&[])?;
            if positional.len() != 1 || command.is_some() {
                return Err(PgxError::usage("usage: pgx git shell <bash|zsh|fish>"));
            }
            env::Shell::parse(&positional[0])?;
            GitCmd::Shell {
                shell: positional[0].clone(),
            }
        }
        "install-shell" => {
            only(&["shell"])?;
            if command.is_some() || positional.len() > 1 {
                return Err(PgxError::usage(
                    "usage: pgx git install-shell [bash|zsh|fish]",
                ));
            }
            let shell = value("shell").or_else(|| positional.first().cloned());
            if let Some(s) = &shell {
                env::Shell::parse(s)?;
            }
            GitCmd::InstallShell { shell }
        }
        other => {
            return Err(PgxError::usage(format!(
                "unknown git subcommand {other:?}\n{USAGE}"
            )))
        }
    };
    Ok(cmd)
}

/// Fix up `uninstall --shell`: the generic loop treats `--shell` as a value
/// flag. Pre-scan so `pgx git uninstall --shell` works as a switch.
pub fn parse_args(tail: &[&str]) -> Result<GitCmd> {
    if tail.first() == Some(&"uninstall") {
        let rest = &tail[1..];
        let mut shell = false;
        for a in rest {
            match *a {
                "--shell" => shell = true,
                other => {
                    return Err(PgxError::usage(format!(
                    "pgx git uninstall: unexpected {other:?}\nusage: pgx git uninstall [--shell]"
                )))
                }
            }
        }
        return Ok(GitCmd::Uninstall { shell });
    }
    parse(tail)
}

pub fn cwd() -> Result<PathBuf> {
    std::env::current_dir()
        .map_err(|e| PgxError::failure(format!("cannot read the current directory: {e}")))
}

pub fn run(out: &crate::out::Out, cmd: GitCmd) -> Result<crate::cli::ExitCode> {
    cmds::run(out, cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<GitCmd> {
        let v: Vec<&str> = s.split_whitespace().collect();
        parse_args(&v)
    }

    #[test]
    fn parses_every_subcommand() {
        assert_eq!(
            p("init --source app_tpl --main master --no-hooks").unwrap(),
            GitCmd::Init(InitOpts {
                source: Some("app_tpl".into()),
                main: Some("master".into()),
                no_hooks: true,
                ..Default::default()
            })
        );
        assert_eq!(p("status").unwrap(), GitCmd::Status);
        assert_eq!(p("ls").unwrap(), GitCmd::Ls);
        assert_eq!(
            p("sync --force").unwrap(),
            GitCmd::Sync {
                force: true,
                migrate: false
            }
        );
        assert_eq!(
            p("env --shell fish").unwrap(),
            GitCmd::Env {
                shell: Some("fish".into()),
                hook: false,
                reveal: false
            }
        );
        assert_eq!(
            p("exec -- bin/rails test --json").unwrap(),
            GitCmd::Exec {
                command: vec!["bin/rails".into(), "test".into(), "--json".into()]
            }
        );
        assert_eq!(
            p("reset --from-main --force").unwrap(),
            GitCmd::Reset {
                from_main: true,
                force: true
            }
        );
        assert_eq!(
            p("prune --apply --grace 1h").unwrap(),
            GitCmd::Prune {
                apply: true,
                grace: Some("1h".into())
            }
        );
        assert_eq!(
            p("prune").unwrap(),
            GitCmd::Prune {
                apply: false,
                grace: None
            }
        );
        assert_eq!(p("doctor").unwrap(), GitCmd::Doctor);
        assert_eq!(p("uninstall").unwrap(), GitCmd::Uninstall { shell: false });
        assert_eq!(
            p("uninstall --shell").unwrap(),
            GitCmd::Uninstall { shell: true }
        );
        assert_eq!(
            p("shell zsh").unwrap(),
            GitCmd::Shell {
                shell: "zsh".into()
            }
        );
        assert_eq!(
            p("install-shell bash").unwrap(),
            GitCmd::InstallShell {
                shell: Some("bash".into())
            }
        );
        assert_eq!(
            p("hook post-checkout aaa bbb 1").unwrap(),
            GitCmd::Hook {
                name: "post-checkout".into(),
                args: vec!["aaa".into(), "bbb".into(), "1".into()]
            }
        );
    }

    #[test]
    fn rejects_bad_usage() {
        for bad in [
            "",
            "frob",
            "init --schema-only --with-data",
            "init --main a..b",
            "init extra",
            "exec",
            "exec --",
            "reset",
            "reset --force",
            "env --hook",
            "env --shell tcsh",
            "shell",
            "shell tcsh",
            "prune --grace soon",
            "status --bogus",
            "uninstall --all",
        ] {
            assert_eq!(
                p(bad).unwrap_err().code,
                crate::cli::ExitCode::Usage,
                "{bad:?}"
            );
        }
    }
}
