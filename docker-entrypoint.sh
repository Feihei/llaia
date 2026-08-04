#!/bin/sh
# 容器入口：首次启动时为 Web channel 生成最小配置，然后启动后台服务。
set -e

WS="${LLAIA_WORKSPACE:-/data}"
mkdir -p "$WS"

CFG="$WS/config.toml"
if [ ! -f "$CFG" ]; then
  cat > "$CFG" <<EOF
# 由 docker-entrypoint.sh 自动生成（首次启动）
[channels.web]
enabled = true
host = "${WEB_HOST:-0.0.0.0}"
port = ${WEB_PORT:-9000}
EOF
fi

exec llaia serve --config_dir "$WS"
