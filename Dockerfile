# syntax=docker/dockerfile:1.7

FROM rust:1.85.1-bookworm AS builder

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime

ENV TG_WS_PROXY_HOST=0.0.0.0 \
    TG_WS_PROXY_PORT=1443 \
    TG_WS_PROXY_ADVERTISE_HOST=127.0.0.1 \
    TG_WS_PROXY_SECRET_FILE=/data/secret \
    RUST_LOG=info

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 app \
    && useradd --uid 10001 --gid app --no-create-home --home-dir /nonexistent app \
    && install -d -o app -g app -m 0700 /data

COPY --from=builder /build/target/release/tg-ws-proxy /usr/local/bin/tg-ws-proxy
COPY LICENSE /usr/share/doc/tg-ws-proxy/LICENSE

WORKDIR /data
USER app

VOLUME ["/data"]
EXPOSE 1443/tcp

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/tg-ws-proxy"]
