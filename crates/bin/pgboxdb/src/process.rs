//! The engine process: spawn detached, wait until it answers, stop it, and
//! tell whether a pid in postmaster.pid is still ours.
use crate::cli::ExitCode;
use crate::config::Config;
use crate::errors::{PgboxdbError, Result};
use crate::layout::{Home, Install};
use crate::pgwire::{Client, WireError};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const STACK_BYTES: u64 = 65520 * 1024;
pub const RUST_MIN_STACK: &str = "33554432";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PidFile {
    pub pid: u32,
    pub data_dir: PathBuf,
    pub port: Option<u16>,
    pub sock_dir: Option<PathBuf>,
}

/// postmaster.pid: pid, data dir, start time, port, socket dir, listen addr, shmem key, status.
pub fn read_pid_file(data: &Path) -> Option<PidFile> {
    let text = std::fs::read_to_string(data.join("postmaster.pid")).ok()?;
    let mut lines = text.lines();
    let pid = lines.next()?.trim().parse().ok()?;
    let data_dir = PathBuf::from(lines.next()?.trim());
    let _start = lines.next();
    let port = lines.next().and_then(|l| l.trim().parse().ok());
    let sock_dir = lines
        .next()
        .map(|l| PathBuf::from(l.trim()))
        .filter(|p| !p.as_os_str().is_empty());
    Some(PidFile {
        pid,
        data_dir,
        port,
        sock_dir,
    })
}

pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let r = unsafe { libc::kill(pid as i32, 0) };
    let exists = r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    // A zombie still "exists" for kill(2) until its parent reaps it. The engine
    // has already exited, so for liveness it is stopped. This matters when the
    // engine's parent (the detached `pgboxdb start`) is gone and PID 1 does not reap
    // children — common in containers without an init and in CI.
    exists && !is_zombie(pid)
}

#[cfg(target_os = "linux")]
fn is_zombie(pid: u32) -> bool {
    // /proc/<pid>/stat: "<pid> (<comm>) <state> ...". comm may contain spaces
    // and parentheses, so read the state after the LAST ')'.
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => match stat.rfind(')') {
            Some(i) => stat[i + 1..].trim_start().starts_with('Z'),
            None => false,
        },
        Err(_) => false,
    }
}

#[cfg(not(target_os = "linux"))]
fn is_zombie(_pid: u32) -> bool {
    false
}

/// The process's command line, or None if it is gone.
pub fn command_line(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) {
            return Some(String::from_utf8_lossy(&raw).replace('\0', " "));
        }
    }
    let out = Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !s.is_empty() {
        Some(s)
    } else {
        None
    }
}

/// True when `cmd` is an engine process launched on exactly `data_dir`, i.e. it
/// carries the `-D <data_dir>` launch flag. Matching the flag (not just the path
/// as a substring) avoids treating an unrelated process whose argv merely
/// mentions the data directory (a backup, an editor) as our engine.
pub fn engine_cmdline_matches(cmd: &str, data_dir: &Path) -> bool {
    let needle = format!("-D {}", data_dir.display());
    cmd.split(&needle)
        .skip(1)
        .any(|rest| rest.is_empty() || rest.starts_with([' ', '\0']))
}

/// True when `pid` is alive and is an engine started on this data directory.
pub fn is_our_engine(pid: u32, data_dir: &Path) -> bool {
    if !pid_alive(pid) {
        return false;
    }
    match command_line(pid) {
        Some(cmd) => engine_cmdline_matches(&cmd, data_dir),
        None => false,
    }
}

pub fn engine_env(install: &Install) -> Vec<(String, String)> {
    vec![
        ("RUST_MIN_STACK".into(), RUST_MIN_STACK.into()),
        (
            "PGRUST_PGSHAREDIR".into(),
            install.share.display().to_string(),
        ),
        (
            "PGRUST_TZDIR".into(),
            install.share.join("timezone").display().to_string(),
        ),
    ]
}

pub fn engine_args(home: &Home, cfg: &Config, sock_dir: &Path) -> Vec<String> {
    vec![
        "-D".into(),
        home.data.display().to_string(),
        "-k".into(),
        sock_dir.display().to_string(),
        "-p".into(),
        cfg.port.to_string(),
        "-c".into(),
        "io_method=sync".into(),
        "-c".into(),
        "max_stack_depth=60000".into(),
    ]
}

/// Mark every descriptor above stderr close-on-exec, so a long-lived child
/// (the engine, the TTL supervisor) never keeps a caller's pipes or sockets
/// open. Everything pgboxdb opens itself is already close-on-exec (std does that),
/// so this only affects descriptors pgboxdb inherited from its parent — which it
/// has no business forwarding. Without it, a descriptor a parent is still
/// initialising when it forks us (macOS has no atomic `pipe2`/`SOCK_CLOEXEC`)
/// lives for as long as the engine does: the parent's `output()` never sees
/// EOF, and a leaked listening socket keeps its port busy.
///
/// Async-signal-safe (only `fcntl`, `close_range`/`syscall`, `sysconf`):
/// safe to call from `pre_exec`.
pub(crate) unsafe fn cloexec_inherited_fds() {
    #[cfg(target_os = "linux")]
    {
        // One syscall on Linux >= 5.11; falls through to the loop on older kernels.
        if libc::syscall(
            libc::SYS_close_range,
            3 as libc::c_uint,
            libc::c_uint::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        ) == 0
        {
            return;
        }
    }
    // Descriptors cannot exceed the soft limit; the clamp only guards an
    // absurd limit from turning this into a long loop.
    let max = libc::sysconf(libc::_SC_OPEN_MAX).clamp(3, 1 << 22) as libc::c_int;
    for fd in 3..max {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }
}

/// Starts the engine detached (own session, stdio to the log file).
pub fn spawn(
    install: &Install,
    home: &Home,
    cfg: &Config,
    sock_dir: &Path,
    extra_args: &[&str],
) -> Result<Child> {
    use std::os::unix::process::CommandExt;
    if let Some(dir) = home.log_file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| PgboxdbError::io("cannot create", dir, &e))?;
    }
    std::fs::create_dir_all(sock_dir)
        .map_err(|e| PgboxdbError::io("cannot create socket directory", sock_dir, &e))?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&home.log_file)
        .map_err(|e| PgboxdbError::io("cannot open log", &home.log_file, &e))?;
    let log2 = log
        .try_clone()
        .map_err(|e| PgboxdbError::io("cannot dup log", &home.log_file, &e))?;
    let mut cmd = Command::new(&install.engine);
    cmd.args(engine_args(home, cfg, sock_dir))
        .args(extra_args)
        .envs(engine_env(install))
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log2);
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            cloexec_inherited_fds();
            let mut rl = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(libc::RLIMIT_STACK, &mut rl) == 0 {
                let want = STACK_BYTES as libc::rlim_t;
                if rl.rlim_cur < want && (rl.rlim_max == libc::RLIM_INFINITY || rl.rlim_max >= want)
                {
                    rl.rlim_cur = want;
                    libc::setrlimit(libc::RLIMIT_STACK, &rl);
                }
            }
            Ok(())
        });
    }
    cmd.spawn().map_err(|e| {
        PgboxdbError::failure(format!(
            "cannot start the engine {}: {e}",
            install.engine.display()
        ))
    })
}

/// Removes a leftover Unix-socket lock file (and socket) for our port when the
/// postmaster it names is not alive. The engine does this itself for a dead
/// PID, but a zombie (unreaped by a non-init PID 1, as in a bare container)
/// looks alive to it and blocks startup; our liveness check sees the zombie as
/// dead, so we clear the stale files. Safe: only when the named PID is gone.
pub fn clear_stale_socket_lock(sock_dir: &Path, port: u16) {
    let lock = sock_dir.join(format!(".s.PGSQL.{port}.lock"));
    let named_pid = std::fs::read_to_string(&lock)
        .ok()
        .and_then(|t| t.lines().next().and_then(|l| l.trim().parse::<u32>().ok()));
    match named_pid {
        Some(pid) if pid_alive(pid) => {} // a live postmaster owns it; leave it
        _ => {
            let _ = std::fs::remove_file(&lock);
            let _ = std::fs::remove_file(sock_dir.join(format!(".s.PGSQL.{port}")));
        }
    }
}

pub fn log_tail(home: &Home, n: usize) -> String {
    std::fs::read_to_string(&home.log_file)
        .map(|s| {
            let lines: Vec<&str> = s.lines().collect();
            lines[lines.len().saturating_sub(n)..].join("\n")
        })
        .unwrap_or_default()
}

/// Connects repeatedly until the engine answers `SELECT 1`, the child exits, or the timeout passes.
pub fn wait_ready(
    home: &Home,
    sock_dir: &Path,
    port: u16,
    child: Option<&mut Child>,
    timeout: Duration,
) -> Result<Client<UnixStream>> {
    let start = Instant::now();
    let mut child = child;
    loop {
        if let Some(c) = child.as_deref_mut() {
            if let Ok(Some(status)) = c.try_wait() {
                return Err(PgboxdbError::failure(format!(
                    "the engine exited during startup ({status})\n--- {} (last lines)\n{}",
                    home.log_file.display(),
                    log_tail(home, 15)
                )));
            }
        }
        match Client::connect_unix(
            sock_dir,
            port,
            "postgres",
            "postgres",
            Duration::from_secs(5),
        ) {
            Ok(mut client) => {
                if client.simple_query("SELECT 1").is_ok() {
                    return Ok(client);
                }
            }
            Err(WireError::Db(e)) if e.sqlstate == "57P03" => {} // starting up
            Err(_) => {}
        }
        if start.elapsed() > timeout {
            return Err(PgboxdbError::failure(format!(
                "the engine did not become ready within {}s\n--- {} (last lines)\n{}",
                timeout.as_secs(),
                home.log_file.display(),
                log_tail(home, 15)
            )));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMode {
    /// SIGINT: disconnect sessions, checkpoint, exit.
    Fast,
    /// SIGKILL after the fast timeout.
    Force,
}

fn signal(pid: u32, sig: i32) -> Result<()> {
    if unsafe { libc::kill(pid as i32, sig) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        return Err(PgboxdbError::failure(format!(
            "cannot signal engine pid {pid}: {e}"
        )));
    }
    Ok(())
}

fn wait_gone(pid: u32, child: Option<&mut Child>, timeout: Duration) -> bool {
    let start = Instant::now();
    let mut child = child;
    loop {
        let alive = match child.as_deref_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => pid_alive(pid),
        };
        if !alive {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Stops the engine. `child` (when we spawned it in this process) lets us reap it.
pub fn stop(
    pid: u32,
    mode: StopMode,
    timeout: Duration,
    mut child: Option<&mut Child>,
) -> Result<()> {
    signal(pid, libc::SIGINT)?;
    if wait_gone(pid, child.as_deref_mut(), timeout) {
        return Ok(());
    }
    match mode {
        StopMode::Fast => Err(PgboxdbError::new(
            ExitCode::Failure,
            format!(
                "the engine (pid {pid}) did not stop within {}s",
                timeout.as_secs()
            ),
        )
        .hint("retry, or `pgboxdb stop --force` to kill it")),
        StopMode::Force => {
            signal(pid, libc::SIGKILL)?;
            if wait_gone(pid, child, Duration::from_secs(10)) {
                Ok(())
            } else {
                Err(PgboxdbError::failure(format!(
                    "the engine (pid {pid}) survived SIGKILL"
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_file_parses_postmaster_pid() {
        let dir = std::env::temp_dir().join(format!("pgboxdb-pidfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("postmaster.pid"), "4242\n/Users/x/.pgboxdb/data\n1760000000\n5433\n/Users/x/.pgboxdb/run\n127.0.0.1\n  5433001        64\nready\n").unwrap();
        let p = read_pid_file(&dir).unwrap();
        assert_eq!(p.pid, 4242);
        assert_eq!(p.data_dir, PathBuf::from("/Users/x/.pgboxdb/data"));
        assert_eq!(p.port, Some(5433));
        assert_eq!(p.sock_dir, Some(PathBuf::from("/Users/x/.pgboxdb/run")));
        assert!(read_pid_file(&dir.join("nope")).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn engine_cmdline_matches_requires_dash_d_flag() {
        let data = std::path::Path::new("/home/u/.pgboxdb/data");
        assert!(engine_cmdline_matches(
            "/opt/pgboxdb/bin/postgres -D /home/u/.pgboxdb/data -k /home/u/.pgboxdb/run -p 5432",
            data
        ));
        // an unrelated process whose argv merely contains the data dir path must NOT match
        assert!(!engine_cmdline_matches(
            "tar czf backup.tgz /home/u/.pgboxdb/data",
            data
        ));
        assert!(!engine_cmdline_matches(
            "vim /home/u/.pgboxdb/data/postgresql.conf",
            data
        ));
        // a different cluster must not match
        assert!(!engine_cmdline_matches(
            "/opt/pgboxdb/bin/postgres -D /home/u/.pgboxdb-other/data",
            data
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn zombie_is_not_alive() {
        // Fork a child that exits immediately; without waitpid it is a zombie.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            unsafe { libc::_exit(0) };
        }
        assert!(pid > 0);
        // give it a moment to exit
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !pid_alive(pid as u32),
            "an unreaped exited child (zombie) must count as not alive"
        );
        // reap it so we leave nothing behind
        let mut status = 0;
        unsafe { libc::waitpid(pid, &mut status, 0) };
    }

    #[test]
    fn own_pid_is_alive_and_not_an_engine() {
        let me = std::process::id();
        assert!(pid_alive(me));
        assert!(!is_our_engine(me, Path::new("/definitely/not/a/data/dir")));
        assert!(!pid_alive(0));
    }

    #[test]
    fn engine_args_name_data_socket_and_port() {
        let home = Home::at(PathBuf::from("/h/.pgboxdb"));
        let cfg = Config {
            port: 5455,
            password: "p".into(),
            durability: Default::default(),
            listen: vec![],
            run_dir: None,
            created_at: "t".into(),
        };
        let a = engine_args(&home, &cfg, Path::new("/h/.pgboxdb/run"));
        assert_eq!(
            a[..6],
            ["-D", "/h/.pgboxdb/data", "-k", "/h/.pgboxdb/run", "-p", "5455"]
        );
        assert!(a.contains(&"io_method=sync".to_string()));
    }
    #[test]
    fn cloexec_inherited_fds_hides_a_leaked_descriptor_from_the_child() {
        use std::os::unix::process::CommandExt;
        // A pipe WITHOUT close-on-exec, as a half-initialised descriptor in a
        // forking parent would be. The child writes a token to it through its
        // /dev/fd path rather than `>&N`: dash (Ubuntu's /bin/sh) accepts only
        // a single digit after `>&`, and N depends on how many descriptors the
        // other test threads hold open at that moment, which made this test
        // fail whenever N reached 10. The path form works in every POSIX shell
        // for any N, and fails exactly when the descriptor is absent in the child.
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let (r, w) = (fds[0], fds[1]);
        let probe = format!("echo leaked > /dev/fd/{w}");
        let without = std::process::Command::new("/bin/sh")
            .args(["-c", &probe])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(
            without.success(),
            "control: the child can use the inherited fd {w}"
        );
        // The token really arrived through our pipe.
        let mut buf = [0u8; 16];
        let n = unsafe { libc::read(r, buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(&buf[..usize::try_from(n).unwrap_or(0)], b"leaked\n");

        let mut c = std::process::Command::new("/bin/sh");
        c.args(["-c", &probe]).stderr(std::process::Stdio::null());
        unsafe {
            c.pre_exec(|| {
                cloexec_inherited_fds();
                Ok(())
            });
        }
        let with = c.status().unwrap();
        assert!(!with.success(), "the inherited fd must be closed on exec");
        // And nothing reached the pipe: a non-blocking read finds it empty
        // (the parent still holds the write end, so this is EAGAIN, not EOF).
        unsafe { libc::fcntl(r, libc::F_SETFL, libc::O_NONBLOCK) };
        let n = unsafe { libc::read(r, buf.as_mut_ptr().cast(), buf.len()) };
        assert!(n <= 0, "a token leaked through the closed descriptor");
        unsafe {
            libc::close(w);
            libc::close(r);
        }
    }
}
