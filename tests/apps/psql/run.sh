#!/bin/sh
# psql fixture: DDL, DML, COPY, a transaction with rollback, \copy, \d.
set -eu
PSQL="${PGBOXDB_TEST_PSQL:-psql}"
command -v "$PSQL" >/dev/null || { echo "APP psql: COVERAGE GAP (psql not found)"; exit 2; }
[ -n "${DATABASE_URL:-}" ] || { echo "APP psql: COVERAGE GAP (DATABASE_URL unset)"; exit 2; }
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
cat > "$TMP/s.sql" <<'SQL'
\set ON_ERROR_STOP on
CREATE TABLE person (id serial PRIMARY KEY, name text NOT NULL, data jsonb);
INSERT INTO person (name, data) VALUES ('ann', '{"age": 30}'), ('bob', '{"age": 41}');
BEGIN;
INSERT INTO person (name) VALUES ('temp');
ROLLBACK;
INSERT INTO person (name) VALUES ('cy');
\copy person(name) TO 'OUT/names.csv' WITH (FORMAT csv)
\copy person(name) FROM 'OUT/names.csv' WITH (FORMAT csv)
SQL
sed -i.bak "s#OUT#$TMP#g" "$TMP/s.sql"
"$PSQL" -v ON_ERROR_STOP=1 -q -d "$DATABASE_URL" -f "$TMP/s.sql"
N="$("$PSQL" -Atq -d "$DATABASE_URL" -c "SELECT count(*) FROM person")"
AGE="$("$PSQL" -Atq -d "$DATABASE_URL" -c "SELECT data->>'age' FROM person WHERE name='bob'")"
DESC="$("$PSQL" -Atq -d "$DATABASE_URL" -c "\d person" 2>&1 | head -1 || true)"
[ "$N" = "6" ] || { echo "APP psql: FAIL (expected 6 rows after rollback+\\copy, got $N)"; exit 1; }
[ "$AGE" = "41" ] || { echo "APP psql: FAIL (jsonb age=$AGE)"; exit 1; }
echo "APP psql: PASS (rows=$N, jsonb ok, copy roundtrip ok)"
