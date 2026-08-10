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

# EXTRAS=full additionally installs a static Node.js interpreter (no npm) so the
# agent can run JS scripts via the terminal tool. Leave empty for the default
# image (curl/git/python3 + small CLI utilities only).
ARG EXTRAS=""

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
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

# Optional static Node.js (no npm) — only when EXTRAS contains "full".
# Pulls the latest 22.x linux-x64 tarball and copies just the `node` binary to
# avoid the npm/docs bloat that `apt-get install nodejs` would bring in.
RUN if echo "${EXTRAS}" | grep -q "full"; then \
        NODE_VER=$(curl -fsSL "https://nodejs.org/dist/latest-v22.x/" \
            | grep -oE "node-v22\.[0-9]+\.[0-9]+-linux-x64\.tar\.xz" \
            | sort -V | tail -1 \
            | sed 's/^node-v//; s/-linux-x64\.tar\.xz$//') \
        && curl -fsSL "https://nodejs.org/dist/latest-v22.x/node-v${NODE_VER}-linux-x64.tar.xz" -o /tmp/node.tar.xz \
        && tar -xJf /tmp/node.tar.xz -C /tmp \
        && cp /tmp/node-v${NODE_VER}-linux-x64/bin/node /usr/local/bin/node \
        && rm -rf /tmp/node.tar.xz /tmp/node-v${NODE_VER}-linux-x64; \
    fi

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
