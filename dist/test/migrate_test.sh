#!/bin/sh
# Upgrade test: an installation made with the CLI from before the rename to
# PGBoxDB is adopted by `pgboxdb migrate`, keeps every database, and can be
# rolled back so the previous CLI works again.
#
#   dist/test/migrate_test.sh <previous-dist-dir> <pgboxdb-dist-dir>
#
# <previous-dist-dir> is an extracted release from before the rename (it has
# bin/pgx), e.g. ~/.pgx/dist/0.1.3; it is only executed, never modified.
# Everything runs under a fresh HOME in a temporary directory: the real
# ~/.pgx and ~/.pgboxdb are never read or written. The work directory is kept
# (its path is printed); MIGRATE_TEST_CLEAN=1 removes it on success.
# Needs psql (PSQL=… or on PATH, or Homebrew postgresql@18).
set -u
OLD_DIST="${1:?usage: migrate_test.sh <previous-dist-dir> <pgboxdb-dist-dir>}"
NEW_DIST="${2:?usage: migrate_test.sh <previous-dist-dir> <pgboxdb-dist-dir>}"
OLD_BIN="$OLD_DIST/bin/pgx"
NEW_BIN="$NEW_DIST/bin/pgboxdb"
[ -x "$OLD_BIN" ] || { echo "no $OLD_BIN" >&2; exit 2; }
[ -x "$NEW_BIN" ] || { echo "no $NEW_BIN" >&2; exit 2; }
PSQL="${PSQL:-$(command -v psql || echo /opt/homebrew/opt/postgresql@18/bin/psql)}"
[ -x "$PSQL" ] || { echo "psql not found (set PSQL)" >&2; exit 2; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/pgbmig.XXXXXX")"
RUN="/tmp/pgbm$$"                      # short: Unix socket path limit
mkdir -p "$RUN"
export HOME="$WORK/home"; mkdir -p "$HOME"
# Nothing from the caller's environment may leak in.
unset PGX_HOME PGX_RUN_DIR PGX_PORT PGBOXDB_HOME PGBOXDB_RUN_DIR PGBOXDB_PORT DATABASE_URL
export SHELL=/bin/zsh GIT_CONFIG_NOSYSTEM=1
git config --global user.email t@example.invalid; git config --global user.name t
git config --global init.defaultBranch main

pass=0
ok() { pass=$((pass + 1)); echo "ok $pass - $*"; }
fail() {
  echo "FAIL: $*" >&2
  echo "work directory kept: $WORK" >&2
  stop_all; exit 1
}
stop_all() {
  PGX_RUN_DIR="$RUN" "$OLD_BIN" stop --force >/dev/null 2>&1 || true
  PGBOXDB_RUN_DIR="$RUN" "$NEW_BIN" stop --force >/dev/null 2>&1 || true
}
old() { PGX_RUN_DIR="$RUN" "$OLD_BIN" "$@"; }
new() { PGBOXDB_RUN_DIR="$RUN" "$NEW_BIN" "$@"; }
sql() { "$PSQL" "$1" -XAtq -v ON_ERROR_STOP=1 -c "$2"; }

echo "# work: $WORK"
echo "# previous CLI: $("$OLD_BIN" version | head -1)"
echo "# new CLI:      $("$NEW_BIN" version | head -1)"

# --- 1. A real installation made by the previous CLI --------------------------
old run >/dev/null 2>"$WORK/old-run.err" || fail "previous CLI: run ($(cat "$WORK/old-run.err"))"
APP="$(old url app)"
sql "$APP" "CREATE TABLE keep(v int); INSERT INTO keep VALUES (42);" || fail "seed app"
old create permanent >/dev/null || fail "create permanent"
sql "$(old url permanent)" "CREATE TABLE p(x text); INSERT INTO p VALUES ('kept');" || fail "seed permanent"
old create shortlived --ttl 2h >/dev/null || fail "create with ttl"
PORT="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["port"])' "$HOME/.pgx/config.json")"

PROJ="$WORK/proj"; mkdir -p "$PROJ"; cd "$PROJ" || fail cd
git init -q . && echo x >f && git add f && git commit -qm init || fail "git init"
mkdir -p .claude
old agent setup --source app --hosts claude --scope project >/dev/null 2>"$WORK/setup.err" \
  || fail "previous CLI: agent setup ($(cat "$WORK/setup.err"))"
[ -f .pgx.toml ] || fail "previous CLI wrote no .pgx.toml"
grep -q 'pgx agent hook' .claude/settings.json || fail "previous CLI wrote no hooks"
old git init --source app >/dev/null 2>"$WORK/gitinit.err" || fail "previous CLI: git init ($(cat "$WORK/gitinit.err"))"
git checkout -qb feature/x || fail checkout
FEAT_DB="$(old git status --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["database"])')" \
  || fail "previous CLI: git status"
sql "$(old url "$FEAT_DB")" "CREATE TABLE feat(y int); INSERT INTO feat VALUES (7);" || fail "seed branch db"
REPO_ID="$(cat .git/pgx-repo-id)"
old git install-shell zsh >/dev/null || fail "previous CLI: install-shell"
grep -q '>>> pgx git shell integration' "$HOME/.zshrc" || fail "no rc block"
ok "previous CLI built an installation (port $PORT, branch db $FEAT_DB)"

# --- 2. The new CLI never forks it -------------------------------------------
out="$(new run 2>&1)"; code=$?
[ $code = 4 ] || fail "pgboxdb run with an unadopted previous home: exit $code, want 4 (conflict): $out"
echo "$out" | grep -q 'pgboxdb migrate' || fail "refusal does not point at pgboxdb migrate: $out"
[ ! -e "$HOME/.pgboxdb" ] || fail "pgboxdb run created ~/.pgboxdb"
ok "pgboxdb run refuses to create an empty ~/.pgboxdb beside it (exit 4)"
out="$(new status 2>&1)"; [ $? = 3 ] && echo "$out" | grep -q 'pgboxdb migrate' || fail "status hint: $out"
ok "pgboxdb status points at pgboxdb migrate"
out="$(PGX_PORT=1 new doctor 2>&1)"; echo "$out" | grep -q 'PGX_PORT is set but not read' || fail "env note: $out"
ok "previous environment variables are reported as ignored, not read"

# --- 3. Refusals: running runtime ---------------------------------------------
out="$(new migrate --apply 2>&1)"; code=$?
[ $code = 4 ] && echo "$out" | grep -q 'is running' || fail "migrate while running: exit $code: $out"
[ -d "$HOME/.pgx/data" ] && [ ! -e "$HOME/.pgboxdb" ] || fail "migrate changed something while blocked"
ok "migrate --apply refuses while the previous runtime runs, and changes nothing"
old stop >/dev/null || fail "previous CLI: stop"

# --- 4. Dry run, then apply ---------------------------------------------------
before="$(cd "$HOME/.pgx" && find . -type f | LC_ALL=C sort | xargs shasum -a 256 2>/dev/null | shasum -a 256)"
out="$(new migrate 2>&1)" || fail "dry run: $out"
echo "$out" | grep -q 'Dry run: nothing was changed' || fail "dry run text: $out"
[ -d "$HOME/.pgx/data" ] && [ ! -e "$HOME/.pgboxdb" ] && [ ! -e .pgboxdb.toml ] && [ ! -e .git/pgboxdb-repo-id ] \
  || fail "dry run changed something"
ok "migrate (dry run) plans and changes nothing"
out="$(new migrate --apply 2>&1)" || fail "apply: $out"
[ -d "$HOME/.pgboxdb/data" ] && [ ! -e "$HOME/.pgx" ] || fail "home not moved: $out"
[ -f "$HOME/.pgboxdb/migrated-from.json" ] || fail "no migration record"
after="$(cd "$HOME/.pgboxdb" && find . -type f ! -name migrated-from.json | LC_ALL=C sort | xargs shasum -a 256 2>/dev/null | shasum -a 256)"
[ "$before" = "$after" ] || fail "file contents changed during the move"
[ -f .pgboxdb.toml ] || fail ".pgboxdb.toml not written"
grep -q 'source = "app"' .pgboxdb.toml || fail ".pgboxdb.toml lost the source"
# The previous CLI wrote trailing comments ("# the PGX database ..."): every
# comment is converted, every value kept.
grep -n 'PGX\|`pgx \|docs/pgx/\|\.pgx\.toml' .pgboxdb.toml && fail ".pgboxdb.toml comments still name the previous CLI"
[ "$(grep -c '=' .pgboxdb.toml)" = "$(grep -c '=' .pgx.toml)" ] || fail ".pgboxdb.toml has a different number of settings"
[ -f .pgx.toml ] || fail ".pgx.toml was removed"
[ "$(cat .git/pgboxdb-repo-id)" = "$REPO_ID" ] && [ "$(cat .git/pgx-repo-id)" = "$REPO_ID" ] || fail "repo id not carried over"
echo "$out" | grep -q 'pgboxdb agent install claude --scope project' || fail "no agent hook action item: $out"
echo "$out" | grep -q 'pgboxdb git install-shell zsh' || fail "no shell action item: $out"
ok "migrate --apply moved the home byte-for-byte, copied .pgboxdb.toml and the repo id, kept the originals"

# --- 5. Everything still works under the new CLI ------------------------------
new start >/dev/null 2>"$WORK/start.err" || fail "pgboxdb start: $(cat "$WORK/start.err")"
[ -f "$HOME/.pgboxdb/data/pgx.conf" ] && [ ! -e "$HOME/.pgboxdb/data/pgboxdb.conf" ] \
  || fail "start did not keep the cluster's included settings file"
NPORT="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["port"])' "$HOME/.pgboxdb/config.json")"
[ "$NPORT" = "$PORT" ] || fail "port changed $PORT -> $NPORT"
[ "$(sql "$(new url app)" 'SELECT v FROM keep')" = 42 ] || fail "app data lost"
[ "$(sql "$(new url permanent)" 'SELECT x FROM p')" = kept ] || fail "permanent data lost"
new --json ls | grep -q '"shortlived"' || fail "TTL database missing"
new ttl shortlived | grep -qi 'unlimited\|never' && fail "TTL lost"
ok "databases, data, TTLs and the port survive (settings file pgx.conf kept)"
gs="$(new git status --json 2>"$WORK/gs.err")" || fail "pgboxdb git status: $(cat "$WORK/gs.err")"
echo "$gs" | grep -q "\"$FEAT_DB\"" || fail "branch database not mapped: $gs"
[ "$(sql "$(new url "$FEAT_DB")" 'SELECT y FROM feat')" = 7 ] || fail "branch data lost"
ok "Git integration keeps the branch database ($FEAT_DB) and its data"

new agent install claude --scope project >/dev/null 2>&1 || fail "pgboxdb agent install"
grep -q 'pgx agent hook' .claude/settings.json && fail "previous hook entries kept beside the new ones"
n="$(grep -o 'pgboxdb agent hook' .claude/settings.json | wc -l | tr -d ' ')"
[ "$n" -ge 6 ] || fail "expected the 6 Claude hooks, found $n"
new agent install claude --scope project >/dev/null 2>&1
[ "$(grep -o 'pgboxdb agent hook' .claude/settings.json | wc -l | tr -d ' ')" = "$n" ] || fail "install is not idempotent"
ok "agent install replaces the previous hook entries (no duplicates; idempotent)"
new git init >/dev/null 2>"$WORK/gi2.err" || fail "pgboxdb git init: $(cat "$WORK/gi2.err")"
grep -q '>>> pgx git hook' .git/hooks/post-checkout && fail "previous git hook block kept"
grep -q '>>> pgboxdb git hook' .git/hooks/post-checkout || fail "no new git hook block"
new git install-shell zsh >/dev/null || fail "pgboxdb git install-shell"
grep -q '>>> pgx git shell' "$HOME/.zshrc" && fail "previous rc block kept"
grep -q '>>> pgboxdb git shell' "$HOME/.zshrc" || fail "no new rc block"
ok "git hooks and shell integration are replaced in place"

out="$(new migrate --apply 2>&1)" || fail "second apply: $out"
echo "$out" | grep -q 'already migrated' || fail "not idempotent: $out"
ok "migrate --apply is idempotent"

# --- 6. Rollback ---------------------------------------------------------------
out="$(new migrate --rollback --apply 2>&1)"; [ $? = 4 ] || fail "rollback while running was not refused: $out"
new stop >/dev/null || fail "pgboxdb stop"
new migrate --rollback --apply >/dev/null 2>&1 || fail "rollback"
[ -d "$HOME/.pgx/data" ] && [ ! -e "$HOME/.pgboxdb" ] || fail "home not moved back"
old start >/dev/null 2>"$WORK/oldstart.err" || fail "previous CLI cannot start after rollback: $(cat "$WORK/oldstart.err")"
[ "$(sql "$(old url app)" 'SELECT v FROM keep')" = 42 ] || fail "data lost after rollback"
old stop >/dev/null
ok "rollback moves the home back; the previous CLI starts and sees the data"

stop_all
echo "PASS: $pass checks"
if [ "${MIGRATE_TEST_CLEAN:-}" = 1 ]; then rm -rf "$WORK" "$RUN"; else echo "work directory kept: $WORK (MIGRATE_TEST_CLEAN=1 removes it)"; fi
