//! Human or JSON output. Every command produces both forms; `--json` selects.
pub struct Out {
    pub json: bool,
    /// When false, `emit` is swallowed. A composite command (`pgboxdb run`) drives
    /// its sub-steps with a quiet Out so only its own final line reaches stdout,
    /// while the sub-steps' human progress notes (stderr) still show.
    emit_enabled: bool,
}

impl Out {
    pub fn new(json: bool) -> Out {
        Out {
            json,
            emit_enabled: true,
        }
    }
    /// A derived Out that keeps the mode and progress notes but swallows `emit`.
    pub fn quiet(&self) -> Out {
        Out {
            json: self.json,
            emit_enabled: false,
        }
    }
    /// `text` goes to stdout in human mode; `value` is printed as one JSON line in --json mode.
    pub fn emit(&self, text: &str, value: serde_json::Value) {
        if !self.emit_enabled {
            return;
        }
        if self.json {
            println!("{value}");
        } else if !text.is_empty() {
            println!("{text}");
        }
    }
    /// Progress lines for humans only (stderr, so stdout stays parseable).
    pub fn note(&self, text: &str) {
        if !self.json {
            eprintln!("{text}");
        }
    }
}
