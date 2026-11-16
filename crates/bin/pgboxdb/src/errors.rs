//! One error type for the whole CLI: an exit code, a message, an optional hint.
use crate::cli::ExitCode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgboxdbError {
    pub code: ExitCode,
    pub message: String,
    pub hint: Option<String>,
    /// Structured cause for `--json` consumers (for example the unsupported
    /// extensions that stopped a copy). Absent from human output.
    /// Boxed: JSON values are large and every `Result` carries this type.
    pub details: Option<Box<serde_json::Value>>,
}

impl PgboxdbError {
    pub fn new(code: ExitCode, message: impl Into<String>) -> Self {
        PgboxdbError {
            code,
            message: message.into(),
            hint: None,
            details: None,
        }
    }
    pub fn details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(Box::new(details));
        self
    }
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
    pub fn usage(message: impl Into<String>) -> Self {
        PgboxdbError::new(ExitCode::Usage, message).hint("run `pgboxdb help` for usage")
    }
    pub fn failure(message: impl Into<String>) -> Self {
        PgboxdbError::new(ExitCode::Failure, message)
    }
    pub fn io(what: &str, path: &std::path::Path, e: &std::io::Error) -> Self {
        PgboxdbError::failure(format!("{what} {}: {e}", path.display()))
    }
    pub fn to_json(&self) -> String {
        let mut err = serde_json::json!({
            "code": self.code.name(),
            "exit_code": self.code as i32,
            "message": self.message,
            "hint": self.hint,
        });
        if let Some(d) = &self.details {
            err["details"] = (**d).clone();
        }
        serde_json::json!({ "error": err }).to_string()
    }
}

impl std::fmt::Display for PgboxdbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(h) = &self.hint {
            write!(f, "\n  hint: {h}")?;
        }
        Ok(())
    }
}

pub type Result<T> = std::result::Result<T, PgboxdbError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ExitCode;

    #[test]
    fn error_carries_exit_code_and_hint() {
        let e = PgboxdbError::new(ExitCode::NotFound, "database \"x\" does not exist")
            .hint("pgboxdb ls");
        assert_eq!(e.code, ExitCode::NotFound);
        assert_eq!(e.message, "database \"x\" does not exist");
        assert_eq!(e.hint.as_deref(), Some("pgboxdb ls"));
        assert_eq!(e.code as i32, 5);
    }

    #[test]
    fn error_json_shape() {
        let e = PgboxdbError::new(ExitCode::Conflict, "port 5432 is in use");
        let v: serde_json::Value = serde_json::from_str(&e.to_json()).unwrap();
        assert_eq!(v["error"]["code"], "conflict");
        assert_eq!(v["error"]["exit_code"], 4);
        assert_eq!(v["error"]["message"], "port 5432 is in use");
        assert!(v["error"].get("details").is_none());
        let e = e.details(serde_json::json!({"unsupported_extensions": ["postgis"]}));
        let v: serde_json::Value = serde_json::from_str(&e.to_json()).unwrap();
        assert_eq!(
            v["error"]["details"]["unsupported_extensions"][0],
            "postgis"
        );
    }
}
