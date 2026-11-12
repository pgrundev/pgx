//! Black-box tests for `pgx top` against a real PGX runtime: the snapshot
//! contract, five real agent databases created through the Claude Code hook
//! payloads, live connection counts, TTL and lifecycle transitions, explicit
//! task phases (`pgx agent activity`), identity collisions, stale metadata,
//! runtime restarts, and the read-only guarantee.
mod common;
use common::Pgx;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SID: &str = "72ac4f1e-5b7d-4c1a-9e0f-3d2a1b0c9d8e";
const AGENTS: [&str; 5] = [
    "agent-a1b2c3d4",
    "agent-b2c3d4e5",
    "agent-c3d4e5f6",
    "agent-d4e5f6a7",
    "agent-e5f6a7b8",
];

/// `pgx --json top --snapshot`: exactly one JSON object, no escape bytes.
fn snapshot(p: &Pgx) -> Value {
    let r = p.run(&["--json", "top", "--snapshot"]);
    assert!(!r.stdout.contains('\x1b'), "ANSI in JSON: {}", r.all());
    assert_eq!(r.stdout.trim().lines().count(), 1, "one line: {}", r.all());
    let v = r.json();
    assert!(v.is_object(), "{v}");
    assert_eq!(v["schema_version"], 1);
    v
}

fn dbs(v: &Value) -> Vec<Value> {
    v["databases"].as_array().cloned().unwrap_or_default()
}

fn db<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    v["databases"]
        .as_array()?
        .iter()
        .find(|d| d["name"] == name)
}

fn wait_for(p: &Pgx, what: &str, secs: u64, ok: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let v = snapshot(p);
        if ok(&v) {
            return v;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {v:#}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn project(p: &Pgx, name: &str, source: &str) -> PathBuf {
    let proj = p.home.parent().unwrap().join(name);
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(
        proj.join(".pgx.toml"),
        format!("[agent]\nsource = \"{source}\"\n"),
    )
    .unwrap();
    proj
}

fn payload(event: &str, sid: &str, cwd: &Path, extra: Value) -> String {
    let mut v = json!({
        "session_id": sid,
        "transcript_path": "/tmp/t.jsonl",
        "cwd": cwd,
        "hook_event_name": event,
        "permission_mode": "default"
    });
    for (k, val) in extra.as_object().unwrap() {
        v[k] = val.clone();
    }
    v.to_string()
}

/// A Claude Code hook delivery (`pgx agent hook` with the payload on stdin).
fn hook(p: &Pgx, proj: &Path, body: &str) -> common::Run {
    let mut c = p.cmd(&["agent", "hook"]);
    c.current_dir(proj)
        .env("CLAUDE_PROJECT_DIR", proj)
        .env("CLAUDE_ENV_FILE", p.home.join("env.sh"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = common::spawn_locked(&mut c).expect("spawn pgx");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    common::Run::from(child.wait_with_output().unwrap())
}

fn session_start(p: &Pgx, proj: &Path, sid: &str) {
    let r = hook(
        p,
        proj,
        &payload("SessionStart", sid, proj, json!({"source": "startup"})),
    );
    assert_eq!(r.code, 0, "{}", r.all());
}

fn subagent_start(p: &Pgx, proj: &Path, sid: &str, agent: &str) {
    let r = hook(
        p,
        proj,
        &payload(
            "SubagentStart",
            sid,
            proj,
            json!({"agent_id": agent, "agent_type": "general-purpose"}),
        ),
    );
    assert_eq!(r.code, 0, "{}", r.all());
}

fn subagent_stop(p: &Pgx, proj: &Path, sid: &str, agent: &str) {
    let r = hook(
        p,
        proj,
        &payload(
            "SubagentStop",
            sid,
            proj,
            json!({"agent_id": agent, "agent_type": "general-purpose", "stop_hook_active": false}),
        ),
    );
    assert_eq!(r.code, 0, "{}", r.all());
}

fn activity(p: &Pgx, proj: &Path, sid: &str, agent: Option<&str>, args: &[&str]) -> common::Run {
    let mut a = vec!["agent", "activity", "--host", "claude", "--id", sid];
    if let Some(ag) = agent {
        a.extend(["--agent", ag]);
    }
    a.extend_from_slice(args);
    let mut c = p.cmd(&a);
    c.current_dir(proj);
    common::Run::from(common::output_locked(&mut c).unwrap())
}

/// A psql session that holds a connection to `db` for `secs`.
fn hold(p: &Pgx, db: &str, secs: u32) -> Child {
    let mut c = Command::new(common::find_psql());
    c.args([
        "-X",
        "-q",
        "-h",
        &p.sock_dir().to_string_lossy(),
        "-p",
        &p.port.to_string(),
        "-U",
        "postgres",
        "-d",
        db,
        "-c",
        &format!("SELECT pg_sleep({secs})"),
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    common::spawn_locked(&mut c).expect("spawn psql")
}

/// Every file under `dir` with its bytes (a checksum by content).
fn tree_bytes(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(b) = std::fs::read(&path) {
                out.insert(path, b);
            }
        }
    }
    out
}

/// A running PGX with a source `myapp` (one seeded table) and a project.
fn lab() -> (Pgx, PathBuf) {
    let p = Pgx::new();
    p.init_ok();
    p.start_ok();
    assert_eq!(p.run(&["create", "myapp"]).code, 0);
    assert_eq!(
        p.psql("myapp", "CREATE TABLE posts(id int PRIMARY KEY, title text); INSERT INTO posts VALUES (1, 'seed'), (2, 'two')").code,
        0
    );
    let proj = project(&p, "remotely", "myapp");
    (p, proj)
}

fn five_agents(p: &Pgx, proj: &Path) {
    session_start(p, proj, SID);
    for a in AGENTS {
        subagent_start(p, proj, SID, a);
    }
}

fn agent_rows(v: &Value) -> Vec<Value> {
    dbs(v)
        .into_iter()
        .filter(|d| !d["session_id"].is_null())
        .collect()
}

fn by_agent(v: &Value, agent: &str) -> Value {
    agent_rows(v)
        .into_iter()
        .find(|d| d["agent_id"] == agent)
        .unwrap_or_else(|| panic!("no row for {agent}: {v:#}"))
}

#[test]
fn snapshot_reports_a_fresh_runtime_without_secrets() {
    let p = Pgx::new();
    p.init_ok();
    p.start_ok();
    let v = snapshot(&p);
    let status = p.run(&["--json", "status"]).json();
    assert_eq!(v["mode"], "live");
    assert_eq!(v["simulated"], false);
    assert_eq!(v["runtime"]["running"], true);
    assert_eq!(v["runtime"]["status"], "running");
    assert_eq!(v["runtime"]["pid"], status["pid"]);
    assert_eq!(v["runtime"]["port"], json!(p.port));
    assert!(
        v["runtime"]["rss_bytes"].as_u64().is_some_and(|b| b > 0),
        "{v}"
    );
    assert_eq!(
        v["runtime"]["cpu_percent"],
        Value::Null,
        "unmeasured is null, not 0"
    );
    let ls = p.run(&["--json", "ls"]).json();
    assert_eq!(
        v["summary"]["databases"],
        json!(ls.as_array().unwrap().len()),
        "{v:#}\n{ls:#}"
    );
    assert_eq!(
        v["summary"]["client_connections"], 0,
        "the monitor's own connection is excluded"
    );
    assert_eq!(v["summary"]["issues"], 0, "{v:#}");
    assert!(db(&v, "template1").is_none() && db(&v, "postgres").is_some());
    let text = v.to_string();
    let pw = p.password();
    assert!(!text.contains(&pw), "password leaked");
    for secret in ["postgres://", "DATABASE_URL", "password"] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    // Human form: plain text, same counts.
    let t = p.run(&["top", "--snapshot"]);
    assert_eq!(t.code, 0, "{}", t.all());
    assert!(
        t.stdout.starts_with("PGX TOP  runtime RUNNING"),
        "{}",
        t.stdout
    );
    assert!(!t.stdout.contains('\x1b'));
    assert!(!t.stdout.contains(&pw));
}

#[test]
fn not_initialised_and_stopped_runtimes_are_reported_not_created() {
    let p = Pgx::new();
    let home = p.home.join("never-created");
    let mut c = p.cmd(&["--json", "top", "--snapshot"]);
    c.env("PGX_HOME", &home);
    let r = common::Run::from(common::output_locked(&mut c).unwrap());
    assert_eq!(r.code, 3, "{}", r.all());
    let v = r.json();
    assert_eq!(v["runtime"]["status"], "not_initialized");
    assert_eq!(v["databases"], Value::Null, "unknown, not an empty list");
    assert!(!home.exists(), "pgx top must not create a home");
    // Interactive mode on a non-initialised home: a clear error (and still nothing created).
    let mut c = p.cmd(&["top"]);
    c.env("PGX_HOME", &home);
    let r = common::Run::from(common::output_locked(&mut c).unwrap());
    assert_ne!(r.code, 0);
    assert!(!home.exists());

    p.init_ok();
    let v = p.run(&["--json", "top", "--snapshot"]);
    assert_eq!(v.code, 3, "stopped: {}", v.all());
    let v = v.json();
    assert_eq!(v["runtime"]["status"], "stopped");
    assert_eq!(v["runtime"]["running"], false);
    assert_eq!(v["summary"]["databases"], Value::Null);
    assert!(
        p.engine_pids().is_empty(),
        "pgx top never starts the runtime"
    );
}

#[test]
fn cli_contract_json_needs_snapshot_and_non_tty_explains() {
    let p = Pgx::new();
    let r = p.run(&["--json", "top"]);
    assert_eq!(r.code, 2, "{}", r.all());
    assert_eq!(r.json()["error"]["code"], "usage");
    let r = p.run(&["top"]);
    assert_eq!(r.code, 2, "{}", r.all());
    assert!(
        r.stderr.contains("needs a terminal") && r.stderr.contains("--snapshot"),
        "{}",
        r.all()
    );
    let r = p.run(&["top", "--demo"]);
    assert_eq!(r.code, 2, "non-TTY demo also explains: {}", r.all());
    for bad in [
        vec!["top", "--interval-ms", "0"],
        vec!["top", "--interval-ms", "-1"],
        vec!["top", "--interval-ms", "10001"],
        vec!["top", "--interval-ms", "abc"],
        vec!["top", "--bogus"],
        vec!["top", "positional"],
    ] {
        let r = p.run(&bad);
        assert_eq!(r.code, 2, "{bad:?}: {}", r.all());
    }
    let r = p.run(&["top", "--help"]);
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains("pgx top") && r.stdout.contains("--interval-ms"),
        "{}",
        r.stdout
    );
    let r = p.run(&["help"]);
    assert!(r.stdout.contains("pgx top") && r.stdout.contains("pgx agent activity"));
    // Existing commands and their exit codes are unchanged.
    assert_eq!(p.run(&["status"]).code, 3, "not initialised");
    assert_eq!(p.run(&["frobnicate"]).code, 2);
    assert_eq!(
        p.run(&["status", "--help"]).code,
        0,
        "global help still wins elsewhere"
    );
}

#[test]
fn demo_snapshot_needs_no_home_and_is_deterministic() {
    let p = Pgx::new();
    let home = p.home.join("absent");
    let run = || {
        let mut c = p.cmd(&["--json", "top", "--demo", "--snapshot"]);
        c.env("PGX_HOME", &home);
        common::Run::from(common::output_locked(&mut c).unwrap())
    };
    let a = run();
    let b = run();
    assert_eq!(a.code, 0, "{}", a.all());
    assert_eq!(a.stdout, b.stdout, "fixed fixture");
    assert!(!home.exists());
    let v = a.json();
    assert_eq!(v["mode"], "demo");
    assert_eq!(v["simulated"], true);
    assert_eq!(v["summary"]["agent_databases"], 5);
    assert_eq!(v["summary"]["sessions"], 1);
    let labels: Vec<&str> = v["databases"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["task_phase"]["label"].as_str())
        .collect();
    assert_eq!(labels, ["JWT", "OAuth", "API", "Schema", "Billing"]);
}

#[test]
fn demo_never_touches_a_live_home() {
    let (p, _proj) = lab();
    let before = tree_bytes(&p.home.join("ttl"));
    let listing = |dir: &Path| -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };
    let top_before = listing(&p.home);
    let r = p.run(&["--json", "top", "--demo", "--snapshot"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.json()["mode"], "demo");
    assert!(!r.stdout.contains("myapp"), "demo shows no real database");
    assert_eq!(tree_bytes(&p.home.join("ttl")), before);
    assert_eq!(listing(&p.home), top_before, "no new files in PGX_HOME");
}

#[test]
fn five_real_agent_databases_are_grouped_and_attributed() {
    let (p, proj) = lab();
    five_agents(&p, &proj);
    let v = snapshot(&p);
    let rows = agent_rows(&v);
    assert_eq!(rows.len(), 6, "parent + five subagents: {v:#}");
    assert_eq!(v["summary"]["agent_databases"], 6);
    assert_eq!(v["summary"]["sessions"], 1);
    let parents: Vec<&Value> = rows.iter().filter(|d| d["role"] == "session").collect();
    assert_eq!(parents.len(), 1, "the parent is never counted as a child");
    assert_eq!(parents[0]["agent_id"], Value::Null);
    let names: std::collections::HashSet<String> = rows
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names.len(), 6, "distinct databases");
    for a in AGENTS {
        let d = by_agent(&v, a);
        assert_eq!(d["role"], "subagent");
        assert_eq!(d["host"], "claude");
        assert_eq!(d["session_id"], SID);
        assert_eq!(d["project"], "remotely");
        assert_eq!(d["parent_database"], "myapp");
        assert_eq!(d["db_state"], "idle");
        assert_eq!(d["lifecycle"], "active");
        assert_eq!(d["deadline_kind"], "idle_ttl");
        assert_eq!(d["task_phase"], Value::Null, "no explicit report yet");
        assert!(d["oid"].as_u64().is_some());
        assert!(d["size_bytes"].as_u64().is_some_and(|s| s > 0), "{d}");
        assert!(
            d["expires_in_seconds"]
                .as_u64()
                .is_some_and(|s| s > 0 && s <= 7200),
            "{d}"
        );
        assert_eq!(d["connections"], 0);
    }
    // The same identities as `pgx agent ls` reports.
    let ls = p.run(&["--json", "agent", "ls"]).json();
    let mut ls_names: Vec<String> = ls
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    ls_names.sort();
    let mut top_names: Vec<String> = names.into_iter().collect();
    top_names.sort();
    assert_eq!(ls_names, top_names);
    // The interactive tree groups them under one project and one session.
    let t = p.run(&["top", "--snapshot"]);
    assert!(t.stdout.contains("▾ remotely"), "{}", t.stdout);
    assert!(
        t.stdout.contains("Claude Code · session 72ac4f1e"),
        "{}",
        t.stdout
    );
    for a in AGENTS {
        assert!(t.stdout.contains(a), "{a} missing:\n{}", t.stdout);
    }
}

#[test]
fn connections_ttl_retirement_and_drop_are_observed() {
    let (p, proj) = lab();
    five_agents(&p, &proj);
    let v = snapshot(&p);
    let a = by_agent(&v, AGENTS[0]);
    let name = a["name"].as_str().unwrap().to_string();
    // Live connections: up, then back to zero (bounded polling, no fixed sleeps).
    let mut s1 = hold(&p, &name, 4);
    let mut s2 = hold(&p, &name, 4);
    let v = wait_for(&p, "2 connections", 20, |v| {
        db(v, &name).is_some_and(|d| d["connections"] == 2)
    });
    assert_eq!(db(&v, &name).unwrap()["db_state"], "connected");
    assert_eq!(v["summary"]["client_connections"], 2);
    let _ = s1.wait();
    let _ = s2.wait();
    let v = wait_for(&p, "0 connections", 20, |v| {
        db(v, &name).is_some_and(|d| d["connections"] == 0)
    });
    assert_eq!(db(&v, &name).unwrap()["db_state"], "idle");

    // Snapshots never rewrite TTL records.
    let ttl_dir = p.home.join("ttl");
    let t0 = tree_bytes(&ttl_dir);
    for _ in 0..3 {
        snapshot(&p);
    }
    assert_eq!(
        tree_bytes(&ttl_dir),
        t0,
        "snapshots never rewrite TTL records"
    );

    // SubagentStop retires one subagent: lifecycle changes, task phase does not appear.
    subagent_stop(&p, &proj, SID, AGENTS[1]);
    let v = snapshot(&p);
    let b = by_agent(&v, AGENTS[1]);
    assert_eq!(b["lifecycle"], "retired");
    assert_eq!(b["deadline_kind"], "grace");
    assert_eq!(
        b["task_phase"],
        Value::Null,
        "retirement never implies COMPLETED"
    );
    assert_eq!(
        by_agent(&v, AGENTS[2])["lifecycle"],
        "active",
        "siblings unaffected"
    );

    // SessionEnd retires the parent: a lifecycle change, never a task result.
    let r = hook(
        &p,
        &proj,
        &payload(
            "SessionEnd",
            SID,
            &proj,
            json!({"reason": "prompt_input_exit"}),
        ),
    );
    assert_eq!(r.code, 0, "{}", r.all());
    let v = snapshot(&p);
    let parent = agent_rows(&v)
        .into_iter()
        .find(|d| d["role"] == "session")
        .unwrap();
    assert_eq!(parent["lifecycle"], "retired", "{parent:#}");
    assert_eq!(parent["task_phase"], Value::Null);
    assert!(agent_rows(&v).iter().all(|d| d["task_phase"].is_null()));

    // `down --now` drops one: gone from the next snapshot, the rest stay.
    let mut c = p.cmd(&[
        "agent", "down", "--host", "claude", "--id", SID, "--agent", AGENTS[2], "--now",
    ]);
    c.current_dir(&proj);
    assert_eq!(
        common::output_locked(&mut c).unwrap().status.code(),
        Some(0)
    );
    let v = snapshot(&p);
    assert_eq!(agent_rows(&v).len(), 5);
    assert!(agent_rows(&v).iter().all(|d| d["agent_id"] != AGENTS[2]));
    assert_eq!(v["summary"]["issues"], 0, "{v:#}");
}

#[test]
fn explicit_phases_are_independent_validated_and_never_touch_ttl() {
    let (p, proj) = lab();
    five_agents(&p, &proj);
    let ttl_dir = p.home.join("ttl");
    let ttl_before = tree_bytes(&ttl_dir);
    let r = activity(
        &p,
        &proj,
        SID,
        Some(AGENTS[1]),
        &["--state", "testing", "--task", "Implement OAuth"],
    );
    assert_eq!(r.code, 0, "{}", r.all());
    let r = activity(
        &p,
        &proj,
        SID,
        None,
        &["--state", "working", "--task", "Coordinate"],
    );
    assert_eq!(r.code, 0, "parent reports independently: {}", r.all());
    assert_eq!(
        tree_bytes(&ttl_dir),
        ttl_before,
        "a phase report never rewrites a TTL record"
    );
    let v = snapshot(&p);
    let b = by_agent(&v, AGENTS[1]);
    assert_eq!(b["task_phase"]["state"], "testing");
    assert_eq!(b["task_phase"]["label"], "Implement OAuth");
    assert_eq!(b["task_phase"]["source"], "pgx agent activity");
    let parent = agent_rows(&v)
        .into_iter()
        .find(|d| d["role"] == "session")
        .unwrap();
    assert_eq!(parent["task_phase"]["state"], "working");
    for a in [AGENTS[0], AGENTS[2], AGENTS[3], AGENTS[4]] {
        assert_eq!(
            by_agent(&v, a)["task_phase"],
            Value::Null,
            "{a}: no leakage"
        );
    }
    // Phase files: private, one per identity.
    use std::os::unix::fs::PermissionsExt;
    let dir = p.home.join("top/phases");
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 2);
    for f in &files {
        assert_eq!(f.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );

    // Rejections: bad state, long or unclean labels, unknown identity, spoofing.
    for (args, code) in [
        (vec!["--state", "done"], 2),
        (vec!["--state", "TESTING"], 2),
        (vec![], 2),
        (vec!["--state", "testing", "--task", &"x".repeat(81)], 2),
        (vec!["--state", "testing", "--task", "\x1b[31mred"], 2),
        (vec!["--state", "testing", "--task", "a\nb"], 2),
    ] {
        let r = activity(&p, &proj, SID, Some(AGENTS[0]), &args);
        assert_eq!(r.code, code, "{args:?}: {}", r.all());
    }
    let r = activity(
        &p,
        &proj,
        SID,
        Some("agent-nobody0"),
        &["--state", "testing"],
    );
    assert_eq!(r.code, 5, "unknown subagent: {}", r.all());
    let r = activity(
        &p,
        &proj,
        "other-session-0000",
        None,
        &["--state", "testing"],
    );
    assert_eq!(r.code, 5, "unknown session: {}", r.all());
    let mut c = p.cmd(&[
        "agent", "activity", "--host", "claude", "--id", SID, "--state", "testing",
    ]);
    c.current_dir(&proj)
        .env("CLAUDE_CODE_SESSION_ID", "a-different-session");
    let r = common::Run::from(common::output_locked(&mut c).unwrap());
    assert_eq!(
        r.code,
        4,
        "reporting for another session is refused: {}",
        r.all()
    );
    // Another project with the same session id cannot reach this session's database.
    let other = project(&p, "elsewhere", "myapp");
    let r = activity(&p, &other, SID, Some(AGENTS[0]), &["--state", "failed"]);
    assert_eq!(r.code, 5, "{}", r.all());
    assert_eq!(
        by_agent(&snapshot(&p), AGENTS[0])["task_phase"],
        Value::Null
    );

    // SubagentStop after a TESTING report: retired, phase still TESTING (never COMPLETED).
    subagent_stop(&p, &proj, SID, AGENTS[1]);
    let b = by_agent(&snapshot(&p), AGENTS[1]);
    assert_eq!(b["lifecycle"], "retired");
    assert_eq!(b["task_phase"]["state"], "testing");
    // An explicit completion is shown as such.
    let r = activity(&p, &proj, SID, Some(AGENTS[1]), &["--state", "completed"]);
    assert_eq!(r.code, 0, "a retired session may still report: {}", r.all());
    assert_eq!(
        by_agent(&snapshot(&p), AGENTS[1])["task_phase"]["state"],
        "completed"
    );

    // Concurrent reports from all five subagents: every file whole.
    let threads: Vec<_> = AGENTS
        .iter()
        .map(|a| {
            let bin = p.bin();
            let home = p.home.clone();
            let proj = proj.clone();
            let a = a.to_string();
            std::thread::spawn(move || {
                for st in ["working", "migrating", "testing"] {
                    let mut c = Command::new(&bin);
                    c.args([
                        "agent", "activity", "--host", "claude", "--id", SID, "--agent", &a,
                        "--state", st,
                    ])
                    .env("PGX_HOME", &home)
                    .env_remove("CLAUDE_CODE_SESSION_ID")
                    .current_dir(&proj);
                    let o = common::output_locked(&mut c).unwrap();
                    assert!(o.status.success() || o.status.code() == Some(5), "{o:?}");
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let v = snapshot(&p);
    assert!(
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["code"] != "phase_records_ignored"),
        "no torn phase files: {v:#}"
    );
    assert_eq!(by_agent(&v, AGENTS[4])["task_phase"]["state"], "testing");
}

#[test]
fn colliding_session_ids_across_projects_stay_separate() {
    let (p, proj) = lab();
    let other = project(&p, "jobsgpt", "myapp");
    session_start(&p, &proj, SID);
    session_start(&p, &other, SID);
    let v = snapshot(&p);
    let rows = agent_rows(&v);
    assert_eq!(rows.len(), 2, "{v:#}");
    let projects: std::collections::HashSet<String> = rows
        .iter()
        .map(|d| d["project_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        projects.len(),
        2,
        "same session id, two projects, two identities"
    );
    assert_eq!(v["summary"]["sessions"], 2);
    let t = p.run(&["top", "--snapshot"]).stdout;
    assert!(t.contains("▾ remotely") && t.contains("▾ jobsgpt"), "{t}");
}

#[test]
fn stale_metadata_never_attaches_to_a_recreated_database() {
    let (p, proj) = lab();
    session_start(&p, &proj, SID);
    let v = snapshot(&p);
    let parent = agent_rows(&v).into_iter().next().unwrap();
    let name = parent["name"].as_str().unwrap().to_string();
    let old_oid = parent["oid"].as_u64().unwrap();
    // Drop and recreate under the same name behind PGX's back.
    assert_eq!(
        p.psql("postgres", &format!("DROP DATABASE \"{name}\""))
            .code,
        0
    );
    assert_eq!(
        p.psql("postgres", &format!("CREATE DATABASE \"{name}\""))
            .code,
        0
    );
    let v = snapshot(&p);
    let d = db(&v, &name).expect("recreated database listed");
    assert_ne!(d["oid"].as_u64().unwrap(), old_oid);
    assert_eq!(d["session_id"], Value::Null, "stale ownership not attached");
    assert_eq!(d["role"], "standalone");
    assert_eq!(d["expires_at"], Value::Null);
    let codes: Vec<&str> = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["code"].as_str())
        .collect();
    assert!(codes.contains(&"ttl_record_stale_oid"), "{v:#}");
    assert!(v["summary"]["issues"].as_u64().unwrap() >= 1);
    // A phase report cannot attach to the impostor either.
    let r = activity(&p, &proj, SID, None, &["--state", "testing"]);
    assert_eq!(r.code, 4, "{}", r.all());
    // And the monitor deleted nothing.
    assert!(p.db_exists(&name));
}

#[test]
fn corrupt_metadata_is_a_warning_and_never_hides_databases() {
    let (p, proj) = lab();
    session_start(&p, &proj, SID);
    std::fs::write(
        p.home.join("ttl/deadbeef.json"),
        "{\"schema\":1, \"name\": ",
    )
    .unwrap();
    std::fs::create_dir_all(p.home.join("top/phases")).unwrap();
    std::fs::write(p.home.join("top/phases/0000000000000000.json"), "garbage").unwrap();
    std::fs::create_dir_all(p.home.join("git/brokenrepo")).unwrap();
    std::fs::write(p.home.join("git/brokenrepo/state.json"), "{nope").unwrap();
    let v = snapshot(&p);
    assert_eq!(agent_rows(&v).len(), 1, "the good record still attaches");
    let codes: Vec<&str> = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["code"].as_str())
        .collect();
    for c in [
        "ttl_record_unreadable",
        "git_state_unreadable",
        "phase_records_ignored",
    ] {
        assert!(codes.contains(&c), "{c}: {v:#}");
    }
    assert!(
        p.home.join("ttl/deadbeef.json").exists(),
        "never repaired or deleted"
    );
}

#[test]
fn restart_and_stop_are_reported_and_recovered() {
    let (p, proj) = lab();
    session_start(&p, &proj, SID);
    let v1 = snapshot(&p);
    let pid1 = v1["runtime"]["pid"].as_u64().unwrap();
    p.stop_ok();
    let r = p.run(&["--json", "top", "--snapshot"]);
    assert_eq!(r.code, 3);
    let v = r.json();
    assert_eq!(v["runtime"]["status"], "stopped");
    assert_eq!(
        v["databases"],
        Value::Null,
        "unknown while stopped, not deleted"
    );
    assert!(p.wait_port_free(30));
    p.start_ok();
    let v2 = snapshot(&p);
    assert_ne!(
        v2["runtime"]["pid"].as_u64().unwrap(),
        pid1,
        "new pid after restart"
    );
    assert_eq!(
        agent_rows(&v2).len(),
        1,
        "ownership still attaches after a restart"
    );
    assert_eq!(agent_rows(&v2)[0]["oid"], agent_rows(&v1)[0]["oid"]);
}

#[test]
fn monitoring_is_read_only() {
    let (p, proj) = lab();
    five_agents(&p, &proj);
    let roles = |p: &Pgx| {
        p.psql(
            "postgres",
            "SELECT string_agg(rolname, ',' ORDER BY rolname) FROM pg_roles",
        )
        .stdout
    };
    let rows = |p: &Pgx| {
        p.psql(
            "myapp",
            "SELECT string_agg(id || ':' || title, ',' ORDER BY id) FROM posts",
        )
        .stdout
    };
    let dbnames = |p: &Pgx| {
        p.psql(
            "postgres",
            "SELECT string_agg(datname, ',' ORDER BY datname) FROM pg_database",
        )
        .stdout
    };
    let (r0, d0, n0) = (roles(&p), rows(&p), dbnames(&p));
    let ttl0 = tree_bytes(&p.home.join("ttl"));
    let git0 = tree_bytes(&p.home.join("git"));
    let agent0 = tree_bytes(&p.home.join("agent"));
    for _ in 0..5 {
        snapshot(&p);
        assert_eq!(p.run(&["top", "--snapshot"]).code, 0);
    }
    assert_eq!(roles(&p), r0, "no role changes");
    assert_eq!(rows(&p), d0, "source data unchanged");
    assert_eq!(dbnames(&p), n0, "no database created or dropped");
    assert_eq!(
        tree_bytes(&p.home.join("ttl")),
        ttl0,
        "TTL records untouched"
    );
    assert_eq!(tree_bytes(&p.home.join("git")), git0);
    assert_eq!(
        tree_bytes(&p.home.join("agent")),
        agent0,
        "no agent state touched"
    );
    assert!(
        !p.home.join("top").exists(),
        "monitoring creates no sidecar directory"
    );
}

#[test]
fn touch_and_expiry_are_observed_never_caused() {
    let (p, proj) = lab();
    // A short idle TTL: `touch` may rewrite every ttl/4 = 3 s, and the TTL
    // worker (polling every second in tests) removes it 12 s after the last touch.
    let mut c = p.cmd(&[
        "--json",
        "agent",
        "up",
        "--host",
        "claude",
        "--id",
        "short-lived-1",
        "--ttl",
        "12s",
    ]);
    c.current_dir(&proj);
    let r = common::Run::from(common::output_locked(&mut c).unwrap());
    assert_eq!(r.code, 0, "{}", r.all());
    let name = r.json()["branch"].as_str().unwrap().to_string();
    let e0 = db(&snapshot(&p), &name).unwrap()["expires_at"].clone();
    // Watching for > ttl/4 does not extend anything.
    let until = Instant::now() + Duration::from_millis(3300);
    while Instant::now() < until {
        assert_eq!(
            db(&snapshot(&p), &name).unwrap()["expires_at"],
            e0,
            "the monitor never extends a TTL"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    let mut c = p.cmd(&[
        "--json",
        "agent",
        "touch",
        "--host",
        "claude",
        "--id",
        "short-lived-1",
    ]);
    c.current_dir(&proj);
    let r = common::Run::from(common::output_locked(&mut c).unwrap());
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(r.json()["skipped"], false, "{}", r.all());
    let e1 = db(&snapshot(&p), &name).unwrap()["expires_at"].clone();
    assert_ne!(e1, e0, "the touch's new deadline is observed");
    assert_eq!(e1, r.json()["expires_at"]);
    // Expiry: the TTL worker drops it and the snapshot stops listing it.
    let v = wait_for(&p, "expiry", 60, |v| db(v, &name).is_none());
    assert!(
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["code"] != "ttl_record_orphaned"),
        "{v:#}"
    );
    assert!(!p.db_exists(&name));
}
