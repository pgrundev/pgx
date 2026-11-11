mod common;
use common::Pgx;

fn up_with_source() -> Pgx {
    let p = Pgx::new();
    p.init_ok();
    p.start_ok();
    assert_eq!(p.run(&["create", "src"]).code, 0);
    let q = p.psql("src", "CREATE TABLE t(id int PRIMARY KEY, v text); INSERT INTO t SELECT g, 'v' || g FROM generate_series(1, 20000) g");
    assert_eq!(q.code, 0, "{}", q.all());
    p
}

fn base_dirs(p: &Pgx) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(p.home.join("data/base"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn clone_is_isolated() {
    let p = up_with_source();
    let r = p.run(&["clone", "src", "c1"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(
        p.psql("c1", "SELECT count(*) FROM t").stdout.trim(),
        "20000"
    );
    assert_eq!(
        p.psql(
            "c1",
            "INSERT INTO t VALUES (99999, 'clone only'); DELETE FROM t WHERE id <= 10"
        )
        .code,
        0
    );
    assert_eq!(
        p.psql("src", "SELECT count(*) FROM t").stdout.trim(),
        "20000",
        "clone write leaked into the source"
    );
    assert_eq!(
        p.psql("src", "SELECT count(*) FROM t WHERE id = 99999")
            .stdout
            .trim(),
        "0"
    );
    assert_eq!(
        p.psql("src", "UPDATE t SET v = 'changed' WHERE id = 20")
            .code,
        0
    );
    assert_eq!(
        p.psql("c1", "SELECT v FROM t WHERE id = 20").stdout.trim(),
        "v20",
        "source write leaked into the clone"
    );
    assert_eq!(
        p.psql("c1", "SELECT count(*) FROM t").stdout.trim(),
        "19991"
    );
}

#[test]
fn clone_json_reports_method_and_timing() {
    let p = up_with_source();
    let j = p.run(&["--json", "clone", "src", "c2"]).json();
    assert_eq!(j["source"], "src");
    assert_eq!(j["target"], "c2");
    assert!(j["elapsed_ms"].as_f64().unwrap() > 0.0, "{j}");
    let method = j["method"].as_str().unwrap();
    assert!(method == "clone" || method == "copy", "{j}");
    if cfg!(target_os = "macos") {
        assert_eq!(method, "clone", "APFS should clone blocks: {j}");
    }
}

#[test]
fn clone_source_in_use_is_exit_6() {
    let p = up_with_source();
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
            "src",
            "-Atc",
            "BEGIN; INSERT INTO t VALUES (50000, 'uncommitted'); SELECT pg_sleep(20)",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut holder = common::spawn_locked(&mut holder_cmd).unwrap();
    for _ in 0..200 {
        if p.psql(
            "postgres",
            "SELECT count(*) FROM pg_stat_activity WHERE datname = 'src'",
        )
        .stdout
        .trim()
            == "1"
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let before = base_dirs(&p);
    let r = p.run(&["clone", "src", "c3"]);
    let _ = holder.kill();
    let _ = holder.wait();
    assert_eq!(r.code, 6, "{}", r.all());
    assert!(r.stderr.contains("connected"), "{}", r.stderr);
    assert_eq!(
        p.psql(
            "postgres",
            "SELECT count(*) FROM pg_database WHERE datname = 'c3'"
        )
        .stdout
        .trim(),
        "0"
    );
    assert_eq!(base_dirs(&p), before, "refused clone left files behind");
}

#[test]
fn clone_missing_source_is_exit_5_and_existing_target_is_exit_4() {
    let p = up_with_source();
    let r = p.run(&["clone", "nope", "c4"]);
    assert_eq!(r.code, 5, "{}", r.all());
    assert_eq!(p.run(&["create", "taken"]).code, 0);
    let before = base_dirs(&p);
    let r = p.run(&["clone", "src", "taken"]);
    assert_eq!(r.code, 4, "{}", r.all());
    assert_eq!(
        base_dirs(&p),
        before,
        "failed clone left a partial directory"
    );
    assert_eq!(
        p.psql(
            "taken",
            "SELECT count(*) FROM pg_tables WHERE tablename = 't'"
        )
        .stdout
        .trim(),
        "0",
        "existing target was overwritten"
    );
}

#[test]
fn failed_clone_leaves_no_partial_dir() {
    // The engine must not leave base/<oid> behind when CREATE DATABASE fails
    // after allocating an oid: provoke a failure with a name that exists.
    let p = up_with_source();
    let before = base_dirs(&p);
    for _ in 0..5 {
        assert_eq!(p.run(&["clone", "src", "src"]).code, 4);
        assert_eq!(p.run(&["clone", "src", "postgres"]).code, 2);
    }
    assert_eq!(base_dirs(&p), before);
}

#[test]
fn concurrent_clones_8() {
    let p = up_with_source();
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let name = format!("cc{i}");
            let mut c = p.cmd(&["--json", "clone", "src", &name]);
            std::thread::spawn(move || {
                (
                    name,
                    common::Run::from(common::output_locked(&mut c).unwrap()),
                )
            })
        })
        .collect();
    let mut names = Vec::new();
    for h in handles {
        let (name, r) = h.join().unwrap();
        assert_eq!(r.code, 0, "clone {name}: {}", r.all());
        names.push(name);
    }
    for (i, name) in names.iter().enumerate() {
        assert_eq!(
            p.psql(name, "SELECT count(*) FROM t").stdout.trim(),
            "20000",
            "{name}"
        );
        assert_eq!(
            p.psql(
                name,
                &format!("INSERT INTO t VALUES (100000 + {i}, 'mine')")
            )
            .code,
            0
        );
    }
    for (i, name) in names.iter().enumerate() {
        assert_eq!(
            p.psql(name, "SELECT count(*) FROM t WHERE id >= 100000")
                .stdout
                .trim(),
            "1",
            "{name} sees another clone's row"
        );
        assert_eq!(
            p.psql(name, "SELECT id FROM t WHERE id >= 100000")
                .stdout
                .trim(),
            (100000 + i).to_string()
        );
    }
    assert_eq!(
        p.psql("src", "SELECT count(*) FROM t WHERE id >= 100000")
            .stdout
            .trim(),
        "0"
    );
    let j = p.run(&["--json", "ls"]).json();
    assert_eq!(j.as_array().unwrap().len(), 1 + 1 + 8, "{j}");
}

/// Racing clones to one name: one wins, every loser is a clean conflict
/// (exit 4) — never a generic failure — so callers can fall back to another
/// name (the agent session path does exactly that).
#[test]
fn concurrent_clones_to_one_name_lose_with_exit_4() {
    let p = up_with_source();
    for round in 0..5 {
        let name = format!("same{round}");
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let mut c = p.cmd(&["clone", "src", &name]);
                std::thread::spawn(move || {
                    common::Run::from(common::output_locked(&mut c).unwrap())
                })
            })
            .collect();
        let runs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let won = runs.iter().filter(|r| r.code == 0).count();
        assert_eq!(won, 1, "round {round}: exactly one clone must win");
        for r in runs.iter().filter(|r| r.code != 0) {
            assert_eq!(
                r.code,
                4,
                "round {round}: a lost race must be exit 4: {}",
                r.all()
            );
        }
    }
}

#[test]
fn clone_then_drop_cycles_leave_no_files() {
    let p = up_with_source();
    let before = base_dirs(&p);
    for i in 0..20 {
        let name = format!("cy{i}");
        assert_eq!(p.run(&["clone", "src", &name]).code, 0, "cycle {i}");
        assert_eq!(
            p.psql(&name, "SELECT count(*) FROM t").stdout.trim(),
            "20000"
        );
        assert_eq!(p.run(&["drop", &name]).code, 0, "cycle {i}");
    }
    assert_eq!(
        base_dirs(&p),
        before,
        "clone/drop cycles leaked base directories"
    );
}
