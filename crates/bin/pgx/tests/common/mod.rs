//! Black-box harness: a temporary installation prefix (the engine and share
//! trees of an assembled distribution, plus the `pgx` binary under test) and a
//! temporary `PGX_HOME`. Every test gets its own; the engine is stopped and
//! the directories removed on drop, even when the test panics.
//!
//! Needs `PGX_DIST=<assembled prefix>` (see dist/assemble.sh). Without it the
//! harness fails loudly: these suites are release gates, never silent skips.
#![allow(dead_code)]
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};

/// Serialises descriptor creation with process spawning across test threads.
///
/// macOS has no `pipe2`/`SOCK_CLOEXEC`: std creates a pipe or socket and only
/// then sets close-on-exec. A fork in another thread inside that window copies
/// the half-initialised descriptor into an unrelated child, which then holds a
/// test's stdout pipe (the parent's `output()` never sees EOF) or its port
/// probe socket (`pgx init --port` fails with exit 4). The lock covers only the
/// fd creation and the spawn, never the wait, so tests still run concurrently.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

/// Hold [`SPAWN_LOCK`] while creating descriptors a child must not inherit
/// by accident (the PTY tests' `openpty` + spawn).
pub fn spawn_guard() -> MutexGuard<'static, ()> {
    SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// `cmd.spawn()` under [`SPAWN_LOCK`]. Use this instead of calling `spawn`
/// directly anywhere in these suites.
pub fn spawn_locked(cmd: &mut Command) -> std::io::Result<Child> {
    let _g = spawn_guard();
    cmd.spawn()
}

/// `cmd.output()` with the spawn (and its pipe creation) under [`SPAWN_LOCK`];
/// waiting for the child happens outside the lock.
pub fn output_locked(cmd: &mut Command) -> std::io::Result<Output> {
    let child = {
        let _g = spawn_guard();
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?
    };
    child.wait_with_output()
}

static COUNTER: AtomicU32 = AtomicU32::new(0);
static PORT_SEQ: AtomicU32 = AtomicU32::new(0);

pub struct Pgx {
    pub prefix: PathBuf,
    pub home: PathBuf,
    pub port: u16,
}

pub fn dist_prefix() -> PathBuf {
    let p = std::env::var("PGX_DIST").unwrap_or_else(|_| {
        panic!("PGX_DIST is not set: point it at an assembled distribution prefix (dist/assemble.sh --out ...)")
    });
    let p = PathBuf::from(p);
    for rel in [
        "bin/postgres",
        "share/timezone",
        "share/pgx/base-cluster/PG_VERSION",
        "share/pgx/dist.json",
    ] {
        assert!(p.join(rel).exists(), "PGX_DIST={} lacks {rel}", p.display());
    }
    p
}

pub fn free_port() -> u16 {
    // Hand out a DISTINCT candidate per call (process-wide sequence) rather than
    // asking the kernel for an ephemeral port with bind(:0): under parallel
    // tests two bind(:0) calls can be handed the same recently-closed ephemeral
    // port, and then two `pgx init`s race to bind it (one gets exit 4). A unique
    // sequence, offset by pid so separate test binaries don't overlap, removes
    // that race; each candidate is still bind-verified to skip a port already
    // held by an unrelated process.
    let offset = (std::process::id().wrapping_mul(131)) % 20000;
    for _ in 0..40000u32 {
        let seq = PORT_SEQ.fetch_add(1, Ordering::SeqCst);
        let port = 20000 + ((offset + seq) % 40000);
        let port = port as u16;
        // Under SPAWN_LOCK: the probe socket must not be half-initialised
        // (no close-on-exec yet) while another thread forks a child.
        let free = {
            let _g = spawn_guard();
            TcpListener::bind(("127.0.0.1", port)).is_ok()
        };
        if free {
            return port;
        }
    }
    panic!("no free port found in [20000, 60000)");
}

pub fn scratch_root() -> PathBuf {
    // Short: the Unix socket path must stay under 103 bytes.
    let root = std::env::var("PGX_TEST_SCRATCH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/pgxt"));
    std::fs::create_dir_all(&root).unwrap();
    root
}

impl Pgx {
    /// A fresh harness: prefix and home created, nothing initialised yet.
    pub fn new() -> Pgx {
        let dist = dist_prefix();
        let id = format!(
            "{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        );
        let base = scratch_root().join(id);
        let prefix = base.join("p");
        let home = base.join("h");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_pgx"), prefix.join("bin/pgx")).unwrap();
        std::os::unix::fs::symlink(dist.join("bin/postgres"), prefix.join("bin/postgres")).unwrap();
        std::os::unix::fs::symlink(dist.join("share"), prefix.join("share")).unwrap();
        Pgx {
            prefix,
            home,
            port: free_port(),
        }
    }

    pub fn with_home(home: PathBuf) -> Pgx {
        let mut p = Pgx::new();
        std::fs::create_dir_all(&home).unwrap();
        p.home = home;
        p
    }

    pub fn bin(&self) -> PathBuf {
        self.prefix.join("bin/pgx")
    }

    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(self.bin());
        c.args(args)
            .env("PGX_HOME", &self.home)
            // Poll TTL cleanup every second in tests so expiry is prompt.
            .env("PGX_TTL_INTERVAL", "1")
            .env_remove("PGPORT")
            .env_remove("PGHOST");
        // Agent-host variables leak in when the suite itself runs inside a
        // coding agent; the tests set exactly the ones they mean.
        for v in [
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_ENV_FILE",
            "CLAUDE_PROJECT_DIR",
            "CODEX_SESSION_ID",
            "CODEX_THREAD_ID",
            "CURSOR_VERSION",
            "CURSOR_PROJECT_DIR",
            "PGX_SOURCE",
            "PGX_AGENT_HOST",
        ] {
            c.env_remove(v);
        }
        c
    }

    /// The background TTL supervisor's pid for this home, if it recorded one.
    pub fn supervisor_pid(&self) -> Option<u32> {
        std::fs::read_to_string(self.home.join("supervisor.pid"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    /// True when a supervisor process recorded in this home is actually alive.
    pub fn supervisor_running(&self) -> bool {
        match self.supervisor_pid() {
            Some(pid) => (unsafe { libc::kill(pid as i32, 0) }) == 0,
            None => false,
        }
    }

    /// Poll until this runtime's TCP port is bindable again (released by a
    /// stopped engine), up to `secs`. Lets a stop/start test rebind the same
    /// port deterministically without racing the kernel's socket teardown under
    /// heavy parallel load.
    pub fn wait_port_free(&self, secs: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            if TcpListener::bind(("127.0.0.1", self.port)).is_ok() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    }

    /// Poll until the supervisor is alive, up to `secs`.
    pub fn wait_supervisor(&self, secs: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            if self.supervisor_running() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    }

    /// Run a prepared command, retrying only a failure to SPAWN the process
    /// (never a non-zero exit, which is a real result): under heavy CI
    /// parallelism fork/exec can transiently fail — EAGAIN, or ETXTBSY on the
    /// freshly copied pgx binary ("Text file busy"). The transient clears in
    /// milliseconds.
    fn output_retrying(&self, make: impl Fn() -> Command, what: &str) -> Run {
        let mut last = None;
        for _ in 0..8 {
            match output_locked(&mut make()) {
                Ok(o) => return Run::from(o),
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
        panic!("could not spawn {what} after retries: {}", last.unwrap());
    }

    pub fn run(&self, args: &[&str]) -> Run {
        self.output_retrying(|| self.cmd(args), "pgx")
    }

    /// `output_retrying` for suites that build their own Command (extra env).
    pub fn output_retrying_pub(&self, make: impl Fn() -> Command, what: &str) -> Run {
        self.output_retrying(make, what)
    }

    pub fn init(&self) -> Run {
        let port = self.port.to_string();
        self.run(&["init", "--port", &port])
    }

    /// `pgx run [extra...]` with PGX_PORT pinned to this harness's free port, so
    /// a fresh run initialises there instead of scanning 5432-5500 (and colliding
    /// with a real PostgreSQL or a sibling test).
    pub fn run_cmd(&self, extra: &[&str]) -> Run {
        let mut args = vec!["run"];
        args.extend_from_slice(extra);
        self.output_retrying(
            || {
                let mut c = self.cmd(&args);
                c.env("PGX_PORT", self.port.to_string());
                c
            },
            "pgx run",
        )
    }

    /// Spawn `pgx run` without waiting — for concurrency tests.
    pub fn spawn_run(&self) -> std::process::Child {
        let mut last = None;
        for _ in 0..8 {
            match spawn_locked(
                self.cmd(&["run"])
                    .env("PGX_PORT", self.port.to_string())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null()),
            ) {
                Ok(c) => return c,
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
        panic!("could not spawn pgx run after retries: {}", last.unwrap());
    }

    pub fn init_ok(&self) {
        let r = self.init();
        assert_eq!(r.code, 0, "pgx init failed: {}", r.all());
    }

    pub fn start_ok(&self) {
        let r = self.run(&["start"]);
        assert_eq!(
            r.code,
            0,
            "pgx start failed: {}\n{}",
            r.all(),
            self.log_tail()
        );
    }

    pub fn stop_ok(&self) {
        let r = self.run(&["stop"]);
        assert_eq!(r.code, 0, "pgx stop failed: {}", r.all());
    }

    pub fn log_tail(&self) -> String {
        std::fs::read_to_string(self.home.join("log/pgx.log"))
            .map(|s| {
                s.lines()
                    .rev()
                    .take(25)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    pub fn config(&self) -> serde_json::Value {
        serde_json::from_str(
            &std::fs::read_to_string(self.home.join("config.json")).expect("config.json"),
        )
        .unwrap()
    }

    pub fn password(&self) -> String {
        self.config()["password"].as_str().unwrap().to_string()
    }

    pub fn sock_dir(&self) -> PathBuf {
        self.config()["run_dir"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.home.join("run"))
    }

    /// Any `postgres` process whose command line names this home's data dir.
    pub fn engine_pids(&self) -> Vec<u32> {
        let out = output_locked(Command::new("pgrep").args([
            "-f",
            "--",
            &format!("postgres .*-D {}( |$)", self.home.join("data").display()),
        ]))
        .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    }

    /// Runs SQL through psql if available, else through `pgx`'s own client via a tiny helper.
    pub fn psql(&self, db: &str, sql: &str) -> Run {
        let psql = find_psql();
        let mut cmd = Command::new(psql);
        cmd.args([
            "-X",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-h",
            &self.sock_dir().to_string_lossy(),
            "-p",
            &self.port.to_string(),
            "-U",
            "postgres",
            "-d",
            db,
            "-Atc",
            sql,
        ]);
        let out = output_locked(&mut cmd).expect("spawn psql");
        Run::from(out)
    }

    pub fn psql_tcp(&self, db: &str, password: &str, sql: &str) -> Run {
        let psql = find_psql();
        let mut cmd = Command::new(psql);
        cmd.args([
            "-X",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-h",
            "127.0.0.1",
            "-p",
            &self.port.to_string(),
            "-U",
            "postgres",
            "-d",
            db,
            "-Atc",
            sql,
        ])
        .env("PGPASSWORD", password);
        let out = output_locked(&mut cmd).expect("spawn psql");
        Run::from(out)
    }
}

pub fn find_psql() -> PathBuf {
    if let Ok(p) = std::env::var("PGX_TEST_PSQL") {
        return PathBuf::from(p);
    }
    for cand in [
        "/opt/homebrew/opt/postgresql@18/bin/psql",
        "/usr/lib/postgresql/18/bin/psql",
        "/usr/local/pgsql/bin/psql",
    ] {
        if Path::new(cand).exists() {
            return PathBuf::from(cand);
        }
    }
    PathBuf::from("psql")
}

impl Drop for Pgx {
    fn drop(&mut self) {
        // Stop the TTL supervisor first so it does not linger after the home is gone.
        if let Some(pid) = self.supervisor_pid() {
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
        let _ = output_locked(&mut self.cmd(&["stop", "--force"]));
        for pid in self.engine_pids() {
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
        if std::env::var("PGX_TEST_KEEP").is_err() {
            if let Some(base) = self.prefix.parent() {
                let _ = std::fs::remove_dir_all(base);
            }
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }
}

#[derive(Debug)]
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl From<Output> for Run {
    fn from(o: Output) -> Run {
        Run {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }
}

impl Run {
    pub fn all(&self) -> String {
        format!(
            "exit {}\nstdout: {}\nstderr: {}",
            self.code, self.stdout, self.stderr
        )
    }
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(self.stdout.trim())
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}", self.all()))
    }
}

// ---------------------------------------------------------------------------
// A real PostgreSQL source for the `pgx copy` suite: its own initdb cluster on
// a free port, trust on the Unix socket (for test setup) and scram-sha-256 over
// TCP (what `pgx copy --from postgres://…` exercises, so wrong credentials are
// really wrong). Stopped and removed on drop.

/// The PostgreSQL bin directory with initdb/pg_ctl/pg_dump: `PGX_TEST_PG_BINDIR`,
/// `PG18_BINDIR` (CI), the directory of the psql we found, or a PATH search.
pub fn pg_bindir() -> PathBuf {
    for var in ["PGX_TEST_PG_BINDIR", "PG18_BINDIR"] {
        if let Ok(d) = std::env::var(var) {
            if Path::new(&d).join("initdb").exists() {
                return PathBuf::from(d);
            }
        }
    }
    let psql = find_psql();
    if psql.is_absolute() {
        if let Some(d) = psql.parent() {
            if d.join("initdb").exists() {
                return d.to_path_buf();
            }
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for d in path.split(':') {
            if Path::new(d).join("initdb").exists() && Path::new(d).join("pg_dump").exists() {
                return PathBuf::from(d);
            }
        }
    }
    panic!("no PostgreSQL bin directory with initdb found; set PGX_TEST_PG_BINDIR")
}

pub struct PgSource {
    pub dir: PathBuf,
    pub port: u16,
    pub bindir: PathBuf,
    pub password: String,
}

impl PgSource {
    pub fn start() -> PgSource {
        let bindir = pg_bindir();
        let id = format!(
            "{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        );
        let dir = scratch_root().join(format!("pg{id}"));
        let _ = std::fs::remove_dir_all(&dir);
        let port = free_port();
        let password = "src-s3cret-PW".to_string();
        let out = output_locked(Command::new(bindir.join("initdb")).args([
            "-D",
            dir.to_str().unwrap(),
            "--no-sync",
            "-A",
            "trust",
            "-U",
            "postgres",
            "-E",
            "UTF8",
            "--no-locale",
        ]))
        .expect("spawn initdb");
        assert!(
            out.status.success(),
            "initdb failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::write(
            dir.join("pg_hba.conf"),
            "local all all trust\nhost all all 127.0.0.1/32 scram-sha-256\nhost all all ::1/128 scram-sha-256\n",
        )
        .unwrap();
        let opts = format!(
            "-p {port} -k {} -c listen_addresses=127.0.0.1 -c fsync=off -c synchronous_commit=off -c full_page_writes=off",
            dir.display()
        );
        let out = output_locked(Command::new(bindir.join("pg_ctl")).args([
            "-D",
            dir.to_str().unwrap(),
            "-o",
            &opts,
            "-l",
            dir.join("server.log").to_str().unwrap(),
            "-w",
            "start",
        ]))
        .expect("spawn pg_ctl");
        assert!(
            out.status.success(),
            "pg_ctl start failed: {}\n{}",
            String::from_utf8_lossy(&out.stderr),
            std::fs::read_to_string(dir.join("server.log")).unwrap_or_default()
        );
        let src = PgSource {
            dir,
            port,
            bindir,
            password,
        };
        let r = src.psql(
            "postgres",
            &format!("ALTER ROLE postgres PASSWORD '{}'", src.password),
        );
        assert_eq!(r.code, 0, "{}", r.all());
        src
    }

    /// `postgres://postgres:<password>@127.0.0.1:<port>/<db>` (scram over TCP).
    pub fn url(&self, db: &str) -> String {
        format!(
            "postgres://postgres:{}@127.0.0.1:{}/{db}",
            self.password, self.port
        )
    }

    /// SQL over the trusted Unix socket; `-c` so a multi-statement string runs
    /// in one transaction (like a migration) unless it contains statements that
    /// refuse that, in which case call once per statement.
    pub fn psql(&self, db: &str, sql: &str) -> Run {
        let mut cmd = Command::new(self.bindir.join("psql"));
        cmd.args([
            "-X",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-h",
            self.dir.to_str().unwrap(),
            "-p",
            &self.port.to_string(),
            "-U",
            "postgres",
            "-d",
            db,
            "-Atc",
            sql,
        ])
        .env_remove("PGPASSWORD");
        Run::from(output_locked(&mut cmd).expect("spawn psql"))
    }

    pub fn create_db(&self, name: &str) {
        let r = self.psql("postgres", &format!("CREATE DATABASE \"{name}\""));
        assert_eq!(r.code, 0, "{}", r.all());
    }

    /// A deterministic fingerprint of every user table's rows and of the
    /// object inventory — to prove the source was not modified.
    pub fn fingerprint(&self, db: &str) -> String {
        let r = self.psql(
            db,
            "SELECT string_agg(x, ';' ORDER BY x) FROM ( \
               SELECT n.nspname || '.' || c.relname || '=' || \
                 (xpath('/row/c/text()', query_to_xml(format('SELECT md5(string_agg(t::text, %L ORDER BY t::text)) AS c FROM %I.%I t', '|', n.nspname, c.relname), false, true, '')))[1]::text AS x \
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
               WHERE c.relkind = 'r' AND n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_toast%' \
               UNION ALL SELECT 'objects=' || count(*)::text FROM pg_class WHERE relnamespace = 'public'::regnamespace \
               UNION ALL SELECT 'procs=' || count(*)::text FROM pg_proc WHERE pronamespace = 'public'::regnamespace \
             ) s",
        );
        assert_eq!(r.code, 0, "{}", r.all());
        r.stdout.trim().to_string()
    }
}

impl Drop for PgSource {
    fn drop(&mut self) {
        let _ = output_locked(Command::new(self.bindir.join("pg_ctl")).args([
            "-D",
            self.dir.to_str().unwrap(),
            "-m",
            "immediate",
            "-w",
            "stop",
        ]));
        if std::env::var("PGX_TEST_KEEP").is_err() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

impl Pgx {
    /// `pgx [--json] copy --from <src url> --to <dest> [extra…]` with the test
    /// PostgreSQL bin directory pinned, so the tools under test are the same
    /// ones that created the source.
    pub fn copy(&self, src: &PgSource, db: &str, dest: &str, extra: &[&str]) -> Run {
        let url = src.url(db);
        let mut args = vec!["copy", "--from", url.as_str(), "--to", dest];
        args.extend_from_slice(extra);
        self.copy_args(src, &args)
    }

    pub fn copy_json(&self, src: &PgSource, db: &str, dest: &str, extra: &[&str]) -> Run {
        let url = src.url(db);
        let mut args = vec!["--json", "copy", "--from", url.as_str(), "--to", dest];
        args.extend_from_slice(extra);
        self.copy_args(src, &args)
    }

    pub fn copy_args(&self, src: &PgSource, args: &[&str]) -> Run {
        self.output_retrying(
            || {
                let mut c = self.cmd(args);
                c.env("PGX_PG_BINDIR", &src.bindir);
                c
            },
            "pgx copy",
        )
    }

    pub fn db_exists(&self, name: &str) -> bool {
        self.psql(
            "postgres",
            &format!("SELECT count(*) FROM pg_database WHERE datname='{name}'"),
        )
        .stdout
        .trim()
            == "1"
    }

    /// Names of `_pgxcopy_*` staging databases currently present.
    pub fn staging_dbs(&self) -> Vec<String> {
        self.psql(
            "postgres",
            "SELECT datname FROM pg_database WHERE datname LIKE '\\_pgxcopy\\_%' ORDER BY 1",
        )
        .stdout
        .lines()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect()
    }

    /// Entries under $PGX_HOME/tmp and $PGX_HOME/copy (both must be empty after
    /// any finished copy).
    pub fn copy_leftovers(&self) -> Vec<String> {
        let mut v = Vec::new();
        for sub in ["tmp", "copy"] {
            if let Ok(rd) = std::fs::read_dir(self.home.join(sub)) {
                for e in rd.flatten() {
                    v.push(format!("{sub}/{}", e.file_name().to_string_lossy()));
                }
            }
        }
        v
    }
}
