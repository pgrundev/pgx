# Live validation: Codex (2026-10-25)

Status: **not run.** Codex CLI is not installed on the validation machine
(`npm i -g @openai/codex` is 0.164.0 at the time of writing) and a login is
required; nothing below is claimed as verified on a live Codex.

## What is verified without a live host

- The wrapper, which is the supported Codex path:
  `pgx agent exec -- <command>` gives the child its own branch in
  `DATABASE_URL`/`PG*`, forwards SIGINT/SIGTERM/SIGHUP, exits with the
  child's status (143 after SIGTERM), keeps the idle TTL alive, drops the
  branch afterwards (or retires it with `--keep`), and attaches to the
  surrounding session when `CODEX_SESSION_ID` is set, reusing one branch
  across calls. `crates/bin/pgx/tests/agent_exec.rs`, `e2e.sh` C and D.
- `pgx agent hook` with Codex's documented payloads (`session_id`, `cwd`,
  `permission_mode`, `transcript_path`, PascalCase events): `SessionStart`
  answered with `hookSpecificOutput.additionalContext` (never an `env`, which
  Codex does not support), the sandbox note when
  `CODEX_SANDBOX_NETWORK_DISABLED=1`, `UserPromptSubmit` touch, `SessionEnd`
  retirement. `tests/agent_hook.rs`.
- `pgx agent install codex` writing `.codex/hooks.json` with three events.
- The `skills` CLI installs the Skill for the `codex` agent into
  `.agents/skills/pgx`, which Codex reads natively (`e2e.sh` G).

## How to run it (a logged-in machine)

```sh
npm i -g @openai/codex && codex login
cd <project with .pgx.toml>
# 1. the wrapper: the branch is in Codex's environment
pgx agent exec -- codex exec --sandbox workspace-write \
  -c sandbox_workspace_write.network_access=true \
  'run: echo "BR=$PGX_BRANCH"; psql "$DATABASE_URL" -Atc "select current_database()"'
# 2. native hooks: trust the project hooks once (/hooks in the TUI), then
codex exec 'run: echo $CODEX_SESSION_ID; pgx agent exec -- psql "$DATABASE_URL" -Atc "select current_database()"'
pgx agent ls
```

Expected: in (1) `BR=agent-codex-<8 hex>` and the same `current_database()`;
in (2) the SessionStart context line about the branch appears in the
model's context, and the inner `pgx agent exec` reports
`agent-codex-<first 8 of CODEX_SESSION_ID>`. Without the sandbox setting the
psql call fails with a connection error and `CODEX_SANDBOX_NETWORK_DISABLED=1`
is set, which is the documented behaviour. Record the result here and update
the Codex row in `docs/pgx/local-dev/agents.md`.
