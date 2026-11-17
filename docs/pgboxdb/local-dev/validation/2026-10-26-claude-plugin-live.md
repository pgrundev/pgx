# Claude Code plugin: live validation (2026-10-26)

Host: Claude Code 2.1.310, macOS 14 arm64. PGX: the published v0.1.1
darwin-arm64 artifact. Project: `tests/apps/rails-agent` (Rails 8.1), fresh
copy with a `.env` pointing at a scratch PostgreSQL 18 holding
`rails_agent_development` (users table, two rows). Plugin installed from the
public branch `pgrundev/pgx#feat/claude-plugin-marketplace`; no Skills CLI
install in this project. Claude Code's plugin root was isolated with
`CLAUDE_CODE_PLUGIN_CACHE_DIR` and the plugin installed at local scope, so the
developer's own `~/.claude` was not touched.

## Installation from GitHub

| Step | Result |
|---|---|
| `claude plugin marketplace add pgrundev/pgx#<branch>` (full clone) | 53 s; shallow clone, 2.2 GB on disk (`.git` 192 MB); `Successfully added marketplace: pgrundev` |
| same with `--sparse .claude-plugin plugins` | 8 s, 72 MB |
| `claude plugin install pgx@pgrundev --scope local` | `Successfully installed plugin: pgx@pgrundev (scope: local)`; installed copy 84 KB, 16 files, nothing from `crates/`, `fuzz/` or `benchmarks/` |
| `claude plugin details pgx` | `Skills (1) pgx`, `Agents (0)`, `Hooks (0)`, `MCP servers (0)`; always-on ~190–270 tokens |
| `claude plugin update pgx@pgrundev` | `already at the latest version (0.1.1)` |

Interactive, in a real session (driven through a pseudo-terminal, screens
recorded): `/plugin marketplace add <source>` → `Successfully added
marketplace: pgrundev`; `/plugin install pgx@pgrundev` opened the plugin's
details with the three scope options; choosing *local* printed `✓ Installed
PGX — Postgres for Agents. Plugin is now active.`; `/plugin` → Installed
lists `PGX — Postgres for Agents · pgrundev · 1 skill`; typing `/pgx:`
offers `/pgx:pgx`; `/pgx:pgx …` answered with the skill name and its base
directory under the plugin cache.

## Three headless sessions through the plugin

| # | Prompt | Turns | Cost | Result |
|---|---|---|---|---|
| 1 | "Use the PGX plugin to set up isolated Postgres databases for this project." | 10 | $0.88 | Loaded `pgx:pgx` (Skill tool), ran the plugin's `detect-project.sh` and `check-pgx.sh` from `…/plugins/cache/pgrundev/pgx/0.1.1/skills/pgx/scripts/` (the `${CLAUDE_PLUGIN_ROOT}` path, substituted), classified the project `local`, `pgx run`, `pgx copy --schema-only` into `rails_agent_template`, `pgx agent setup --source rails_agent_template` (hooks for Claude Code, Cursor, Codex installed exactly once; `.pgx.toml` written), `pgx agent exec -- bin/rails db:migrate` in its branch `agent-claude-72d1102e`. 123 s. |
| 2 | new session: "Add a last_login_at timestamp to users and run the tests." | 8 | $0.64 | Branch `agent-claude-637bdc7d` provided by the SessionStart hook; migration written, `bin/rails test`: 3 runs, 5 assertions, 0 failures. `last_login_at` exists in the branch only: template 0, developer PostgreSQL 0. 81 s. |
| 3 | new session: print `PGX_BRANCH`, `PGX_ISOLATION`, the database name, `pgx agent ls` | 2 | $0.35 | `PGX_BRANCH=agent-claude-94863d2f`, `PGX_ISOLATION=ok`, `DATABASE_URL` names the same branch; `pgx agent ls` showed it active beside the two earlier branches, both `retired` with their grace period running. 13 s. |

Total $1.87. The developer PostgreSQL's schema dump is byte-identical before
and after (ignoring pg_dump's per-run `\restrict` token) and its write
statistics show no inserts, updates or deletes beyond the fixture's own
rows. Each session ended with its branch `retired`; the supervisor removed
them after the 10-minute grace period (see the cleanup line below).

Removing the plugin afterwards (`claude plugin uninstall pgx@pgrundev
--scope local`) left `pgx ls` (template, `app`, `postgres`), `.pgx.toml`
and `.claude/settings.json` unchanged, and `pgx agent doctor` kept working;
reinstalling restored the plugin.

## Coexistence with the Skills CLI copy

In a separate project with the plugin installed, `npx skills add … --skill
pgx -a claude-code` added `.claude/skills/pgx`. The session's inventory
(`--output-format stream-json`, `init` event) then listed both `pgx` and
`pgx:pgx` under `skills` and `slash_commands`; with the plugin alone it
listed `pgx:pgx` only. Neither copy hides the other, and `claude plugin list`
is unaffected. Hooks are not involved in either copy.

## Cleanup

After the sessions ended, `pgx agent ls` listed the three branches as
`retired`; within 10 minutes of each session's end the supervisor dropped
them (`pgx agent ls`: no agent branches; `pgx ls`: `app`, `postgres`,
`rails_agent_template` only). Nothing was deleted by hand.
