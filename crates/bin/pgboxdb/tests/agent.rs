//! Black-box tests for the `pgboxdb agent` lifecycle primitives: `up`, `touch`,
//! `down`, `env`, `ls`. Real runtime, real clones, real expiry.
mod common;
use common::PGBoxDB;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A runtime with a `myapp` template holding 20k rows, and an empty project dir.
fn up_with_source() -> (PGBoxDB, PathBuf) {
    let p = PGBoxDB::new();
    p.init_ok();
    p.start_ok();
    assert_eq!(p.run(&["create", "myapp"]).code, 0);
    let q = p.psql(
        "myapp",
        "CREATE TABLE posts(id int PRIMARY KEY, title text); \
         INSERT INTO posts SELECT g, 'post ' || g FROM generate_series(1, 20000) g",
    );
    assert_eq!(q.code, 0, "{}", q.all());
    let proj = p.home.parent().unwrap().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    (p, proj)
}

/// `pgboxdb --json agent <args>` run from `dir` with extra environment.
fn agent(p: &PGBoxDB, dir: &Path, env: &[(&str, &str)], args: &[&str]) -> common::Run {
    p.output_retrying_pub(
        || {
            let mut full = vec!["--json", "agent"];
            full.extend_from_slice(args);
            let mut c = p.cmd(&full);
            c.current_dir(dir);
            for (k, v) in env {
                c.env(k, v);
            }
            c
        },
        "pgboxdb agent",
    )
}

fn psql_url(url: &str, sql: &str) -> common::Run {
    let mut cmd = Command::new(common::find_psql());
    cmd.args(["-X", "-q", "-v", "ON_ERROR_STOP=1", "-Atc", sql, url]);
    common::Run::from(common::output_locked(&mut cmd).expect("spawn psql"))
}

fn db_exists(p: &PGBoxDB, name: &str) -> bool {
    p.psql(
        "postgres",
        &format!("SELECT count(*) FROM pg_database WHERE datname='{name}'"),
    )
    .stdout
    .trim()
        == "1"
}

fn fingerprint(p: &PGBoxDB, db: &str) -> String {
    p.psql(
        db,
        "SELECT count(*), coalesce(sum(id),0), md5(string_agg(title, ',' ORDER BY id)) FROM posts",
    )
    .stdout
    .trim()
    .to_string()
}

fn agent_ls(p: &PGBoxDB, dir: &Path) -> Vec<serde_json::Value> {
    agent(p, dir, &[], &["ls"])
        .json()
        .as_array()
        .unwrap()
        .clone()
}

const SRC: [(&str, &str); 1] = [("PGBOXDB_SOURCE", "myapp")];

#[test]
fn up_is_idempotent_per_session_and_isolated_across_sessions() {
    let (p, proj) = up_with_source();
    let before = fingerprint(&p, "myapp");
    let a1 = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "9b1f3c2d-1111", "--host", "claude"],
    );
    assert_eq!(a1.code, 0, "{}", a1.all());
    let a1 = a1.json();
    assert_eq!(a1["branch"], "agent-claude-9b1f3c2d", "{a1}");
    assert_eq!(a1["created"], true, "{a1}");
    assert_eq!(a1["source"], "myapp", "{a1}");
    assert_eq!(a1["host"], "claude", "{a1}");
    assert_eq!(a1["session_id"], "9b1f3c2d-1111", "{a1}");
    assert_eq!(a1["idle_ttl"], "2h", "{a1}");
    assert_eq!(a1["project_id"].as_str().unwrap().len(), 16, "{a1}");
    let left = a1["expires_in_seconds"].as_u64().unwrap();
    assert!((7_000..=7_200).contains(&left), "{a1}");
    let url = a1["url"].as_str().unwrap().to_string();
    assert!(url.ends_with("/agent-claude-9b1f3c2d"), "{url}");

    // Second `up` for the same session: the same branch, nothing created.
    let a2 = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "9b1f3c2d-1111", "--host", "claude"],
    )
    .json();
    assert_eq!(a2["branch"], a1["branch"]);
    assert_eq!(a2["created"], false, "{a2}");
    assert_eq!(a2["url"], a1["url"]);

    // Another session: another branch, invisible changes.
    let b = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "4e77a0c1-2222", "--host", "claude"],
    )
    .json();
    assert_eq!(b["branch"], "agent-claude-4e77a0c1", "{b}");
    assert_eq!(b["created"], true);
    let w = psql_url(&url, "INSERT INTO posts VALUES (99999, 'only in A')");
    assert_eq!(w.code, 0, "{}", w.all());
    let r = psql_url(
        b["url"].as_str().unwrap(),
        "SELECT count(*) FROM posts WHERE id = 99999",
    );
    assert_eq!(r.stdout.trim(), "0", "{}", r.all());
    // The template is read only.
    assert_eq!(fingerprint(&p, "myapp"), before);
}

#[test]
fn up_without_source_uses_app_and_creates_it_empty() {
    let p = PGBoxDB::new();
    p.init_ok();
    p.start_ok();
    let proj = p.home.parent().unwrap().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(!db_exists(&p, "app"));
    let r = agent(
        &p,
        &proj,
        &[],
        &["up", "--id", "abcdef01", "--host", "codex"],
    );
    assert_eq!(r.code, 0, "{}", r.all());
    let j = r.json();
    assert_eq!(j["source"], "app", "{j}");
    assert_eq!(j["source_created"], true, "{j}");
    assert_eq!(j["branch"], "agent-codex-abcdef01", "{j}");
    assert!(db_exists(&p, "app"));
    // An explicit missing source is an error with the pgboxdb copy hint.
    let r = agent(
        &p,
        &proj,
        &[],
        &[
            "up", "--id", "abcdef02", "--host", "codex", "--source", "nope",
        ],
    );
    assert_eq!(r.code, 5, "{}", r.all());
    assert!(
        r.json()["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("pgboxdb copy"),
        "{}",
        r.all()
    );
    assert!(!db_exists(&p, "agent-codex-abcdef02"));
}

#[test]
fn up_reads_source_and_ttl_from_pgboxdb_toml() {
    let (p, proj) = up_with_source();
    std::fs::write(
        proj.join(".pgboxdb.toml"),
        "[agent]\nsource = \"myapp\"\nidle_ttl = \"1h\"\n",
    )
    .unwrap();
    let deep = proj.join("src/deep");
    std::fs::create_dir_all(&deep).unwrap();
    let j = agent(
        &p,
        &deep,
        &[],
        &["up", "--id", "aaaa1111", "--host", "cursor"],
    )
    .json();
    assert_eq!(j["source"], "myapp", "{j}");
    assert_eq!(j["idle_ttl"], "1h", "{j}");
    assert_eq!(
        j["project_root"],
        proj.canonicalize().unwrap().to_string_lossy().to_string(),
        "{j}"
    );
    // --ttl unlimited is refused: agent branches always expire.
    let r = agent(
        &p,
        &deep,
        &[],
        &[
            "up",
            "--id",
            "aaaa2222",
            "--host",
            "cursor",
            "--ttl",
            "unlimited",
        ],
    );
    assert_eq!(r.code, 2, "{}", r.all());
}

#[test]
fn up_refuses_a_busy_source_with_exit_6() {
    let (p, proj) = up_with_source();
    // Hold a session open on the template.
    let mut hold = Command::new(common::find_psql());
    hold.args([
        "-X",
        "-h",
        p.sock_dir().to_str().unwrap(),
        "-p",
        &p.port.to_string(),
        "-U",
        "postgres",
        "-d",
        "myapp",
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let mut child = common::spawn_locked(&mut hold).expect("psql");
    // Wait until the backend shows up.
    let t0 = Instant::now();
    loop {
        let n = p
            .psql(
                "postgres",
                "SELECT count(*) FROM pg_stat_activity WHERE datname='myapp'",
            )
            .stdout
            .trim()
            .to_string();
        if n == "1" || t0.elapsed() > Duration::from_secs(10) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let r = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "bbbb1111", "--host", "claude"],
    );
    assert_eq!(r.code, 6, "{}", r.all());
    let msg = r.json()["error"]["message"].as_str().unwrap().to_string();
    assert!(msg.contains("must be idle"), "{msg}");
    assert!(!db_exists(&p, "agent-claude-bbbb1111"));
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn touch_extends_and_rate_limits() {
    let (p, proj) = up_with_source();
    let j = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "cccc1111", "--host", "claude", "--ttl", "90s"],
    )
    .json();
    let first = j["expires_at"].as_str().unwrap().to_string();
    // Immediately: skipped (under 30 s since the last activity).
    let t = agent(
        &p,
        &proj,
        &SRC,
        &["touch", "--id", "cccc1111", "--host", "claude"],
    )
    .json();
    assert_eq!(t["skipped"], true, "{t}");
    assert_eq!(t["branch"], "agent-claude-cccc1111", "{t}");
    // Pretend the last touch was long ago by editing the record's last_touch.
    let ttl_dir = p.home.join("ttl");
    for e in std::fs::read_dir(&ttl_dir).unwrap() {
        let path = e.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        if text.contains("cccc1111") {
            let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
            v["agent"]["last_touch"] = serde_json::json!(1_000_000);
            std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        }
    }
    std::thread::sleep(Duration::from_millis(1100));
    let t = agent(
        &p,
        &proj,
        &SRC,
        &["touch", "--id", "cccc1111", "--host", "claude"],
    )
    .json();
    assert_eq!(t["skipped"], false, "{t}");
    assert!(
        t["expires_at"].as_str().unwrap() > first.as_str(),
        "{t} vs {first}"
    );
    // Touching an unknown session is not an error from the CLI's point of view.
    let t = agent(
        &p,
        &proj,
        &SRC,
        &["touch", "--id", "nobody", "--host", "claude"],
    );
    assert_eq!(t.code, 0, "{}", t.all());
    assert!(t.json()["branch"].is_null());
}

#[test]
fn down_retires_with_grace_then_up_revives() {
    let (p, proj) = up_with_source();
    let j = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "dddd1111", "--host", "claude"],
    )
    .json();
    let branch = j["branch"].as_str().unwrap().to_string();
    let d = agent(
        &p,
        &proj,
        &SRC,
        &["down", "--id", "dddd1111", "--host", "claude"],
    )
    .json();
    assert_eq!(d["branch"], branch, "{d}");
    assert_eq!(d["dropped"], false, "{d}");
    let until = d["retired_until_seconds"].as_u64().unwrap();
    assert!((590..=600).contains(&until), "{d}");
    assert!(db_exists(&p, &branch));
    let row = agent_ls(&p, &proj)
        .into_iter()
        .find(|r| r["name"] == branch)
        .unwrap();
    assert_eq!(row["state"], "retired", "{row}");
    // A new session start within the grace period gets the same branch back.
    let j2 = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "dddd1111", "--host", "claude"],
    )
    .json();
    assert_eq!(j2["branch"], branch);
    assert_eq!(j2["created"], false, "{j2}");
    let left = j2["expires_in_seconds"].as_u64().unwrap();
    assert!(left > 7_000, "{j2}");
    let row = agent_ls(&p, &proj)
        .into_iter()
        .find(|r| r["name"] == branch)
        .unwrap();
    assert_eq!(row["state"], "active", "{row}");
    // --now drops immediately, terminating sessions on that branch only.
    let d = agent(
        &p,
        &proj,
        &SRC,
        &["down", "--id", "dddd1111", "--host", "claude", "--now"],
    )
    .json();
    assert_eq!(d["dropped"], true, "{d}");
    assert!(!db_exists(&p, &branch));
    assert!(db_exists(&p, "myapp"));
    // Down for an unknown session: exit 5 from the CLI.
    let d = agent(
        &p,
        &proj,
        &SRC,
        &["down", "--id", "nobody", "--host", "claude"],
    );
    assert_eq!(d.code, 5, "{}", d.all());
}

#[test]
fn down_never_drops_a_database_it_does_not_own() {
    let (p, proj) = up_with_source();
    // A hand-made database with an agent-like name and no agent record.
    assert_eq!(
        p.run(&["create", "agent-claude-eeee1111", "--ttl", "1h"])
            .code,
        0
    );
    let d = agent(
        &p,
        &proj,
        &SRC,
        &["down", "--id", "eeee1111-xxxx", "--host", "claude", "--now"],
    );
    assert_eq!(d.code, 5, "{}", d.all());
    assert!(db_exists(&p, "agent-claude-eeee1111"));
    // `up` for that session treats the name as foreign and picks the long one.
    let j = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "eeee1111-xxxx", "--host", "claude"],
    )
    .json();
    let b = j["branch"].as_str().unwrap();
    assert!(b.starts_with("agent-claude-eeee1111-"), "{j}");
    assert_eq!(b.len(), "agent-claude-eeee1111-".len() + 16, "{j}");
    assert!(db_exists(&p, "agent-claude-eeee1111"));
}

#[test]
fn expired_agent_branch_is_removed_by_the_supervisor() {
    let (p, proj) = up_with_source();
    let j = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "ffff1111", "--host", "claude", "--ttl", "3s"],
    )
    .json();
    let branch = j["branch"].as_str().unwrap().to_string();
    assert!(db_exists(&p, &branch));
    let t0 = Instant::now();
    while db_exists(&p, &branch) && t0.elapsed() < Duration::from_secs(25) {
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(!db_exists(&p, &branch), "branch outlived its TTL");
    assert!(db_exists(&p, "myapp"));
    assert!(agent_ls(&p, &proj).iter().all(|r| r["name"] != branch));
}

#[test]
fn env_prints_exports_for_the_current_session() {
    let (p, proj) = up_with_source();
    let env = [
        ("PGBOXDB_SOURCE", "myapp"),
        ("CLAUDE_CODE_SESSION_ID", "0a0b0c0d-1234"),
    ];
    // `--env` on up prints the export lines; `env` reads the host's session var.
    let r = p.output_retrying_pub(
        || {
            let mut c = p.cmd(&["agent", "up", "--env"]);
            c.current_dir(&proj);
            for (k, v) in env {
                c.env(k, v);
            }
            c
        },
        "pgboxdb agent up --env",
    );
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(
        r.stdout
            .starts_with("export DATABASE_URL='postgres://postgres:"),
        "{}",
        r.stdout
    );
    assert!(
        r.stdout
            .contains("export PGBOXDB_BRANCH='agent-claude-0a0b0c0d'"),
        "{}",
        r.stdout
    );
    assert!(
        r.stdout
            .contains("export PGDATABASE='agent-claude-0a0b0c0d'"),
        "{}",
        r.stdout
    );
    assert!(
        r.stdout.contains("export PGBOXDB_SOURCE='myapp'"),
        "{}",
        r.stdout
    );
    assert!(r.stdout.contains("export PGPORT='"), "{}", r.stdout);
    let r2 = p.output_retrying_pub(
        || {
            let mut c = p.cmd(&["agent", "env"]);
            c.current_dir(&proj);
            for (k, v) in env {
                c.env(k, v);
            }
            c
        },
        "pgboxdb agent env",
    );
    assert_eq!(r2.stdout, r.stdout, "{}", r2.all());
    // Evaluating the lines yields a working DATABASE_URL.
    let sh = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!(
            "{}\npsql -X -Atc 'select current_database()' \"$DATABASE_URL\"",
            r.stdout
        ))
        .env(
            "PATH",
            format!(
                "{}:{}",
                common::find_psql().parent().unwrap().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&sh.stdout).trim(),
        "agent-claude-0a0b0c0d"
    );
    // No session anywhere: a usage error that says how to pass one.
    let r3 = agent(&p, &proj, &[], &["env"]);
    assert_eq!(r3.code, 2, "{}", r3.all());
}

#[test]
fn ttl_change_keeps_agent_metadata() {
    let (p, proj) = up_with_source();
    let j = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "a1a1a1a1", "--host", "claude"],
    )
    .json();
    let branch = j["branch"].as_str().unwrap().to_string();
    assert_eq!(p.run(&["ttl", &branch, "4h"]).code, 0);
    let row = agent_ls(&p, &proj)
        .into_iter()
        .find(|r| r["name"] == branch)
        .expect("still an agent branch");
    assert_eq!(row["agent"]["host"], "claude", "{row}");
    let d = agent(
        &p,
        &proj,
        &SRC,
        &["down", "--id", "a1a1a1a1", "--host", "claude", "--now"],
    )
    .json();
    assert_eq!(d["dropped"], true, "{d}");
}

#[test]
fn concurrent_ups_for_one_session_make_one_branch() {
    let (p, proj) = up_with_source();
    let mut kids = Vec::new();
    for _ in 0..6 {
        let mut c = p.cmd(&[
            "--json",
            "agent",
            "up",
            "--id",
            "c0c0c0c0-race",
            "--host",
            "cursor",
        ]);
        c.current_dir(&proj).env("PGBOXDB_SOURCE", "myapp");
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        kids.push(common::spawn_locked(&mut c).unwrap());
    }
    let mut created = 0;
    let mut names = std::collections::BTreeSet::new();
    for k in kids {
        let r = common::Run::from(k.wait_with_output().unwrap());
        assert_eq!(r.code, 0, "{}", r.all());
        let j = r.json();
        names.insert(j["branch"].as_str().unwrap().to_string());
        if j["created"] == true {
            created += 1;
        }
    }
    assert_eq!(names.len(), 1, "{names:?}");
    assert_eq!(created, 1);
    let agents: Vec<_> = agent_ls(&p, &proj)
        .into_iter()
        .filter(|r| r["agent"]["session_id"] == "c0c0c0c0-race")
        .collect();
    assert_eq!(agents.len(), 1);
}

#[test]
fn ls_lists_only_agent_branches() {
    let (p, proj) = up_with_source();
    assert_eq!(p.run(&["create", "plain", "--ttl", "1h"]).code, 0);
    agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "b2b2b2b2", "--host", "codex"],
    );
    let rows = agent_ls(&p, &proj);
    assert!(
        rows.iter().any(|r| r["name"] == "agent-codex-b2b2b2b2"),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .all(|r| r["name"] != "plain" && r["name"] != "myapp"),
        "{rows:?}"
    );
    let row = rows
        .iter()
        .find(|r| r["name"] == "agent-codex-b2b2b2b2")
        .unwrap();
    assert_eq!(row["agent"]["source"], "myapp");
    assert_eq!(row["state"], "active");
    // Human output has a header and the branch.
    let r = p.output_retrying_pub(
        || {
            let mut c = p.cmd(&["agent", "ls"]);
            c.current_dir(&proj);
            c
        },
        "pgboxdb agent ls",
    );
    assert!(
        r.stdout.contains("NAME") && r.stdout.contains("agent-codex-b2b2b2b2"),
        "{}",
        r.all()
    );
    // pgboxdb ls also carries the agent object in JSON.
    let ls = p.run(&["--json", "ls"]).json();
    let e = ls
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "agent-codex-b2b2b2b2")
        .unwrap();
    assert_eq!(e["agent"]["host"], "codex", "{e}");
}

#[test]
fn pgboxdb_doctor_and_pgboxdb_ls_know_about_agent_branches() {
    let (p, proj) = up_with_source();
    agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "d0c70e00", "--host", "claude"],
    );
    // pgboxdb doctor has an `agents` check that counts them and points at agent doctor.
    let d = p.run(&["--json", "doctor"]).json();
    let c = d["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "agents")
        .unwrap_or_else(|| panic!("no agents check: {d}"));
    assert_eq!(c["status"], "ok", "{c}");
    assert!(
        c["detail"].as_str().unwrap().contains("1 agent branch"),
        "{c}"
    );
    // pgboxdb ls shows the owning host next to the TTL.
    let ls = p.run(&["ls"]);
    let line = ls
        .stdout
        .lines()
        .find(|l| l.starts_with("agent-claude-d0c70e00"))
        .unwrap_or_else(|| panic!("{}", ls.all()));
    assert!(line.contains("2h (claude)"), "{line}");
}

#[test]
fn a_database_the_session_recreates_is_adopted_not_orphaned() {
    // Rails `db:reset`/purge drops and recreates the branch under the same
    // name from inside the session. The new incarnation must stay this
    // session's branch: `up` adopts it, `down --now` drops it, nothing leaks.
    let (p, proj) = up_with_source();
    let j = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "e0e0e0e0-recreate", "--host", "claude"],
    )
    .json();
    let branch = j["branch"].as_str().unwrap().to_string();
    assert_eq!(branch, "agent-claude-e0e0e0e0");
    let r = p.psql("postgres", &format!("DROP DATABASE \"{branch}\""));
    assert_eq!(r.code, 0, "{}", r.all());
    let r = p.psql("postgres", &format!("CREATE DATABASE \"{branch}\""));
    assert_eq!(r.code, 0, "{}", r.all());
    let j2 = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "e0e0e0e0-recreate", "--host", "claude"],
    )
    .json();
    assert_eq!(j2["branch"], branch, "same name, adopted: {j2}");
    assert_eq!(j2["created"], false, "{j2}");
    assert_eq!(j2["adopted"], true, "{j2}");
    let rows = agent_ls(&p, &proj);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["state"], "active");
    // down --now removes the recreated database too (ownership by name + record).
    let d = agent(
        &p,
        &proj,
        &SRC,
        &[
            "down",
            "--id",
            "e0e0e0e0-recreate",
            "--host",
            "claude",
            "--now",
        ],
    )
    .json();
    assert_eq!(d["dropped"], true, "{d}");
    assert!(!db_exists(&p, &branch));
    assert!(agent_ls(&p, &proj).is_empty());
    // A recreated database that the session never touches again is still
    // retired by SessionEnd (down adopts too), so the grace period applies.
    let j3 = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "f1f1f1f1-recreate", "--host", "claude"],
    )
    .json();
    let b3 = j3["branch"].as_str().unwrap().to_string();
    p.psql("postgres", &format!("DROP DATABASE \"{b3}\""));
    p.psql("postgres", &format!("CREATE DATABASE \"{b3}\""));
    let d = agent(
        &p,
        &proj,
        &SRC,
        &["down", "--id", "f1f1f1f1-recreate", "--host", "claude"],
    )
    .json();
    assert_eq!(d["dropped"], false, "{d}");
    assert_eq!(agent_ls(&p, &proj)[0]["state"], "retired");
}

#[test]
fn pgboxdb_drop_forgets_the_agent_record_and_a_retired_session_never_adopts() {
    let (p, proj) = up_with_source();
    // pgboxdb drop of an agent branch leaves no record behind that a later
    // same-name database could inherit.
    let j = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "d1d1d1d1-drop", "--host", "claude"],
    )
    .json();
    let branch = j["branch"].as_str().unwrap().to_string();
    assert_eq!(p.run(&["drop", &branch, "--force"]).code, 0);
    assert!(
        agent_ls(&p, &proj).is_empty(),
        "record must go with the database"
    );
    let r = p.psql("postgres", &format!("CREATE DATABASE \"{branch}\""));
    assert_eq!(r.code, 0, "{}", r.all());
    let d = agent(
        &p,
        &proj,
        &SRC,
        &["down", "--id", "d1d1d1d1-drop", "--host", "claude", "--now"],
    );
    assert_eq!(
        d.code,
        5,
        "a database with a forgotten name is foreign: {}",
        d.all()
    );
    assert!(db_exists(&p, &branch));
    // A session that ended (retired) cannot be the author of a same-name
    // database that appears later: the supervisor must not adopt and drop it.
    let j = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "d2d2d2d2-ret", "--host", "claude"],
    )
    .json();
    let b2 = j["branch"].as_str().unwrap().to_string();
    let d = agent(
        &p,
        &proj,
        &SRC,
        &[
            "down",
            "--id",
            "d2d2d2d2-ret",
            "--host",
            "claude",
            "--grace",
            "3s",
        ],
    )
    .json();
    assert_eq!(d["dropped"], false, "{d}");
    p.psql("postgres", &format!("DROP DATABASE \"{b2}\""));
    p.psql("postgres", &format!("CREATE DATABASE \"{b2}\""));
    let t0 = Instant::now();
    while agent_ls(&p, &proj).iter().any(|r| r["name"] == b2)
        && t0.elapsed() < Duration::from_secs(25)
    {
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        agent_ls(&p, &proj).iter().all(|r| r["name"] != b2),
        "stale record should be forgotten"
    );
    assert!(db_exists(&p, &b2), "the foreign database survives");
}

#[test]
fn sessions_sharing_a_name_prefix_get_distinct_branches() {
    // Two sessions whose ids share the first 8 characters collide on the short
    // name; the second takes the long name instead of failing with exit 4.
    let (p, proj) = up_with_source();
    let a = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "abcdefgh-1111", "--host", "claude"],
    )
    .json();
    let b = agent(
        &p,
        &proj,
        &SRC,
        &["up", "--id", "abcdefgh-2222", "--host", "claude"],
    );
    assert_eq!(b.code, 0, "{}", b.all());
    let b = b.json();
    assert_eq!(a["branch"], "agent-claude-abcdefgh", "{a}");
    assert!(
        b["branch"]
            .as_str()
            .unwrap()
            .starts_with("agent-claude-abcdefgh-"),
        "{b}"
    );
    assert_ne!(a["branch"], b["branch"]);
    // Each session keeps finding its own branch afterwards.
    assert_eq!(
        agent(
            &p,
            &proj,
            &SRC,
            &["up", "--id", "abcdefgh-1111", "--host", "claude"]
        )
        .json()["branch"],
        a["branch"]
    );
    assert_eq!(
        agent(
            &p,
            &proj,
            &SRC,
            &["up", "--id", "abcdefgh-2222", "--host", "claude"]
        )
        .json()["branch"],
        b["branch"]
    );
}
