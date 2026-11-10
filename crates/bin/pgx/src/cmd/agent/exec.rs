//! `pgx agent exec -- <command>`: a branch for the lifetime of one process.
//! The universal adapter: Codex, any CLI agent, a test suite, a script. The
//! wrapper is the parent of the command, so it can set the command's
//! environment without touching anyone else's.
use super::session::{self, ResolveInput};
use super::ExecOpts;
use crate::cli::ExitCode;
use crate::errors::{PgxError, Result};
use crate::out::Out;
use serde_json::json;
use std::io::Write;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

static GOT_SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_signal(sig: libc::c_int) {
    GOT_SIGNAL.store(sig, Ordering::SeqCst);
}

fn install_handlers() {
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: a plain handler that stores an integer.
        unsafe {
            libc::signal(sig, on_signal as *const () as libc::sighandler_t);
        }
    }
}

/// The host label of a branch `exec` makes for itself. Always `exec` unless
/// `--host` says otherwise: the label is part of the branch's identity, and
/// deriving it from the command name made `pgx agent down --id X` unable to
/// find a branch made by `pgx agent exec --id X -- sh -c …`.
pub const EXEC_HOST: &str = "exec";

fn random_id() -> String {
    let mut buf = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut buf);
    } else {
        let t = crate::fsutil::now_epoch() ^ (std::process::id() as u64);
        buf = t.to_le_bytes();
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn run(out: &Out, opts: ExecOpts) -> Result<ExitCode> {
    let program = opts.command[0].clone();
    // Attach to the surrounding agent session (or subagent: a routed command
    // carries PGX_AGENT_ID) when one is visible, otherwise this run is its
    // own session.
    let attached = opts.id.is_none()
        && ["CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID"]
            .iter()
            .any(|v| std::env::var(v).is_ok_and(|s| !s.is_empty()));
    let (id, host) = if attached {
        (None, opts.host.clone())
    } else {
        (
            Some(opts.id.clone().unwrap_or_else(random_id)),
            Some(opts.host.clone().unwrap_or_else(|| EXEC_HOST.to_string())),
        )
    };
    let r = session::resolve(&ResolveInput {
        id,
        host,
        agent_id: opts.agent.clone(),
        source: opts.source.clone(),
        ttl: opts.ttl.clone(),
        cwd: opts.cwd.clone(),
        ..Default::default()
    })?;
    // PGX Git, any host: a shell script that switches branches and then
    // keeps going would run on the previous branch's database. Refuse it
    // before anything runs (Claude Code's hook refuses it earlier too).
    if r.git.is_some() {
        if let Some(script) = crate::cmd::git::guard::shell_script(&opts.command) {
            let dir = opts
                .cwd
                .clone()
                .map(Ok)
                .unwrap_or_else(std::env::current_dir)
                .unwrap_or_default();
            if let Some(reason) = crate::cmd::git::guard::check(script, &dir) {
                return Err(PgxError::usage(format!(
                    "PGX refused the command: {reason}"
                )));
            }
        }
    }
    let started = Instant::now();
    let res = session::up(&out.quiet(), &r)?;
    let provision_ms = started.elapsed().as_secs_f64() * 1000.0;
    for w in &res.guard_warnings {
        eprintln!("pgx agent exec: guard not applied on {}: {w}", res.branch);
    }

    let mut cmd = std::process::Command::new(&program);
    cmd.args(&opts.command[1..]);
    // The password travels in the environment, never in an argument. An
    // inherited PGSERVICE could redirect libpq elsewhere: cleared.
    for (k, v) in session::env_pairs(&res) {
        cmd.env(k, v);
    }
    for k in ["PGSERVICE", "PGSERVICEFILE"] {
        cmd.env_remove(k);
    }
    cmd.env("PGX_AGENT_SESSION", &r.key.session_id);
    if r.key.agent_id.is_some() {
        cmd.env("PGX_ROUTED", "1");
    }
    install_handlers();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            cleanup(&r, attached, opts.keep);
            return Err(PgxError::failure(format!("cannot run {program}: {e}")));
        }
    };

    // Wait, forwarding signals and keeping the idle TTL alive.
    // Beat well inside the TTL: a quarter of it (at least 30 s) for normal
    // TTLs, half of it for very short ones, so the supervisor never removes
    // the branch under a running command.
    let idle = r.idle_secs();
    let heartbeat = Duration::from_secs(if idle >= 120 {
        (idle / 4).max(30)
    } else {
        (idle / 2).max(1)
    });
    let mut last_beat = Instant::now();
    let mut forwarded = 0;
    let status = loop {
        if let Some(st) = child
            .try_wait()
            .map_err(|e| PgxError::failure(format!("wait: {e}")))?
        {
            break st;
        }
        let sig = GOT_SIGNAL.load(Ordering::SeqCst);
        if sig != 0 && sig != forwarded {
            // SAFETY: kill(2) on our own child.
            unsafe {
                libc::kill(child.id() as libc::pid_t, sig);
            }
            forwarded = sig;
        }
        if last_beat.elapsed() >= heartbeat {
            let _ = session::touch(&r);
            last_beat = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    use std::os::unix::process::ExitStatusExt;
    let code = match (status.code(), status.signal()) {
        (Some(c), _) => c,
        (None, Some(s)) => 128 + s,
        _ => 1,
    };
    let outcome = cleanup(&r, attached, opts.keep);
    if out.json {
        println!(
            "{}",
            json!({
                "branch": res.branch.as_str(),
                "session_id": r.key.session_id,
                "agent_id": r.key.agent_id,
                "host": r.key.host,
                "source": res.source,
                // `created`: this run cloned the branch; otherwise it attached
                // to one that existed (a session's, or an earlier --keep run's).
                "created": res.created,
                "fresh": res.fresh,
                "method": res.method,
                // Wall time of ensuring the branch, clone included; only
                // meaningful when it was created, so null otherwise.
                "elapsed_ms": if res.created { Some((provision_ms * 10.0).round() / 10.0) } else { None },
                "exit_code": code,
                "attached": attached,
                "dropped": outcome == Outcome::Dropped,
                "retired": false,
                "status": outcome.status(attached),
            })
        );
    }
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Still active, on its idle TTL: an attached session's branch, or `--keep`.
    Kept,
    Dropped,
}

impl Outcome {
    fn status(self, attached: bool) -> &'static str {
        match (self, attached) {
            (Outcome::Dropped, _) => "dropped",
            (Outcome::Kept, true) => "attached",
            (Outcome::Kept, false) => "active",
        }
    }
}

/// After the command: leave an attached session's branch alone; with
/// `--keep`, leave the branch active for its idle TTL (the next `exec` with
/// the same id attaches to it, data intact); drop it otherwise. Failures are
/// reported, never fatal (the command's exit status is what the caller is
/// waiting for; the TTL is the backstop).
fn cleanup(r: &session::Resolved, attached: bool, keep: bool) -> Outcome {
    if attached {
        return Outcome::Kept;
    }
    if keep {
        if let Err(e) = session::touch(r) {
            eprintln!("pgx agent exec: could not extend the branch: {e}");
        }
        return Outcome::Kept;
    }
    match session::down(r, true) {
        Ok(d) if d.dropped => Outcome::Dropped,
        Ok(_) => Outcome::Kept,
        Err(e) => {
            eprintln!("pgx agent exec: could not drop the branch: {e}");
            Outcome::Kept
        }
    }
}
