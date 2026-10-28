# Build from the repository root:
#   docker build -f relay/Dockerfile -t linecard-relay .
# Run (data persists in the volume):
#   docker run -d -p 8443:8443 -v linecard-relay:/data ghcr.io/paintballrefjosh/linecard-relay
FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p linecard-relay

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 relay && mkdir /data && chown relay /data
COPY --from=build /src/target/release/linecard-relay /usr/local/bin/linecard-relay
USER relay
ENV LINECARD_RELAY_ADDR=0.0.0.0:8443 \
    LINECARD_RELAY_DB=/data/linecard-relay.db
VOLUME /data
EXPOSE 8443
ENTRYPOINT ["linecard-relay"]
