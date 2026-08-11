# syntax=docker/dockerfile:1

# ---------- Build stage ----------
FROM rust:1-bookworm AS builder
WORKDIR /build

# rusqlite (bundled) needs a C compiler. TLS is rustls, so no system OpenSSL needed.
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential \
    && rm -rf /var/lib/apt/lists/*

COPY . .
RUN cargo build --release --locked \
    && strip target/release/llaia

# ---------- Runtime stage ----------
FROM debian:bookworm-slim
WORKDIR /app

# Single lean image: shell essentials + Python + uv.
# If you need Node.js, derive your own image:
#   FROM ghcr.io/feihei/llaia:latest
#   RUN apt-get update && apt-get install -y nodejs npm

ENV LANG=C.UTF-8

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        wget \
        git \
        jq \
        unzip \
        tzdata \
        bash \
        libstdc++6 \
        python3 \
        python3-pip \
    && rm -rf /var/lib/apt/lists/*

# Static uv binary for fast Python package management.
COPY --from=ghcr.io/astral-sh/uv:latest /uv /usr/local/bin/uv

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
