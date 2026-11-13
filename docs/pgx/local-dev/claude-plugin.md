# The PGX plugin for Claude Code

**One plugin. Every Claude Code agent gets its own Postgres.**

The plugin packages the PGX Agent Skill (`skills/pgx/`, the canonical
implementation) for Claude Code's plugin manager. It is published in two
places:

- **`pgrundev/pgx-plugin`** (https://github.com/pgrundev/pgx-plugin): a
  lightweight repository that holds only the plugin, about 110 KB in 19
  files. It is generated from this repository (see
  [The standalone repository](#the-standalone-repository-pgrundevpgx-plugin))
  and is what Anthropic's plugin directory reads. Install from here.
- **This repository**, as a marketplace named `pgrundev`
  (`.claude-plugin/marketplace.json`) with the plugin at `plugins/pgx/`. It
  keeps working for everyone who installed `pgx@pgrundev`, but adding it as a
  marketplace clones the whole repository (about 2 GB).

Nothing else changes: the Skill in `skills/pgx/` stays the canonical
implementation (`plugins/pgx/skills/pgx` is a generated copy, `plugins/sync.sh
--check` fails CI on any difference) and the Skills CLI install keeps working.

## Install, inside Claude Code

```text
/plugin marketplace add pgrundev/pgx-plugin
/plugin install pgx@pgx-plugin
```

The second command opens the plugin's page in the `/plugin` panel. Choose a
scope:

| Scope | Who gets it | Written to |
|---|---|---|
| user | you, in every project on this machine | `~/.claude/settings.json` |
| project | everyone who works in this repository | `.claude/settings.json` (committed) |
| local | you, in this repository only | `.claude/settings.local.json` |

From a shell, without a session, one command adds the marketplace and
installs the plugin:

```sh
claude plugin install pgx --marketplace pgrundev/pgx-plugin   # add --scope user|project|local
```

Claude Code has no command that installs a plugin straight from a repository
without a marketplace (`claude plugin install pgrundev/pgx-plugin` answers
`not found in any configured marketplace`), which is why `pgx-plugin`
carries a one-entry marketplace manifest whose plugin source is its own root
(`"source": "./"`). `claude plugin validate --profile directory` confirms
that the directory treats such a folder as the plugin.

The original route is unchanged: `/plugin marketplace add pgrundev/pgx` then
`/plugin install pgx@pgrundev`. `pgx@pgx-plugin` and `pgx@pgrundev` are the
same plugin and both load the skill as `/pgx:pgx`; install from one
marketplace only. Nobody who installed `pgx@pgrundev` has to do anything: the
`pgrundev` marketplace entry still points at `./plugins/pgx` and `claude
plugin update pgx@pgrundev` keeps working.

Then, in a project with a database:

```text
/pgx:pgx Set up isolated Postgres databases for this project.
```

`pgx:pgx` is the skill's name under the plugin's namespace. You can also just
describe database work ("add a column and run the tests"); Claude selects
the skill when it is relevant. Both routes run the same Skill: detect the
project, check for PGX 0.1.2 or newer and offer to install it, prepare a
schema-only template, `pgx agent setup`, then work inside the session's
branch. See [agents.md](agents.md) for what that gives you.

Check what is installed:

```sh
claude plugin list             # pgx@pgx-plugin  Version: 0.1.3  Status: enabled
claude plugin details pgx      # Component inventory: Skills (1) pgx, Hooks (0), MCP servers (0)
```

## What the plugin contains

- `skills/pgx/`: the Skill, its eleven reference documents and two helper
  scripts, identical to `skills/pgx/` at the repository root.
- `.claude-plugin/plugin.json`: name `pgx`, version `0.1.3` (the Skill's
  version; it moves independently of the PGX runtime version, which the
  skill requires to be 0.1.2 or newer), display name, links.
- `README.md`.

In `pgrundev/pgx-plugin` additionally `LICENSE` (the AGPL-3.0 text the
directory requires), `.claude-plugin/marketplace.json` (the one-entry
marketplace) and `.github/workflows/validate.yml` (its own checks). About
110 KB, 19 files; roughly 190 tokens of context in every session while
installed (`claude plugin details pgx`).

It contains **no hooks, MCP servers, commands or binaries**. Installing it at
user scope does nothing in projects that have not opted in: the Skill is
available, and database infrastructure appears only when you ask for it in a
project and `pgx agent setup` writes that project's `.pgx.toml` and hooks.
The plugin is instructions and integration logic; the PGX runtime (`pgx`,
0.1.2 or newer) is installed separately, and a project still needs its
one-time `pgx agent setup`.

## Hooks: one owner, no duplicates

Lifecycle hooks (`SessionStart` → branch, `UserPromptSubmit`/`PreToolUse` →
keep-alive and subagent routing, `SubagentStart`/`SubagentStop` → a branch
per subagent, `SessionEnd` → retire) are installed per project by `pgx agent
setup`, which is idempotent and recognises its own entries by their command
string. The plugin never registers hooks of its own, so there is exactly one
source of hooks whichever way the Skill was installed, and installing,
updating, disabling or removing the plugin never touches `.claude/settings.json`
(tested in `plugins/tests/pgx/run.sh`). Fail-closed behaviour is unchanged:
it lives in `pgx agent hook`, not in the Skill files.

## The plugin and the Skills CLI together

`npx skills add pgrundev/pgx --skill pgx` copies the Skill into
`.claude/skills/pgx` (and `.agents/skills/pgx`). That copy is a plain skill
named `pgx`. With the plugin installed as well, Claude Code lists both
`/pgx` and `/pgx:pgx` (verified: the session inventory shows the two names
side by side; neither hides the other). They are the same text, so whichever
Claude picks runs the same workflow, and nothing is duplicated at the
database level because branches and hooks come from `pgx agent`, not from
the skill files. The cost is two descriptions in context.

To keep one copy, either remove the Skills CLI one (`npx skills remove pgx`,
which deletes only `.claude/skills/pgx` and `.agents/skills/pgx`) or the
plugin (`/plugin uninstall pgx@pgx-plugin`). Switching from the Skills CLI to
the plugin is those two steps; nothing in `.pgx.toml`, the hooks or the
databases changes.

Cursor and Codex are not Claude Code plugins hosts: keep using the Skills
CLI for them. The PGX runtime itself is independent of both.

## Update, disable, remove

| Action | In a session | In a shell |
|---|---|---|
| Update | `/plugin` → Installed → pgx → Update now | `claude plugin update pgx@pgx-plugin` |
| Disable / enable | `/plugin` → Installed → Space | `claude plugin disable pgx@pgx-plugin` / `enable` |
| Remove | `/plugin uninstall pgx@pgx-plugin` | `claude plugin uninstall pgx@pgx-plugin` |
| Remove the marketplace | `/plugin marketplace remove pgx-plugin` | `claude plugin marketplace remove pgx-plugin` |

(Replace `pgx-plugin` with `pgrundev` for an installation from this
repository.) Updates follow the plugin's `version` field: a new PGX Skill
release bumps it in `plugins/pgx/.claude-plugin/plugin.json` and
`skills/pgx/SKILL.md`, CI publishes the result to `pgrundev/pgx-plugin`, and
`claude plugin update` (or the marketplace's auto-update, off by default for
third-party marketplaces) fetches it; users on an older copy keep working.
Once the plugin is listed in Anthropic's directory, the directory follows
`pgx-plugin`'s `main` branch and serves each published version to the people
who installed it from there.

Uninstalling removes the Skill files from Claude Code's plugin cache and the
`enabledPlugins` entry, nothing else: PGX databases, templates, `.pgx.toml`,
the PGX runtime and any hooks `pgx agent setup` installed all stay.
`pgx agent uninstall` removes a project's hooks when you want that.

## Performance and what gets downloaded

Measured on 2026-10-26 (Claude Code 2.1.311, macOS, fresh `CLAUDE_CONFIG_DIR`):

| | `pgrundev/pgx-plugin` | this repository (`pgrundev/pgx`) |
|---|---|---|
| `claude plugin marketplace add` | 4.0 s | 65 s |
| Marketplace clone on disk | 316 KB (`.git` 172 KB) | 2.2 GB (`.git` 178 MB, the fuzzing corpora and benchmark results checked out) |
| `claude plugin install` | under 1 s | under 1 s |
| Installed plugin in the cache | 19 files, 110 KB | 16 files, 96 KB |
| `claude plugin install pgx --marketplace …` (both steps) | 4.0 s | not measured |

Claude Code clones a marketplace once per machine into
`~/.claude/plugins/marketplaces/<name>/` and copies the plugin out of that
clone; `claude plugin marketplace update` afterwards is incremental. From a
shell the large clone can be avoided with `--sparse .claude-plugin plugins`
(8 s, 72 MB measured earlier), but the in-session `/plugin marketplace add`
has no sparse option, which is what the lightweight repository solves.

## The standalone repository (`pgrundev/pgx-plugin`)

`pgrundev/pgx-plugin` is generated; nothing in it is edited by hand. Its
whole tree is, exactly:

| In `pgx-plugin` | From this repository |
|---|---|
| `.claude-plugin/plugin.json`, `README.md`, `skills/pgx/**` | `plugins/pgx/` (the plugin package; `skills/pgx` inside it is the synced canonical Skill) |
| `LICENSE` | `LICENSE` |
| `.claude-plugin/marketplace.json`, `.github/workflows/validate.yml` | `plugins/standalone/` (the overlay only that repository needs) |

```sh
plugins/sync.sh --standalone <dir>         # assemble (replaces everything in <dir> except .git)
plugins/sync.sh --check-standalone <dir>   # exit 1 if a pgx-plugin checkout differs from the assembly
```

**Publishing.** `.github/workflows/publish-plugin.yml` runs on every push to
`main` that touches `skills/pgx/**`, `plugins/**` or `LICENSE` (and as a
dry run on pull requests that touch them): it checks the skill copy is in
sync, assembles the tree, runs `claude plugin validate --strict` and the
directory profile on it, diffs it against the live `pgx-plugin` `main`, and,
only on `main` of `pgrundev/pgx` and only when something differs, commits the
new tree to `pgx-plugin` with a regular push (never a force-push), tagging
`pgx--v<version>` the first time a version is published. Feature-branch
content is never published; a manual `workflow_dispatch` defaults to
`dry_run=true`. The publish job runs in the `plugin-publish` GitHub
environment, so required reviewers can be added to it. `pgx-plugin` never
pushes back, so there is no loop.

**Authentication (one-time setup, not yet configured).** The publish job
needs a credential that can push to `pgrundev/pgx-plugin` and nothing else.
No personal access token is used. Configure one of these as repository
secrets of `pgrundev/pgx` (Settings → Secrets and variables → Actions); the
job stops at its authentication step and prints these instructions while
neither exists:

1. *GitHub App (preferred).* Create an app under the `pgrundev` organization
   with the repository permission **Contents: Read and write** only, install
   it on `pgrundev/pgx-plugin` only, and add `PGX_PLUGIN_APP_ID` and
   `PGX_PLUGIN_APP_PRIVATE_KEY` (the PEM). The workflow mints a short-lived
   installation token with `actions/create-github-app-token` scoped to that
   one repository.
2. *Deploy key.* `ssh-keygen -t ed25519 -f pgx_plugin_deploy -N '' -C
   publish-plugin`; add the public half to `pgrundev/pgx-plugin` (Settings →
   Deploy keys, allow write) and the private file as `PGX_PLUGIN_DEPLOY_KEY`.

**Drift.** `pgx-plugin`'s own workflow (`validate.yml`) runs on every push,
on pull requests and daily: `claude plugin validate --strict`, the directory
profile, package hygiene (no symlinks, system files, hooks, binaries or
secrets; the directory's file-count and file-size limits), the helper
scripts, an install cycle from the checkout in an isolated configuration,
gitleaks, and `plugins/sync.sh --check-standalone` against this repository's
`main`. A hand edit in `pgx-plugin` or an unpublished change here fails that
check, so the two cannot diverge silently. The same assembly is tested here
in `plugins/tests/pgx/run.sh`.

## Anthropic's plugin directory

The marketplaces above are self-hosted. **PGX is not listed in Anthropic's
directory and no submission has been accepted.** A submission of this
repository with plugin path `plugins/pgx` was refused by the directory's
validator ("Folders on the way to the plugin are too large to read": the
folders leading to the plugin hold more entries, longer names or more levels
than it reads), which is why `pgrundev/pgx-plugin` exists: the plugin at the
root of a 19-file repository. `claude plugin validate --profile directory`
on it reports no blocking finding (one warning: no icon). Submission is a
separate, human-approved step through the developer portal at
https://claude.ai/directory/manage (a paid claude.ai plan, GitHub account
connected with push access to the repository); the portal applies rules the
CLI does not check and a security scan. Values for the form:

| Field | Value |
|---|---|
| Submission type | Plugin bundle |
| Repository | `pgrundev/pgx-plugin` (https://github.com/pgrundev/pgx-plugin) |
| Plugin path | *(empty: the repository root)* |
| Branch or tag | *(empty: follows `main`)* |
| Plugin name / display name | `pgx` / "PGX — Postgres for Agents" (read from `plugin.json`) |
| Description | PGX gives every Claude Code agent its own isolated, disposable PostgreSQL-compatible database. Agents can run migrations, tests, and experiments in parallel without database conflicts. No Docker or cloud account required. Built in Rust. |
| Category | Developer tools, or Databases if the form offers it |
| Version | 0.1.3 (plugin and skill); requires PGX runtime 0.1.2 or newer |
| License | AGPL-3.0 (`LICENSE` in the repository, `license` in `plugin.json`) |
| Documentation / support | https://github.com/pgrundev/pgx (`documentationUrl`, `supportUrl` in `plugin.json`) |
| Icon | none yet; the directory takes the icon from the first submission only, so add a square PNG (512–2048 px) as `.claude-plugin/icon.png` in `plugins/pgx` before submitting if one is wanted |
| Data handling | reads no personal data; sends nothing to any service (everything runs on the user's machine against the local PGX runtime); keeps nothing beyond the project's `.pgx.toml` and the PGX databases the user creates; not intended for people under 18 specifically |

For the security review: the plugin ships one Agent Skill and two shell
scripts, no hooks, MCP servers, binaries or package installs, and runs
nothing until the user invokes the skill. The Skill runs the `pgx` CLI
locally; it installs the PGX runtime only after asking (`npm i -g pgxdb` or
the pgx.dev installer, both verified release artifacts of this repository).
Database credentials for agent branches are generated by PGX and exported
into the session by hooks that `pgx agent setup` installs in the project on
request; the Skill never prints them. An existing PostgreSQL is read with
`pg_dump --schema-only` to build a template; its data is copied only with
explicit approval. Branches expire on an idle TTL plus a grace period and
the PGX supervisor removes them; if isolation cannot be established the
session's `DATABASE_URL` is pointed at an unreachable sentinel rather than at
a shared database (`skills/pgx/references/security.md`). PGX is a PGRunDev
project, not an Anthropic one.

## Live validation

- 2026-10-26, from this repository's marketplace: three headless Claude Code
  sessions in a Rails project, plus the interactive `/plugin` flow:
  [validation/2026-10-26-claude-plugin-live.md](validation/2026-10-26-claude-plugin-live.md).
- 2026-10-26, subagent isolation (3, then 20 concurrent subagents):
  [validation/2026-10-26-subagent-isolation-live.md](validation/2026-10-26-subagent-isolation-live.md).
- 2026-10-26, from `pgrundev/pgx-plugin`: install from GitHub, setup through
  `/pgx:pgx`, three concurrent subagents in their own branches, developer
  database unchanged:
  [validation/2026-10-26-pgx-plugin-standalone-live.md](validation/2026-10-26-pgx-plugin-standalone-live.md).

## Tests

`plugins/tests/pgx/run.sh` (CI job `plugin`): drift check against
`skills/pgx`, version agreement, packaging hygiene (no hooks, binaries,
symlinks, parent paths, absolute paths, credentials), `claude plugin validate
--strict` on the plugin and the marketplace, the directory profile, and a
full install → list → details → disable → enable → update → project/local
scope → uninstall → reinstall → marketplace remove cycle from the repository
as a local marketplace in an isolated `CLAUDE_CONFIG_DIR`, plus the
"existing hooks untouched" and "Skills CLI copy alongside" cases. Then the
same for the standalone tree: `plugins/sync.sh --standalone` and
`--check-standalone` (including that drift is detected), byte-identity of
the skill, manifest and licence with the canonical files, validation of the
root as plugin plus marketplace, and an install cycle from a git clone of
the assembly as `pgx@pgx-plugin`. The Skills CLI contract
(`skills/tests/pgx/run.sh`, `skills-ref validate`) is unchanged and still
runs.
