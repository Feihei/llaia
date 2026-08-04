# syntax=docker/dockerfile:1

# ---------- Build stage ----------
FROM rust:1-bookworm AS builder
WORKDIR /build

# rusqlite (bundled) needs a C compiler; native-tls needs OpenSSL dev libs on Linux
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY . .
RUN cargo build --release --locked \
    && strip target/release/llaia

# ---------- Runtime stage ----------
FROM debian:bookworm-slim
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/llaia /usr/local/bin/llaia
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

# Web channel listen address. Host is bound to 0.0.0.0 so the container is
# reachable from outside; port defaults to the code default 51217.
ENV LLAIA_WORKSPACE=/data \
    WEB_HOST=0.0.0.0 \
    WEB_PORT=51217

VOLUME ["/data"]
EXPOSE 51217

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
