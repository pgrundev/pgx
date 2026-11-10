//! The same-command branch-switch guard for agent shell commands.
//!
//! A command's database environment is fixed when the command starts. In
//! `git switch feature/x && bin/rails db:migrate` the migration would run
//! with the environment resolved *before* the switch, i.e. against the
//! previous Git branch's database. No shell can re-resolve its own
//! environment between two statements, so PGX refuses such a command
//! before it runs (the agent hook turns this into a deny with the reason)
//! and names the two reliable alternatives:
//!
//! - switch in one command, work in the next (the next command is resolved
//!   for the new branch);
//! - wrap the database part: `git switch x && pgx git exec -- <cmd>` —
//!   the wrapper resolves when *it* starts, after the switch.
//!
//! The scan is deliberately conservative and dependency-free: quote-aware
//! splitting on `&&`, `||`, `;`, `|`, `&`, newlines and parentheses. A
//! branch change hidden inside a script or a variable is not visible to it
//! (documented in docs/pgx/local-dev/git.md).
use std::path::{Path, PathBuf};

/// Split a shell command into simple-command segments, each a word list
/// with quotes removed.
fn segments(cmd: &str) -> Vec<Vec<String>> {
    let mut segs: Vec<Vec<String>> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = cmd.chars().peekable();
    let flush_word = |words: &mut Vec<String>, cur: &mut String, in_word: &mut bool| {
        if *in_word {
            words.push(std::mem::take(cur));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for x in chars.by_ref() {
                    if x == '\'' {
                        break;
                    }
                    cur.push(x);
                }
            }
            '"' => {
                in_word = true;
                while let Some(x) = chars.next() {
                    match x {
                        '"' => break,
                        '\\' => {
                            if let Some(y) = chars.next() {
                                cur.push(y);
                            }
                        }
                        _ => cur.push(x),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(y) = chars.next() {
                    if y != '\n' {
                        cur.push(y);
                    }
                }
            }
            '#' if !in_word => {
                // A comment runs to the end of the line.
                for x in chars.by_ref() {
                    if x == '\n' {
                        break;
                    }
                }
                flush_word(&mut words, &mut cur, &mut in_word);
                segs.push(std::mem::take(&mut words));
            }
            ' ' | '\t' => flush_word(&mut words, &mut cur, &mut in_word),
            ';' | '&' | '|' | '\n' | '(' | ')' | '{' | '}'
                if !in_word || matches!(c, ';' | '&' | '|' | '\n' | '(' | ')') =>
            {
                flush_word(&mut words, &mut cur, &mut in_word);
                segs.push(std::mem::take(&mut words));
            }
            x => {
                in_word = true;
                cur.push(x);
            }
        }
    }
    flush_word(&mut words, &mut cur, &mut in_word);
    segs.push(words);
    segs.retain(|s| !s.is_empty());
    segs
}

fn program(words: &[String]) -> (Option<&str>, &[String]) {
    // Skip `VAR=value` prefixes and the `command`/`env` launchers.
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        let assign = w.find('=').is_some_and(|p| {
            p > 0
                && w[..p]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        });
        if assign || w == "command" || w == "env" || w == "builtin" {
            i += 1;
            continue;
        }
        break;
    }
    match words.get(i) {
        Some(w) => (Some(w.rsplit('/').next().unwrap_or(w)), &words[i + 1..]),
        None => (None, &[]),
    }
}

/// Does this simple command move HEAD to another branch (or commit)?
fn switches_branch(words: &[String]) -> bool {
    let (prog, args) = program(words);
    match prog {
        Some("git") => {
            let mut i = 0;
            // Global options: `-C <dir>`, `-c k=v` take a value.
            while i < args.len() && args[i].starts_with('-') {
                if args[i] == "-C" || args[i] == "-c" {
                    i += 1;
                }
                i += 1;
            }
            match args.get(i).map(String::as_str) {
                Some("switch") => !args[i + 1..].iter().any(|a| a == "-h" || a == "--help"),
                // `git checkout -- <paths>` restores files; everything else may move HEAD.
                Some("checkout") => !args[i + 1..]
                    .iter()
                    .any(|a| a == "--" || a == "-h" || a == "--help"),
                _ => false,
            }
        }
        Some("gh") => args.len() >= 2 && args[0] == "pr" && args[1] == "checkout",
        _ => false,
    }
}

/// A `cd`/`pushd` to a literal directory in another Git worktree or repository.
fn changes_worktree(words: &[String], cwd: &Path) -> Option<PathBuf> {
    let (prog, args) = program(words);
    if !matches!(prog, Some("cd") | Some("pushd")) {
        return None;
    }
    let target = args.iter().find(|a| !a.starts_with('-'))?;
    if target.contains('$') || target.contains('`') || target.contains('*') {
        return None;
    }
    let p = if let Some(rest) = target.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME")?).join(rest)
    } else {
        cwd.join(target)
    };
    let here = super::repo::discover(cwd)
        .ok()
        .flatten()
        .map(|r| r.toplevel);
    let there = super::repo::discover(&p).ok().flatten().map(|r| r.toplevel);
    match (here, there) {
        (h, Some(t)) if h.as_ref() != Some(&t) => Some(p),
        _ => None,
    }
}

/// Re-resolving wrappers: they look the database up when they start.
fn is_reresolving(words: &[String]) -> bool {
    let (prog, args) = program(words);
    prog == Some("pgx")
        && args.len() >= 2
        && matches!(args[0].as_str(), "git" | "agent")
        && args[1] == "exec"
        || prog == Some("pgx")
            && args.len() >= 3
            && args[0] == "--json"
            && matches!(args[1].as_str(), "git" | "agent")
            && args[2] == "exec"
}

/// Commands that never touch a database and are harmless after a switch.
fn is_inert(words: &[String]) -> bool {
    matches!(
        program(words).0,
        Some(
            "git"
                | "echo"
                | "printf"
                | "true"
                | "false"
                | ":"
                | "pwd"
                | "ls"
                | "cat"
                | "sleep"
                | "gh"
                | "cd"
                | "pushd"
                | "popd"
        )
    )
}

/// `Some(reason)` when the command changes branch/worktree and then runs
/// something that would still use the old database.
pub fn check(cmd: &str, cwd: &Path) -> Option<String> {
    let segs = segments(cmd);
    let mut cwd = cwd.to_path_buf();
    let mut changed: Option<String> = None;
    for words in &segs {
        if let Some(what) = &changed {
            if is_reresolving(words) || is_inert(words) {
                continue;
            }
            return Some(format!(
                "this command {what} and then runs `{}` in the same invocation, which would still use the previous branch's database (a command's DATABASE_URL is fixed when it starts). Run the switch as its own command and the database work in the next one, or wrap the database part so it resolves after the switch: `… && pgx git exec -- {}`",
                words.join(" "),
                words.join(" ")
            ));
        }
        if switches_branch(words) {
            changed = Some(format!("switches Git branches (`{}`)", words.join(" ")));
        } else if let Some(p) = changes_worktree(words, &cwd) {
            changed = Some(format!(
                "moves to another Git worktree (`{}`)",
                words.join(" ")
            ));
            cwd = p;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deny(c: &str) -> bool {
        check(c, Path::new("/")).is_some()
    }

    #[test]
    fn switch_then_database_work_is_refused() {
        for c in [
            "git switch feature/x && bin/rails db:migrate",
            "git checkout -b feature/y; psql -c 'select 1'",
            "git -C . switch main\nnpm test",
            "FOO=1 git switch x || pytest",
            "(git switch x && npx prisma migrate dev)",
            "gh pr checkout 12 && bin/rails test",
            "git switch x && git pull && bin/rails db:migrate",
            "git switch x | tee log && python manage.py migrate",
        ] {
            assert!(deny(c), "{c}");
        }
    }

    #[test]
    fn safe_forms_pass() {
        for c in [
            "git switch feature/x",
            "git checkout -b y && git push -u origin y",
            "git switch x && pgx git exec -- bin/rails db:migrate",
            "git switch x && /usr/local/bin/pgx agent exec -- npm test",
            "git switch x && echo done",
            "git checkout -- app/models/user.rb && bin/rails test",
            "bin/rails db:migrate && git switch main",
            "echo 'git switch x && psql'",
            "psql -c \"select 'git switch && x'\"",
            "git switch --help && psql",
            "# git switch x && psql\nls",
            "cd app && npm test",
        ] {
            assert!(!deny(c), "{c}");
        }
    }

    #[test]
    fn segments_respect_quotes() {
        let s = segments("a 'b && c' && d \"e;f\" ; g\\;h");
        assert_eq!(
            s,
            vec![
                vec!["a".to_string(), "b && c".into()],
                vec!["d".into(), "e;f".into()],
                vec!["g;h".into()]
            ]
        );
    }
}
