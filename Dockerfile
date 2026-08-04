# syntax=docker/dockerfile:1

# ---------- 构建阶段 ----------
FROM rust:1-bookworm AS builder
WORKDIR /build

# rusqlite(bundled) 需要 C 编译器；native-tls 在 Linux 需要 OpenSSL 开发库
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY . .
RUN cargo build --release --locked \
    && strip target/release/llaia

# ---------- 运行阶段 ----------
FROM debian:bookworm-slim
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/llaia /usr/local/bin/llaia
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

# Web channel 监听地址（部署时对外暴露需绑定 0.0.0.0）
ENV LLAIA_WORKSPACE=/data \
    WEB_HOST=0.0.0.0 \
    WEB_PORT=9000

VOLUME ["/data"]
EXPOSE 9000

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
