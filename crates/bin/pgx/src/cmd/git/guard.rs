//! The same-command branch-switch guard for agent shell commands.
//!
//! A command's database environment is fixed when the command starts. In
//! `git switch feature/x && bin/rails db:migrate` the migration would run
//! with the environment resolved *before* the switch, i.e. against the
//! previous Git branch's database. No shell can re-resolve its own
//! environment between two statements, so PGX refuses such a command
//! before it runs and names the two reliable alternatives:
//!
//! - switch in one command, work in the next (the next command is resolved
//!   for the new branch);
//! - wrap the database part: `git switch x && pgx git exec -- <cmd>` —
//!   the wrapper resolves when *it* starts, after the switch.
//!
//! Where it runs: Claude Code's `PreToolUse` hook (parent sessions and
//! subagents), and inside `pgx agent exec` / `pgx git exec` for a
//! `<shell> -c '<script>'` command (Codex, Cursor and scripts use these).
//!
//! The scan is dependency-free and conservative: quote-aware splitting on
//! `&&`, `||`, `;`, `|`, `&`, newlines and parentheses, redirections such as
//! `2>&1` and `&>file` kept inside their command, shell keywords (`if`,
//! `then`, `!`, `time`, …) and launchers (`env`, `command`, `nohup`, …)
//! skipped. `git checkout <x>` counts as a switch when `<x>` is a ref (asked
//! from Git), not when it is a file. A branch change hidden inside a script
//! or a variable is not visible to it (documented in docs/pgx/local-dev/git.md).
use std::path::{Path, PathBuf};

/// Split a shell command into simple-command segments, each a word list
/// with quotes removed.
pub fn segments(cmd: &str) -> Vec<Vec<String>> {
    let mut segs: Vec<Vec<String>> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = cmd.chars().peekable();
    fn flush(words: &mut Vec<String>, cur: &mut String, in_word: &mut bool) {
        if *in_word {
            words.push(std::mem::take(cur));
            *in_word = false;
        }
    }
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
                flush(&mut words, &mut cur, &mut in_word);
                segs.push(std::mem::take(&mut words));
            }
            ' ' | '\t' => flush(&mut words, &mut cur, &mut in_word),
            // Redirections stay inside their command: `2>&1`, `>&2`, `<&3`,
            // `&>file`, `&>>file`, `>|file`.
            '&' if cur.ends_with('>') || cur.ends_with('<') || chars.peek() == Some(&'>') => {
                in_word = true;
                cur.push('&');
            }
            '|' if cur.ends_with('>') => {
                in_word = true;
                cur.push('|');
            }
            ';' | '&' | '|' | '\n' | '(' | ')' => {
                flush(&mut words, &mut cur, &mut in_word);
                segs.push(std::mem::take(&mut words));
            }
            x => {
                in_word = true;
                cur.push(x);
            }
        }
    }
    flush(&mut words, &mut cur, &mut in_word);
    segs.push(words);
    segs.retain(|s| !s.is_empty());
    segs
}

const KEYWORDS: [&str; 12] = [
    "if", "then", "elif", "else", "do", "while", "until", "!", "{", "}", "fi", "done",
];
const LAUNCHERS: [&str; 7] = ["command", "builtin", "exec", "nohup", "time", "env", "sudo"];

/// The program a simple command runs, and its arguments: `VAR=value`
/// prefixes, shell keywords and launchers (with their options) skipped.
fn program(words: &[String]) -> (Option<&str>, &[String]) {
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        let assign = w.find('=').is_some_and(|p| {
            p > 0
                && w[..p]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        });
        if assign || KEYWORDS.contains(&w) {
            i += 1;
            continue;
        }
        if LAUNCHERS.contains(&w) {
            i += 1;
            while i < words.len() && words[i].starts_with('-') {
                i += 1;
            }
            continue;
        }
        break;
    }
    match words.get(i) {
        Some(w) => (Some(w.rsplit('/').next().unwrap_or(w)), &words[i + 1..]),
        None => (None, &[]),
    }
}

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    super::repo::git_cmd(dir)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `git checkout <x>`: does `<x>` name something HEAD would move to?
fn is_ref(dir: &Path, x: &str) -> bool {
    if x.is_empty() || x.starts_with('-') && x != "-" {
        return false;
    }
    if x == "-" || x.starts_with("@{") {
        return true;
    }
    git_ok(dir, &["rev-parse", "--verify", "-q", &format!("{x}^{{commit}}")])
        // `git checkout feature` creates a tracking branch from a remote one.
        || super::repo::git_out(dir, &["for-each-ref", "--count=1", "--format=%(refname)", &format!("refs/remotes/*/{x}")])
            .is_some_and(|o| !o.trim().is_empty())
}

/// Does this simple command move HEAD to another branch (or commit)?
fn switches_branch(words: &[String], cwd: &Path) -> bool {
    let (prog, args) = program(words);
    match prog {
        Some("git") => {
            let mut dir = cwd.to_path_buf();
            let mut i = 0;
            // Global options; these take a separate value.
            while i < args.len() && args[i].starts_with('-') {
                let a = args[i].as_str();
                if matches!(
                    a,
                    "-C" | "-c"
                        | "--git-dir"
                        | "--work-tree"
                        | "--namespace"
                        | "--config-env"
                        | "--super-prefix"
                ) {
                    if a == "-C" {
                        if let Some(d) = args.get(i + 1) {
                            dir = dir.join(d);
                        }
                    }
                    i += 1;
                }
                i += 1;
            }
            let rest = args.get(i + 1..).unwrap_or(&[]);
            if rest.iter().any(|a| a == "-h" || a == "--help") {
                return false;
            }
            match args.get(i).map(String::as_str) {
                Some("switch") => true,
                Some("checkout") => {
                    // `git checkout [<tree>] -- <paths>` restores files.
                    if rest.iter().any(|a| a == "--") {
                        return false;
                    }
                    if rest.iter().any(|a| {
                        matches!(a.as_str(), "-b" | "-B" | "--orphan" | "--detach" | "-")
                            || a.starts_with("--orphan=")
                    }) {
                        return true;
                    }
                    match rest
                        .iter()
                        .find(|a| !a.starts_with('-') || a.as_str() == "-")
                    {
                        // `git checkout` alone does not move HEAD.
                        None => false,
                        Some(x) => is_ref(&dir, x),
                    }
                }
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
    if prog != Some("pgx") {
        return false;
    }
    let a: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--json")
        .collect();
    a.len() >= 2 && matches!(a[0], "git" | "agent") && a[1] == "exec"
}

/// Commands that never touch a database and are harmless after a switch.
fn is_inert(words: &[String]) -> bool {
    matches!(
        program(words).0,
        None | Some(
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
                | "test"
                | "["
                | "tail"
                | "head"
                | "grep"
                | "egrep"
                | "wc"
                | "sort"
                | "uniq"
                | "less"
                | "more"
                | "tee"
                | "cut"
                | "tr"
                | "column"
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
            let rest = words.join(" ");
            return Some(format!(
                "this command {what} and then runs `{rest}` in the same invocation, which would still use the previous branch's database (a command's DATABASE_URL is fixed when it starts). Run the switch as its own command and the database work in the next one, or wrap the database part so it resolves after the switch: `… && pgx git exec -- {rest}`"
            ));
        }
        if switches_branch(words, &cwd) {
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

/// The script of a `<shell> -c '<script>'` argv (what `pgx agent exec` and
/// `pgx git exec` run for hosts), for [`check`].
pub fn shell_script(argv: &[String]) -> Option<&str> {
    let prog = Path::new(argv.first()?).file_name()?.to_str()?;
    if !matches!(prog, "sh" | "bash" | "zsh" | "dash" | "ksh" | "ash") {
        return None;
    }
    let i = argv.iter().position(|a| {
        a == "-c" || (a.starts_with('-') && !a.starts_with("--") && a.ends_with('c'))
    })?;
    argv.get(i + 1).map(String::as_str)
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
            "git checkout - && npm test",
            "git -C . switch main\nnpm test",
            "FOO=1 git switch x || pytest",
            "(git switch x && npx prisma migrate dev)",
            "gh pr checkout 12 && bin/rails test",
            "git switch x && git pull && bin/rails db:migrate",
            "git switch x | tee log && python manage.py migrate",
            "if git switch x; then bin/rails db:migrate; fi",
            "time git switch x && psql",
            "time -p git switch x && psql",
            "! git switch x || psql",
            "git --git-dir .git switch x && psql",
            "git --work-tree . --git-dir .git switch x && psql",
            "env A=1 git switch x && psql",
            "git switch x 2>&1 && bin/rails db:migrate",
            "git switch x &>/dev/null; psql",
            "while true; do git switch x; psql; done",
        ] {
            assert!(deny(c), "{c}");
        }
    }

    #[test]
    fn safe_forms_pass() {
        for c in [
            "git switch feature/x",
            "git switch x 2>&1",
            "git switch x >/dev/null 2>&1",
            "git checkout main 2>&1 | tail -5",
            "git switch x 2>&1 | cat",
            "git checkout -b y && git push -u origin y",
            "git switch x && pgx git exec -- bin/rails db:migrate",
            "git switch x && pgx --json git exec -- bin/rails db:migrate",
            "git switch x && /usr/local/bin/pgx agent exec -- npm test",
            "git switch x && echo done",
            "git checkout -- app/models/user.rb && bin/rails test",
            "git checkout HEAD -- app/models/user.rb && bin/rails test",
            "bin/rails db:migrate && git switch main",
            "echo 'git switch x && psql'",
            "psql -c \"select 'git switch && x'\"",
            "git switch --help && psql",
            "# git switch x && psql\nls",
            "cd app && npm test",
            "if git switch x; then echo ok; fi",
        ] {
            assert!(!deny(c), "{c}");
        }
    }

    #[test]
    fn checkout_of_a_file_is_not_a_switch_but_of_a_ref_is() {
        if !super::super::repo::git_available() {
            return;
        }
        let d = std::env::temp_dir().join(format!(
            "pgx-guard-{}-{}",
            std::process::id(),
            crate::fsutil::now_epoch()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("app/models")).unwrap();
        let g = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(&d)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap()
                .status
                .success());
        };
        g(&["init", "-q", "-b", "main"]);
        std::fs::write(d.join("app/models/user.rb"), "x").unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "c"]);
        g(&["branch", "feature/a"]);
        assert!(check("git checkout app/models/user.rb && bin/rails test", &d).is_none());
        assert!(check("git checkout feature/a && bin/rails test", &d).is_some());
        assert!(check("git checkout main 2>&1 | tail -5", &d).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn segments_respect_quotes_and_redirections() {
        let s = segments("a 'b && c' && d \"e;f\" ; g\\;h");
        assert_eq!(
            s,
            vec![
                vec!["a".to_string(), "b && c".into()],
                vec!["d".into(), "e;f".into()],
                vec!["g;h".into()]
            ]
        );
        assert_eq!(
            segments("git switch x 2>&1"),
            vec![vec![
                "git".to_string(),
                "switch".into(),
                "x".into(),
                "2>&1".into()
            ]]
        );
        assert_eq!(segments("a &>/dev/null; b").len(), 2);
        assert_eq!(segments("a & b").len(), 2);
    }

    #[test]
    fn shell_scripts_are_found_in_argv() {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            shell_script(&v(&["/bin/zsh", "-c", "git switch x"])),
            Some("git switch x")
        );
        assert_eq!(shell_script(&v(&["bash", "-lc", "x"])), Some("x"));
        assert_eq!(shell_script(&v(&["bin/rails", "test"])), None);
    }
}
