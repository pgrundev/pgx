//! PostgreSQL client tools (`pg_dump`, `pg_restore`) for `pgboxdb copy`: where
//! they are, which version, and how to run one with its output streamed,
//! redacted and interruptible.
//!
//! The distribution does not bundle them (see docs/pgboxdb/local-dev/copy.md); a
//! developer who has PostgreSQL has them, and `pgboxdb doctor` reports them.
use crate::conninfo::Redactor;
use crate::errors::{PgboxdbError, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// Set by the SIGINT/SIGTERM handler installed by [`install_interrupt_handler`].
pub static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Route SIGINT and SIGTERM to a flag instead of killing the process, so a
/// long-running command can stop its children, drop its staging state and
/// exit cleanly. The terminal still delivers SIGINT to the children (same
/// process group); they exit on their own.
pub fn install_interrupt_handler() {
    // A function pointer cast through `*const ()`, as rustc's
    // `function_casts_as_integer` lint asks for.
    let handler = on_signal as *const () as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGHUP, handler);
    }
}

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tools {
    pub dir: PathBuf,
    pub pg_dump: PathBuf,
    pub pg_restore: PathBuf,
    /// e.g. "18.6"
    pub version: String,
    pub major: u32,
}

/// Directories searched after `PGBOXDB_PG_BINDIR` and `PATH`, highest version
/// first within each family.
pub fn well_known_dirs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    for base in [
        "/opt/homebrew/opt",
        "/usr/local/opt",
        "/home/linuxbrew/.linuxbrew/opt",
    ] {
        for major in (14..=20).rev() {
            v.push(PathBuf::from(format!("{base}/postgresql@{major}/bin")));
        }
        v.push(PathBuf::from(format!("{base}/libpq/bin")));
    }
    v.push(PathBuf::from(
        "/Applications/Postgres.app/Contents/Versions/latest/bin",
    ));
    for major in (14..=20).rev() {
        v.push(PathBuf::from(format!("/usr/lib/postgresql/{major}/bin")));
        v.push(PathBuf::from(format!("/usr/pgsql-{major}/bin")));
    }
    v.push(PathBuf::from("/usr/local/pgsql/bin"));
    v.push(PathBuf::from("/usr/local/bin"));
    v.push(PathBuf::from("/usr/bin"));
    v
}

/// `pg_dump (PostgreSQL) 18.6 (Homebrew)` → ("18.6", 18); `17beta1` → ("17beta1", 17).
pub fn parse_version_output(text: &str) -> Option<(String, u32)> {
    let token = text
        .split_whitespace()
        .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let major: u32 = token
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()?;
    Some((token.to_string(), major))
}

fn probe_dir(dir: &Path) -> Option<Tools> {
    let pg_dump = dir.join("pg_dump");
    let pg_restore = dir.join("pg_restore");
    if !pg_dump.is_file() || !pg_restore.is_file() {
        return None;
    }
    let out = Command::new(&pg_dump)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let (version, major) = parse_version_output(&String::from_utf8_lossy(&out.stdout))?;
    // pg_restore must be the same major, or the archive format may not match.
    let r = Command::new(&pg_restore)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let (_, rmajor) = parse_version_output(&String::from_utf8_lossy(&r.stdout))?;
    if rmajor != major {
        return None;
    }
    Some(Tools {
        dir: dir.to_path_buf(),
        pg_dump,
        pg_restore,
        version,
        major,
    })
}

/// Candidate directories in precedence order: `PGBOXDB_PG_BINDIR`, each `PATH`
/// entry, then the well-known locations.
pub fn candidate_dirs(env_bindir: Option<&str>, path: Option<&str>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = env_bindir.filter(|d| !d.is_empty()) {
        v.push(PathBuf::from(d));
    }
    if let Some(p) = path {
        for entry in p.split(':').filter(|e| !e.is_empty()) {
            v.push(PathBuf::from(entry));
        }
    }
    v.extend(well_known_dirs());
    let mut seen = std::collections::HashSet::new();
    v.retain(|d| seen.insert(d.clone()));
    v
}

/// Pick the tools: an explicit `PGBOXDB_PG_BINDIR` is used as is (and must work);
/// otherwise the highest version among PATH and well-known dirs, PATH first
/// on a tie.
pub fn locate() -> Result<Tools> {
    let env_bindir = std::env::var("PGBOXDB_PG_BINDIR").ok();
    let path = std::env::var("PATH").ok();
    locate_in(env_bindir.as_deref(), path.as_deref())
}

pub fn locate_in(env_bindir: Option<&str>, path: Option<&str>) -> Result<Tools> {
    if let Some(d) = env_bindir.filter(|d| !d.is_empty()) {
        return probe_dir(Path::new(d)).ok_or_else(|| {
            PgboxdbError::failure(format!(
                "PGBOXDB_PG_BINDIR={d} does not contain working pg_dump and pg_restore of the same version"
            ))
            .hint("point PGBOXDB_PG_BINDIR at a PostgreSQL bin directory, or unset it to search PATH")
        });
    }
    let mut best: Option<Tools> = None;
    for dir in candidate_dirs(None, path) {
        if let Some(t) = probe_dir(&dir) {
            let better = match &best {
                None => true,
                Some(b) => t.major > b.major,
            };
            if better {
                best = Some(t);
            }
        }
    }
    best.ok_or_else(missing_tools_error)
}

pub fn missing_tools_error() -> PgboxdbError {
    let hint = if cfg!(target_os = "macos") {
        "install the PostgreSQL client tools: `brew install libpq` (then add $(brew --prefix libpq)/bin to PATH) or `brew install postgresql@18`; or set PGBOXDB_PG_BINDIR to a directory containing pg_dump and pg_restore"
    } else {
        "install the PostgreSQL client tools: Debian/Ubuntu `apt install postgresql-client-18` (apt.postgresql.org), Fedora/RHEL `dnf install postgresql18`, Alpine `apk add postgresql18-client`; or set PGBOXDB_PG_BINDIR to a directory containing pg_dump and pg_restore"
    };
    PgboxdbError::failure(
        "pg_dump and pg_restore were not found; `pgboxdb copy` needs the PostgreSQL client tools (they are not bundled with PGBoxDB)",
    )
    .hint(hint)
}

/// What a finished child left behind.
#[derive(Debug)]
pub struct Finished {
    pub success: bool,
    pub status_text: String,
    /// Last stderr lines (redacted), for diagnostics.
    pub tail: Vec<String>,
}

/// Run `cmd` with stderr piped. Every stderr line (redacted) goes to
/// `on_line`; `on_tick` runs about every 200 ms while the child lives (for
/// progress). If the interrupt flag is raised the child is terminated and the
/// result is an "interrupted" error. The caller decides what a non-zero exit
/// means.
pub fn run_streaming(
    cmd: &mut Command,
    redactor: &Redactor,
    mut on_line: impl FnMut(&str),
    mut on_tick: impl FnMut(),
) -> Result<Finished> {
    cmd.stdin(Stdio::null()).stderr(Stdio::piped());
    let mut child: Child = cmd.spawn().map_err(|e| {
        PgboxdbError::failure(format!(
            "cannot run {}: {e}",
            Path::new(cmd.get_program()).display()
        ))
    })?;
    let stderr = child.stderr.take().expect("stderr piped");
    let (tx, rx) = mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead;
        // Bytes, not `lines()`: a non-UTF-8 line (object names in the source's
        // encoding) must not end the reader, which would close the pipe and
        // kill the tool with SIGPIPE.
        let mut r = std::io::BufReader::new(stderr);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf)
                        .trim_end_matches(['\n', '\r'])
                        .to_string();
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut tail: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut push_tail = |line: String| {
        if tail.len() == 200 {
            tail.pop_front();
        }
        tail.push_back(line);
    };
    let mut exited: Option<std::process::ExitStatus> = None;
    let mut killed_for_interrupt = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                let line = redactor.redact(&line);
                on_line(&line);
                push_tail(line);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // reader finished: child closed stderr; collect its status
                if exited.is_none() {
                    exited = Some(child.wait().map_err(|e| {
                        PgboxdbError::failure(format!("cannot wait for child: {e}"))
                    })?);
                }
                break;
            }
        }
        if interrupted() && !killed_for_interrupt {
            killed_for_interrupt = true;
            if exited.is_none() {
                unsafe {
                    libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
                }
            }
        }
        if exited.is_none() {
            if let Ok(Some(st)) = child.try_wait() {
                exited = Some(st);
            }
        }
        on_tick();
    }
    let _ = reader.join();
    let status = match exited {
        Some(s) => s,
        None => child
            .wait()
            .map_err(|e| PgboxdbError::failure(format!("cannot wait for child: {e}")))?,
    };
    if killed_for_interrupt || interrupted() {
        return Err(PgboxdbError::failure("interrupted"));
    }
    Ok(Finished {
        success: status.success(),
        status_text: status.to_string(),
        tail: tail.into_iter().collect(),
    })
}

/// `cmd` run to completion with stdout captured (stderr too), redacted.
pub fn run_capture(cmd: &mut Command, redactor: &Redactor) -> Result<(bool, String, String)> {
    let out = cmd.stdin(Stdio::null()).output().map_err(|e| {
        PgboxdbError::failure(format!(
            "cannot run {}: {e}",
            Path::new(cmd.get_program()).display()
        ))
    })?;
    Ok((
        out.status.success(),
        redactor.redact(&String::from_utf8_lossy(&out.stdout)),
        redactor.redact(&String::from_utf8_lossy(&out.stderr)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(
            parse_version_output("pg_dump (PostgreSQL) 18.6 (Homebrew)\n"),
            Some(("18.6".into(), 18))
        );
        assert_eq!(
            parse_version_output("pg_restore (PostgreSQL) 17beta1"),
            Some(("17beta1".into(), 17))
        );
        assert_eq!(
            parse_version_output("pg_dump (PostgreSQL) 16.4 (Ubuntu 16.4-1.pgdg24.04+1)"),
            Some(("16.4".into(), 16))
        );
        assert_eq!(parse_version_output("garbage"), None);
    }

    #[test]
    fn candidates_prefer_env_then_path_then_known() {
        let v = candidate_dirs(Some("/x/bin"), Some("/a:/b::/a"));
        assert_eq!(v[0], PathBuf::from("/x/bin"));
        assert_eq!(v[1], PathBuf::from("/a"));
        assert_eq!(v[2], PathBuf::from("/b"));
        assert!(v.contains(&PathBuf::from("/usr/lib/postgresql/18/bin")));
        assert!(v.contains(&PathBuf::from("/opt/homebrew/opt/libpq/bin")));
        // deduplicated
        assert_eq!(
            v.iter().filter(|d| d.as_path() == Path::new("/a")).count(),
            1
        );
    }

    fn fake_tools(dir: &Path, version: &str) {
        std::fs::create_dir_all(dir).unwrap();
        for name in ["pg_dump", "pg_restore"] {
            let p = dir.join(name);
            std::fs::write(
                &p,
                format!("#!/bin/sh\necho \"{name} (PostgreSQL) {version}\"\n"),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn locate_picks_highest_version_and_honours_env() {
        let base = std::env::temp_dir().join(format!("pgboxdb-pgtools-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        fake_tools(&base.join("v16"), "16.4");
        fake_tools(&base.join("v18"), "18.6");
        std::fs::create_dir_all(base.join("empty")).unwrap();
        let path = format!(
            "{}:{}:{}",
            base.join("empty").display(),
            base.join("v16").display(),
            base.join("v18").display()
        );
        let t = locate_in(None, Some(&path)).unwrap();
        assert_eq!(t.major, 18);
        assert_eq!(t.version, "18.6");
        assert_eq!(t.dir, base.join("v18"));
        let t = locate_in(Some(base.join("v16").to_str().unwrap()), Some(&path)).unwrap();
        assert_eq!(t.major, 16, "an explicit PGBOXDB_PG_BINDIR wins");
        let e = locate_in(Some(base.join("empty").to_str().unwrap()), Some(&path)).unwrap_err();
        assert!(e.message.contains("PGBOXDB_PG_BINDIR"), "{}", e.message);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn missing_tools_error_explains_install() {
        let e = missing_tools_error();
        assert_eq!(e.code, crate::cli::ExitCode::Failure);
        assert!(e.hint.as_deref().unwrap().contains("PGBOXDB_PG_BINDIR"));
    }

    #[test]
    fn streaming_survives_non_utf8_output() {
        let r = Redactor::new(None);
        let mut lines = Vec::new();
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "printf 'caf\\351 ok\\n' >&2; echo after >&2; exit 0"]);
        let f = run_streaming(&mut cmd, &r, |l| lines.push(l.to_string()), || {}).unwrap();
        assert!(f.success, "{f:?}");
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[1], "after");
    }

    #[test]
    fn streaming_redacts_and_reports_status() {
        let r = Redactor::new(Some("hunter2"));
        let mut lines = Vec::new();
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "echo 'pw=hunter2 ok' >&2; echo second >&2; exit 3"]);
        let f = run_streaming(&mut cmd, &r, |l| lines.push(l.to_string()), || {}).unwrap();
        assert!(!f.success);
        assert_eq!(lines, vec!["pw=*** ok", "second"]);
        assert_eq!(f.tail, lines);
        assert!(f.status_text.contains('3'), "{}", f.status_text);
    }
}
