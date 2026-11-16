//! The `--from` connection string of `pgboxdb copy`: split the password out so it
//! travels to `pg_dump` only through `PGPASSWORD`, never on a command line,
//! and build a credential-free description for messages and JSON.
//!
//! Accepts what libpq accepts: a `postgres://` / `postgresql://` URL, a
//! `key=value` conninfo string, or a bare database name. The parse mirrors
//! libpq's rules (userinfo ends at the first `@` before any `/`, the password
//! starts at the first `:` of the userinfo, percent-escapes are decoded).
use crate::errors::{PgboxdbError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The connection string with any password removed; safe to pass as an
    /// argument and to show.
    pub arg: String,
    /// The password, if the string carried one. Goes to `PGPASSWORD`.
    pub password: Option<String>,
    /// `sslpassword=` (client key passphrase), if given. Goes to `PGSSLPASSWORD`.
    pub ssl_password: Option<String>,
    /// `user@host:port/dbname` for humans (never includes a password).
    pub display: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub dbname: Option<String>,
}

pub fn parse(raw: &str) -> Result<Source> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(PgboxdbError::usage(
            "--from needs a PostgreSQL connection URL",
        ));
    }
    if let Some(rest) = s
        .strip_prefix("postgres://")
        .or_else(|| s.strip_prefix("postgresql://"))
    {
        let scheme = &s[..s.len() - rest.len()];
        return parse_url(scheme, rest);
    }
    if s.contains("://") {
        return Err(PgboxdbError::usage(format!(
            "unsupported connection URL scheme in --from ({})",
            s.split("://").next().unwrap_or("")
        ))
        .hint("use postgres://user:password@host:port/database"));
    }
    if s.contains('=') {
        return parse_conninfo(s);
    }
    // A bare database name, and nothing that could be a mistyped URL carrying
    // credentials (`user:pw@host/db` without the scheme): those must never be
    // echoed or passed on.
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(PgboxdbError::usage(
            "invalid --from value: not a postgres:// URL, a key=value conninfo string or a plain database name (the value is not shown; check for a missing postgres:// prefix)",
        ));
    }
    // A bare database name: libpq fills host/port/user from PG* variables.
    Ok(Source {
        arg: s.to_string(),
        password: None,
        ssl_password: None,
        display: s.to_string(),
        host: None,
        port: None,
        user: None,
        dbname: Some(s.to_string()),
    })
}

fn parse_url(scheme: &str, rest: &str) -> Result<Source> {
    // userinfo: up to the first '@' that comes before any '/'.
    let (userinfo, after_user) = match rest.find(['@', '/']) {
        Some(i) if rest.as_bytes()[i] == b'@' => (Some(&rest[..i]), &rest[i + 1..]),
        _ => (None, rest),
    };
    let (user, password) = match userinfo {
        None => (None, None),
        Some(ui) => match ui.split_once(':') {
            Some((u, p)) => (Some(u), Some(p).filter(|p| !p.is_empty())),
            None => (Some(ui), None),
        },
    };
    // hostspec: up to '/' or '?'.
    let host_end = after_user.find(['/', '?']).unwrap_or(after_user.len());
    let hostspec = &after_user[..host_end];
    let tail = &after_user[host_end..];
    let (path, query) = match tail.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (tail, None),
    };
    let dbname = path.strip_prefix('/').filter(|d| !d.is_empty());

    // A password may also arrive as ?password=… ; pull it out of the query.
    let mut password = password.map(percent_decode);
    let mut ssl_password: Option<String> = None;
    let mut kept_params: Vec<String> = Vec::new();
    let mut query_host: Option<String> = None;
    let mut query_port: Option<String> = None;
    let mut query_user: Option<String> = None;
    let mut query_dbname: Option<String> = None;
    if let Some(q) = query {
        for pair in q.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let k = percent_decode(k);
            match k.as_str() {
                // libpq: a later setting wins over an earlier one.
                "password" => password = Some(percent_decode(v)),
                "sslpassword" => ssl_password = Some(percent_decode(v)),
                _ => {
                    match k.as_str() {
                        "host" | "hostaddr" => query_host = Some(percent_decode(v)),
                        "port" => query_port = Some(percent_decode(v)),
                        "user" => query_user = Some(percent_decode(v)),
                        "dbname" => query_dbname = Some(percent_decode(v)),
                        _ => {}
                    }
                    kept_params.push(pair.to_string());
                }
            }
        }
    }
    for (k, v) in [("user", user), ("password", password.as_deref())] {
        if let Some(v) = v {
            if v.chars().any(|c| c.is_control()) {
                return Err(PgboxdbError::usage(format!(
                    "invalid --from URL: the {k} contains control characters"
                )));
            }
        }
    }
    if hostspec
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(PgboxdbError::usage(
            "invalid --from URL: the host part contains whitespace",
        ));
    }
    if hostspec.contains('@') {
        return Err(PgboxdbError::usage(
            "invalid --from URL: a password containing @ : or / must be percent-encoded (%40 %3A %2F)",
        ));
    }

    // Rebuild without the password.
    let mut arg = String::from(scheme);
    if let Some(u) = user {
        arg.push_str(u);
        arg.push('@');
    }
    arg.push_str(hostspec);
    arg.push_str(path);
    if !kept_params.is_empty() {
        arg.push('?');
        arg.push_str(&kept_params.join("&"));
    }

    let (host, port) = split_host_port(hostspec);
    let host = host.map(percent_decode).or(query_host);
    let port_text = port.map(String::from).or(query_port);
    let port = parse_port(port_text.as_deref(), "URL")?;
    let user = user.map(percent_decode).or(query_user);
    let dbname = dbname.map(percent_decode).or(query_dbname);
    let display = describe(user.as_deref(), host.as_deref(), port, dbname.as_deref());
    Ok(Source {
        arg,
        password,
        ssl_password,
        display,
        host,
        port,
        user,
        dbname,
    })
}

/// `host:port`, `[v6]:port`, `host`, `[v6]`, or several comma-separated
/// hosts (returned verbatim, no port).
fn split_host_port(hostspec: &str) -> (Option<&str>, Option<&str>) {
    if hostspec.is_empty() {
        return (None, None);
    }
    if hostspec.contains(',') {
        return (Some(hostspec), None);
    }
    if let Some(rest) = hostspec.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            let host = &rest[..end];
            let after = &rest[end + 1..];
            let port = after.strip_prefix(':').filter(|p| !p.is_empty());
            return (Some(host), port);
        }
        return (Some(hostspec), None);
    }
    // `host:port`; a non-numeric port is reported by the caller's parse.
    match hostspec.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() => (Some(h).filter(|h| !h.is_empty()), Some(p)),
        _ => (Some(hostspec), None),
    }
}

/// `key=value [key=value …]`, values optionally single-quoted with `\'`
/// and `\\` escapes. `password=` is removed; everything else is kept verbatim.
fn parse_conninfo(s: &str) -> Result<Source> {
    let mut kept: Vec<String> = Vec::new();
    let mut password = None;
    let mut ssl_password = None;
    let mut host = None;
    let mut port = None;
    let mut user = None;
    let mut dbname = None;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let key_start = i;
        while i < chars.len() && chars[i] != '=' && !chars[i].is_whitespace() {
            i += 1;
        }
        let key: String = chars[key_start..i].iter().collect();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            return Err(PgboxdbError::usage(
                "invalid --from conninfo: keys must be libpq option names like host, port, user, dbname (the value is not shown)",
            ));
        }
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() || chars[i] != '=' {
            return Err(PgboxdbError::usage(format!(
                "invalid --from conninfo: missing '=' after {key:?}"
            )));
        }
        i += 1;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        let raw_start = i;
        let mut value = String::new();
        if i < chars.len() && chars[i] == '\'' {
            i += 1;
            loop {
                if i >= chars.len() {
                    return Err(PgboxdbError::usage(
                        "invalid --from conninfo: unterminated quoted value",
                    ));
                }
                match chars[i] {
                    '\\' if i + 1 < chars.len() => {
                        value.push(chars[i + 1]);
                        i += 2;
                    }
                    '\'' => {
                        i += 1;
                        break;
                    }
                    c => {
                        value.push(c);
                        i += 1;
                    }
                }
            }
        } else {
            while i < chars.len() && !chars[i].is_whitespace() {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    value.push(chars[i + 1]);
                    i += 2;
                } else {
                    value.push(chars[i]);
                    i += 1;
                }
            }
        }
        let raw: String = chars[raw_start..i].iter().collect();
        match key.as_str() {
            "password" => password = Some(value),
            "sslpassword" => ssl_password = Some(value),
            other => {
                match other {
                    "host" | "hostaddr" => host = Some(value.clone()),
                    "port" => port = Some(value.clone()),
                    "user" => user = Some(value.clone()),
                    "dbname" => dbname = Some(value.clone()),
                    _ => {}
                }
                kept.push(format!("{key}={raw}"));
            }
        }
    }
    let port = parse_port(port.as_deref(), "conninfo")?;
    let display = describe(user.as_deref(), host.as_deref(), port, dbname.as_deref());
    Ok(Source {
        arg: kept.join(" "),
        password,
        ssl_password,
        display,
        host,
        port,
        user,
        dbname,
    })
}

/// A single numeric port; a comma list (one per host) is accepted and not
/// shown; anything else is a usage error that does not echo the value.
fn parse_port(text: Option<&str>, kind: &str) -> Result<Option<u16>> {
    match text {
        None | Some("") => Ok(None),
        Some(p) if p.contains(',') => Ok(None),
        Some(p) => p.parse::<u16>().map(Some).map_err(|_| {
            PgboxdbError::usage(format!(
                "invalid --from {kind}: the port is not a number (a password containing : must be percent-encoded)"
            ))
        }),
    }
}

fn describe(user: Option<&str>, host: Option<&str>, port: Option<u16>, db: Option<&str>) -> String {
    let mut s = String::new();
    if let Some(u) = user {
        s.push_str(u);
        s.push('@');
    }
    match host {
        Some(h) if h.starts_with('/') => s.push_str(&format!("[socket {h}]")),
        Some(h) => s.push_str(h),
        None => s.push_str("localhost"),
    }
    if let Some(p) = port {
        s.push_str(&format!(":{p}"));
    }
    s.push('/');
    s.push_str(db.unwrap_or("(default database)"));
    s
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if let Some(hex) = b.get(i + 1..i + 3) {
                if let Ok(v) = u8::from_str_radix(std::str::from_utf8(hex).unwrap_or("zz"), 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Masks every occurrence of a secret (and its percent-encoded form) in text
/// that came from a child process or an error, so a password can never reach
/// the terminal, a log or JSON.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    needles: Vec<String>,
}

impl Redactor {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(secret: Option<&str>) -> Redactor {
        Redactor::with_secrets(&[secret])
    }
    /// Masks each secret given; `None`/empty entries are ignored.
    pub fn with_secrets(secrets: &[Option<&str>]) -> Redactor {
        let mut needles = Vec::new();
        for s in secrets.iter().flatten().filter(|s| s.len() >= 2) {
            needles.push(s.to_string());
            let enc = percent_encode_all(s);
            if enc != *s {
                needles.push(enc);
            }
        }
        // Longest first so an encoded form is masked before its raw substring.
        needles.sort_by_key(|n| std::cmp::Reverse(n.len()));
        Redactor { needles }
    }
    pub fn redact(&self, text: &str) -> String {
        let mut t = text.to_string();
        for n in &self.needles {
            t = t.replace(n.as_str(), "***");
        }
        t
    }
}

fn percent_encode_all(s: &str) -> String {
    let mut out = String::new();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ExitCode;

    #[test]
    fn url_password_is_split_out_and_decoded() {
        let s =
            parse("postgres://app:p%40ss%3Aw%2Frd@localhost:5432/myapp?sslmode=require").unwrap();
        assert_eq!(s.arg, "postgres://app@localhost:5432/myapp?sslmode=require");
        assert_eq!(s.password.as_deref(), Some("p@ss:w/rd"));
        assert_eq!(s.user.as_deref(), Some("app"));
        assert_eq!(s.host.as_deref(), Some("localhost"));
        assert_eq!(s.port, Some(5432));
        assert_eq!(s.dbname.as_deref(), Some("myapp"));
        assert_eq!(s.display, "app@localhost:5432/myapp");
        assert!(
            !s.arg.contains("p%40") && !s.arg.contains("p@ss"),
            "{}",
            s.arg
        );
    }

    #[test]
    fn url_without_password_is_unchanged() {
        let s = parse("postgresql://alex@db.internal/prod").unwrap();
        assert_eq!(s.arg, "postgresql://alex@db.internal/prod");
        assert_eq!(s.password, None);
        assert_eq!(s.port, None);
        assert_eq!(s.display, "alex@db.internal/prod");
        let s = parse("postgres:///mydb").unwrap();
        assert_eq!(s.arg, "postgres:///mydb");
        assert_eq!(s.display, "localhost/mydb");
        // an empty userinfo password means "none", so ?password= still applies
        let s = parse("postgres://u:@h/d?password=real").unwrap();
        assert_eq!(s.password.as_deref(), Some("real"));
        assert_eq!(s.arg, "postgres://u@h/d");
    }

    #[test]
    fn sslpassword_and_encoded_keys_are_stripped_too() {
        let s =
            parse("postgres://u@h/d?sslpassword=keypw&sslmode=verify-full&pass%77ord=pw2").unwrap();
        assert_eq!(s.ssl_password.as_deref(), Some("keypw"));
        assert_eq!(s.password.as_deref(), Some("pw2"));
        assert_eq!(s.arg, "postgres://u@h/d?sslmode=verify-full");
        let s = parse("host=h sslpassword=kp dbname=d").unwrap();
        assert_eq!(s.ssl_password.as_deref(), Some("kp"));
        assert_eq!(s.arg, "host=h dbname=d");
        // multi-host with one port per host is valid libpq
        let s = parse("postgres:///d?host=h1,h2&port=5432,5433").unwrap();
        assert_eq!(s.port, None);
        assert_eq!(
            parse("host=h1,h2 port=5432,5433 dbname=d").unwrap().port,
            None
        );
    }

    #[test]
    fn mistyped_sources_are_rejected_without_echoing_them() {
        for bad in [
            "app:s3cret@db.example.com:5432/prod",
            "user:pa=ss@host/db",
            "postgres://u:pa/ss@h/d",
            "postgres://u:p@ss@h/d",
            "postgres://u:p%40ss@h:notaport/d",
        ] {
            let e = parse(bad).unwrap_err();
            assert_eq!(e.code, ExitCode::Usage, "{bad}");
            for frag in ["s3cret", "pa=ss", "pa/ss", "ss@h", "notaport"] {
                assert!(
                    !e.message.contains(frag),
                    "{bad}: {} leaks {frag}",
                    e.message
                );
            }
        }
    }

    #[test]
    fn percent_decode_never_panics_on_multibyte() {
        assert_eq!(percent_decode("50%日本"), "50%日本");
        assert_eq!(percent_decode("%e6%97%a5"), "日");
        assert_eq!(percent_decode("a%4日"), "a%4日");
    }

    #[test]
    fn url_password_in_query_is_removed() {
        let s = parse("postgres://u@h/d?password=secret&application_name=x").unwrap();
        assert_eq!(s.password.as_deref(), Some("secret"));
        assert_eq!(s.arg, "postgres://u@h/d?application_name=x");
        assert!(!s.arg.contains("secret"));
    }

    #[test]
    fn url_ipv6_and_socket_hosts() {
        let s = parse("postgres://u:pw@[::1]:5433/d").unwrap();
        assert_eq!(s.host.as_deref(), Some("::1"));
        assert_eq!(s.port, Some(5433));
        assert_eq!(s.arg, "postgres://u@[::1]:5433/d");
        let s = parse("postgres://u@%2Ftmp:5432/d").unwrap();
        assert_eq!(s.host.as_deref(), Some("/tmp"));
        assert_eq!(s.display, "u@[socket /tmp]:5432/d");
        let s = parse("postgres:///d?host=/var/run/postgresql&port=5432").unwrap();
        assert_eq!(s.host.as_deref(), Some("/var/run/postgresql"));
        assert_eq!(s.port, Some(5432));
        assert_eq!(s.arg, "postgres:///d?host=/var/run/postgresql&port=5432");
    }

    #[test]
    fn url_multiple_hosts_kept_verbatim() {
        let s = parse("postgres://u:x@h1:5432,h2:5433/d").unwrap();
        assert_eq!(s.arg, "postgres://u@h1:5432,h2:5433/d");
        assert_eq!(s.host.as_deref(), Some("h1:5432,h2:5433"));
        assert_eq!(s.port, None);
    }

    #[test]
    fn url_password_with_at_sign_percent_encoded() {
        // libpq ends the userinfo at the first '@'; a raw '@' in a password is
        // not valid, the encoded one is.
        let s = parse("postgres://u:a%40b@h/d").unwrap();
        assert_eq!(s.password.as_deref(), Some("a@b"));
        assert_eq!(s.arg, "postgres://u@h/d");
    }

    #[test]
    fn conninfo_password_is_removed_quotes_handled() {
        let s2 =
            parse("host=localhost port=5432 user=app password='a b\\'c' dbname=myapp").unwrap();
        assert_eq!(s2.password.as_deref(), Some("a b'c"));
        assert_eq!(s2.arg, "host=localhost port=5432 user=app dbname=myapp");
        assert_eq!(s2.display, "app@localhost:5432/myapp");
        assert_eq!(s2.port, Some(5432));
        let s3 = parse("dbname=d password=plain host=h").unwrap();
        assert_eq!(s3.password.as_deref(), Some("plain"));
        assert_eq!(s3.arg, "dbname=d host=h");
    }

    #[test]
    fn bare_database_name() {
        let s = parse("myapp").unwrap();
        assert_eq!(s.arg, "myapp");
        assert_eq!(s.dbname.as_deref(), Some("myapp"));
        assert_eq!(s.password, None);
    }

    #[test]
    fn invalid_inputs_are_usage_errors() {
        for bad in [
            "",
            "mysql://u@h/d",
            "postgres://u:p@h:notaport/d",
            "postgres://u@h o s t/d",
            "host=localhost password",
            "host='unterminated",
            "my app",
            "Host=x dbname=d",
        ] {
            let e = parse(bad).unwrap_err();
            assert_eq!(e.code, ExitCode::Usage, "{bad:?}: {}", e.message);
        }
    }

    #[test]
    fn redactor_masks_raw_and_encoded_forms() {
        let r = Redactor::new(Some("p@ss:w/rd"));
        assert_eq!(
            r.redact("connection to postgres://app:p@ss:w/rd@h failed"),
            "connection to postgres://app:***@h failed"
        );
        assert_eq!(
            r.redact("url postgres://app:p%40ss%3Aw%2Frd@h"),
            "url postgres://app:***@h"
        );
        assert_eq!(Redactor::new(None).redact("nothing"), "nothing");
        assert_eq!(Redactor::new(Some("")).redact("a b"), "a b");
        let r = Redactor::with_secrets(&[Some("pw"), None, Some("keypw")]);
        assert_eq!(r.redact("pw and keypw"), "*** and ***");
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b%2F"), "a b/");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("abc%"), "abc%");
        assert_eq!(percent_decode("%4"), "%4");
    }
}
