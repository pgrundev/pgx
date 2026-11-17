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
# A volume created by the image from before the rename records that image's
# socket directory (/var/run/pgx), which this image does not have. It holds
# only the Unix socket, no data: point it at this image's run directory and
# keep the previous file as config.json.pre-migrate. Any other value is left
# alone.
cfg="$PGBOXDB_HOME/config.json"
if grep -q '"run_dir": *"/var/run/pgx"' "$cfg" 2>/dev/null && [ ! -d /var/run/pgx ]; then
  [ -e "$cfg.pre-migrate" ] || cp -p "$cfg" "$cfg.pre-migrate"
  sed "s#\"run_dir\": *\"/var/run/pgx\"#\"run_dir\": \"$PGBOXDB_RUN_DIR\"#" "$cfg" > "$cfg.tmp" && mv "$cfg.tmp" "$cfg"
  echo "pgboxdb: volume from before the rename: socket directory /var/run/pgx -> $PGBOXDB_RUN_DIR (previous config kept as config.json.pre-migrate)"
fi
exec pgboxdb serve
