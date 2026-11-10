//! Who a branch belongs to: `(host, session_id, project_id)`, and the database
//! name derived from it.
use crate::errors::{PgxError, Result};

/// FNV-1a 64-bit: a stable, dependency-free hash. (`std`'s `DefaultHasher` is
/// not stable across Rust versions, and a renamed branch would break resume.)
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn hex16(v: u64) -> String {
    format!("{v:016x}")
}

/// The identity of one coding-agent session, or of one subagent inside it.
///
/// A host session is `(project, host, session id)`. A subagent that the host
/// identifies (Claude Code's `agent_id` in `SubagentStart`, `SubagentStop`
/// and the tool hooks it fires) adds that id: every subagent of a session is
/// its own key and therefore its own branch, and the parent keeps the key it
/// always had, so records written before subagents existed still match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionKey {
    pub host: String,
    pub session_id: String,
    pub project_id: String,
    /// `None` for the host session itself (the parent agent).
    pub agent_id: Option<String>,
    /// PGX Git: the worktree + Git branch the session works on (a hash).
    /// `None` outside PGX Git, so every pre-existing key is unchanged. A
    /// session that switches Git branches gets a separate branch per Git
    /// branch, each cloned from that Git branch's database.
    pub scope: Option<String>,
}

impl SessionKey {
    /// The bytes every derived hash is computed over. A parent's material is
    /// exactly what it was before subagents existed (lock files and long
    /// names stay stable across upgrades).
    pub fn material(&self) -> String {
        let mut m = match &self.agent_id {
            None => format!("{}|{}|{}", self.project_id, self.host, self.session_id),
            Some(a) => format!(
                "{}|{}|{}|agent:{}",
                self.project_id, self.host, self.session_id, a
            ),
        };
        if let Some(s) = &self.scope {
            m.push_str("|git:");
            m.push_str(s);
        }
        m
    }

    /// `claude session 9b1f…` or `claude session 9b1f… agent abc1…`, for messages.
    pub fn describe(&self) -> String {
        match &self.agent_id {
            None => format!("{} session {}", self.host, self.session_id),
            Some(a) => format!("{} session {} agent {}", self.host, self.session_id, a),
        }
    }
}

/// A host label: `^[a-z][a-z0-9]{0,15}$` after lowercasing.
pub fn validate_host(s: &str) -> Result<String> {
    let h = s.to_ascii_lowercase();
    let ok = !h.is_empty()
        && h.len() <= 16
        && h.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && h.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if ok {
        Ok(h)
    } else {
        Err(PgxError::usage(format!(
            "invalid host {s:?}: use 1-16 lowercase letters or digits, starting with a letter (claude, cursor, codex, …)"
        )))
    }
}

/// A session id as hosts hand it over (UUIDs, chat ids) or as a user picks it
/// for `pgx agent exec --id`: 1..=128 bytes of printable ASCII, no whitespace,
/// no `/`, no shell metacharacters. It is used as a lookup key and never reaches
/// SQL unquoted, but a tight alphabet keeps hostile payloads boring.
pub fn validate_session_id(s: &str) -> Result<String> {
    validate_id(s, "session id")
}

/// A subagent id as the host hands it over (Claude Code: `agent_id`, e.g.
/// `agent-abc123` or a hex string). Same alphabet and limits as a session id.
pub fn validate_agent_id(s: &str) -> Result<String> {
    validate_id(s, "agent id")
}

fn validate_id(s: &str, what: &str) -> Result<String> {
    let ok = !s.is_empty()
        && s.len() <= 128
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':' | b'@' | b'+' | b'=')
        })
        && !s.contains("..");
    if ok {
        Ok(s.to_string())
    } else {
        Err(PgxError::usage(format!(
            "invalid {what} {s:?}: use up to 128 characters from A-Z a-z 0-9 - _ . : @ + ="
        )))
    }
}

/// Eight readable characters of an id: its first 8, lowercased, when all of
/// them are `[a-z0-9]` (UUIDs and hex ids qualify); otherwise the first 8 hex
/// digits of the id's own FNV hash. A leading `agent-`, as Claude Code
/// prefixes its subagent ids, is skipped so the visible part is the unique
/// part.
fn readable8(id: &str) -> String {
    let id = id
        .strip_prefix("agent-")
        .filter(|r| !r.is_empty())
        .unwrap_or(id);
    let lowered = id.to_ascii_lowercase();
    let first8: String = lowered.chars().take(8).collect();
    if first8.len() == 8
        && first8
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        first8
    } else {
        hex16(fnv1a64(id.as_bytes()))[..8].to_string()
    }
}

/// The branch name for a key.
///
/// - parent: `agent-<host>-<sid8>`; long form `agent-<host>-<sid8>-<16 hex of the full key>`
/// - subagent: `agent-<host>-<sid8>-<aid8>`; long form adds the 16 hex digits too
///
/// `sid8`/`aid8` are [`readable8`] of the session and agent id. The short
/// name is what a human sees in `pgx ls`; the long name carries a hash of the
/// complete identity (project, host, session, agent) and is used whenever
/// the short name is taken by something else, so uniqueness never rests on
/// the eight visible characters alone. Longest possible name: 6 + 16 + 1 +
/// 8 + 1 + 8 + 1 + 16 = 57 bytes, inside PostgreSQL's 63-byte limit.
pub fn branch_name(key: &SessionKey, long: bool) -> String {
    let mut name = format!("agent-{}-{}", key.host, readable8(&key.session_id));
    if let Some(a) = &key.agent_id {
        name.push('-');
        name.push_str(&readable8(a));
    }
    if long {
        name.push('-');
        name.push_str(&hex16(fnv1a64(key.material().as_bytes())));
    }
    name
}

/// The environment variable a host sets in the commands it runs, carrying its
/// own session id. Cursor sets none.
pub fn session_env_var(host: &str) -> Option<&'static str> {
    match host {
        "claude" => Some("CLAUDE_CODE_SESSION_ID"),
        "codex" => Some("CODEX_SESSION_ID"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(host: &str, sid: &str) -> SessionKey {
        SessionKey {
            host: host.to_string(),
            session_id: sid.to_string(),
            project_id: "a1b2c3d4e5f60718".to_string(),
            agent_id: None,
            scope: None,
        }
    }

    fn subkey(host: &str, sid: &str, aid: &str) -> SessionKey {
        let mut k = key(host, sid);
        k.agent_id = Some(aid.to_string());
        k
    }

    #[test]
    fn subagents_get_their_own_names_and_parents_keep_theirs() {
        let sid = "f775e29e-e738-4f37-93b4-9b82f539ba81";
        let parent = key("claude", sid);
        assert_eq!(branch_name(&parent, false), "agent-claude-f775e29e");
        // Claude's `agent-` prefix is skipped: the visible part is the unique part.
        let a = subkey("claude", sid, "agent-ada61284");
        let b = subkey("claude", sid, "agent-a9f6af68");
        assert_eq!(branch_name(&a, false), "agent-claude-f775e29e-ada61284");
        assert_eq!(branch_name(&b, false), "agent-claude-f775e29e-a9f6af68");
        assert_ne!(branch_name(&a, true), branch_name(&b, true));
        assert_ne!(a.material(), parent.material());
        // Hex ids as the harness hands them over work too.
        let c = subkey("claude", sid, "ada612848f911988e");
        assert_eq!(branch_name(&c, false), "agent-claude-f775e29e-ada61284");
        assert!(crate::names::validate(&branch_name(&c, true)).is_ok());
        assert!(branch_name(&c, true).len() <= 63);
        // The parent's material is byte-identical to the pre-subagent format.
        assert_eq!(
            parent.material(),
            "a1b2c3d4e5f60718|claude|".to_string() + sid
        );
        assert!(a.describe().contains("agent agent-ada61284"));
        assert!(!parent.describe().contains("agent agent-"));
    }

    #[test]
    fn two_subagents_sharing_eight_characters_differ_in_the_long_name() {
        let sid = "f775e29e-e738-4f37-93b4-9b82f539ba81";
        let a = subkey("claude", sid, "agent-deadbeef01");
        let b = subkey("claude", sid, "agent-deadbeef02");
        assert_eq!(branch_name(&a, false), branch_name(&b, false));
        assert_ne!(branch_name(&a, true), branch_name(&b, true));
    }

    #[test]
    fn longest_name_fits_postgres() {
        let k = SessionKey {
            host: "x".repeat(16),
            session_id: "s".repeat(128),
            project_id: "f".repeat(16),
            agent_id: Some("a".repeat(128)),
            scope: Some("f".repeat(16)),
        };
        let n = branch_name(&k, true);
        assert!(n.len() <= 63, "{n} is {} bytes", n.len());
        assert!(crate::names::validate(&n).is_ok(), "{n}");
    }

    #[test]
    fn a_git_scope_gives_a_distinct_identity_and_none_keeps_the_old_one() {
        let sid = "f775e29e-e738-4f37-93b4-9b82f539ba81";
        let plain = key("claude", sid);
        let mut a = key("claude", sid);
        a.scope = Some("1111111111111111".into());
        let mut b = key("claude", sid);
        b.scope = Some("2222222222222222".into());
        assert_eq!(
            plain.material(),
            "a1b2c3d4e5f60718|claude|".to_string() + sid
        );
        assert_ne!(a.material(), plain.material());
        assert_ne!(branch_name(&a, true), branch_name(&b, true));
        assert_eq!(branch_name(&a, false), branch_name(&plain, false));
    }

    #[test]
    fn agent_ids_are_validated_like_session_ids() {
        assert_eq!(validate_agent_id("agent-abc123").unwrap(), "agent-abc123");
        assert!(validate_agent_id("").is_err());
        assert!(validate_agent_id("a b").is_err());
        assert!(validate_agent_id("../x").is_err());
        assert!(validate_agent_id("x; rm -rf /").is_err());
    }

    #[test]
    fn fnv_matches_reference_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf29ce484222325);
        assert_eq!(fnv1a64(b"a"), 0xaf63dc4c8601ec8c);
        assert_eq!(hex16(0xaf63dc4c8601ec8c), "af63dc4c8601ec8c");
        assert_eq!(hex16(1), "0000000000000001");
    }

    #[test]
    fn uuid_session_gives_readable_name() {
        let k = key("claude", "9B1F3C2D-aaaa-bbbb-cccc-dddddddddddd");
        assert_eq!(branch_name(&k, false), "agent-claude-9b1f3c2d");
        let long = branch_name(&k, true);
        assert!(long.starts_with("agent-claude-9b1f3c2d-"), "{long}");
        assert_eq!(long.len(), "agent-claude-9b1f3c2d-".len() + 16);
    }

    #[test]
    fn odd_session_ids_hash_and_stay_valid_names() {
        let k = key("exec", "my task!");
        let short = branch_name(&k, false);
        assert!(short.starts_with("agent-exec-"), "{short}");
        assert_eq!(short.len(), "agent-exec-".len() + 8);
        assert!(crate::names::validate(&short).is_ok());
        assert!(crate::names::validate(&branch_name(&k, true)).is_ok());
        // Different projects, same host and id: same short name, different long name.
        let mut other = key("exec", "my task!");
        other.project_id = "ffffffffffffffff".into();
        assert_eq!(branch_name(&other, false), short);
        assert_ne!(branch_name(&other, true), branch_name(&k, true));
    }

    #[test]
    fn short_ids_are_not_padded_with_garbage() {
        let k = key("codex", "abc");
        let n = branch_name(&k, false);
        assert!(crate::names::validate(&n).is_ok());
        assert!(n.starts_with("agent-codex-"), "{n}");
    }

    #[test]
    fn hosts_and_ids_are_validated() {
        assert_eq!(validate_host("claude").unwrap(), "claude");
        assert_eq!(validate_host("Claude").unwrap(), "claude");
        assert!(validate_host("").is_err());
        assert!(validate_host("my host").is_err());
        assert!(validate_host("a-b").is_err());
        assert!(validate_host(&"x".repeat(17)).is_err());
        assert_eq!(validate_session_id("9b1f-3c2d").unwrap(), "9b1f-3c2d");
        assert!(validate_session_id("").is_err());
        assert!(validate_session_id("a b").is_err());
        assert!(validate_session_id("../x").is_err());
        assert!(validate_session_id("x; rm -rf /").is_err());
        assert!(validate_session_id(&"x".repeat(129)).is_err());
        assert!(validate_session_id("é").is_err());
    }

    #[test]
    fn session_env_vars_per_host() {
        assert_eq!(session_env_var("claude"), Some("CLAUDE_CODE_SESSION_ID"));
        assert_eq!(session_env_var("codex"), Some("CODEX_SESSION_ID"));
        assert_eq!(session_env_var("cursor"), None);
        assert_eq!(session_env_var("exec"), None);
    }
}
