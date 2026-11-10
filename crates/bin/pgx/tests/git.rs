//! Black-box tests for PGX Git: real Git repositories (temporary), real
//! hooks fired by real `git switch`, a real runtime, real clones. Latencies
//! printed here are measured, never assumed (run with `--nocapture`).
mod common;
use common::{find_psql, Pgx, Run};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const AGENT_VARS: [&str; 9] = [
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDECODE",
    "CODEX_SESSION_ID",
    "PGX_AGENT_SESSION",
    "PGX_AGENT_ID",
    "PGX_ROUTED",
    "DATABASE_URL",
    "PGDATABASE",
    "PGX_GIT_SKIP",
];

struct Lab {
    p: Pgx,
    root: PathBuf,
}

fn lab() -> Lab {
    let p = Pgx::new();
    p.init_ok();
    p.start_ok();
    let root = p.home.parent().unwrap().join("w");
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    Lab { p, root }
}

impl Lab {
    fn scrub(&self, c: &mut Command) {
        for v in AGENT_VARS {
            c.env_remove(v);
        }
        c.env("PGX_HOME", &self.p.home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env("HOME", self.p.home.parent().unwrap())
            .env("PGX_PG_BINDIR", common::pg_bindir());
    }

    fn git(&self, dir: &Path, args: &[&str]) -> Run {
        let mut c = Command::new("git");
        c.args(args).current_dir(dir);
        self.scrub(&mut c);
        let r = Run::from(common::output_locked(&mut c).unwrap());
        assert_eq!(r.code, 0, "git {args:?}: {}", r.all());
        r
    }

    fn pgx(&self, dir: &Path, args: &[&str]) -> Run {
        self.pgx_env(dir, &[], args)
    }

    fn pgx_env(&self, dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Run {
        self.p.output_retrying_pub(
            || {
                let mut c = self.p.cmd(args);
                c.current_dir(dir);
                self.scrub(&mut c);
                for (k, v) in env {
                    c.env(k, v);
                }
                c
            },
            "pgx",
        )
    }

    fn json(&self, dir: &Path, args: &[&str]) -> serde_json::Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        let r = self.pgx(dir, &a);
        assert_eq!(r.code, 0, "pgx {args:?}: {}", r.all());
        r.json()
    }

    /// `pgx git exec -- psql -c <sql>` in `dir`: stdout, trimmed.
    fn q(&self, dir: &Path, sql: &str) -> String {
        let psql = find_psql();
        let r = self.pgx(
            dir,
            &[
                "git",
                "exec",
                "--",
                psql.to_str().unwrap(),
                "-X",
                "-qAt",
                "-v",
                "ON_ERROR_STOP=1",
                "-c",
                sql,
            ],
        );
        assert_eq!(r.code, 0, "{sql}: {}", r.all());
        r.stdout.trim().to_string()
    }

    fn repo(&self, name: &str) -> PathBuf {
        let d = self.root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        self.git(&d, &["init", "-q", "-b", "main"]);
        std::fs::write(d.join("README"), "x\n").unwrap();
        self.git(&d, &["add", "README"]);
        self.git(&d, &["commit", "-q", "-m", "init"]);
        d
    }

    fn init(&self, dir: &Path, extra: &[&str]) -> serde_json::Value {
        let mut a = vec!["git", "init"];
        a.extend_from_slice(extra);
        self.json(dir, &a)
    }

    fn db_exists(&self, name: &str) -> bool {
        self.p
            .psql(
                "postgres",
                &format!("SELECT count(*) FROM pg_database WHERE datname = '{name}'"),
            )
            .stdout
            .trim()
            == "1"
    }

    fn databases(&self) -> Vec<String> {
        self.p
            .psql("postgres", "SELECT datname FROM pg_database ORDER BY 1")
            .stdout
            .lines()
            .map(String::from)
            .collect()
    }
}

fn current_db(l: &Lab, dir: &Path) -> String {
    l.q(dir, "SELECT current_database()")
}

// ---------------------------------------------------------------- A

#[test]
fn a_init_new_existing_and_repeated_projects() {
    let l = lab();
    // No Git repository: a clear error, nothing created.
    let bare = l.root.join("not-a-repo");
    std::fs::create_dir_all(&bare).unwrap();
    let r = l.pgx(&bare, &["git", "init"]);
    assert_eq!(r.code, 5, "{}", r.all());
    assert!(
        r.stderr.contains("not inside a Git repository"),
        "{}",
        r.all()
    );

    // New project.
    let t0 = Instant::now();
    let d = l.repo("newapp");
    let v = l.init(&d, &[]);
    println!(
        "init (new project, empty main): {} ms",
        t0.elapsed().as_millis()
    );
    assert_eq!(v["ready"], true);
    assert_eq!(v["main_branch"], "main");
    assert_eq!(v["snapshot"], 1);
    let main_db = v["main_database"].as_str().unwrap().to_string();
    assert!(main_db.starts_with("pgx_newapp_main_"), "{main_db}");
    let toml = std::fs::read_to_string(d.join(".pgx.toml")).unwrap();
    assert!(
        toml.contains("[git]") && toml.contains("main_branch = \"main\""),
        "{toml}"
    );
    assert!(
        !toml.contains("://") && !toml.contains(&l.p.password()),
        "no secrets in committed config"
    );
    for h in ["post-checkout", "post-merge", "post-rewrite"] {
        assert!(d.join(".git/hooks").join(h).is_file(), "{h}");
    }

    // Repeated: same databases, same generation, nothing overwritten.
    l.q(
        &d,
        "CREATE TABLE keepme(x int); INSERT INTO keepme VALUES (42)",
    );
    let again = l.init(&d, &[]);
    assert_eq!(again["main_database"], main_db.as_str());
    assert_eq!(again["main_database_how"], "kept");
    assert_eq!(l.q(&d, "SELECT x FROM keepme"), "42");
    assert_eq!(again["config_written"], false);

    // Existing PGX project: .pgx.toml with [agent] and an existing template.
    let d2 = l.repo("oldapp");
    assert_eq!(l.p.run(&["create", "oldapp_template"]).code, 0);
    assert_eq!(
        l.p.psql(
            "oldapp_template",
            "CREATE TABLE posts(id int); INSERT INTO posts VALUES (1),(2)"
        )
        .code,
        0
    );
    std::fs::write(
        d2.join(".pgx.toml"),
        "# mine\n[agent]\nsource = \"oldapp_template\"\nidle_ttl = \"3h\"\n",
    )
    .unwrap();
    let v2 = l.init(&d2, &[]);
    assert_eq!(v2["main_database_how"], "cloned");
    assert_eq!(l.q(&d2, "SELECT count(*) FROM posts"), "2");
    let t = std::fs::read_to_string(d2.join(".pgx.toml")).unwrap();
    assert!(
        t.starts_with("# mine\n[agent]\nsource = \"oldapp_template\"\nidle_ttl = \"3h\"\n"),
        "{t}"
    );
    assert!(t.contains("[git]"));
    // The template itself was only read.
    assert_eq!(
        l.p.psql("oldapp_template", "SELECT count(*) FROM posts")
            .stdout
            .trim(),
        "2"
    );

    // --source <template> on a third repo, and --main for a master repo.
    let d3 = l.root.join("legacy");
    std::fs::create_dir_all(&d3).unwrap();
    l.git(&d3, &["init", "-q", "-b", "master"]);
    std::fs::write(d3.join("f"), "1").unwrap();
    l.git(&d3, &["add", "f"]);
    l.git(&d3, &["commit", "-q", "-m", "c"]);
    let v3 = l.init(&d3, &["--source", "oldapp_template"]);
    assert_eq!(v3["main_branch"], "master", "{v3}");
    assert_eq!(l.q(&d3, "SELECT count(*) FROM posts"), "2");
    // A missing named source is an error, not an empty database.
    let d4 = l.repo("missing");
    let r = l.pgx(&d4, &["git", "init", "--source", "no_such_db"]);
    assert_eq!(r.code, 5, "{}", r.all());
}

#[test]
fn a2_init_imports_external_postgres_schema_only_by_default() {
    let l = lab();
    let src = common::PgSource::start();
    src.create_db("prod_like");
    assert_eq!(
        src.psql("prod_like", "CREATE TABLE customers(id int, email text); INSERT INTO customers VALUES (1, 'secret@example.com'); CREATE TABLE schema_migrations(version varchar primary key); INSERT INTO schema_migrations VALUES ('20240101000000')").code,
        0
    );
    let before = src.fingerprint("prod_like");
    let d = l.repo("imported");
    let url = src.url("prod_like");
    let v = l.init(&d, &["--source", &url]);
    assert_eq!(v["main_database_how"], "imported (schema only)", "{v}");
    assert_eq!(
        l.q(&d, "SELECT count(*) FROM customers"),
        "0",
        "no rows copied by default"
    );
    assert_eq!(
        l.q(&d, "SELECT version FROM schema_migrations"),
        "20240101000000",
        "bookkeeping kept"
    );
    assert_eq!(
        src.fingerprint("prod_like"),
        before,
        "the source was only read"
    );
    let toml = std::fs::read_to_string(d.join(".pgx.toml")).unwrap();
    assert!(
        !toml.contains("postgres://") && !toml.contains(&url),
        "{toml}"
    );
}

// ---------------------------------------------------------------- B C D

#[test]
fn bcd_branches_get_isolated_databases_switching_reuses_them() {
    let l = lab();
    let d = l.repo("myapp");
    l.init(&d, &[]);
    let main_db = current_db(&l, &d);
    l.q(&d, "CREATE TABLE users(id serial primary key, email text)");
    assert_eq!(l.pgx(&d, &["git", "sync"]).code, 0);

    let t = Instant::now();
    let out = l.git(&d, &["switch", "-c", "feature/auth"]);
    let switch_create_ms = t.elapsed().as_millis();
    assert!(
        out.stderr.contains("✓ PGX database ready: feature/auth"),
        "{}",
        out.all()
    );
    assert!(
        out.stderr.contains("Cloned from main (v2)"),
        "{}",
        out.all()
    );
    let auth = current_db(&l, &d);
    assert_ne!(auth, main_db);
    assert!(auth.starts_with("pgx_myapp_auth_"), "{auth}");
    // D: the branch's own change.
    l.q(
        &d,
        "CREATE TABLE sessions(id int); INSERT INTO users(email) VALUES ('a@x')",
    );

    l.git(&d, &["switch", "-c", "feature/billing"]);
    let billing = current_db(&l, &d);
    assert!(billing.starts_with("pgx_myapp_billing_"), "{billing}");
    assert_ne!(billing, auth);
    assert_eq!(
        l.q(&d, "SELECT to_regclass('sessions') IS NULL"),
        "t",
        "isolated from feature/auth"
    );

    let t = Instant::now();
    let out = l.git(&d, &["switch", "main"]);
    let switch_existing_ms = t.elapsed().as_millis();
    assert!(
        out.stderr.contains("✓ PGX database: main →"),
        "{}",
        out.all()
    );
    assert!(
        out.stderr.contains("environment is unchanged"),
        "never claims to change the parent shell: {}",
        out.all()
    );
    assert_eq!(current_db(&l, &d), main_db);
    assert_eq!(l.q(&d, "SELECT to_regclass('sessions') IS NULL"), "t");
    assert_eq!(l.q(&d, "SELECT count(*) FROM users"), "0");

    l.git(&d, &["switch", "feature/auth"]);
    assert_eq!(current_db(&l, &d), auth, "the same database again");
    assert_eq!(
        l.q(&d, "SELECT to_regclass('sessions') IS NOT NULL"),
        "t",
        "persisted"
    );
    assert_eq!(l.q(&d, "SELECT count(*) FROM users"), "1");
    l.git(&d, &["switch", "feature/billing"]);
    assert_eq!(current_db(&l, &d), billing);

    let ls = l.json(&d, &["git", "ls"]);
    let n = ls["branches"].as_array().unwrap().len();
    assert_eq!(n, 3, "{ls}");
    let created_ms: Vec<f64> = ls["branches"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["role"] == "branch")
        .map(|b| b["elapsed_ms"].as_f64().unwrap())
        .collect();
    println!(
        "branch databases: {n}; clone ms {created_ms:?}; `git switch -c` with hook {switch_create_ms} ms; `git switch` to an existing branch {switch_existing_ms} ms"
    );
}

// ---------------------------------------------------------------- E

#[test]
fn e_main_sync_feeds_new_branches_and_preserves_existing_ones() {
    let l = lab();
    let d = l.repo("app");
    l.init(&d, &[]);
    l.q(&d, "CREATE TABLE users(id int)");
    let s = l.json(&d, &["git", "sync"]);
    assert_eq!(s["status"], "published", "{s}");
    assert_eq!(s["generation"], 2);
    println!("sync (clone of idle main): {} ms", s["elapsed_ms"]);
    // Unchanged main: no new snapshot.
    let s = l.json(&d, &["git", "sync"]);
    assert_eq!(s["status"], "unchanged", "{s}");
    assert_eq!(s["generation"], 2);

    l.git(&d, &["switch", "-c", "feature/auth"]);
    l.q(
        &d,
        "ALTER TABLE users ADD COLUMN token text; INSERT INTO users VALUES (1, 'mine')",
    );
    l.git(&d, &["switch", "-c", "feature/search"]);
    l.q(&d, "INSERT INTO users VALUES (7)");

    l.git(&d, &["switch", "main"]);
    l.q(&d, "ALTER TABLE users ADD COLUMN email text");
    let st = l.json(&d, &["git", "status"]);
    assert_eq!(st["sync"]["status"], "main_changed", "{st}");
    // Sync while on a feature branch is allowed and leaves it alone.
    l.git(&d, &["switch", "feature/search"]);
    let s = l.json(&d, &["git", "sync"]);
    assert_eq!(s["status"], "published", "{s}");
    assert_eq!(s["generation"], 3);
    println!("sync after a schema change: {} ms", s["elapsed_ms"]);
    assert_eq!(
        l.q(&d, "SELECT count(*) FROM users"),
        "1",
        "feature/search kept its data"
    );

    l.git(&d, &["switch", "-c", "feature/billing"]);
    assert_eq!(
        l.q(&d, "SELECT string_agg(column_name::text, ',' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_name = 'users'"),
        "id,email",
        "new branch has main's latest schema"
    );
    l.git(&d, &["switch", "feature/auth"]);
    assert_eq!(
        l.q(&d, "SELECT string_agg(column_name::text, ',' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_name = 'users'"),
        "id,token",
        "existing branch keeps its own schema"
    );
    assert_eq!(l.q(&d, "SELECT token FROM users"), "mine");
    let st = l.json(&d, &["git", "status"]);
    assert_eq!(st["main_snapshot"], 3);
    assert_eq!(st["branch_baseline"], 2);
    assert_eq!(st["sync"]["branch_stale"], true, "{st}");
    let ls = l.json(&d, &["git", "ls"]);
    let by: std::collections::HashMap<String, String> = ls["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            (
                b["branch"].as_str().unwrap().to_string(),
                b["status"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(by["feature/auth"], "current");
    assert_eq!(by["feature/search"], "stale");
    assert_eq!(by["feature/billing"], "ready");
    // The snapshot is immutable: nobody can connect to it.
    let snap = ls["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["generation"] == 3)
        .unwrap()
        .clone();
    assert_eq!(snap["locked"], true);
    let c = l.p.psql(snap["database"].as_str().unwrap(), "SELECT 1");
    assert_ne!(c.code, 0, "snapshot accepts no connections: {}", c.all());

    // Reset to main is explicit and refuses without --force when non-interactive.
    let r = l.pgx(&d, &["git", "reset", "--from-main"]);
    assert_eq!(r.code, 2, "{}", r.all());
    assert_eq!(l.q(&d, "SELECT token FROM users"), "mine", "untouched");
    let r = l.json(&d, &["git", "reset", "--from-main", "--force"]);
    assert_eq!(r["generation"], 3, "{r}");
    assert_eq!(
        l.q(&d, "SELECT string_agg(column_name::text, ',' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_name = 'users'"),
        "id,email"
    );
    assert_eq!(l.json(&d, &["git", "status"])["branch_baseline"], 3);
    // Never on main.
    l.git(&d, &["switch", "main"]);
    let r = l.pgx(&d, &["git", "reset", "--from-main", "--force"]);
    assert_eq!(r.code, 2, "{}", r.all());
    assert!(r.stderr.contains("protected"), "{}", r.all());
}

#[test]
fn e2_pending_migrations_are_reported_not_hidden() {
    let l = lab();
    let d = l.repo("rails");
    l.init(&d, &[]);
    l.q(&d, "CREATE TABLE schema_migrations(version varchar primary key); INSERT INTO schema_migrations VALUES ('20240101000000')");
    std::fs::create_dir_all(d.join("db/migrate")).unwrap();
    std::fs::write(
        d.join("db/migrate/20240101000000_create_users.rb"),
        "# m1\n",
    )
    .unwrap();
    l.git(&d, &["add", "db"]);
    l.git(&d, &["commit", "-q", "-m", "m1"]);
    let s = l.json(&d, &["git", "sync"]);
    assert_eq!(s["status"], "published", "{s}");
    // A newly committed (pulled) migration is not "synchronised".
    std::fs::write(d.join("db/migrate/20240202000000_add_email.rb"), "# m2\n").unwrap();
    l.git(&d, &["add", "db"]);
    l.git(&d, &["commit", "-q", "-m", "m2"]);
    let s = l.json(&d, &["git", "sync"]);
    assert_eq!(s["status"], "pending", "{s}");
    assert_eq!(s["pending_migrations"][0], "rails:20240202000000", "{s}");
    let st = l.json(&d, &["git", "status"]);
    assert_eq!(
        st["sync"]["pending_migrations"][0], "rails:20240202000000",
        "{st}"
    );
    // A configured command runs only on explicit request.
    std::fs::create_dir_all(d.join("bin")).unwrap();
    let psql = find_psql();
    std::fs::write(
        d.join("bin/migrate"),
        format!("#!/bin/sh\nexec {} -X -q -c \"INSERT INTO schema_migrations VALUES ('20240202000000'); ALTER TABLE schema_migrations ADD COLUMN IF NOT EXISTS x int\"\n", psql.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        d.join("bin/migrate"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let mut t = std::fs::read_to_string(d.join(".pgx.toml")).unwrap();
    t = t.replace(
        "# command = \"bin/rails db:migrate\"",
        "command = \"bin/migrate\"",
    );
    std::fs::write(d.join(".pgx.toml"), t).unwrap();
    let s = l.json(&d, &["git", "sync"]);
    assert_eq!(s["status"], "pending", "not run without --migrate: {s}");
    let s = l.json(&d, &["git", "sync", "--migrate"]);
    assert_eq!(s["status"], "published", "{s}");
    assert_eq!(s["migrated"], true);
    assert!(
        s["pending_migrations"].as_array().unwrap().is_empty(),
        "{s}"
    );
}

// ---------------------------------------------------------------- F

// Fault injection is compiled only with `--features failpoints`.
#[cfg(feature = "failpoints")]
#[test]
fn f_failed_sync_keeps_the_previous_snapshot_and_branches() {
    let l = lab();
    let d = l.repo("app");
    l.init(&d, &[]);
    l.q(&d, "CREATE TABLE t(x int)");
    assert_eq!(l.json(&d, &["git", "sync"])["generation"], 2);
    l.git(&d, &["switch", "-c", "feature/x"]);
    l.q(&d, "INSERT INTO t VALUES (5)");
    l.git(&d, &["switch", "main"]);
    l.q(&d, "ALTER TABLE t ADD COLUMN y int");
    for fp in ["copy", "validate", "publish"] {
        let r = l.pgx_env(&d, &[("PGX_GIT_FAILPOINT", fp)], &["--json", "git", "sync"]);
        assert_eq!(r.code, 1, "{fp}: {}", r.all());
        let v = r.json();
        assert_eq!(v["status"], "failed", "{v}");
        assert!(
            v["message"]
                .as_str()
                .unwrap()
                .contains("v2 is still the baseline"),
            "{v}"
        );
        let st = l.json(&d, &["git", "status"]);
        assert_eq!(st["main_snapshot"], 2, "{fp}: {st}");
        assert_eq!(st["sync"]["status"], "failed", "{st}");
        assert!(
            !l.databases()
                .iter()
                .any(|n| n.contains("_stg_") || n.contains("_base_v3_")),
            "{fp}: no partial snapshot left: {:?}",
            l.databases()
        );
    }
    // While snapshots keep failing, new branches still work (from v2); the
    // existing branch is unchanged.
    let mut c = Command::new("git");
    c.args(["switch", "-c", "feature/y"]).current_dir(&d);
    l.scrub(&mut c);
    c.env("PGX_GIT_FAILPOINT", "validate");
    let r = Run::from(common::output_locked(&mut c).unwrap());
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(r.stderr.contains("Cloned from main (v2)"), "{}", r.all());
    assert_eq!(
        l.q(
            &d,
            "SELECT count(*) FROM information_schema.columns WHERE table_name = 't'"
        ),
        "1"
    );
    l.git(&d, &["switch", "feature/x"]);
    assert_eq!(l.q(&d, "SELECT x FROM t"), "5");
    // And the next sync succeeds as v3.
    let s = l.json(&d, &["git", "sync"]);
    assert_eq!(s["generation"], 3, "{s}");
}

// ---------------------------------------------------------------- G

#[test]
fn g_worktrees_and_repositories_never_share_databases() {
    let l = lab();
    let d = l.repo("app");
    l.init(&d, &[]);
    l.q(&d, "CREATE TABLE t(who text)");
    l.pgx(&d, &["git", "sync"]);
    l.git(&d, &["branch", "feature/auth"]);
    l.git(&d, &["branch", "feature/billing"]);
    let wa = l.root.join("app-auth");
    let wb = l.root.join("app-billing");
    l.git(
        &d,
        &[
            "worktree",
            "add",
            "-q",
            wa.to_str().unwrap(),
            "feature/auth",
        ],
    );
    l.git(
        &d,
        &[
            "worktree",
            "add",
            "-q",
            wb.to_str().unwrap(),
            "feature/billing",
        ],
    );
    assert!(
        wa.join(".git").is_file(),
        "linked worktrees have a .git file"
    );
    let a = current_db(&l, &wa);
    let b = current_db(&l, &wb);
    let m = current_db(&l, &d);
    assert!(a != b && a != m && b != m, "{a} {b} {m}");
    l.q(&wa, "INSERT INTO t VALUES ('auth')");
    l.q(&wb, "INSERT INTO t VALUES ('billing')");
    assert_eq!(l.q(&wa, "SELECT string_agg(who, ',') FROM t"), "auth");
    assert_eq!(l.q(&wb, "SELECT string_agg(who, ',') FROM t"), "billing");
    assert_eq!(l.q(&d, "SELECT count(*) FROM t"), "0");
    // Returning to a worktree reuses its database.
    assert_eq!(current_db(&l, &wa), a);
    let st = l.json(&wa, &["git", "status"]);
    assert_eq!(st["linked_worktree"], true);
    assert_eq!(st["database"], a.as_str());
    // The same branch name in another worktree of the same repository.
    l.git(&wa, &["switch", "-c", "shared-name"]);
    let wc = l.root.join("app-c");
    l.git(
        &d,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "shared-name-2",
            wc.to_str().unwrap(),
        ],
    );
    l.git(&wc, &["switch", "--detach"]);
    l.git(&wc, &["branch", "-D", "shared-name-2"]);
    // A detached HEAD gets its own per-worktree database.
    let det = current_db(&l, &wc);
    assert!(det.contains("_detached_"), "{det}");

    // Two repositories with the same branch: different databases.
    let other = l.repo("other");
    l.init(&other, &[]);
    l.git(&other, &["switch", "-c", "feature/auth"]);
    let o = current_db(&l, &other);
    assert_ne!(o, a);
    assert!(o.starts_with("pgx_other_auth_"), "{o}");
}

// ---------------------------------------------------------------- H

#[test]
fn h_parallel_agents_on_one_branch_get_separate_databases() {
    let l = lab();
    let d = l.repo("app");
    l.init(&d, &[]);
    l.q(&d, "CREATE TABLE posts(id int)");
    l.pgx(&d, &["git", "sync"]);
    l.git(&d, &["switch", "-c", "feature/auth"]);
    l.q(
        &d,
        "CREATE TABLE auth_only(x int); INSERT INTO auth_only VALUES (1)",
    );
    let dev = current_db(&l, &d);

    // Two Claude sessions and a Codex session, concurrently, on the same branch.
    let sessions = [
        (
            "claude",
            "CLAUDE_CODE_SESSION_ID",
            "aaaa1111-0000-0000-0000-000000000001",
        ),
        (
            "claude",
            "CLAUDE_CODE_SESSION_ID",
            "bbbb2222-0000-0000-0000-000000000002",
        ),
        (
            "codex",
            "CODEX_SESSION_ID",
            "cccc3333-0000-0000-0000-000000000003",
        ),
    ];
    let t0 = Instant::now();
    let handles: Vec<_> = sessions
        .iter()
        .map(|(host, var, id)| {
            let mut c = l.p.cmd(&["--json", "agent", "up", "--host", host]);
            c.current_dir(&d);
            l.scrub(&mut c);
            c.env(var, id);
            common::spawn_locked(
                c.stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped()),
            )
            .unwrap()
        })
        .collect();
    let mut branches = Vec::new();
    for h in handles {
        let o = h.wait_with_output().unwrap();
        let r = Run::from(o);
        assert_eq!(r.code, 0, "{}", r.all());
        let v = r.json();
        assert_eq!(
            v["source"],
            dev.as_str(),
            "cloned from the Git branch database: {v}"
        );
        branches.push(v["branch"].as_str().unwrap().to_string());
    }
    println!(
        "3 concurrent agent sessions on one Git branch: {} ms total",
        t0.elapsed().as_millis()
    );
    let mut uniq = branches.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), 3, "{branches:?}");
    // A subagent of session A: its own database too.
    let sub = l.pgx_env(
        &d,
        &[("CLAUDE_CODE_SESSION_ID", sessions[0].2)],
        &[
            "--json",
            "agent",
            "up",
            "--host",
            "claude",
            "--agent",
            "agent-sub1",
        ],
    );
    assert_eq!(sub.code, 0, "{}", sub.all());
    let sub = sub.json()["branch"].as_str().unwrap().to_string();
    assert!(!branches.contains(&sub) && sub != dev);
    // Each sees the branch's table; writes stay private.
    for b in branches.iter().chain(std::iter::once(&sub)) {
        assert_eq!(
            l.p.psql(b, "SELECT count(*) FROM auth_only").stdout.trim(),
            "1",
            "{b}"
        );
    }
    assert_eq!(
        l.p.psql(&branches[0], "INSERT INTO auth_only VALUES (2)")
            .code,
        0
    );
    assert_eq!(
        l.p.psql(&branches[1], "SELECT count(*) FROM auth_only")
            .stdout
            .trim(),
        "1"
    );
    assert_eq!(
        l.q(&d, "SELECT count(*) FROM auth_only"),
        "1",
        "the developer's database is untouched"
    );
    // Inside a session, `pgx git exec` uses the session's own database.
    let psql = find_psql();
    let r = l.pgx_env(
        &d,
        &[("CLAUDE_CODE_SESSION_ID", sessions[1].2)],
        &[
            "git",
            "exec",
            "--",
            psql.to_str().unwrap(),
            "-X",
            "-qAtc",
            "SELECT current_database()",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(r.stdout.trim(), branches[1], "{}", r.all());
    // A busy branch database (a dev server connected) still branches: live copy.
    let mut hc = l.p.cmd(&[
        "git",
        "exec",
        "--",
        psql.to_str().unwrap(),
        "-X",
        "-qc",
        "SELECT pg_sleep(20)",
    ]);
    hc.current_dir(&d);
    l.scrub(&mut hc);
    let mut child = common::spawn_locked(
        hc.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let r = l.pgx_env(
        &d,
        &[("CLAUDE_CODE_SESSION_ID", "dddd4444-0000")],
        &["--json", "agent", "up", "--host", "claude"],
    );
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(r.code, 0, "{}", r.all());
    let v = r.json();
    assert_eq!(
        v["method"], "dump",
        "a busy source is copied logically, never terminated: {v}"
    );
    assert_eq!(
        l.p.psql(
            v["branch"].as_str().unwrap(),
            "SELECT count(*) FROM auth_only"
        )
        .stdout
        .trim(),
        "1"
    );

    // Switching Git branches gives the session a database for the new branch.
    l.git(&d, &["switch", "main"]);
    let m = l.pgx_env(
        &d,
        &[("CLAUDE_CODE_SESSION_ID", sessions[0].2)],
        &["--json", "agent", "up", "--host", "claude"],
    );
    assert_eq!(m.code, 0, "{}", m.all());
    let m = m.json();
    assert_ne!(m["branch"], branches[0].as_str());
    assert_eq!(
        l.p.psql(
            m["branch"].as_str().unwrap(),
            "SELECT to_regclass('auth_only') IS NULL"
        )
        .stdout
        .trim(),
        "t"
    );
    // Ending the session ends its branches for every Git branch.
    let down = l.pgx_env(
        &d,
        &[("CLAUDE_CODE_SESSION_ID", sessions[0].2)],
        &["agent", "down", "--host", "claude", "--now"],
    );
    assert_eq!(down.code, 0, "{}", down.all());
    assert!(!l.db_exists(m["branch"].as_str().unwrap()));
    assert!(
        !l.db_exists(&branches[0]),
        "the feature/auth branch of the session too"
    );
    assert!(l.db_exists(&branches[1]), "other sessions untouched");

    // Without PGX Git, agents keep the old behaviour.
    let plain = l.root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(l.p.run(&["create", "app"]).code, 0);
    let r = l.pgx_env(
        &plain,
        &[],
        &["--json", "agent", "up", "--id", "zzz", "--host", "exec"],
    );
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(r.json()["source"], "app");
}

// ---------------------------------------------------------------- I

#[test]
fn i_existing_hooks_keep_working_and_uninstall_removes_only_ours() {
    let l = lab();
    let d = l.repo("app");
    let hooks = d.join(".git/hooks");
    let original = "#!/bin/sh\n# team hook\necho \"$1 $2 $3\" >> \"$(git rev-parse --git-dir)/checkout.log\"\nexit 0\n";
    std::fs::write(hooks.join("post-checkout"), original).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        hooks.join("post-checkout"),
        std::fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    let rewrite = "#!/bin/sh\nwhile read old new; do echo \"$old\" >> \"$(git rev-parse --git-dir)/rewrite.log\"; done\n";
    std::fs::write(hooks.join("post-rewrite"), rewrite).unwrap();
    std::fs::set_permissions(
        hooks.join("post-rewrite"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    let v = l.init(&d, &[]);
    let actions: Vec<String> = v["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            format!(
                "{}={}",
                h["name"].as_str().unwrap(),
                h["action"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(
        actions,
        vec![
            "post-checkout=inserted",
            "post-merge=created",
            "post-rewrite=inserted"
        ]
    );
    let mode = std::fs::metadata(hooks.join("post-checkout"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o750, "permissions preserved");
    // Rerun: idempotent.
    let v2 = l.init(&d, &[]);
    assert!(
        v2["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["action"] == "present"),
        "{v2}"
    );
    let text = std::fs::read_to_string(hooks.join("post-checkout")).unwrap();
    assert_eq!(text.matches(">>> pgx git hook >>>").count(), 1);

    l.git(&d, &["switch", "-c", "feature/x"]);
    let log = std::fs::read_to_string(d.join(".git/checkout.log")).unwrap();
    assert!(
        log.trim_end().ends_with(" 1"),
        "the team hook still ran: {log}"
    );
    assert!(current_db(&l, &d).starts_with("pgx_app_x_"));
    // post-rewrite keeps its stdin (our block reads nothing).
    std::fs::write(d.join("f"), "1").unwrap();
    l.git(&d, &["add", "f"]);
    l.git(&d, &["commit", "-q", "-m", "a"]);
    l.git(&d, &["commit", "-q", "--amend", "-m", "b"]);
    assert!(
        std::fs::read_to_string(d.join(".git/rewrite.log"))
            .unwrap()
            .len()
            >= 40,
        "the rewrite hook got its stdin"
    );

    // A broken PGX never blocks Git.
    let r = {
        let mut c = Command::new("git");
        c.args(["switch", "-c", "feature/y"]).current_dir(&d);
        l.scrub(&mut c);
        c.env("PGX_HOME", l.root.join("no-such-home"));
        Run::from(common::output_locked(&mut c).unwrap())
    };
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(r.stderr.contains("not initialised"), "{}", r.all());

    let u = l.json(&d, &["git", "uninstall"]);
    assert_eq!(u["databases_kept"], true);
    assert_eq!(
        std::fs::read_to_string(hooks.join("post-checkout")).unwrap(),
        original,
        "byte-identical"
    );
    assert_eq!(
        std::fs::read_to_string(hooks.join("post-rewrite")).unwrap(),
        rewrite
    );
    assert!(!hooks.join("post-merge").exists(), "our own file removed");
    assert_eq!(
        std::fs::metadata(hooks.join("post-checkout"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert!(
        std::fs::read_to_string(d.join(".pgx.toml"))
            .unwrap()
            .contains("[git]"),
        "shared config kept"
    );
    assert!(l.db_exists(
        l.json(&d, &["git", "ls"])["branches"][0]["database"]
            .as_str()
            .unwrap()
    ));

    // core.hooksPath: hooks go where Git looks; a committed hooks dir is left alone.
    let d2 = l.repo("husky");
    std::fs::create_dir_all(d2.join(".husky")).unwrap();
    std::fs::write(d2.join(".husky/post-checkout"), "#!/bin/sh\necho team\n").unwrap();
    l.git(&d2, &["add", ".husky"]);
    l.git(&d2, &["commit", "-q", "-m", "husky"]);
    l.git(&d2, &["config", "core.hooksPath", ".husky"]);
    let v = l.init(&d2, &[]);
    let by: std::collections::HashMap<String, String> = v["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["name"].as_str().unwrap().to_string(),
                h["action"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(by["post-checkout"], "skipped", "{v}");
    assert_eq!(by["post-merge"], "created", "{v}");
    assert!(d2.join(".husky/post-merge").is_file());
    assert_eq!(
        std::fs::read_to_string(d2.join(".husky/post-checkout")).unwrap(),
        "#!/bin/sh\necho team\n"
    );
}

// ---------------------------------------------------------------- J

#[test]
fn j_prune_finds_only_abandoned_git_databases() {
    let l = lab();
    let d = l.repo("app");
    l.init(&d, &[]);
    l.git(&d, &["switch", "-c", "feature/gone"]);
    let gone = current_db(&l, &d);
    l.git(&d, &["switch", "-c", "feature/kept"]);
    let kept = current_db(&l, &d);
    let wt = l.root.join("app-wt");
    l.git(
        &d,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature/wt",
            wt.to_str().unwrap(),
        ],
    );
    let wtdb = current_db(&l, &wt);
    l.git(&d, &["switch", "main"]);
    let main_db = current_db(&l, &d);
    l.git(&d, &["branch", "-D", "feature/gone"]);
    l.git(&d, &["worktree", "remove", wt.to_str().unwrap()]);
    // Unrelated databases that must never be touched.
    assert_eq!(l.p.run(&["create", "pgx_app_manual"]).code, 0);
    let agent = l.pgx_env(
        &d,
        &[("CLAUDE_CODE_SESSION_ID", "eeee5555-0000")],
        &["--json", "agent", "up", "--host", "claude"],
    );
    assert_eq!(agent.code, 0, "{}", agent.all());
    let agent_db = agent.json()["branch"].as_str().unwrap().to_string();

    // Within the 7d grace period: nothing is due.
    let p = l.json(&d, &["git", "prune"]);
    assert!(
        p["actions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["action"] == "keep"),
        "{p}"
    );
    std::thread::sleep(Duration::from_millis(1100));
    let p = l.json(&d, &["git", "prune", "--grace", "1s"]);
    assert_eq!(p["applied"], false);
    let drops: Vec<String> = p["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["action"] == "drop")
        .map(|a| a["database"].as_str().unwrap().to_string())
        .collect();
    assert!(drops.contains(&gone) && drops.contains(&wtdb), "{p}");
    assert!(!drops.contains(&kept) && !drops.contains(&main_db), "{p}");
    assert!(l.db_exists(&gone), "dry run deletes nothing");
    let p = l.json(&d, &["git", "prune", "--grace", "1s", "--apply"]);
    assert_eq!(p["applied"], true);
    assert!(!l.db_exists(&gone) && !l.db_exists(&wtdb));
    for keep in [&kept, &main_db, &agent_db, &"pgx_app_manual".to_string()] {
        assert!(l.db_exists(keep), "{keep} must survive");
    }
    // Retired snapshots are pruned, the current and previous kept.
    for i in 0..3 {
        l.q(&d, &format!("CREATE TABLE t{i}(x int)"));
        l.pgx(&d, &["git", "sync"]);
    }
    let ls = l.json(&d, &["git", "ls"]);
    let retired: Vec<String> = ls["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["state"] == "retired")
        .map(|s| s["database"].as_str().unwrap().to_string())
        .collect();
    assert!(!retired.is_empty(), "{ls}");
    l.json(&d, &["git", "prune", "--apply"]);
    for r in &retired {
        assert!(!l.db_exists(r), "{r}");
    }
    let ls = l.json(&d, &["git", "ls"]);
    let ready = ls["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["state"] == "ready")
        .count();
    assert_eq!(ready, 2, "current + previous: {ls}");
}

// ---------------------------------------------------------------- K

#[test]
fn k_concurrent_operations_and_crash_recovery() {
    let l = lab();
    let d = l.repo("app");
    l.init(&d, &[]);
    l.q(&d, "CREATE TABLE t(x int)");
    l.pgx(&d, &["git", "sync"]);
    let mut wts = Vec::new();
    for i in 0..4 {
        let w = l.root.join(format!("wt{i}"));
        // --no-checkout hooks: create the worktrees silently, race later.
        let mut c = Command::new("git");
        c.args([
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("feature/c{i}"),
            w.to_str().unwrap(),
        ])
        .current_dir(&d);
        l.scrub(&mut c);
        c.env("PGX_GIT_SKIP", "1");
        assert_eq!(Run::from(common::output_locked(&mut c).unwrap()).code, 0);
        wts.push(w);
    }
    // Simultaneous: four first-use resolutions (two per worktree) + two forced syncs.
    let psql = find_psql();
    let t0 = Instant::now();
    let mut kids = Vec::new();
    for w in wts.iter().chain(wts.iter()) {
        let mut c = l.p.cmd(&[
            "git",
            "exec",
            "--",
            psql.to_str().unwrap(),
            "-X",
            "-qAtc",
            "SELECT current_database()",
        ]);
        c.current_dir(w);
        l.scrub(&mut c);
        kids.push((
            w.clone(),
            common::spawn_locked(
                c.stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped()),
            )
            .unwrap(),
        ));
    }
    for _ in 0..2 {
        let mut c = l.p.cmd(&["git", "sync", "--force"]);
        c.current_dir(&d);
        l.scrub(&mut c);
        kids.push((
            d.clone(),
            common::spawn_locked(
                c.stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped()),
            )
            .unwrap(),
        ));
    }
    let mut seen: std::collections::HashMap<PathBuf, String> = Default::default();
    for (w, k) in kids {
        let r = Run::from(k.wait_with_output().unwrap());
        assert_eq!(r.code, 0, "{}", r.all());
        if w != d {
            let db = r.stdout.trim().to_string();
            if let Some(prev) = seen.insert(w.clone(), db.clone()) {
                assert_eq!(prev, db, "one database per worktree even under races");
            }
        }
    }
    println!(
        "4 worktrees x2 concurrent first resolutions + 2 forced syncs: {} ms",
        t0.elapsed().as_millis()
    );
    let dbs: std::collections::HashSet<_> = seen.values().collect();
    assert_eq!(dbs.len(), 4);
    let ls = l.json(&d, &["git", "ls"]);
    assert_eq!(
        ls["branches"].as_array().unwrap().len(),
        5,
        "main + 4, no duplicates: {ls}"
    );
    let gen_before = l.json(&d, &["git", "status"])["main_snapshot"]
        .as_u64()
        .unwrap();

    if !cfg!(feature = "failpoints") {
        println!("crash-recovery part needs --features failpoints (fault injection); skipped in this build");
        return;
    }
    // Crash in the middle of staging, then a PGX restart.
    l.q(&d, "ALTER TABLE t ADD COLUMN y int");
    let mut c = l.p.cmd(&["git", "sync"]);
    c.current_dir(&d);
    l.scrub(&mut c);
    c.env("PGX_GIT_FAILPOINT", "hang");
    let mut hung = common::spawn_locked(
        c.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !l.databases().iter().any(|n| n.contains("_stg_")) {
        assert!(Instant::now() < deadline, "staging never appeared");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(300));
    unsafe { libc::kill(hung.id() as i32, libc::SIGKILL) };
    let _ = hung.wait();
    assert_eq!(l.p.run(&["restart"]).code, 0);
    let st = l.json(&d, &["git", "status"]);
    assert_eq!(
        st["main_snapshot"].as_u64().unwrap(),
        gen_before,
        "the valid snapshot is still current"
    );
    let s = l.json(&d, &["git", "sync"]);
    assert_eq!(s["status"], "published", "{s}");
    assert_eq!(s["generation"].as_u64().unwrap(), gen_before + 1);
    assert!(
        !l.databases().iter().any(|n| n.contains("_stg_")),
        "staging copy recovered: {:?}",
        l.databases()
    );
    for w in &wts {
        assert_eq!(
            &current_db(&l, w),
            seen.get(w).unwrap(),
            "mappings survived"
        );
    }
}

/// A release build ignores the fault-injection variable entirely.
#[cfg(not(feature = "failpoints"))]
#[test]
fn failpoints_are_not_compiled_into_normal_builds() {
    let l = lab();
    let d = l.repo("app");
    l.init(&d, &[]);
    l.q(&d, "CREATE TABLE t(x int)");
    for fp in ["copy", "validate", "publish"] {
        let r = l.pgx_env(
            &d,
            &[("PGX_GIT_FAILPOINT", fp)],
            &["--json", "git", "sync", "--force"],
        );
        assert_eq!(r.code, 0, "{fp}: {}", r.all());
        assert_eq!(r.json()["status"], "published", "{fp}");
    }
}

// ---------------------------------------------------------------- L

fn shell_run(l: &Lab, shell: &str, dir: &Path, script: &str) -> Run {
    let mut c = Command::new(shell);
    if shell.ends_with("fish") {
        c.args(["--no-config", "-c", script]);
    } else if shell.ends_with("zsh") {
        c.args(["-f", "-c", script]);
    } else {
        c.args(["--norc", "--noprofile", "-c", script]);
    }
    c.current_dir(dir);
    l.scrub(&mut c);
    let path = format!(
        "{}:{}",
        l.p.prefix.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    c.env("PATH", path);
    Run::from(common::output_locked(&mut c).unwrap())
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var("PATH")
        .ok()?
        .split(':')
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
}

#[test]
fn l_environment_reaches_bash_zsh_fish_and_exec() {
    let l = lab();
    let d = l.repo("app");
    l.init(&d, &[]);
    l.git(&d, &["switch", "-c", "feature/env"]);
    let want = current_db(&l, &d);
    let psql = find_psql();
    let check = format!("{} -X -qAtc 'SELECT current_database()'", psql.display());
    let mut shells = vec!["bash".to_string(), "zsh".to_string()];
    if which("fish").is_some() {
        shells.push("fish".into());
    } else {
        println!("fish not installed: fish env/shell integration not exercised on this machine");
    }
    for sh in &shells {
        let Some(bin) = which(sh) else { continue };
        let script = if sh == "fish" {
            format!("pgx git env --shell fish | source; and {check}; and echo \"git=$PGX_GIT_BRANCH iso=$PGX_ISOLATION\"")
        } else {
            format!("eval \"$(pgx git env --shell {sh})\" && {check} && echo \"git=$PGX_GIT_BRANCH iso=$PGX_ISOLATION\"")
        };
        let r = shell_run(&l, bin.to_str().unwrap(), &d, &script);
        assert_eq!(r.code, 0, "{sh}: {}", r.all());
        assert!(r.stdout.contains(&want), "{sh}: {}", r.all());
        assert!(
            r.stdout.contains("git=feature/env iso=ok"),
            "{sh}: {}",
            r.all()
        );

        // The prompt integration: refresh on branch change, restore outside.
        let refresh = "__pgx_git_refresh";
        let script = if sh == "fish" {
            format!(
                "set -gx DATABASE_URL postgres://outer/db; pgx git shell fish | source; echo A=$PGDATABASE; git switch -q main 2>/dev/null; {refresh}; echo B=$PGDATABASE; cd /; {refresh}; echo C=$DATABASE_URL; set -q PGX_GIT_BRANCH; or echo D=unset"
            )
        } else {
            format!(
                "export DATABASE_URL=postgres://outer/db; eval \"$(pgx git shell {sh})\"; echo A=$PGDATABASE; git switch -q main 2>/dev/null; {refresh}; echo B=$PGDATABASE; cd /; {refresh}; echo C=$DATABASE_URL; [ -z \"${{PGX_GIT_BRANCH+x}}\" ] && echo D=unset"
            )
        };
        let r = shell_run(&l, bin.to_str().unwrap(), &d, &script);
        assert_eq!(r.code, 0, "{sh}: {}", r.all());
        let main_db = l.json(&d, &["git", "status"])["main_database"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(r.stdout.contains(&format!("A={want}")), "{sh}: {}", r.all());
        assert!(
            r.stdout.contains(&format!("B={main_db}")),
            "{sh}: {}",
            r.all()
        );
        assert!(
            r.stdout.contains("C=postgres://outer/db"),
            "restored on leaving: {sh}: {}",
            r.all()
        );
        assert!(r.stdout.contains("D=unset"), "{sh}: {}", r.all());
        l.git(&d, &["switch", "feature/env"]);
    }
    // JSON is redacted too unless --reveal is given.
    let pw = l.p.password();
    let v = l.json(&d, &["git", "env"]);
    assert_eq!(v["redacted"], true);
    assert_eq!(v["env"]["PGPASSWORD"], "********", "{v}");
    assert!(!v.to_string().contains(&pw), "{v}");
    let v = l.json(&d, &["git", "env", "--reveal"]);
    assert_eq!(v["env"]["PGPASSWORD"], pw.as_str());
    assert!(v["env"]["DATABASE_URL"].as_str().unwrap().contains(&pw));
    // Human output masks the password.
    let r = l.pgx(&d, &["git", "env"]);
    assert_eq!(r.code, 0);
    assert!(!r.stdout.contains(&l.p.password()), "{}", r.stdout);
    assert!(r.stdout.contains("********") && r.stdout.contains("This shell is not changed"));
    // exec: exit codes and signals are the command's own.
    let r = l.pgx(&d, &["git", "exec", "--", "sh", "-c", "exit 7"]);
    assert_eq!(r.code, 7);
    let mut c = l.p.cmd(&["git", "exec", "--", "sh", "-c", "kill -TERM $$"]);
    c.current_dir(&d);
    l.scrub(&mut c);
    let o = common::output_locked(&mut c).unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(o.status.signal(), Some(libc::SIGTERM), "{o:?}");
    // install-shell is explicit and idempotent.
    let home = l.p.home.parent().unwrap();
    for _ in 0..2 {
        assert_eq!(l.pgx(&d, &["git", "install-shell", "bash"]).code, 0);
    }
    let rc = std::fs::read_to_string(home.join(".bashrc")).unwrap();
    assert_eq!(rc.matches(">>> pgx git shell integration >>>").count(), 1);
    assert!(!rc.contains(&l.p.password()) && !rc.contains("postgres://"));
    assert_eq!(l.pgx(&d, &["git", "uninstall", "--shell"]).code, 0);
    assert!(!std::fs::read_to_string(home.join(".bashrc"))
        .unwrap()
        .contains("pgx git"));
}

// ---------------------------------------------------------------- M

#[test]
fn m_hostile_names_collisions_and_inherited_urls() {
    let l = lab();
    let d = l.repo("app");
    l.init(&d, &[]);
    // Branch names Git accepts but a shell or SQL would not like.
    let evil = "feat/x;$(touch${IFS}pwned)`touch${IFS}pwned2`'\"".to_string();
    l.git(&d, &["switch", "-c", &evil]);
    let db = current_db(&l, &d);
    assert!(db.starts_with("pgx_app_x_touch_"), "{db}");
    assert!(
        db.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
        "{db}"
    );
    for m in ["pwned", "pwned2"] {
        assert!(
            !d.join(m).exists() && !l.root.join(m).exists(),
            "nothing was evaluated"
        );
    }
    let st = l.json(&d, &["git", "status"]);
    assert_eq!(
        st["git_branch"],
        evil.as_str(),
        "the real name is kept as metadata"
    );
    // Very long names fit PostgreSQL.
    let long = format!("feature/{}", "a".repeat(200));
    l.git(&d, &["switch", "-c", &long]);
    let ldb = current_db(&l, &d);
    assert!(ldb.len() <= 63, "{ldb}");
    // A foreign database already holding the derived name is never reused.
    l.git(&d, &["switch", "main"]);
    l.git(&d, &["switch", "-c", "feature/taken"]);
    let taken_db = current_db(&l, &d);
    l.git(&d, &["switch", "main"]);
    l.git(&d, &["branch", "-D", "feature/taken"]);
    std::thread::sleep(Duration::from_millis(1100));
    l.json(&d, &["git", "prune", "--grace", "1s", "--apply"]);
    assert!(!l.db_exists(&taken_db));
    assert_eq!(l.p.run(&["create", &taken_db]).code, 0);
    assert_eq!(
        l.p.psql(&taken_db, "CREATE TABLE foreign_data(x int)").code,
        0
    );
    l.git(&d, &["switch", "-c", "feature/taken"]);
    let now_db = current_db(&l, &d);
    assert_ne!(now_db, taken_db, "collision resolved with the long name");
    assert!(
        now_db.starts_with(&taken_db[..taken_db.len() - 6]),
        "{now_db}"
    );
    assert_eq!(
        l.p.psql(&taken_db, "SELECT count(*) FROM foreign_data")
            .stdout
            .trim(),
        "0",
        "foreign database untouched"
    );

    // An inherited DATABASE_URL is never used when resolution fails.
    let plain = l.repo("plain");
    let out_file = l.root.join("ran");
    let r = l.pgx_env(
        &plain,
        &[("DATABASE_URL", "postgres://u:p@prod.example.com/db")],
        &[
            "git",
            "exec",
            "--",
            "sh",
            "-c",
            &format!("touch {}", out_file.display()),
        ],
    );
    assert_eq!(r.code, 3, "not enabled: refuse, {}", r.all());
    assert!(!out_file.exists(), "the command did not run");
    let r = l.pgx_env(&plain, &[], &["git", "env", "--shell", "bash", "--hook"]);
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.trim().is_empty(),
        "a repository without PGX Git is left alone: {}",
        r.stdout
    );
    // Enabled but broken state: the shell gets the fail-closed sentinel.
    let broken = l.repo("broken");
    std::fs::write(broken.join(".pgx.toml"), "[git]\nenabled = true\n").unwrap();
    let r = l.pgx_env(&broken, &[], &["git", "env", "--shell", "bash", "--hook"]);
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains("pgx_isolation_failed") && r.stdout.contains("PGX_ISOLATION='failed'"),
        "{}",
        r.all()
    );
    let r = l.pgx_env(
        &broken,
        &[("DATABASE_URL", "postgres://u:p@prod.example.com/db")],
        &[
            "git",
            "exec",
            "--",
            "sh",
            "-c",
            &format!("touch {}", out_file.display()),
        ],
    );
    assert_ne!(r.code, 0);
    assert!(!out_file.exists());
    // Unsafe configuration values are refused, and the hook never blocks Git.
    std::fs::write(
        broken.join(".pgx.toml"),
        "[git]\nsource = \"postgres://u:p@prod/db\"\n",
    )
    .unwrap();
    let r = l.pgx(&broken, &["git", "status"]);
    assert_eq!(r.code, 2, "{}", r.all());
    assert!(
        !r.stderr.contains("u:p@prod"),
        "no credentials echoed back: {}",
        r.all()
    );
    std::fs::write(broken.join(".pgx.toml"), "[git]\nmain_branch = \"-x ..\"\n").unwrap();
    assert_eq!(l.pgx(&broken, &["git", "status"]).code, 2);
    std::fs::write(
        broken.join(".pgx.toml"),
        "[git]\nenabled = true\n[git.migrations]\ncommand = \"rails db:migrate; curl x | sh\"\n",
    )
    .unwrap();
    assert_eq!(l.pgx(&broken, &["git", "status"]).code, 2);
    let r = {
        let mut c = Command::new("git");
        c.args(["switch", "-c", "anything"]).current_dir(&broken);
        l.scrub(&mut c);
        Run::from(common::output_locked(&mut c).unwrap())
    };
    assert_eq!(
        r.code,
        0,
        "a broken .pgx.toml never blocks git: {}",
        r.all()
    );
    // No credentials in hooks, state files or the committed config.
    let pw = l.p.password();
    for f in ["post-checkout", "post-merge", "post-rewrite"] {
        assert!(!std::fs::read_to_string(d.join(".git/hooks").join(f))
            .unwrap()
            .contains(&pw));
    }
    for e in std::fs::read_dir(l.p.home.join("git")).unwrap().flatten() {
        let Ok(s) = std::fs::read_to_string(e.path().join("state.json")) else {
            continue;
        };
        assert!(!s.contains(&pw) && !s.contains("postgres://"), "{s}");
    }
}

// ---------------------------------------------------------------- misc

#[test]
fn renamed_branches_keep_their_database_and_doctor_reports() {
    let l = lab();
    let d = l.repo("app");
    l.init(&d, &[]);
    l.git(&d, &["switch", "-c", "feature/old"]);
    l.q(&d, "CREATE TABLE mine(x int)");
    let db = current_db(&l, &d);
    l.git(&d, &["branch", "-m", "feature/old", "feature/new"]);
    assert_eq!(current_db(&l, &d), db, "carried over on rename");
    let st = l.json(&d, &["git", "status"]);
    assert_eq!(st["git_branch"], "feature/new");
    let doc = l.pgx(&d, &["--json", "git", "doctor"]);
    let v = doc.json();
    assert_eq!(v["ok"], true, "{v}");
    let names: Vec<String> = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["check"].as_str().unwrap().to_string())
        .collect();
    for want in [
        "repository",
        "config",
        "runtime",
        "main database",
        "snapshot",
        "hook",
        "shell",
        "mappings",
        "agents",
    ] {
        assert!(
            names.iter().any(|n| n == want),
            "doctor lacks {want}: {names:?}"
        );
    }
}

#[test]
fn h2_a_claude_session_that_switches_git_branches_is_routed_to_the_new_branch() {
    use std::io::Write;
    let l = lab();
    let d = l.repo("app");
    l.init(&d, &[]);
    l.git(&d, &["switch", "-c", "feature/one"]);
    l.q(&d, "CREATE TABLE one_only(x int)");
    let sid = "ffff6666-0000-0000-0000-000000000006";
    let env_file = l.root.join("claude.env");
    let hook = |payload: serde_json::Value| -> Run {
        let mut c = l.p.cmd(&["agent", "hook"]);
        c.current_dir(&l.root);
        l.scrub(&mut c);
        c.env("CLAUDE_ENV_FILE", &env_file);
        c.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = common::spawn_locked(&mut c).unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        Run::from(child.wait_with_output().unwrap())
    };
    let base = |event: &str| serde_json::json!({"session_id": sid, "transcript_path": "/tmp/t.jsonl", "cwd": d, "hook_event_name": event, "permission_mode": "default"});
    let r = hook(base("SessionStart"));
    assert_eq!(r.code, 0, "{}", r.all());
    let exported = std::fs::read_to_string(&env_file).unwrap();
    let start_branch = exported
        .lines()
        .find_map(|l| l.strip_prefix("export PGX_BRANCH="))
        .unwrap()
        .trim_matches('\'')
        .to_string();
    assert_eq!(
        l.p.psql(&start_branch, "SELECT to_regclass('one_only') IS NOT NULL")
            .stdout
            .trim(),
        "t",
        "cloned from feature/one's database"
    );

    let mut tool = base("PreToolUse");
    tool["tool_name"] = "Bash".into();
    tool["tool_input"] = serde_json::json!({"command": "psql -Atc 'select current_database()'", "description": "db"});
    // Same Git branch: nothing to rewrite.
    let r = hook(tool.clone());
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(!r.stdout.contains("updatedInput"), "{}", r.all());
    // The agent switches Git branches: its next command goes to the new branch's copy.
    l.git(&d, &["switch", "-c", "feature/two"]);
    let r = hook(tool.clone());
    assert_eq!(r.code, 0, "{}", r.all());
    let v: serde_json::Value =
        serde_json::from_str(r.stdout.trim()).unwrap_or_else(|_| panic!("{}", r.all()));
    let cmd = v["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        cmd.contains(" agent exec --cwd ") && cmd.contains("# pgx-routed"),
        "{cmd}"
    );
    let new_branch = cmd.rsplit(' ').next().unwrap().to_string();
    assert_ne!(new_branch, start_branch);
    assert_eq!(
        l.p.psql(&new_branch, "SELECT to_regclass('one_only') IS NULL")
            .stdout
            .trim(),
        "t",
        "a copy of feature/two"
    );
    // Run the rewritten command the way Claude Code would (session id in the env).
    let psql_dir = find_psql().parent().unwrap().to_path_buf();
    let mut c = Command::new("/bin/sh");
    c.arg("-c").arg(&cmd).current_dir(&d);
    l.scrub(&mut c);
    c.env("CLAUDE_CODE_SESSION_ID", sid)
        .env("PATH", format!("{}:/usr/bin:/bin", psql_dir.display()));
    let r = Run::from(common::output_locked(&mut c).unwrap());
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(r.stdout.trim(), new_branch);
    // Same-command switch: a command that switches branches and then uses the
    // database is refused before it runs (it would use the old environment)…
    let mut both = tool.clone();
    both["tool_input"]["command"] =
        "git switch -c feature/three && psql -Atc 'select current_database()'".into();
    let r = hook(both);
    let v: serde_json::Value =
        serde_json::from_str(r.stdout.trim()).unwrap_or_else(|_| panic!("{}", r.all()));
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny", "{v}");
    let why = v["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap();
    assert!(why.contains("pgx git exec --"), "{why}");
    assert!(
        !d.join(".git/refs/heads/feature/three").exists(),
        "nothing ran"
    );
    // …a switch on its own is fine…
    let mut alone = tool.clone();
    alone["tool_input"]["command"] = "git switch -c feature/three".into();
    assert!(!hook(alone).stdout.contains("\"deny\""));
    l.git(&d, &["switch", "feature/two"]);
    // …and the documented one-command form re-resolves after the switch.
    let mut wrapped = tool.clone();
    let one = format!(
        "git switch -q -c feature/three && {} git exec -- {} -X -Atc 'select current_database()'",
        l.p.bin().display(),
        find_psql().display()
    );
    wrapped["tool_input"]["command"] = one.clone().into();
    let r = hook(wrapped);
    assert!(!r.stdout.contains("\"deny\""), "{}", r.all());
    let mut c = Command::new("/bin/sh");
    c.arg("-c").arg(&one).current_dir(&d);
    l.scrub(&mut c);
    c.env("CLAUDE_CODE_SESSION_ID", sid);
    let r = Run::from(common::output_locked(&mut c).unwrap());
    assert_eq!(r.code, 0, "{}", r.all());
    let three = r.stdout.trim().to_string();
    assert!(
        three.starts_with("agent-claude-"),
        "the session's own branch: {three}"
    );
    assert_ne!(
        three, new_branch,
        "resolved for feature/three, not feature/two"
    );
    let ls = l.json(&d, &["agent", "ls"]);
    let rec = ls
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == three.as_str())
        .unwrap()
        .clone();
    assert_eq!(rec["agent"]["git_branch"], "feature/three", "{rec}");
    l.git(&d, &["switch", "feature/two"]);
    // A routed command is not routed again.
    let mut again = tool.clone();
    again["tool_input"]["command"] = cmd.clone().into();
    assert!(!hook(again).stdout.contains("updatedInput"));
}

#[test]
fn n_repairs_and_non_shell_hooks() {
    let l = lab();
    let d = l.repo("app");
    // A Ruby hook (overcommit-style) must not get a POSIX block.
    let hooks = d.join(".git/hooks");
    let ruby = "#!/usr/bin/env ruby\nputs 'team'\n";
    std::fs::write(hooks.join("post-merge"), ruby).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        hooks.join("post-merge"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let v = l.init(&d, &[]);
    let pm = v["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == "post-merge")
        .unwrap()
        .clone();
    assert_eq!(pm["action"], "skipped", "{v}");
    assert_eq!(
        std::fs::read_to_string(hooks.join("post-merge")).unwrap(),
        ruby
    );

    l.q(&d, "CREATE TABLE t(x int)");
    let s = l.json(&d, &["git", "sync"]);
    let gen = s["generation"].as_u64().unwrap();
    // A snapshot dropped by hand: a plain sync notices and republishes.
    let ls = l.json(&d, &["git", "ls"]);
    let snap = ls["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["generation"] == gen)
        .unwrap()["database"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(l.p.run(&["drop", &snap]).code, 0);
    let s = l.json(&d, &["git", "sync"]);
    assert_eq!(s["status"], "published", "{s}");
    l.git(&d, &["switch", "-c", "feature/after-repair"]);
    assert_eq!(l.q(&d, "SELECT to_regclass('t') IS NOT NULL"), "t");

    // The main database dropped by hand: restored from the snapshot, never
    // published as an empty baseline.
    l.git(&d, &["switch", "main"]);
    let main_db = current_db(&l, &d);
    assert_eq!(l.p.run(&["drop", &main_db, "--force"]).code, 0);
    assert_eq!(
        l.q(&d, "SELECT to_regclass('t') IS NOT NULL"),
        "t",
        "restored with its schema"
    );
    let s = l.json(&d, &["git", "sync"]);
    assert_eq!(s["status"], "unchanged", "{s}");

    // A branch whose .pgx.toml predates PGX Git stays isolated.
    l.git(&d, &["switch", "-c", "feature/old-config"]);
    std::fs::remove_file(d.join(".pgx.toml")).unwrap();
    let r = l.pgx(&d, &["--json", "git", "status"]);
    assert_eq!(r.json()["enabled"], true, "{}", r.all());
    assert!(current_db(&l, &d).starts_with("pgx_app_old_config_"));
}
