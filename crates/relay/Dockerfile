# dchat-relay container image. Build from the repository root:
#   docker build -f crates/relay/Dockerfile -t dchat-relay .
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p relay

FROM debian:bookworm-slim
RUN useradd --system --no-create-home --shell /usr/sbin/nologin dchat
COPY --from=build /src/target/release/dchat-relay /usr/local/bin/dchat-relay
USER dchat
# Plain WebSocket; put a TLS proxy (see deploy/relay) in front for wss://.
ENV DCHAT_RELAY_LISTEN=0.0.0.0:7447
EXPOSE 7447
ENTRYPOINT ["dchat-relay"]
