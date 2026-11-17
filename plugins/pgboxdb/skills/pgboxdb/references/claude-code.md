# Claude Code

Status: **verified live** (Claude Code 2.1.310, macOS): the SessionStart
hook's exports reached Bash tool commands, the session id matched
`CLAUDE_CODE_SESSION_ID`, PreToolUse and SessionEnd fired, SessionEnd fired on
a normal headless exit.

## Two ways to install the skill in Claude Code

- **Plugin (native)**: `/plugin marketplace add pgrundev/pgboxdb` then
  `/plugin install pgboxdb@pgrundev` inside a session (shell:
  `claude plugin marketplace add pgrundev/pgboxdb && claude plugin install
  pgboxdb@pgrundev`). The skill is namespaced `/pgboxdb:pgboxdb`; `claude plugin list`
  shows `pgboxdb@pgrundev`, `claude plugin update pgboxdb@pgrundev` upgrades it,
  `/plugin uninstall pgboxdb@pgrundev` removes it (the skill only: databases,
  templates, `.pgboxdb.toml` and project hooks are untouched).
- **Skills CLI**: `npx skills add pgrundev/pgboxdb --skill pgboxdb` copies the skill
  into `.claude/skills/pgboxdb` (plus `.agents/skills/pgboxdb` for Cursor and Codex).
  It loads as the plain skill `/pgboxdb`.

Both copies are the same skill. With both installed, Claude Code lists `/pgboxdb`
and `/pgboxdb:pgboxdb` side by side and either runs the same workflow; nothing is
duplicated at the database level because hooks and branches come from `pgboxdb
agent setup`, never from the skill files. To keep one copy, remove the Skills
CLI one with `npx skills remove pgboxdb` or the plugin with `/plugin uninstall
pgboxdb@pgrundev`.

The plugin contains no hooks: `pgboxdb agent setup` installs the project's hooks
into `.claude/settings.json` (idempotent, ownership-checked, see
[agent-lifecycle.md](agent-lifecycle.md)), so a plugin installed at user scope
does nothing in projects that have not opted in with `.pgboxdb.toml`.

## How it works

`pgboxdb agent setup` (or `pgboxdb agent install claude`) adds six entries to
`.claude/settings.json`:

- `SessionStart` → `pgboxdb agent up` for `session_id`, appends the nine
  `export` lines to `$CLAUDE_ENV_FILE` (Claude runs that file before every
  Bash command), prints the context line you see at the top of the session.
  Fires on startup, resume, `/clear`, compaction and fork; resume and
  compaction find the same branch; `/clear` and fork are new session ids and
  get new branches.
- `UserPromptSubmit` → `pgboxdb agent touch`.
- `PreToolUse` (matcher `Bash`): for the session's own commands, `touch`. For
  a subagent's command (the payload carries `agent_id`), the hook ensures the
  subagent's branch and answers with `updatedInput`: the command becomes

  ```
  /path/to/pgboxdb agent exec --agent <agent_id> --cwd <project> -- /bin/zsh -c '<original>' # pgboxdb-routed <branch>
  ```

  The wrapper attaches to the subagent's branch, puts its
  `DATABASE_URL`/`PG*` into the environment (overriding whatever the parent
  session exported, and clearing `PGSERVICE`), runs the original text in
  your shell and exits with its status. The original is one single-quoted
  argument, so quoting, pipes, heredocs, redirects, background jobs and the
  exit status are untouched, and no secret appears in the command text. If
  the branch cannot be ensured (PGBoxDB not initialised, source missing, bad
  `.pgboxdb.toml`, unusable `agent_id`) the hook answers `permissionDecision:
  deny` with the reason, so the command never runs against the parent's
  database; a stopped runtime is started on demand, as for `SessionStart`.
  The hook never answers `allow`: the rewritten command goes through Claude
  Code's normal permission flow, evaluated on the rewritten text. Add an
  allow rule for the wrapper if you do not want to confirm every subagent
  command, for example `Bash(/path/to/pgboxdb agent exec:*)`, where `/path/to/pgboxdb` is the absolute path the rewritten command starts with (for the installer, `~/.local/bin/pgboxdb` written out; `pgboxdb agent doctor` shows it) (the
  path is the `pgboxdb` the hook resolved; `pgboxdb agent doctor` prints it). In
  auto mode, Claude Code's classifier judges the rewritten command and may
  refuse one it would have allowed unwrapped; the same allow rule is the
  remedy.
- `SubagentStart` → `pgboxdb agent up` for `(session_id, agent_id)`: the
  subagent's own branch, cloned from the same source, named
  `agent-claude-<session8>-<agent8>`; the subagent reads a context line
  naming it. Starting the same subagent again (resume) finds the same branch.
- `SubagentStop` → `pgboxdb agent down` for that subagent: its backends are
  terminated (a statement it left running cannot commit later) and the
  branch is retired with the grace period. The parent's branch is untouched.
- `SessionEnd` → `pgboxdb agent down` (grace period). `claude --continue` within
  10 minutes finds the branch intact.

Why routing rather than environment: Claude Code offers `CLAUDE_ENV_FILE` to
`SessionStart` only, and a subagent's Bash runs with the parent's
environment, so without the `PreToolUse` rewrite every subagent would use
the parent's branch (the 2026-10-26 benchmark showed exactly that). Tools
other than Bash, MCP servers and processes the parent started before the
subagent existed are not routed: they see the parent's environment.

`claude --worktree <name>` starts a new session id in a new worktree and
therefore gets its own branch; `.pgboxdb.toml` must be committed so the worktree
sees it.

## In the session that ran setup

That session had no SessionStart after the hooks were installed, so its Bash
environment has no `DATABASE_URL` from PGBoxDB. Use `pgboxdb agent exec -- <cmd>`:
it attaches to this session (`CLAUDE_CODE_SESSION_ID`), creates the branch
on first use, and keeps it. Or tell the user to start a new session.

## Scope choices

`--scope project` (default) writes `.claude/settings.json`, shared through
git; teammates without PGBoxDB get a silent no-op. `--scope local` writes
`.claude/settings.local.json` (personal, gitignored by Claude Code).
`--scope user` writes `~/.claude/settings.json` for every project; then the
source comes from each project's `.pgboxdb.toml`.

## Trust and headless runs

Project hooks run in trusted folders; `claude -p` runs them regardless
(unless `--bare`). The hook touches only the local PGBoxDB home.

## Checking

`pgboxdb agent doctor` inside the session: the `session` line must say that
`DATABASE_URL` is the branch, `hooks:claude` must report 6 hooks (a
`partial installation` means the project was set up before the subagent
hooks existed: `pgboxdb agent install claude` adds them), and the `subagents`
line summarises the hook log. `echo $PGBOXDB_BRANCH` in Bash shows the branch;
inside a subagent, `echo $PGBOXDB_BRANCH $PGBOXDB_AGENT_ID $PGBOXDB_ROUTED` shows its
own branch, its id and `1`. `~/.pgboxdb/agent/hooks.log` lists every delivery
(`SubagentStart … -> created agent-claude-…`, `PreToolUse … -> routed to …`,
`SubagentStop … -> retired …`).
