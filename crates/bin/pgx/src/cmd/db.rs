//! `pgx create | drop | clone | branch | ls | url`.
use super::{db_error, Ctx};
use crate::cli::ExitCode;
use crate::errors::{PgxError, Result};
use crate::names::{self, DbName};
use crate::out::Out;
use crate::pgwire::quote_literal;
use serde_json::json;
use std::time::Instant;

type Client = crate::pgwire::Client<std::os::unix::net::UnixStream>;

pub(crate) fn exists(ctx: &Ctx, client: &mut Client, name: &DbName) -> Result<bool> {
    let _ = ctx;
    let r = client
        .simple_query(&format!(
            "SELECT 1 FROM pg_database WHERE datname = {}",
            quote_literal(name.as_str())
        ))
        .map_err(|e| db_error(e, "cannot query pg_database"))?;
    Ok(!r.rows.is_empty())
}

/// The OID of a database by name, or None if it does not exist. The OID is the
/// database incarnation identity recorded in TTL metadata: a dropped-and-
/// recreated database gets a new OID, so stale metadata cannot match it.
pub fn database_oid(client: &mut Client, name: &str) -> Result<Option<u64>> {
    let r = client
        .simple_query(&format!(
            "SELECT oid FROM pg_database WHERE datname = {}",
            quote_literal(name)
        ))
        .map_err(|e| db_error(e, "cannot read database oid"))?;
    Ok(r.scalar().and_then(|s| s.parse().ok()))
}

/// Client backends connected to `name` (other than ourselves). A clone source
/// must have none: `CREATE DATABASE … TEMPLATE` refuses a busy template.
pub fn active_connections(client: &mut Client, name: &str) -> Result<u64> {
    let r = client
        .simple_query(&format!(
            "SELECT count(*) FROM pg_stat_activity WHERE datname = {} AND pid <> pg_backend_pid()",
            quote_literal(name)
        ))
        .map_err(|e| db_error(e, "cannot read pg_stat_activity"))?;
    Ok(r.scalar().and_then(|s| s.parse().ok()).unwrap_or(0))
}

/// Apply a `--ttl` choice to a freshly created/cloned database `name`.
/// `Unlimited` (the default) writes no record and clears any stale one, so the
/// database is permanent and never a cleanup candidate. A finite TTL records a
/// durable metadata file keyed to the database's current OID and cluster.
pub fn record_ttl(
    ctx: &Ctx,
    client: &mut Client,
    name: &DbName,
    ttl_arg: Option<&str>,
) -> Result<crate::ttl::Ttl> {
    let store = crate::ttlstore::MetaStore::new(&ctx.home.root);
    let ttl = match ttl_arg {
        None => crate::ttl::Ttl::Unlimited,
        Some(s) => crate::ttl::parse_ttl(s)?,
    };
    match ttl {
        crate::ttl::Ttl::Unlimited => {
            // A permanent (re)incarnation: remove any leftover record for this name.
            store.remove(name.as_str())?;
        }
        crate::ttl::Ttl::Seconds(_) => {
            let now = crate::fsutil::now_epoch();
            let oid = database_oid(client, name.as_str())?.ok_or_else(|| {
                PgxError::failure(format!(
                    "database {name} vanished before its TTL was recorded"
                ))
            })?;
            let rec = crate::ttlstore::TtlRecord {
                name: name.as_str().to_string(),
                oid,
                cluster_id: ctx.cluster_id(),
                created_at: now,
                ttl_label: ttl.label(),
                ttl_seconds: ttl.as_seconds(),
                expires_at: ttl.expires_at(now)?,
                state: crate::ttlstore::State::Active,
                last_attempt: None,
                last_error: None,
                agent: None,
            };
            store.save(&rec)?;
        }
    }
    Ok(ttl)
}

/// JSON fields describing a TTL for command output.
pub fn ttl_json(ttl: &crate::ttl::Ttl, expires_at: Option<u64>, now: u64) -> serde_json::Value {
    json!({
        "ttl": ttl.label(),
        "ttl_seconds": ttl.as_seconds(),
        "expires_at": expires_at.map(crate::fsutil::rfc3339),
        "expires_in_seconds": expires_at.map(|e| e.saturating_sub(now)),
    })
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Percent-encode a URL userinfo component (the password): the unreserved set
/// only, so `/ ? # @ : [ ]` and the rest are escaped and cannot alter the
/// authority of a `postgres://` URL.
fn percent_encode_userinfo(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn tcp_url(ctx: &Ctx, name: &str) -> String {
    format!(
        "postgres://postgres:{}@127.0.0.1:{}/{}",
        percent_encode_userinfo(&ctx.config.password),
        ctx.config.port,
        percent_encode(name)
    )
}

pub fn socket_url(ctx: &Ctx, name: &str) -> String {
    format!(
        "postgres://postgres@/{}?host={}&port={}",
        percent_encode(name),
        percent_encode(&ctx.sock_dir().to_string_lossy()),
        ctx.config.port
    )
}

pub fn create(out: &Out, name: &str, ttl_arg: Option<&str>) -> Result<ExitCode> {
    let name = names::validate_user_target(name)?;
    // Validate the TTL before creating anything, so a bad --ttl fails fast.
    if let Some(s) = ttl_arg {
        crate::ttl::parse_ttl(s)?;
    }
    let ctx = Ctx::load()?;
    let mut client = ctx.connect("postgres")?;
    client
        .simple_query(&format!("CREATE DATABASE {}", name.quoted()))
        .map_err(|e| db_error(e, &format!("cannot create database {name}")))?;
    let now = crate::fsutil::now_epoch();
    let ttl = record_ttl(&ctx, &mut client, &name, ttl_arg)?;
    let expires_at = ttl.expires_at(now)?;
    client.close();
    let url = tcp_url(&ctx, name.as_str());
    let mut obj = json!({"created": name.as_str(), "url": url, "socket_url": socket_url(&ctx, name.as_str())});
    merge(&mut obj, ttl_json(&ttl, expires_at, now));
    let ttl_line = match expires_at {
        Some(_) => format!(
            "\n  ttl  {} (expires in {})",
            ttl.label(),
            crate::ttl::humanize_remaining(expires_at, now)
        ),
        None => String::new(),
    };
    out.emit(
        &format!("created database {name}{ttl_line}\n  url  {url}"),
        obj,
    );
    Ok(ExitCode::Ok)
}

/// Shallow-merge the fields of `extra` into `obj` (both must be objects).
fn merge(obj: &mut serde_json::Value, extra: serde_json::Value) {
    if let (Some(o), Some(e)) = (obj.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
}

pub fn drop(out: &Out, name: &str, force: bool) -> Result<ExitCode> {
    let name = names::validate_user_target(name)?;
    let ctx = Ctx::load()?;
    let mut client = ctx.connect("postgres")?;
    if !exists(&ctx, &mut client, &name)? {
        return Err(PgxError::new(
            ExitCode::NotFound,
            format!("database {name} does not exist"),
        )
        .hint("`pgx ls` lists databases"));
    }
    let sql = if force {
        format!("DROP DATABASE {} WITH (FORCE)", name.quoted())
    } else {
        format!("DROP DATABASE {}", name.quoted())
    };
    client.simple_query(&sql).map_err(|e| {
        let err = db_error(e, &format!("cannot drop database {name}"));
        if err.code == ExitCode::InUse {
            err.hint("disconnect the clients, or `pgx drop --force` to terminate their sessions")
        } else {
            err
        }
    })?;
    client.close();
    // The TTL record (and any agent ownership) goes with the database, so a
    // later database under this name starts with no history to inherit.
    crate::ttlstore::MetaStore::new(&ctx.home.root).remove(name.as_str())?;
    out.emit(
        &format!("dropped database {name}"),
        json!({"dropped": name.as_str(), "forced": force}),
    );
    Ok(ExitCode::Ok)
}

/// What a clone produced, for the command's output.
pub struct Cloned {
    pub elapsed_ms: f64,
    /// `clone` when the engine's `file_copy_method` shares blocks, else `copy`.
    pub method: &'static str,
    pub ttl: crate::ttl::Ttl,
    pub expires_at: Option<u64>,
    pub now: u64,
}

/// The one clone path, shared by `pgx clone` and `pgx branch`: check the
/// source and the target, `CREATE DATABASE … TEMPLATE … STRATEGY = FILE_COPY`,
/// record the TTL. A refused call creates nothing. The explicit target check
/// gives a clear message; the engine's own uniqueness check stays the
/// authority when two calls race for one name (sqlstate 42P04, or 23505 on
/// pg_database_datname_index → exit 4).
pub(crate) fn clone_database(
    ctx: &Ctx,
    client: &mut Client,
    source: &DbName,
    target: &DbName,
    ttl_arg: Option<&str>,
) -> Result<Cloned> {
    if !exists(ctx, client, source)? {
        return Err(PgxError::new(
            ExitCode::NotFound,
            format!("source database {source} does not exist"),
        )
        .hint("`pgx ls` lists databases"));
    }
    if exists(ctx, client, target)? {
        return Err(PgxError::new(
            ExitCode::Conflict,
            format!("database {target} already exists"),
        )
        .hint(format!(
            "pick another name, or `pgx drop {target}` first; nothing was changed"
        )));
    }
    // STRATEGY = FILE_COPY makes CREATE DATABASE copy the files, so the engine's
    // file_copy_method applies: clonefile on APFS, copy_file_range/FICLONE on
    // Linux reflink filesystems and ZFS, i.e. copy-on-write where the filesystem
    // supports it. The default WAL_LOG strategy copies every block through the
    // buffer cache and never shares blocks, which would defeat disposable clones.
    let file_copy_method = client
        .simple_query("SHOW file_copy_method")
        .ok()
        .and_then(|r| r.scalar().map(String::from))
        .unwrap_or_else(|| "unknown".into());
    let method = if file_copy_method == "clone" {
        "clone"
    } else {
        "copy"
    };
    let t0 = Instant::now();
    client
        .simple_query(&format!(
            "CREATE DATABASE {} TEMPLATE {} STRATEGY = FILE_COPY",
            target.quoted(),
            source.quoted()
        ))
        .map_err(|e| {
            let err = db_error(e, &format!("cannot clone {source} to {target}"));
            if err.code == ExitCode::InUse {
                err.hint(format!(
                    "clients are connected to {source}; disconnect them and retry"
                ))
            } else {
                err
            }
        })?;
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    // TTL is never inherited from the source: it comes only from --ttl here.
    let now = crate::fsutil::now_epoch();
    let ttl = record_ttl(ctx, client, target, ttl_arg)?;
    let expires_at = ttl.expires_at(now)?;
    Ok(Cloned {
        elapsed_ms,
        method,
        ttl,
        expires_at,
        now,
    })
}

pub fn clone(out: &Out, source: &str, target: &str, ttl_arg: Option<&str>) -> Result<ExitCode> {
    let target = names::validate_user_target(target)?;
    let source = names::validate(source)?;
    if let Some(s) = ttl_arg {
        crate::ttl::parse_ttl(s)?;
    }
    let ctx = Ctx::load()?;
    let mut client = ctx.connect("postgres")?;
    let c = clone_database(&ctx, &mut client, &source, &target, ttl_arg)?;
    client.close();
    let Cloned {
        elapsed_ms,
        method,
        ttl,
        expires_at,
        now,
    } = c;
    let url = tcp_url(&ctx, target.as_str());
    let mut obj = json!({"source": source.as_str(), "target": target.as_str(), "elapsed_ms": (elapsed_ms * 10.0).round() / 10.0, "method": method, "url": url});
    merge(&mut obj, ttl_json(&ttl, expires_at, now));
    let ttl_line = match expires_at {
        Some(_) => format!(
            "\n  ttl  {} (expires in {})",
            ttl.label(),
            crate::ttl::humanize_remaining(expires_at, now)
        ),
        None => String::new(),
    };
    out.emit(
        &format!(
            "cloned {source} to {target} in {elapsed_ms:.0} ms ({method}){ttl_line}\n  url  {url}"
        ),
        obj,
    );
    Ok(ExitCode::Ok)
}

/// The TTL a branch gets when `--ttl` is not given.
pub const BRANCH_DEFAULT_TTL: &str = "2h";

/// `pgx branch <source> <name> [--ttl <dur>]`: a disposable database for one
/// agent. It is `pgx clone` with a 2-hour default TTL and the connection URL
/// as the last line of output, so `DATABASE_URL="$(pgx branch app agent-1 | tail -1)"`
/// works; `--json` carries the same fields as `pgx clone --json` plus `branch`.
pub fn branch(out: &Out, source: &str, name: &str, ttl_arg: Option<&str>) -> Result<ExitCode> {
    let target = names::validate_user_target(name)?;
    let source = names::validate(source)?;
    let ttl_arg = ttl_arg.unwrap_or(BRANCH_DEFAULT_TTL);
    // Validate the TTL before touching the runtime, so a bad --ttl creates nothing.
    crate::ttl::parse_ttl(ttl_arg)?;
    let ctx = Ctx::load()?;
    let mut client = ctx.connect("postgres")?;
    let c = clone_database(&ctx, &mut client, &source, &target, Some(ttl_arg))?;
    client.close();
    let url = tcp_url(&ctx, target.as_str());
    let mut obj = json!({
        "branch": target.as_str(),
        "database": target.as_str(),
        "source": source.as_str(),
        "url": url,
        "socket_url": socket_url(&ctx, target.as_str()),
        "host": "127.0.0.1",
        "port": ctx.config.port,
        "user": "postgres",
        "elapsed_ms": (c.elapsed_ms * 10.0).round() / 10.0,
        "method": c.method,
    });
    merge(&mut obj, ttl_json(&c.ttl, c.expires_at, c.now));
    let expires = match c.ttl.as_seconds() {
        Some(secs) => crate::ttl::humanize_long(secs),
        None => format!("never (permanent; `pgx drop {target}` removes it)"),
    };
    out.emit(
        &format!(
            "✓ Branch created: {target}\n✓ Source: {source}\n✓ Expires in: {expires}\n\nDATABASE_URL:\n{url}"
        ),
        obj,
    );
    Ok(ExitCode::Ok)
}

pub fn ls(out: &Out) -> Result<ExitCode> {
    let ctx = Ctx::load()?;
    let mut client = ctx.connect("postgres")?;
    let r = client
        .simple_query(
            "SELECT d.datname, pg_database_size(d.oid), d.datistemplate, \
             (SELECT count(*) FROM pg_stat_activity a WHERE a.datname = d.datname AND a.backend_type = 'client backend'), \
             d.datconnlimit = -2 \
             FROM pg_database d WHERE NOT d.datistemplate ORDER BY d.datname",
        )
        .map_err(|e| db_error(e, "cannot list databases"))?;
    client.close();
    // TTL metadata (only present for temporary databases), keyed by name.
    let (records, _bad) = crate::ttlstore::MetaStore::new(&ctx.home.root).list();
    let now = crate::fsutil::now_epoch();
    let rec_by_name: std::collections::HashMap<&str, &crate::ttlstore::TtlRecord> =
        records.iter().map(|r| (r.name.as_str(), r)).collect();
    let mut items = Vec::new();
    let mut text = format!(
        "{:<24} {:<10} {:<10} {:<12} {:>12}\n",
        "NAME", "STATUS", "TTL", "EXPIRES IN", "SIZE"
    );
    for row in &r.rows {
        let name = row[0].clone().unwrap_or_default();
        let size: u64 = row[1].as_deref().and_then(|s| s.parse().ok()).unwrap_or(0);
        let conns: u64 = row[3].as_deref().and_then(|s| s.parse().ok()).unwrap_or(0);
        let invalid = row[4].as_deref() == Some("t");
        let rec = rec_by_name.get(name.as_str());
        let (ttl_label, expires_at, ttl_seconds) = match rec {
            Some(r) => (r.ttl_label.clone(), r.expires_at, r.ttl_seconds),
            None => ("unlimited".to_string(), None, None),
        };
        let status = if invalid {
            "invalid"
        } else if let Some(s) = crate::cmd::copy::staging_status(&ctx.home, &name) {
            s
        } else {
            match rec.map(|r| r.state) {
                Some(crate::ttlstore::State::Expiring) => "expiring",
                Some(crate::ttlstore::State::Deleting) => "deleting",
                Some(crate::ttlstore::State::RetryPending) => "retry",
                _ => "ready",
            }
        };
        let expires_in = crate::ttl::humanize_remaining(expires_at, now);
        // An agent branch names its host next to the TTL: `2h (claude)`.
        let ttl_col = match rec.and_then(|r| r.agent.as_ref()) {
            Some(m) => format!("{ttl_label} ({})", m.host),
            None => ttl_label.clone(),
        };
        text.push_str(&format!(
            "{name:<24} {status:<10} {ttl_col:<10} {expires_in:<12} {:>12}\n",
            human_size(size),
        ));
        items.push(json!({
            "name": name,
            "status": status,
            "size_bytes": size,
            "connections": conns,
            "is_template": row[2].as_deref() == Some("t"),
            "invalid": invalid,
            "ttl": ttl_label,
            "ttl_seconds": ttl_seconds,
            "expires_at": expires_at.map(crate::fsutil::rfc3339),
            "expires_in_seconds": expires_at.map(|e| e.saturating_sub(now)),
            "agent": rec.and_then(|r| r.agent.as_ref()).map(|m| m.to_json()),
        }));
    }
    out.emit(text.trim_end(), json!(items));
    Ok(ExitCode::Ok)
}

pub fn url(out: &Out, name: &str, socket: bool) -> Result<ExitCode> {
    let name = names::validate(name)?;
    let ctx = Ctx::load()?;
    let mut client = ctx.connect("postgres")?;
    if !exists(&ctx, &mut client, &name)? {
        return Err(PgxError::new(
            ExitCode::NotFound,
            format!("database {name} does not exist"),
        )
        .hint(format!("`pgx create {name}`")));
    }
    client.close();
    let url = if socket {
        socket_url(&ctx, name.as_str())
    } else {
        tcp_url(&ctx, name.as_str())
    };
    out.emit(
        &url,
        json!({"database": name.as_str(), "url": url, "port": ctx.config.port, "host": "127.0.0.1", "user": "postgres", "socket_dir": ctx.sock_dir(), "socket_url": socket_url(&ctx, name.as_str())}),
    );
    Ok(ExitCode::Ok)
}

fn human_size(b: u64) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encoding_keeps_paths_and_escapes_specials() {
        assert_eq!(percent_encode("/tmp/pgx run"), "/tmp/pgx%20run");
        assert_eq!(percent_encode("p@ss:w/rd"), "p%40ss%3Aw/rd");
        assert_eq!(percent_encode("Abc-_.~09"), "Abc-_.~09");
    }

    #[test]
    fn userinfo_encoding_escapes_slash_and_url_specials() {
        // A password with / ? # @ : must not break the authority of a postgres:// URL.
        assert_eq!(percent_encode_userinfo("pa/ss"), "pa%2Fss");
        assert_eq!(percent_encode_userinfo("a?b#c@d:e"), "a%3Fb%23c%40d%3Ae");
        assert_eq!(percent_encode_userinfo("Abc-_.~09"), "Abc-_.~09");
        assert!(!percent_encode_userinfo("x/y?z").contains('/'));
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(7_500_000), "7.2 MB");
    }
}
