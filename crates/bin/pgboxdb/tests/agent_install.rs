//! Black-box tests for `pgboxdb agent install|uninstall|setup|doctor`. Every run
//! uses a scratch HOME so nothing can touch the developer's real hook files.
mod common;
use common::PGBoxDB;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;

const MARKER: &str = "pgboxdb agent hook";

struct Lab {
    p: PGBoxDB,
    home: PathBuf, // scratch $HOME
    proj: PathBuf,
}

fn lab(init: bool) -> Lab {
    let p = PGBoxDB::new();
    if init {
        p.init_ok();
        p.start_ok();
        assert_eq!(p.run(&["create", "myapp"]).code, 0);
        assert_eq!(
            p.psql("myapp", "CREATE TABLE posts(id int PRIMARY KEY)")
                .code,
            0
        );
    }
    let base = p.home.parent().unwrap().to_path_buf();
    let home = base.join("userhome");
    let proj = base.join("proj");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(proj.join(".git")).unwrap();
    Lab { p, home, proj }
}

impl Lab {
    fn run(&self, dir: &Path, env: &[(&str, &str)], args: &[&str]) -> common::Run {
        self.p.output_retrying_pub(
            || {
                let mut full = vec!["--json", "agent"];
                full.extend_from_slice(args);
                let mut c = self.p.cmd(&full);
                c.current_dir(dir)
                    .env("HOME", &self.home)
                    .env("PGBOXDB_PORT", self.p.port.to_string());
                for (k, v) in env {
                    c.env(k, v);
                }
                c
            },
            "pgboxdb agent",
        )
    }
    fn read(&self, rel: &str) -> Value {
        let path = if rel.starts_with('~') {
            self.home.join(&rel[2..])
        } else {
            self.proj.join(rel)
        };
        serde_json::from_str(
            &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
        )
        .unwrap()
    }
    fn link_pgboxdb_into_local_bin(&self) {
        let bin = self.home.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(self.p.bin(), bin.join("pgboxdb")).unwrap();
    }
}

fn ours(entries: &Value) -> usize {
    entries
        .as_array()
        .map(|a| a.iter().filter(|e| e.to_string().contains(MARKER)).count())
        .unwrap_or(0)
}

#[test]
fn install_merges_and_preserves_foreign_hooks() {
    let l = lab(true);
    std::fs::create_dir_all(l.proj.join(".claude")).unwrap();
    std::fs::write(
        l.proj.join(".claude/settings.json"),
        json!({
            "env": {"FOO": "1"},
            "permissions": {"allow": ["Bash(ls:*)"]},
            "hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "orca-hook start"}]}],
                      "PreToolUse": [{"matcher": "Edit", "hooks": [{"type": "command", "command": "orca-hook edit"}]}]}
        })
        .to_string(),
    )
    .unwrap();
    let r = l.run(&l.proj, &[], &["install", "claude"]);
    assert_eq!(r.code, 0, "{}", r.all());
    let j = r.json();
    let h = &j["hosts"][0];
    assert_eq!(h["host"], "claude", "{j}");
    assert!(
        h["file"]
            .as_str()
            .unwrap()
            .ends_with(".claude/settings.json"),
        "{j}"
    );
    assert_eq!(h["added"], 6, "{j}");
    assert_eq!(h["present"], 0, "{j}");
    let s = l.read(".claude/settings.json");
    assert_eq!(s["env"]["FOO"], "1");
    assert_eq!(s["permissions"]["allow"][0], "Bash(ls:*)");
    assert_eq!(
        s["hooks"]["SessionStart"][0]["hooks"][0]["command"], "orca-hook start",
        "foreign entry first and intact"
    );
    assert_eq!(ours(&s["hooks"]["SessionStart"]), 1);
    assert_eq!(ours(&s["hooks"]["UserPromptSubmit"]), 1);
    assert_eq!(ours(&s["hooks"]["SessionEnd"]), 1);
    assert_eq!(ours(&s["hooks"]["PreToolUse"]), 1);
    assert_eq!(s["hooks"]["PreToolUse"][0]["matcher"], "Edit");
    assert!(
        !s.to_string().contains("--host"),
        "the Claude file is host-less: {s}"
    );
    let mine = s["hooks"]["PreToolUse"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e.to_string().contains(MARKER))
        .unwrap();
    assert_eq!(mine["matcher"], "Bash", "{mine}");
    assert_eq!(mine["hooks"][0]["timeout"], 15, "{mine}");
    for ev in ["SubagentStart", "SubagentStop"] {
        assert_eq!(ours(&s["hooks"][ev]), 1, "{ev}");
        assert!(
            s["hooks"][ev][0].get("matcher").is_none(),
            "{ev} matches every agent type"
        );
    }
    let start = s["hooks"]["SessionStart"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e.to_string().contains(MARKER))
        .unwrap();
    assert_eq!(start["hooks"][0]["timeout"], 30, "{start}");
    let cmd = start["hooks"][0]["command"].as_str().unwrap();
    assert!(
        cmd.contains("command -v pgboxdb") && cmd.contains("exit 0"),
        "guarded: {cmd}"
    );
    // Idempotent.
    let j2 = l.run(&l.proj, &[], &["install", "claude"]).json();
    assert_eq!(j2["hosts"][0]["added"], 0, "{j2}");
    assert_eq!(j2["hosts"][0]["present"], 6, "{j2}");
    assert_eq!(
        ours(&l.read(".claude/settings.json")["hooks"]["SessionStart"]),
        1
    );
    // .pgboxdb.toml was written with the default source.
    let toml = std::fs::read_to_string(l.proj.join(".pgboxdb.toml")).unwrap();
    assert!(toml.contains("source = \"app\""), "{toml}");
    assert!(!toml.contains("://"));
}

#[test]
fn install_cursor_codex_and_scopes() {
    let l = lab(true);
    let j = l
        .run(
            &l.proj,
            &[],
            &["install", "cursor", "codex", "--source", "myapp"],
        )
        .json();
    assert_eq!(j["hosts"].as_array().unwrap().len(), 2, "{j}");
    let c = l.read(".cursor/hooks.json");
    assert_eq!(c["version"], 1);
    for ev in [
        "sessionStart",
        "beforeSubmitPrompt",
        "afterShellExecution",
        "sessionEnd",
    ] {
        assert_eq!(ours(&c["hooks"][ev]), 1, "{ev}: {c}");
        assert!(
            c["hooks"][ev][0]["command"]
                .as_str()
                .unwrap()
                .starts_with("/bin/sh -c"),
            "{c}"
        );
    }
    assert!(
        c["hooks"].get("beforeShellExecution").is_none(),
        "touch never sits on a permission hook: {c}"
    );
    let x = l.read(".codex/hooks.json");
    for ev in ["SessionStart", "UserPromptSubmit", "SessionEnd"] {
        assert_eq!(ours(&x["hooks"][ev]), 1, "{ev}: {x}");
    }
    // Host-specific files name their host (Codex hooks carry no host variables);
    // the Claude file stays host-less because Cursor's compat loader reads it too.
    assert!(
        x["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("pgboxdb agent hook --host codex"),
        "{x}"
    );
    assert!(
        c["hooks"]["sessionStart"][0]["command"]
            .as_str()
            .unwrap()
            .contains("pgboxdb agent hook --host cursor"),
        "{c}"
    );
    assert!(std::fs::read_to_string(l.proj.join(".pgboxdb.toml"))
        .unwrap()
        .contains("source = \"myapp\""));
    // --scope local for Claude goes to settings.local.json; --scope user to $HOME.
    l.run(&l.proj, &[], &["install", "claude", "--scope", "local"]);
    assert_eq!(
        ours(&l.read(".claude/settings.local.json")["hooks"]["SessionStart"]),
        1
    );
    assert!(!l.proj.join(".claude/settings.json").exists());
    l.run(
        &l.proj,
        &[],
        &["install", "claude", "cursor", "codex", "--scope", "user"],
    );
    assert_eq!(
        ours(&l.read("~/.claude/settings.json")["hooks"]["SessionStart"]),
        1
    );
    assert_eq!(
        ours(&l.read("~/.cursor/hooks.json")["hooks"]["sessionStart"]),
        1
    );
    assert_eq!(
        ours(&l.read("~/.codex/hooks.json")["hooks"]["SessionStart"]),
        1
    );
    // A conflicting --source against an existing .pgboxdb.toml is refused (exit 4).
    let r = l.run(&l.proj, &[], &["install", "cursor", "--source", "other"]);
    assert_eq!(r.code, 4, "{}", r.all());
    // Unknown host.
    assert_eq!(l.run(&l.proj, &[], &["install", "emacs"]).code, 2);
}

#[test]
fn install_detects_hosts_from_the_machine_when_none_are_named() {
    let l = lab(true);
    std::fs::create_dir_all(l.home.join(".cursor")).unwrap();
    std::fs::create_dir_all(l.proj.join(".claude")).unwrap();
    let j = l.run(&l.proj, &[], &["install"]).json();
    let hosts: Vec<&str> = j["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["host"].as_str().unwrap())
        .collect();
    assert_eq!(hosts, vec!["claude", "cursor"], "{j}");
    // Nothing detected: a clear error rather than a silent no-op.
    let l2 = lab(false);
    let r = l2.run(&l2.proj, &[], &["install"]);
    assert_eq!(r.code, 2, "{}", r.all());
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no agent host"),
        "{}",
        r.all()
    );
}

#[test]
fn install_refuses_invalid_json_and_leaves_it_alone() {
    let l = lab(true);
    std::fs::create_dir_all(l.proj.join(".claude")).unwrap();
    std::fs::write(l.proj.join(".claude/settings.json"), "{not json").unwrap();
    let r = l.run(&l.proj, &[], &["install", "claude"]);
    assert_eq!(r.code, 1, "{}", r.all());
    assert_eq!(
        std::fs::read_to_string(l.proj.join(".claude/settings.json")).unwrap(),
        "{not json"
    );
}

#[test]
fn uninstall_removes_only_ours() {
    let l = lab(true);
    std::fs::create_dir_all(l.proj.join(".claude")).unwrap();
    std::fs::write(
        l.proj.join(".claude/settings.json"),
        json!({"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "orca-hook"}]}]}}).to_string(),
    )
    .unwrap();
    l.run(&l.proj, &[], &["install", "claude", "cursor"]);
    let j = l
        .run(&l.proj, &[], &["uninstall", "claude", "cursor"])
        .json();
    assert_eq!(j["hosts"][0]["removed"], 6, "{j}");
    assert_eq!(j["hosts"][1]["removed"], 4, "{j}");
    let s = l.read(".claude/settings.json");
    assert_eq!(s["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
    assert_eq!(
        s["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "orca-hook"
    );
    assert!(
        s["hooks"].get("UserPromptSubmit").is_none(),
        "empty event arrays are removed: {s}"
    );
    let c = l.read(".cursor/hooks.json");
    assert_eq!(c["version"], 1);
    assert!(c["hooks"].as_object().unwrap().is_empty(), "{c}");
    assert!(
        l.proj.join(".pgboxdb.toml").exists(),
        "uninstall never deletes project config"
    );
    // Running again is a no-op.
    let j = l.run(&l.proj, &[], &["uninstall", "claude"]).json();
    assert_eq!(j["hosts"][0]["removed"], 0, "{j}");
}

#[test]
fn hook_command_self_test_resolves_pgboxdb_on_the_installed_path() {
    let l = lab(true);
    // Without pgboxdb on the hook's PATH the self-test says so (and install still succeeds).
    let j = l
        .run(
            &l.proj,
            &[("PATH", "/usr/bin:/bin")],
            &["install", "claude"],
        )
        .json();
    assert_eq!(j["selftest"]["ok"], false, "{j}");
    assert!(
        j["selftest"]["hint"].as_str().unwrap().contains("PATH"),
        "{j}"
    );
    // With ~/.local/bin/pgboxdb (the installer's location) it resolves.
    l.link_pgboxdb_into_local_bin();
    let j = l
        .run(
            &l.proj,
            &[("PATH", "/usr/bin:/bin")],
            &["install", "claude"],
        )
        .json();
    assert_eq!(j["selftest"]["ok"], true, "{j}");
    assert!(
        j["selftest"]["pgboxdb"]
            .as_str()
            .unwrap()
            .ends_with(".local/bin/pgboxdb"),
        "{j}"
    );
    // The version comes from the distribution's dist.json (CI labels it
    // 0.0.0-ci.<sha>), so only its presence is asserted.
    assert!(
        j["selftest"]["version"]
            .as_str()
            .is_some_and(|v| !v.is_empty()),
        "{j}"
    );
}

#[test]
fn setup_writes_toml_installs_hooks_and_verifies_a_branch() {
    let l = lab(true);
    l.link_pgboxdb_into_local_bin();
    let r = l.run(
        &l.proj,
        &[],
        &["setup", "--source", "myapp", "--hosts", "claude,cursor"],
    );
    assert_eq!(r.code, 0, "{}", r.all());
    let j = r.json();
    assert_eq!(j["source"]["name"], "myapp", "{j}");
    assert_eq!(j["source"]["created"], false, "{j}");
    assert_eq!(j["source"]["connections"], 0, "{j}");
    assert_eq!(j["toml"]["written"], true, "{j}");
    assert_eq!(j["hooks"].as_array().unwrap().len(), 2, "{j}");
    assert_eq!(j["runtime"]["port"], l.p.port, "{j}");
    assert!(j["check"]["elapsed_ms"].as_f64().unwrap() > 0.0, "{j}");
    assert!(
        j["check"]["branch"]
            .as_str()
            .unwrap()
            .starts_with("agent-setup-"),
        "{j}"
    );
    assert_eq!(j["selftest"]["ok"], true, "{j}");
    let toml = std::fs::read_to_string(l.proj.join(".pgboxdb.toml")).unwrap();
    assert!(toml.contains("source = \"myapp\""), "{toml}");
    assert!(toml.contains("idle_ttl = \"2h\""), "{toml}");
    assert!(toml.contains("grace_period = \"10m\""), "{toml}");
    assert_eq!(
        ours(&l.read(".claude/settings.json")["hooks"]["SessionStart"]),
        1
    );
    assert_eq!(
        ours(&l.read(".cursor/hooks.json")["hooks"]["sessionStart"]),
        1
    );
    // The check branch is gone; no agent branches remain.
    let ls = l.p.run(&["--json", "agent", "ls"]).json();
    assert!(ls.as_array().unwrap().is_empty(), "{ls}");
    // Human output has the summary block.
    let h = l.p.output_retrying_pub(
        || {
            let mut c = l.p.cmd(&["agent", "setup", "--hosts", "claude"]);
            c.current_dir(&l.proj).env("HOME", &l.home);
            c
        },
        "pgboxdb agent setup",
    );
    assert!(
        h.stdout.contains("PGBoxDB agent setup complete") && h.stdout.contains("Next:"),
        "{}",
        h.all()
    );
    // --no-hooks writes config only.
    let l2 = lab(true);
    let j = l2
        .run(&l2.proj, &[], &["setup", "--source", "myapp", "--no-hooks"])
        .json();
    assert!(j["hooks"].as_array().unwrap().is_empty(), "{j}");
    assert!(!l2.proj.join(".claude").exists());
}

#[test]
fn setup_initialises_pgboxdb_when_needed_and_creates_the_default_source() {
    let l = lab(false); // never initialised
    let r = l.run(&l.proj, &[], &["setup", "--hosts", "claude"]);
    assert_eq!(r.code, 0, "{}", r.all());
    let j = r.json();
    assert_eq!(j["runtime"]["initialized"], true, "{j}");
    assert_eq!(j["source"]["name"], "app", "{j}");
    assert_eq!(j["source"]["created"], true, "{j}");
    assert!(l.p.run(&["status"]).code == 0);
    // Running again changes nothing and reports it.
    let j = l.run(&l.proj, &[], &["setup", "--hosts", "claude"]).json();
    assert_eq!(j["runtime"]["initialized"], false, "{j}");
    assert_eq!(j["toml"]["written"], false, "{j}");
    assert_eq!(j["hooks"][0]["added"], 0, "{j}");
    l.p.stop_ok();
}

#[test]
fn doctor_reports_hooks_source_and_sessions() {
    let l = lab(true);
    // Before setup: hooks missing, no .pgboxdb.toml.
    let r = l.run(&l.proj, &[], &["doctor"]);
    let j = r.json();
    let by = |name: &str, j: &Value| {
        j["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("no check {name}: {j}"))
    };
    assert_eq!(by("config", &j)["status"], "warn", "{j}");
    assert_eq!(by("hooks:claude", &j)["status"], "warn", "{j}");
    assert_eq!(by("runtime", &j)["status"], "ok", "{j}");
    l.run(
        &l.proj,
        &[],
        &["setup", "--source", "myapp", "--hosts", "claude"],
    );
    let j = l.run(&l.proj, &[], &["doctor"]).json();
    assert_eq!(j["ok"], true, "{j}");
    assert_eq!(by("config", &j)["status"], "ok", "{j}");
    assert_eq!(by("source", &j)["status"], "ok", "{j}");
    assert!(
        by("source", &j)["detail"]
            .as_str()
            .unwrap()
            .contains("myapp"),
        "{j}"
    );
    assert_eq!(by("hooks:claude", &j)["status"], "ok", "{j}");
    // Inside a session: the session check knows the branch and whether DATABASE_URL names it.
    let sid = "9b1f3c2d-aaaa-bbbb-cccc-dddddddddddd";
    let up = l
        .run(&l.proj, &[("CLAUDE_CODE_SESSION_ID", sid)], &["up"])
        .json();
    let url = up["url"].as_str().unwrap();
    let j = l
        .run(
            &l.proj,
            &[("CLAUDE_CODE_SESSION_ID", sid), ("DATABASE_URL", url)],
            &["doctor"],
        )
        .json();
    assert_eq!(by("session", &j)["status"], "ok", "{j}");
    let j = l
        .run(
            &l.proj,
            &[
                ("CLAUDE_CODE_SESSION_ID", sid),
                ("DATABASE_URL", "postgres://x@localhost/dev"),
            ],
            &["doctor"],
        )
        .json();
    assert_eq!(by("session", &j)["status"], "fail", "{j}");
    assert_eq!(j["ok"], false);
    // A busy source is a failure with the idle hint.
    let mut hold = std::process::Command::new(common::find_psql());
    hold.args([
        "-X",
        "-h",
        l.p.sock_dir().to_str().unwrap(),
        "-p",
        &l.p.port.to_string(),
        "-U",
        "postgres",
        "-d",
        "myapp",
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let mut child = common::spawn_locked(&mut hold).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let j = l.run(&l.proj, &[], &["doctor"]).json();
    assert_eq!(by("source", &j)["status"], "fail", "{j}");
    assert!(
        by("source", &j)["hint"].as_str().unwrap().contains("idle"),
        "{j}"
    );
    let _ = child.kill();
    let _ = child.wait();
    // Duplicate entries are flagged.
    let path = l.proj.join(".claude/settings.json");
    let mut s: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let dup = s["hooks"]["SessionStart"][0].clone();
    s["hooks"]["SessionStart"].as_array_mut().unwrap().push(dup);
    std::fs::write(&path, s.to_string()).unwrap();
    let j = l.run(&l.proj, &[], &["doctor"]).json();
    assert_eq!(by("hooks:claude", &j)["status"], "warn", "{j}");
    assert!(
        by("hooks:claude", &j)["detail"]
            .as_str()
            .unwrap()
            .contains("duplicate"),
        "{j}"
    );
}

#[test]
fn setup_refuses_a_committed_source_that_does_not_exist() {
    // A teammate's clone carries .pgboxdb.toml naming a template this machine
    // has not imported yet: setup must say so, not create an empty database
    // under that name and let every session branch from nothing.
    let l = lab(true);
    std::fs::write(
        l.proj.join(".pgboxdb.toml"),
        "[agent]\nsource = \"team_template\"\n",
    )
    .unwrap();
    let r = l.run(&l.proj, &[], &["setup", "--hosts", "claude"]);
    assert_eq!(r.code, 5, "{}", r.all());
    assert!(
        r.json()["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("pgboxdb copy"),
        "{}",
        r.all()
    );
    assert_eq!(
        l.p.psql(
            "postgres",
            "SELECT count(*) FROM pg_database WHERE datname='team_template'"
        )
        .stdout
        .trim(),
        "0"
    );
    assert!(
        !l.proj.join(".claude").exists(),
        "no hooks installed on failure"
    );
}

#[test]
fn uninstall_matches_commands_only_and_keeps_mixed_groups() {
    // A foreign entry that merely mentions "pgboxdb agent hook" in a message must
    // survive; a group holding a foreign hook and ours keeps the foreign one.
    let l = lab(true);
    std::fs::create_dir_all(l.proj.join(".claude")).unwrap();
    std::fs::write(
        l.proj.join(".claude/settings.json"),
        json!({"hooks": {
            "SessionStart": [{"hooks": [{"type": "command", "command": "orca-hook", "statusMessage": "runs before pgboxdb agent hook"}]}],
            "SessionEnd": [{"hooks": [{"type": "command", "command": "orca-end"}]}]
        }}).to_string(),
    ).unwrap();
    l.run(&l.proj, &[], &["install", "claude"]);
    // Merge our hook into the foreign SessionEnd group by hand (what a user might do).
    let path = l.proj.join(".claude/settings.json");
    let mut s: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let ours = s["hooks"]["SessionEnd"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e.to_string().contains(MARKER))
        .unwrap()["hooks"][0]
        .clone();
    s["hooks"]["SessionEnd"]
        .as_array_mut()
        .unwrap()
        .retain(|e| !e.to_string().contains(MARKER));
    s["hooks"]["SessionEnd"][0]["hooks"]
        .as_array_mut()
        .unwrap()
        .push(ours);
    std::fs::write(&path, s.to_string()).unwrap();
    let j = l.run(&l.proj, &[], &["uninstall", "claude"]).json();
    assert_eq!(j["hosts"][0]["removed"], 6, "{j}");
    let s = l.read(".claude/settings.json");
    assert_eq!(
        s["hooks"]["SessionStart"].as_array().unwrap().len(),
        1,
        "{s}"
    );
    assert_eq!(
        s["hooks"]["SessionStart"][0]["hooks"][0]["command"], "orca-hook",
        "foreign entry mentioning the marker survives: {s}"
    );
    assert_eq!(
        s["hooks"]["SessionEnd"][0]["hooks"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "{s}"
    );
    assert_eq!(
        s["hooks"]["SessionEnd"][0]["hooks"][0]["command"], "orca-end",
        "{s}"
    );
    assert!(!s.to_string().contains("exec pgboxdb agent hook"), "{s}");
}

#[test]
fn install_keeps_the_users_key_order_and_doctor_prefers_pgboxdb_source() {
    let l = lab(true);
    std::fs::create_dir_all(l.proj.join(".claude")).unwrap();
    std::fs::write(l.proj.join(".claude/settings.json"), "{\n  \"permissions\": {\"allow\": [\"Bash(ls:*)\"]},\n  \"env\": {\"ZED\": \"1\", \"ALPHA\": \"2\"},\n  \"hooks\": {}\n}\n").unwrap();
    l.run(&l.proj, &[], &["install", "claude", "--source", "myapp"]);
    let text = std::fs::read_to_string(l.proj.join(".claude/settings.json")).unwrap();
    let perm = text.find("\"permissions\"").unwrap();
    let env = text.find("\"env\"").unwrap();
    let zed = text.find("\"ZED\"").unwrap();
    let alpha = text.find("\"ALPHA\"").unwrap();
    assert!(perm < env && zed < alpha, "key order preserved: {text}");
    // doctor resolves the source like the hook does: PGBOXDB_SOURCE beats .pgboxdb.toml.
    assert_eq!(l.p.run(&["create", "other"]).code, 0);
    let j = l
        .run(&l.proj, &[("PGBOXDB_SOURCE", "other")], &["doctor"])
        .json();
    let src = j["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "source")
        .unwrap();
    assert!(
        src["detail"].as_str().unwrap().starts_with("other"),
        "{src}"
    );
}
