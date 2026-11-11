//! Connection environment for the current Git database, and shell integration.
//!
//! A Git hook runs in a child process of `git`, which is a child of the
//! shell: it cannot change the shell's environment, and PGX never pretends it
//! did. Three honest mechanisms instead:
//!
//! - `pgx git exec -- <cmd>`: resolve now, exec the command with the
//!   variables set (works anywhere, no setup);
//! - `pgx git env --shell <sh>`: print `export` lines for `eval`;
//! - `pgx git shell <sh>`: a prompt hook that re-runs `pgx git env` whenever
//!   the worktree or branch changes, so the *next* command typed in that
//!   shell uses the right database. Already-running processes keep the
//!   environment they started with.
use super::state::BranchDb;
use crate::cmd::db::tcp_url;
use crate::cmd::Ctx;
use crate::errors::{PgxError, Result};
use std::path::Path;

/// Every variable PGX Git sets (and restores when the shell leaves the repository).
pub const VARS: [&str; 13] = [
    "DATABASE_URL",
    "PGHOST",
    "PGPORT",
    "PGUSER",
    "PGPASSWORD",
    "PGDATABASE",
    "PGX_BRANCH",
    "PGX_GIT_BRANCH",
    "PGX_WORKTREE",
    "PGX_SOURCE",
    "PGX_ISOLATION",
    "PGX_ISOLATION_REASON",
    "PGX_AGENT_ID",
];

pub fn pairs_for(
    ctx: &Ctx,
    db: &BranchDb,
    git_branch: &str,
    worktree: &Path,
    source: &str,
) -> Vec<(&'static str, String)> {
    vec![
        ("DATABASE_URL", tcp_url(ctx, &db.database)),
        ("PGHOST", "127.0.0.1".to_string()),
        ("PGPORT", ctx.config.port.to_string()),
        ("PGUSER", "postgres".to_string()),
        ("PGPASSWORD", ctx.config.password.clone()),
        ("PGDATABASE", db.database.clone()),
        ("PGX_BRANCH", db.database.clone()),
        ("PGX_GIT_BRANCH", git_branch.to_string()),
        ("PGX_WORKTREE", worktree.to_string_lossy().into_owned()),
        ("PGX_SOURCE", source.to_string()),
        ("PGX_ISOLATION", "ok".to_string()),
    ]
}

/// Fail closed: an unreachable URL, so nothing falls back to an inherited database.
pub fn sentinel(reason: &str) -> Vec<(&'static str, String)> {
    let mut v: Vec<(&'static str, String)> = crate::cmd::agent::session::sentinel_pairs(reason);
    v.push(("PGX_GIT_BRANCH", String::new()));
    v.push(("PGX_WORKTREE", String::new()));
    v
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Posix,
    Fish,
}

impl Shell {
    pub fn parse(s: &str) -> Result<Shell> {
        match s {
            "bash" | "zsh" | "sh" | "posix" => Ok(Shell::Posix),
            "fish" => Ok(Shell::Fish),
            other => Err(PgxError::usage(format!(
                "unsupported shell {other:?}: use bash, zsh or fish"
            ))),
        }
    }
}

fn sq(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

fn fish_q(v: &str) -> String {
    format!("'{}'", v.replace('\\', "\\\\").replace('\'', "\\'"))
}

pub fn export(sh: Shell, k: &str, v: &str) -> String {
    match sh {
        Shell::Posix => format!("export {k}={}\n", sq(v)),
        Shell::Fish => format!("set -gx {k} {}\n", fish_q(v)),
    }
}

pub fn unset(sh: Shell, k: &str) -> String {
    match sh {
        Shell::Posix => format!("unset {k}\n"),
        Shell::Fish => format!("set -e {k}\n"),
    }
}

/// Lines for `eval`. In `hook` mode (the prompt integration) the variables
/// the shell had before PGX took over are saved once, and restored by
/// [`restore_lines`] when the shell leaves PGX Git repositories.
pub fn lines(sh: Shell, pairs: &[(&'static str, String)], hook: bool) -> String {
    let mut s = String::new();
    if hook && std::env::var_os("__PGX_GIT_ACTIVE").is_none() {
        for k in VARS {
            match std::env::var(k) {
                Ok(v) => s.push_str(&export(sh, &format!("__PGX_SAVED_{k}"), &v)),
                Err(_) => s.push_str(&export(sh, &format!("__PGX_UNSET_{k}"), "1")),
            }
        }
    }
    for (k, v) in pairs {
        s.push_str(&export(sh, k, v));
    }
    // A sentinel set may not cover every variable: clear the rest.
    for k in VARS {
        if !pairs.iter().any(|(p, _)| p == &k) {
            s.push_str(&unset(sh, k));
        }
    }
    if hook {
        s.push_str(&export(sh, "__PGX_GIT_ACTIVE", "1"));
    }
    s
}

/// Undo [`lines`] in hook mode: the shell left every PGX Git repository.
pub fn restore_lines(sh: Shell) -> String {
    if std::env::var_os("__PGX_GIT_ACTIVE").is_none() {
        return String::new();
    }
    let mut s = String::new();
    for k in VARS {
        let saved = format!("__PGX_SAVED_{k}");
        let unset_mark = format!("__PGX_UNSET_{k}");
        if let Ok(v) = std::env::var(&saved) {
            s.push_str(&export(sh, k, &v));
        } else {
            s.push_str(&unset(sh, k));
        }
        s.push_str(&unset(sh, &saved));
        s.push_str(&unset(sh, &unset_mark));
    }
    s.push_str(&unset(sh, "PGX_ISOLATION_REASON"));
    s.push_str(&unset(sh, "__PGX_GIT_ACTIVE"));
    s
}

/// The prompt hook for `eval "$(pgx git shell zsh)"`.
pub fn snippet(shell: &str, pgx: &Path) -> Result<String> {
    let bin = pgx.to_string_lossy();
    Ok(match shell {
        "zsh" => format!(
            r#"# PGX Git shell integration (zsh). Refreshes DATABASE_URL and PG* before
# each prompt when the Git worktree or branch changed. Running processes keep
# the environment they started with: restart servers after switching branches.
__pgx_git_refresh() {{
  local key
  key="$(command git rev-parse --show-toplevel --symbolic-full-name HEAD 2>/dev/null)"
  [[ "$key" == "${{__PGX_GIT_KEY-}}" ]] && return
  __PGX_GIT_KEY="$key"
  [[ -z "$key" && -z "${{__PGX_GIT_ACTIVE-}}" ]] && return
  eval "$(command {pgx} git env --shell zsh --hook)"
}}
autoload -Uz add-zsh-hook 2>/dev/null && add-zsh-hook precmd __pgx_git_refresh
__pgx_git_refresh
"#,
            pgx = sq(&bin)
        ),
        "bash" => format!(
            r#"# PGX Git shell integration (bash). Refreshes DATABASE_URL and PG* before
# each prompt when the Git worktree or branch changed. Running processes keep
# the environment they started with: restart servers after switching branches.
__pgx_git_refresh() {{
  local key
  key="$(command git rev-parse --show-toplevel --symbolic-full-name HEAD 2>/dev/null)"
  [[ "$key" == "${{__PGX_GIT_KEY-}}" ]] && return
  __PGX_GIT_KEY="$key"
  [[ -z "$key" && -z "${{__PGX_GIT_ACTIVE-}}" ]] && return
  eval "$(command {pgx} git env --shell bash --hook)"
}}
case ";${{PROMPT_COMMAND-}};" in
  *";__pgx_git_refresh;"*) ;;
  *) PROMPT_COMMAND="__pgx_git_refresh${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}" ;;
esac
__pgx_git_refresh
"#,
            pgx = sq(&bin)
        ),
        "fish" => format!(
            r#"# PGX Git shell integration (fish). Refreshes DATABASE_URL and PG* before
# each prompt when the Git worktree or branch changed. Running processes keep
# the environment they started with: restart servers after switching branches.
function __pgx_git_refresh --on-event fish_prompt
    set -l key (command git rev-parse --show-toplevel --symbolic-full-name HEAD 2>/dev/null | string collect)
    if test "$key" = "$__PGX_GIT_KEY"
        return
    end
    set -g __PGX_GIT_KEY "$key"
    if test -z "$key"; and not set -q __PGX_GIT_ACTIVE
        return
    end
    command {pgx} git env --shell fish --hook | source
end
__pgx_git_refresh
"#,
            pgx = fish_q(&bin)
        ),
        other => {
            return Err(PgxError::usage(format!(
                "unsupported shell {other:?}: use bash, zsh or fish"
            )))
        }
    })
}

pub const RC_BEGIN: &str = "# >>> pgx git shell integration >>>";
pub const RC_END: &str = "# <<< pgx git shell integration <<<";

pub fn rc_file(shell: &str) -> Result<std::path::PathBuf> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| PgxError::failure("HOME is not set"))?;
    Ok(match shell {
        "zsh" => std::env::var_os("ZDOTDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or(home)
            .join(".zshrc"),
        "bash" => home.join(".bashrc"),
        "fish" => home.join(".config").join("fish").join("config.fish"),
        other => {
            return Err(PgxError::usage(format!(
                "unsupported shell {other:?}: use bash, zsh or fish"
            )))
        }
    })
}

pub fn rc_block(shell: &str) -> String {
    let line = match shell {
        "fish" => "if command -q pgx\n    pgx git shell fish | source\nend".to_string(),
        sh => {
            format!("if command -v pgx >/dev/null 2>&1; then\n  eval \"$(pgx git shell {sh})\"\nfi")
        }
    };
    format!("{RC_BEGIN}\n# Added by `pgx git install-shell`; remove with `pgx git uninstall --shell`.\n{line}\n{RC_END}\n")
}

/// Remove the marked block; `None` when there was none.
pub fn strip_block(text: &str, begin: &str, end: &str) -> Option<String> {
    let start = text.find(begin)?;
    let stop = text[start..].find(end)? + start + end.len();
    let mut out = String::new();
    out.push_str(&text[..start]);
    let rest = &text[stop..];
    out.push_str(rest.strip_prefix('\n').unwrap_or(rest));
    Some(out)
}

pub fn shell_installed(shell: &str) -> bool {
    rc_file(shell)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .is_some_and(|t| t.contains(RC_BEGIN))
}

pub fn any_shell_installed() -> bool {
    ["zsh", "bash", "fish"].iter().any(|s| shell_installed(s))
}

/// The user's login shell family, for defaults.
pub fn default_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .and_then(|s| {
            Path::new(&s)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .filter(|s| matches!(s.as_str(), "zsh" | "bash" | "fish"))
        .unwrap_or_else(|| "zsh".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_survives_hostile_values() {
        let v = "it's $(rm -rf /) `x` \\ \"q\"";
        let line = export(Shell::Posix, "X", v);
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("{line}printf '%s' \"$X\""))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), v);
        assert_eq!(
            export(Shell::Fish, "X", "a'b\\c"),
            "set -gx X 'a\\'b\\\\c'\n"
        );
    }

    #[test]
    fn rc_blocks_are_idempotent_and_removable() {
        let b = rc_block("zsh");
        let text = format!("export A=1\n{b}alias x=y\n");
        assert_eq!(
            strip_block(&text, RC_BEGIN, RC_END).unwrap(),
            "export A=1\nalias x=y\n"
        );
        assert!(strip_block("nothing", RC_BEGIN, RC_END).is_none());
        assert!(rc_block("fish").contains("pgx git shell fish | source"));
    }

    #[test]
    fn snippets_exist_for_each_shell() {
        for sh in ["zsh", "bash", "fish"] {
            let s = snippet(sh, Path::new("/opt/p x/pgx")).unwrap();
            assert!(s.contains("git env --shell"), "{sh}");
            assert!(s.contains("restart servers"), "{sh}");
        }
        assert!(snippet("tcsh", Path::new("/x")).is_err());
    }
}
