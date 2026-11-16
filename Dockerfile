# PGBoxDB — a local, disposable, PostgreSQL-compatible runtime.
#
# Multi-stage: build the engine and CLI from source with the pinned toolchain
# and PostgreSQL 18 client tools (for initdb, to bake the base cluster), then
# ship a slim runtime image with libxml2 and ICU for full SQL/XML and ICU.
#
#   docker build -t pgboxdb:dev .
#   docker run --rm -p 5432:5432 -e PGBOXDB_PASSWORD=secret -v pgboxdbdata:/var/lib/pgboxdb pgboxdb:dev
#   psql "postgres://postgres:secret@localhost:5432/postgres"

# ---------- builder ----------
FROM rust:1.96-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential pkg-config libre2-dev clang ca-certificates gnupg lsb-release wget \
 && install -d /usr/share/postgresql-common/pgdg \
 && wget -qO /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc https://www.postgresql.org/media/keys/ACCC4CF8.asc \
 && echo "deb [signed-by=/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc] http://apt.postgresql.org/pub/repos/apt $(lsb_release -cs)-pgdg main" > /etc/apt/sources.list.d/pgdg.list \
 && apt-get update && apt-get install -y --no-install-recommends postgresql-18 postgresql-client-18 \
 && rm -rf /var/lib/apt/lists/*
ENV PATH="/usr/lib/postgresql/18/bin:${PATH}" CC=/usr/bin/cc
WORKDIR /src
COPY . .
RUN cargo build --release --locked --bin postgres && cargo build --release --locked --bin pgboxdb
# initdb (run by assemble.sh to bake the base cluster) refuses to run as root,
# so assemble as a non-root user. /out is owned by that user; the built
# binaries and share trees under /src are world-readable.
RUN useradd -m builder && mkdir -p /out && chown builder:builder /out \
 && runuser -u builder -- dist/assemble.sh --out /out --version "${PGBOXDB_VERSION:-0.1.0-docker}" --pg-bindir /usr/lib/postgresql/18/bin \
 && mv /out/pgboxdb-*/ /pgboxdb

# ---------- runtime ----------
FROM ubuntu:24.04
# The PGBoxDB engine (like PostgreSQL) refuses to run as root, so the image runs as
# an unprivileged `pgboxdb` user. Create it, own the data/run dirs, and COPY the
# distribution owned by pgboxdb so the bundled base cluster (mode 700) is readable.
RUN apt-get update && apt-get install -y --no-install-recommends libxml2 libicu74 ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd -r -u 10001 pgboxdb \
 && mkdir -p /var/lib/pgboxdb /var/run/pgboxdb && chown pgboxdb:pgboxdb /var/lib/pgboxdb /var/run/pgboxdb
COPY --from=builder --chown=pgboxdb:pgboxdb /pgboxdb /opt/pgboxdb
COPY dist/docker/entrypoint.sh /usr/local/bin/pgboxdb-entrypoint
RUN ln -s /opt/pgboxdb/bin/pgboxdb /usr/local/bin/pgboxdb && chmod +x /usr/local/bin/pgboxdb-entrypoint
# PGBOXDB_HOME lives on the volume so data and the generated config persist.
ENV PGBOXDB_HOME=/var/lib/pgboxdb PGBOXDB_RUN_DIR=/var/run/pgboxdb PGBOXDB_LISTEN=0.0.0.0 PGBOXDB_PORT=5432
USER pgboxdb
VOLUME /var/lib/pgboxdb
EXPOSE 5432
ENTRYPOINT ["pgboxdb-entrypoint"]
