//! Cheap change detection for the main database: a schema fingerprint over
//! the catalogs (one round trip per object kind, hashed client side), the
//! migration bookkeeping rows of every framework PGX recognises, and
//! optionally row counts. No dump, no export: a few milliseconds on a typical
//! development database, so it can run on Git events.
//!
//! Pending migrations are found by comparing the migration files committed
//! at main's Git revision (read from Git's object store, not the working
//! tree) with the bookkeeping rows in the database.
use crate::cmd::agent::identity::{fnv1a64, hex16};
use crate::cmd::db_error;
use crate::errors::Result;
use crate::pgwire::quote_ident;
use std::collections::BTreeSet;

type Client = crate::pgwire::Client<std::os::unix::net::UnixStream>;

const USER_NS: &str = "n.nspname NOT IN ('pg_catalog','information_schema','pg_toast') \
     AND n.nspname NOT LIKE 'pg_temp%' AND n.nspname NOT LIKE 'pg_toast_temp%'";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fingerprint {
    pub schema: String,
    pub migrations: String,
    pub data: Option<String>,
    pub tables: u64,
    /// `framework:id` for every applied migration found.
    pub applied: BTreeSet<String>,
    /// Bookkeeping tables found (`rails`, `prisma`, …).
    pub frameworks: Vec<String>,
}

impl Fingerprint {
    pub fn same_as(&self, schema: &str, migrations: &str, data: Option<&str>) -> bool {
        self.schema == schema
            && self.migrations == migrations
            && match (&self.data, data) {
                (Some(a), Some(b)) => a == b,
                (Some(_), None) => false,
                _ => true,
            }
    }
}

fn rows(client: &mut Client, sql: &str) -> Result<Vec<String>> {
    let r = client
        .simple_query(sql)
        .map_err(|e| db_error(e, "cannot read the catalog"))?;
    Ok(r.rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|c| c.unwrap_or_default())
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect())
}

fn hash(items: &[String]) -> String {
    let mut v: Vec<&String> = items.iter().collect();
    v.sort();
    let mut text = String::new();
    for i in v {
        text.push_str(i);
        text.push('\n');
    }
    hex16(fnv1a64(text.as_bytes()))
}

/// (framework, table, SQL selecting the applied ids)
const BOOKKEEPING: [(&str, &str, &str); 9] = [
    (
        "rails",
        "schema_migrations",
        "SELECT version::text FROM {t}",
    ),
    (
        "prisma",
        "_prisma_migrations",
        "SELECT migration_name FROM {t} WHERE finished_at IS NOT NULL AND rolled_back_at IS NULL",
    ),
    (
        "django",
        "django_migrations",
        "SELECT app || '.' || name FROM {t}",
    ),
    ("knex", "knex_migrations", "SELECT name FROM {t}"),
    ("alembic", "alembic_version", "SELECT version_num FROM {t}"),
    (
        "goose",
        "goose_db_version",
        "SELECT version_id::text FROM {t} WHERE is_applied",
    ),
    (
        "flyway",
        "flyway_schema_history",
        "SELECT coalesce(version, script) FROM {t} WHERE success",
    ),
    ("drizzle", "__drizzle_migrations", "SELECT hash FROM {t}"),
    ("sequelize", "SequelizeMeta", "SELECT name FROM {t}"),
];

pub fn compute(client: &mut Client, with_data: bool) -> Result<Fingerprint> {
    let mut schema = Vec::new();
    let tables = rows(
        client,
        &format!(
            "SELECT n.nspname::text, c.relname::text, c.relkind::text FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE {USER_NS} AND c.relkind IN ('r','p','v','m','f','S')"
        ),
    )?;
    let table_count = tables
        .iter()
        .filter(|t| t.ends_with("|r") || t.ends_with("|p"))
        .count() as u64;
    schema.extend(tables.iter().map(|t| format!("rel:{t}")));
    schema.extend(
        rows(
            client,
            &format!(
                "SELECT n.nspname::text, c.relname::text, a.attnum::text, a.attname::text, \
                 format_type(a.atttypid, a.atttypmod), a.attnotnull::text, \
                 coalesce(pg_get_expr(d.adbin, d.adrelid), '') \
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 JOIN pg_attribute a ON a.attrelid = c.oid \
                 LEFT JOIN pg_attrdef d ON d.adrelid = c.oid AND d.adnum = a.attnum \
                 WHERE {USER_NS} AND a.attnum > 0 AND NOT a.attisdropped \
                 AND c.relkind IN ('r','p','v','m','f')"
            ),
        )
        .or_else(|_| {
            // Engines without pg_get_expr on defaults: columns without them.
            rows(
                client,
                &format!(
                    "SELECT n.nspname::text, c.relname::text, a.attnum::text, a.attname::text, \
                     format_type(a.atttypid, a.atttypmod), a.attnotnull::text \
                     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                     JOIN pg_attribute a ON a.attrelid = c.oid \
                     WHERE {USER_NS} AND a.attnum > 0 AND NOT a.attisdropped \
                     AND c.relkind IN ('r','p','v','m','f')"
                ),
            )
        })?
        .into_iter()
        .map(|c| format!("col:{c}")),
    );
    schema.extend(
        rows(
            client,
            "SELECT schemaname::text, indexname::text, indexdef FROM pg_indexes \
             WHERE schemaname NOT IN ('pg_catalog','information_schema')",
        )?
        .into_iter()
        .map(|c| format!("idx:{c}")),
    );
    schema.extend(
        rows(
            client,
            &format!(
                "SELECT n.nspname::text, co.conname::text, pg_get_constraintdef(co.oid) \
                 FROM pg_constraint co JOIN pg_namespace n ON n.oid = co.connamespace WHERE {USER_NS}"
            ),
        )?
        .into_iter()
        .map(|c| format!("con:{c}")),
    );
    if let Ok(fns) = rows(
        client,
        &format!(
            "SELECT n.nspname::text, p.proname::text, md5(p.prosrc) FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace WHERE {USER_NS}"
        ),
    ) {
        schema.extend(fns.into_iter().map(|c| format!("fn:{c}")));
    }
    if let Ok(types) = rows(
        client,
        &format!(
            "SELECT n.nspname::text, t.typname::text, t.typtype::text FROM pg_type t \
             JOIN pg_namespace n ON n.oid = t.typnamespace \
             WHERE {USER_NS} AND t.typtype IN ('e','d','c') \
             AND NOT EXISTS (SELECT 1 FROM pg_class c WHERE c.reltype = t.oid AND c.relkind <> 'c')"
        ),
    ) {
        schema.extend(types.into_iter().map(|c| format!("type:{c}")));
    }
    if let Ok(ext) = rows(client, "SELECT extname::text, extversion FROM pg_extension") {
        schema.extend(ext.into_iter().map(|c| format!("ext:{c}")));
    }

    // Migration bookkeeping: whichever known tables exist.
    let present: BTreeSet<String> = rows(
        client,
        &format!(
            "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE {USER_NS} AND c.relkind IN ('r','p') AND c.relname IN ({})",
            BOOKKEEPING
                .iter()
                .map(|(_, t, _)| crate::pgwire::quote_literal(t))
                .collect::<Vec<_>>()
                .join(",")
        ),
    )?
    .into_iter()
    .collect();
    let mut applied = BTreeSet::new();
    let mut frameworks = Vec::new();
    for (fw, table, sql) in BOOKKEEPING {
        if !present.contains(table) {
            continue;
        }
        let q = sql.replace("{t}", &quote_ident(table));
        // A bookkeeping table with an unexpected shape is hashed whole instead.
        let ids = match rows(client, &q) {
            Ok(v) => v,
            Err(_) => rows(
                client,
                &format!("SELECT t::text FROM {} t", quote_ident(table)),
            )
            .unwrap_or_default(),
        };
        frameworks.push(fw.to_string());
        for id in ids {
            applied.insert(format!("{fw}:{id}"));
        }
    }
    let migrations = hash(&applied.iter().cloned().collect::<Vec<_>>());

    let data = if with_data {
        let mut counts = Vec::new();
        for t in &tables {
            let parts: Vec<&str> = t.split('|').collect();
            if parts.len() == 3 && (parts[2] == "r" || parts[2] == "p") {
                let n = client
                    .simple_query(&format!(
                        "SELECT count(*) FROM {}.{}",
                        quote_ident(parts[0]),
                        quote_ident(parts[1])
                    ))
                    .ok()
                    .and_then(|r| r.scalar().map(String::from))
                    .unwrap_or_default();
                counts.push(format!("{}.{}={n}", parts[0], parts[1]));
            }
        }
        Some(hash(&counts))
    } else {
        None
    };
    Ok(Fingerprint {
        schema: hash(&schema),
        migrations,
        data,
        tables: table_count,
        applied,
        frameworks,
    })
}

/// Migrations committed at a revision, as `framework:id` (the same ids the
/// bookkeeping tables hold). Only the frameworks PGX recognises by layout.
pub fn committed_migrations(files: &[String]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for f in files {
        let parts: Vec<&str> = f.split('/').collect();
        let name = *parts.last().unwrap_or(&"");
        let parent = if parts.len() >= 2 {
            parts[parts.len() - 2]
        } else {
            ""
        };
        // Rails: db/migrate/20240101000000_create_users.rb
        if parent == "migrate"
            && parts.len() >= 2
            && f.contains("db/migrate/")
            && name.ends_with(".rb")
        {
            let v: String = name.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !v.is_empty() {
                out.insert(format!("rails:{v}"));
            }
            continue;
        }
        // Prisma: prisma/migrations/<name>/migration.sql
        if name == "migration.sql" && parts.len() >= 3 && parts[parts.len() - 3] == "migrations" {
            out.insert(format!("prisma:{parent}"));
            continue;
        }
        // Django: <app>/migrations/0001_initial.py
        if parent == "migrations"
            && name.ends_with(".py")
            && name != "__init__.py"
            && name.chars().next().is_some_and(|c| c.is_ascii_digit())
            && parts.len() >= 3
        {
            let app = parts[parts.len() - 3];
            out.insert(format!("django:{app}.{}", name.trim_end_matches(".py")));
            continue;
        }
        // golang-migrate / dbmate style: migrations/<version>_<name>.up.sql
        if parent == "migrations" && name.ends_with(".up.sql") {
            let v: String = name.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !v.is_empty() {
                out.insert(format!("rails:{v}"));
            }
        }
    }
    out
}

/// The committed migrations a database has not applied, limited to the
/// frameworks whose files exist at the revision. A framework whose
/// bookkeeping table is missing entirely counts every file as pending.
pub fn pending(committed: &BTreeSet<String>, fp: &Fingerprint) -> Vec<String> {
    committed
        .iter()
        .filter(|m| !fp.applied.contains(*m))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_migrations_by_layout() {
        let files: Vec<String> = [
            "db/migrate/20240101000000_create_users.rb",
            "db/migrate/20240202000000_add_email.rb",
            "db/schema.rb",
            "prisma/migrations/20240101_init/migration.sql",
            "prisma/migrations/migration_lock.toml",
            "blog/migrations/0001_initial.py",
            "blog/migrations/__init__.py",
            "migrations/000002_add_x.up.sql",
            "migrations/000002_add_x.down.sql",
            "README.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let m = committed_migrations(&files);
        let want: BTreeSet<String> = [
            "rails:20240101000000",
            "rails:20240202000000",
            "prisma:20240101_init",
            "django:blog.0001_initial",
            "rails:000002",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(m, want);
        let fp = Fingerprint {
            applied: ["rails:20240101000000".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let p = pending(&m, &fp);
        assert!(p.contains(&"rails:20240202000000".to_string()));
        assert!(!p.contains(&"rails:20240101000000".to_string()));
    }

    #[test]
    fn hashing_is_order_independent() {
        assert_eq!(
            hash(&["a".into(), "b".into()]),
            hash(&["b".into(), "a".into()])
        );
        assert_ne!(hash(&["a".into()]), hash(&["b".into()]));
    }
}
