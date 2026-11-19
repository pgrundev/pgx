# Troubleshooting

Start with `pgboxdb agent doctor` (project state, this session) and `pgboxdb doctor`
(the runtime). Both print one line per check with a hint.

| Symptom | Cause | Fix |
|---|---|---|
| `PGBOXDB_ISOLATION=failed`, `DATABASE_URL` points at port 1 | The hook could not create a branch and failed closed. `PGBOXDB_ISOLATION_REASON` says why. | Fix the reason (below), then start a new session. Do not run database commands in this one. |
| `source "x" does not exist` | `.pgboxdb.toml` names a template that is not in PGBoxDB (new machine, dropped). | `pgboxdb copy --from … --to x --schema-only` (or `pgboxdb create x`), or change `source`. |
| `source … has N active connections; a branch source must be idle` | Something connects to the template directly. | Disconnect it; keep a dedicated `<app>_template` nobody uses. |
| `PGBoxDB is not initialised` | PGBoxDB was never set up on this machine. | `pgboxdb agent setup`. |
| Context line present but `echo $DATABASE_URL` is empty (Claude Code) | A Bash tool issue, or the env file was cleared by a `cd`-driven hook. | `pgboxdb agent env` and `eval` it; new session. Check `~/.claude/session-env/<session>/`. |
| Subagents all use the parent's branch | The project's hooks predate subagent support (`pgboxdb agent doctor` → `hooks:claude: partial installation`), or PGBoxDB is older than the hooks. | `pgboxdb agent install claude` (adds `SubagentStart`/`SubagentStop`), upgrade PGBoxDB, start a new session. `~/.pgboxdb/agent/hooks.log` must show `SubagentStart … -> created` and `PreToolUse … -> routed`. |
| A subagent's Bash command is refused with `PGBoxDB: …` | Its branch could not be ensured: runtime stopped, source missing, bad `.pgboxdb.toml`, unusable `agent_id`. This is the fail-closed path. | Fix the reason from the message (`pgboxdb agent doctor`); the subagent cannot fix it itself and must not try another database. |
| Every subagent Bash command asks for permission, or auto mode refuses them (`Auto-Mode Bypass`, `Traffic Redirection`) | Permission rules and the auto-mode classifier are evaluated on the rewritten command (`<pgboxdb> agent exec --agent … -- <shell> -c '…'`), not on the text the model wrote. | The user's decision: allow the wrapper once with a rule such as `Bash(/path/to/pgboxdb agent exec:*)`, where `/path/to/pgboxdb` is the absolute path the rewritten command starts with (for the installer, `~/.local/bin/pgboxdb` written out; `pgboxdb agent doctor` shows it) (the path is the one at the start of the rewritten command). The hook itself never approves anything. |
| A routed command fails with `no branch for claude session … agent …` or exit 3 before running | The wrapper could not attach: PGBoxDB not initialised in the shell's `PGBOXDB_HOME`, or the hook and the shell see different homes. | `pgboxdb agent doctor` in the same shell; make `PGBOXDB_HOME` consistent. |
| `pgboxdb agent down --id x` says `no branch for exec session x` for an old branch | Branches made by `pgboxdb agent exec --id` before 0.1.2 were labelled by the command's name (`sh`, `bash`, …). | `pgboxdb agent ls` shows the host label; `pgboxdb agent down --id x --host <label>`, or `pgboxdb drop <name> --force`. New `exec` branches are always labelled `exec`. |
| No context line at all | Hooks not installed, PATH problem in the hook, or an untrusted folder. | `pgboxdb agent doctor` → `hooks:<host>`; `pgboxdb agent install <host>` reports the self-test and the `pgboxdb` it resolved. Trust the folder. |
| Hook reports `pgboxdb not found on the hook's PATH` | Editor launched from the Dock without the shell's PATH, `pgboxdb` in an unusual place. | Link it: `ln -s "$(command -v pgboxdb)" ~/.local/bin/pgboxdb`. |
| Two branches for one Cursor chat | Older PGBoxDB before the per-session lock, or two different project roots. | Upgrade; make sure `.pgboxdb.toml` is at the repository root. |
| Branch disappeared mid-work | Idle TTL passed (no prompt or command for 2h) or the session ended more than 10 minutes ago. | The next session start makes a fresh one ("fresh" in the context line). Raise `idle_ttl` in `.pgboxdb.toml` if needed. |
| `pgboxdb copy` refuses an extension | The schema needs something PGBoxDB does not bundle. | See postgres-import.md; tell the user exactly what. |
| Codex commands cannot connect, `CODEX_SANDBOX_NETWORK_DISABLED=1` | Sandbox blocks loopback. | `sandbox_workspace_write.network_access = true` (user's decision). |
| Rails created `agent-…-0/1/2` databases | Parallel tests. | Harmless; `PARALLEL_WORKERS=1` or `pgboxdb drop` them. |
| `ActiveRecord::EnvironmentMismatchError` after a green test run | The branch's `ar_internal_metadata` says `development`; Rails' `db:test:prepare` refuses to purge it. | Harmless; ignore. Never `db:environment:set` inside a branch. |
| Rails re-ran every migration on a fresh branch and failed on an existing table | The template was imported schema-only with an older `pgboxdb copy` that dropped `schema_migrations` rows. | Re-import with any PGBoxDB release (its `--schema-only` keeps migration bookkeeping) or insert the rows into the template once. |
| Port conflict on first run | Another service on the chosen port. | `pgboxdb doctor`; `PGBOXDB_PORT=<free> pgboxdb run` on a fresh home only; never `init --force`. |

Logs: `~/.pgboxdb/log/` (runtime and supervisor); `~/.pgboxdb/agent/hooks.log` (one
line per hook delivery: host, event, session, agent, tool, outcome; no
values). Records of agent branches: `~/.pgboxdb/ttl/*.json` (0600, no secrets).
