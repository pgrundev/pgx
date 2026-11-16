# Upgrading an installation made before the rename to PGBoxDB

PGBoxDB is the new name of this runtime. Its command is `pgboxdb`, its npm
package is `pgboxdb`, its data home is `~/.pgboxdb` and its environment
variables start with `PGBOXDB_`. PGBoxDB does not read the previous names: it
installs no alias command and reads none of the previous environment variables
or project files as configuration.

It does not ignore your existing databases either. If an initialised data home
from before the rename exists (`~/.pgx`, or the directory its home variable
pointed at), PGBoxDB will not quietly start a second, empty one beside it:

```text
$ pgboxdb run
pgboxdb: found databases from an installation made before the rename to PGBoxDB at /Users/you/.pgx; PGBoxDB did not create a new, empty home at /Users/you/.pgboxdb
hint: `pgboxdb migrate` shows how to adopt them …
```

You choose one of three paths.

## 1. Adopt it: `pgboxdb migrate` (recommended)

```bash
pgboxdb migrate            # dry run: lists every step, changes nothing
# stop the old runtime with the CLI that started it, then:
pgboxdb migrate --apply
pgboxdb start
```

Run it from inside a project to also carry over that project's files.
`--apply` does the following, and nothing else:

| Step | What happens | What is never done |
|---|---|---|
| data home | `~/.pgx` is renamed to `~/.pgboxdb` with a single `rename(2)`; a record `migrated-from.json` is written inside | no copy, no delete, no merge: refused while its runtime runs, when `~/.pgboxdb` exists (an empty directory counts as free), or across filesystems |
| config.json | paths inside it that point into the old home are rewritten | the previous file is kept as `config.json.pre-migrate` |
| cluster settings | nothing: the cluster's `postgresql.conf` keeps including its generated settings file under its original name, and `pgboxdb start` keeps regenerating that file | `postgresql.conf` is never rewritten |
| project file | `.pgx.toml` is copied to `.pgboxdb.toml` (comments updated, values unchanged) | `.pgx.toml` stays; delete it once everyone working on the project has upgraded |
| Git identity | `.git/pgx-repo-id` and `pgx-worktree-id` are copied to `pgboxdb-repo-id` / `pgboxdb-worktree-id` | the originals stay; branch databases stay mapped (even without this step PGBoxDB reads the old id files) |
| agent hooks | reported with the command that replaces them: `pgboxdb agent install <host> --scope <scope>` | the old entries are replaced in place by that command, never kept beside the new ones (a session would get two databases) |
| Git hooks | reported: run `pgboxdb git init` in the repository | the old hook block is replaced; other hook content is untouched |
| shell integration | reported: `pgboxdb git install-shell <shell>` | the old rc block is replaced; the rest of the rc file is untouched |

`pgboxdb migrate --apply` is idempotent: run again, it reports "already
migrated". If any step is blocked, nothing is applied.

### Rolling back

```bash
pgboxdb stop
pgboxdb migrate --rollback            # dry run
pgboxdb migrate --rollback --apply    # renames ~/.pgboxdb back to where it came from
```

Rollback needs the record written by the migration, the runtime stopped, and
the original location free. It restores `config.json.pre-migrate`. The
previous CLI then starts the cluster again with every database. Files copied
into projects stay (they are ignored by the previous CLI). Agent hooks that
`pgboxdb agent install` replaced are restored by the previous CLI's own
`agent install`.

The binaries the previous installer or npm package put under `~/.pgx/dist/`
move with the home. Its command (e.g. `~/.local/bin/pgx`) stops resolving
after `--apply` and resolves again after a rollback.

## 2. Keep it where it is: `PGBOXDB_HOME`

```bash
export PGBOXDB_HOME=~/.pgx
pgboxdb start
```

PGBoxDB uses the existing home in place. Nothing is moved. Project and Git
files are handled as in path 1 (`pgboxdb migrate` with `PGBOXDB_HOME` set does
only the project steps).

## 3. Start fresh

```bash
export PGBOXDB_HOME=~/.pgboxdb
pgboxdb run
```

Setting `PGBOXDB_HOME` explicitly is the way to say "a separate installation".
The old home is left untouched. `pgboxdb doctor` keeps reminding you that it is
there.

## What changed for scripts and integrations

| Before | Now | Note |
|---|---|---|
| `pgx …` | `pgboxdb …` | same subcommands, flags, output and exit codes |
| `npx pgxdb@latest` | `npx pgboxdb@latest` | new package; the old one is not changed by this release |
| `PGX_HOME`, `PGX_PORT`, `PGX_RUN_DIR`, `PGX_DURABILITY`, `PGX_LISTEN`, `PGX_PASSWORD`, `PGX_PG_BINDIR`, `PGX_SOURCE`, `PGX_TTL_INTERVAL`, `PGX_GIT`, `PGX_GIT_SKIP` | `PGBOXDB_…` | the old names are not read; `init`, `run`, `start`, `status`, `doctor` and `migrate` print a note when one is set |
| variables exported into agent sessions and `git exec` (`PGX_AGENT_ID`, `PGX_BRANCH`, `PGX_GIT_BRANCH`, `PGX_ISOLATION`, …) | `PGBOXDB_…` | scripts that read them must be updated |
| `pgx --json version` key `pgx_version`, `share/pgx/dist.json` | `pgboxdb_version`, `share/pgboxdb/dist.json` | |
| new Git databases `pgx_<repo>_…` | `pgboxdb_<repo>_…` | existing databases keep their names and mappings |
| Docker volume `/var/lib/pgx`, image user `pgx` | `/var/lib/pgboxdb`, user `pgboxdb` (same uid 10001) | mount an existing volume at `/var/lib/pgboxdb`; the data inside works unchanged |
| Claude Code plugin `pgx` | plugin `pgboxdb` | install the new plugin; uninstall the old one |
