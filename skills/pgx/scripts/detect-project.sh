#!/usr/bin/env bash
# detect-project.sh [dir]: classify a project's framework and database setup
# for the pgx Skill. Prints exactly one JSON object on stdout. Reads only the
# files it names; never prints a password, a credentialed URL or file contents.
set -u
dir=${1:-.}
cd "$dir" 2>/dev/null || { echo '{"error":"no such directory"}'; exit 1; }

esc() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g; s/	/ /g'; }
has() { [ -e "$1" ]; }

# --- framework ---------------------------------------------------------------
framework=unknown; migrations=unknown; signals=""
add_signal() { signals="$signals\"$(esc "$1")\","; }
if has Gemfile && grep -qE '^\s*gem\s+["'\'']rails' Gemfile 2>/dev/null; then framework=rails; add_signal Gemfile; has bin/rails && add_signal bin/rails; fi
if has manage.py; then framework=django; add_signal manage.py; fi
if has prisma/schema.prisma; then framework=prisma; add_signal prisma/schema.prisma; fi
if [ $framework = unknown ] && has package.json && grep -qE '"(pg|postgres|pg-promise|kysely|drizzle-orm|knex)"' package.json 2>/dev/null; then framework=node-pg; add_signal package.json; fi
if [ $framework = unknown ] && { grep -qiE 'sqlalchemy' requirements.txt pyproject.toml 2>/dev/null; }; then framework=sqlalchemy; has pyproject.toml && add_signal pyproject.toml; has requirements.txt && add_signal requirements.txt; fi
if [ $framework = unknown ] && has go.mod && grep -qE 'jackc/pgx|lib/pq|pgx/v' go.mod 2>/dev/null; then framework=go; add_signal go.mod; fi
case $framework in
  rails) has db/migrate && migrations=rails ;;
  django) migrations=django ;;
  prisma) migrations=prisma ;;
esac
[ $migrations = unknown ] && has alembic.ini && migrations=alembic
[ $migrations = unknown ] && ls migrations/*.sql >/dev/null 2>&1 && migrations=golang-migrate
[ $migrations = unknown ] && [ $framework = rails ] && migrations=rails

# --- database configuration --------------------------------------------------
files=""; url_sources=""; host=""; port=""; db=""; url_seen=0; ssl=0
add_file() { files="$files\"$(esc "$1")\","; }
add_src() { url_sources="$url_sources\"$(esc "$1")\","; }
# Split a URL into host/port/db without ever echoing userinfo.
parse_url() {
  local u=$1 rest hp
  rest=${u#*://}; rest=${rest##*@}          # drop scheme and userinfo
  hp=${rest%%/*}; rest=${rest#*/}
  host=${hp%%:*}; [ "$host" = "$hp" ] && port="" || port=${hp#*:}
  db=${rest%%\?*}
  case "$rest" in *sslmode=require*|*sslmode=verify-ca*|*sslmode=verify-full*) ssl=1 ;; esac
  url_seen=1
}
if [ -n "${DATABASE_URL:-}" ]; then add_src env:DATABASE_URL; parse_url "$DATABASE_URL"; fi
for f in .env .env.local .env.development .env.test; do
  has "$f" || continue
  add_file "$f"
  if [ $url_seen = 0 ]; then
    line=$(grep -E '^\s*(export\s+)?DATABASE_URL\s*=' "$f" 2>/dev/null | head -1 | sed -E 's/^[^=]*=//; s/^["'\'' ]*//; s/["'\'' ]*$//')
    if [ -n "$line" ]; then add_src "file:$f"; parse_url "$line"; fi
  fi
  if [ -z "$host" ]; then
    h=$(grep -E '^\s*(export\s+)?PGHOST\s*=' "$f" 2>/dev/null | head -1 | sed -E 's/^[^=]*=//; s/^["'\'' ]*//; s/["'\'' ]*$//')
    [ -n "$h" ] && { host=$h; add_src "file:$f:PGHOST"; }
    p=$(grep -E '^\s*(export\s+)?PGPORT\s*=' "$f" 2>/dev/null | head -1 | sed -E 's/^[^=]*=//; s/^["'\'' ]*//; s/["'\'' ]*$//'); [ -n "$p" ] && port=$p
    d=$(grep -E '^\s*(export\s+)?PGDATABASE\s*=' "$f" 2>/dev/null | head -1 | sed -E 's/^[^=]*=//; s/^["'\'' ]*//; s/["'\'' ]*$//'); [ -n "$d" ] && db=$d
  fi
done
if has config/database.yml; then
  add_file config/database.yml; add_src file:config/database.yml
  if grep -qE 'DATABASE_URL' config/database.yml; then add_src "config/database.yml:DATABASE_URL"; fi
  [ -z "$host" ] && host=$(grep -E '^\s*host:' config/database.yml | head -1 | sed -E 's/^[^:]*:\s*//; s/["'\'' ]//g')
  [ -z "$db" ] && db=$(awk '/^development:/{f=1} f && /^\s*database:/{sub(/^[^:]*:[ \t]*/,""); gsub(/["'\'' ]/,""); print; exit}' config/database.yml)
fi
has prisma/schema.prisma && add_file prisma/schema.prisma
for f in docker-compose.yml compose.yml; do has "$f" && { add_file "$f"; if [ -z "$host" ] && grep -qE 'image:\s*postgres' "$f"; then host=localhost; add_src "file:$f:postgres service"; fi; }; done
has settings.py && add_file settings.py
[ -z "$host" ] && [ -n "$db$url_sources" ] && host=localhost   # a db name with no host means libpq defaults

classification=none
if [ -n "$host$db" ] || [ -n "$files" ]; then
  if [ -z "$host$db" ] && [ -z "$url_sources" ]; then classification=none
  elif printf '%s' "$host" | grep -qiE '(^|[.-])prod|production|rds\.amazonaws|neon\.tech|supabase|render\.com|railway|herokuapp|amazonaws|azure|googleapis|fly\.dev|timescale|aiven|crunchy|cloud'; then classification=sensitive
  elif [ -z "$host" ] || printf '%s' "$host" | grep -qE '^(localhost|127\.0\.0\.1|::1|0\.0\.0\.0|/|host\.docker\.internal)$'; then classification=local
  elif [ "$ssl" = 1 ]; then classification=sensitive   # TLS-only remote databases are rarely throwaway
  else classification=remote
  fi
fi

# --- pgx ---------------------------------------------------------------------
pgx_installed=false; pgx_version=null; pgx_port=""
if command -v pgx >/dev/null 2>&1; then
  pgx_installed=true
  v=$(pgx --json version 2>/dev/null | sed -nE 's/.*"pgx_version":"([^"]+)".*/\1/p'); [ -n "$v" ] && pgx_version="\"$(esc "$v")\""
  pgx_port=$(pgx --json status 2>/dev/null | sed -nE 's/.*"port":([0-9]+).*/\1/p')
fi
if [ "$classification" = local ] && [ -n "$pgx_port" ] && [ "$port" = "$pgx_port" ]; then classification=pgx; fi
toml=false; git_toml=false; d=$PWD; while [ "$d" != / ]; do [ -f "$d/.pgx.toml" ] && { toml=true; grep -qE '^[[:space:]]*\[git\][[:space:]]*(#.*)?$' "$d/.pgx.toml" && git_toml=true; break; }; d=$(dirname "$d"); done
git_repo=false; command -v git >/dev/null 2>&1 && git rev-parse --show-toplevel >/dev/null 2>&1 && git_repo=true
hook() { [ -f "$1" ] && grep -q 'pgx agent hook' "$1" 2>/dev/null && echo true || echo false; }

printf '{"framework":"%s","signals":[%s],"database_config":{"files":[%s],"url_sources":[%s],"classification":"%s","host":"%s","port":"%s","db":"%s"},"migrations":"%s","pgx":{"installed":%s,"version":%s,"toml":%s,"hooks":{"claude":%s,"cursor":%s,"codex":%s}},"git":{"repository":%s,"pgx_git":%s}}\n' \
  "$framework" "${signals%,}" "${files%,}" "${url_sources%,}" "$classification" "$(esc "$host")" "$(esc "$port")" "$(esc "$db")" "$migrations" \
  "$pgx_installed" "$pgx_version" "$toml" "$(hook .claude/settings.json)" "$(hook .cursor/hooks.json)" "$(hook .codex/hooks.json)" "$git_repo" "$git_toml"
