# Live validation: Cursor (2026-10-25)

Status: **not run.** The Cursor CLI (`agent` 2026.01.23) is installed on the
validation machine but not logged in (`agent status` reports that
authentication is required, and `agent login` is interactive). No Cursor
session was executed, so nothing below is claimed as verified live.

## What is verified without a live host

- `pgx agent hook` with Cursor's documented payloads (`conversation_id`,
  `hook_event_name: sessionStart|beforeSubmitPrompt|afterShellExecution|
  sessionEnd`, `is_background_agent`, `workspace_roots`): branch creation,
  the `{"env": {...}, "additional_context": "..."}` answer, `{}` on activity
  hooks, retirement on `sessionEnd`, background agents skipped, a double
  delivery (native file plus Cursor's Claude-compat loader) yielding one
  branch, and the fail-closed sentinel. `crates/bin/pgx/tests/agent_hook.rs`.
- `pgx agent install cursor` / `setup` writing `.cursor/hooks.json`
  (`version: 1`, four events, touch hooks on non-permission events) and
  leaving foreign entries intact. `tests/agent_install.rs`, `e2e.sh` G.
- Cursor's response validator accepts `permission` as optional, so `{}` is a
  valid answer (inspected in the CLI bundle).
- The `skills` CLI installs the Skill for the `cursor` agent into
  `.agents/skills/pgx`, which Cursor reads natively (`e2e.sh` G).

## What remains unverified

Whether the `sessionStart` hook's `env` object reaches the Shell tool's
commands in a real Cursor conversation. Cursor's docs and staff say it does,
and the CLI bundle contains the code path (`setSessionEnvironment`), but it
has not been observed here. Until it is, the Skill and `agents.md` describe
Cursor as "hooks installed; use `pgx agent exec -- <command>` when
`PGX_ISOLATION` is unset".

## How to run it (a logged-in machine, about two minutes)

```sh
agent login                                   # once, interactive
cd <a project with .pgx.toml and .cursor/hooks.json from pgx agent setup>
agent -p -f --output-format text 'Run: echo "BR=$PGX_BRANCH ISO=$PGX_ISOLATION"; psql "$DATABASE_URL" -Atc "select current_database()"'
pgx agent ls                                  # the branch for that chat, state active/retired
```

Expected: `BR=agent-cursor-<8 chars of the chat id> ISO=ok` and the same name
from `current_database()`. Run two `agent -p` in parallel to confirm
isolation; reopen a stopped chat to observe the documented limitation (no new
`sessionStart`, env not re-applied). Record the result in this file and flip
the Cursor row in `docs/pgx/local-dev/agents.md` to "verified".
