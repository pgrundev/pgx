//! `pgboxdb start` / `pgboxdb restart`.
use super::Ctx;
use crate::cli::ExitCode;
use crate::cmd::init::port_check;
use crate::errors::{PgboxdbError, Result};
use crate::out::Out;
use crate::{conf, fsdetect, process};
use std::os::fd::AsRawFd;
use std::time::Duration;

/// Serialises concurrent `pgboxdb start`s on one home: the second waits for the
/// first and then finds the engine running.
pub struct StartLock(#[allow(dead_code)] std::fs::File);

impl StartLock {
    pub fn acquire(ctx: &Ctx) -> Result<StartLock> {
        let path = ctx.home.root.join("start.lock");
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| PgboxdbError::io("cannot open", &path, &e))?;
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(PgboxdbError::failure(format!(
                "cannot lock {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            )));
        }
        Ok(StartLock(f))
    }
}

/// The result of ensuring the runtime is up, for callers that compose (`pgboxdb run`).
pub struct StartOutcome {
    pub started: bool,
    pub already_running: bool,
    pub pid: u32,
}

pub fn run(out: &Out) -> Result<ExitCode> {
    let ctx = Ctx::load()?;
    let outcome = ensure_running(out, &ctx)?;
    emit_start(out, &ctx, &outcome, false);
    Ok(ExitCode::Ok)
}

pub fn restart(out: &Out) -> Result<ExitCode> {
    let ctx = Ctx::load()?;
    let _lock = StartLock::acquire(&ctx)?;
    if let Some(pid) = ctx.running_pid() {
        out.note(&format!("stopping pid {pid}"));
        process::stop(pid, process::StopMode::Fast, Duration::from_secs(60), None)?;
    }
    let outcome = start_locked(out, &ctx)?;
    emit_start(out, &ctx, &outcome, true);
    Ok(ExitCode::Ok)
}

/// Acquire the start lock and start the runtime if it is not already up.
/// Idempotent; serialises concurrent starts; does not emit, so a composite
/// command (`pgboxdb run`) can report the whole operation once.
pub fn ensure_running(out: &Out, ctx: &Ctx) -> Result<StartOutcome> {
    let _lock = StartLock::acquire(ctx)?;
    start_locked(out, ctx)
}

fn start_locked(out: &Out, ctx: &Ctx) -> Result<StartOutcome> {
    let sock_dir = ctx.sock_dir();
    if let Some(pid) = ctx.running_pid() {
        return Ok(StartOutcome {
            started: false,
            already_running: true,
            pid,
        });
    }
    let pid_file = ctx.home.data.join("postmaster.pid");
    if pid_file.exists() {
        out.note("removing a stale postmaster.pid");
        std::fs::remove_file(&pid_file)
            .map_err(|e| PgboxdbError::io("cannot remove", &pid_file, &e))?;
    }
    // Clear a socket lock left by an engine that is gone (its PID not alive,
    // zombie-aware). On a host whose PID 1 reaps children the engine does this
    // itself; in a bare container it does not, and the stale lock blocks start.
    process::clear_stale_socket_lock(&sock_dir, ctx.config.port);
    // Retry the probe briefly: a genuine conflict persists, but a socket lingering
    // from an engine we just stopped or killed clears within the kernel's cleanup
    // window (the engine itself binds with SO_REUSEADDR and would not hit it).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match port_check(ctx.config.port) {
            Ok(()) => break,
            Err(why) => {
                if std::time::Instant::now() >= deadline {
                    return Err(PgboxdbError::new(ExitCode::Conflict, format!("port {} is in use ({why})", ctx.config.port))
                        .hint("stop the other process, or re-initialise on another port with `pgboxdb init --force --port N`"));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
    // pgboxdb.conf is regenerated from config.json on every start.
    let block_clone = fsdetect::supports_block_clone(&ctx.home.data);
    let pgboxdb_conf = ctx.home.data.join("pgboxdb.conf");
    std::fs::write(
        &pgboxdb_conf,
        conf::render_pgboxdb_conf(&ctx.config, &sock_dir, block_clone),
    )
    .map_err(|e| PgboxdbError::io("cannot write", &pgboxdb_conf, &e))?;

    let mut child = process::spawn(&ctx.install, &ctx.home, &ctx.config, &sock_dir, &[])?;
    let pid = child.id();
    match process::wait_ready(
        &ctx.home,
        &sock_dir,
        ctx.config.port,
        Some(&mut child),
        Duration::from_secs(60),
    ) {
        Ok(client) => client.close(),
        Err(e) => {
            let _ = process::stop(
                pid,
                process::StopMode::Force,
                Duration::from_secs(10),
                Some(&mut child),
            );
            let _ = std::fs::remove_file(&pid_file);
            return Err(e);
        }
    }
    // Bring up the background TTL cleanup worker (single-instance; a redundant
    // spawn exits immediately). It runs detached and outlives this CLI process.
    spawn_supervisor(&ctx.home);
    // The child lives on in its own session; the CLI exits without reaping it.
    Ok(StartOutcome {
        started: true,
        already_running: false,
        pid,
    })
}

/// Render the outcome of `pgboxdb start` / `pgboxdb restart` in human and JSON form.
fn emit_start(out: &Out, ctx: &Ctx, o: &StartOutcome, restarted: bool) {
    let sock_dir = ctx.sock_dir();
    if o.already_running {
        out.emit(
            &format!(
                "PGBoxDB is already running (pid {}) on port {}",
                o.pid, ctx.config.port
            ),
            serde_json::json!({"started": false, "already_running": true, "pid": o.pid, "port": ctx.config.port, "socket_dir": sock_dir}),
        );
        return;
    }
    let url = format!(
        "postgres://postgres:***@127.0.0.1:{}/postgres",
        ctx.config.port
    );
    out.emit(
        &format!(
            "{} PGBoxDB (pid {}) on port {}\n  socket  {}\n  url     {url}   (`pgboxdb url <database>` shows the password)\n  log     {}",
            if restarted { "restarted" } else { "started" },
            o.pid,
            ctx.config.port,
            sock_dir.display(),
            ctx.home.log_file.display()
        ),
        serde_json::json!({"started": true, "already_running": false, "pid": o.pid, "port": ctx.config.port, "socket_dir": sock_dir, "log": ctx.home.log_file}),
    );
}

/// `pgboxdb serve`: run the engine in the FOREGROUND, replacing this process, so it
/// can be PID 1 in a container or a systemd simple service. Initialises first
/// if $PGBOXDB_HOME is empty, reading PGBOXDB_PORT / PGBOXDB_DURABILITY / PGBOXDB_LISTEN like
/// Spawn the background TTL supervisor, detached in its own session so it
/// outlives this CLI invocation and survives a terminal hangup. Single-instance
/// by its own lock, so a redundant spawn simply exits; callers need not check.
pub fn spawn_supervisor(home: &crate::layout::Home) {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.arg("supervise")
        .env("PGBOXDB_HOME", &home.root)
        .stdin(Stdio::null());
    if let Some(parent) = home.log_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&home.log_file)
    {
        Ok(f) => match f.try_clone() {
            Ok(f2) => {
                cmd.stdout(Stdio::from(f)).stderr(Stdio::from(f2));
            }
            Err(_) => {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
        },
        Err(_) => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    // SAFETY: setsid() and fcntl() in the child only; no allocation or
    // shared-state access. Inherited descriptors are dropped at exec so the
    // supervisor never pins a caller's pipe or socket open.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            crate::process::cloexec_inherited_fds();
            Ok(())
        });
    }
    let _ = cmd.spawn();
}

/// Stop the background TTL supervisor for this home, if one is running. The
/// supervisor is tied to the runtime's lifetime, so `pgboxdb stop` ends it
/// deterministically rather than leaving it to notice the engine is gone on its
/// next poll. Doing it promptly also frees the single-instance lock, so a
/// relocated or quickly-restarted runtime can spawn a fresh supervisor without
/// racing a lingering one for the lock. Sends SIGTERM, waits briefly for exit,
/// and removes the pid file.
pub fn stop_supervisor(home: &crate::layout::Home) {
    let pid_path = home.root.join("supervisor.pid");
    let Ok(text) = std::fs::read_to_string(&pid_path) else {
        return;
    };
    if let Ok(pid) = text.trim().parse::<i32>() {
        if pid > 1 && unsafe { libc::kill(pid, 0) } == 0 {
            unsafe { libc::kill(pid, libc::SIGTERM) };
            // Wait up to ~3s for it to exit so the lock is released before we return.
            for _ in 0..30 {
                if unsafe { libc::kill(pid, 0) } != 0 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    let _ = std::fs::remove_file(&pid_path);
}

/// the Docker entrypoint expects.
pub fn serve(out: &Out) -> Result<ExitCode> {
    use crate::layout::Home;
    let home = Home::resolve();
    if !home.is_initialized() {
        let port = std::env::var("PGBOXDB_PORT").ok().and_then(|p| p.parse().ok());
        let durability = std::env::var("PGBOXDB_DURABILITY").ok();
        out.note("pgboxdb serve: initialising a new cluster");
        crate::cmd::init::run(out, port, durability, false)?;
    }
    let ctx = Ctx::load()?;
    if let Some(pid) = ctx.running_pid() {
        return Err(PgboxdbError::new(
            ExitCode::Conflict,
            format!("PGBoxDB is already running (pid {pid})"),
        )
        .hint("`pgboxdb serve` runs the engine in the foreground; stop the background runtime first"));
    }
    let sock_dir = ctx.sock_dir();
    let pid_file = ctx.home.data.join("postmaster.pid");
    if pid_file.exists() {
        let _ = std::fs::remove_file(&pid_file);
    }
    crate::process::clear_stale_socket_lock(&sock_dir, ctx.config.port);
    let block_clone = fsdetect::supports_block_clone(&ctx.home.data);
    let pgboxdb_conf = ctx.home.data.join("pgboxdb.conf");
    std::fs::write(
        &pgboxdb_conf,
        conf::render_pgboxdb_conf(&ctx.config, &sock_dir, block_clone),
    )
    .map_err(|e| PgboxdbError::io("cannot write", &pgboxdb_conf, &e))?;
    std::fs::create_dir_all(&sock_dir).map_err(|e| PgboxdbError::io("cannot create", &sock_dir, &e))?;
    // Start the TTL worker before we exec the engine (this process is replaced).
    // It polls until the engine accepts connections, then cleans up normally.
    spawn_supervisor(&ctx.home);
    out.note(&format!(
        "pgboxdb serve: exec engine on port {} (socket {})",
        ctx.config.port,
        sock_dir.display()
    ));
    exec_engine_foreground(&ctx.install, &ctx.home, &ctx.config, &sock_dir)
}

/// Replaces this process with the engine (execvp). Returns only on failure.
fn exec_engine_foreground(
    install: &crate::layout::Install,
    home: &crate::layout::Home,
    cfg: &crate::config::Config,
    sock_dir: &std::path::Path,
) -> Result<ExitCode> {
    use std::os::unix::process::CommandExt;
    // Raise the stack soft limit for the engine, like process::spawn does.
    let mut rl = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_STACK, &mut rl) } == 0 {
        let want = crate::process::STACK_BYTES as libc::rlim_t;
        if rl.rlim_cur < want && (rl.rlim_max == libc::RLIM_INFINITY || rl.rlim_max >= want) {
            rl.rlim_cur = want;
            unsafe { libc::setrlimit(libc::RLIMIT_STACK, &rl) };
        }
    }
    // Same hygiene as the detached engine: nothing we inherited goes on to the
    // engine (our own descriptors are already close-on-exec).
    unsafe { crate::process::cloexec_inherited_fds() };
    let err = std::process::Command::new(&install.engine)
        .args(crate::process::engine_args(home, cfg, sock_dir))
        .envs(crate::process::engine_env(install))
        .exec();
    Err(PgboxdbError::failure(format!(
        "cannot exec the engine {}: {err}",
        install.engine.display()
    )))
}
