//! `pgx agent hook`: the one command every host's hook configuration runs.
//! Reads the host's JSON payload on stdin, works out who is calling and what
//! happened, and answers in that host's dialect. It never exits 2 (a blocking
//! error in every host) and never prints a password to stdout.
//!
//! Claude Code subagents: `SubagentStart` gives the subagent its own branch,
//! `PreToolUse` for its Bash commands makes each command source that branch's
//! environment before it runs (the host applies `updatedInput`), and
//! `SubagentStop` retires the branch. The session itself keeps the
//! `SessionStart`/`CLAUDE_ENV_FILE` path it always had.
use super::hosts::{self, Event, HookInput, Host};
use super::session::{self, ResolveInput, Resolved, UpResult};
use super::HookOpts;
use crate::cli::ExitCode;
use crate::errors::{PgxError, Result};
use crate::layout::Home;
use crate::out::Out;
use serde_json::json;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;

fn env_map() -> HashMap<String, String> {
    std::env::vars().collect()
}

fn notice(msg: &str) {
    eprintln!("pgx agent hook: {msg}");
}

/// The trailing comment of a command this hook has already rewritten; a
/// second delivery of the same command (two hook files) leaves it alone.
pub const ROUTED_MARK: &str = "# pgx-routed";

/// A command this hook rewrote, parsed back: `(agent id, cwd, the original
/// command)`. Strict: exactly `<pgx> agent exec [--agent <id>] --cwd <dir>
/// -- <shell> -c '<original>' # pgx-routed <branch>` and nothing else, so a
/// hand-written `# pgx-routed` comment cannot pass for one.
pub fn parse_routed(command: &str) -> Option<(Option<String>, String, String)> {
    let last = command.lines().last()?;
    let mark = last.rfind(ROUTED_MARK)?;
    let tail = &last[mark + ROUTED_MARK.len()..];
    let tail_ok = tail.starts_with(' ') && tail.split_whitespace().count() == 1;
    if !tail_ok {
        return None;
    }
    let segs = crate::cmd::git::guard::segments(command);
    let [w] = segs.as_slice() else { return None };
    let w: Vec<&str> = w.iter().map(String::as_str).collect();
    let prog_ok = |p: &str| p.rsplit('/').next() == Some("pgx");
    let (agent, rest) = match w.as_slice() {
        [p, "agent", "exec", "--agent", a, rest @ ..] if prog_ok(p) => (Some(a.to_string()), rest),
        [p, "agent", "exec", rest @ ..] if prog_ok(p) => (None, rest),
        _ => return None,
    };
    match rest {
        ["--cwd", cwd, "--", shell, "-c", inner]
            if matches!(
                shell.rsplit('/').next(),
                Some("sh" | "bash" | "zsh" | "dash" | "ksh")
            ) =>
        {
            Some((agent, cwd.to_string(), inner.to_string()))
        }
        _ => None,
    }
}

/// Already rewritten by this hook (for anyone: [`parse_routed`] says for whom).
#[cfg(test)]
pub fn is_routed(command: &str) -> bool {
    parse_routed(command).is_some()
}

/// Rewritten by this hook *for this caller* (the parent session when
/// `agent_id` is None, else that subagent).
fn routed_for(command: &str, agent_id: Option<&str>) -> Option<String> {
    let (a, _cwd, inner) = parse_routed(command)?;
    (a.as_deref() == agent_id).then_some(inner)
}

/// PGX Git: the Git scope a session (or subagent) last ran a command in.
/// Several agents in one worktree share its HEAD: when it moves under an
/// agent, that agent is told once before its database changes.
fn scope_path(home: &Home, key: &super::identity::SessionKey) -> std::path::PathBuf {
    let mut k = key.clone();
    k.scope = None;
    home.root.join("agent").join("exported").join(format!(
        "scope-{}",
        super::identity::hex16(super::identity::fnv1a64(k.material().as_bytes()))
    ))
}

/// PGX Git: the branch a Claude Code session's environment file exported at
/// `SessionStart`. When the session later switches Git branches, its Bash
/// commands are routed to the branch for the new Git branch instead.
fn exported_path(home: &Home, key: &super::identity::SessionKey) -> std::path::PathBuf {
    let mut k = key.clone();
    k.scope = None;
    home.root
        .join("agent")
        .join("exported")
        .join(super::identity::hex16(super::identity::fnv1a64(
            k.material().as_bytes(),
        )))
}

fn write_exported(home: &Home, key: &super::identity::SessionKey, branch: &str) {
    let p = exported_path(home, key);
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(&p, branch);
}

fn read_exported(home: &Home, key: &super::identity::SessionKey) -> Option<String> {
    std::fs::read_to_string(exported_path(home, key))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The parent session's form of [`routed_command`]: no `--agent`, the
/// session id comes from the environment Claude Code gives its commands.
pub fn routed_parent_command(
    pgx: &Path,
    shell: &Path,
    cwd: &Path,
    branch: &str,
    original: &str,
) -> String {
    format!(
        "{} agent exec --cwd {} -- {} -c {} {ROUTED_MARK} {branch}",
        shell_word(&pgx.to_string_lossy()),
        shell_word(&cwd.to_string_lossy()),
        shell_word(&shell.to_string_lossy()),
        sq(original)
    )
}

/// Where every hook delivery leaves one line: `$PGX_HOME/agent/hooks.log`.
/// Timestamps, host, event, ids, tool and outcome only, never a value from
/// the environment. `pgx agent doctor` reads it; so does anyone checking
/// that a subagent really was started, routed and stopped.
pub fn audit_log_path(home: &Home) -> std::path::PathBuf {
    home.root.join("agent").join("hooks.log")
}

const AUDIT_MAX_BYTES: u64 = 512 * 1024;

fn audit(home: &Home, input: &HookInput, outcome: &str) {
    let path = audit_log_path(home);
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    // Keep the file bounded: past the limit, keep roughly the last quarter.
    if let Ok(md) = std::fs::metadata(&path) {
        if md.len() > AUDIT_MAX_BYTES {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let keep = text.len() / 4;
                let tail = &text[text.len() - keep..];
                let tail = tail.split_once('\n').map(|(_, t)| t).unwrap_or(tail);
                let _ = std::fs::write(&path, tail);
            }
        }
    }
    let sid: String = input.session_id.chars().take(12).collect();
    let line = format!(
        "{} {} {} session={} agent={} tool={} -> {}\n",
        crate::fsutil::now_rfc3339(),
        input.host.as_str(),
        input.raw_event,
        sid,
        input.agent_id.as_deref().unwrap_or("-"),
        input.tool_name.as_deref().unwrap_or("-"),
        outcome
    );
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    if let Ok(mut f) = opts.open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// The sentence the agent sees when its branch is ready.
fn ready_context(res: &UpResult, host: Host, env: &HashMap<String, String>) -> String {
    let mut origin = format!("a copy of {}", res.source);
    if res.source_created {
        origin.push_str(", created empty because no .pgx.toml names a source");
    } else if res.fresh {
        origin.push_str(", fresh");
    }
    let mut s = format!(
        "PGX: this session's database is branch {} ({origin}). DATABASE_URL and PG* are already set in your shell; do not point them elsewhere. The branch is disposable and expires {} after your last activity.",
        res.branch,
        crate::ttl::humanize_long(res.idle_ttl.as_seconds().unwrap_or(0)),
    );
    if host == Host::Codex {
        s.push_str(
            " Shell commands do not inherit it from this hook: run database commands as `pgx agent exec -- <command>`, or start Codex as `pgx agent exec -- codex`.",
        );
        if env
            .get("CODEX_SANDBOX_NETWORK_DISABLED")
            .is_some_and(|v| v == "1")
        {
            s.push_str(
                " Codex's sandbox blocks loopback TCP; set sandbox_workspace_write.network_access = true for this project.",
            );
        }
    }
    s
}

/// What a subagent reads before its first prompt. No connection details:
/// its Bash commands are routed to the branch by this hook, so there is
/// nothing for the model to copy, and nothing it could get wrong.
fn subagent_ready_context(res: &UpResult) -> String {
    let mut origin = format!("a copy of {}", res.source);
    if res.fresh {
        origin.push_str(", fresh");
    }
    format!(
        "PGX: this subagent has its own database, branch {} ({origin}), separate from the parent session's. Every Bash command you run is routed to it automatically (DATABASE_URL and PG* are set for you); do not set them yourself and do not use another branch's connection. It is disposable and expires {} after your last activity.",
        res.branch,
        crate::ttl::humanize_long(res.idle_ttl.as_seconds().unwrap_or(0)),
    )
}

fn failed_context(reason: &str) -> String {
    format!(
        "PGX isolation FAILED for this session: {reason}. DATABASE_URL now points at an unreachable sentinel so nothing runs against another database. Fix it (pgx agent doctor) and start a new session."
    )
}

fn subagent_failed_context(reason: &str) -> String {
    format!(
        "PGX isolation FAILED for this subagent: {reason}. Bash commands will be refused until it is fixed (pgx agent doctor); report this to the parent instead of working around it."
    )
}

/// Append to `$CLAUDE_ENV_FILE`; without it, the lines go to stderr as a notice.
fn write_claude_env(env: &HashMap<String, String>, lines: &str) {
    match env.get("CLAUDE_ENV_FILE").filter(|p| !p.is_empty()) {
        Some(path) => {
            let r = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| f.write_all(lines.as_bytes()));
            if let Err(e) = r {
                notice(&format!("cannot write {path}: {e}"));
            }
        }
        None => {
            // Never echo values (the URL carries the password): names only.
            let names: Vec<&str> = lines
                .lines()
                .filter_map(|l| l.strip_prefix("export "))
                .filter_map(|l| l.split('=').next())
                .collect();
            notice(&format!(
                "CLAUDE_ENV_FILE is not set (not a SessionStart hook?); could not export {}",
                names.join(", ")
            ))
        }
    }
}

fn pairs_to_json(pairs: &[(&'static str, String)]) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    for (k, v) in pairs {
        m.insert((*k).to_string(), json!(v));
    }
    serde_json::Value::Object(m)
}

fn emit_start(host: Host, env: &HashMap<String, String>, res: &UpResult) {
    let context = ready_context(res, host, env);
    match host {
        Host::Claude => {
            write_claude_env(env, &session::env_lines(res));
            println!("{context}");
        }
        Host::Cursor => println!(
            "{}",
            json!({"env": pairs_to_json(&session::env_pairs(res)), "additional_context": context})
        ),
        Host::Codex => println!(
            "{}",
            json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": context}})
        ),
    }
}

/// `up` failed. With isolation enabled the session fails closed; otherwise it
/// just learns why it has no database.
fn emit_failure(
    host: Host,
    env: &HashMap<String, String>,
    isolation: bool,
    reason: &str,
) -> ExitCode {
    notice(&format!("no database for this session: {reason}"));
    if isolation {
        let context = failed_context(reason);
        match host {
            Host::Claude => {
                write_claude_env(env, &session::sentinel_env_lines(reason));
                println!("{context}");
            }
            Host::Cursor => println!(
                "{}",
                json!({"env": pairs_to_json(&session::sentinel_pairs(reason)), "additional_context": context})
            ),
            Host::Codex => println!(
                "{}",
                json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": context}})
            ),
        }
        // Hosts apply a hook's output only on exit 0 (Cursor drops the JSON,
        // Claude drops the context): the sentinel travels on success, the
        // stderr notice explains. Exit 1 is for the case nothing was emitted.
        return ExitCode::Ok;
    }
    ExitCode::Failure
}

fn cursor_ack(host: Host) {
    if host == Host::Cursor {
        println!("{{}}");
    }
}

fn reason_of(e: &PgxError) -> String {
    let mut reason = e.message.clone();
    if let Some(h) = &e.hint {
        reason.push_str(&format!(" ({h})"));
    }
    reason
}

fn sq(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

/// Quote only when the shell would need it, so the rewritten command stays
/// readable and a host's prefix-based permission rule (`Bash(<pgx> agent
/// exec:*)`) can match it.
fn shell_word(v: &str) -> String {
    let plain = !v.is_empty()
        && v.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'-' | b'_' | b'.' | b'/' | b':' | b'@' | b'+' | b'=' | b',' | b'%'
                )
        });
    if plain {
        v.to_string()
    } else {
        sq(v)
    }
}

/// The command a subagent's Bash tool runs instead of `original`:
/// `<pgx> agent exec --agent <id> --cwd <dir> -- <shell> -c '<original>'
/// # pgx-routed <branch>`. The wrapper attaches to the subagent's branch
/// (resolved from the session id in the environment plus the agent id;
/// `--cwd` pins the project the hook saw, so a `cd` elsewhere cannot make
/// it a different one), puts that branch's `DATABASE_URL`/`PG*` into the
/// child's environment, clears inherited `PGSERVICE` settings, runs the
/// original text in the user's shell and exits with its status. The
/// original is passed as one single-quoted argument (`'` becomes `'\''`),
/// so it reaches the inner shell byte for byte: pipes, heredocs, redirects,
/// quoting and background jobs are the original's. No secret is in the
/// text: the password travels in the wrapper's environment. If the branch
/// cannot be attached the wrapper fails before running anything. The
/// invocation comes first and the marker last so that a prefix-based
/// permission rule can recognise every routed command.
pub fn routed_command(
    pgx: &Path,
    shell: &Path,
    cwd: &Path,
    agent_id: &str,
    branch: &str,
    original: &str,
) -> String {
    format!(
        "{} agent exec --agent {} --cwd {} -- {} -c {} {ROUTED_MARK} {branch}",
        shell_word(&pgx.to_string_lossy()),
        shell_word(agent_id),
        shell_word(&cwd.to_string_lossy()),
        shell_word(&shell.to_string_lossy()),
        sq(original)
    )
}

/// How to invoke `pgx` from the host's shell: the path the hook was started
/// with when absolute, else the first `pgx` on the hook's PATH (the hook
/// command appends the installer's locations), else the running executable.
/// A stable path such as `~/.local/bin/pgx` is what a permission rule names.
fn pgx_invocation(env: &HashMap<String, String>) -> std::path::PathBuf {
    if let Some(arg0) = std::env::args_os().next() {
        let p = std::path::PathBuf::from(arg0);
        if p.is_absolute() {
            return p;
        }
    }
    if let Some(path) = env.get("PATH") {
        for dir in path.split(':').filter(|d| !d.is_empty()) {
            let cand = std::path::Path::new(dir).join("pgx");
            if cand.is_file() {
                return cand;
            }
        }
    }
    std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("pgx"))
}

/// The shell the host runs Bash tool commands in: `$SHELL` when it names an
/// absolute path to a Bourne-family shell, else `/bin/sh`.
fn user_shell(env: &HashMap<String, String>) -> std::path::PathBuf {
    let candidate = env
        .get("SHELL")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_file())
        .filter(|p| {
            matches!(
                p.file_name().and_then(|n| n.to_str()),
                Some("bash" | "zsh" | "sh" | "dash" | "ksh")
            )
        });
    candidate.unwrap_or_else(|| std::path::PathBuf::from("/bin/sh"))
}

fn subagent_start_json(context: &str) -> serde_json::Value {
    json!({"hookSpecificOutput": {"hookEventName": "SubagentStart", "additionalContext": context}})
}

fn deny_json(reason: &str) -> serde_json::Value {
    json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": format!("PGX: {reason}")}})
}

fn routed_json(tool_input: &serde_json::Value, command: String) -> serde_json::Value {
    let mut input = tool_input.clone();
    input["command"] = json!(command);
    json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "updatedInput": input}})
}

/// `SubagentStart`: the subagent's own branch, created or resumed.
fn subagent_start(out: &Out, home: &Home, input: &HookInput, r: &Resolved) -> ExitCode {
    let started = std::time::Instant::now();
    match session::up(&out.quiet(), r) {
        Ok(res) => {
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            for w in &res.guard_warnings {
                notice(&format!("guard not applied on {}: {w}", res.branch));
            }
            println!("{}", subagent_start_json(&subagent_ready_context(&res)));
            audit(
                home,
                input,
                &format!(
                    "{} {} in {ms:.0} ms",
                    if res.created { "created" } else { "resumed" },
                    res.branch
                ),
            );
            ExitCode::Ok
        }
        Err(e) => {
            let reason = reason_of(&e);
            notice(&format!("no database for this subagent: {reason}"));
            audit(home, input, &format!("failed: {reason}"));
            // The model learns why; the PreToolUse hook enforces it.
            println!("{}", subagent_start_json(&subagent_failed_context(&reason)));
            ExitCode::Ok
        }
    }
}

/// `SubagentStop`: retire the subagent's branch (grace period), terminating
/// whatever it left connected. Never the parent's branch: the key carries
/// the agent id. Repeated stops are harmless.
fn subagent_stop(home: &Home, input: &HookInput, r: &Resolved) -> ExitCode {
    match session::down(r, false) {
        Ok(d) => audit(
            home,
            input,
            &match &d.branch {
                Some(b) => format!(
                    "retired {b} (terminated {} backend(s), {} remaining)",
                    d.backends_terminated, d.backends_remaining
                ),
                None => "no branch".to_string(),
            },
        ),
        Err(e) => {
            notice(&e.message);
            audit(home, input, &format!("down failed: {}", e.message));
        }
    }
    ExitCode::Ok
}

fn touch_quietly(r: &Resolved) {
    if let Err(e) = session::touch(r) {
        notice(&e.message);
    }
}

/// `PreToolUse`. The session's own tool calls are activity. A subagent's
/// Bash command in an opted-in project is routed to the subagent's branch:
/// the branch is ensured (resumed, or recreated if it vanished) and the host
/// told to run the command through the wrapper. If the branch cannot be
/// ensured the command is refused with the reason; it never runs against
/// whatever the parent exported.
fn tool_use(
    out: &Out,
    home: &Home,
    env: &HashMap<String, String>,
    input: &HookInput,
    r: &Resolved,
) -> ExitCode {
    let bash = input.tool_name.as_deref() == Some("Bash");
    // PGX Git: a command that switches branches and then keeps going would
    // run its database work with the previous branch's environment. Refuse
    // it before it runs (parent and subagents alike), with the fix.
    if bash && r.git.is_some() && input.host == Host::Claude {
        let command = input
            .tool_input
            .as_ref()
            .and_then(|t| t.get("command"))
            .and_then(|c| c.as_str())
            .unwrap_or("");
        let cwd = input.cwd.clone().unwrap_or_else(|| r.project.root.clone());
        // A command this hook rewrote is checked by what it will run.
        let text =
            routed_for(command, input.agent_id.as_deref()).unwrap_or_else(|| command.to_string());
        if let Some(reason) = crate::cmd::git::guard::check(&text, &cwd) {
            println!("{}", deny_json(&reason));
            audit(
                home,
                input,
                "denied: branch switch followed by more commands",
            );
            return ExitCode::Ok;
        }
        // The worktree's HEAD moved since this agent's last command (it
        // switched, or another agent or the developer did): say so once
        // before its commands start using another Git branch's database.
        if let Some(scope) = &r.key.scope {
            let p = scope_path(home, &r.key);
            let last = std::fs::read_to_string(&p)
                .ok()
                .map(|s| s.trim().to_string());
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(&p, scope);
            if last.as_deref().is_some_and(|l| !l.is_empty() && l != scope) {
                let label = r.git.as_ref().map(|g| g.label.clone()).unwrap_or_default();
                let why = format!(
                    "the Git branch in this worktree is now {label}, not the one your previous command ran on. From now on your commands use your own copy of {label}'s database. If you did not switch branches yourself, another agent or the developer did (agents sharing a worktree share its HEAD; give each agent its own worktree): check before changing data. Re-run the command to continue."
                );
                println!("{}", deny_json(&why));
                audit(
                    home,
                    input,
                    &format!("denied once: git branch changed to {label}"),
                );
                return ExitCode::Ok;
            }
        }
    }
    if input.agent_id.is_none()
        && bash
        && input.agent_id_error.is_none()
        && r.git.is_some()
        && input.host == Host::Claude
    {
        // PGX Git: the session's exported environment names the branch for
        // the Git branch it started on. After a `git switch` its commands
        // must reach the branch for the new Git branch.
        let command = input
            .tool_input
            .as_ref()
            .and_then(|t| t.get("command"))
            .and_then(|c| c.as_str())
            .unwrap_or("");
        if let (Some(exported), false) = (
            read_exported(home, &r.key),
            command.trim().is_empty() || routed_for(command, None).is_some(),
        ) {
            match session::up(&out.quiet(), r) {
                Ok(res) if res.branch.as_str() != exported => {
                    let cwd = input.cwd.clone().unwrap_or_else(|| r.project.root.clone());
                    let routed = routed_parent_command(
                        &pgx_invocation(env),
                        &user_shell(env),
                        &cwd,
                        res.branch.as_str(),
                        command,
                    );
                    println!(
                        "{}",
                        routed_json(input.tool_input.as_ref().unwrap(), routed)
                    );
                    audit(
                        home,
                        input,
                        &format!("git branch changed: routed to {}", res.branch),
                    );
                    return ExitCode::Ok;
                }
                Ok(_) => {}
                Err(e) => {
                    let reason = reason_of(&e);
                    println!("{}", deny_json(&reason));
                    audit(home, input, &format!("denied: {reason}"));
                    return ExitCode::Ok;
                }
            }
            return ExitCode::Ok;
        }
    }
    if input.agent_id.is_none() {
        if bash && input.agent_id_error.is_some() && r.isolation_enabled() {
            let why = format!(
                "cannot identify this subagent ({}); refusing to run the command against an unknown database",
                input.agent_id_error.as_deref().unwrap_or("")
            );
            println!("{}", deny_json(&why));
            audit(home, input, &format!("denied: {why}"));
            return ExitCode::Ok;
        }
        touch_quietly(r);
        return ExitCode::Ok;
    }
    if !bash || !r.isolation_enabled() {
        touch_quietly(r);
        return ExitCode::Ok;
    }
    let Some(tool_input) = input.tool_input.as_ref() else {
        touch_quietly(r);
        return ExitCode::Ok;
    };
    let command = tool_input
        .get("command")
        .and_then(|c| c.as_str())
        .unwrap_or("");
    if command.trim().is_empty() || routed_for(command, input.agent_id.as_deref()).is_some() {
        touch_quietly(r);
        return ExitCode::Ok;
    }
    let agent_id = input.agent_id.as_deref().unwrap_or_default();
    let started = std::time::Instant::now();
    match session::up(&out.quiet(), r) {
        Ok(res) => {
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            let pgx = pgx_invocation(env);
            let cwd = input.cwd.clone().unwrap_or_else(|| r.project.root.clone());
            let routed = routed_command(
                &pgx,
                &user_shell(env),
                &cwd,
                agent_id,
                res.branch.as_str(),
                command,
            );
            println!("{}", routed_json(tool_input, routed));
            audit(
                home,
                input,
                &format!(
                    "routed to {}{} in {ms:.0} ms",
                    res.branch,
                    if res.created { " (created)" } else { "" }
                ),
            );
            ExitCode::Ok
        }
        Err(e) => {
            let reason = reason_of(&e);
            println!("{}", deny_json(&reason));
            audit(home, input, &format!("denied: {reason}"));
            ExitCode::Ok
        }
    }
}

pub fn run(out: &Out, opts: HookOpts) -> Result<ExitCode> {
    let mut payload = String::new();
    if std::io::stdin().read_to_string(&mut payload).is_err() || payload.trim().is_empty() {
        return Ok(ExitCode::Ok);
    }
    let env = env_map();
    let value: serde_json::Value = match serde_json::from_str(&payload) {
        Ok(v) => v,
        Err(e) => {
            notice(&format!("payload is not JSON: {e}"));
            return Ok(ExitCode::Failure);
        }
    };
    let input: HookInput = match hosts::detect(&value, &env, opts.host.as_deref()) {
        Ok(i) => i,
        Err(e) => {
            notice(&e.message);
            return Ok(ExitCode::Failure);
        }
    };
    if input.background || input.event == Event::Ignore {
        cursor_ack(input.host);
        return Ok(ExitCode::Ok);
    }
    let home = Home::resolve();
    let subagent_bash = input.event == Event::ToolUse
        && input.tool_name.as_deref() == Some("Bash")
        && (input.agent_id.is_some() || input.agent_id_error.is_some());
    let resolved: std::result::Result<Resolved, PgxError> = session::resolve(&ResolveInput {
        id: Some(input.session_id.clone()),
        host: Some(input.host.as_str().to_string()),
        agent_id: input.agent_id.clone(),
        source: opts.source.clone(),
        ttl: opts.ttl.clone(),
        grace: opts.grace.clone(),
        cwd: input.cwd.clone(),
        fallbacks: input.project_dir.iter().cloned().collect(),
    });
    let r = match resolved {
        Ok(r) => r,
        Err(e) => {
            // A broken .pgx.toml is an opted-in project whose setup is wrong:
            // fail closed where a database would be used, stay quiet otherwise.
            return Ok(match input.event {
                Event::Start => emit_failure(input.host, &env, true, &e.message),
                Event::SubagentStart => {
                    audit(&home, &input, &format!("failed: {}", e.message));
                    println!(
                        "{}",
                        subagent_start_json(&subagent_failed_context(&e.message))
                    );
                    ExitCode::Ok
                }
                Event::ToolUse if subagent_bash => {
                    audit(&home, &input, &format!("denied: {}", e.message));
                    println!("{}", deny_json(&e.message));
                    ExitCode::Ok
                }
                _ => {
                    cursor_ack(input.host);
                    ExitCode::Ok
                }
            });
        }
    };
    match input.event {
        Event::Start => match session::up(&out.quiet(), &r) {
            Ok(res) => {
                if r.git.is_some() && input.host == Host::Claude {
                    write_exported(&home, &r.key, res.branch.as_str());
                    if let Some(scope) = &r.key.scope {
                        let _ = std::fs::write(scope_path(&home, &r.key), scope);
                    }
                }
                emit_start(input.host, &env, &res);
                audit(&home, &input, &format!("session branch {}", res.branch));
                Ok(ExitCode::Ok)
            }
            Err(e) => {
                let reason = reason_of(&e);
                audit(&home, &input, &format!("failed: {reason}"));
                Ok(emit_failure(
                    input.host,
                    &env,
                    r.isolation_enabled(),
                    &reason,
                ))
            }
        },
        Event::Activity => {
            touch_quietly(&r);
            cursor_ack(input.host);
            Ok(ExitCode::Ok)
        }
        Event::ToolUse => Ok(tool_use(out, &home, &env, &input, &r)),
        Event::SubagentStart => {
            if input.agent_id.is_none() {
                // No usable id: nothing to key a branch on. Say so and stay quiet.
                let why = input
                    .agent_id_error
                    .clone()
                    .unwrap_or_else(|| "payload has no agent_id".to_string());
                audit(&home, &input, &format!("ignored: {why}"));
                return Ok(ExitCode::Ok);
            }
            Ok(subagent_start(out, &home, &input, &r))
        }
        Event::SubagentStop => {
            if input.agent_id.is_none() {
                audit(&home, &input, "ignored: no agent_id");
                return Ok(ExitCode::Ok);
            }
            Ok(subagent_stop(&home, &input, &r))
        }
        Event::End => {
            match session::down(&r, false) {
                Ok(d) => audit(
                    &home,
                    &input,
                    &format!(
                        "retired {} (terminated {} backend(s))",
                        d.branch.as_deref().unwrap_or("-"),
                        d.backends_terminated
                    ),
                ),
                Err(e) => notice(&e.message),
            }
            Ok(ExitCode::Ok)
        }
        Event::Ignore => {
            cursor_ack(input.host);
            Ok(ExitCode::Ok)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routed_command_wraps_the_original_as_one_quoted_argument() {
        let original =
            "cat <<'EOF' | psql \"$DATABASE_URL\"\nselect 'it''s';\nEOF\necho done; exit 3";
        let c = routed_command(
            Path::new("/opt/pg x/bin/pgx"),
            Path::new("/bin/zsh"),
            Path::new("/work/my app"),
            "agent-abc123",
            "agent-claude-x-y",
            original,
        );
        assert_eq!(
            c,
            "'/opt/pg x/bin/pgx' agent exec --agent agent-abc123 --cwd '/work/my app' -- /bin/zsh -c 'cat <<'\\''EOF'\\'' | psql \"$DATABASE_URL\"\nselect '\\''it'\\'''\\''s'\\'';\nEOF\necho done; exit 3' # pgx-routed agent-claude-x-y"
        );
        assert!(is_routed(&c));
        assert!(!is_routed(original));
        assert!(!is_routed("echo '# pgx-routed x'"));
        // A plain path needs no quotes, so a prefix permission rule can name it.
        let plain = routed_command(
            Path::new("/Users/alex/.local/bin/pgx"),
            Path::new("/bin/sh"),
            Path::new("/work/app"),
            "abc",
            "b",
            "ls",
        );
        assert!(plain.starts_with("/Users/alex/.local/bin/pgx agent exec --agent abc --cwd /work/app -- /bin/sh -c 'ls'"), "{plain}");
        // A POSIX shell parsing that argument gets the original back verbatim.
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf '%s' {}", sq(original)))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), original);
    }

    #[test]
    fn user_shell_accepts_only_absolute_bourne_shells() {
        let mut env = HashMap::new();
        assert_eq!(user_shell(&env), Path::new("/bin/sh"));
        env.insert("SHELL".to_string(), "/bin/bash".to_string());
        assert_eq!(user_shell(&env), Path::new("/bin/bash"));
        env.insert("SHELL".to_string(), "zsh".to_string());
        assert_eq!(user_shell(&env), Path::new("/bin/sh"));
        env.insert("SHELL".to_string(), "/usr/bin/python3".to_string());
        assert_eq!(user_shell(&env), Path::new("/bin/sh"));
        env.insert("SHELL".to_string(), "/nonexistent/bash".to_string());
        assert_eq!(user_shell(&env), Path::new("/bin/sh"));
    }

    #[test]
    fn hook_json_shapes() {
        let d = deny_json("runtime stopped");
        assert_eq!(d["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(d["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(d["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .starts_with("PGX: "));
        let input = json!({"command": "ls", "description": "list", "timeout": 1000, "run_in_background": false});
        let r = routed_json(
            &input,
            "/x/pgx agent exec --agent a --cwd /p -- /bin/sh -c 'ls' # pgx-routed b".into(),
        );
        let u = &r["hookSpecificOutput"]["updatedInput"];
        assert_eq!(u["description"], "list");
        assert_eq!(u["timeout"], 1000);
        assert_eq!(u["run_in_background"], false);
        assert!(u["command"].as_str().unwrap().contains(" -c 'ls' "));
        assert!(r["hookSpecificOutput"].get("permissionDecision").is_none());
        let s = subagent_start_json("ctx");
        assert_eq!(s["hookSpecificOutput"]["hookEventName"], "SubagentStart");
        assert_eq!(s["hookSpecificOutput"]["additionalContext"], "ctx");
    }
}
