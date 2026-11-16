//! `pgboxdb copy --from <postgres url> --to <database>`: a logical copy of an
//! external PostgreSQL database into this PGBoxDB runtime, as a template to
//! branch from with `pgboxdb clone`.
//!
//! Stages (docs/superpowers/specs/2026-10-24-pgboxdb-copy-design.md):
//! client tools → schema dump + compatibility gate → export (one snapshot) →
//! staging database → restore (`--exit-on-error`) → validate → optimize
//! (`VACUUM (FREEZE, ANALYZE)`) → atomic rename to the destination.
//!
//! The source is only ever read (by pg_dump). A failure at any point drops
//! the staging database and the temporary dump, so the destination name is
//! either absent or a complete, validated copy.
use super::{db_error, Ctx};
use crate::cli::ExitCode;
use crate::conninfo::{self, Redactor, Source};
use crate::errors::{PgboxdbError, Result};
use crate::layout::Home;
use crate::names::{self, DbName};
use crate::out::Out;
use crate::pgtools::{self, Tools};
use crate::pgwire::quote_literal;
use crate::toc::{self, Inventory, Toc};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

type Client = crate::pgwire::Client<std::os::unix::net::UnixStream>;

pub const STAGING_PREFIX: &str = "_pgboxdbcopy_";
const LONG_TIMEOUT: Duration = Duration::from_secs(7 * 24 * 3600);

/// Migration bookkeeping tables whose rows a `--schema-only` import keeps, so a
/// framework sees its migrations as applied. Names as the frameworks create them.
pub const BOOKKEEPING_TABLES: [&str; 20] = [
    "schema_migrations",
    "ar_internal_metadata",
    "django_migrations",
    "_prisma_migrations",
    "alembic_version",
    "flyway_schema_history",
    "schema_version",
    "knex_migrations",
    "knex_migrations_lock",
    "goose_db_version",
    "migrations",
    "typeorm_metadata",
    "__drizzle_migrations",
    "sequelizemeta",
    "phinxlog",
    "atlas_schema_revisions",
    "__EFMigrationsHistory",
    "mikro_orm_migrations",
    "dbmate_schema_migrations",
    "schema_info",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opts {
    pub from: String,
    pub to: String,
    pub schema_only: bool,
    pub jobs: Option<u32>,
    pub verbose: bool,
    pub optimize: bool,
}

// ---------------------------------------------------------------- markers

/// `$PGBOXDB_HOME/copy/<staging>.json`: who is importing what. Lets the next
/// `pgboxdb copy` remove a staging database left by a killed process and lets
/// `pgboxdb ls` label staging databases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub staging: String,
    pub destination: String,
    pub pid: u32,
    pub tmp_dir: PathBuf,
    pub started_at: String,
    pub path: PathBuf,
}

impl Marker {
    pub fn owner_alive(&self) -> bool {
        crate::process::pid_alive(self.pid)
    }
}

pub fn markers_dir(home: &Home) -> PathBuf {
    home.root.join("copy")
}

pub fn markers(home: &Home) -> Vec<Marker> {
    let dir = markers_dir(home);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let staging = s("staging");
        if !staging.starts_with(STAGING_PREFIX) {
            continue;
        }
        out.push(Marker {
            staging,
            destination: s("destination"),
            pid: v.get("pid").and_then(|p| p.as_u64()).unwrap_or(0) as u32,
            tmp_dir: PathBuf::from(s("tmp_dir")),
            started_at: s("started_at"),
            path,
        });
    }
    out.sort_by(|a, b| a.staging.cmp(&b.staging));
    out
}

fn write_marker(home: &Home, m: &Marker) -> Result<()> {
    let dir = markers_dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| PgboxdbError::io("cannot create", &dir, &e))?;
    let v = json!({
        "staging": m.staging,
        "destination": m.destination,
        "pid": m.pid,
        "tmp_dir": m.tmp_dir,
        "started_at": m.started_at,
    });
    let tmp = m.path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&v).expect("marker"))
        .map_err(|e| PgboxdbError::io("cannot write", &tmp, &e))?;
    std::fs::rename(&tmp, &m.path).map_err(|e| PgboxdbError::io("cannot rename", &m.path, &e))
}

/// Drop staging databases whose importing process is gone. Safe by
/// construction: only names with the staging prefix that a marker names, and
/// only when the recorded pid is not alive.
fn sweep_abandoned(ctx: &Ctx, client: &mut Client, report: &mut Report) -> Result<()> {
    // Temp dirs (`tmp/copy-<pid>-<rand>`) of processes that are gone: a copy
    // killed before it had a staging database leaves only this behind.
    if let Ok(rd) = std::fs::read_dir(ctx.home.root.join("tmp")) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(rest) = name.strip_prefix("copy-") else {
                continue;
            };
            let pid: u32 = rest
                .split('-')
                .next()
                .and_then(|p| p.parse().ok())
                .unwrap_or(0);
            if pid != std::process::id() && !crate::process::pid_alive(pid) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    for m in markers(&ctx.home) {
        if m.owner_alive() {
            continue;
        }
        let name = match names::validate(&m.staging) {
            Ok(n) => n,
            Err(_) => continue,
        };
        let sql = format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", name.quoted());
        match client.simple_query(&sql) {
            Ok(_) => {
                if m.tmp_dir.starts_with(ctx.home.root.join("tmp")) {
                    let _ = std::fs::remove_dir_all(&m.tmp_dir);
                }
                let _ = std::fs::remove_file(&m.path);
                report.warn(format!(
                    "removed staging database {} left by an interrupted copy to {} (pid {})",
                    m.staging, m.destination, m.pid
                ));
            }
            Err(e) => {
                report.warn(format!(
                    "could not remove abandoned staging database {}: {e}",
                    m.staging
                ));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- reporting

struct Stage {
    name: &'static str,
    detail: String,
    ms: u64,
}

struct Report<'a> {
    out: &'a Out,
    verbose: bool,
    tty: bool,
    stages: Vec<Stage>,
    warnings: Vec<String>,
    progress_shown: bool,
}

impl<'a> Report<'a> {
    fn new(out: &'a Out, verbose: bool) -> Report<'a> {
        Report {
            out,
            verbose,
            tty: !out.json && unsafe { libc::isatty(2) } == 1,
            stages: Vec::new(),
            warnings: Vec::new(),
            progress_shown: false,
        }
    }
    fn clear_progress(&mut self) {
        if self.progress_shown {
            eprint!("\r\x1b[2K");
            self.progress_shown = false;
        }
    }
    /// A transient line on a terminal (overwritten by the next one).
    fn progress(&mut self, text: &str) {
        if self.tty {
            eprint!("\r\x1b[2K  … {text}");
            use std::io::Write;
            let _ = std::io::stderr().flush();
            self.progress_shown = true;
        }
    }
    fn done(&mut self, name: &'static str, detail: impl Into<String>, started: Instant) {
        let detail = detail.into();
        let ms = started.elapsed().as_millis() as u64;
        self.clear_progress();
        self.out.note(&format!("  ✓ {name:<13} {detail}"));
        self.stages.push(Stage { name, detail, ms });
    }
    fn warn(&mut self, text: String) {
        self.clear_progress();
        self.out.note(&format!("  ! {text}"));
        self.warnings.push(text);
    }
    /// A line from a child tool, shown only with --verbose.
    fn tool_line(&mut self, line: &str) {
        if self.verbose && !self.out.json {
            self.clear_progress();
            eprintln!("    {line}");
        }
    }
    fn stages_json(&self) -> serde_json::Value {
        json!(self
            .stages
            .iter()
            .map(|s| json!({"name": s.name, "detail": s.detail, "ms": s.ms}))
            .collect::<Vec<_>>())
    }
}

fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

fn plural(n: u64, word: &str) -> String {
    match (n, word) {
        (1, _) => format!("1 {word}"),
        (_, "index") => format!("{n} indexes"),
        _ => format!("{n} {word}s"),
    }
}

fn secs(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 10.0 {
        format!("{s:.1} s")
    } else {
        format!("{s:.0} s")
    }
}

// ---------------------------------------------------------------- staging guard

/// Owns the staging database, its marker and the temp dir. Dropped on every
/// exit path; unless `published`, it removes all three (best effort: a failed
/// DROP leaves the marker so the next `pgboxdb copy` retries).
struct Staging<'a> {
    ctx: &'a Ctx,
    name: DbName,
    marker: Marker,
    published: bool,
}

impl Drop for Staging<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.marker.tmp_dir);
        if self.published {
            let _ = std::fs::remove_file(&self.marker.path);
            return;
        }
        let dropped = match self.ctx.connect("postgres") {
            Ok(mut c) => {
                let r = c.simple_query(&format!(
                    "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                    self.name.quoted()
                ));
                c.close();
                r.is_ok()
            }
            Err(_) => false,
        };
        if dropped {
            let _ = std::fs::remove_file(&self.marker.path);
        }
    }
}

// ---------------------------------------------------------------- helpers

fn exists(client: &mut Client, name: &str) -> Result<bool> {
    let r = client
        .simple_query(&format!(
            "SELECT 1 FROM pg_database WHERE datname = {}",
            quote_literal(name)
        ))
        .map_err(|e| db_error(e, "cannot query pg_database"))?;
    Ok(!r.rows.is_empty())
}

fn random_suffix(n: usize) -> Result<String> {
    Ok(crate::fsutil::random_password()?[..n].to_ascii_lowercase())
}

/// `_pgboxdbcopy_<dest>_<6 random>`; the destination part is clipped so the whole
/// name stays under 63 bytes and passes the CLI's name rules.
pub fn staging_name(dest: &str, suffix: &str) -> String {
    let max_dest = 63 - STAGING_PREFIX.len() - 1 - suffix.len();
    let clipped: String = dest.chars().take(max_dest).collect();
    format!("{STAGING_PREFIX}{clipped}_{suffix}")
}

fn make_tmp_dir(home: &Home) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let base = home.root.join("tmp");
    std::fs::create_dir_all(&base).map_err(|e| PgboxdbError::io("cannot create", &base, &e))?;
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| PgboxdbError::io("cannot chmod", &base, &e))?;
    let dir = base.join(format!("copy-{}-{}", std::process::id(), random_suffix(6)?));
    std::fs::create_dir(&dir).map_err(|e| PgboxdbError::io("cannot create", &dir, &e))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| PgboxdbError::io("cannot chmod", &dir, &e))?;
    Ok(dir)
}

fn file_size(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// Test hook: `PGBOXDB_COPY_FAULT=crash:<stage>` exits abruptly (no cleanup) right
/// after the stage, `sleep:<stage>` pauses before it. Used by the cancellation
/// and recovery tests; never set in normal use.
fn fault(point: &str) {
    if let Ok(v) = std::env::var("PGBOXDB_COPY_FAULT") {
        if v == format!("crash:{point}") {
            std::process::exit(70);
        }
        if v == format!("sleep:{point}") {
            let deadline = Instant::now() + Duration::from_secs(120);
            while Instant::now() < deadline && !pgtools::interrupted() {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn check_interrupted() -> Result<()> {
    if pgtools::interrupted() {
        Err(PgboxdbError::failure("interrupted"))
    } else {
        Ok(())
    }
}

/// Explain a failed pg_dump from its last lines (already redacted).
fn dump_error(source: &Source, tail: &[String], status: &str) -> PgboxdbError {
    let text = tail.join("\n");
    let last = tail
        .iter()
        .rev()
        .find(|l| l.contains("error:") || l.contains("FATAL") || l.contains("failed"))
        .or(tail.last())
        .cloned()
        .unwrap_or_else(|| format!("pg_dump exited with {status}"));
    let base = PgboxdbError::failure(format!(
        "cannot read the source database ({}): {last}",
        source.display
    ));
    if text.contains("password authentication failed")
        || text.contains("no password supplied")
        || text.contains("authentication failed")
    {
        base.hint("check the user and password in --from (a password with special characters must be percent-encoded in a URL)")
    } else if text.contains("Connection refused")
        || text.contains("could not connect")
        || text.contains("could not translate host name")
        || text.contains("timeout expired")
        || text.contains("No such file or directory")
    {
        base.hint("is PostgreSQL running and listening on that host and port? `pg_isready -h <host> -p <port>`")
    } else if text.contains("does not exist") && text.contains("database") {
        base.hint(
            "the database named in --from does not exist on that server; `psql -l` lists databases",
        )
    } else if text.contains("server version mismatch")
        || text.contains("aborting because of server version mismatch")
    {
        base.hint("pg_dump must be at least as new as the source server; install a newer PostgreSQL client and point PGBOXDB_PG_BINDIR at its bin directory")
    } else if text.contains("no pg_hba.conf entry") {
        base.hint("the PostgreSQL server rejects this client (pg_hba.conf); check host, SSL settings and user")
    } else if text.contains("permission denied") {
        base.hint("the source user lacks read access to some objects; use a role that can read the whole database")
    } else if text.contains("No space left on device") {
        base.hint(
            "the dump needs free space under $PGBOXDB_HOME/tmp; free disk space or use --schema-only",
        )
    } else if text.contains("SSL") || text.contains("ssl") {
        base.hint(
            "check the SSL settings of --from (sslmode=…); PGBoxDB passes them to pg_dump unchanged",
        )
    } else {
        base.hint("run with --verbose to see the full pg_dump output")
    }
}

/// Explain a failed pg_restore: the failing object and the server error.
fn restore_error(tail: &[String], status: &str) -> PgboxdbError {
    let errors: Vec<&String> = tail
        .iter()
        .filter(|l| {
            l.contains("error:")
                || l.contains("ERROR:")
                || l.contains("DETAIL:")
                || l.contains("HINT:")
                || l.contains("Command was:")
                || l.contains("No space left")
        })
        .collect();
    let detail = if errors.is_empty() {
        tail.iter()
            .rev()
            .take(5)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n  ")
    } else {
        errors
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    };
    let mut e = PgboxdbError::failure(format!(
        "restore into PGBoxDB failed ({status}); the staging database was removed and nothing was published:\n  {detail}"
    ))
    .details(json!({"pg_restore": tail}));
    if detail.contains("No space left") {
        e = e.hint("free disk space under $PGBOXDB_HOME and retry");
    } else if detail.contains("role") && detail.contains("does not exist") {
        e = e.hint("the schema refers to a role that does not exist in PGBoxDB (for example in a policy); create it first with psql \"$(pgboxdb url postgres)\" -c 'CREATE ROLE <name>' and retry");
    } else if detail.contains("could not access file") || detail.contains("could not load library")
    {
        e = e.hint("a function needs a C library PGBoxDB does not have; PGBoxDB runs PL/pgSQL and SQL functions and its built-in extensions only");
    } else {
        e = e.hint("PGBoxDB could not create one of the source's objects; see docs/pgboxdb/local-dev/copy.md (Limitations) or run with --verbose");
    }
    e
}

// ---------------------------------------------------------------- the command

pub fn run(out: &Out, opts: Opts) -> Result<ExitCode> {
    let r = run_inner(out, opts);
    match r {
        // Whatever failed first after Ctrl-C (a killed tool, a closed
        // session), the user asked to stop: say so, once cleanup has run.
        Err(_) if pgtools::interrupted() => Err(PgboxdbError::failure("interrupted")),
        other => other,
    }
}

fn run_inner(out: &Out, opts: Opts) -> Result<ExitCode> {
    let t_all = Instant::now();
    // 1. Validate everything that needs no runtime first.
    let dest = names::validate_user_target(&opts.to)?;
    let source = conninfo::parse(&opts.from)?;
    if let Some(j) = opts.jobs {
        if j == 0 || j > 64 {
            return Err(PgboxdbError::usage(format!(
                "--jobs must be between 1 and 64, got {j}"
            )));
        }
    }
    // Mask the password, the ssl passphrase and the raw --from string itself,
    // so nothing a tool echoes can carry a credential to the terminal or JSON.
    let redactor = Redactor::with_secrets(&[
        source.password.as_deref(),
        source.ssl_password.as_deref(),
        Some(opts.from.as_str()),
    ]);
    let ctx = Ctx::load()?;
    let mut client = ctx.connect("postgres")?;
    let mut report = Report::new(out, opts.verbose);
    pgtools::install_interrupt_handler();

    out.note(&format!(
        "copying PostgreSQL database {} into PGBoxDB as {dest}{}",
        source.display,
        if opts.schema_only {
            " (schema only)"
        } else {
            ""
        }
    ));

    // 2. Client tools.
    let t = Instant::now();
    let tools: Tools = pgtools::locate()?;
    report.done(
        "client tools",
        format!("pg_dump {} ({})", tools.version, tools.dir.display()),
        t,
    );

    // 3. Clean up after an earlier copy that died, then refuse an existing destination.
    sweep_abandoned(&ctx, &mut client, &mut report)?;
    if exists(&mut client, dest.as_str())? {
        return Err(PgboxdbError::new(
            ExitCode::Conflict,
            format!("database {dest} already exists"),
        )
        .hint(format!(
            "use another name (for example --to {dest}_template), or drop it first with `pgboxdb drop {dest}` if you are sure"
        )));
    }

    // 4. What the engine offers.
    let pgboxdb_major: u32 = client
        .simple_query("SHOW server_version_num")
        .ok()
        .and_then(|r| r.scalar().and_then(|s| s.parse::<u32>().ok()))
        .map(|n| n / 10000)
        .unwrap_or(18);
    let available_extensions: Vec<String> = client
        .simple_query("SELECT name FROM pg_available_extensions ORDER BY name")
        .map_err(|e| db_error(e, "cannot list PGBoxDB extensions"))?
        .rows
        .iter()
        .filter_map(|r| r.first().cloned().flatten())
        .collect();

    // 5. Temp dir for the dumps.
    let tmp_dir = make_tmp_dir(&ctx.home)?;
    // Removed on every path by the staging guard; until the guard exists, here.
    struct TmpGuard(Option<PathBuf>);
    impl Drop for TmpGuard {
        fn drop(&mut self) {
            if let Some(p) = &self.0 {
                let _ = std::fs::remove_dir_all(p);
            }
        }
    }
    let mut tmp_guard = TmpGuard(Some(tmp_dir.clone()));

    let pg_dump = |args: &[&str], file: &Path| -> Command {
        let mut c = Command::new(&tools.pg_dump);
        c.arg("--format=custom")
            .arg("--no-sync")
            .args(args)
            .arg("--file")
            .arg(file)
            .arg("--")
            .arg(&source.arg)
            .env("PGAPPNAME", "pgboxdb copy");
        if std::env::var_os("PGCONNECT_TIMEOUT").is_none() {
            c.env("PGCONNECT_TIMEOUT", "20");
        }
        if let Some(p) = &source.password {
            c.env("PGPASSWORD", p);
        }
        if let Some(p) = &source.ssl_password {
            c.env("PGSSLPASSWORD", p);
        }
        c
    };

    // 6. Schema dump: cheap, and everything the gate needs.
    let t = Instant::now();
    let schema_dump = tmp_dir.join("schema.dump");
    let mut dump_lines: Vec<String> = Vec::new();
    {
        let mut cmd = pg_dump(&["--schema-only"], &schema_dump);
        report.progress("connecting to the source");
        let fin = pgtools::run_streaming(
            &mut cmd,
            &redactor,
            |l| dump_lines.push(l.to_string()),
            || {},
        )?;
        for l in &dump_lines {
            report.tool_line(l);
        }
        if !fin.success {
            return Err(dump_error(&source, &fin.tail, &fin.status_text));
        }
    }
    let schema_toc = read_toc(&tools, &schema_dump, &redactor)?;
    report.done(
        "source",
        format!(
            "PostgreSQL {} · {}",
            schema_toc.source_version.as_deref().unwrap_or("?"),
            source.display
        ),
        t,
    );

    // 7. Compatibility gate.
    let t = Instant::now();
    let inv = schema_toc.inventory();
    let gate = toc::evaluate(&inv, &available_extensions, pgboxdb_major);
    for w in toc::version_warnings(&schema_toc, pgboxdb_major) {
        report.warn(w);
    }
    if !gate.compatible() {
        let mut lines = vec![format!(
            "PostgreSQL database {} cannot be copied into PGBoxDB:",
            source.display
        )];
        let unsupported_ext: Vec<&str> = gate
            .reasons
            .iter()
            .filter(|r| r.rule == "extension_unavailable")
            .map(|r| r.object.as_str())
            .collect();
        if !unsupported_ext.is_empty() {
            lines.push(format!(
                "  unsupported extension{}: {}",
                if unsupported_ext.len() > 1 { "s" } else { "" },
                unsupported_ext.join(", ")
            ));
        }
        for r in gate
            .reasons
            .iter()
            .filter(|r| r.rule != "extension_unavailable")
        {
            lines.push(format!("  {}: {} ({})", r.rule, r.object, r.detail));
        }
        lines.push("PGBoxDB cannot restore this database; nothing was created.".into());
        return Err(PgboxdbError::failure(lines.join("\n"))
            .hint("PGBoxDB ships a fixed set of built-in extensions (docs/pgboxdb/local-dev/copy.md lists them); drop the extension in a copy of the source, or keep this database on PostgreSQL")
            .details(json!({
                "reasons": gate.reasons.iter().map(|r| json!({"rule": r.rule, "object": r.object, "detail": r.detail})).collect::<Vec<_>>(),
                "unsupported_extensions": unsupported_ext,
                "source_version": schema_toc.source_version,
            })));
    }
    for w in &gate.warnings {
        report.warn(w.clone());
    }
    report.done(
        "compatibility",
        format!(
            "{} extension{} ({}), {} objects",
            inv.extensions.len(),
            if inv.extensions.len() == 1 { "" } else { "s" },
            if inv.extensions.is_empty() {
                "none".to_string()
            } else {
                inv.extensions.join(", ")
            },
            schema_toc.entries.len()
        ),
        t,
    );
    check_interrupted()?;

    // 8. Export: one full dump (a single snapshot) unless --schema-only.
    let t = Instant::now();
    let archive: PathBuf;
    let archive_toc: Toc;
    if opts.schema_only {
        archive = schema_dump.clone();
        archive_toc = schema_toc.clone();
        report.done(
            "export",
            format!("schema only, {}", human_bytes(file_size(&archive))),
            t,
        );
    } else {
        archive = tmp_dir.join("full.dump");
        let mut cmd = pg_dump(&[], &archive);
        let mut lines: Vec<String> = Vec::new();
        let archive_for_tick = archive.clone();
        let mut last_shown = Instant::now() - Duration::from_secs(1);
        let fin = {
            let report = &mut report;
            pgtools::run_streaming(
                &mut cmd,
                &redactor,
                |l| lines.push(l.to_string()),
                || {
                    if last_shown.elapsed() >= Duration::from_millis(500) {
                        last_shown = Instant::now();
                        report.progress(&format!(
                            "exporting schema and data: {} written",
                            human_bytes(file_size(&archive_for_tick))
                        ));
                    }
                },
            )?
        };
        for l in &lines {
            report.tool_line(l);
        }
        if !fin.success {
            return Err(dump_error(&source, &fin.tail, &fin.status_text));
        }
        archive_toc = read_toc(&tools, &archive, &redactor)?;
        report.done(
            "export",
            format!(
                "schema and data, {} in {}",
                human_bytes(file_size(&archive)),
                secs(t.elapsed())
            ),
            t,
        );
    }
    let expected = archive_toc.inventory();
    fault("exported");
    check_interrupted()?;

    // 9. Staging database.
    let t = Instant::now();
    let staging_name = staging_name(dest.as_str(), &random_suffix(6)?);
    let staging_db = names::validate(&staging_name)?;
    let marker = Marker {
        staging: staging_name.clone(),
        destination: dest.as_str().to_string(),
        pid: std::process::id(),
        tmp_dir: tmp_dir.clone(),
        started_at: crate::fsutil::now_rfc3339(),
        path: markers_dir(&ctx.home).join(format!("{staging_name}.json")),
    };
    write_marker(&ctx.home, &marker)?;
    tmp_guard.0 = None; // the staging guard owns the temp dir from here
    let mut staging = Staging {
        ctx: &ctx,
        name: staging_db.clone(),
        marker,
        published: false,
    };
    client
        .simple_query(&format!("CREATE DATABASE {}", staging_db.quoted()))
        .map_err(|e| db_error(e, "cannot create the staging database"))?;
    report.done("staging", format!("database {staging_name}"), t);

    // 10. Restore, stopping at the first error.
    let t = Instant::now();
    fault("restore");
    check_interrupted()?;
    {
        let mut cmd = Command::new(&tools.pg_restore);
        cmd.args([
            "--exit-on-error",
            "--no-owner",
            "--no-privileges",
            "--no-tablespaces",
            "--no-subscriptions",
            "--no-publications",
            "--no-security-labels",
            "--verbose",
        ]);
        if let Some(j) = opts.jobs {
            cmd.arg(format!("--jobs={j}"));
        }
        cmd.arg("--host")
            .arg(ctx.sock_dir())
            .arg("--port")
            .arg(ctx.config.port.to_string())
            .args(["--username", "postgres", "--no-password", "--dbname"])
            .arg(staging_db.as_str())
            .arg("--")
            .arg(&archive)
            .env("PGAPPNAME", "pgboxdb copy");
        // The restore targets the local engine over its socket; no ambient
        // libpq setting may redirect or alter it.
        for var in [
            "PGPASSWORD",
            "PGSSLPASSWORD",
            "PGOPTIONS",
            "PGSERVICE",
            "PGSERVICEFILE",
            "PGHOST",
            "PGHOSTADDR",
            "PGPORT",
            "PGUSER",
            "PGDATABASE",
            "PGSSLMODE",
            "PGREQUIRESSL",
            "PGPASSFILE",
            "PGTARGETSESSIONATTRS",
            "PGGSSENCMODE",
        ] {
            cmd.env_remove(var);
        }
        let total = archive_toc.entries.len();
        let mut done_count = 0usize;
        let mut verbose_lines: Vec<String> = Vec::new();
        // The first error and what follows it, kept apart from the rolling
        // tail so a long "Command was:" body cannot push the cause out.
        let mut error_block: Vec<String> = Vec::new();
        let fin = {
            let report = &mut report;
            let verbose = opts.verbose;
            pgtools::run_streaming(
                &mut cmd,
                &redactor,
                |l| {
                    if let Some(rest) = l.strip_prefix("pg_restore: ") {
                        if rest.starts_with("creating ")
                            || rest.starts_with("processing data for ")
                            || rest.starts_with("executing ")
                        {
                            done_count += 1;
                            report.progress(&format!("restoring {done_count}/{total}: {rest}"));
                        }
                    }
                    if verbose
                        || l.contains("error")
                        || l.contains("ERROR")
                        || l.contains("warning")
                    {
                        verbose_lines.push(l.to_string());
                    }
                    if (!error_block.is_empty() && error_block.len() < 60)
                        || l.contains("error:")
                        || l.contains("ERROR:")
                    {
                        error_block.push(l.to_string());
                    }
                },
                || {},
            )?
        };
        for l in &verbose_lines {
            report.tool_line(l);
        }
        if !fin.success {
            let mut lines = error_block;
            if lines.is_empty() {
                lines = fin.tail.clone();
            }
            return Err(restore_error(&lines, &fin.status_text));
        }
    }
    report.done(
        "restore",
        format!(
            "{} objects in {}{}",
            archive_toc.entries.len(),
            secs(t.elapsed()),
            opts.jobs
                .map(|j| format!(" ({j} jobs)"))
                .unwrap_or_default()
        ),
        t,
    );
    fault("restored");
    check_interrupted()?;

    // 10b. Schema-only: the framework's migration bookkeeping rows travel with
    // the schema, or the first branch re-runs every migration and fails on
    // tables that already exist (Rails then purges the database).
    let mut bookkeeping: Vec<String> = Vec::new();
    if opts.schema_only {
        let present: Vec<(Option<String>, String)> = schema_toc
            .entries
            .iter()
            .filter(|e| e.desc == "TABLE" && BOOKKEEPING_TABLES.contains(&e.name.as_str()))
            .map(|e| (e.schema.clone(), e.name.clone()))
            .collect();
        if !present.is_empty() {
            let t = Instant::now();
            let data_dump = tmp_dir.join("bookkeeping.dump");
            // `--table` takes a psql-style pattern: unquoted names are
            // lower-cased and `.` is a separator, so every part is quoted,
            // and --strict-names makes a non-match loud instead of silent.
            let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
            let mut args: Vec<String> =
                vec!["--data-only".to_string(), "--strict-names".to_string()];
            for (schema, name) in &present {
                args.push(match schema {
                    Some(sch) => format!("--table={}.{}", q(sch), q(name)),
                    None => format!("--table={}", q(name)),
                });
            }
            let arg_refs: Vec<&str> = args.iter().map(|a| a.as_str()).collect();
            let mut cmd = pg_dump(&arg_refs, &data_dump);
            let fin = pgtools::run_streaming(&mut cmd, &redactor, |_| {}, || {})?;
            if !fin.success {
                return Err(dump_error(&source, &fin.tail, &fin.status_text));
            }
            let mut cmd = Command::new(&tools.pg_restore);
            cmd.args([
                "--data-only",
                "--exit-on-error",
                "--no-owner",
                "--no-privileges",
            ])
            .arg("--host")
            .arg(ctx.sock_dir())
            .arg("--port")
            .arg(ctx.config.port.to_string())
            .args(["--username", "postgres", "--no-password", "--dbname"])
            .arg(staging_db.as_str())
            .arg("--")
            .arg(&data_dump)
            .env("PGAPPNAME", "pgboxdb copy");
            for var in [
                "PGPASSWORD",
                "PGSSLPASSWORD",
                "PGOPTIONS",
                "PGSERVICE",
                "PGSERVICEFILE",
                "PGHOST",
                "PGHOSTADDR",
                "PGPORT",
                "PGUSER",
                "PGDATABASE",
                "PGSSLMODE",
                "PGREQUIRESSL",
                "PGPASSFILE",
                "PGTARGETSESSIONATTRS",
                "PGGSSENCMODE",
            ] {
                cmd.env_remove(var);
            }
            let fin = pgtools::run_streaming(&mut cmd, &redactor, |_| {}, || {})?;
            if !fin.success {
                return Err(restore_error(&fin.tail, &fin.status_text));
            }
            bookkeeping = present.iter().map(|(_, n)| n.clone()).collect();
            report.done(
                "migration bookkeeping",
                format!(
                    "{} kept with rows: {}",
                    plural(bookkeeping.len() as u64, "table"),
                    bookkeeping.join(", ")
                ),
                t,
            );
        }
    }

    // 11. Validate: connectivity and the inventory.
    let t = Instant::now();
    let mut sc = connect_long(&ctx, staging_db.as_str())?;
    let found = count_objects(&mut sc)?;
    let mismatch = validate_counts(&expected, &found);
    if let Some(missing) = &mismatch.0 {
        drop(sc);
        return Err(PgboxdbError::failure(format!(
            "validation failed after restore: {missing}; the staging database was removed and nothing was published"
        ))
        .details(json!({"expected": inventory_json(&expected), "found": counts_json(&found)}))
        .hint("this indicates objects pg_restore reported as done are missing; run with --verbose and report it"));
    }
    for w in mismatch.1 {
        report.warn(w);
    }
    report.done(
        "validate",
        [
            plural(found.tables, "table"),
            plural(found.sequences, "sequence"),
            plural(found.views, "view"),
            plural(found.matviews, "materialized view"),
            plural(found.indexes, "index"),
            plural(found.functions, "function"),
        ]
        .join(", "),
        t,
    );
    check_interrupted()?;

    // 12. Optimize for cloning: freeze (sets hint bits so a clone's first read
    //     does not rewrite, and so un-share, every page) and gather statistics.
    if opts.optimize {
        let t = Instant::now();
        report.progress("VACUUM (FREEZE, ANALYZE)");
        // Runs on a helper thread so Ctrl-C is honoured while the engine works:
        // on interrupt we abandon the session, and dropping the staging
        // database WITH (FORCE) terminates it.
        let worker = std::thread::spawn(move || {
            let mut sc = sc;
            let r = sc
                .simple_query("VACUUM (FREEZE, ANALYZE)")
                .and_then(|_| sc.simple_query("CHECKPOINT"));
            sc.close();
            r
        });
        while !worker.is_finished() {
            check_interrupted()?;
            std::thread::sleep(Duration::from_millis(100));
        }
        worker
            .join()
            .map_err(|_| PgboxdbError::failure("the optimize step panicked"))?
            .map_err(|e| db_error(e, "VACUUM (FREEZE, ANALYZE) failed on the staging database"))?;
        report.done(
            "optimize",
            format!("VACUUM (FREEZE, ANALYZE) in {}", secs(t.elapsed())),
            t,
        );
    } else {
        sc.close();
        report.warn(
            "optimize skipped (--no-optimize): the first read of each clone will rewrite pages"
                .into(),
        );
    }
    check_interrupted()?;

    // 13. Publish: an atomic catalog rename. Nothing else is connected to the
    //     staging database (our session above was closed).
    let t = Instant::now();
    let size: u64 = client
        .simple_query(&format!(
            "SELECT pg_database_size({})",
            quote_literal(staging_db.as_str())
        ))
        .ok()
        .and_then(|r| r.scalar().and_then(|s| s.parse().ok()))
        .unwrap_or(0);
    let rename = format!(
        "ALTER DATABASE {} RENAME TO {}",
        staging_db.quoted(),
        dest.quoted()
    );
    let mut renamed = client.simple_query(&rename);
    // Our own catalog connection may still be seen as "accessing" the staging
    // database for an instant after the restore's sessions closed; retry briefly.
    let deadline = Instant::now() + Duration::from_secs(10);
    while let Err(crate::pgwire::WireError::Db(d)) = &renamed {
        if d.sqlstate != "55006" || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        renamed = client.simple_query(&rename);
    }
    renamed.map_err(|e| {
        let err = db_error(e, &format!("cannot publish {staging_name} as {dest}"));
        if err.code == ExitCode::Conflict {
            err.hint(format!(
                "database {dest} was created while the copy was running; the staging copy was removed. Retry with another --to name"
            ))
        } else {
            err
        }
    })?;
    staging.published = true;
    // The TTL store may hold a stale record for a previous database of this
    // name; a copy is permanent.
    if let Err(e) = crate::ttlstore::MetaStore::new(&ctx.home.root).remove(dest.as_str()) {
        report.warn(format!(
            "could not clear a stale TTL record for {dest}: {}",
            e.message
        ));
    }
    report.done("publish", format!("{dest} ({})", human_bytes(size)), t);
    client.close();
    drop(staging);

    // 14. Report.
    let url = crate::cmd::db::tcp_url(&ctx, dest.as_str());
    let duration_ms = t_all.elapsed().as_millis() as u64;
    let text = format!(
        "\nDatabase ready: {dest} ({})\n  url  {url}\n\nNext:\n  pgboxdb branch {dest} agent-1",
        human_bytes(size)
    );
    out.emit(
        &text,
        json!({
            "status": "success",
            "operation": "copy",
            "source": {
                "engine": "postgresql",
                "version": schema_toc.source_version,
                "host": source.host,
                "port": source.port,
                "user": source.user,
                "database": source.dbname.clone().or(schema_toc.dbname.clone()),
            },
            "destination": {
                "engine": "pgboxdb",
                "database": dest.as_str(),
                "url": url,
                "size_bytes": size,
                "port": ctx.config.port,
            },
            "schema_only": opts.schema_only,
            "bookkeeping_tables": bookkeeping,
            "optimized": opts.optimize,
            "tools": {"pg_dump": tools.version, "dir": tools.dir},
            "extensions": inv.extensions,
            "objects": counts_json(&found),
            "stages": report.stages_json(),
            "warnings": report.warnings,
            "duration_ms": duration_ms,
        }),
    );
    Ok(ExitCode::Ok)
}

fn connect_long(ctx: &Ctx, database: &str) -> Result<Client> {
    crate::pgwire::Client::connect_unix(
        &ctx.sock_dir(),
        ctx.config.port,
        "postgres",
        database,
        LONG_TIMEOUT,
    )
    .map_err(|e| PgboxdbError::failure(format!("cannot connect to {database}: {e}")))
}

/// `pg_restore --list` on an archive: also the integrity check of the file.
fn read_toc(tools: &Tools, archive: &Path, redactor: &Redactor) -> Result<Toc> {
    let mut cmd = Command::new(&tools.pg_restore);
    cmd.arg("--list").arg("--").arg(archive);
    let (ok, stdout, stderr) = pgtools::run_capture(&mut cmd, redactor)?;
    if !ok {
        return Err(PgboxdbError::failure(format!(
            "the dump {} is unreadable: {}",
            archive.display(),
            stderr.trim()
        ))
        .hint("the export may have been interrupted or the disk may be full; retry"));
    }
    let toc = toc::parse(&stdout);
    if toc.entries.is_empty() {
        return Err(PgboxdbError::failure(format!(
            "the dump {} has no table of contents entries",
            archive.display()
        )));
    }
    Ok(toc)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counts {
    pub tables: u64,
    pub sequences: u64,
    pub views: u64,
    pub matviews: u64,
    pub indexes: u64,
    pub functions: u64,
}

/// User objects in the staging database, excluding system schemas and
/// extension members (pg_dump does not list those either).
fn count_objects(c: &mut Client) -> Result<Counts> {
    let rel = |kinds: &str| {
        format!(
            "(SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind IN ({kinds}) AND n.nspname NOT IN ('pg_catalog','information_schema') \
             AND n.nspname NOT LIKE 'pg\\_toast%' AND n.nspname NOT LIKE 'pg\\_temp%' \
             AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_class'::regclass \
             AND d.objid = c.oid AND d.deptype = 'e'))"
        )
    };
    let sql = format!(
        "SELECT {}, {}, {}, {}, {}, \
         (SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
          WHERE n.nspname NOT IN ('pg_catalog','information_schema') \
          AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass \
          AND d.objid = p.oid AND d.deptype IN ('e','i')))",
        rel("'r','p'"),
        rel("'S'"),
        rel("'v'"),
        rel("'m'"),
        rel("'i','I'")
    );
    let r = c
        .simple_query(&sql)
        .map_err(|e| db_error(e, "cannot inspect the restored database"))?;
    let row = r.rows.first().cloned().unwrap_or_default();
    let n = |i: usize| -> u64 {
        row.get(i)
            .and_then(|v| v.as_deref())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    };
    Ok(Counts {
        tables: n(0),
        sequences: n(1),
        views: n(2),
        matviews: n(3),
        indexes: n(4),
        functions: n(5),
    })
}

/// (fatal message if something expected is missing, warnings for extras).
pub fn validate_counts(expected: &Inventory, found: &Counts) -> (Option<String>, Vec<String>) {
    let mut missing = Vec::new();
    let mut extra = Vec::new();
    let mut check = |what: &str, exp: usize, got: u64| {
        let exp = exp as u64;
        if got < exp {
            missing.push(format!("{what}: expected {exp}, found {got}"));
        } else if got > exp {
            extra.push(format!("{what}: {got} found, {exp} in the dump"));
        }
    };
    check("tables", expected.tables, found.tables);
    check("sequences", expected.sequences, found.sequences);
    check("views", expected.views, found.views);
    check("materialized views", expected.matviews, found.matviews);
    check("functions", expected.functions, found.functions);
    // Constraint-backed indexes are CONSTRAINT entries in the dump, so the
    // catalog legitimately has more indexes than INDEX entries; fewer is wrong.
    if found.indexes < expected.indexes as u64 {
        missing.push(format!(
            "indexes: expected at least {}, found {}",
            expected.indexes, found.indexes
        ));
    }
    let fatal = if missing.is_empty() {
        None
    } else {
        Some(missing.join("; "))
    };
    let warnings = extra
        .into_iter()
        .map(|e| format!("more objects than the dump lists ({e}); the restore is complete, this is informational"))
        .collect();
    (fatal, warnings)
}

fn counts_json(c: &Counts) -> serde_json::Value {
    json!({
        "tables": c.tables, "sequences": c.sequences, "views": c.views,
        "materialized_views": c.matviews, "indexes": c.indexes, "functions": c.functions,
    })
}

fn inventory_json(i: &Inventory) -> serde_json::Value {
    json!({
        "tables": i.tables, "sequences": i.sequences, "views": i.views,
        "materialized_views": i.matviews, "indexes": i.indexes, "functions": i.functions,
        "data_entries": i.data_entries, "extensions": i.extensions,
    })
}

/// Used by `pgboxdb ls`: label for a database that is (or was) a copy staging area.
pub fn staging_status(home: &Home, name: &str) -> Option<&'static str> {
    if !name.starts_with(STAGING_PREFIX) {
        return None;
    }
    match markers(home).iter().find(|m| m.staging == name) {
        Some(m) if m.owner_alive() => Some("importing"),
        _ => Some("abandoned"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_names_are_valid_and_bounded() {
        let n = staging_name("myapp_template", "abc123");
        assert_eq!(n, "_pgboxdbcopy_myapp_template_abc123");
        assert!(names::validate(&n).is_ok());
        let long = "x".repeat(80);
        let n = staging_name(&long, "abc123");
        assert_eq!(n.len(), 63);
        assert!(names::validate(&n).is_ok());
        assert!(n.starts_with(STAGING_PREFIX));
        assert!(n.ends_with("_abc123"));
    }

    #[test]
    fn validation_distinguishes_missing_from_extra() {
        let exp = Inventory {
            tables: 2,
            sequences: 1,
            views: 1,
            matviews: 0,
            indexes: 1,
            functions: 2,
            ..Default::default()
        };
        let ok = Counts {
            tables: 2,
            sequences: 1,
            views: 1,
            matviews: 0,
            indexes: 3,
            functions: 2,
        };
        let (fatal, warns) = validate_counts(&exp, &ok);
        assert!(fatal.is_none());
        assert!(
            warns.is_empty(),
            "more indexes than INDEX entries is normal: {warns:?}"
        );
        let missing = Counts {
            tables: 1,
            ..ok.clone()
        };
        let (fatal, _) = validate_counts(&exp, &missing);
        assert!(fatal.unwrap().contains("tables: expected 2, found 1"));
        let extra = Counts {
            functions: 3,
            ..ok.clone()
        };
        let (fatal, warns) = validate_counts(&exp, &extra);
        assert!(fatal.is_none());
        assert_eq!(warns.len(), 1);
        let few_idx = Counts { indexes: 0, ..ok };
        assert!(validate_counts(&exp, &few_idx).0.is_some());
    }

    #[test]
    fn markers_roundtrip_and_staging_status() {
        let root = std::env::temp_dir().join(format!("pgboxdb-copy-markers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = Home::at(root.clone());
        assert!(markers(&home).is_empty());
        let m = Marker {
            staging: "_pgboxdbcopy_app_abc123".into(),
            destination: "app".into(),
            pid: std::process::id(),
            tmp_dir: root.join("tmp/copy-1"),
            started_at: "2026-10-24T00:00:00Z".into(),
            path: markers_dir(&home).join("_pgboxdbcopy_app_abc123.json"),
        };
        write_marker(&home, &m).unwrap();
        let dead = Marker {
            staging: "_pgboxdbcopy_app_dead00".into(),
            pid: 0,
            path: markers_dir(&home).join("_pgboxdbcopy_app_dead00.json"),
            ..m.clone()
        };
        write_marker(&home, &dead).unwrap();
        // a marker naming a non-staging database is ignored
        std::fs::write(
            markers_dir(&home).join("evil.json"),
            r#"{"staging":"app","destination":"x","pid":0}"#,
        )
        .unwrap();
        let got = markers(&home);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], m);
        assert!(got[0].owner_alive());
        assert!(!got[1].owner_alive());
        assert_eq!(
            staging_status(&home, "_pgboxdbcopy_app_abc123"),
            Some("importing")
        );
        assert_eq!(
            staging_status(&home, "_pgboxdbcopy_app_dead00"),
            Some("abandoned")
        );
        assert_eq!(
            staging_status(&home, "_pgboxdbcopy_unknown_000000"),
            Some("abandoned")
        );
        assert_eq!(staging_status(&home, "app"), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn dump_errors_get_actionable_hints() {
        let src = conninfo::parse("postgres://u:pw@h:5432/d").unwrap();
        let e = dump_error(
            &src,
            &["pg_dump: error: connection to server at \"h\" (1.2.3.4), port 5432 failed: FATAL:  password authentication failed for user \"u\"".to_string()],
            "exit status: 1",
        );
        assert!(e.hint.as_deref().unwrap().contains("password"), "{e}");
        assert!(!e.message.contains("pw"), "{e}");
        let e = dump_error(
            &src,
            &["pg_dump: error: connection to server at \"h\", port 5432 failed: Connection refused".to_string()],
            "exit status: 1",
        );
        assert!(e.hint.as_deref().unwrap().contains("pg_isready"), "{e}");
        let e = dump_error(
            &src,
            &["pg_dump: error: connection to server failed: FATAL:  database \"d\" does not exist".to_string()],
            "exit status: 1",
        );
        assert!(e.hint.as_deref().unwrap().contains("psql -l"), "{e}");
        let e = dump_error(
            &src,
            &["pg_dump: error: aborting because of server version mismatch".to_string()],
            "exit status: 1",
        );
        assert!(e.hint.as_deref().unwrap().contains("PGBOXDB_PG_BINDIR"), "{e}");
    }

    #[test]
    fn restore_errors_quote_the_failing_object() {
        let tail = vec![
            "pg_restore: creating TABLE \"public.t\"".to_string(),
            "pg_restore: creating POLICY \"public.t p\"".to_string(),
            "pg_restore: error: could not execute query: ERROR:  role \"app_reader\" does not exist".to_string(),
            "Command was: CREATE POLICY p ON public.t TO app_reader USING (true);".to_string(),
        ];
        let e = restore_error(&tail, "exit status: 1");
        assert_eq!(e.code, ExitCode::Failure);
        assert!(e.message.contains("app_reader"), "{e}");
        assert!(e.message.contains("Command was"), "{e}");
        assert!(e.hint.as_deref().unwrap().contains("CREATE ROLE"), "{e}");
        assert!(e.details.is_some());
    }

    #[test]
    fn plurals() {
        assert_eq!(plural(1, "table"), "1 table");
        assert_eq!(plural(2, "index"), "2 indexes");
        assert_eq!(plural(0, "materialized view"), "0 materialized views");
    }

    #[test]
    fn sizes_and_durations_format() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(30 * 1024 * 1024), "30.0 MB");
        assert_eq!(secs(Duration::from_millis(1234)), "1.2 s");
        assert_eq!(secs(Duration::from_secs(75)), "75 s");
    }
}
