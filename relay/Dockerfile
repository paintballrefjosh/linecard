# Build from the repository root:
#   docker build -f relay/Dockerfile -t linecard-relay .
FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p linecard-relay

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 relay
COPY --from=build /src/target/release/linecard-relay /usr/local/bin/linecard-relay
USER relay
ENV LINECARD_RELAY_ADDR=0.0.0.0:8443
EXPOSE 8443
ENTRYPOINT ["linecard-relay"]
