# Cursor (IDE agent and the `agent` CLI)

Status: **hooks installed and tested with Cursor's payload format; live
propagation to the Shell tool not yet observed on a logged-in Cursor.**
Cursor's documentation and staff state that the `sessionStart` hook's `env`
object is injected into the agent's shell tool for that conversation, and the
CLI bundle contains that code path. Until a live run confirms it in this
repository's validation notes, treat Cursor as "hooks plus wrapper".

## How it works

`pgboxdb agent setup` adds to `.cursor/hooks.json` (`version: 1`):
`sessionStart` → `up`, answered with `{"env": {…nine variables…},
"additional_context": "PGBoxDB: …"}`; `beforeSubmitPrompt` and
`afterShellExecution` → `touch` (answered `{}`); `sessionEnd` → `down`
(grace). The touch hooks are on non-permission events on purpose.

Cursor also loads Claude Code hook files (`.claude/settings*.json`). The
hook detects Cursor's payload (`conversation_id`) regardless of which file
delivered it, and a per-session lock makes a double delivery produce one
branch.

## Known limits (from Cursor's own notes)

- `sessionStart` does not fire again when a stopped chat is reopened: the
  environment is gone. If `PGBOXDB_ISOLATION` is unset in the agent's shell, use
  `pgboxdb agent exec -- <command>` for database work (each call is its own
  short-lived branch unless `--id` names one) or ask the user to start a new
  chat.
- Cursor subagents do not inherit the environment.
- Cloud/background agents run elsewhere; the hook skips them
  (`is_background_agent`). Out of scope.
- `sessionEnd` is reported to fire mainly on window/tab close; the idle TTL
  is the real cleanup.

## The `agent` CLI

`agent -p "…"` inherits the launching shell's environment and runs
hooks, so `pgboxdb agent exec -- agent -p "…"` is the guaranteed path: the
wrapper's branch is in the CLI's environment for the whole run.

## Checking

Ask the agent to run `echo $PGBOXDB_BRANCH $PGBOXDB_ISOLATION` and
`pgboxdb agent doctor`. A branch name and `ok` mean the hook worked.
