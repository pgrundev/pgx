#!/bin/sh
# Container entrypoint: run the PGBoxDB engine in the foreground as PID 1.
# First run initialises the cluster on the volume (reading PGBOXDB_PASSWORD,
# PGBOXDB_PORT, PGBOXDB_LISTEN, PGBOXDB_DURABILITY); later runs reuse it.
set -eu
mkdir -p "$PGBOXDB_HOME" "$PGBOXDB_RUN_DIR"
if [ ! -f "$PGBOXDB_HOME/config.json" ]; then
  echo "pgboxdb: first run — initialising cluster in $PGBOXDB_HOME (listen ${PGBOXDB_LISTEN}:${PGBOXDB_PORT})"
  pgboxdb init --port "${PGBOXDB_PORT:-5432}" ${PGBOXDB_DURABILITY:+--durability "$PGBOXDB_DURABILITY"} >/dev/null
  if [ -z "${PGBOXDB_PASSWORD:-}" ]; then
    echo "pgboxdb: no PGBOXDB_PASSWORD set; a random superuser password was generated (run 'pgboxdb url postgres' inside the container to see the connection URL)"
  fi
fi
exec pgboxdb serve
