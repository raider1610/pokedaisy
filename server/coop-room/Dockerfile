# The co-op room server (server/coop-room). Build from this folder: docker build -t pokedaisy-coop-room .
FROM rust:1-slim AS build
WORKDIR /src
COPY . /src
RUN cargo build --release --locked -p pokedaisy-coop-server

FROM debian:stable-slim
RUN useradd --system --uid 10001 --home /data --shell /usr/sbin/nologin coop && mkdir -p /data && chown coop /data
COPY --from=build /src/target/release/pokedaisy-coop-server /usr/local/bin/pokedaisy-coop-server
USER coop
WORKDIR /data
# server.crt / server.key are made on the first start (keep /data on a volume: a new certificate means every
# player has to pin the new fingerprint). authorized_keys holds the access keys, one per line.
ENV CERT=/data/server.crt KEY=/data/server.key AUTH_KEYS=/data/authorized_keys
EXPOSE 9595/udp 9595/tcp
ENTRYPOINT ["/usr/local/bin/pokedaisy-coop-server"]
