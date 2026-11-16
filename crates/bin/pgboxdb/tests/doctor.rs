mod common;
use common::PGBoxDB;

fn checks(j: &serde_json::Value) -> Vec<(String, String, String)> {
    j["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap().to_string(),
                c["status"].as_str().unwrap().to_string(),
                c["detail"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

fn status_of(j: &serde_json::Value, name: &str) -> (String, String) {
    checks(j)
        .into_iter()
        .find(|(n, _, _)| n == name)
        .map(|(_, s, d)| (s, d))
        .unwrap_or_else(|| panic!("no check named {name} in {j}"))
}

#[test]
fn doctor_ok_on_fresh_init() {
    let p = PGBoxDB::new();
    p.init_ok();
    let r = p.run(&["--json", "doctor"]);
    assert_eq!(r.code, 0, "{}", r.all());
    let j = r.json();
    for name in [
        "install",
        "dist",
        "engine",
        "data_dir",
        "config",
        "socket_path",
        "runtime",
        "port",
        "disk_free",
        "stack_limit",
        "filesystem",
    ] {
        let (s, d) = status_of(&j, name);
        assert!(s == "ok" || s == "warn", "{name}: {s} {d}");
    }
    assert_eq!(status_of(&j, "runtime").0, "ok");
    assert!(status_of(&j, "runtime").1.contains("not running"));
    assert_eq!(status_of(&j, "port").0, "ok");
    assert_eq!(j["ok"], true);
    let text = p.run(&["doctor"]);
    assert_eq!(text.code, 0);
    assert!(
        text.stdout.contains("ok") && text.stdout.contains("install"),
        "{}",
        text.stdout
    );
}

#[test]
fn doctor_reports_missing_share_dir_as_fail() {
    let p = PGBoxDB::new();
    std::fs::remove_file(p.prefix.join("share")).unwrap();
    std::fs::create_dir_all(p.prefix.join("share/timezonesets")).unwrap();
    let r = p.run(&["--json", "doctor"]);
    assert_eq!(r.code, 1, "{}", r.all());
    let j = r.json();
    let (s, d) = status_of(&j, "install");
    assert_eq!(s, "fail");
    assert!(
        d.contains("share/timezone") && d.contains("base-cluster"),
        "{d}"
    );
    assert_eq!(j["ok"], false);
}

#[test]
fn doctor_before_init_points_at_init() {
    let p = PGBoxDB::new();
    let r = p.run(&["--json", "doctor"]);
    assert_eq!(r.code, 1, "{}", r.all());
    let j = r.json();
    let (s, _) = status_of(&j, "data_dir");
    assert_eq!(s, "fail");
    let hint = j["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "data_dir")
        .unwrap()["hint"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(hint.contains("pgboxdb init"), "{hint}");
}

#[test]
fn doctor_detects_port_conflict_and_stale_pid() {
    let p = PGBoxDB::new();
    p.init_ok();
    let busy = std::net::TcpListener::bind(("127.0.0.1", p.port)).unwrap();
    let j = p.run(&["--json", "doctor"]).json();
    assert_eq!(status_of(&j, "port").0, "fail", "{j}");
    assert!(status_of(&j, "port").1.contains("in use"), "{j}");
    drop(busy);
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
    let j = p.run(&["--json", "doctor"]).json();
    let (s, d) = status_of(&j, "runtime");
    assert_eq!(s, "warn", "{j}");
    assert!(d.contains("stale"), "{d}");
}

#[test]
fn doctor_running_reports_libraries_and_pid() {
    let p = PGBoxDB::new();
    p.init_ok();
    p.start_ok();
    let r = p.run(&["--json", "doctor"]);
    assert_eq!(r.code, 0, "{}", r.all());
    let j = r.json();
    let (s, d) = status_of(&j, "runtime");
    assert_eq!(s, "ok");
    assert!(d.contains(&p.engine_pids()[0].to_string()), "{d}");
    assert_eq!(status_of(&j, "port").0, "ok");
    assert!(status_of(&j, "port").1.contains("PGBoxDB"), "{j}");
    let (s, d) = status_of(&j, "libxml2");
    assert!(s == "ok" || s == "warn", "{s} {d}");
    let (s, _) = status_of(&j, "icu");
    assert!(s == "ok" || s == "warn");
    assert!(status_of(&j, "filesystem").1.contains("clon"), "{j}");
}

#[test]
fn doctor_warns_on_loose_config_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let p = PGBoxDB::new();
    p.init_ok();
    std::fs::set_permissions(
        p.home.join("config.json"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let j = p.run(&["--json", "doctor"]).json();
    let (s, d) = status_of(&j, "config");
    assert_eq!(s, "warn", "{d}");
    assert!(d.contains("644"), "{d}");
}

#[test]
fn doctor_reports_old_orphan_dirs_but_not_fresh_ones() {
    let p = PGBoxDB::new();
    p.init_ok();
    p.start_ok();
    let base = p.home.join("data/base");
    std::fs::create_dir(base.join("777777")).unwrap();
    std::fs::write(base.join("777777/1259"), vec![0u8; 8192]).unwrap();
    // age the orphan: touch -t two days ago
    let st = std::process::Command::new("touch")
        .args(["-t", "202601010000", &base.join("777777").to_string_lossy()])
        .status()
        .unwrap();
    assert!(st.success());
    std::fs::create_dir(base.join("888888")).unwrap(); // fresh: may be an in-flight CREATE DATABASE
    let j = p.run(&["--json", "doctor"]).json();
    let (s, d) = status_of(&j, "orphan_dirs");
    assert_eq!(s, "warn", "{d}");
    assert!(d.contains("777777") && !d.contains("888888"), "{d}");
    assert!(base.join("777777").is_dir(), "doctor must never delete");
    let hint = j["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "orphan_dirs")
        .unwrap()["hint"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(hint.contains("never deletes"), "{hint}");
    std::fs::remove_dir_all(base.join("777777")).unwrap();
    std::fs::remove_dir_all(base.join("888888")).unwrap();
    let j = p.run(&["--json", "doctor"]).json();
    assert_eq!(status_of(&j, "orphan_dirs").0, "ok");
}
