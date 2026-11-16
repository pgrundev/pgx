//! Black-box tests for `pgboxdb branch <source> <name> [--ttl <dur>]`: a disposable
//! database for one agent, built on the clone path. Real runtime, real TTL
//! expiry, real connections through the printed URL.
mod common;
use common::Pgboxdb;
use std::process::Command;
use std::time::{Duration, Instant};

fn up_with_source() -> Pgboxdb {
    let p = Pgboxdb::new();
    p.init_ok();
    p.start_ok();
    assert_eq!(p.run(&["create", "myapp"]).code, 0);
    let q = p.psql(
        "myapp",
        "CREATE TABLE posts(id int PRIMARY KEY, title text); \
         INSERT INTO posts SELECT g, 'post ' || g FROM generate_series(1, 20000) g",
    );
    assert_eq!(q.code, 0, "{}", q.all());
    p
}

fn base_dirs(p: &Pgboxdb) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(p.home.join("data/base"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn db_exists(p: &Pgboxdb, name: &str) -> bool {
    p.psql(
        "postgres",
        &format!("SELECT count(*) FROM pg_database WHERE datname='{name}'"),
    )
    .stdout
    .trim()
        == "1"
}

fn ls_entry(p: &Pgboxdb, name: &str) -> serde_json::Value {
    let ls = p.run(&["--json", "ls"]).json();
    ls.as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == name)
        .unwrap_or_else(|| panic!("{name} not in pgboxdb ls: {ls}"))
        .clone()
}

/// Run SQL through a connection URL exactly as an agent would use it.
fn psql_url(url: &str, sql: &str) -> common::Run {
    let mut cmd = Command::new(common::find_psql());
    cmd.args(["-X", "-q", "-v", "ON_ERROR_STOP=1", "-Atc", sql, url]);
    common::Run::from(common::output_locked(&mut cmd).expect("spawn psql"))
}

#[test]
fn branch_defaults_to_two_hours_and_prints_a_working_url_last() {
    let p = up_with_source();
    let t0 = Instant::now();
    let r = p.run(&["branch", "myapp", "agent-1"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(
        t0.elapsed() < Duration::from_secs(10),
        "branching a small database took {:?}",
        t0.elapsed()
    );
    let out = r.stdout.trim_end();
    assert!(out.contains("✓ Branch created: agent-1"), "{out}");
    assert!(out.contains("✓ Source: myapp"), "{out}");
    assert!(out.contains("✓ Expires in: 2 hours"), "{out}");
    assert!(out.contains("\nDATABASE_URL:\n"), "{out}");
    let url = out.lines().last().unwrap();
    assert!(
        url.starts_with("postgres://postgres:") && url.ends_with("/agent-1"),
        "last line must be the URL: {url}"
    );
    // The printed URL is a real, working DATABASE_URL (password included).
    let q = psql_url(url, "SELECT count(*) FROM posts");
    assert_eq!(q.code, 0, "{}", q.all());
    assert_eq!(q.stdout.trim(), "20000");
    // The default TTL is recorded as 2h.
    let e = ls_entry(&p, "agent-1");
    assert_eq!(e["ttl"], "2h", "{e}");
    assert_eq!(e["status"], "ready", "{e}");
    let left = e["expires_in_seconds"].as_u64().unwrap();
    assert!((7_000..=7_200).contains(&left), "{e}");
    // The source stays permanent.
    assert_eq!(ls_entry(&p, "myapp")["ttl"], "unlimited");
}

#[test]
fn branch_json_contract() {
    let p = up_with_source();
    let r = p.run(&["--json", "branch", "myapp", "agent-2"]);
    assert_eq!(r.code, 0, "{}", r.all());
    let j = r.json();
    assert_eq!(j["branch"], "agent-2");
    assert_eq!(j["database"], "agent-2");
    assert_eq!(j["source"], "myapp");
    assert_eq!(j["ttl"], "2h");
    assert_eq!(j["ttl_seconds"], 7_200);
    assert!(j["expires_at"].is_string(), "{j}");
    assert!(j["expires_in_seconds"].as_u64().unwrap() <= 7_200, "{j}");
    assert_eq!(j["host"], "127.0.0.1");
    assert_eq!(j["port"], p.port);
    assert_eq!(j["user"], "postgres");
    assert!(j["elapsed_ms"].as_f64().unwrap() > 0.0, "{j}");
    let method = j["method"].as_str().unwrap();
    assert!(method == "clone" || method == "copy", "{j}");
    let url = j["url"].as_str().unwrap();
    assert!(url.ends_with("/agent-2"), "{j}");
    assert_eq!(
        psql_url(url, "SELECT count(*) FROM posts").stdout.trim(),
        "20000"
    );
    assert!(
        j["socket_url"]
            .as_str()
            .unwrap()
            .starts_with("postgres://postgres@/agent-2?host="),
        "{j}"
    );
    // Nothing but the JSON object on stdout.
    assert_eq!(r.stdout.trim().lines().count(), 1, "{}", r.stdout);
}

#[test]
fn branch_custom_ttl_and_unlimited() {
    let p = up_with_source();
    let r = p.run(&["branch", "myapp", "agent-3", "--ttl", "30m"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(
        r.stdout.contains("✓ Expires in: 30 minutes"),
        "{}",
        r.stdout
    );
    assert_eq!(ls_entry(&p, "agent-3")["ttl"], "30m");

    let r = p.run(&["branch", "myapp", "keeper", "--ttl", "unlimited"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(r.stdout.contains("✓ Expires in: never"), "{}", r.stdout);
    assert_eq!(ls_entry(&p, "keeper")["ttl"], "unlimited");
    assert!(p
        .run(&["--json", "branch", "myapp", "k2", "--ttl", "unlimited"])
        .json()["expires_at"]
        .is_null());

    // A bad TTL is a usage error and creates nothing.
    let before = base_dirs(&p);
    let r = p.run(&["branch", "myapp", "agent-4", "--ttl", "5"]);
    assert_eq!(r.code, 2, "{}", r.all());
    assert!(!db_exists(&p, "agent-4"));
    assert_eq!(base_dirs(&p), before);
}

#[test]
fn branch_existing_name_is_refused_without_changes() {
    let p = up_with_source();
    assert_eq!(p.run(&["create", "taken"]).code, 0);
    assert_eq!(
        p.psql(
            "taken",
            "CREATE TABLE mine(id int); INSERT INTO mine VALUES (1)"
        )
        .code,
        0
    );
    let before = base_dirs(&p);
    let r = p.run(&["branch", "myapp", "taken"]);
    assert_eq!(r.code, 4, "{}", r.all());
    assert!(r.stderr.contains("already exists"), "{}", r.stderr);
    // The existing database is untouched, keeps its data and stays permanent.
    assert_eq!(
        p.psql("taken", "SELECT count(*) FROM mine").stdout.trim(),
        "1"
    );
    assert_eq!(
        p.psql(
            "taken",
            "SELECT count(*) FROM pg_tables WHERE tablename = 'posts'"
        )
        .stdout
        .trim(),
        "0"
    );
    assert_eq!(ls_entry(&p, "taken")["ttl"], "unlimited");
    assert_eq!(
        base_dirs(&p),
        before,
        "refused branch changed the data directory"
    );
    // Same for the source's own name, and with --json the error is one object.
    let r = p.run(&["--json", "branch", "myapp", "myapp"]);
    assert_eq!(r.code, 4, "{}", r.all());
    let j = r.json();
    assert_eq!(j["error"]["code"], "conflict", "{j}");
    assert_eq!(j["error"]["exit_code"], 4, "{j}");
    assert!(
        j["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already exists"),
        "{j}"
    );
    assert_eq!(ls_entry(&p, "myapp")["ttl"], "unlimited");
}

#[test]
fn branch_missing_source_is_exit_5_and_reserved_name_is_exit_2() {
    let p = up_with_source();
    let before = base_dirs(&p);
    let r = p.run(&["branch", "nope", "agent-1"]);
    assert_eq!(r.code, 5, "{}", r.all());
    assert!(!db_exists(&p, "agent-1"));
    let r = p.run(&["branch", "myapp", "postgres"]);
    assert_eq!(r.code, 2, "{}", r.all());
    let r = p.run(&["branch", "myapp", "template1"]);
    assert_eq!(r.code, 2, "{}", r.all());
    assert_eq!(base_dirs(&p), before);
}

#[test]
fn branches_are_isolated_from_the_source_and_each_other() {
    let p = up_with_source();
    for a in ["agent-1", "agent-2"] {
        let r = p.run(&["branch", "myapp", a]);
        assert_eq!(r.code, 0, "{}", r.all());
    }
    let w = p.psql(
        "agent-1",
        "DELETE FROM posts WHERE id <= 5000; ALTER TABLE posts ADD COLUMN draft bool DEFAULT false; CREATE TABLE agent_only(id int)",
    );
    assert_eq!(w.code, 0, "{}", w.all());
    assert_eq!(
        p.psql("agent-1", "SELECT count(*) FROM posts")
            .stdout
            .trim(),
        "15000"
    );
    for other in ["agent-2", "myapp"] {
        assert_eq!(
            p.psql(other, "SELECT count(*) FROM posts").stdout.trim(),
            "20000",
            "{other} saw agent-1's writes"
        );
        assert_eq!(
            p.psql(
                other,
                "SELECT count(*) FROM pg_tables WHERE tablename = 'agent_only'"
            )
            .stdout
            .trim(),
            "0",
            "{other}"
        );
    }
    // Writes to the source after branching do not reach the branches.
    assert_eq!(
        p.psql(
            "myapp",
            "UPDATE posts SET title = 'edited' WHERE id = 20000"
        )
        .code,
        0
    );
    assert_eq!(
        p.psql("agent-2", "SELECT title FROM posts WHERE id = 20000")
            .stdout
            .trim(),
        "post 20000"
    );
}

#[test]
fn branch_expires_on_its_own_and_the_source_survives() {
    let p = up_with_source();
    let r = p.run(&["branch", "myapp", "short", "--ttl", "3s"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(r.stdout.contains("✓ Expires in: 3 seconds"), "{}", r.stdout);
    let deadline = Instant::now() + Duration::from_secs(25);
    while db_exists(&p, "short") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(400));
    }
    assert!(
        !db_exists(&p, "short"),
        "branch must expire on its own:\n{}",
        p.log_tail()
    );
    assert!(db_exists(&p, "myapp"));
    assert_eq!(
        p.psql("myapp", "SELECT count(*) FROM posts").stdout.trim(),
        "20000"
    );
}

#[test]
fn branch_needs_a_running_runtime_and_help_mentions_it() {
    let p = Pgboxdb::new();
    let h = p.run(&["help"]);
    assert!(
        h.stdout.contains("pgboxdb branch <source> <name>"),
        "{}",
        h.stdout
    );
    let r = p.run(&["branch", "myapp", "agent-1"]);
    assert_eq!(r.code, 3, "not initialised: {}", r.all());
    p.init_ok();
    let r = p.run(&["branch", "myapp", "agent-1"]);
    assert_eq!(r.code, 3, "not running: {}", r.all());
}
