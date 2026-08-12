# 频道（Channels）

频道让 LLAIA 不只活在终端/Web UI，还能通过你常用的 IM 触达你。**所有频道默认关闭**，在 `config.toml` 的 `[channels.<name>]` 下 `enabled = true` 开启。凭据放 `.env`，config 里用 `${VAR}` 引用。

每个频道都有 `allow_*` 字段作为**单用户安全锁**——个人助理只回应你一个人的消息。

> 频道在 `serve` 模式下随服务启动；`chat` 模式只跑 CliChannel（终端）。

## QQ（官方机器人，C2C 单聊）

```toml
[channels.qq]
enabled = false
app_id = "${QQ_APP_ID}"
app_secret = "${QQ_APP_SECRET}"
confirm_mode = "none"        # none / always / session（旧字段，语义已被权限档位取代）
```

启动时用 `app_id` + `app_secret` 换 `access_token`（有效期 7200s，过期前 60s 自动刷新）。

## Telegram

```toml
[channels.telegram]
enabled = false
bot_token = "${TELEGRAM_BOT_TOKEN}"   # @BotFather 颁发
allow_chat_id = 0                      # 只响应此 chat；0 = 不限制
```

官方 Bot API + **long polling**，免公网回调。

## 钉钉

```toml
[channels.dingtalk]
enabled = false
client_id = "${DINGTALK_CLIENT_ID}"
client_secret = "${DINGTALK_CLIENT_SECRET}"
allow_staff_id = ""                    # 只响应此 staffId；空 = 不限制
```

开放平台机器人 + **Stream Mode WebSocket**，免公网回调。

## 微信（ClawBot / ilink）

```toml
[channels.wechat]
enabled = false
allow_user_id = ""                     # 只响应此 ilink_user_id；空 = 不限制
```

首次启动打印二维码链接，手机扫码登录。**登录态**（token / sync_buf / context_tokens）不入 config，持久化在 `<config_dir>/wechat_state.json`，避免敏感凭证与配置混写。

## 邮箱（Mail）

```toml
[channels.mail]
enabled = false
imap_server = "imap.gmail.com"
imap_port = 993                        # 隐式 TLS
imap_user = "you@example.com"
imap_pass = "${MAIL_IMAP_PASS}"        # 密码/授权码，支持 ${VAR}
smtp_server = "smtp.gmail.com"
smtp_port = 465                        # 465 隐式 TLS；587 走 STARTTLS
smtp_user = ""                         # 留空复用 imap_user
smtp_pass = ""                         # 留空复用 imap_pass
poll_interval_secs = 30
mailbox = "INBOX"
owner_email = "you@example.com"        # 单用户锁：只响应此地址，避免邮件循环
from_name = "LLAIA"
mark_seen = true                       # 处理后标记已读
max_attachment_mb = 10                 # 超出仅提示不下载
```

IMAP 轮询收件 + SMTP 发信。仅响应 `owner_email` 发来的邮件。也可作为 cron 主动推送目标（结果发往 `owner_email`）。

## 飞书（Feishu / Lark）

```toml
[channels.feishu]
enabled = false
app_id = "${FEISHU_APP_ID}"
app_secret = "${FEISHU_APP_SECRET}"
allow_open_id = ""                     # 只响应此 open_id；空 = 不限制
mention_only = false                   # true=群聊仅 @ 时回复；私聊始终回复
```

开放平台事件订阅「长连接」模式（WebSocket 免公网回调）。

## 权限与审批

非交互频道（QQ/Telegram/Web 等）遇到需确认的操作，历史上只能「拒绝」。引入[权限档位](permissions.md)后，统一用 `/ok` `/deny` 交互式审批，跨频道一致；`cron` / `delegate` 等无法等待的场景自动拒绝并说明。

## 相关

- Web UI 里配置频道：[Web UI](webui.md)
- 频道相关的架构决策：[ADR-0009](../adr/0009-qq-channel.md) · [ADR-0011](../adr/0011-qq-capability-boundary.md)
