//! Black-box tests for `pgboxdb agent hook`: the one entry point Claude Code,
//! Cursor and Codex call. Synthetic payloads in the hosts' exact shapes.
mod common;
use common::Pgboxdb;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

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

/// Run `pgboxdb agent hook [args]` from the HOME dir (not the project: the payload's
/// cwd must be what locates the project) with `payload` on stdin.
fn hook(p: &Pgboxdb, env: &[(&str, &str)], args: &[&str], payload: &str) -> common::Run {
    let mut full = vec!["agent", "hook"];
    full.extend_from_slice(args);
    let mut c = p.cmd(&full);
    c.current_dir(&p.home);
    for (k, v) in env {
        c.env(k, v);
    }
    c.stdin(Stdio::piped())
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

fn claude(event: &str, sid: &str, cwd: &Path, extra: Value) -> String {
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

fn cursor(event: &str, cid: &str, root: &Path, extra: Value) -> String {
    let mut v = json!({
        "conversation_id": cid,
        "generation_id": "g",
        "model": "m",
        "hook_event_name": event,
        "cursor_version": "3.0.0",
        "workspace_roots": [root],
        "user_email": null,
        "transcript_path": null
    });
    for (k, val) in extra.as_object().unwrap() {
        v[k] = val.clone();
    }
    v.to_string()
}

fn codex(event: &str, sid: &str, cwd: &Path, extra: Value) -> String {
    let mut v = json!({
        "session_id": sid,
        "cwd": cwd,
        "hook_event_name": event,
        "model": "gpt-5-codex",
        "permission_mode": "default",
        "transcript_path": null
    });
    for (k, val) in extra.as_object().unwrap() {
        v[k] = val.clone();
    }
    v.to_string()
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

fn env_file(p: &Pgboxdb, tag: &str) -> PathBuf {
    let d = p.home.parent().unwrap().join("session-env");
    std::fs::create_dir_all(&d).unwrap();
    d.join(format!("{tag}.sh"))
}

#[test]
fn claude_session_start_writes_env_file_and_context() {
    let (p, proj) = up_with_source();
    let ef = env_file(&p, "a");
    std::fs::write(&ef, "export OTHER_HOOK=1\n").unwrap();
    let sid = "9b1f3c2d-aaaa-bbbb-cccc-dddddddddddd";
    let r = hook(
        &p,
        &[("CLAUDE_ENV_FILE", ef.to_str().unwrap())],
        &[],
        &claude("SessionStart", sid, &proj, json!({"source": "startup"})),
    );
    assert_eq!(r.code, 0, "{}", r.all());
    let text = std::fs::read_to_string(&ef).unwrap();
    assert!(
        text.starts_with("export OTHER_HOOK=1\n"),
        "other hooks' lines survive: {text}"
    );
    assert!(
        text.contains("export DATABASE_URL='postgres://postgres:"),
        "{text}"
    );
    assert!(text.contains("/agent-claude-9b1f3c2d'\n"), "{text}");
    assert!(
        text.contains("export PGDATABASE='agent-claude-9b1f3c2d'"),
        "{text}"
    );
    assert!(text.contains("export PGBOXDB_ISOLATION='ok'"), "{text}");
    assert!(
        r.stdout.starts_with(
            "PGBoxDB: this session's database is branch agent-claude-9b1f3c2d (a copy of myapp)."
        ),
        "{}",
        r.stdout
    );
    assert!(
        r.stdout
            .contains("expires 2 hours after your last activity"),
        "{}",
        r.stdout
    );
    assert!(
        !r.stdout.contains(&p.password()),
        "the context line never carries the password"
    );
    assert!(db_exists(&p, "agent-claude-9b1f3c2d"));
    // A resume delivers SessionStart again: same branch, no second clone.
    let r2 = hook(
        &p,
        &[("CLAUDE_ENV_FILE", ef.to_str().unwrap())],
        &[],
        &claude("SessionStart", sid, &proj, json!({"source": "resume"})),
    );
    assert_eq!(r2.code, 0, "{}", r2.all());
    assert_eq!(agent_ls(&p).len(), 1);
}

#[test]
fn claude_prompt_and_tool_events_touch_quietly() {
    let (p, proj) = up_with_source();
    let ef = env_file(&p, "b");
    let sid = "4e77a0c1-aaaa-bbbb-cccc-dddddddddddd";
    assert_eq!(
        hook(
            &p,
            &[("CLAUDE_ENV_FILE", ef.to_str().unwrap())],
            &[],
            &claude("SessionStart", sid, &proj, json!({}))
        )
        .code,
        0
    );
    // Make the record look stale so the touch is not rate-limited.
    for e in std::fs::read_dir(p.home.join("ttl")).unwrap() {
        let path = e.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let mut v: Value = serde_json::from_str(&text).unwrap();
        v["agent"]["last_touch"] = json!(1_000_000);
        v["expires_at"] = json!(2_000_000_000u64);
        std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    }
    for ev in ["UserPromptSubmit", "PreToolUse"] {
        let r = hook(
            &p,
            &[("CLAUDE_CODE_SESSION_ID", sid)],
            &[],
            &claude(ev, sid, &proj, json!({"tool_name": "Bash"})),
        );
        assert_eq!(r.code, 0, "{}", r.all());
        assert_eq!(r.stdout, "", "{ev} prints nothing");
    }
    let row = agent_ls(&p)
        .into_iter()
        .find(|r| r["name"] == "agent-claude-4e77a0c1")
        .unwrap();
    let left = row["expires_in_seconds"].as_u64().unwrap();
    assert!(
        (7_000..=7_200).contains(&left),
        "touched back to ~2h: {row}"
    );
}

#[test]
fn claude_session_end_retires_with_grace() {
    let (p, proj) = up_with_source();
    let ef = env_file(&p, "c");
    let sid = "c3c3c3c3-aaaa-bbbb-cccc-dddddddddddd";
    assert_eq!(
        hook(
            &p,
            &[("CLAUDE_ENV_FILE", ef.to_str().unwrap())],
            &[],
            &claude("SessionStart", sid, &proj, json!({}))
        )
        .code,
        0
    );
    let r = hook(
        &p,
        &[("CLAUDE_CODE_SESSION_ID", sid)],
        &[],
        &claude(
            "SessionEnd",
            sid,
            &proj,
            json!({"reason": "prompt_input_exit"}),
        ),
    );
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(r.stdout, "");
    let row = agent_ls(&p)
        .into_iter()
        .find(|r| r["name"] == "agent-claude-c3c3c3c3")
        .unwrap();
    assert_eq!(row["state"], "retired", "{row}");
    assert!(row["expires_in_seconds"].as_u64().unwrap() <= 600, "{row}");
    assert!(db_exists(&p, "agent-claude-c3c3c3c3"));
    // A SessionEnd for a session that never started is fine.
    let r = hook(
        &p,
        &[("CLAUDE_CODE_SESSION_ID", "nobody-1")],
        &[],
        &claude("SessionEnd", "nobody-1", &proj, json!({})),
    );
    assert_eq!(r.code, 0, "{}", r.all());
}

#[test]
fn cursor_session_start_returns_env_json_and_background_is_skipped() {
    let (p, proj) = up_with_source();
    let cid = "c0ffee00-1111-2222-3333-444444444444";
    let r = hook(
        &p,
        &[("CURSOR_VERSION", "3.0.0")],
        &[],
        &cursor(
            "sessionStart",
            cid,
            &proj,
            json!({"session_id": cid, "is_background_agent": false}),
        ),
    );
    assert_eq!(r.code, 0, "{}", r.all());
    let j = r.json();
    assert!(
        j["env"]["DATABASE_URL"]
            .as_str()
            .unwrap()
            .ends_with("/agent-cursor-c0ffee00"),
        "{j}"
    );
    assert_eq!(j["env"]["PGBOXDB_BRANCH"], "agent-cursor-c0ffee00", "{j}");
    assert_eq!(j["env"]["PGDATABASE"], "agent-cursor-c0ffee00", "{j}");
    assert_eq!(j["env"]["PGBOXDB_ISOLATION"], "ok", "{j}");
    assert!(
        j["additional_context"]
            .as_str()
            .unwrap()
            .starts_with("PGBoxDB: this session's database is branch agent-cursor-c0ffee00"),
        "{j}"
    );
    assert!(db_exists(&p, "agent-cursor-c0ffee00"));
    // Background (cloud) agents get nothing and create nothing.
    let r = hook(
        &p,
        &[("CURSOR_VERSION", "3.0.0")],
        &[],
        &cursor(
            "sessionStart",
            "bbbbbbbb-1111-2222-3333-444444444444",
            &proj,
            json!({"is_background_agent": true}),
        ),
    );
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(r.stdout.trim(), "{}");
    assert!(!db_exists(&p, "agent-cursor-bbbbbbbb"));
}

#[test]
fn cursor_same_conversation_delivered_twice_makes_one_branch() {
    let (p, proj) = up_with_source();
    let cid = "d0d0d0d0-1111-2222-3333-444444444444";
    // Native delivery and the Claude-compat delivery (CLAUDE_PROJECT_DIR set,
    // no CURSOR_VERSION) arrive together.
    let native = cursor("sessionStart", cid, &proj, json!({}));
    let compat = native.clone();
    let mut kids = Vec::new();
    for (env_k, env_v, payload) in [
        ("CURSOR_VERSION", "3.0.0", native),
        ("CLAUDE_PROJECT_DIR", proj.to_str().unwrap(), compat),
    ] {
        let mut c = p.cmd(&["agent", "hook"]);
        c.current_dir(&p.home).env(env_k, env_v);
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = common::spawn_locked(&mut c).unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        kids.push(child);
    }
    let mut branches = std::collections::BTreeSet::new();
    for k in kids {
        let r = common::Run::from(k.wait_with_output().unwrap());
        assert_eq!(r.code, 0, "{}", r.all());
        branches.insert(
            r.json()["env"]["PGBOXDB_BRANCH"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    assert_eq!(branches.len(), 1, "{branches:?}");
    assert_eq!(agent_ls(&p).len(), 1);
    // Activity and end through Cursor's events.
    for ev in [
        "beforeSubmitPrompt",
        "afterShellExecution",
        "beforeShellExecution",
    ] {
        let r = hook(
            &p,
            &[("CURSOR_VERSION", "3.0.0")],
            &[],
            &cursor(ev, cid, &proj, json!({"command": "ls"})),
        );
        assert_eq!(r.code, 0, "{}", r.all());
        assert_eq!(r.stdout.trim(), "{}", "{ev}");
    }
    let r = hook(
        &p,
        &[("CURSOR_VERSION", "3.0.0")],
        &[],
        &cursor("sessionEnd", cid, &proj, json!({"reason": "user_close"})),
    );
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(agent_ls(&p)[0]["state"], "retired");
}

#[test]
fn codex_session_start_returns_additional_context_with_exec_advice() {
    let (p, proj) = up_with_source();
    let sid = "0199c3a2-7b1e-7c3f-9d2a-5e6f7a8b9c0d";
    let r = hook(
        &p,
        &[
            ("CODEX_THREAD_ID", "t1"),
            ("CODEX_SANDBOX_NETWORK_DISABLED", "1"),
        ],
        &[],
        &codex("SessionStart", sid, &proj, json!({"source": "startup"})),
    );
    assert_eq!(r.code, 0, "{}", r.all());
    let j = r.json();
    assert_eq!(
        j["hookSpecificOutput"]["hookEventName"], "SessionStart",
        "{j}"
    );
    let ctx = j["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        ctx.starts_with("PGBoxDB: this session's database is branch agent-codex-0199c3a2"),
        "{ctx}"
    );
    assert!(ctx.contains("pgboxdb agent exec -- "), "{ctx}");
    assert!(
        ctx.contains("sandbox_workspace_write.network_access"),
        "{ctx}"
    );
    assert!(!ctx.contains(&p.password()));
    assert!(
        j.get("env").is_none(),
        "Codex hooks cannot set env; none is claimed"
    );
    assert!(db_exists(&p, "agent-codex-0199c3a2"));
    let r = hook(
        &p,
        &[("CODEX_SESSION_ID", sid)],
        &[],
        &codex("UserPromptSubmit", sid, &proj, json!({})),
    );
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout, "");
    let r = hook(
        &p,
        &[("CODEX_SESSION_ID", sid)],
        &[],
        &codex("SessionEnd", sid, &proj, json!({"reason": "other"})),
    );
    assert_eq!(r.code, 0);
    assert_eq!(agent_ls(&p)[0]["state"], "retired");
}

#[test]
fn missing_source_with_pgboxdb_toml_fails_closed() {
    let (p, proj) = up_with_source();
    std::fs::write(proj.join(".pgboxdb.toml"), "[agent]\nsource = \"nope\"\n").unwrap();
    let ef = env_file(&p, "d");
    let sid = "e5e5e5e5-aaaa-bbbb-cccc-dddddddddddd";
    let r = hook(
        &p,
        &[("CLAUDE_ENV_FILE", ef.to_str().unwrap())],
        &[],
        &claude("SessionStart", sid, &proj, json!({})),
    );
    assert_eq!(
        r.code,
        0,
        "fail closed travels on exit 0 (hosts drop non-zero output): {}",
        r.all()
    );
    assert!(r.stderr.contains("nope"), "{}", r.all());
    assert!(
        r.stdout
            .starts_with("PGBoxDB isolation FAILED for this session"),
        "{}",
        r.stdout
    );
    let text = std::fs::read_to_string(&ef).unwrap();
    assert!(text.contains("export PGBOXDB_ISOLATION='failed'"), "{text}");
    assert!(text.contains("export DATABASE_URL='postgres://pgboxdb-isolation-failed@127.0.0.1:1/pgboxdb_isolation_failed'"), "{text}");
    assert!(text.contains("export PGPORT='1'"), "{text}");
    assert!(!text.contains(&p.password()));
    assert!(agent_ls(&p).is_empty());
    // The sentinel really cannot connect.
    let out = std::process::Command::new(common::find_psql())
        .args([
            "-X",
            "-Atc",
            "select 1",
            "postgres://pgboxdb-isolation-failed@127.0.0.1:1/pgboxdb_isolation_failed",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    // Cursor gets the same sentinel through env.
    let r = hook(
        &p,
        &[("CURSOR_VERSION", "3.0.0")],
        &[],
        &cursor(
            "sessionStart",
            "f6f6f6f6-1111-2222-3333-444444444444",
            &proj,
            json!({}),
        ),
    );
    assert_eq!(r.code, 0, "Cursor applies env only on exit 0: {}", r.all());
    let j = r.json();
    assert_eq!(j["env"]["PGBOXDB_ISOLATION"], "failed", "{j}");
    assert!(
        j["additional_context"]
            .as_str()
            .unwrap()
            .starts_with("PGBoxDB isolation FAILED"),
        "{j}"
    );
}

#[test]
fn uninitialised_pgboxdb_with_pgboxdb_toml_fails_closed_and_without_it_only_warns() {
    let p = Pgboxdb::new(); // never initialised
    let proj = p.home.parent().unwrap().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let ef = env_file(&p, "e");
    let sid = "a7a7a7a7-aaaa-bbbb-cccc-dddddddddddd";
    // No .pgboxdb.toml: a notice, no env.
    let r = hook(
        &p,
        &[("CLAUDE_ENV_FILE", ef.to_str().unwrap())],
        &[],
        &claude("SessionStart", sid, &proj, json!({})),
    );
    assert_eq!(r.code, 1, "{}", r.all());
    assert!(r.stderr.contains("not initialised"), "{}", r.all());
    assert!(!ef.exists() || std::fs::read_to_string(&ef).unwrap().is_empty());
    // With .pgboxdb.toml: fail closed.
    std::fs::write(proj.join(".pgboxdb.toml"), "[agent]\nsource = \"myapp\"\n").unwrap();
    let r = hook(
        &p,
        &[("CLAUDE_ENV_FILE", ef.to_str().unwrap())],
        &[],
        &claude("SessionStart", sid, &proj, json!({})),
    );
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(std::fs::read_to_string(&ef)
        .unwrap()
        .contains("PGBOXDB_ISOLATION='failed'"));
}

#[test]
fn hook_never_exits_2_and_tolerates_garbage() {
    let (p, proj) = up_with_source();
    let ef = env_file(&p, "f");
    for payload in [
        "",
        "not json",
        "[]",
        "{}",
        "{\"hook_event_name\":\"SessionStart\",\"session_id\":\"../../etc\"}",
    ] {
        let r = hook(
            &p,
            &[("CLAUDE_ENV_FILE", ef.to_str().unwrap())],
            &[],
            payload,
        );
        assert!(r.code == 0 || r.code == 1, "{payload:?}: {}", r.all());
        assert_ne!(r.code, 2);
    }
    assert!(agent_ls(&p).is_empty());
    // Unknown --host is the one usage error, and it is reported, not silent.
    let r = hook(
        &p,
        &[],
        &["--host", "emacs"],
        &claude("SessionStart", "abcd1234", &proj, json!({})),
    );
    assert_eq!(r.code, 1, "{}", r.all());
    assert!(r.stderr.contains("emacs"));
}

#[test]
fn hook_ignores_cursor_events_it_does_not_handle() {
    let (p, proj) = up_with_source();
    let r = hook(
        &p,
        &[("CURSOR_VERSION", "3.0.0")],
        &[],
        &cursor(
            "afterFileEdit",
            "e0e0e0e0-1111-2222-3333-444444444444",
            &proj,
            json!({}),
        ),
    );
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(r.stdout.trim(), "{}");
    assert!(agent_ls(&p).is_empty());
    let r = hook(
        &p,
        &[("CLAUDE_ENV_FILE", "/dev/null")],
        &[],
        &claude(
            "Notification",
            "e1e1e1e1-aaaa-bbbb-cccc-dddddddddddd",
            &proj,
            json!({}),
        ),
    );
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(r.stdout, "");
}

#[test]
fn codex_hook_without_host_variables_is_still_codex_and_leaks_nothing() {
    // Codex hook processes carry no CODEX_* variables; a PascalCase payload
    // with no CLAUDE_* variables is Codex, and in no case may the hook print
    // a password-bearing URL anywhere.
    let (p, proj) = up_with_source();
    let sid = "0199c3a2-7b1e-7c3f-9d2a-5e6f7a8b9c0e";
    let r = hook(
        &p,
        &[],
        &[],
        &codex("SessionStart", sid, &proj, json!({"source": "startup"})),
    );
    assert_eq!(r.code, 0, "{}", r.all());
    let j = r.json();
    assert!(
        j["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("agent-codex-0199c3a2"),
        "{j}"
    );
    assert!(
        !r.stderr.contains(&p.password()) && !r.stdout.contains(&p.password()),
        "{}",
        r.all()
    );
    // Forced --host codex (what install writes into .codex/hooks.json) works the same.
    let r = hook(
        &p,
        &[],
        &["--host", "codex"],
        &codex("UserPromptSubmit", sid, &proj, json!({})),
    );
    assert_eq!(r.code, 0, "{}", r.all());
    // A Claude payload whose CLAUDE_ENV_FILE is missing must not print exports with secrets either.
    let r = hook(
        &p,
        &[("CLAUDE_PROJECT_DIR", proj.to_str().unwrap())],
        &[],
        &claude(
            "SessionStart",
            "abababab-aaaa-bbbb-cccc-dddddddddddd",
            &proj,
            json!({}),
        ),
    );
    assert!(
        !r.stderr.contains(&p.password()) && !r.stdout.contains(&p.password()),
        "{}",
        r.all()
    );
}
