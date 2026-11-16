//! Black-box tests for `pgboxdb agent exec -- <command>`: a branch for the
//! lifetime of any process, with the environment, signals and exit status
//! handled the way a wrapper must.
mod common;
use common::Pgboxdb;
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn up_with_source() -> (Pgboxdb, PathBuf) {
    let p = Pgboxdb::new();
    p.init_ok();
    p.start_ok();
    assert_eq!(p.run(&["create", "myapp"]).code, 0);
    assert_eq!(
        p.psql("myapp", "CREATE TABLE posts(id int PRIMARY KEY)")
            .code,
        0
    );
    let proj = p.home.parent().unwrap().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join(".pgboxdb.toml"), "[agent]\nsource = \"myapp\"\n").unwrap();
    (p, proj)
}

fn path_with_psql() -> String {
    format!(
        "{}:{}",
        common::find_psql().parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

fn exec_cmd(p: &Pgboxdb, proj: &std::path::Path, env: &[(&str, &str)], args: &[&str]) -> Command {
    let mut full = vec!["agent", "exec"];
    full.extend_from_slice(args);
    let mut c = p.cmd(&full);
    c.current_dir(proj).env("PATH", path_with_psql());
    for (k, v) in env {
        c.env(k, v);
    }
    c
}

fn exec(p: &Pgboxdb, proj: &std::path::Path, env: &[(&str, &str)], args: &[&str]) -> common::Run {
    p.output_retrying_pub(|| exec_cmd(p, proj, env, args), "pgboxdb agent exec")
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

fn agent_ls(p: &Pgboxdb) -> Vec<Value> {
    p.run(&["--json", "agent", "ls"])
        .json()
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn exec_gives_the_child_its_own_branch_and_retires_it() {
    let (p, proj) = up_with_source();
    let r = exec(
        &p,
        &proj,
        &[],
        &["--", "sh", "-c", "psql \"$DATABASE_URL\" -X -Atc 'create table t(x int)' && echo BR=$PGBOXDB_BRANCH SRC=$PGBOXDB_SOURCE DB=$PGDATABASE ISO=$PGBOXDB_ISOLATION SESSION=$PGBOXDB_AGENT_SESSION"],
    );
    assert_eq!(r.code, 0, "{}", r.all());
    let line = r
        .stdout
        .lines()
        .find(|l| l.starts_with("BR="))
        .unwrap_or_else(|| panic!("{}", r.all()));
    // The host label is `exec` whatever the command is called, so `pgboxdb agent
    // down --id` can find the branch later.
    assert!(line.contains("BR=agent-exec-"), "{line}");
    assert!(line.contains("SRC=myapp"), "{line}");
    assert!(line.contains("ISO=ok"), "{line}");
    assert!(line.contains("SESSION="), "{line}");
    let branch = line
        .split_whitespace()
        .next()
        .unwrap()
        .trim_start_matches("BR=")
        .to_string();
    assert!(line.contains(&format!("DB={branch}")), "{line}");
    assert!(
        !r.stdout.contains(&p.password()),
        "the wrapper never prints the URL"
    );
    // Gone when the command exits; the template is untouched.
    assert!(!db_exists(&p, &branch));
    assert!(agent_ls(&p).is_empty());
    assert_eq!(
        p.psql(
            "myapp",
            "SELECT count(*) FROM pg_tables WHERE tablename='t'"
        )
        .stdout
        .trim(),
        "0"
    );
}

#[test]
fn exec_json_summary_and_host_label() {
    let (p, proj) = up_with_source();
    let r = p.output_retrying_pub(
        || {
            let mut c = p.cmd(&[
                "--json", "agent", "exec", "--host", "codex", "--", "sh", "-c", "exit 3",
            ]);
            c.current_dir(&proj);
            c
        },
        "pgboxdb agent exec",
    );
    assert_eq!(r.code, 3, "{}", r.all());
    let j = r.json();
    assert!(
        j["branch"].as_str().unwrap().starts_with("agent-codex-"),
        "{j}"
    );
    assert_eq!(j["host"], "codex");
    assert_eq!(j["exit_code"], 3);
    assert_eq!(j["dropped"], true);
    assert!(j.get("url").is_none(), "{j}");
}

#[test]
fn exec_propagates_exit_code_and_still_cleans_up() {
    let (p, proj) = up_with_source();
    let r = exec(&p, &proj, &[], &["--", "sh", "-c", "exit 7"]);
    assert_eq!(r.code, 7, "{}", r.all());
    assert!(agent_ls(&p).is_empty());
    // A command that cannot be spawned is a normal error, not a leaked branch.
    let r = exec(&p, &proj, &[], &["--", "/nonexistent/binary"]);
    assert_ne!(r.code, 0);
    assert!(agent_ls(&p).is_empty());
}

#[test]
fn exec_forwards_sigterm_and_exits_143() {
    let (p, proj) = up_with_source();
    let mut c = exec_cmd(&p, &proj, &[], &["--", "sh", "-c", "echo READY; sleep 30"]);
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = common::spawn_locked(&mut c).unwrap();
    // Wait until the branch exists (the child is running).
    let t0 = Instant::now();
    while agent_ls(&p).is_empty() && t0.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(
        agent_ls(&p).len(),
        1,
        "branch should exist while the child runs"
    );
    let branch = agent_ls(&p)[0]["name"].as_str().unwrap().to_string();
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let out = child.wait_with_output().unwrap();
    let r = common::Run::from(out);
    assert_eq!(r.code, 143, "{}", r.all());
    assert!(
        !db_exists(&p, &branch),
        "branch must be dropped after SIGTERM"
    );
    assert!(agent_ls(&p).is_empty());
}

#[test]
fn exec_attaches_to_the_surrounding_session() {
    let (p, proj) = up_with_source();
    let sid = "0199c3a2-7b1e-7c3f-9d2a-5e6f7a8b9c0d";
    let env = [("CODEX_SESSION_ID", sid)];
    let a = exec(
        &p,
        &proj,
        &env,
        &[
            "--",
            "sh",
            "-c",
            "echo $PGBOXDB_BRANCH; psql \"$DATABASE_URL\" -X -Atc 'create table keep(x int)'",
        ],
    );
    assert_eq!(a.code, 0, "{}", a.all());
    let branch = a.stdout.lines().next().unwrap().trim().to_string();
    assert_eq!(branch, "agent-codex-0199c3a2");
    // Still there: an attached exec never retires the session's branch.
    assert!(db_exists(&p, &branch));
    assert_eq!(agent_ls(&p)[0]["state"], "active");
    let b = exec(
        &p,
        &proj,
        &env,
        &[
            "--",
            "sh",
            "-c",
            "echo $PGBOXDB_BRANCH; psql \"$DATABASE_URL\" -X -Atc 'select count(*) from keep'",
        ],
    );
    assert_eq!(b.code, 0, "{}", b.all());
    assert_eq!(b.stdout.lines().next().unwrap().trim(), branch);
    assert_eq!(
        b.stdout.lines().nth(1).unwrap().trim(),
        "0",
        "same branch, table persisted"
    );
    // Claude's variable works the same way.
    let c = exec(
        &p,
        &proj,
        &[(
            "CLAUDE_CODE_SESSION_ID",
            "9b1f3c2d-aaaa-bbbb-cccc-dddddddddddd",
        )],
        &["--", "sh", "-c", "echo $PGBOXDB_BRANCH"],
    );
    assert_eq!(c.stdout.trim(), "agent-claude-9b1f3c2d", "{}", c.all());
    assert!(db_exists(&p, "agent-claude-9b1f3c2d"));
}

#[test]
fn exec_keep_leaves_the_branch_active_for_its_idle_ttl() {
    let (p, proj) = up_with_source();
    let r = exec(
        &p,
        &proj,
        &[],
        &["--keep", "--id", "keepme-01", "--ttl", "1h", "--", "true"],
    );
    assert_eq!(r.code, 0, "{}", r.all());
    let rows = agent_ls(&p);
    assert_eq!(rows.len(), 1, "{rows:?}");
    // `--keep` means: the branch stays, active, on its idle TTL; nothing
    // about it is "retired" (that word is for a session that ended).
    assert_eq!(rows[0]["state"], "active");
    assert_eq!(rows[0]["deadline_kind"], "idle_ttl");
    let left = rows[0]["expires_in_seconds"].as_u64().unwrap();
    assert!(left > 3000 && left <= 3600, "{left}");
    assert_eq!(rows[0]["agent"]["session_id"], "keepme-01");
    assert_eq!(rows[0]["agent"]["host"], "exec");
    // Without --keep the branch is dropped at exit (as before).
    let r = exec(&p, &proj, &[], &["--id", "dropme-01", "--", "true"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(agent_ls(&p).len(), 1);
}

#[test]
fn exec_two_parallel_wrappers_are_isolated() {
    let (p, proj) = up_with_source();
    let mut kids = Vec::new();
    for i in 0..4 {
        let script = format!(
            "psql \"$DATABASE_URL\" -X -Atc 'create table only_{i}(x int); insert into only_{i} values ({i})' >/dev/null && psql \"$DATABASE_URL\" -X -Atc \"select count(*) from pg_tables where tablename like 'only_%'\" && echo $PGBOXDB_BRANCH"
        );
        let mut c = exec_cmd(&p, &proj, &[], &["--keep", "--", "sh", "-c", &script]);
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        kids.push(common::spawn_locked(&mut c).unwrap());
    }
    let mut branches = std::collections::BTreeSet::new();
    for k in kids {
        let r = common::Run::from(k.wait_with_output().unwrap());
        assert_eq!(r.code, 0, "{}", r.all());
        let mut lines = r.stdout.lines();
        assert_eq!(
            lines.next().unwrap().trim(),
            "1",
            "each branch sees only its own table: {}",
            r.all()
        );
        branches.insert(lines.next().unwrap().trim().to_string());
    }
    assert_eq!(branches.len(), 4, "{branches:?}");
    assert_eq!(
        p.psql(
            "myapp",
            "SELECT count(*) FROM pg_tables WHERE tablename LIKE 'only_%'"
        )
        .stdout
        .trim(),
        "0"
    );
}

#[test]
fn exec_with_a_very_short_ttl_keeps_the_branch_alive_for_the_whole_command() {
    // The heartbeat must beat faster than the TTL, or the supervisor removes
    // the branch under a running command.
    let (p, proj) = up_with_source();
    let r = exec(
        &p,
        &proj,
        &[],
        &[
            "--ttl",
            "6s",
            "--",
            "sh",
            "-c",
            "sleep 9; psql \"$DATABASE_URL\" -X -Atc 'select 1'",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(r.stdout.trim(), "1", "{}", r.all());
}
