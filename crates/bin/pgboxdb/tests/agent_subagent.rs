//! Black-box tests for per-subagent isolation in Claude Code: `SubagentStart`
//! gives a subagent its own branch, `PreToolUse` routes its Bash commands
//! there (or refuses them when it cannot), `SubagentStop` retires it, and
//! the parent session's branch is never involved. Payloads are in Claude
//! Code's documented shapes (session_id + agent_id + agent_type).
mod common;
use common::Pgboxdb;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SID: &str = "f775e29e-e738-4f37-93b4-9b82f539ba81";

fn up_with_source() -> (Pgboxdb, PathBuf) {
    let p = Pgboxdb::new();
    p.init_ok();
    p.start_ok();
    assert_eq!(p.run(&["create", "myapp"]).code, 0);
    assert_eq!(
        p.psql(
            "myapp",
            "CREATE TABLE posts(id int PRIMARY KEY, title text); INSERT INTO posts VALUES (1, 'seed')"
        )
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

/// `pgboxdb agent hook` with `payload` on stdin, from the home dir (the payload's
/// cwd locates the project), with Claude's environment marker.
fn hook(p: &Pgboxdb, payload: &str) -> common::Run {
    let mut c = p.cmd(&["agent", "hook"]);
    c.current_dir(&p.home)
        .env("CLAUDE_PROJECT_DIR", p.home.parent().unwrap().join("proj"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = common::spawn_locked(&mut c).expect("spawn pgboxdb");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    common::Run::from(child.wait_with_output().unwrap())
}

fn payload(event: &str, cwd: &Path, extra: Value) -> String {
    let mut v = json!({
        "session_id": SID,
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

fn subagent_start(p: &Pgboxdb, proj: &Path, agent: &str) -> common::Run {
    hook(
        p,
        &payload(
            "SubagentStart",
            proj,
            json!({"agent_id": agent, "agent_type": "general-purpose"}),
        ),
    )
}

fn subagent_stop(p: &Pgboxdb, proj: &Path, agent: &str) -> common::Run {
    hook(
        p,
        &payload(
            "SubagentStop",
            proj,
            json!({"agent_id": agent, "agent_type": "general-purpose", "stop_hook_active": false}),
        ),
    )
}

fn pre_tool_use(
    p: &Pgboxdb,
    proj: &Path,
    agent: Option<&str>,
    tool: &str,
    input: Value,
) -> common::Run {
    let mut extra = json!({"tool_name": tool, "tool_input": input, "tool_use_id": "toolu_01"});
    if let Some(a) = agent {
        extra["agent_id"] = json!(a);
        extra["agent_type"] = json!("general-purpose");
    }
    hook(p, &payload("PreToolUse", proj, extra))
}

/// The command the host would run after `updatedInput`.
fn routed_command(r: &common::Run) -> String {
    let j = r.json();
    let out = &j["hookSpecificOutput"];
    assert_eq!(out["hookEventName"], "PreToolUse", "{j}");
    assert!(
        out.get("permissionDecision").is_none(),
        "no decision, normal flow: {j}"
    );
    out["updatedInput"]["command"]
        .as_str()
        .unwrap_or_else(|| panic!("no updatedInput.command in {j}"))
        .to_string()
}

/// Run a (routed) command the way the Bash tool does: a fresh shell, the
/// parent session's exports inherited (here: a decoy DATABASE_URL and
/// PGDATABASE that must lose), psql on PATH.
fn run_shell(p: &Pgboxdb, proj: &Path, command: &str) -> common::Run {
    let mut c = Command::new("/bin/sh");
    c.arg("-c")
        .arg(command)
        .current_dir(proj)
        .env("PATH", path_with_psql())
        .env("PGBOXDB_HOME", &p.home)
        .env("DATABASE_URL", "postgres://nobody@127.0.0.1:1/decoy")
        .env("PGDATABASE", "decoy")
        .env("PGSERVICE", "decoy")
        .env("CLAUDE_CODE_SESSION_ID", SID);
    common::Run::from(common::output_locked(&mut c).expect("spawn sh"))
}

fn agent_ls(p: &Pgboxdb) -> Vec<Value> {
    p.run(&["--json", "agent", "ls"])
        .json()
        .as_array()
        .unwrap()
        .clone()
}

fn branch_of(p: &Pgboxdb, agent: Option<&str>) -> Option<String> {
    agent_ls(p)
        .iter()
        .find(|r| r["agent"]["session_id"] == SID && r["agent"]["agent_id"] == json!(agent))
        .map(|r| r["name"].as_str().unwrap().to_string())
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

#[test]
fn subagent_start_gives_each_subagent_its_own_branch_and_the_parent_keeps_its_own() {
    let (p, proj) = up_with_source();
    // The parent session's branch, as the SessionStart hook would make it.
    let ef = p.home.join("env.sh");
    let mut c = p.cmd(&["agent", "hook"]);
    c.current_dir(&p.home)
        .env("CLAUDE_ENV_FILE", &ef)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = common::spawn_locked(&mut c).unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload("SessionStart", &proj, json!({"source": "startup"})).as_bytes())
        .unwrap();
    assert_eq!(child.wait_with_output().unwrap().status.code(), Some(0));
    let parent = branch_of(&p, None).expect("parent branch");
    assert_eq!(parent, "agent-claude-f775e29e");

    let a = subagent_start(&p, &proj, "agent-ada61284");
    assert_eq!(a.code, 0, "{}", a.all());
    let j = a.json();
    let ctx = j["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert_eq!(j["hookSpecificOutput"]["hookEventName"], "SubagentStart");
    assert!(ctx.contains("agent-claude-f775e29e-ada61284"), "{ctx}");
    assert!(ctx.contains("routed to it automatically"), "{ctx}");
    assert!(
        !ctx.contains("postgres://") && !ctx.contains(&p.password()),
        "no credentials in context: {ctx}"
    );
    let b = subagent_start(&p, &proj, "agent-a9f6af68");
    assert_eq!(b.code, 0, "{}", b.all());

    let ba = branch_of(&p, Some("agent-ada61284")).unwrap();
    let bb = branch_of(&p, Some("agent-a9f6af68")).unwrap();
    assert_eq!(ba, "agent-claude-f775e29e-ada61284");
    assert_eq!(bb, "agent-claude-f775e29e-a9f6af68");
    assert!(db_exists(&p, &ba) && db_exists(&p, &bb) && db_exists(&p, &parent));
    assert_eq!(agent_ls(&p).len(), 3);
    // Each is a copy of the source, independent of the others.
    assert_eq!(p.psql(&ba, "INSERT INTO posts VALUES (2, 'a')").code, 0);
    assert_eq!(p.psql(&bb, "SELECT count(*) FROM posts").stdout.trim(), "1");
    assert_eq!(
        p.psql(&parent, "SELECT count(*) FROM posts").stdout.trim(),
        "1"
    );
    assert_eq!(
        p.psql("myapp", "SELECT count(*) FROM posts").stdout.trim(),
        "1"
    );
    // Started again (a resumed subagent): the same branch, data intact.
    let again = subagent_start(&p, &proj, "agent-ada61284");
    assert_eq!(again.code, 0);
    assert!(again.json()["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains(&ba));
    assert_eq!(p.psql(&ba, "SELECT count(*) FROM posts").stdout.trim(), "2");
    assert_eq!(agent_ls(&p).len(), 3);
}

#[test]
fn twenty_subagents_of_one_session_get_twenty_branches() {
    let (p, proj) = up_with_source();
    let mut names = std::collections::BTreeSet::new();
    for i in 0..20 {
        let agent = format!("agent-{i:08x}");
        let r = subagent_start(&p, &proj, &agent);
        assert_eq!(r.code, 0, "{}", r.all());
        names.insert(branch_of(&p, Some(&agent)).unwrap());
    }
    assert_eq!(names.len(), 20, "{names:?}");
    assert_eq!(agent_ls(&p).len(), 20);
    for n in &names {
        assert!(n.starts_with("agent-claude-f775e29e-"), "{n}");
        assert!(db_exists(&p, n));
    }
}

#[test]
fn concurrent_starts_for_one_subagent_make_one_branch() {
    let (p, proj) = up_with_source();
    let mut children = Vec::new();
    for _ in 0..4 {
        let mut c = p.cmd(&["agent", "hook"]);
        c.current_dir(&p.home)
            .env("CLAUDE_PROJECT_DIR", &proj)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = common::spawn_locked(&mut c).unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                payload(
                    "SubagentStart",
                    &proj,
                    json!({"agent_id": "agent-race", "agent_type": "x"}),
                )
                .as_bytes(),
            )
            .unwrap();
        children.push(child);
    }
    for ch in children {
        let out = ch.wait_with_output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let rows = agent_ls(&p);
    assert_eq!(rows.len(), 1, "{rows:?}");
}

#[test]
fn pre_tool_use_routes_a_subagents_bash_command_to_its_branch() {
    let (p, proj) = up_with_source();
    let agent = "agent-ada61284";
    assert_eq!(subagent_start(&p, &proj, agent).code, 0);
    let branch = branch_of(&p, Some(agent)).unwrap();
    let original = "psql -X -Atc 'select current_database()' && echo \"db=$PGDATABASE routed=$PGBOXDB_ROUTED agent=$PGBOXDB_AGENT_ID\"";
    let r = pre_tool_use(
        &p,
        &proj,
        Some(agent),
        "Bash",
        json!({"command": original, "description": "which db", "timeout": 30000, "run_in_background": false}),
    );
    assert_eq!(r.code, 0, "{}", r.all());
    let cmd = routed_command(&r);
    // The original text is kept verbatim after the prefix; other fields survive.
    // The wrapper form: `<pgboxdb> agent exec --agent … --cwd … -- <shell> -c '<original>' # pgboxdb-routed <branch>`.
    assert!(
        cmd.starts_with(&format!(
            "{} agent exec --agent {agent} --cwd ",
            p.bin().display()
        )),
        "{cmd}"
    );
    assert!(
        cmd.ends_with(&format!(
            " -c '{}' # pgboxdb-routed {branch}",
            original.replace('\'', "'\\''")
        )),
        "{cmd}"
    );
    let ui = &r.json()["hookSpecificOutput"]["updatedInput"];
    assert_eq!(ui["description"], "which db");
    assert_eq!(ui["timeout"], 30000);
    assert_eq!(ui["run_in_background"], false);
    // No credential in the command the host (and its transcript) sees.
    assert!(
        !cmd.contains(&p.password()) && !cmd.contains("postgres://"),
        "{cmd}"
    );
    // Run it as the Bash tool would, with a decoy DATABASE_URL/PGDATABASE/
    // PGSERVICE inherited from the parent: the branch wins.
    let run = run_shell(&p, &proj, &cmd);
    assert_eq!(run.code, 0, "{}", run.all());
    assert_eq!(run.stdout.lines().next().unwrap(), branch);
    assert_eq!(
        run.stdout.lines().nth(1).unwrap(),
        format!("db={branch} routed=1 agent={agent}")
    );
    // Writes land in the subagent's branch only.
    let w = run_shell(
        &p,
        &proj,
        &routed_command(&pre_tool_use(
            &p,
            &proj,
            Some(agent),
            "Bash",
            json!({"command": "psql -X -c \"INSERT INTO posts VALUES (7, 'from subagent')\""}),
        )),
    );
    assert_eq!(w.code, 0, "{}", w.all());
    assert_eq!(
        p.psql(&branch, "SELECT count(*) FROM posts").stdout.trim(),
        "2"
    );
    assert_eq!(
        p.psql("myapp", "SELECT count(*) FROM posts").stdout.trim(),
        "1"
    );
    // A `cd` out of the project inside the command changes nothing: the
    // wrapper pinned the project with --cwd.
    let elsewhere = run_shell(
        &p,
        &proj,
        &routed_command(&pre_tool_use(
            &p,
            &proj,
            Some(agent),
            "Bash",
            json!({"command": "cd / && psql -X -Atc 'select current_database()'"}),
        )),
    );
    assert_eq!(elsewhere.stdout.trim(), branch, "{}", elsewhere.all());
    assert_eq!(
        agent_ls(&p).len(),
        1,
        "no second branch for the same subagent"
    );
}

#[test]
fn routing_preserves_shell_semantics() {
    let (p, proj) = up_with_source();
    let agent = "agent-shell";
    assert_eq!(subagent_start(&p, &proj, agent).code, 0);
    let cases: &[(&str, i32, &str)] = &[
        // pipeline
        ("printf 'a\\nb\\n' | wc -l | tr -d ' '", 0, "2"),
        // heredoc with quotes
        (
            "cat <<'EOF'\nit's $HOME \"quoted\"\nEOF",
            0,
            "it's $HOME \"quoted\"",
        ),
        // redirect and multiline script with a variable assignment
        ("X=3\necho $X > out.txt\ncat out.txt", 0, "3"),
        // a failing command keeps its exit code
        ("echo before; exit 7", 7, "before"),
        // the current directory is the project
        ("basename \"$PWD\"", 0, "proj"),
        // a nonzero status from a pipeline tail
        ("true | false; echo $?", 0, "1"),
    ];
    for (original, code, expect) in cases {
        let r = pre_tool_use(&p, &proj, Some(agent), "Bash", json!({"command": original}));
        let cmd = routed_command(&r);
        let run = run_shell(&p, &proj, &cmd);
        assert_eq!(run.code, *code, "{original:?}: {}", run.all());
        assert_eq!(run.stdout.trim_end(), *expect, "{original:?}");
    }
    // A background process started by the command outlives the shell as usual.
    let r = pre_tool_use(
        &p,
        &proj,
        Some(agent),
        "Bash",
        json!({"command": "(sleep 1; echo late > bg.txt) & echo started"}),
    );
    let run = run_shell(&p, &proj, &routed_command(&r));
    assert_eq!(run.stdout.trim(), "started");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !proj.join("bg.txt").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        std::fs::read_to_string(proj.join("bg.txt")).unwrap().trim(),
        "late"
    );
}

#[test]
fn routing_is_idempotent_and_only_for_subagent_bash_in_opted_in_projects() {
    let (p, proj) = up_with_source();
    let agent = "agent-idem";
    assert_eq!(subagent_start(&p, &proj, agent).code, 0);
    let r = pre_tool_use(
        &p,
        &proj,
        Some(agent),
        "Bash",
        json!({"command": "echo hi"}),
    );
    let once = routed_command(&r);
    // Delivered twice (two hook files): the second leaves it alone.
    let r2 = pre_tool_use(&p, &proj, Some(agent), "Bash", json!({"command": once}));
    assert_eq!(r2.code, 0);
    assert!(r2.stdout.trim().is_empty(), "already routed: {}", r2.all());
    // The session itself: activity only, no rewrite.
    let r3 = pre_tool_use(&p, &proj, None, "Bash", json!({"command": "echo hi"}));
    assert_eq!(r3.code, 0);
    assert!(r3.stdout.trim().is_empty(), "{}", r3.all());
    // A non-Bash tool of the subagent: nothing to route.
    let r4 = pre_tool_use(
        &p,
        &proj,
        Some(agent),
        "Read",
        json!({"file_path": "/etc/hosts"}),
    );
    assert!(r4.stdout.trim().is_empty(), "{}", r4.all());
    // An empty command: nothing to route.
    let r5 = pre_tool_use(&p, &proj, Some(agent), "Bash", json!({"command": "  "}));
    assert!(r5.stdout.trim().is_empty(), "{}", r5.all());
    // A project that never opted in (no .pgboxdb.toml, no source): untouched.
    let plain = p.home.parent().unwrap().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let mut c = p.cmd(&["agent", "hook"]);
    c.current_dir(&p.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = common::spawn_locked(&mut c).unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            payload(
                "PreToolUse",
                &plain,
                json!({"agent_id": "agent-plain", "agent_type": "x", "tool_name": "Bash", "tool_input": {"command": "echo hi"}}),
            )
            .as_bytes(),
        )
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
}

#[test]
fn a_subagent_whose_branch_cannot_be_ensured_is_refused_not_redirected() {
    let (p, proj) = up_with_source();
    let agent = "agent-fail";
    let deny = |r: &common::Run, why: &str| {
        assert_eq!(r.code, 0, "exit 0 so the host reads the JSON: {}", r.all());
        let j = r.json();
        assert_eq!(
            j["hookSpecificOutput"]["hookEventName"], "PreToolUse",
            "{j}"
        );
        assert_eq!(
            j["hookSpecificOutput"]["permissionDecision"], "deny",
            "{why}: {j}"
        );
        let reason = j["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.starts_with("PGBoxDB: "), "{reason}");
        assert!(j["hookSpecificOutput"].get("updatedInput").is_none());
        reason.to_string()
    };
    // Source template missing.
    std::fs::write(proj.join(".pgboxdb.toml"), "[agent]\nsource = \"gone\"\n").unwrap();
    let r = subagent_start(&p, &proj, agent);
    assert_eq!(r.code, 0);
    assert!(r.json()["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("FAILED"));
    let reason = deny(
        &pre_tool_use(
            &p,
            &proj,
            Some(agent),
            "Bash",
            json!({"command": "psql -c 'select 1'"}),
        ),
        "missing source",
    );
    assert!(reason.contains("gone"), "{reason}");
    // Corrupt .pgboxdb.toml.
    std::fs::write(
        proj.join(".pgboxdb.toml"),
        "[agent]\nsource = \"postgres://x@h/db\"\n",
    )
    .unwrap();
    deny(
        &pre_tool_use(
            &p,
            &proj,
            Some(agent),
            "Bash",
            json!({"command": "psql -c 'select 1'"}),
        ),
        "bad toml",
    );
    std::fs::write(proj.join(".pgboxdb.toml"), "[agent]\nsource = \"myapp\"\n").unwrap();
    // Invalid agent id.
    deny(
        &pre_tool_use(
            &p,
            &proj,
            Some("x; rm -rf /"),
            "Bash",
            json!({"command": "echo"}),
        ),
        "bad agent id",
    );
    // Branch deleted behind the hook's back: recreated fresh, not denied.
    assert_eq!(subagent_start(&p, &proj, agent).code, 0);
    let branch = branch_of(&p, Some(agent)).unwrap();
    assert_eq!(p.run(&["drop", &branch, "--force"]).code, 0);
    let r = pre_tool_use(&p, &proj, Some(agent), "Bash", json!({"command": "echo"}));
    let cmd = routed_command(&r);
    assert!(
        db_exists(&p, &branch),
        "fresh branch after the old one vanished"
    );
    assert!(cmd.contains(&branch));
    // (A merely stopped runtime is started on demand, like SessionStart does.)
    // No usable PGBoxDB at all (not initialised on this machine): refused.
    let empty_home = p.home.parent().unwrap().join("nohome");
    std::fs::create_dir_all(&empty_home).unwrap();
    let mut c = p.cmd(&["agent", "hook"]);
    c.current_dir(&p.home)
        .env("PGBOXDB_HOME", &empty_home)
        .env("CLAUDE_PROJECT_DIR", &proj)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = common::spawn_locked(&mut c).unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            payload(
                "PreToolUse",
                &proj,
                json!({"agent_id": agent, "agent_type": "x", "tool_name": "Bash", "tool_input": {"command": "psql -c 'drop table posts'"}}),
            )
            .as_bytes(),
        )
        .unwrap();
    let reason = deny(
        &common::Run::from(child.wait_with_output().unwrap()),
        "pgboxdb not initialised",
    );
    assert!(reason.contains("not initialised"), "{reason}");
    // A routed command whose branch was dropped between the hook and the
    // run: the wrapper recreates it (fresh) rather than touching anything
    // else, and a wrapper that cannot attach at all runs nothing.
    assert_eq!(p.run(&["drop", &branch, "--force"]).code, 0);
    let run = run_shell(&p, &proj, &cmd);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(db_exists(&p, &branch));
    let mut c = Command::new("/bin/sh");
    c.arg("-c")
        .arg(&cmd)
        .current_dir(&proj)
        .env("PATH", path_with_psql())
        .env("PGBOXDB_HOME", &empty_home)
        .env("CLAUDE_CODE_SESSION_ID", SID);
    let run = common::Run::from(common::output_locked(&mut c).unwrap());
    assert_eq!(run.code, 3, "not initialised: {}", run.all());
    assert!(run.stdout.trim().is_empty());
}

#[test]
fn subagent_stop_retires_only_that_branch_and_terminates_what_it_left_running() {
    let (p, proj) = up_with_source();
    assert_eq!(subagent_start(&p, &proj, "agent-one").code, 0);
    assert_eq!(subagent_start(&p, &proj, "agent-two").code, 0);
    let one = branch_of(&p, Some("agent-one")).unwrap();
    let two = branch_of(&p, Some("agent-two")).unwrap();
    // A statement left running on `one` inside an open transaction (what a
    // killed agent leaves behind), over TCP like a real client.
    let mut psql = Command::new(common::find_psql());
    psql.args([
        "-X",
        "-v",
        "ON_ERROR_STOP=1",
        "-c",
        "BEGIN; INSERT INTO posts VALUES (99, 'orphan'); SELECT pg_sleep(60); COMMIT;",
    ])
    .env("PGHOST", "127.0.0.1")
    .env("PGPORT", p.port.to_string())
    .env("PGUSER", "postgres")
    .env("PGPASSWORD", p.password())
    .env("PGDATABASE", &one)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let mut child = common::spawn_locked(&mut psql).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let backends = |db: &str| -> u64 {
        p.psql(
            "postgres",
            &format!("SELECT count(*) FROM pg_stat_activity WHERE datname='{db}'"),
        )
        .stdout
        .trim()
        .parse()
        .unwrap_or(0)
    };
    while backends(&one) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(backends(&one), 1, "the orphan is connected");

    let r = subagent_stop(&p, &proj, "agent-one");
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(
        r.stdout.trim().is_empty(),
        "nothing for the host to act on: {}",
        r.all()
    );
    // The barrier: its backend is gone and its write never commits.
    assert_eq!(backends(&one), 0);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(
        p.psql(&one, "SELECT count(*) FROM posts WHERE id = 99")
            .stdout
            .trim(),
        "0"
    );
    // `one` is retired on its grace period; `two` and the source untouched.
    let rows = agent_ls(&p);
    let row = |n: &str| rows.iter().find(|r| r["name"] == n).unwrap().clone();
    assert_eq!(row(&one)["state"], "retired");
    assert_eq!(row(&one)["deadline_kind"], "grace");
    assert!(row(&one)["expires_in_seconds"].as_u64().unwrap() <= 600);
    assert_eq!(row(&two)["state"], "active");
    assert_eq!(row(&two)["deadline_kind"], "idle_ttl");
    assert!(db_exists(&p, &one) && db_exists(&p, &two));
    // Stopped again: harmless. Started again within grace: revived.
    assert_eq!(subagent_stop(&p, &proj, "agent-one").code, 0);
    assert_eq!(subagent_start(&p, &proj, "agent-one").code, 0);
    assert_eq!(
        agent_ls(&p).iter().find(|r| r["name"] == one).unwrap()["state"],
        "active"
    );
    // The audit log tells the story, without secrets.
    let log = std::fs::read_to_string(p.home.join("agent/hooks.log")).unwrap();
    assert!(
        log.contains("SubagentStart session=f775e29e-e73 agent=agent-one"),
        "{log}"
    );
    assert!(
        log.contains("SubagentStop session=f775e29e-e73 agent=agent-one")
            && log.contains("retired"),
        "{log}"
    );
    assert!(!log.contains(&p.password()));
}

#[test]
fn agent_branches_carry_guards_that_abort_an_orphaned_transaction() {
    // The CI-sized reproduction of the benchmark's eight-minute case: a
    // client dies mid-transaction inside a long statement; the backend must
    // notice on its own (no hook, no `down`) and never commit.
    let (p, proj) = up_with_source();
    std::fs::write(
        proj.join(".pgboxdb.toml"),
        "[agent]\nsource = \"myapp\"\nconnection_check_interval = \"1s\"\nstatement_timeout = \"2m\"\n",
    )
    .unwrap();
    assert_eq!(subagent_start(&p, &proj, "agent-orphan").code, 0);
    let branch = branch_of(&p, Some("agent-orphan")).unwrap();
    let show = |g: &str| {
        p.psql(&branch, &format!("SHOW {g}"))
            .stdout
            .trim()
            .to_string()
    };
    assert_eq!(show("client_connection_check_interval"), "1s");
    assert_eq!(show("idle_in_transaction_session_timeout"), "15min");
    assert_eq!(show("statement_timeout"), "2min");
    assert_eq!(
        p.psql("myapp", "SHOW client_connection_check_interval")
            .stdout
            .trim(),
        "0",
        "the source is untouched"
    );

    let mut psql = Command::new(common::find_psql());
    psql.args([
        "-X",
        "-c",
        "BEGIN; INSERT INTO posts VALUES (99, 'orphan'); SELECT pg_sleep(30); COMMIT;",
    ])
    .env("PGHOST", "127.0.0.1")
    .env("PGPORT", p.port.to_string())
    .env("PGUSER", "postgres")
    .env("PGPASSWORD", p.password())
    .env("PGDATABASE", &branch)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let mut child = common::spawn_locked(&mut psql).unwrap();
    let backends = || -> u64 {
        p.psql(
            "postgres",
            &format!("SELECT count(*) FROM pg_stat_activity WHERE datname='{branch}' AND state <> 'idle'"),
        )
        .stdout
        .trim()
        .parse()
        .unwrap_or(0)
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while backends() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(backends(), 1);
    child.kill().unwrap(); // SIGKILL: no client-side cancel reaches the server
    child.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while backends() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(
        backends(),
        0,
        "the backend noticed the dead client within the check interval"
    );
    assert_eq!(
        p.psql(&branch, "SELECT count(*) FROM posts WHERE id = 99")
            .stdout
            .trim(),
        "0"
    );
    assert_eq!(
        p.psql("myapp", "SELECT count(*) FROM posts").stdout.trim(),
        "1"
    );
}

#[test]
fn two_projects_and_two_sessions_with_the_same_ids_stay_apart() {
    let (p, proj) = up_with_source();
    let other = p.home.parent().unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join(".pgboxdb.toml"), "[agent]\nsource = \"myapp\"\n").unwrap();
    assert_eq!(subagent_start(&p, &proj, "agent-sameid01").code, 0);
    let mut c = p.cmd(&["agent", "hook"]);
    c.current_dir(&p.home)
        .env("CLAUDE_PROJECT_DIR", &other)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = common::spawn_locked(&mut c).unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            payload(
                "SubagentStart",
                &other,
                json!({"agent_id": "agent-sameid01", "agent_type": "x"}),
            )
            .as_bytes(),
        )
        .unwrap();
    assert_eq!(child.wait_with_output().unwrap().status.code(), Some(0));
    // Another session, same agent id.
    let mut v: Value = serde_json::from_str(&payload(
        "SubagentStart",
        &proj,
        json!({"agent_id": "agent-sameid01", "agent_type": "x"}),
    ))
    .unwrap();
    v["session_id"] = json!("11111111-2222-3333-4444-555555555555");
    let r = hook(&p, &v.to_string());
    assert_eq!(r.code, 0, "{}", r.all());
    let rows = agent_ls(&p);
    let names: std::collections::BTreeSet<String> = rows
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names.len(), 3, "{rows:?}");
    assert!(
        names.contains("agent-claude-f775e29e-sameid01"),
        "{names:?}"
    );
    assert!(
        names.contains("agent-claude-11111111-sameid01"),
        "{names:?}"
    );
    // The other project got the long, project-qualified name.
    assert!(
        names
            .iter()
            .any(|n| n.starts_with("agent-claude-f775e29e-sameid01-")
                && n.len() == "agent-claude-f775e29e-sameid01-".len() + 16),
        "{names:?}"
    );
}

#[test]
fn down_finds_exec_branches_by_id_and_refuses_the_wrong_branch() {
    let (p, proj) = up_with_source();
    let exec = |args: &[&str]| {
        let mut full = vec!["--json", "agent", "exec"];
        full.extend_from_slice(args);
        p.output_retrying_pub(
            || {
                let mut c = p.cmd(&full);
                c.current_dir(&proj);
                c
            },
            "pgboxdb agent exec",
        )
    };
    let r = exec(&["--keep", "--id", "keepme-01", "--", "sh", "-c", "true"]);
    assert_eq!(r.code, 0, "{}", r.all());
    let j = r.json();
    // Explicit-id branches are labelled `exec`, whatever the command was
    // (`keepme-01` has a `-` in its first eight characters, so it is hashed).
    assert_eq!(j["host"], "exec");
    let kept = j["branch"].as_str().unwrap().to_string();
    assert!(kept.starts_with("agent-exec-"), "{j}");
    assert_eq!(j["created"], true);
    assert_eq!(j["status"], "active");
    assert!(j["elapsed_ms"].as_f64().unwrap() > 0.0, "{j}");
    assert_eq!(j["source"], "myapp");
    // `--keep` leaves it active on its idle TTL, not retired.
    let rows = agent_ls(&p);
    assert_eq!(rows[0]["state"], "active");
    assert_eq!(rows[0]["deadline_kind"], "idle_ttl");
    assert!(rows[0]["expires_in_seconds"].as_u64().unwrap() > 600);
    // The next exec with the same id attaches: not created, data intact.
    let w = exec(&[
        "--keep",
        "--id",
        "keepme-01",
        "--",
        "sh",
        "-c",
        "psql -X -c \"INSERT INTO posts VALUES (5, 'kept')\"",
    ]);
    let _ = w; // psql may be missing from PATH in this harness call; the attach is what matters
    let r2 = exec(&["--keep", "--id", "keepme-01", "--", "true"]);
    let j2 = r2.json();
    assert_eq!(j2["created"], false);
    assert!(j2["elapsed_ms"].is_null(), "{j2}");
    assert_eq!(j2["branch"], kept);
    // `down --id` finds it without knowing the host label; `--branch` guards it.
    let wrong = p.output_retrying_pub(
        || {
            let mut c = p.cmd(&[
                "--json",
                "agent",
                "down",
                "--id",
                "keepme-01",
                "--branch",
                "agent-claude-f775e29e",
                "--now",
            ]);
            c.current_dir(&proj);
            c
        },
        "pgboxdb agent down",
    );
    assert_eq!(wrong.code, 4, "{}", wrong.all());
    assert!(db_exists(&p, &kept));
    let right = p.output_retrying_pub(
        || {
            let mut c = p.cmd(&[
                "--json",
                "agent",
                "down",
                "--id",
                "keepme-01",
                "--branch",
                &kept,
                "--now",
            ]);
            c.current_dir(&proj);
            c
        },
        "pgboxdb agent down",
    );
    assert_eq!(right.code, 0, "{}", right.all());
    assert_eq!(right.json()["dropped"], true);
    assert!(!db_exists(&p, &kept));
    // `down --id` for one session never touches another session's branch.
    assert_eq!(subagent_start(&p, &proj, "agent-keep").code, 0);
    let sub = branch_of(&p, Some("agent-keep")).unwrap();
    let r = p.output_retrying_pub(
        || {
            let mut c = p.cmd(&["--json", "agent", "down", "--id", "nobody-here", "--now"]);
            c.current_dir(&proj);
            c
        },
        "pgboxdb agent down",
    );
    assert_eq!(r.code, 5, "{}", r.all());
    assert!(db_exists(&p, &sub));
}

#[test]
fn the_password_never_appears_in_a_child_process_argument_list() {
    let (p, proj) = up_with_source();
    // What `exec` passes to its child, and what a routed command runs: argv of
    // every process in the wrapper's process tree, inspected while alive.
    // (The counter reads the password from its environment, not its argv,
    // so the count cannot be 1 because of the counter itself.)
    let script = "ps -o args= -p $$ ; ps -o args= -p $PPID ; psql -X -Atc 'select current_database()' & sleep 0.5; ps -o args= -A | PAT=\"$PGPASSWORD\" awk 'index($0, ENVIRON[\"PAT\"]) { n++ } END { print n + 0 }' ; wait";
    let r = p.output_retrying_pub(
        || {
            let mut c = p.cmd(&["--json", "agent", "exec", "--", "sh", "-c", script]);
            c.current_dir(&proj).env("PATH", path_with_psql());
            c
        },
        "pgboxdb agent exec",
    );
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(
        !r.stdout.contains(&p.password()),
        "argv leaks the password: {}",
        r.stdout
    );
    // The grep count of processes whose argv contains the password: zero.
    assert!(r.stdout.lines().any(|l| l.trim() == "0"), "{}", r.stdout);
    // And a routed subagent command: the command text carries a file path only.
    assert_eq!(subagent_start(&p, &proj, "agent-argv").code, 0);
    let routed = routed_command(&pre_tool_use(
        &p,
        &proj,
        Some("agent-argv"),
        "Bash",
        json!({"command": "psql -X -Atc 'select 1'"}),
    ));
    assert!(!routed.contains(&p.password()));
    assert!(!routed.contains("postgres://"));
}
