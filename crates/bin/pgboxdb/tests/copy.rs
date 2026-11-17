//! Black-box tests for `pgboxdb copy`: a real PostgreSQL 18 source (initdb'd per
//! test, scram over TCP) copied into a real PGBoxDB runtime from the assembled
//! distribution, then branched with `pgboxdb clone`. Needs PGBOXDB_DIST and a
//! PostgreSQL bin directory (PGBOXDB_TEST_PG_BINDIR / PG18_BINDIR / PATH).
mod common;
use common::{PgSource, Pgboxdb};
use std::time::{Duration, Instant};

/// A representative application schema: extensions, enum, serials, FK, GIN
/// index, view, materialized view, SQL + PL/pgSQL functions, trigger, a free
/// sequence, a comment, and data.
const APP_SCHEMA: &str = "
CREATE EXTENSION IF NOT EXISTS pgcrypto; CREATE EXTENSION IF NOT EXISTS citext;
CREATE TYPE mood AS ENUM ('sad','ok','happy');
CREATE TABLE users(id bigserial PRIMARY KEY, email citext UNIQUE NOT NULL, name text, mood mood DEFAULT 'ok', created_at timestamptz DEFAULT now());
CREATE TABLE posts(id bigserial PRIMARY KEY, user_id bigint REFERENCES users(id) ON DELETE CASCADE, title text NOT NULL, body text, tags text[], meta jsonb DEFAULT '{}'::jsonb);
CREATE INDEX posts_user_idx ON posts(user_id); CREATE INDEX posts_meta_gin ON posts USING gin(meta);
CREATE VIEW user_posts AS SELECT u.email, count(p.id) AS n FROM users u LEFT JOIN posts p ON p.user_id=u.id GROUP BY u.email;
CREATE FUNCTION post_count(uid bigint) RETURNS bigint LANGUAGE sql STABLE AS $$ SELECT count(*) FROM posts WHERE user_id = uid $$;
CREATE FUNCTION touch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.created_at := now(); RETURN NEW; END $$;
CREATE TRIGGER users_touch BEFORE INSERT ON users FOR EACH ROW EXECUTE FUNCTION touch();
CREATE SEQUENCE order_no START 1000;
CREATE MATERIALIZED VIEW post_stats AS SELECT user_id, count(*) FROM posts GROUP BY user_id;
INSERT INTO users(email,name) SELECT 'u'||g||'@x.io','User '||g FROM generate_series(1,2000) g;
INSERT INTO posts(user_id,title,body,tags,meta) SELECT (g % 2000) + 1, 'Title '||g, repeat('lorem ipsum ',10), ARRAY['a','b'], jsonb_build_object('n',g) FROM generate_series(1,20000) g;
REFRESH MATERIALIZED VIEW post_stats;
SELECT nextval('order_no'); SELECT nextval('order_no');
COMMENT ON TABLE users IS 'people';
";

fn source_with_app() -> PgSource {
    let src = PgSource::start();
    src.create_db("myapp");
    let r = src.psql("myapp", APP_SCHEMA);
    assert_eq!(r.code, 0, "{}", r.all());
    src
}

fn pgboxdb_up() -> Pgboxdb {
    let p = Pgboxdb::new();
    let r = p.run_cmd(&[]);
    assert_eq!(
        r.code,
        0,
        "pgboxdb run failed: {}\n{}",
        r.all(),
        p.log_tail()
    );
    p
}

fn count(p: &Pgboxdb, db: &str, sql: &str) -> String {
    let r = p.psql(db, sql);
    assert_eq!(r.code, 0, "{}", r.all());
    r.stdout.trim().to_string()
}

#[test]
fn a_full_copy_restores_schema_and_data_without_touching_the_source() {
    let src = source_with_app();
    let p = pgboxdb_up();
    let before = src.fingerprint("myapp");

    let r = p.copy(&src, "myapp", "myapp_template", &[]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(
        r.stdout.contains("Database ready: myapp_template"),
        "{}",
        r.stdout
    );
    assert!(r.stdout.contains("pgboxdb branch myapp_template agent-1"));
    assert!(r.stderr.contains("✓ publish"), "{}", r.stderr);
    // The source password must never appear anywhere in the output.
    assert!(!r.stdout.contains(&src.password) && !r.stderr.contains(&src.password));

    // Data and objects.
    assert_eq!(
        count(&p, "myapp_template", "SELECT count(*) FROM users"),
        "2000"
    );
    assert_eq!(
        count(&p, "myapp_template", "SELECT count(*) FROM posts"),
        "20000"
    );
    assert_eq!(
        count(&p, "myapp_template", "SELECT count(*) FROM post_stats"),
        "2000"
    );
    assert_eq!(
        count(&p, "myapp_template", "SELECT nextval('order_no')"),
        "1002",
        "sequence value continues after the source's"
    );
    assert_eq!(
        count(
            &p,
            "myapp_template",
            "SELECT nextval('users_id_seq') > 2000"
        ),
        "t"
    );
    assert_eq!(
        count(&p, "myapp_template", "SELECT count(*) FROM user_posts"),
        "2000"
    );
    assert_eq!(count(&p, "myapp_template", "SELECT post_count(1)"), "10");
    assert_eq!(
        count(
            &p,
            "myapp_template",
            "SELECT obj_description('users'::regclass)"
        ),
        "people"
    );
    assert_eq!(
        count(
            &p,
            "myapp_template",
            "SELECT string_agg(extname, ',' ORDER BY extname) FROM pg_extension"
        ),
        "citext,pgcrypto,plpgsql"
    );
    assert_eq!(
        count(
            &p,
            "myapp_template",
            "SELECT count(*) FROM pg_indexes WHERE tablename = 'posts'"
        ),
        "3"
    );
    assert_eq!(
        count(
            &p,
            "myapp_template",
            "SELECT 'x@Y.io'::citext = 'X@y.io'::citext"
        ),
        "t"
    );
    assert_eq!(
        count(&p, "myapp_template", "SELECT 'happy'::mood > 'sad'::mood"),
        "t"
    );
    // FK and trigger survived.
    let fk = p.psql(
        "myapp_template",
        "INSERT INTO posts(user_id, title) VALUES (999999, 'orphan')",
    );
    assert_ne!(fk.code, 0, "foreign key must be enforced: {}", fk.all());
    assert_eq!(
        count(&p, "myapp_template", "INSERT INTO users(email, created_at) VALUES ('t@t.io', '2000-01-01') RETURNING created_at > '2020-01-01'"),
        "t",
        "BEFORE INSERT trigger must run"
    );
    // Objects are owned by the PGBoxDB superuser, not the source role.
    assert_eq!(
        count(
            &p,
            "myapp_template",
            "SELECT tableowner FROM pg_tables WHERE tablename = 'users'"
        ),
        "postgres"
    );

    // Nothing left behind, and the source is byte-for-byte the same.
    assert!(p.copy_leftovers().is_empty(), "{:?}", p.copy_leftovers());
    assert!(p.staging_dbs().is_empty());
    assert_eq!(
        src.fingerprint("myapp"),
        before,
        "the source must not change"
    );
}

#[test]
fn a_json_contract() {
    let src = source_with_app();
    let p = pgboxdb_up();
    let r = p.copy_json(&src, "myapp", "tpl", &[]);
    assert_eq!(r.code, 0, "{}", r.all());
    let j = r.json();
    assert_eq!(j["status"], "success");
    assert_eq!(j["operation"], "copy");
    assert_eq!(j["source"]["engine"], "postgresql");
    assert!(
        j["source"]["version"].as_str().unwrap().starts_with("1"),
        "{j}"
    );
    assert_eq!(j["source"]["database"], "myapp");
    assert_eq!(j["source"]["port"].as_u64().unwrap(), src.port as u64);
    assert_eq!(j["destination"]["engine"], "pgboxdb");
    assert_eq!(j["destination"]["database"], "tpl");
    assert!(j["destination"]["url"].as_str().unwrap().ends_with("/tpl"));
    assert_eq!(j["schema_only"], false);
    assert!(j["duration_ms"].as_u64().unwrap() > 0);
    assert_eq!(j["objects"]["tables"], 2);
    assert_eq!(j["objects"]["materialized_views"], 1);
    assert_eq!(j["extensions"], serde_json::json!(["citext", "pgcrypto"]));
    assert!(j["stages"].as_array().unwrap().len() >= 8, "{j}");
    assert_eq!(j["warnings"], serde_json::json!([]));
    let text = r.stdout.clone() + &r.stderr;
    assert!(!text.contains(&src.password), "source password leaked");
    // JSON mode prints exactly one line on stdout.
    assert_eq!(r.stdout.trim().lines().count(), 1, "{}", r.stdout);
}

#[test]
fn b_schema_only_copy_has_every_object_and_no_rows() {
    let src = source_with_app();
    let p = pgboxdb_up();
    let r = p.copy_json(&src, "myapp", "schema_tpl", &["--schema-only"]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(r.json()["schema_only"], true);
    for (sql, want) in [
        ("SELECT count(*) FROM users", "0"),
        ("SELECT count(*) FROM posts", "0"),
        ("SELECT count(*) FROM pg_tables WHERE schemaname = 'public'", "2"),
        ("SELECT count(*) FROM pg_indexes WHERE schemaname = 'public'", "5"),
        ("SELECT count(*) FROM pg_views WHERE schemaname = 'public'", "1"),
        ("SELECT count(*) FROM pg_matviews WHERE schemaname = 'public'", "1"),
        (
            "SELECT count(*) FROM pg_proc p WHERE p.pronamespace = 'public'::regnamespace AND NOT EXISTS \
             (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid AND d.deptype = 'e')",
            "2",
        ),
        ("SELECT count(*) FROM pg_trigger WHERE tgname = 'users_touch'", "1"),
        ("SELECT count(*) FROM pg_type WHERE typname = 'mood'", "1"),
        ("SELECT count(*) FROM pg_extension WHERE extname IN ('citext','pgcrypto')", "2"),
        ("SELECT count(*) FROM pg_class WHERE relkind = 'S' AND relnamespace = 'public'::regnamespace", "3"),
        ("SELECT conname FROM pg_constraint WHERE conname = 'posts_user_id_fkey'", "posts_user_id_fkey"),
    ] {
        assert_eq!(count(&p, "schema_tpl", sql), want, "{sql}");
    }
    // Usable for fixtures and migrations right away.
    assert_eq!(
        count(
            &p,
            "schema_tpl",
            "INSERT INTO users(email) VALUES ('a@b.c') RETURNING id"
        ),
        "1"
    );
    assert!(p.copy_leftovers().is_empty());
}

#[test]
fn c_copy_then_clone_branches_are_isolated() {
    let src = source_with_app();
    let p = pgboxdb_up();
    let before = src.fingerprint("myapp");
    assert_eq!(p.copy(&src, "myapp", "tpl", &["--jobs", "2"]).code, 0);
    for agent in ["agent-1", "agent-2"] {
        let r = p.run(&["clone", "tpl", agent, "--ttl", "2h"]);
        assert_eq!(r.code, 0, "{}", r.all());
    }
    assert_eq!(count(&p, "agent-1", "SELECT count(*) FROM posts"), "20000");
    assert_eq!(count(&p, "agent-2", "SELECT count(*) FROM posts"), "20000");
    // Writes, DDL and a migration in agent-1 only.
    let w = p.psql(
        "agent-1",
        "DELETE FROM posts WHERE id <= 5000; INSERT INTO users(email) VALUES ('new@agent1.io'); \
         ALTER TABLE users ADD COLUMN plan text DEFAULT 'free'; CREATE TABLE agent_only(id int); DROP VIEW user_posts",
    );
    assert_eq!(w.code, 0, "{}", w.all());
    assert_eq!(count(&p, "agent-1", "SELECT count(*) FROM posts"), "15000");
    assert_eq!(count(&p, "agent-1", "SELECT count(*) FROM users"), "2001");
    for other in ["agent-2", "tpl"] {
        assert_eq!(
            count(&p, other, "SELECT count(*) FROM posts"),
            "20000",
            "{other}"
        );
        assert_eq!(
            count(&p, other, "SELECT count(*) FROM users"),
            "2000",
            "{other}"
        );
        assert_eq!(count(&p, other, "SELECT count(*) FROM information_schema.columns WHERE table_name='users' AND column_name='plan'"), "0", "{other}");
        assert_eq!(
            count(
                &p,
                other,
                "SELECT count(*) FROM pg_tables WHERE tablename='agent_only'"
            ),
            "0",
            "{other}"
        );
        assert_eq!(
            count(
                &p,
                other,
                "SELECT count(*) FROM pg_views WHERE viewname='user_posts'"
            ),
            "1",
            "{other}"
        );
    }
    // Each branch has its own URL; the source PostgreSQL is untouched.
    let u1 = p.run(&["url", "agent-1"]).stdout;
    let u2 = p.run(&["url", "agent-2"]).stdout;
    assert_ne!(u1.trim(), u2.trim());
    assert!(u1.contains("/agent-1"));
    assert_eq!(src.fingerprint("myapp"), before);
    assert_eq!(
        src.psql("myapp", "SELECT count(*) FROM posts")
            .stdout
            .trim(),
        "20000"
    );
}

#[test]
fn d_rails_style_migrations_run_on_a_branch() {
    let src = source_with_app();
    let p = pgboxdb_up();
    assert_eq!(p.copy(&src, "myapp", "tpl", &[]).code, 0);
    assert_eq!(p.run(&["clone", "tpl", "migrate-1", "--ttl", "1h"]).code, 0);
    // What `rails db:migrate` issues (ActiveRecord PostgreSQL adapter), one
    // statement per call where PostgreSQL forbids a transaction block.
    let steps: &[&str] = &[
        "SELECT pg_try_advisory_lock(3053653674220360)",
        "CREATE TABLE IF NOT EXISTS \"schema_migrations\" (\"version\" character varying NOT NULL PRIMARY KEY)",
        "CREATE TABLE IF NOT EXISTS \"ar_internal_metadata\" (\"key\" character varying NOT NULL PRIMARY KEY, \"value\" character varying, \"created_at\" timestamp(6) NOT NULL, \"updated_at\" timestamp(6) NOT NULL)",
        "CREATE EXTENSION IF NOT EXISTS \"plpgsql\"",
        "BEGIN; CREATE TABLE \"comments\" (\"id\" bigserial primary key, \"post_id\" bigint NOT NULL, \"body\" text, \"created_at\" timestamp(6) NOT NULL, \"updated_at\" timestamp(6) NOT NULL); \
         CREATE INDEX \"index_comments_on_post_id\" ON \"comments\" (\"post_id\"); \
         ALTER TABLE \"comments\" ADD CONSTRAINT \"fk_rails_2fd19c0db7\" FOREIGN KEY (\"post_id\") REFERENCES \"posts\" (\"id\"); \
         INSERT INTO \"schema_migrations\" (version) VALUES ('20261024000001'); COMMIT",
        "BEGIN; ALTER TABLE \"users\" ADD \"plan\" character varying DEFAULT 'free' NOT NULL; \
         ALTER TABLE \"users\" RENAME COLUMN \"name\" TO \"full_name\"; \
         ALTER TABLE \"posts\" ALTER COLUMN \"title\" TYPE character varying(255); \
         ALTER TABLE \"posts\" ADD CONSTRAINT \"title_len\" CHECK (length(title) > 0) NOT VALID; \
         INSERT INTO \"schema_migrations\" (version) VALUES ('20261024000002'); COMMIT",
        "ALTER TABLE \"posts\" VALIDATE CONSTRAINT \"title_len\"",
        "CREATE INDEX CONCURRENTLY \"index_users_on_plan\" ON \"users\" (\"plan\")",
        "CREATE UNIQUE INDEX CONCURRENTLY \"index_users_on_lower_email\" ON \"users\" (lower(email::text))",
        "BEGIN; UPDATE users SET plan = 'pro' WHERE id % 10 = 0; INSERT INTO \"schema_migrations\" (version) VALUES ('20261024000003'); COMMIT",
        "INSERT INTO \"ar_internal_metadata\" (\"key\", \"value\", \"created_at\", \"updated_at\") VALUES ('environment', 'development', now(), now()) ON CONFLICT (\"key\") DO UPDATE SET \"value\" = EXCLUDED.\"value\", \"updated_at\" = EXCLUDED.\"updated_at\"",
        "SELECT pg_advisory_unlock(3053653674220360)",
    ];
    for s in steps {
        let r = p.psql("migrate-1", s);
        assert_eq!(r.code, 0, "migration step failed: {s}\n{}", r.all());
    }
    assert_eq!(
        count(&p, "migrate-1", "SELECT count(*) FROM schema_migrations"),
        "3"
    );
    assert_eq!(
        count(
            &p,
            "migrate-1",
            "SELECT count(*) FROM users WHERE plan = 'pro'"
        ),
        "200"
    );
    assert_eq!(
        count(
            &p,
            "migrate-1",
            "SELECT count(*) FROM pg_indexes WHERE tablename = 'users'"
        ),
        "4"
    );
    assert_eq!(
        count(&p, "migrate-1", "SELECT data_type FROM information_schema.columns WHERE table_name = 'posts' AND column_name = 'title'"),
        "character varying"
    );
    // The template did not migrate.
    assert_eq!(
        count(
            &p,
            "tpl",
            "SELECT count(*) FROM pg_tables WHERE tablename IN ('schema_migrations','comments')"
        ),
        "0"
    );
    assert_eq!(count(&p, "tpl", "SELECT count(*) FROM information_schema.columns WHERE table_name='users' AND column_name='name'"), "1");
    // A second agent gets the unmigrated template.
    assert_eq!(p.run(&["clone", "tpl", "migrate-2", "--ttl", "1h"]).code, 0);
    assert_eq!(
        count(
            &p,
            "migrate-2",
            "SELECT count(*) FROM pg_tables WHERE tablename = 'comments'"
        ),
        "0"
    );
}

#[test]
fn e_restore_failure_publishes_nothing_and_cleans_up() {
    let src = PgSource::start();
    src.create_db("rlsdb");
    // A policy granted to a role that does not exist in PGBoxDB: pg_restore fails
    // on that object. (Roles are not part of a database dump.)
    assert_eq!(src.psql("postgres", "CREATE ROLE app_reader").code, 0);
    let r = src.psql(
        "rlsdb",
        "CREATE TABLE t(id int); INSERT INTO t VALUES (1),(2); ALTER TABLE t ENABLE ROW LEVEL SECURITY; \
         CREATE POLICY p ON t TO app_reader USING (true)",
    );
    assert_eq!(r.code, 0, "{}", r.all());
    let p = pgboxdb_up();
    assert_eq!(p.run(&["create", "existing"]).code, 0);
    assert_eq!(
        p.psql(
            "existing",
            "CREATE TABLE keep(id int); INSERT INTO keep VALUES (1)"
        )
        .code,
        0
    );
    let before = src.fingerprint("rlsdb");

    let r = p.copy(&src, "rlsdb", "broken", &[]);
    assert_eq!(r.code, 1, "{}", r.all());
    assert!(
        r.stderr.contains("app_reader"),
        "names the failing object: {}",
        r.stderr
    );
    assert!(r.stderr.contains("nothing was published"), "{}", r.stderr);
    assert!(r.stderr.contains("CREATE ROLE"), "hint: {}", r.stderr);
    assert!(
        !p.db_exists("broken"),
        "a partial destination must not exist"
    );
    assert!(
        p.staging_dbs().is_empty(),
        "staging removed: {:?}",
        p.staging_dbs()
    );
    assert!(p.copy_leftovers().is_empty(), "{:?}", p.copy_leftovers());
    // Other databases and the source are intact.
    assert_eq!(count(&p, "existing", "SELECT count(*) FROM keep"), "1");
    assert_eq!(count(&p, "app", "SELECT 1"), "1");
    assert_eq!(src.fingerprint("rlsdb"), before);
    // JSON form of the same failure.
    let j = p.copy_json(&src, "rlsdb", "broken", &[]);
    assert_eq!(j.code, 1);
    let v = j.json();
    assert_eq!(v["error"]["exit_code"], 1);
    assert!(v["error"]["details"]["pg_restore"].is_array(), "{v}");
    // After creating the role in PGBoxDB the same copy succeeds.
    assert_eq!(p.psql("postgres", "CREATE ROLE app_reader").code, 0);
    let r = p.copy(&src, "rlsdb", "fixed", &[]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert_eq!(count(&p, "fixed", "SELECT count(*) FROM t"), "2");
    assert_eq!(
        count(
            &p,
            "fixed",
            "SELECT count(*) FROM pg_policies WHERE policyname = 'p'"
        ),
        "1"
    );
}

#[test]
fn f_postgres_and_pgboxdb_coexist_on_different_ports() {
    let src = source_with_app();
    let p = pgboxdb_up();
    assert_ne!(src.port, p.port);
    // Both answer on 127.0.0.1 at the same time, and the copy goes across.
    assert_eq!(src.psql("myapp", "SELECT 1").stdout.trim(), "1");
    assert_eq!(
        p.psql_tcp("app", &p.password(), "SELECT 1").stdout.trim(),
        "1"
    );
    let r = p.copy_json(&src, "myapp", "tpl", &[]);
    assert_eq!(r.code, 0, "{}", r.all());
    let j = r.json();
    assert_eq!(j["source"]["port"].as_u64().unwrap(), src.port as u64);
    assert_eq!(j["destination"]["port"].as_u64().unwrap(), p.port as u64);
    assert_eq!(p.run(&["clone", "tpl", "agent-1", "--ttl", "1h"]).code, 0);
    let url = p.run(&["url", "agent-1"]).stdout.trim().to_string();
    assert!(url.contains(&format!(":{}/agent-1", p.port)), "{url}");
    assert!(!url.contains(&format!(":{}/", src.port)));
}

#[test]
fn c2_copy_then_branch_is_the_two_command_workflow() {
    // The product story in two commands: import once, then a branch per agent.
    let src = source_with_app();
    let p = pgboxdb_up();
    let before = src.fingerprint("myapp");
    assert_eq!(p.copy(&src, "myapp", "myapp", &[]).code, 0);
    let mut urls = Vec::new();
    for agent in ["agent-1", "agent-2"] {
        let r = p.run(&["branch", "myapp", agent]);
        assert_eq!(r.code, 0, "{}", r.all());
        assert!(r.stdout.contains("✓ Expires in: 2 hours"), "{}", r.stdout);
        urls.push(r.stdout.trim_end().lines().last().unwrap().to_string());
    }
    for (agent, url) in ["agent-1", "agent-2"].iter().zip(&urls) {
        assert!(url.ends_with(&format!("/{agent}")), "{url}");
        assert_eq!(count(&p, agent, "SELECT count(*) FROM posts"), "20000");
    }
    assert_eq!(
        p.psql("agent-1", "DELETE FROM posts WHERE id <= 5000").code,
        0
    );
    assert_eq!(count(&p, "agent-1", "SELECT count(*) FROM posts"), "15000");
    assert_eq!(count(&p, "agent-2", "SELECT count(*) FROM posts"), "20000");
    assert_eq!(count(&p, "myapp", "SELECT count(*) FROM posts"), "20000");
    let ls = p.run(&["--json", "ls"]).json();
    let ttl_of = |n: &str| {
        ls.as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == n)
            .unwrap()["ttl"]
            .clone()
    };
    assert_eq!(ttl_of("myapp"), "unlimited");
    assert_eq!(ttl_of("agent-1"), "2h");
    assert_eq!(ttl_of("agent-2"), "2h");
    // The PostgreSQL source was only ever read.
    assert_eq!(src.fingerprint("myapp"), before);
}

#[test]
fn g_ttl_branch_of_a_copy_expires_and_the_template_survives() {
    let src = source_with_app();
    let p = pgboxdb_up();
    assert_eq!(p.copy(&src, "myapp", "tpl", &[]).code, 0);
    assert_eq!(p.run(&["clone", "tpl", "short", "--ttl", "3s"]).code, 0);
    assert!(p.db_exists("short"));
    let deadline = Instant::now() + Duration::from_secs(25);
    while p.db_exists("short") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(400));
    }
    assert!(
        !p.db_exists("short"),
        "TTL branch must expire:\n{}",
        p.log_tail()
    );
    assert!(p.db_exists("tpl"));
    assert_eq!(count(&p, "tpl", "SELECT count(*) FROM posts"), "20000");
    // The template itself is permanent.
    let ls = p.run(&["--json", "ls"]).json();
    let tpl = ls
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "tpl")
        .unwrap();
    assert_eq!(tpl["ttl"], "unlimited");
    assert_eq!(tpl["status"], "ready");
}

#[test]
fn h_crash_after_restore_is_recovered_by_the_next_copy() {
    let src = source_with_app();
    let p = pgboxdb_up();
    // Simulate the process dying right after pg_restore finished (before the
    // destination name exists): the staging database, marker and temp dir stay.
    let r = p.output_retrying_pub(
        || {
            let url = src.url("myapp");
            let mut c = p.cmd(&["copy", "--from", &url, "--to", "tpl"]);
            c.env("PGBOXDB_PG_BINDIR", &src.bindir)
                .env("PGBOXDB_COPY_FAULT", "crash:restored");
            c
        },
        "pgboxdb copy (crash)",
    );
    assert_eq!(r.code, 70, "{}", r.all());
    assert!(!p.db_exists("tpl"), "nothing published");
    let staging = p.staging_dbs();
    assert_eq!(staging.len(), 1, "{staging:?}");
    assert!(!p.copy_leftovers().is_empty());
    let ls = p.run(&["--json", "ls"]).json();
    let row = ls
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == staging[0])
        .unwrap();
    assert_eq!(row["status"], "abandoned");
    // The next copy sweeps the leftovers and succeeds.
    let r = p.copy(&src, "myapp", "tpl", &[]);
    assert_eq!(r.code, 0, "{}", r.all());
    assert!(
        r.stderr.contains("removed staging database"),
        "{}",
        r.stderr
    );
    assert!(p.staging_dbs().is_empty());
    assert!(p.copy_leftovers().is_empty(), "{:?}", p.copy_leftovers());
    assert_eq!(count(&p, "tpl", "SELECT count(*) FROM posts"), "20000");

    // A crash before staging (after export) leaves only a temp dir; swept too.
    let r = p.output_retrying_pub(
        || {
            let url = src.url("myapp");
            let mut c = p.cmd(&["copy", "--from", &url, "--to", "tpl2"]);
            c.env("PGBOXDB_PG_BINDIR", &src.bindir)
                .env("PGBOXDB_COPY_FAULT", "crash:exported");
            c
        },
        "pgboxdb copy (crash)",
    );
    assert_eq!(r.code, 70);
    assert_eq!(p.copy_leftovers().len(), 1, "{:?}", p.copy_leftovers());
    assert_eq!(p.copy(&src, "myapp", "tpl2", &["--schema-only"]).code, 0);
    assert!(p.copy_leftovers().is_empty(), "{:?}", p.copy_leftovers());
}

#[test]
fn h_sigint_during_restore_cleans_up() {
    let src = source_with_app();
    let p = pgboxdb_up();
    let url = src.url("myapp");
    let mut cmd = p.cmd(&["copy", "--from", &url, "--to", "tpl"]);
    cmd.env("PGBOXDB_PG_BINDIR", &src.bindir)
        .env("PGBOXDB_COPY_FAULT", "sleep:restore")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = common::spawn_locked(&mut cmd).expect("spawn pgboxdb copy");
    // Wait until the staging database exists (the copy is in its pause).
    let deadline = Instant::now() + Duration::from_secs(60);
    while p.staging_dbs().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    let staging = p.staging_dbs();
    assert_eq!(
        staging.len(),
        1,
        "staging database expected: {}",
        p.log_tail()
    );
    let ls = p.run(&["--json", "ls"]).json();
    let row = ls
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == staging[0])
        .unwrap();
    assert_eq!(row["status"], "importing");
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let out = child.wait_with_output().unwrap();
    let r = common::Run::from(out);
    assert_eq!(r.code, 1, "{}", r.all());
    assert!(r.stderr.contains("interrupted"), "{}", r.stderr);
    assert!(!p.db_exists("tpl"));
    assert!(p.staging_dbs().is_empty(), "{:?}", p.staging_dbs());
    assert!(p.copy_leftovers().is_empty(), "{:?}", p.copy_leftovers());
    // The runtime is healthy and a retry works.
    assert_eq!(p.copy(&src, "myapp", "tpl", &[]).code, 0);
}

#[test]
fn errors_destination_exists_is_a_conflict_and_changes_nothing() {
    let src = source_with_app();
    let p = pgboxdb_up();
    assert_eq!(
        p.psql(
            "app",
            "CREATE TABLE mine(id int); INSERT INTO mine VALUES (42)"
        )
        .code,
        0
    );
    let r = p.copy(&src, "myapp", "app", &[]);
    assert_eq!(r.code, 4, "{}", r.all());
    assert!(r.stderr.contains("already exists"), "{}", r.stderr);
    assert!(
        r.stderr.contains("app_template"),
        "suggests another name: {}",
        r.stderr
    );
    assert_eq!(
        count(&p, "app", "SELECT id FROM mine"),
        "42",
        "the existing database is untouched"
    );
    assert!(p.copy_leftovers().is_empty());
    let j = p.copy_json(&src, "myapp", "app", &[]);
    assert_eq!(j.json()["error"]["code"], "conflict");
}

#[test]
fn errors_source_problems_are_clear_and_never_leak_the_password() {
    let src = source_with_app();
    let p = pgboxdb_up();
    // Wrong password (scram over TCP, so it is really checked).
    let bad = format!("postgres://postgres:wr0ng-PW@127.0.0.1:{}/myapp", src.port);
    let r = p.copy_args(&src, &["copy", "--from", &bad, "--to", "t"]);
    assert_eq!(r.code, 1, "{}", r.all());
    assert!(r.stderr.contains("authentication failed"), "{}", r.stderr);
    assert!(
        r.stderr.contains("check the user and password"),
        "{}",
        r.stderr
    );
    assert!(
        !r.stderr.contains("wr0ng-PW") && !r.stdout.contains("wr0ng-PW"),
        "password leaked: {}",
        r.all()
    );
    let j = p.copy_args(&src, &["--json", "copy", "--from", &bad, "--to", "t"]);
    assert!(!j.stdout.contains("wr0ng-PW"), "{}", j.stdout);
    assert_eq!(j.json()["error"]["exit_code"], 1);
    // Missing database.
    let r = p.copy(&src, "nosuchdb", "t", &[]);
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("does not exist"), "{}", r.stderr);
    // Unreachable server.
    let dead = "postgres://postgres:x@127.0.0.1:1/myapp";
    let r = p.copy_args(&src, &["copy", "--from", dead, "--to", "t"]);
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("pg_isready"), "{}", r.stderr);
    // Invalid URL and invalid destination are usage errors, before any work.
    let r = p.copy_args(&src, &["copy", "--from", "mysql://h/d", "--to", "t"]);
    assert_eq!(r.code, 2, "{}", r.all());
    let r = p.copy(&src, "myapp", "postgres", &[]);
    assert_eq!(r.code, 2, "{}", r.all());
    let r = p.copy(&src, "myapp", "bad name", &[]);
    assert_eq!(r.code, 2, "{}", r.all());
    let r = p.copy(&src, "myapp", "t", &["--data-only"]);
    assert_eq!(r.code, 2, "{}", r.all());
    assert!(!p.db_exists("t"));
    assert!(p.copy_leftovers().is_empty());
}

#[test]
fn errors_missing_client_tools() {
    let src = source_with_app();
    let p = pgboxdb_up();
    let empty = p.home.join("no-tools");
    std::fs::create_dir_all(&empty).unwrap();
    let url = src.url("myapp");
    let r = p.output_retrying_pub(
        || {
            let mut c = p.cmd(&["copy", "--from", &url, "--to", "t"]);
            c.env("PGBOXDB_PG_BINDIR", &empty);
            c
        },
        "pgboxdb copy",
    );
    assert_eq!(r.code, 1, "{}", r.all());
    assert!(r.stderr.contains("pg_dump"), "{}", r.stderr);
    assert!(r.stderr.contains("PGBOXDB_PG_BINDIR"), "{}", r.stderr);
    assert!(!p.db_exists("t"));
    // doctor reports the tools (found via the real bin dir).
    let d = p.output_retrying_pub(
        || {
            let mut c = p.cmd(&["--json", "doctor"]);
            c.env("PGBOXDB_PG_BINDIR", &src.bindir);
            c
        },
        "pgboxdb doctor",
    );
    let checks = d.json();
    let ct = checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "client_tools")
        .unwrap_or_else(|| panic!("{checks}"))
        .clone();
    assert_eq!(ct["status"], "ok", "{ct}");
}

#[test]
fn errors_unsupported_extension_is_refused_before_anything_is_created() {
    let src = PgSource::start();
    let p = pgboxdb_up();
    // An extension the source PostgreSQL can create but PGBoxDB does not ship.
    let pgboxdb_ext = p
        .psql("postgres", "SELECT name FROM pg_available_extensions")
        .stdout;
    let pgboxdb_set: std::collections::HashSet<&str> = pgboxdb_ext.lines().collect();
    let src_ext = src.psql(
        "postgres",
        "SELECT name FROM pg_available_extensions WHERE name NOT LIKE 'pl%' AND name NOT IN ('adminpack') ORDER BY name",
    );
    let candidate = src_ext
        .stdout
        .lines()
        .find(|n| !pgboxdb_set.contains(n))
        .unwrap_or_else(|| {
            panic!("the source PostgreSQL has no extension PGBoxDB lacks; cannot test the gate")
        })
        .to_string();
    src.create_db("extdb");
    let r = src.psql(
        "extdb",
        &format!("CREATE EXTENSION \"{candidate}\"; CREATE TABLE t(id int)"),
    );
    assert_eq!(r.code, 0, "{}", r.all());
    let r = p.copy(&src, "extdb", "ext_tpl", &[]);
    assert_eq!(r.code, 1, "{}", r.all());
    assert!(r.stderr.contains("unsupported extension"), "{}", r.stderr);
    assert!(r.stderr.contains(&candidate), "{}", r.stderr);
    assert!(
        r.stderr.contains("cannot be copied into PGBoxDB"),
        "{}",
        r.stderr
    );
    assert!(
        !r.stderr.contains("staging"),
        "gate must refuse before staging: {}",
        r.stderr
    );
    assert!(!p.db_exists("ext_tpl"));
    assert!(p.staging_dbs().is_empty());
    assert!(p.copy_leftovers().is_empty());
    let j = p.copy_json(&src, "extdb", "ext_tpl", &[]).json();
    assert_eq!(
        j["error"]["details"]["unsupported_extensions"][0],
        candidate
    );
    assert_eq!(
        j["error"]["details"]["reasons"][0]["rule"],
        "extension_unavailable"
    );
}

#[test]
fn errors_runtime_not_running_or_not_initialised() {
    let src = source_with_app();
    let p = Pgboxdb::new();
    let r = p.copy(&src, "myapp", "t", &[]);
    assert_eq!(r.code, 3, "not initialised: {}", r.all());
    p.init_ok();
    let r = p.copy(&src, "myapp", "t", &[]);
    assert_eq!(r.code, 3, "not running: {}", r.all());
    assert!(r.stderr.contains("pgboxdb start"), "{}", r.stderr);
    p.start_ok();
    assert_eq!(p.copy(&src, "myapp", "t", &[]).code, 0);
}

#[test]
fn help_and_doctor_mention_copy() {
    let p = Pgboxdb::new();
    let h = p.run(&["help"]);
    assert!(h.stdout.contains("pgboxdb copy --from"), "{}", h.stdout);
    assert!(h.stdout.contains("--schema-only"));
}

#[test]
fn b2_schema_only_keeps_migration_bookkeeping_rows() {
    // A schema-only template must still know which migrations ran, or the
    // framework re-runs them on the first branch and fails on existing tables
    // (Rails then purges the database). Bookkeeping tables keep their rows.
    let src = common::PgSource::start();
    src.create_db("bk");
    let q = src.psql(
        "bk",
        "CREATE TABLE users(id serial PRIMARY KEY, email text); INSERT INTO users(email) VALUES ('a'),('b');
         CREATE TABLE schema_migrations(version varchar PRIMARY KEY); INSERT INTO schema_migrations VALUES ('20260101000001'),('20260102000002');
         CREATE TABLE ar_internal_metadata(key varchar PRIMARY KEY, value varchar, created_at timestamp, updated_at timestamp);
         INSERT INTO ar_internal_metadata VALUES ('environment','development',now(),now()),('schema_sha1','abc',now(),now());
         CREATE TABLE django_migrations(id serial PRIMARY KEY, app text, name text, applied timestamptz); INSERT INTO django_migrations(app,name,applied) VALUES ('auth','0001_initial',now());
         CREATE TABLE _prisma_migrations(id text PRIMARY KEY, migration_name text); INSERT INTO _prisma_migrations VALUES ('m1','init');
         CREATE TABLE alembic_version(version_num varchar PRIMARY KEY); INSERT INTO alembic_version VALUES ('abc123');
         CREATE TABLE \"__EFMigrationsHistory\"(\"MigrationId\" varchar PRIMARY KEY, \"ProductVersion\" varchar); INSERT INTO \"__EFMigrationsHistory\" VALUES ('20260101_Init','9.0');
         CREATE SCHEMA \"App.Core\"; CREATE TABLE \"App.Core\".schema_migrations(version varchar PRIMARY KEY); INSERT INTO \"App.Core\".schema_migrations VALUES ('1');",
    );
    assert_eq!(q.code, 0, "{}", q.all());
    let p = Pgboxdb::new();
    p.init_ok();
    p.start_ok();
    let r = p.copy_json(&src, "bk", "bk_tpl", &["--schema-only"]);
    assert_eq!(r.code, 0, "{}", r.all());
    let j = r.json();
    assert_eq!(j["schema_only"], true, "{j}");
    let kept = j["bookkeeping_tables"]
        .as_array()
        .unwrap_or_else(|| panic!("{j}"));
    let names: Vec<&str> = kept.iter().map(|v| v.as_str().unwrap()).collect();
    for t in [
        "schema_migrations",
        "ar_internal_metadata",
        "django_migrations",
        "_prisma_migrations",
        "alembic_version",
    ] {
        assert!(names.contains(&t), "{t} missing from {names:?}");
    }
    assert_eq!(
        p.psql("bk_tpl", "SELECT count(*) FROM users").stdout.trim(),
        "0",
        "no application rows"
    );
    assert_eq!(
        p.psql("bk_tpl", "SELECT count(*) FROM schema_migrations")
            .stdout
            .trim(),
        "2"
    );
    assert_eq!(
        p.psql("bk_tpl", "SELECT count(*) FROM ar_internal_metadata")
            .stdout
            .trim(),
        "2"
    );
    assert_eq!(
        p.psql("bk_tpl", "SELECT count(*) FROM django_migrations")
            .stdout
            .trim(),
        "1"
    );
    assert_eq!(
        p.psql("bk_tpl", "SELECT count(*) FROM _prisma_migrations")
            .stdout
            .trim(),
        "1"
    );
    assert_eq!(
        p.psql("bk_tpl", "SELECT count(*) FROM alembic_version")
            .stdout
            .trim(),
        "1"
    );
    assert_eq!(
        p.psql("bk_tpl", "SELECT count(*) FROM \"__EFMigrationsHistory\"")
            .stdout
            .trim(),
        "1",
        "mixed-case names are quoted for pg_dump"
    );
    assert_eq!(
        p.psql(
            "bk_tpl",
            "SELECT count(*) FROM \"App.Core\".schema_migrations"
        )
        .stdout
        .trim(),
        "1",
        "schemas with dots are quoted for pg_dump"
    );
    // Human output says so.
    let h = p.copy(&src, "bk", "bk_tpl2", &["--schema-only"]);
    assert!(
        h.stdout.contains("migration bookkeeping") || h.stderr.contains("migration bookkeeping"),
        "{}",
        h.all()
    );
}
