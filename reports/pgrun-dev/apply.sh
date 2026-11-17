#!/bin/sh
# Switch a PGRun checkout's local agent integration from the previous CLI to
# PGBoxDB, or roll it back. Developer machines only: PGRun production does not
# read either file (reports/PGBOXDB_PGRUN_MIGRATION.md §A).
#
#   reports/pgrun-dev/apply.sh <pgrun-checkout>             # dry run
#   reports/pgrun-dev/apply.sh <pgrun-checkout> --apply
#   reports/pgrun-dev/apply.sh <pgrun-checkout> --rollback
#
# Files (both untracked in PGRun today; this script does not commit or add
# them to git):
#   .claude/settings.json  six hook entries `pgx agent hook` -> `pgboxdb agent hook`
#   .pgx.toml              copied to .pgboxdb.toml (comments converted); kept
#
# --apply
#   1. refuses unless `pgboxdb` is on PATH (else the new hooks would be
#      silent no-ops, like the old ones once `pgx` is gone);
#   2. refuses unless both files are byte-identical to before/ (reviewed);
#      if they changed, run the two pgboxdb commands below by hand instead;
#   3. backs both files up to .pgboxdb-dev-backup/ (refuses if one exists);
#   4. writes .pgboxdb.toml from the reviewed after/ copy (no-clobber; it is
#      what `pgboxdb migrate --apply` writes for this .pgx.toml), then runs
#      `pgboxdb agent install claude --scope project`, which rewrites only
#      .claude/settings.json. It deliberately does NOT run `pgboxdb migrate
#      --apply`: that would also move ~/.pgx, a separate step (§B3) to do
#      by hand with the old runtime stopped;
#   5. verifies the result is byte-identical to after/; otherwise restores
#      the backup and fails.
# --rollback restores .claude/settings.json from the backup and moves
#   .pgboxdb.toml into the backup directory (nothing is deleted).
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
die() { echo "pgrun-dev: $*" >&2; exit 1; }
[ $# -ge 1 ] || die "usage: apply.sh <pgrun-checkout> [--apply|--rollback]"
REPO="$(cd "$1" && pwd)"; MODE="${2:-plan}"
BK="$REPO/.pgboxdb-dev-backup"
sha() { shasum -a 256 "$1" | cut -d' ' -f1; }
same() { [ -f "$1" ] && [ -f "$2" ] && [ "$(sha "$1")" = "$(sha "$2")" ]; }

restore() {
  cp -p "$BK/claude-settings.json" "$REPO/.claude/settings.json"
  if [ -f "$REPO/.pgboxdb.toml" ]; then mv "$REPO/.pgboxdb.toml" "$BK/pgboxdb.toml.$(date +%Y%m%d%H%M%S)"; fi
}

case "$MODE" in
  plan|--apply)
    if same "$REPO/.claude/settings.json" "$HERE/after/claude-settings.json" && same "$REPO/.pgboxdb.toml" "$HERE/after/pgboxdb.toml"; then
      echo "already applied"; exit 0
    fi
    same "$REPO/.claude/settings.json" "$HERE/before/claude-settings.json" || die ".claude/settings.json differs from the reviewed before/ copy; run \`pgboxdb agent install claude --scope project\` by hand"
    same "$REPO/.pgx.toml" "$HERE/before/pgx.toml" || die ".pgx.toml differs from the reviewed before/ copy; run \`pgboxdb migrate --apply\` by hand"
    [ ! -e "$REPO/.pgboxdb.toml" ] || die ".pgboxdb.toml exists already; not overwriting it"
    command -v pgboxdb >/dev/null 2>&1 || die "pgboxdb is not on PATH; install PGBoxDB first"
    if [ "$MODE" = plan ]; then
      echo "plan: back up to $BK, then in $REPO:"
      echo "  write .pgboxdb.toml from $HERE/after/pgboxdb.toml (no-clobber)"
      echo "  pgboxdb agent install claude --scope project  # 6 hook entries replaced"
      echo "dry run: nothing changed. Re-run with --apply."
      exit 0
    fi
    [ ! -e "$BK" ] || die "$BK exists; not overwriting a previous backup"
    mkdir "$BK"
    cp -p "$REPO/.claude/settings.json" "$BK/claude-settings.json"
    cp -p "$REPO/.pgx.toml" "$BK/pgx.toml"
    ( set -C; cat "$HERE/after/pgboxdb.toml" > "$REPO/.pgboxdb.toml" ) || { restore; die "could not write .pgboxdb.toml; restored"; }
    ( cd "$REPO" && pgboxdb agent install claude --scope project ) || { restore; die "agent install failed; restored"; }
    if ! same "$REPO/.claude/settings.json" "$HERE/after/claude-settings.json" || ! same "$REPO/.pgboxdb.toml" "$HERE/after/pgboxdb.toml"; then
      restore; die "the result differs from the reviewed after/ files; restored the backup"
    fi
    same "$REPO/.pgx.toml" "$BK/pgx.toml" || die ".pgx.toml changed unexpectedly (backup in $BK)"
    echo "applied. Backup: $BK. Rollback: $0 $REPO --rollback"
    ;;
  --rollback)
    [ -f "$BK/claude-settings.json" ] || die "no backup at $BK"
    restore
    echo "rolled back .claude/settings.json; .pgboxdb.toml moved into $BK"
    ;;
  *) die "unknown mode $MODE" ;;
esac
