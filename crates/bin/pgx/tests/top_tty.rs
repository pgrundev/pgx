//! Pseudo-terminal tests for the interactive `pgx top`: the real binary on a
//! real PTY (libc `openpty`), driven by keystrokes and window resizes, with
//! bounded timeouts. They check what the user sees (strings on the screen)
//! and that the terminal is restored — alternate screen left, cursor shown,
//! canonical mode and echo back on — after `q`, Ctrl-C and SIGTERM.
mod common;
use common::Pgx;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Tty {
    child: Child,
    master: std::fs::File,
    /// Kept open so the terminal settings can be read after the child exits.
    slave: OwnedFd,
    out: Arc<Mutex<Vec<u8>>>,
}

fn winsize(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

fn set_cloexec(fd: i32) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
    }
}

impl Tty {
    fn spawn(mut cmd: Command, cols: u16, rows: u16) -> Tty {
        let mut m: libc::c_int = -1;
        let mut s: libc::c_int = -1;
        let ws = winsize(cols, rows);
        // Under the harness's spawn lock: the PTY fds must be close-on-exec
        // before any other test thread forks.
        let child = {
            let _g = common::spawn_guard();
            let rc = unsafe {
                libc::openpty(
                    &mut m,
                    &mut s,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &ws as *const _ as *mut _,
                )
            };
            assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
            set_cloexec(m);
            set_cloexec(s);
            let slave = unsafe { OwnedFd::from_raw_fd(s) };
            let io = |fd: &OwnedFd| Stdio::from(fd.try_clone().unwrap());
            cmd.stdin(io(&slave))
                .stdout(io(&slave))
                .stderr(io(&slave))
                .env("TERM", "xterm-256color");
            unsafe {
                use std::os::unix::process::CommandExt;
                cmd.pre_exec(|| {
                    // A new session with the PTY as its controlling terminal,
                    // so SIGWINCH and job control behave as in a real shell.
                    if libc::setsid() < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = cmd.spawn().expect("spawn pgx top on a pty");
            (child, slave)
        };
        let (child, slave) = child;
        let master = unsafe { std::fs::File::from_raw_fd(m) };
        let out = Arc::new(Mutex::new(Vec::new()));
        let mut reader = master.try_clone().unwrap();
        let sink = out.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => sink.lock().unwrap().extend_from_slice(&buf[..n]),
                }
            }
        });
        Tty {
            child,
            master,
            slave,
            out,
        }
    }

    fn raw(&self) -> Vec<u8> {
        self.out.lock().unwrap().clone()
    }

    /// The output with escape sequences removed (what a human reads, roughly).
    fn text(&self) -> String {
        strip_ansi(&String::from_utf8_lossy(&self.raw()))
    }

    fn send(&mut self, s: &str) {
        self.master.write_all(s.as_bytes()).unwrap();
        self.master.flush().unwrap();
    }

    /// Wait until the screen output (after `from` bytes) contains `needle`.
    fn expect(&self, needle: &str, from: usize, secs: u64) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let raw = self.raw();
            let tail = strip_ansi(&String::from_utf8_lossy(&raw[from.min(raw.len())..]));
            if tail.contains(needle) {
                return;
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for {needle:?}; screen text:\n{}", tail);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn mark(&self) -> usize {
        self.out.lock().unwrap().len()
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let ws = winsize(cols, rows);
        unsafe {
            libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &ws);
            libc::kill(self.child.id() as i32, libc::SIGWINCH);
        }
    }

    fn wait_exit(&mut self, secs: u64) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(st) = self.child.try_wait().unwrap() {
                // Let the reader drain the last bytes.
                std::thread::sleep(Duration::from_millis(150));
                return st;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!(
                    "pgx top did not exit within {secs}s; screen:\n{}",
                    self.text()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The PTY's local modes, read through the master (after the session
    /// leader exits, macOS revokes the slave descriptor).
    fn lflag(&self) -> libc::tcflag_t {
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::tcgetattr(self.master.as_raw_fd(), &mut t) };
        let rc = if rc == 0 {
            rc
        } else {
            unsafe { libc::tcgetattr(self.slave.as_raw_fd(), &mut t) }
        };
        assert_eq!(rc, 0, "tcgetattr: {}", std::io::Error::last_os_error());
        t.c_lflag
    }

    /// Terminal state after exit: canonical mode and echo are back.
    fn assert_restored(&self) {
        let t = libc::termios {
            c_lflag: self.lflag(),
            ..unsafe { std::mem::zeroed() }
        };
        assert!(
            t.c_lflag & libc::ICANON != 0,
            "raw mode left on (ICANON off)"
        );
        assert!(t.c_lflag & libc::ECHO != 0, "echo left off");
        let raw = String::from_utf8_lossy(&self.raw()).into_owned();
        let enter = raw
            .find("\x1b[?1049h")
            .expect("entered the alternate screen");
        let leave = raw.rfind("\x1b[?1049l").expect("left the alternate screen");
        assert!(leave > enter);
        let show = raw.rfind("\x1b[?25h").expect("cursor shown again");
        assert!(show > enter);
    }
}

impl Drop for Tty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('[') => {
                // CSI: parameters then one final byte in @..~
                for d in it.by_ref() {
                    if ('@'..='~').contains(&d) {
                        break;
                    }
                }
                // Cursor moves split words; keep a separator so lines stay apart.
                out.push(' ');
            }
            Some(']') => {
                // OSC: until BEL or ST
                while let Some(d) = it.next() {
                    if d == '\x07' {
                        break;
                    }
                    if d == '\x1b' && it.peek() == Some(&'\\') {
                        it.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn demo_cmd(p: &Pgx, home: &std::path::Path, extra: &[&str]) -> Command {
    let mut args = vec!["top", "--demo"];
    args.extend_from_slice(extra);
    let mut c = p.cmd(&args);
    c.env("PGX_HOME", home);
    c
}

#[test]
fn demo_session_keys_resize_and_quit() {
    let p = Pgx::new();
    let home = p.home.join("no-home-for-demo");
    let mut t = Tty::spawn(demo_cmd(&p, &home, &[]), 100, 30);
    t.expect("PGX TOP", 0, 15);
    t.expect("DEMO / SIMULATED", 0, 5);
    // The simulator brings the agents in over the first seconds.
    t.expect("feature-auth", 0, 20);
    t.expect("agent-a", 0, 20);
    t.expect("Claude Code", 0, 5);
    // Navigation + inspector.
    let m = t.mark();
    t.send("jjj");
    t.send("\r");
    t.expect("DETAILS", m, 5);
    t.send("\x1b");
    std::thread::sleep(Duration::from_millis(150));
    // Search: type, see the query, Enter keeps it.
    let m = t.mark();
    t.send("/agent-b");
    t.expect("/agent-b", m, 5);
    t.send("\r");
    t.send("\x1b"); // Esc clears the filter
                    // Flat view, pause, help.
    let m = t.mark();
    t.send("t");
    t.send("p");
    t.expect("PAUSED", m, 5);
    t.send("p");
    let m = t.mark();
    t.send("?");
    t.expect("keys (any key closes)", m, 5);
    t.send(" ");
    // Resize: compact, then wide.
    let m = t.mark();
    t.resize(40, 12);
    t.expect("PGX TOP", m, 5);
    let m = t.mark();
    t.resize(130, 40);
    t.expect("LIFECYCLE", m, 5);
    t.send("q");
    let st = t.wait_exit(10);
    assert_eq!(st.code(), Some(0), "screen:\n{}", t.text());
    t.assert_restored();
    assert!(!home.exists(), "--demo never creates a PGX home");
}

#[test]
fn ctrl_c_quits_cleanly() {
    let p = Pgx::new();
    let home = p.home.join("absent");
    let mut t = Tty::spawn(demo_cmd(&p, &home, &["--no-color"]), 80, 24);
    t.expect("PGX TOP", 0, 15);
    // The check below is meaningful: while running, the PTY is in raw mode.
    assert_eq!(
        t.lflag() & (libc::ICANON | libc::ECHO),
        0,
        "raw mode while running"
    );
    t.send("\x03");
    let st = t.wait_exit(10);
    assert_eq!(st.code(), Some(0));
    t.assert_restored();
    // --no-color: no colour SGR (30-37/90-97 foregrounds, 40-47 backgrounds).
    let raw = String::from_utf8_lossy(&t.raw()).into_owned();
    for code in [
        "\x1b[32m",
        "\x1b[31m",
        "\x1b[33m",
        "\x1b[36m",
        "\x1b[38;5;",
        "\x1b[43m",
    ] {
        assert!(!raw.contains(code), "colour {code:?} with --no-color");
    }
}

#[test]
fn sigterm_restores_the_terminal() {
    let p = Pgx::new();
    let home = p.home.join("absent");
    let mut t = Tty::spawn(demo_cmd(&p, &home, &[]), 80, 24);
    t.expect("PGX TOP", 0, 15);
    unsafe {
        libc::kill(t.child.id() as i32, libc::SIGTERM);
    }
    let st = t.wait_exit(10);
    assert!(st.success(), "{st:?}");
    t.assert_restored();
}

#[test]
fn hangup_of_the_terminal_exits() {
    // The terminal goes away (an ssh disconnect): the monitor must exit, not spin.
    let p = Pgx::new();
    let home = p.home.join("absent");
    let mut t = Tty::spawn(demo_cmd(&p, &home, &[]), 80, 24);
    t.expect("PGX TOP", 0, 15);
    unsafe {
        libc::kill(t.child.id() as i32, libc::SIGHUP);
    }
    // Exiting at all (within the bound) is the assertion: wait_exit panics otherwise.
    let _ = t.wait_exit(10);
}

#[test]
fn live_runtime_smoke_on_a_pty() {
    let p = Pgx::new();
    p.init_ok();
    p.start_ok();
    assert_eq!(p.run(&["create", "smoke_db"]).code, 0);
    let mut t = Tty::spawn(p.cmd(&["top", "--interval-ms", "250"]), 120, 32);
    t.expect("PGX TOP", 0, 15);
    t.expect("RUNNING", 0, 10);
    t.expect("smoke_db", 0, 10);
    assert!(
        !t.text().contains("DEMO"),
        "live mode is never labelled demo"
    );
    // A database created while the monitor is open shows up without a key press.
    let m = t.mark();
    assert_eq!(p.run(&["create", "appeared_later"]).code, 0);
    t.expect("appeared_later", m, 10);
    t.expect("database created", m, 10);
    // Stopping the runtime is shown, and the monitor keeps running.
    let m = t.mark();
    p.stop_ok();
    t.expect("STOPPED", m, 15);
    t.send("q");
    assert_eq!(t.wait_exit(10).code(), Some(0));
    t.assert_restored();
}
