//! A minimal PostgreSQL frontend/backend protocol client: startup over a
//! Unix socket with trust authentication, simple queries, text results.
//! Enough for the CLI to manage the runtime; applications use real drivers.
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbError {
    pub severity: String,
    pub sqlstate: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
}

#[derive(Debug)]
pub enum WireError {
    Io(std::io::Error),
    Db(DbError),
    Protocol(String),
}

impl From<std::io::Error> for WireError {
    fn from(e: std::io::Error) -> Self {
        WireError::Io(e)
    }
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Io(e) => write!(f, "connection error: {e}"),
            WireError::Db(e) => write!(f, "{}: {}", e.sqlstate, e.message),
            WireError::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

pub type Row = Vec<Option<String>>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
    /// one CommandComplete tag per statement, in order
    pub tags: Vec<String>,
}

impl QueryResult {
    /// First column of the first row, as text.
    pub fn scalar(&self) -> Option<&str> {
        self.rows
            .first()
            .and_then(|r| r.first())
            .and_then(|v| v.as_deref())
    }
}

pub struct Client<S> {
    stream: S,
    parameters: HashMap<String, String>,
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn encode_startup(user: &str, database: &str) -> Vec<u8> {
    encode_startup_app(user, database, "pgx")
}

/// The startup message with a chosen `application_name` (`pgx top` names its
/// monitoring connection `pgx-top` so it is recognisable in pg_stat_activity).
pub fn encode_startup_app(user: &str, database: &str, application_name: &str) -> Vec<u8> {
    let mut body = Vec::new();
    for (k, v) in [
        ("user", user),
        ("database", database),
        ("client_encoding", "UTF8"),
        ("application_name", application_name),
    ] {
        body.extend_from_slice(k.as_bytes());
        body.push(0);
        body.extend_from_slice(v.as_bytes());
        body.push(0);
    }
    body.push(0);
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(&196608u32.to_be_bytes());
    out.extend_from_slice(&body);
    out
}

pub fn encode_query(sql: &str) -> Vec<u8> {
    let mut out = vec![b'Q'];
    out.extend_from_slice(&((sql.len() + 5) as u32).to_be_bytes());
    out.extend_from_slice(sql.as_bytes());
    out.push(0);
    out
}

pub fn parse_error_fields(payload: &[u8]) -> DbError {
    let mut e = DbError {
        severity: String::new(),
        sqlstate: String::new(),
        message: String::new(),
        detail: None,
        hint: None,
    };
    for field in payload.split(|b| *b == 0) {
        let Some((&tag, rest)) = field.split_first() else {
            continue;
        };
        let text = String::from_utf8_lossy(rest).into_owned();
        match tag {
            b'S' => e.severity = text,
            b'C' => e.sqlstate = text,
            b'M' => e.message = text,
            b'D' => e.detail = Some(text),
            b'H' => e.hint = Some(text),
            _ => {}
        }
    }
    e
}

pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn read_exact<S: Read>(s: &mut S, n: usize) -> Result<Vec<u8>, WireError> {
    let mut buf = vec![0u8; n];
    s.read_exact(&mut buf)?;
    Ok(buf)
}

/// Reads one backend message: (tag, payload without the length word).
fn read_message<S: Read>(s: &mut S) -> Result<(u8, Vec<u8>), WireError> {
    let head = read_exact(s, 5)?;
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if !(4..=64 * 1024 * 1024).contains(&len) {
        return Err(WireError::Protocol(format!(
            "message length {len} out of range"
        )));
    }
    let payload = read_exact(s, len - 4)?;
    Ok((head[0], payload))
}

fn cstring_at(buf: &[u8], pos: &mut usize) -> Result<String, WireError> {
    let start = *pos;
    let end = buf[start..]
        .iter()
        .position(|b| *b == 0)
        .ok_or_else(|| WireError::Protocol("unterminated string".into()))?
        + start;
    *pos = end + 1;
    Ok(String::from_utf8_lossy(&buf[start..end]).into_owned())
}

impl Client<UnixStream> {
    pub fn socket_path(sock_dir: &Path, port: u16) -> std::path::PathBuf {
        sock_dir.join(format!(".s.PGSQL.{port}"))
    }

    pub fn connect_unix(
        sock_dir: &Path,
        port: u16,
        user: &str,
        database: &str,
        timeout: Duration,
    ) -> Result<Self, WireError> {
        Self::connect_unix_app(sock_dir, port, user, database, "pgx", timeout)
    }

    pub fn connect_unix_app(
        sock_dir: &Path,
        port: u16,
        user: &str,
        database: &str,
        application_name: &str,
        timeout: Duration,
    ) -> Result<Self, WireError> {
        let stream = UnixStream::connect(Self::socket_path(sock_dir, port))?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        Client::handshake_app(stream, user, database, application_name)
    }
}

impl<S: Read + Write> Client<S> {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn handshake(stream: S, user: &str, database: &str) -> Result<Self, WireError> {
        Self::handshake_app(stream, user, database, "pgx")
    }

    pub fn handshake_app(
        mut stream: S,
        user: &str,
        database: &str,
        application_name: &str,
    ) -> Result<Self, WireError> {
        stream.write_all(&encode_startup_app(user, database, application_name))?;
        stream.flush()?;
        let mut client = Client {
            stream,
            parameters: HashMap::new(),
        };
        loop {
            let (tag, payload) = read_message(&mut client.stream)?;
            match tag {
                b'R' => {
                    let kind = u32::from_be_bytes(
                        payload[..4]
                            .try_into()
                            .map_err(|_| WireError::Protocol("short auth message".into()))?,
                    );
                    if kind != 0 {
                        return Err(WireError::Protocol(format!("unexpected authentication request {kind}; the Unix socket must use trust authentication")));
                    }
                }
                b'S' => {
                    let mut pos = 0;
                    let k = cstring_at(&payload, &mut pos)?;
                    let v = cstring_at(&payload, &mut pos)?;
                    client.parameters.insert(k, v);
                }
                b'K' | b'N' => {}
                b'E' => return Err(WireError::Db(parse_error_fields(&payload))),
                b'Z' => return Ok(client),
                other => {
                    return Err(WireError::Protocol(format!(
                        "unexpected message {:?} during startup",
                        other as char
                    )))
                }
            }
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn parameter(&self, name: &str) -> Option<&str> {
        self.parameters.get(name).map(String::as_str)
    }

    /// Runs one simple-query message (may hold several statements). Returns the
    /// rows of the last statement that produced rows and every command tag.
    pub fn simple_query(&mut self, sql: &str) -> Result<QueryResult, WireError> {
        self.stream.write_all(&encode_query(sql))?;
        self.stream.flush()?;
        let mut result = QueryResult::default();
        let mut error: Option<DbError> = None;
        loop {
            let (tag, payload) = read_message(&mut self.stream)?;
            match tag {
                b'T' => {
                    let n = u16::from_be_bytes([payload[0], payload[1]]) as usize;
                    let mut pos = 2;
                    let mut cols = Vec::with_capacity(n);
                    for _ in 0..n {
                        cols.push(cstring_at(&payload, &mut pos)?);
                        pos += 18;
                    }
                    result.columns = cols;
                    result.rows.clear();
                }
                b'D' => {
                    let n = u16::from_be_bytes([payload[0], payload[1]]) as usize;
                    let mut pos = 2;
                    let mut row = Vec::with_capacity(n);
                    for _ in 0..n {
                        let len = i32::from_be_bytes(
                            payload[pos..pos + 4]
                                .try_into()
                                .map_err(|_| WireError::Protocol("short data row".into()))?,
                        );
                        pos += 4;
                        if len < 0 {
                            row.push(None);
                        } else {
                            let len = len as usize;
                            row.push(Some(
                                String::from_utf8_lossy(&payload[pos..pos + len]).into_owned(),
                            ));
                            pos += len;
                        }
                    }
                    result.rows.push(row);
                }
                b'C' => {
                    let mut pos = 0;
                    result.tags.push(cstring_at(&payload, &mut pos)?);
                }
                b'E' => error = Some(parse_error_fields(&payload)),
                b'Z' => {
                    return match error {
                        Some(e) => Err(WireError::Db(e)),
                        None => Ok(result),
                    };
                }
                // EmptyQueryResponse, NoticeResponse, ParameterStatus, CopyIn/Out are not used by the CLI.
                b'I' | b'N' | b'S' | b'A' => {}
                other => {
                    return Err(WireError::Protocol(format!(
                        "unexpected message {:?} in query response",
                        other as char
                    )))
                }
            }
        }
    }

    pub fn close(mut self) {
        let _ = self.stream.write_all(&[b'X', 0, 0, 0, 4]);
        let _ = self.stream.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read, Write};

    /// A fake duplex stream: the test supplies what the "server" sends and
    /// captures what the client writes.
    struct Fake {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }
    impl Read for Fake {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(buf)
        }
    }
    impl Write for Fake {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn msg(tag: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![tag];
        v.extend_from_slice(&((payload.len() + 4) as u32).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }
    fn cstr(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }
    fn row_description(cols: &[&str]) -> Vec<u8> {
        let mut p = (cols.len() as u16).to_be_bytes().to_vec();
        for c in cols {
            p.extend(cstr(c));
            p.extend_from_slice(&[0u8; 18]); // table oid, attnum, type oid, typlen, typmod, format
        }
        msg(b'T', &p)
    }
    fn data_row(values: &[Option<&str>]) -> Vec<u8> {
        let mut p = (values.len() as u16).to_be_bytes().to_vec();
        for v in values {
            match v {
                None => p.extend_from_slice(&(-1i32).to_be_bytes()),
                Some(s) => {
                    p.extend_from_slice(&(s.len() as i32).to_be_bytes());
                    p.extend_from_slice(s.as_bytes());
                }
            }
        }
        msg(b'D', &p)
    }

    #[test]
    fn startup_message_bytes() {
        let b = encode_startup("postgres", "mydb");
        let mut expect = Vec::new();
        let body =
            b"user\0postgres\0database\0mydb\0client_encoding\0UTF8\0application_name\0pgx\0\0";
        expect.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
        expect.extend_from_slice(&196608u32.to_be_bytes());
        expect.extend_from_slice(body);
        assert_eq!(b, expect);
    }

    #[test]
    fn query_message_bytes() {
        assert_eq!(encode_query("SELECT 1"), msg(b'Q', b"SELECT 1\0"));
    }

    #[test]
    fn parse_error_response_fields() {
        let e = parse_error_fields(
            b"SERROR\0C42P01\0Mrelation \"x\" does not exist\0Hcheck the name\0\0",
        );
        assert_eq!(e.severity, "ERROR");
        assert_eq!(e.sqlstate, "42P01");
        assert_eq!(e.message, "relation \"x\" does not exist");
        assert_eq!(e.hint.as_deref(), Some("check the name"));
    }

    #[test]
    fn handshake_trust_then_query_with_nulls() {
        let mut server = Vec::new();
        server.extend(msg(b'R', &0u32.to_be_bytes())); // AuthenticationOk
        server.extend(msg(b'S', b"server_version\x0018.6\x00"));
        server.extend(msg(b'K', &[0, 0, 0, 7, 0, 0, 0, 9]));
        server.extend(msg(b'Z', b"I"));
        server.extend(row_description(&["a", "b"]));
        server.extend(data_row(&[Some("1"), None]));
        server.extend(data_row(&[Some("2"), Some("x")]));
        server.extend(msg(b'C', b"SELECT 2\0"));
        server.extend(msg(b'Z', b"I"));
        let fake = Fake {
            input: Cursor::new(server),
            output: Vec::new(),
        };
        let mut c = Client::handshake(fake, "postgres", "db").unwrap();
        assert_eq!(c.parameter("server_version"), Some("18.6"));
        let r = c.simple_query("SELECT * FROM t").unwrap();
        assert_eq!(r.columns, vec!["a", "b"]);
        assert_eq!(
            r.rows,
            vec![
                vec![Some("1".to_string()), None],
                vec![Some("2".to_string()), Some("x".to_string())]
            ]
        );
        assert_eq!(r.tags, vec!["SELECT 2"]);
        let written = &c.stream.output;
        assert!(written.starts_with(&encode_startup("postgres", "db")));
        assert!(written.ends_with(&encode_query("SELECT * FROM t")));
    }

    #[test]
    fn error_response_is_returned_and_client_is_ready_again() {
        let mut server = Vec::new();
        server.extend(msg(b'R', &0u32.to_be_bytes()));
        server.extend(msg(b'Z', b"I"));
        server.extend(msg(b'E', b"SERROR\0C42P01\0Mnope\0\0"));
        server.extend(msg(b'Z', b"I"));
        server.extend(msg(b'C', b"CREATE DATABASE\0"));
        server.extend(msg(b'Z', b"I"));
        let fake = Fake {
            input: Cursor::new(server),
            output: Vec::new(),
        };
        let mut c = Client::handshake(fake, "postgres", "db").unwrap();
        match c.simple_query("SELECT * FROM missing") {
            Err(WireError::Db(e)) => assert_eq!(e.sqlstate, "42P01"),
            other => panic!("expected a database error, got {other:?}"),
        }
        let r = c.simple_query("CREATE DATABASE x").unwrap();
        assert_eq!(r.tags, vec!["CREATE DATABASE"]);
        assert!(r.rows.is_empty());
    }

    #[test]
    fn unexpected_authentication_request_is_an_error() {
        let mut server = Vec::new();
        server.extend(msg(b'R', &10u32.to_be_bytes())); // AuthenticationSASL
        let fake = Fake {
            input: Cursor::new(server),
            output: Vec::new(),
        };
        match Client::handshake(fake, "postgres", "db") {
            Err(WireError::Protocol(m)) => assert!(m.contains("authentication"), "{m}"),
            other => panic!("expected protocol error, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn quote_identifier_and_literal() {
        assert_eq!(quote_ident("my_db"), "\"my_db\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(quote_literal("it's"), "'it''s'");
        assert_eq!(quote_literal("back\\slash"), "'back\\slash'");
    }

    /// Needs a live server: `PGX_TEST_SOCKET=<dir> PGX_TEST_PORT=<n> cargo test -p pgx -- --ignored`.
    /// The black-box suites in tests/ exercise the live path on every run.
    #[test]
    #[ignore = "needs PGX_TEST_SOCKET / PGX_TEST_PORT pointing at a running engine"]
    fn query_against_live_server() {
        let dir = std::env::var("PGX_TEST_SOCKET").expect("PGX_TEST_SOCKET");
        let port: u16 = std::env::var("PGX_TEST_PORT")
            .expect("PGX_TEST_PORT")
            .parse()
            .unwrap();
        let mut c = Client::connect_unix(
            std::path::Path::new(&dir),
            port,
            "postgres",
            "postgres",
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        let r = c
            .simple_query("SELECT 1 AS one, NULL::text AS none")
            .unwrap();
        assert_eq!(r.columns, vec!["one", "none"]);
        assert_eq!(r.rows, vec![vec![Some("1".to_string()), None]]);
    }
}
