# 安装 LLAIA

LLAIA 是一个 Rust 编写的本地轻量 AI 助手。它本身**不自带模型**——你需要自备一个 OpenAI 兼容（或 Anthropic）端点的模型服务，本地（Ollama / LM Studio）或云端（OpenRouter / Anthropic）都行。

安装完成后，按 [快速开始](quick-start.md) 初始化并配置 provider 即可使用。

## 方式一：预编译二进制（推荐）

到 [Release 页面](https://github.com/Feihei/llaia/releases) 下载对应架构的二进制，放到系统 `PATH` 里，验证：

```bash
llaia help
```

## 方式二：Docker 镜像

官方镜像发布到 **`ghcr.io/feihei/llaia:latest`**（约 280 MB，基于 Debian bookworm-slim）。

镜像内置了终端工具链，供 agent 的 `terminal` 工具使用：`bash`、`curl`、`wget`、`git`、`jq`、`unzip`、`python3`（含 `pip`），以及 [uv](https://github.com/astral-sh/uv) 用于快速装 Python 包。

### 单容器运行

```bash
docker run -d --name llaia \
  -p 51217:51217 \
  -v llaia-data:/data \
  ghcr.io/feihei/llaia:latest
```

首次启动会在 `/data` 下自动生成最小化配置并启用 Web UI。从容器日志取访问 token，然后浏览器打开：

```bash
docker logs llaia | grep -i token
# → 打开 http://127.0.0.1:51217
```

### docker compose

```yaml
# compose.yml
services:
  llaia:
    image: ghcr.io/feihei/llaia:latest
    container_name: llaia
    restart: unless-stopped
    ports:
      - "51217:51217"
    volumes:
      - llaia-data:/data

volumes:
  llaia-data:
```

### 浏览器自动化 sidecar（可选）

需要页面渲染、截图、表单填充时，可挂一个浏览器 sidecar，agent 通过 CDP 与 `http://browser:3000` 通信：

```yaml
# compose.browser.yml（extends compose.yml）
services:
  browser:
    image: browserless/chrome:latest
    restart: unless-stopped
    ports:
      - "3000:3000"
    environment:
      - CONNECTION_TIMEOUT=600000
```

> 这只是起点——具体用 Playwright 还是裸 CDP 由你自己接。

## 方式三：从源码编译

需要 **Rust 工具链**（[rustup](https://rustup.rs)）。Windows 下请用 **Git Bash** 运行。

```bash
git clone https://github.com/Feihei/llaia.git && cd llaia
cargo build --release
# 二进制在 ./target/release/llaia
```

## 下一步

- 初始化与配置：见 [快速开始](quick-start.md)
- 完整配置项说明：见 [配置参考](configuration.md)
