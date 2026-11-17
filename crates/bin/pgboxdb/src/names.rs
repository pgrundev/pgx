//! Database names the CLI accepts. Stricter than PostgreSQL on purpose: a
//! name an agent can pass on a command line without quoting trouble.
use crate::errors::{PgboxdbError, Result};
use crate::pgwire::quote_ident;

pub const RESERVED: [&str; 3] = ["postgres", "template0", "template1"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbName(String);

impl DbName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Always quoted: case is preserved and nothing is interpreted.
    pub fn quoted(&self) -> String {
        quote_ident(&self.0)
    }
}

impl std::fmt::Display for DbName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// `^[A-Za-z_][A-Za-z0-9_-]{0,62}$`
pub fn validate(name: &str) -> Result<DbName> {
    let bad = |why: &str| {
        PgboxdbError::usage(format!("invalid database name {name:?}: {why}"))
            .hint("use letters, digits, _ and - (first character a letter or _), at most 63 bytes")
    };
    if name.is_empty() {
        return Err(bad("empty"));
    }
    if name.len() > 63 {
        return Err(bad("longer than 63 bytes"));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(bad("must start with a letter or _"));
    }
    if let Some(c) = chars.find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-')) {
        return Err(bad(&format!("contains {c:?}")));
    }
    Ok(DbName(name.to_string()))
}

/// A name the user may create, drop or clone onto: valid and not a system database.
pub fn validate_user_target(name: &str) -> Result<DbName> {
    let n = validate(name)?;
    if RESERVED.contains(&n.as_str()) {
        return Err(PgboxdbError::usage(format!(
            "{name:?} is a system database and cannot be created, dropped or overwritten"
        )));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_identifier_like_names() {
        for ok in [
            "myapp",
            "my_app_development",
            "My-App_2",
            "_x",
            "a".repeat(63).as_str(),
            "test_1",
        ] {
            assert_eq!(validate(ok).unwrap().as_str(), ok);
        }
        assert_eq!(validate("My-App").unwrap().quoted(), "\"My-App\"");
    }

    #[test]
    fn rejects_everything_else() {
        for bad in [
            "",
            "1abc",
            "-a",
            "a b",
            "a;b",
            "a\"b",
            "a'b",
            "é",
            "x".repeat(64).as_str(),
            "a.b",
            "a/b",
            "a\0",
        ] {
            let e = validate(bad).unwrap_err();
            assert_eq!(e.code, crate::cli::ExitCode::Usage, "{bad:?}");
        }
    }

    #[test]
    fn reserved_names_are_valid_but_not_targets() {
        for r in RESERVED {
            assert!(validate(r).is_ok());
            assert_eq!(
                validate_user_target(r).unwrap_err().code,
                crate::cli::ExitCode::Usage
            );
        }
        assert!(validate_user_target("postgres2").is_ok());
    }
}
