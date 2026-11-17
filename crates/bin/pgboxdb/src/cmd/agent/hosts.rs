//! Which host delivered a hook payload, and what it means.
//!
//! One `pgboxdb agent hook` serves Claude Code, Cursor and Codex. Each host has
//! its own payload shape and vocabulary (see
//! docs/superpowers/specs/2026-10-25-agent-integration-research.md); this
//! module turns a payload plus the hook's environment into a host, an event
//! class and a validated session id. Payloads are untrusted input.
use super::identity;
use crate::errors::{PgboxdbError, Result};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    Claude,
    Cursor,
    Codex,
}

impl Host {
    pub fn parse(s: &str) -> Result<Host> {
        match s {
            "claude" | "claude-code" => Ok(Host::Claude),
            "cursor" => Ok(Host::Cursor),
            "codex" => Ok(Host::Codex),
            other => Err(PgboxdbError::usage(format!(
                "unknown host {other:?}: use claude, cursor or codex"
            ))),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Host::Claude => "claude",
            Host::Cursor => "cursor",
            Host::Codex => "codex",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A session began (or resumed, forked, compacted): ensure the branch.
    Start,
    /// The agent did something: extend the idle TTL.
    Activity,
    /// A tool is about to run (Claude Code `PreToolUse`): activity, and for a
    /// subagent's Bash command the point where it is routed to its branch.
    ToolUse,
    /// A subagent of the session started: ensure its own branch.
    SubagentStart,
    /// A subagent finished: retire its branch.
    SubagentStop,
    /// The session ended: retire the branch.
    End,
    /// Delivered to us but not ours to act on.
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookInput {
    pub host: Host,
    pub event: Event,
    pub session_id: String,
    /// The subagent this delivery is about or comes from (Claude Code's
    /// `agent_id`); `None` for the session itself.
    pub agent_id: Option<String>,
    /// An `agent_id` was present but not acceptable: the reason. The hook
    /// treats such a delivery as a subagent it cannot identify.
    pub agent_id_error: Option<String>,
    /// `PreToolUse`: the tool and its input.
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    /// Where the session runs (payload `cwd`, or Cursor's first workspace root).
    pub cwd: Option<PathBuf>,
    /// The host's project root from its environment, when it differs from `cwd`.
    pub project_dir: Option<PathBuf>,
    pub reason: Option<String>,
    /// A Cursor cloud/background agent: nothing local to do.
    pub background: bool,
    pub raw_event: String,
}

fn has(env: &HashMap<String, String>, k: &str) -> bool {
    env.get(k).is_some_and(|v| !v.is_empty())
}

fn str_field(v: &serde_json::Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(String::from)
}

/// Decide the host, in this order: `forced`; a Cursor-shaped payload
/// (`conversation_id`) or `CURSOR_VERSION`; Claude's `CLAUDE_ENV_FILE` /
/// `CLAUDE_CODE_SESSION_ID`; Codex's `CODEX_THREAD_ID` / `CODEX_SESSION_ID`;
/// finally a PascalCase `hook_event_name` with `session_id` means Claude.
pub fn detect(
    payload: &serde_json::Value,
    env: &HashMap<String, String>,
    forced: Option<&str>,
) -> Result<HookInput> {
    let obj = payload
        .as_object()
        .ok_or_else(|| PgboxdbError::usage("hook payload is not a JSON object"))?;
    let raw_event = str_field(payload, "hook_event_name").unwrap_or_default();
    let host = match forced {
        Some(h) => Host::parse(h)?,
        None => {
            if obj.contains_key("conversation_id") || has(env, "CURSOR_VERSION") {
                Host::Cursor
            } else if has(env, "CLAUDE_ENV_FILE")
                || has(env, "CLAUDE_CODE_SESSION_ID")
                || has(env, "CLAUDE_PROJECT_DIR")
            {
                Host::Claude
            } else if has(env, "CODEX_THREAD_ID") || has(env, "CODEX_SESSION_ID") {
                Host::Codex
            } else if obj.contains_key("session_id")
                && raw_event
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_uppercase())
            {
                // Claude Code sets CLAUDE_PROJECT_DIR for every hook; a
                // PascalCase payload with no host variable at all is Codex,
                // whose hook processes carry none.
                Host::Codex
            } else {
                return Err(PgboxdbError::usage(
                    "cannot tell which agent sent this hook payload; pass --host claude|cursor|codex",
                ));
            }
        }
    };
    let event = match host {
        Host::Claude => match raw_event.as_str() {
            "SessionStart" => Event::Start,
            "UserPromptSubmit" | "PostToolUse" => Event::Activity,
            "PreToolUse" => Event::ToolUse,
            "SubagentStart" => Event::SubagentStart,
            "SubagentStop" => Event::SubagentStop,
            "SessionEnd" => Event::End,
            _ => Event::Ignore,
        },
        // Codex has no subagent events and its PreToolUse is activity only.
        Host::Codex => match raw_event.as_str() {
            "SessionStart" => Event::Start,
            "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Event::Activity,
            "SessionEnd" => Event::End,
            _ => Event::Ignore,
        },
        Host::Cursor => match raw_event.as_str() {
            "sessionStart" => Event::Start,
            "beforeSubmitPrompt"
            | "afterShellExecution"
            | "beforeShellExecution"
            | "preToolUse"
            | "postToolUse" => Event::Activity,
            "sessionEnd" => Event::End,
            _ => Event::Ignore,
        },
    };
    let sid = match host {
        Host::Cursor => {
            str_field(payload, "conversation_id").or_else(|| str_field(payload, "session_id"))
        }
        _ => str_field(payload, "session_id"),
    }
    .ok_or_else(|| PgboxdbError::usage("hook payload has no session id"))?;
    let session_id = identity::validate_session_id(&sid)?;
    let cwd = str_field(payload, "cwd")
        .or_else(|| {
            payload
                .get("workspace_roots")
                .and_then(|w| w.as_array())
                .and_then(|a| a.first())
                .and_then(|x| x.as_str())
                .map(String::from)
        })
        .map(PathBuf::from);
    let project_dir = match host {
        Host::Cursor => env
            .get("CURSOR_PROJECT_DIR")
            .or_else(|| env.get("CLAUDE_PROJECT_DIR")),
        _ => env.get("CLAUDE_PROJECT_DIR"),
    }
    .filter(|v| !v.is_empty())
    .map(PathBuf::from);
    // A subagent id, when the host sends one. Only Claude Code does today; it
    // is validated like a session id, and a bad one is remembered rather than
    // rejected so the hook can still answer the delivery safely.
    let (agent_id, agent_id_error) = match str_field(payload, "agent_id").filter(|a| !a.is_empty())
    {
        Some(a) if host == Host::Claude => match identity::validate_agent_id(&a) {
            Ok(a) => (Some(a), None),
            Err(e) => (None, Some(e.message)),
        },
        _ => (None, None),
    };
    let tool_name = str_field(payload, "tool_name");
    let tool_input = payload.get("tool_input").filter(|t| t.is_object()).cloned();
    Ok(HookInput {
        host,
        event,
        session_id,
        agent_id,
        agent_id_error,
        tool_name,
        tool_input,
        cwd,
        project_dir,
        reason: str_field(payload, "reason"),
        background: payload
            .get("is_background_agent")
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
        raw_event,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn claude(event: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut v = json!({
            "session_id": "9b1f3c2d-aaaa-bbbb-cccc-dddddddddddd",
            "transcript_path": "/Users/x/.claude/projects/p/9b1f.jsonl",
            "cwd": "/work/app",
            "hook_event_name": event,
            "permission_mode": "default"
        });
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        v
    }

    fn cursor(event: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut v = json!({
            "conversation_id": "c0ffee00-1111-2222-3333-444444444444",
            "generation_id": "g1",
            "model": "gpt-5",
            "hook_event_name": event,
            "cursor_version": "3.0.0",
            "workspace_roots": ["/work/app"],
            "user_email": null,
            "transcript_path": null
        });
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        v
    }

    fn codex(event: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut v = json!({
            "session_id": "0199c3a2-7b1e-7c3f-9d2a-5e6f7a8b9c0d",
            "cwd": "/work/app",
            "hook_event_name": event,
            "model": "gpt-5-codex",
            "permission_mode": "default",
            "transcript_path": null
        });
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        v
    }

    #[test]
    fn claude_events_map_with_env_file_present() {
        let e = env(&[
            ("CLAUDE_ENV_FILE", "/tmp/x.sh"),
            ("CLAUDE_PROJECT_DIR", "/work/app"),
        ]);
        let h = detect(
            &claude("SessionStart", json!({"source": "startup"})),
            &e,
            None,
        )
        .unwrap();
        assert_eq!(h.host, Host::Claude);
        assert_eq!(h.event, Event::Start);
        assert_eq!(h.session_id, "9b1f3c2d-aaaa-bbbb-cccc-dddddddddddd");
        assert_eq!(h.cwd.as_deref(), Some(std::path::Path::new("/work/app")));
        assert_eq!(
            h.project_dir.as_deref(),
            Some(std::path::Path::new("/work/app"))
        );
        assert!(!h.background);
        assert!(h.agent_id.is_none() && h.agent_id_error.is_none());
        for ev in ["UserPromptSubmit", "PostToolUse"] {
            assert_eq!(
                detect(&claude(ev, json!({})), &e, None).unwrap().event,
                Event::Activity,
                "{ev}"
            );
        }
        let t = detect(
            &claude(
                "PreToolUse",
                json!({"tool_name": "Bash", "tool_input": {"command": "psql -c 'select 1'", "timeout": 5000}}),
            ),
            &e,
            None,
        )
        .unwrap();
        assert_eq!(t.event, Event::ToolUse);
        assert_eq!(t.tool_name.as_deref(), Some("Bash"));
        assert_eq!(t.tool_input.unwrap()["command"], "psql -c 'select 1'");
        // Subagent deliveries carry the agent id; a bad one is remembered, not fatal.
        let s = detect(
            &claude(
                "SubagentStart",
                json!({"agent_id": "agent-abc123", "agent_type": "Explore"}),
            ),
            &e,
            None,
        )
        .unwrap();
        assert_eq!(s.event, Event::SubagentStart);
        assert_eq!(s.agent_id.as_deref(), Some("agent-abc123"));
        let s = detect(
            &claude("SubagentStop", json!({"agent_id": "agent-abc123"})),
            &e,
            None,
        )
        .unwrap();
        assert_eq!(s.event, Event::SubagentStop);
        let bad = detect(
            &claude(
                "PreToolUse",
                json!({"agent_id": "x; rm -rf /", "tool_name": "Bash"}),
            ),
            &e,
            None,
        )
        .unwrap();
        assert!(bad.agent_id.is_none());
        assert!(bad.agent_id_error.is_some());
        let end = detect(
            &claude("SessionEnd", json!({"reason": "prompt_input_exit"})),
            &e,
            None,
        )
        .unwrap();
        assert_eq!(end.event, Event::End);
        assert_eq!(end.reason.as_deref(), Some("prompt_input_exit"));
        assert_eq!(
            detect(&claude("Notification", json!({})), &e, None)
                .unwrap()
                .event,
            Event::Ignore
        );
    }

    #[test]
    fn claude_is_recognised_by_its_project_dir_and_envless_pascalcase_is_codex() {
        // Claude Code sets CLAUDE_PROJECT_DIR for every hook; Codex hooks carry
        // no host variables at all, so a bare PascalCase payload is Codex.
        let h = detect(
            &claude("SessionStart", json!({"source": "resume"})),
            &env(&[("CLAUDE_PROJECT_DIR", "/w")]),
            None,
        )
        .unwrap();
        assert_eq!(h.host, Host::Claude);
        assert_eq!(h.event, Event::Start);
        let h = detect(
            &claude("SessionStart", json!({"source": "resume"})),
            &env(&[]),
            None,
        )
        .unwrap();
        assert_eq!(h.host, Host::Codex);
    }

    #[test]
    fn cursor_events_map_and_background_is_flagged() {
        let e = env(&[
            ("CURSOR_VERSION", "3.0.0"),
            ("CURSOR_PROJECT_DIR", "/work/app"),
        ]);
        let h = detect(&cursor("sessionStart", json!({"session_id": "c0ffee00-1111-2222-3333-444444444444", "is_background_agent": false, "composer_mode": "agent"})), &e, None).unwrap();
        assert_eq!(h.host, Host::Cursor);
        assert_eq!(h.event, Event::Start);
        assert_eq!(h.session_id, "c0ffee00-1111-2222-3333-444444444444");
        assert_eq!(h.cwd.as_deref(), Some(std::path::Path::new("/work/app")));
        let bg = detect(
            &cursor("sessionStart", json!({"is_background_agent": true})),
            &e,
            None,
        )
        .unwrap();
        assert!(bg.background);
        for ev in [
            "beforeSubmitPrompt",
            "afterShellExecution",
            "beforeShellExecution",
            "preToolUse",
            "postToolUse",
        ] {
            assert_eq!(
                detect(&cursor(ev, json!({})), &e, None).unwrap().event,
                Event::Activity,
                "{ev}"
            );
        }
        let end = detect(
            &cursor(
                "sessionEnd",
                json!({"reason": "window_close", "duration_ms": 10}),
            ),
            &e,
            None,
        )
        .unwrap();
        assert_eq!(end.event, Event::End);
        assert_eq!(end.reason.as_deref(), Some("window_close"));
        assert_eq!(
            detect(&cursor("afterFileEdit", json!({})), &e, None)
                .unwrap()
                .event,
            Event::Ignore
        );
        assert_eq!(
            detect(&cursor("stop", json!({})), &e, None).unwrap().event,
            Event::Ignore
        );
    }

    #[test]
    fn cursor_payload_delivered_through_claude_compat_is_detected_as_cursor() {
        // Cursor loads Claude hook files and sets CLAUDE_PROJECT_DIR too, but the
        // payload is Cursor's: conversation_id and a camelCase event name.
        let e = env(&[
            ("CURSOR_VERSION", "3.0.0"),
            ("CLAUDE_PROJECT_DIR", "/work/app"),
        ]);
        let h = detect(&cursor("sessionStart", json!({})), &e, None).unwrap();
        assert_eq!(h.host, Host::Cursor);
        assert_eq!(h.event, Event::Start);
        // Even without CURSOR_VERSION the payload shape decides.
        let h = detect(
            &cursor("preToolUse", json!({"tool_name": "Shell"})),
            &env(&[("CLAUDE_PROJECT_DIR", "/w")]),
            None,
        )
        .unwrap();
        assert_eq!(h.host, Host::Cursor);
        assert_eq!(h.event, Event::Activity);
    }

    #[test]
    fn codex_events_map() {
        let e = env(&[("CODEX_THREAD_ID", "t1")]);
        let h = detect(
            &codex("SessionStart", json!({"source": "startup"})),
            &e,
            None,
        )
        .unwrap();
        assert_eq!(h.host, Host::Codex);
        assert_eq!(h.event, Event::Start);
        assert_eq!(
            detect(&codex("UserPromptSubmit", json!({})), &e, None)
                .unwrap()
                .event,
            Event::Activity
        );
        assert_eq!(
            detect(&codex("SessionEnd", json!({"reason": "other"})), &e, None)
                .unwrap()
                .event,
            Event::End
        );
        // Without any host variable a PascalCase payload is Codex (Claude always
        // sets CLAUDE_PROJECT_DIR for hooks).
        let h = detect(&codex("SessionStart", json!({})), &env(&[]), None).unwrap();
        assert_eq!(h.host, Host::Codex);
        let h = detect(&codex("SessionStart", json!({})), &env(&[]), Some("codex")).unwrap();
        assert_eq!(h.host, Host::Codex);
    }

    #[test]
    fn forced_host_wins_and_is_validated() {
        let e = env(&[("CLAUDE_ENV_FILE", "/tmp/x.sh")]);
        assert_eq!(
            detect(&claude("SessionStart", json!({})), &e, Some("cursor"))
                .unwrap()
                .host,
            Host::Cursor
        );
        assert!(detect(&claude("SessionStart", json!({})), &e, Some("emacs")).is_err());
    }

    #[test]
    fn hostile_or_missing_session_ids_are_rejected() {
        let e = env(&[("CLAUDE_ENV_FILE", "/tmp/x.sh")]);
        let mut bad = claude("SessionStart", json!({}));
        bad["session_id"] = json!("x; rm -rf /");
        assert!(detect(&bad, &e, None).is_err());
        let mut none = claude("SessionStart", json!({}));
        none.as_object_mut().unwrap().remove("session_id");
        assert!(detect(&none, &e, None).is_err());
        assert!(detect(&json!("not an object"), &e, None).is_err());
    }
}
