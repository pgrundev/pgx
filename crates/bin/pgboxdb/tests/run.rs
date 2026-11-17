//! Black-box tests for `pgboxdb run` against an assembled distribution: the
//! one-command developer onboarding. Covers a fresh install, idempotency,
//! persistence across restart, secure localhost-only defaults, concurrent
//! invocations, and recovery after an engine crash.
mod common;
use common::Pgboxdb;
use std::time::{Duration, Instant};

fn wait_engine_gone(p: &Pgboxdb) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if p.engine_pids().is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn run_from_fresh_install_is_ready() {
    let p = Pgboxdb::new();
    // A brand-new $PGBOXDB_HOME: nothing initialised, nothing running.
    assert!(!p.home.join("data/PG_VERSION").is_file());
    let r = p.run_cmd(&[]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(r.stdout.contains("PGBoxDB is ready"), "{}", r.stdout);
    assert!(
        r.stdout.contains(&format!("127.0.0.1:{}/app", p.port)),
        "URL with the app database expected: {}",
        r.stdout
    );
    // The runtime is up and the app database is usable.
    assert_eq!(p.engine_pids().len(), 1, "exactly one engine running");
    assert_eq!(p.psql("app", "SELECT 1").stdout.trim(), "1");
}

#[test]
fn run_is_idempotent_and_creates_no_duplicates() {
    let p = Pgboxdb::new();
    let first = p.run_cmd(&["--json"]).json();
    assert_eq!(first["initialized"], true);
    assert_eq!(first["started"], true);
    assert_eq!(first["database_created"], true);
    let pid1 = first["pid"].as_u64().unwrap();

    let second = p.run_cmd(&["--json"]).json();
    assert_eq!(second["initialized"], false, "already initialised");
    assert_eq!(second["already_running"], true, "already running");
    assert_eq!(second["database_created"], false, "app already exists");
    assert_eq!(
        second["pid"].as_u64().unwrap(),
        pid1,
        "a second run must not restart the engine"
    );

    let n = p.psql(
        "postgres",
        "SELECT count(*) FROM pg_database WHERE datname='app'",
    );
    assert_eq!(n.stdout.trim(), "1", "exactly one app database");
}

#[test]
fn run_json_is_one_object_with_connection_url() {
    let p = Pgboxdb::new();
    let out = p.run_cmd(&["--json"]);
    assert_eq!(out.code, 0, "{}", out.all());
    assert_eq!(
        out.stdout.trim().lines().count(),
        1,
        "exactly one JSON object on stdout: {}",
        out.stdout
    );
    let j = out.json();
    assert_eq!(j["ready"], true);
    assert_eq!(j["database"], "app");
    assert_eq!(j["user"], "postgres");
    assert_eq!(j["host"], "127.0.0.1");
    assert_eq!(j["port"].as_u64().unwrap(), p.port as u64);
    assert!(j["url"].as_str().unwrap().ends_with("/app"), "{}", j["url"]);
    assert!(j["socket_url"].as_str().unwrap().contains("host="));
}

#[test]
fn run_preserves_data_across_restart() {
    let p = Pgboxdb::new();
    assert_eq!(p.run_cmd(&[]).code, 0);
    assert_eq!(
        p.psql(
            "app",
            "CREATE TABLE t(v int); INSERT INTO t VALUES (42); SELECT count(*) FROM t"
        )
        .stdout
        .trim(),
        "1"
    );
    p.stop_ok();
    let r = p.run_cmd(&[]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(
        p.psql("app", "SELECT v FROM t").stdout.trim(),
        "42",
        "data written before `pgboxdb stop` must survive the next `pgboxdb run`"
    );
}

#[test]
fn run_never_touches_existing_databases() {
    let p = Pgboxdb::new();
    // An existing cluster with a developer's own database and data.
    p.init_ok();
    p.start_ok();
    assert_eq!(p.run(&["create", "mydb"]).code, 0);
    p.psql(
        "mydb",
        "CREATE TABLE keep(v int); INSERT INTO keep VALUES (99)",
    );

    // `pgboxdb run` must add `app` without disturbing anything that exists.
    let r = p.run_cmd(&[]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(
        p.psql("mydb", "SELECT v FROM keep").stdout.trim(),
        "99",
        "an existing database and its data must be untouched"
    );
    assert_eq!(
        p.psql(
            "postgres",
            "SELECT count(*) FROM pg_database WHERE datname='app'"
        )
        .stdout
        .trim(),
        "1"
    );
}

#[test]
fn run_defaults_are_localhost_only_and_password_protected() {
    let p = Pgboxdb::new();
    assert_eq!(p.run_cmd(&[]).code, 0);
    let hba = std::fs::read_to_string(p.home.join("data/pg_hba.conf")).unwrap();
    assert!(
        !hba.contains("0.0.0.0"),
        "must not listen on the world: {hba}"
    );
    assert!(
        hba.contains("scram-sha-256"),
        "TCP must need a password: {hba}"
    );
    let conf = std::fs::read_to_string(p.home.join("data/pgboxdb.conf")).unwrap();
    assert!(
        conf.contains("listen_addresses = '127.0.0.1, ::1'"),
        "loopback only: {conf}"
    );
    // The right password connects over TCP; a wrong one is refused.
    let pw = p.password();
    assert_eq!(p.psql_tcp("app", &pw, "SELECT 1").stdout.trim(), "1");
    assert_ne!(
        p.psql_tcp("app", "definitely-the-wrong-password", "SELECT 1")
            .code,
        0,
        "a wrong password must be rejected"
    );
}

#[test]
fn concurrent_runs_make_one_engine_and_one_app() {
    let p = Pgboxdb::new();
    // Eight developers (or agents) racing `pgboxdb run` on a fresh home at once.
    let kids: Vec<_> = (0..8).map(|_| p.spawn_run()).collect();
    let codes: Vec<i32> = kids
        .into_iter()
        .map(|mut k| k.wait().unwrap().code().unwrap_or(-1))
        .collect();
    assert!(
        codes.iter().all(|&c| c == 0),
        "every concurrent `pgboxdb run` must succeed: {codes:?}\n{}",
        p.log_tail()
    );
    assert_eq!(
        p.engine_pids().len(),
        1,
        "concurrent runs must not start more than one engine"
    );
    assert_eq!(
        p.psql(
            "postgres",
            "SELECT count(*) FROM pg_database WHERE datname='app'"
        )
        .stdout
        .trim(),
        "1",
        "concurrent runs must create exactly one app database"
    );
}

#[test]
fn run_recovers_after_an_engine_crash() {
    let p = Pgboxdb::new();
    assert_eq!(p.run_cmd(&[]).code, 0);
    assert_eq!(p.psql("app", "SELECT 1").stdout.trim(), "1");

    // Simulate a crash: SIGKILL the engine, leaving a stale postmaster.pid and
    // possibly a lingering socket.
    for pid in p.engine_pids() {
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }
    wait_engine_gone(&p);
    assert!(p.engine_pids().is_empty(), "engine should be gone");

    // `pgboxdb run` must clean up and bring a healthy runtime back.
    let r = p.run_cmd(&[]);
    assert_eq!(
        r.code,
        0,
        "`pgboxdb run` must recover after a crash: {}\n{}",
        r.all(),
        p.log_tail()
    );
    assert_eq!(
        p.engine_pids().len(),
        1,
        "exactly one engine after recovery"
    );
    // app is guaranteed present afterwards (run re-ensures it, idempotently).
    assert_eq!(p.psql("app", "SELECT 1").stdout.trim(), "1");
}
