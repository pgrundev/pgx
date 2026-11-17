mod common;
use common::Pgboxdb;

fn up() -> Pgboxdb {
    let p = Pgboxdb::new();
    p.init_ok();
    p.start_ok();
    p
}

#[test]
fn create_then_connect() {
    let p = up();
    let r = p.run(&["create", "myapp"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(r.stdout.contains("myapp"), "{}", r.stdout);
    let q = p.psql(
        "myapp",
        "CREATE TABLE t(id int); INSERT INTO t VALUES (7); SELECT id FROM t",
    );
    assert_eq!(q.code, 0, "{}", q.all());
    assert_eq!(q.stdout.trim(), "7");
    let j = p.run(&["--json", "create", "second"]).json();
    assert_eq!(j["created"], "second");
    assert!(
        j["url"]
            .as_str()
            .unwrap()
            .starts_with("postgres://postgres:"),
        "{j}"
    );
    // Mixed case and hyphens are preserved exactly.
    let r = p.run(&["create", "My-App_2"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(
        p.psql("My-App_2", "SELECT current_database()")
            .stdout
            .trim(),
        "My-App_2"
    );
}

#[test]
fn create_duplicate_is_exit_4() {
    let p = up();
    assert_eq!(p.run(&["create", "dup"]).code, 0);
    let r = p.run(&["create", "dup"]);
    assert_eq!(r.code, 4, "{}", r.all());
    assert!(r.stderr.contains("exists"), "{}", r.stderr);
}

#[test]
fn invalid_name_is_usage_error() {
    let p = up();
    for bad in [
        "a;drop database postgres",
        "",
        "1leading",
        "has space",
        "a\"b",
        "x".repeat(64).as_str(),
        "-dash",
        "é",
    ] {
        let r = p.run(&["create", bad]);
        assert_eq!(r.code, 2, "name {bad:?}: {}", r.all());
    }
    let q = p.psql("postgres", "SELECT count(*) FROM pg_database");
    assert_eq!(
        q.stdout.trim(),
        "3",
        "invalid names must create nothing: {}",
        q.all()
    );
}

#[test]
fn reserved_names_refused() {
    let p = up();
    for name in ["postgres", "template0", "template1"] {
        assert_eq!(p.run(&["create", name]).code, 2, "create {name}");
        assert_eq!(p.run(&["drop", name]).code, 2, "drop {name}");
        assert_eq!(
            p.run(&["drop", name, "--force"]).code,
            2,
            "drop --force {name}"
        );
        assert_eq!(p.run(&["clone", "x", name]).code, 2, "clone onto {name}");
    }
    assert_eq!(
        p.psql("postgres", "SELECT 1").code,
        0,
        "postgres database must still exist"
    );
}

#[test]
fn drop_removes_database_and_files() {
    let p = up();
    assert_eq!(p.run(&["create", "gone"]).code, 0);
    let oid = p
        .psql(
            "postgres",
            "SELECT oid FROM pg_database WHERE datname = 'gone'",
        )
        .stdout
        .trim()
        .to_string();
    assert!(
        p.home.join("data/base").join(&oid).is_dir(),
        "oid dir {oid} missing before drop"
    );
    let r = p.run(&["drop", "gone"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(
        p.psql(
            "postgres",
            "SELECT count(*) FROM pg_database WHERE datname = 'gone'"
        )
        .stdout
        .trim(),
        "0"
    );
    assert!(
        !p.home.join("data/base").join(&oid).exists(),
        "oid dir {oid} survived drop"
    );
    let r = p.run(&["drop", "gone"]);
    assert_eq!(r.code, 5, "{}", r.all());
}

#[test]
fn drop_in_use_requires_force() {
    let p = up();
    assert_eq!(p.run(&["create", "busy"]).code, 0);
    assert_eq!(
        p.psql(
            "busy",
            "CREATE TABLE keep(v int); INSERT INTO keep VALUES (1)"
        )
        .code,
        0
    );
    // Hold a session open on the database.
    let mut holder_cmd = std::process::Command::new(common::find_psql());
    holder_cmd
        .args([
            "-X",
            "-h",
            &p.sock_dir().to_string_lossy(),
            "-p",
            &p.port.to_string(),
            "-U",
            "postgres",
            "-d",
            "busy",
            "-Atc",
            "SELECT pg_sleep(30)",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut holder = common::spawn_locked(&mut holder_cmd).unwrap();
    // wait until the session is visible
    for _ in 0..100 {
        if p.psql(
            "postgres",
            "SELECT count(*) FROM pg_stat_activity WHERE datname = 'busy'",
        )
        .stdout
        .trim()
            == "1"
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let r = p.run(&["drop", "busy"]);
    assert_eq!(r.code, 6, "{}", r.all());
    assert!(r.stderr.contains("--force"), "{}", r.stderr);
    assert_eq!(
        p.psql("busy", "SELECT v FROM keep").stdout.trim(),
        "1",
        "data must be intact after a refused drop"
    );
    let r = p.run(&["drop", "busy", "--force"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(
        p.psql(
            "postgres",
            "SELECT count(*) FROM pg_database WHERE datname = 'busy'"
        )
        .stdout
        .trim(),
        "0"
    );
    let status = holder.wait().unwrap();
    assert!(
        !status.success(),
        "the held session should have been terminated"
    );
}

#[test]
fn url_has_password_and_port() {
    let p = up();
    assert_eq!(p.run(&["create", "app"]).code, 0);
    let r = p.run(&["url", "app"]);
    assert_eq!(r.code, 0, "{}", r.all());
    let url = r.stdout.trim().to_string();
    assert_eq!(
        url,
        format!(
            "postgres://postgres:{}@127.0.0.1:{}/app",
            p.password(),
            p.port
        )
    );
    // The URL works with a real client.
    let out = common::output_locked(std::process::Command::new(common::find_psql()).args([
        "-X",
        "-Atc",
        "SELECT current_database()",
        &url,
    ]))
    .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "app",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = p.run(&["url", "app", "--socket"]);
    assert_eq!(r.code, 0, "{}", r.all());
    let surl = r.stdout.trim().to_string();
    assert!(surl.starts_with("postgres://postgres@/app?host="), "{surl}");
    let out = common::output_locked(std::process::Command::new(common::find_psql()).args([
        "-X",
        "-Atc",
        "SELECT current_database()",
        &surl,
    ]))
    .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "app",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j = p.run(&["--json", "url", "app"]).json();
    assert_eq!(j["url"], url);
    assert_eq!(j["database"], "app");
    assert_eq!(j["port"], p.port);
    let r = p.run(&["url", "nope"]);
    assert_eq!(r.code, 5, "{}", r.all());
}

#[test]
fn ls_json_shape() {
    let p = up();
    assert_eq!(p.run(&["create", "alpha"]).code, 0);
    assert_eq!(p.run(&["create", "beta"]).code, 0);
    let j = p.run(&["--json", "ls"]).json();
    let arr = j.as_array().expect("ls --json is an array");
    let names: Vec<&str> = arr.iter().map(|d| d["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["alpha", "beta", "postgres"], "{names:?}");
    for d in arr {
        assert!(d["size_bytes"].as_u64().unwrap() > 0, "{d}");
        assert!(d["connections"].as_u64().is_some(), "{d}");
        assert_eq!(d["is_template"], false);
    }
    let r = p.run(&["ls"]);
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains("alpha") && r.stdout.contains("beta"),
        "{}",
        r.stdout
    );
}

#[test]
fn db_commands_need_running_runtime() {
    let p = Pgboxdb::new();
    p.init_ok();
    let r = p.run(&["create", "x"]);
    assert_eq!(r.code, 3, "{}", r.all());
    assert!(r.stderr.contains("pgboxdb start"), "{}", r.stderr);
    assert_eq!(p.run(&["ls"]).code, 3);
    assert_eq!(p.run(&["url", "x"]).code, 3);
}

#[test]
fn ls_flags_invalid_databases() {
    let p = up();
    assert_eq!(p.run(&["create", "inv"]).code, 0);
    // What an interrupted DROP DATABASE leaves behind (PostgreSQL 15+ marks the row invalid first).
    assert_eq!(
        p.psql(
            "postgres",
            "UPDATE pg_database SET datconnlimit = -2 WHERE datname = 'inv'"
        )
        .code,
        0
    );
    let j = p.run(&["--json", "ls"]).json();
    let inv = j
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "inv")
        .unwrap();
    assert_eq!(inv["invalid"], true);
    let r = p.run(&["ls"]);
    assert!(r.stdout.contains("invalid"), "{}", r.stdout);
    let r = p.run(&["drop", "inv", "--force"]);
    assert_eq!(r.code, 0, "{}", r.all());
}
