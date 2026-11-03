# syntax=docker/dockerfile:1
#
# Linecard sync relay: a static musl binary on a distroless base (no shell, no
# package manager, runs as a non-root user).
#
# Build from the repository root:
#   docker build -f relay/Dockerfile -t linecard-relay .
# Run (data persists in the volume):
#   docker run -d -p 8443:8443 -v linecard-relay:/data ghcr.io/paintballrefjosh/linecard-relay
# Admin commands run in the same container:
#   docker exec <container> linecard-relay account add alice
#
# Dependencies build in their own layer (cargo-chef), so a code change only
# rebuilds the relay itself.

FROM lukemathwalker/cargo-chef:latest-rust-1-alpine AS chef
# musl-dev and a C compiler: SQLite is compiled in (rusqlite "bundled").
RUN apk add --no-cache musl-dev gcc
WORKDIR /src

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build
COPY --from=planner /src/recipe.json recipe.json
# The patched IronRDP crates (root Cargo.toml [patch.crates-io]) must exist before cargo reads the manifest.
COPY vendor vendor
RUN cargo chef cook --release -p linecard-relay --recipe-path recipe.json
COPY . .
RUN cargo build --release -p linecard-relay \
 && mkdir /data-seed /backups-seed

FROM gcr.io/distroless/static-debian12:nonroot
LABEL org.opencontainers.image.title="Linecard relay" \
      org.opencontainers.image.description="Stores and forwards end-to-end encrypted Linecard sync data" \
      org.opencontainers.image.source="https://github.com/paintballrefjosh/linecard" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"
COPY --from=build /src/target/release/linecard-relay /usr/local/bin/linecard-relay
# The data folder, owned by the image's non-root user (65532) so a named
# volume starts out writable. A bind mount needs the same ownership.
COPY --from=build --chown=65532:65532 /data-seed /data
# Where built-in backups go when LINECARD_RELAY_BACKUP_DIR=/backups is set.
COPY --from=build --chown=65532:65532 /backups-seed /backups
USER 65532:65532
ENV LINECARD_RELAY_ADDR=0.0.0.0:8443 \
    LINECARD_RELAY_DB=/data/linecard-relay.db
VOLUME /data
EXPOSE 8443
# The binary checks itself: there is no curl in this image.
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
  CMD ["linecard-relay", "health"]
STOPSIGNAL SIGTERM
ENTRYPOINT ["linecard-relay"]
