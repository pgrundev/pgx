mod common;
use common::Pgboxdb;
use std::os::unix::fs::PermissionsExt;

#[test]
fn version_prints_engine_and_compat() {
    let p = Pgboxdb::new();
    let r = p.run(&["version"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(r.stdout.starts_with("pgboxdb "), "{}", r.stdout);
    assert!(r.stdout.contains("PostgreSQL 18.6"), "{}", r.stdout);
    assert!(r.stdout.contains("engine "), "{}", r.stdout);
    let j = p.run(&["--json", "version"]).json();
    assert_eq!(j["postgres_compat"], "18.6");
    assert!(j["engine_commit"].as_str().unwrap().len() >= 7);
    assert!(j["pgboxdb_version"].as_str().unwrap().starts_with("0."));
}

#[test]
fn init_creates_cluster_and_config() {
    let p = Pgboxdb::new();
    let r = p.init();
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(p.home.join("data/PG_VERSION").is_file());
    assert!(p.home.join("data/pgboxdb.conf").is_file());
    assert!(p.home.join("data/pg_hba.conf").is_file());
    let mode = std::fs::metadata(p.home.join("data"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700, "data dir mode {mode:o}");
    let mode = std::fs::metadata(p.home.join("config.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "config mode {mode:o}");
    let c = p.config();
    assert_eq!(c["port"], p.port);
    assert_eq!(c["durability"], "ephemeral");
    assert!(c["password"].as_str().unwrap().len() >= 24);
    assert!(p.engine_pids().is_empty(), "init left an engine running");
    assert!(
        !p.home.join("data/postmaster.pid").exists(),
        "init left postmaster.pid"
    );
    let conf = std::fs::read_to_string(p.home.join("data/pgboxdb.conf")).unwrap();
    assert!(conf.contains(&format!("port = {}", p.port)));
    assert!(
        conf.contains("listen_addresses = '127.0.0.1, ::1'"),
        "{conf}"
    );
    assert!(
        conf.contains("fsync = off"),
        "ephemeral profile expected: {conf}"
    );
    let hba = std::fs::read_to_string(p.home.join("data/pg_hba.conf")).unwrap();
    assert!(hba.contains("scram-sha-256"), "{hba}");
    assert!(!hba.contains("0.0.0.0/0"), "{hba}");
    assert!(
        r.stdout.contains("pgboxdb start"),
        "init should tell the user what to do next: {}",
        r.stdout
    );
}

#[test]
fn init_json_output() {
    let p = Pgboxdb::new();
    let port = p.port.to_string();
    let j = p.run(&["--json", "init", "--port", &port]).json();
    assert_eq!(j["port"], p.port);
    assert_eq!(j["durability"], "ephemeral");
    assert_eq!(
        j["data_dir"],
        p.home.join("data").to_string_lossy().as_ref()
    );
}

#[test]
fn init_full_durability_writes_fsync_on() {
    let p = Pgboxdb::new();
    let port = p.port.to_string();
    let r = p.run(&["init", "--port", &port, "--durability", "full"]);
    assert_eq!(r.code, 0, "{}", r.all());
    let conf = std::fs::read_to_string(p.home.join("data/pgboxdb.conf")).unwrap();
    assert!(conf.contains("fsync = on"), "{conf}");
    assert!(conf.contains("synchronous_commit = on"), "{conf}");
    assert!(conf.contains("full_page_writes = on"), "{conf}");
    assert_eq!(p.config()["durability"], "full");
}

#[test]
fn init_twice_is_conflict_and_keeps_data() {
    let p = Pgboxdb::new();
    p.init_ok();
    let before = std::fs::read_to_string(p.home.join("config.json")).unwrap();
    let r = p.init();
    assert_eq!(r.code, 4, "{}", r.all());
    assert!(r.stderr.contains("already"), "{}", r.stderr);
    assert_eq!(
        std::fs::read_to_string(p.home.join("config.json")).unwrap(),
        before,
        "second init changed config"
    );
    let j = p
        .run(&["--json", "init", "--port", &p.port.to_string()])
        .json();
    assert_eq!(j["error"]["exit_code"], 4);
}

#[test]
fn init_force_recreates() {
    let p = Pgboxdb::new();
    p.init_ok();
    std::fs::write(p.home.join("data/marker"), b"old").unwrap();
    let port = p.port.to_string();
    let r = p.run(&["init", "--port", &port, "--force"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(
        !p.home.join("data/marker").exists(),
        "--force did not recreate the data dir"
    );
}

#[test]
fn init_refuses_long_socket_path() {
    let long = common::scratch_root().join("x".repeat(95));
    let p = Pgboxdb::with_home(long);
    let r = p.init();
    assert_eq!(r.code, 2, "{}", r.all());
    assert!(
        r.stderr.contains("socket") && r.stderr.contains("PGBOXDB_RUN_DIR"),
        "{}",
        r.stderr
    );
    assert!(
        !p.home.join("data").exists(),
        "refused init must not leave a data dir"
    );
}

#[test]
fn init_with_run_dir_override_accepts_long_home() {
    let long = common::scratch_root().join("y".repeat(95));
    let p = Pgboxdb::with_home(long);
    let run_dir = common::scratch_root().join(format!("rd{}", p.port));
    let port = p.port.to_string();
    let r = p.output_retrying_pub(
        || {
            let mut c = p.cmd(&["init", "--port", &port]);
            c.env("PGBOXDB_RUN_DIR", &run_dir);
            c
        },
        "pgboxdb init",
    );
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(p.config()["run_dir"], run_dir.to_string_lossy().as_ref());
    let _ = std::fs::remove_dir_all(run_dir);
}

#[test]
fn init_picks_free_port_when_5432_busy() {
    // Simulated: ask for a port that is busy and expect init to refuse it;
    // the automatic choice is exercised by `init_without_port_chooses_a_free_one`.
    let p = Pgboxdb::new();
    let busy = std::net::TcpListener::bind(("127.0.0.1", p.port)).unwrap();
    let r = p.init();
    drop(busy);
    assert_eq!(r.code, 4, "{}", r.all());
    assert!(r.stderr.contains("in use"), "{}", r.stderr);
}

#[test]
fn init_without_port_chooses_a_free_one() {
    let p = Pgboxdb::new();
    let r = p.run(&["--json", "init"]);
    assert_eq!(r.code, 0, "{}", r.all());
    let port = r.json()["port"].as_u64().unwrap() as u16;
    assert!(port >= 5432, "{port}");
    // The chosen port is free (bind succeeds now that init has stopped its probe server).
    std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
}

#[test]
fn start_requires_init() {
    let p = Pgboxdb::new();
    let r = p.run(&["start"]);
    assert_eq!(r.code, 3, "{}", r.all());
    assert!(r.stderr.contains("pgboxdb init"), "{}", r.stderr);
}

#[test]
fn incomplete_installation_is_exit_7() {
    let p = Pgboxdb::new();
    std::fs::remove_file(p.prefix.join("share")).unwrap();
    std::fs::create_dir_all(p.prefix.join("share/timezone")).unwrap();
    let r = p.run(&["init"]);
    assert_eq!(r.code, 7, "{}", r.all());
    assert!(r.stderr.contains("base-cluster"), "{}", r.stderr);
}

// ---------------------------------------------------------------- start / stop / status

#[test]
fn start_stop_roundtrip_select_1() {
    let p = Pgboxdb::new();
    p.init_ok();
    let r = p.run(&["start"]);
    assert_eq!(r.code, 0, "{}\n{}", r.all(), p.log_tail());
    assert!(
        r.stdout.contains(&format!("port {}", p.port)) || r.stdout.contains(&p.port.to_string()),
        "{}",
        r.stdout
    );
    let pids = p.engine_pids();
    assert_eq!(pids.len(), 1, "expected one engine, got {pids:?}");
    let q = p.psql("postgres", "SELECT 1");
    assert_eq!(q.code, 0, "{}", q.all());
    assert_eq!(q.stdout.trim(), "1");
    let q = p.psql_tcp("postgres", &p.password(), "SELECT host(inet_server_addr())");
    assert_eq!(q.code, 0, "{}", q.all());
    assert_eq!(q.stdout.trim(), "127.0.0.1");
    let bad = p.psql_tcp("postgres", "wrong", "SELECT 1");
    assert_ne!(bad.code, 0, "wrong password must be rejected");
    let r = p.run(&["stop"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(p.engine_pids().is_empty(), "stop left an engine");
    assert!(
        !p.home.join("data/postmaster.pid").exists(),
        "stop left postmaster.pid"
    );
}

#[test]
fn start_twice_is_noop() {
    let p = Pgboxdb::new();
    p.init_ok();
    p.start_ok();
    let first = p.engine_pids();
    let r = p.run(&["start"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(r.stdout.contains("already running"), "{}", r.stdout);
    assert_eq!(p.engine_pids(), first, "second start changed the engine");
}

#[test]
fn stop_when_not_running_is_ok() {
    let p = Pgboxdb::new();
    p.init_ok();
    let r = p.run(&["stop"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(r.stdout.contains("not running"), "{}", r.stdout);
}

#[test]
fn status_reports_running_and_stopped() {
    let p = Pgboxdb::new();
    p.init_ok();
    let j = p.run(&["--json", "status"]).json();
    assert_eq!(j["running"], false);
    assert_eq!(j["port"], p.port);
    let r = p.run(&["status"]);
    assert_eq!(r.code, 3, "stopped status exits 3: {}", r.all());
    p.start_ok();
    let r = p.run(&["status"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(r.stdout.contains("running"), "{}", r.stdout);
    let j = p.run(&["--json", "status"]).json();
    assert_eq!(j["running"], true);
    assert_eq!(j["pid"], p.engine_pids()[0]);
    assert_eq!(j["port"], p.port);
    assert_eq!(j["durability"], "ephemeral");
    assert!(j["databases"].as_u64().unwrap() >= 1, "{j}");
    assert!(j["uptime_s"].as_u64().is_some(), "{j}");
    assert!(j["memory_rss_bytes"].as_u64().unwrap() > 1_000_000, "{j}");
    assert_eq!(j["socket_dir"], p.sock_dir().to_string_lossy().as_ref());
    assert!(j["libraries"]["libxml2"]["available"].is_boolean(), "{j}");
}

#[test]
fn stale_pid_reused_by_other_process() {
    let p = Pgboxdb::new();
    p.init_ok();
    // A pid that is alive but is not our engine: this test process.
    let me = std::process::id();
    std::fs::write(
        p.home.join("data/postmaster.pid"),
        format!(
            "{me}\n{}\n1\n{}\n{}\n\n  0 0\nready\n",
            p.home.join("data").display(),
            p.port,
            p.sock_dir().display()
        ),
    )
    .unwrap();
    let j = p.run(&["--json", "status"]).json();
    assert_eq!(
        j["running"], false,
        "a foreign pid must not count as running: {j}"
    );
    let r = p.run(&["start"]);
    assert_eq!(
        r.code,
        0,
        "start must clear the stale pid file: {}\n{}",
        r.all(),
        p.log_tail()
    );
    assert_eq!(p.engine_pids().len(), 1);
    assert!(std::fs::read_to_string(p.home.join("data/postmaster.pid"))
        .unwrap()
        .starts_with(&p.engine_pids()[0].to_string()));
}

#[test]
fn stale_pid_of_dead_process_is_cleared() {
    let p = Pgboxdb::new();
    p.init_ok();
    std::fs::write(
        p.home.join("data/postmaster.pid"),
        format!(
            "999999\n{}\n1\n{}\n{}\n\n  0 0\nready\n",
            p.home.join("data").display(),
            p.port,
            p.sock_dir().display()
        ),
    )
    .unwrap();
    let r = p.run(&["start"]);
    assert_eq!(r.code, 0, "{}\n{}", r.all(), p.log_tail());
}

#[test]
fn port_conflict_is_exit_4() {
    let p = Pgboxdb::new();
    p.init_ok();
    let busy = std::net::TcpListener::bind(("127.0.0.1", p.port)).unwrap();
    let r = p.run(&["start"]);
    drop(busy);
    assert_eq!(r.code, 4, "{}", r.all());
    assert!(r.stderr.contains("in use"), "{}", r.stderr);
    assert!(
        p.engine_pids().is_empty(),
        "a failed start must leave no engine"
    );
    assert!(!p.home.join("data/postmaster.pid").exists());
}

#[test]
fn concurrent_start_single_server() {
    let p = Pgboxdb::new();
    p.init_ok();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let mut c = p.cmd(&["start"]);
            std::thread::spawn(move || common::Run::from(common::output_locked(&mut c).unwrap()))
        })
        .collect();
    let runs: Vec<common::Run> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    for r in &runs {
        assert_eq!(r.code, 0, "{}", r.all());
    }
    assert_eq!(
        p.engine_pids().len(),
        1,
        "concurrent starts produced {} engines",
        p.engine_pids().len()
    );
    assert!(
        runs.iter()
            .filter(|r| r.stdout.contains("already running"))
            .count()
            >= 1,
        "no start reported the other: {:?}",
        runs.iter().map(|r| r.stdout.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn restart_preserves_data() {
    let p = Pgboxdb::new();
    p.init_ok();
    p.start_ok();
    assert_eq!(
        p.psql(
            "postgres",
            "CREATE TABLE keep(v text); INSERT INTO keep VALUES ('survives')"
        )
        .code,
        0
    );
    let r = p.run(&["restart"]);
    assert_eq!(r.code, 0, "{}\n{}", r.all(), p.log_tail());
    let q = p.psql("postgres", "SELECT v FROM keep");
    assert_eq!(q.stdout.trim(), "survives", "{}", q.all());
    p.stop_ok();
    p.start_ok();
    let q = p.psql("postgres", "SELECT v FROM keep");
    assert_eq!(
        q.stdout.trim(),
        "survives",
        "data lost across stop/start: {}",
        q.all()
    );
}

#[test]
fn stop_leaves_no_orphan_and_force_kills() {
    let p = Pgboxdb::new();
    p.init_ok();
    p.start_ok();
    let pid = p.engine_pids()[0];
    let r = p.run(&["stop", "--force"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(!p.engine_pids().contains(&pid));
    assert!(p.engine_pids().is_empty());
}

#[test]
fn start_survives_cli_parent_exit_and_terminal_hangup() {
    // The engine must be in its own session: a SIGHUP to the CLI's process group must not reach it.
    let p = Pgboxdb::new();
    p.init_ok();
    p.start_ok();
    let pid = p.engine_pids()[0];
    let sid = unsafe { libc::getsid(pid as i32) };
    assert_eq!(
        sid, pid as i32,
        "engine should lead its own session (sid {sid}, pid {pid})"
    );
}

#[test]
fn serve_runs_in_foreground_and_initialises() {
    let p = Pgboxdb::new();
    // No init: serve must create the cluster and run the engine in the foreground.
    let mut child = common::spawn_locked(p.cmd(&["serve"]).env("PGBOXDB_PORT", p.port.to_string()))
        .expect("spawn pgboxdb serve");
    // wait for readiness
    let mut ready = false;
    for _ in 0..240 {
        if p.home.join("config.json").is_file()
            && !p.engine_pids().is_empty()
            && p.psql("postgres", "SELECT 1").stdout.trim() == "1"
        {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        ready,
        "pgboxdb serve did not become ready\n{}",
        p.log_tail()
    );
    // The engine is a child of the serve process (foreground), i.e. serve exec'd it.
    assert!(
        p.home.join("data/PG_VERSION").is_file(),
        "serve did not initialise"
    );
    // SIGTERM to the serve process stops the engine cleanly.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let status = child.wait().expect("wait serve");
    let _ = status;
    for _ in 0..200 {
        if p.engine_pids().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        p.engine_pids().is_empty(),
        "engine survived SIGTERM to the foreground process"
    );
}

#[test]
fn init_honors_pgboxdb_password_env() {
    let p = Pgboxdb::new();
    let port = p.port.to_string();
    let r = p.output_retrying_pub(
        || {
            let mut c = p.cmd(&["init", "--port", &port]);
            c.env("PGBOXDB_PASSWORD", "supersecret123");
            c
        },
        "pgboxdb init",
    );
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(p.config()["password"], "supersecret123");
    // too short is rejected
    let p2 = Pgboxdb::new();
    let port2 = p2.port.to_string();
    let r = p2.output_retrying_pub(
        || {
            let mut c = p2.cmd(&["init", "--port", &port2]);
            c.env("PGBOXDB_PASSWORD", "short");
            c
        },
        "pgboxdb init",
    );
    assert_eq!(r.code, 2, "{}", r.all());
    assert!(r.stderr.contains("too short"), "{}", r.stderr);
}

#[test]
fn init_force_refuses_while_recorded_pid_is_alive() {
    // A postmaster.pid naming a live process (here, this test process) must stop
    // `pgboxdb init --force` from deleting the data directory, even though that
    // process is not our engine. Guards against a mis-identified live cluster.
    let p = Pgboxdb::new();
    p.init_ok();
    std::fs::write(p.home.join("data/marker"), b"keep").unwrap();
    let me = std::process::id();
    std::fs::write(
        p.home.join("data/postmaster.pid"),
        format!(
            "{me}\n{}\n1\n{}\n{}\n\n  0 0\nready\n",
            p.home.join("data").display(),
            p.port,
            p.sock_dir().display()
        ),
    )
    .unwrap();
    let port = p.port.to_string();
    let r = p.run(&["init", "--port", &port, "--force"]);
    assert_eq!(r.code, 4, "{}", r.all());
    assert!(r.stderr.contains("alive"), "{}", r.stderr);
    assert!(
        p.home.join("data/marker").is_file(),
        "--force deleted the data dir while a pid was alive"
    );
    // With a dead pid in the file, --force proceeds.
    std::fs::write(
        p.home.join("data/postmaster.pid"),
        format!(
            "999999\n{}\n1\n{}\n{}\n\n  0 0\nready\n",
            p.home.join("data").display(),
            p.port,
            p.sock_dir().display()
        ),
    )
    .unwrap();
    let r = p.run(&["init", "--port", &port, "--force"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(
        !p.home.join("data/marker").exists(),
        "--force should have recreated the data dir"
    );
}

/// A descriptor the CLI inherits without close-on-exec (what a caller's
/// half-initialised pipe or socket looks like under a concurrent fork) must not
/// survive into the detached engine or TTL supervisor. If it does, the caller's
/// `output()` never sees EOF (observed hang) and a leaked listening socket
/// keeps its port busy for as long as the engine lives (observed CI exit 4).
#[test]
fn start_does_not_leak_inherited_fds_into_engine_or_supervisor() {
    use std::os::unix::process::CommandExt;
    let p = Pgboxdb::new();
    p.init_ok();
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (r, w) = (fds[0], fds[1]);
    let mut c = p.cmd(&["start"]);
    // dup2 clears FD_CLOEXEC on the target, so fd 200 is inherited by `pgboxdb start`.
    unsafe {
        c.pre_exec(move || {
            libc::dup2(w, 200);
            Ok(())
        });
    }
    let out = common::Run::from(common::output_locked(&mut c).unwrap());
    assert_eq!(out.code, 0, "{}", out.all());
    unsafe { libc::close(w) };
    assert!(!p.engine_pids().is_empty(), "engine not running");
    assert!(p.wait_supervisor(5), "supervisor not running");
    // Only the parent's write end is closed above; EOF arrives iff no other
    // process still holds fd 200 (or any dup of it).
    let flags = unsafe { libc::fcntl(r, libc::F_GETFL) };
    unsafe { libc::fcntl(r, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut buf = [0u8; 1];
    let eof = loop {
        let n = unsafe { libc::read(r, buf.as_mut_ptr() as *mut libc::c_void, 1) };
        if n == 0 {
            break true;
        }
        if std::time::Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    unsafe { libc::close(r) };
    assert!(
        eof,
        "a descriptor inherited by `pgboxdb start` is still held open by the engine or the \
         supervisor: inherited fds must be closed on exec in those children"
    );
}
