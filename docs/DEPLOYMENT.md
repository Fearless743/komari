# 部署指南

本文档覆盖 `komari-rs` 的 systemd、Docker/Compose、反向代理、备份、健康检查和
端口 `25774`。配置键的完整说明见 [CONFIGURATION.md](CONFIGURATION.md)，从 Go
迁移的步骤见 [MIGRATION.md](MIGRATION.md)。

> **当前实现边界**
>
> 第二轮合并后（B01–B21），`komari server` 已装配 HTTP/WebSocket/静态页面、
> OAuth、后台任务和 cloudflared，并监听 `25774`。本文的 systemd/Docker/反代和
> 健康检查配置可直接用于 Rust 二进制。模块状态见 [PARITY.md](PARITY.md)。

## 1. 构建

要求 Rust 1.85+（edition 2024）。

```sh
cargo build --release --locked -p komari-cli
install -m 0755 target/release/komari /opt/komari/komari
```

发布构建注入版本：

```sh
KOMARI_VERSION=1.2.3 \
KOMARI_VERSION_HASH=$(git rev-parse --short HEAD) \
cargo build --release --locked -p komari-cli
```

`KOMARI_VERSION_HASH` 缺省为 `unknown`，会按开发构建使用 `debug` 日志级别。
详见 [MIGRATION.md](MIGRATION.md) 第 2 节。

## 2. 目录布局

推荐布局：

```text
/opt/komari/
├── komari                 # Rust 二进制
├── komari-go              # 可选：Go 回退二进制
└── data/
    ├── komari.db          # SQLite 数据库
    ├── komari.db-wal      # 运行时存在
    ├── komari.db-shm      # 运行时存在
    ├── theme/             # 自定义主题（B14 已实现并已装配）
    ├── favicon.ico        # 可选
    └── GeoLite2-Country.mmdb  # 使用 mmdb GeoIP 时
```

默认数据库路径是相对路径 `data/komari.db`，因此 **WorkingDirectory 必须是
`/opt/komari`**，或者显式设置绝对路径：

```sh
KOMARI_DB_FILE=/opt/komari/data/komari.db
```

运行用户需要对 `data/` 有读写权限：

```sh
useradd --system --home /opt/komari --shell /usr/sbin/nologin komari
install -d -o komari -g komari -m 0750 /opt/komari/data
chown -R komari:komari /opt/komari
```

## 3. systemd

### 3.1 环境文件

`/etc/komari/komari.env`：

```sh
KOMARI_LISTEN=0.0.0.0:25774
KOMARI_DB_TYPE=sqlite
KOMARI_DB_FILE=/opt/komari/data/komari.db
KOMARI_LOG_LEVEL=info
# 运行时 CORS 以数据库 configs.allow_cors 为准；这里仅设置启动配置
# KOMARI_ALLOW_CORS=false
KOMARI_WS_DISABLE_ORIGIN=false
```

权限：

```sh
install -d -m 0750 /etc/komari
chown root:komari /etc/komari/komari.env
chmod 0640 /etc/komari/komari.env
```

### 3.2 unit 文件

`/etc/systemd/system/komari.service`：

```ini
[Unit]
Description=Komari server
Documentation=https://github.com/Fearless743/komari
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=komari
Group=komari
WorkingDirectory=/opt/komari
EnvironmentFile=-/etc/komari/komari.env
ExecStart=/opt/komari/komari server
Restart=on-failure
RestartSec=5
TimeoutStopSec=30
LimitNOFILE=65535
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/opt/komari/data
UMask=0027

[Install]
WantedBy=multi-user.target
```

启用：

```sh
systemctl daemon-reload
systemctl enable --now komari
systemctl status komari
journalctl -u komari -f
```

`25774` 大于 1024，不需要 `CAP_NET_BIND_SERVICE`。如果使用 80/443，请让反向代理
监听低端口，后端仍监听 `127.0.0.1:25774`。

### 3.3 只监听本机

反向代理与后端同机时，建议：

```sh
KOMARI_LISTEN=127.0.0.1:25774
```

只暴露反向代理的 80/443，不直接暴露 `25774`。

## 4. Docker / Compose

**以仓库根目录的 `Dockerfile` 和 `docker-compose.yml` 为准。** 这两个文件由 B10
维护，当前已合并。

当前 B10 的形态（细节以文件为准）：

- 多阶段构建：`cargo-chef` 缓存依赖，`rust:<version>-bookworm` 编译，
  `debian:bookworm-slim` 运行；
- 运行镜像包含 `ca-certificates`、`tzdata`，不包含 `curl`/`wget`；
- 非 root 用户运行（uid/gid `10001`）；
- 数据目录 `/app/data` 使用命名卷 `komari-data`；
- `KOMARI_THEME_SRC` 是 B10 Dockerfile 的构建参数（默认
  `crates/komari-web/defaultTheme`），复制到构建阶段的 `/src/theme`；
- `KOMARI_DEFAULT_THEME_DIR` 是 B14 的**编译期**变量
  （`crates/komari-web/build.rs`），Docker 默认 `/src/theme`，不是 compose 运行时
  环境变量。以上变量都不由 `komari-config` 读取。
- 旧的 `KOMARI_WEB_DIST` / `KOMARI_WEB_DIR` 已废弃。

构建/启动：

```sh
docker build -t komari:local .

# 带版本注入和真实主题
docker build \
  --build-arg KOMARI_VERSION=1.2.3 \
  --build-arg KOMARI_VERSION_HASH="$(git rev-parse HEAD)" \
  --build-arg KOMARI_THEME_SRC=.komari-theme \
  -t komari:local .

docker compose -f docker-compose.yml up -d --build
docker compose -f docker-compose.yml logs -f komari
```

数据卷：

```yaml
volumes:
  - komari-data:/app/data
# 或 bind mount（先创建并修正 uid 10001 权限）：
# - ./data:/app/data
```

```sh
mkdir -p ./data
sudo chown -R 10001:10001 ./data
```

注意：

- 数据卷必须指向原有 `data/`，否则会创建空数据库。
- 不要同时运行 Go 和 Rust 容器写同一个 SQLite 文件。
- 镜像当前没有 `curl`/`wget`；不要在 compose 中写依赖这些命令的 healthcheck。
  请用宿主机/外部监控检查 `http://127.0.0.1:25774/ping`，或按 `Dockerfile`
  实际安装的工具调整。
- 健康检查应返回 `pong`；若失败，检查 `KOMARI_LISTEN`、端口映射和防火墙。
- 若容器内需要 mmdb 自动下载，确保 `/app/data` 可写且容器能访问外网。
- CORS 运行时以数据库 `configs.allow_cors` 为准；不要在 compose 中设置
  `KOMARI_ALLOW_CORS` 来“开启”运行时 CORS。

## 5. 反向代理

### 5.1 通用要求

| 项目 | 建议值 | 原因 |
| --- | --- | --- |
| WebSocket | 升级 `Upgrade` / `Connection` 头 | agent 上报 `/api/clients/report` 和浏览器 `/api/clients` 都是 WebSocket。 |
| 读超时 | `>= 3600s` | WebSocket 长连接；默认 60s 会被代理切断。 |
| 写超时 | `>= 3600s` | 大响应/长连接。 |
| 请求体 | `128m` 或更大 | 备份上传、主题上传、批量数据；Go 只对 favicon 限制 5MB。 |
| 缓冲 | 关闭 | WebSocket 和流式响应不应被代理缓冲。 |
| 真实 IP | `X-Real-IP` / `X-Forwarded-For` | GeoIP、审计日志、`uploadBasicInfo` 使用这些头。 |
| TLS | 在代理终止 | 后端默认明文 HTTP；不要直接暴露到公网。 |

### 5.2 Nginx

在 `http {}` 中加入：

```nginx
map $http_upgrade $connection_upgrade {
    default upgrade;
    ''      close;
}

server {
    listen 80;
    server_name komari.example.com;

    client_max_body_size 256m;

    location / {
        proxy_pass http://127.0.0.1:25774;
        proxy_http_version 1.1;

        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;

        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
        proxy_buffering off;
        proxy_request_buffering off;
    }
}
```

如果只反代 WebSocket 路径，可单独为 `/api/clients` 和 `/api/clients/report`
设置 `proxy_read_timeout`；其余 API 可用较短超时。

### 5.3 Caddy

```caddyfile
komari.example.com {
    encode gzip
    reverse_proxy 127.0.0.1:25774 {
        header_up Host {host}
        header_up X-Real-IP {remote_host}
        header_up X-Forwarded-For {remote_host}
        header_up X-Forwarded-Proto {scheme}
        transport http {
            read_timeout 1h
            write_timeout 1h
        }
    }
}
```

Caddy 会自动处理 WebSocket 升级。请求体默认不设上限；如需限制，请在 Caddy
配置或前置 WAF 中设置请求体上限。

### 5.4 CORS 与 Origin

- 后端 CORS 由持久化 `configs.allow_cors` 控制；`KOMARI_ALLOW_CORS` 只是启动
  配置。不要同时在 Nginx/Caddy 重复添加 `Access-Control-Allow-Origin`，否则会
  出现重复头。
- WebSocket Origin 默认要求 `Origin` host 与请求 `Host` 一致；反向代理必须保留
  `Host`。同源部署通常不需要设置 `KOMARI_WS_DISABLE_ORIGIN=true`。
- 跨域前端场景才考虑 `allow_cors=true` 或 `KOMARI_WS_DISABLE_ORIGIN=true`，
  并限制来源。

## 6. 数据备份

### 6.1 手动备份

```sh
systemctl stop komari
install -d -m 0750 /var/backups/komari
sqlite3 /opt/komari/data/komari.db \
  ".backup '/var/backups/komari/komari-$(date +%F-%H%M%S).db'"
tar -C /opt/komari -czf /var/backups/komari/data-$(date +%F-%H%M%S).tar.gz \
  data/theme data/favicon.ico data/GeoLite2-Country.mmdb 2>/dev/null || true
systemctl start komari
```

说明：

- `sqlite3 .backup` 在 WAL 模式下是安全的；直接 `cp komari.db` 可能丢失 WAL 中
  尚未 checkpoint 的事务。
- 主题、favicon、mmdb 不在数据库里，需要单独备份。
- `/api/admin/download/backup` 会生成与 Go 兼容的 zip；恢复在下次启动时完成。

### 6.2 定时备份

```cron
15 3 * * * root sqlite3 /opt/komari/data/komari.db ".backup '/var/backups/komari/komari-$(date +\%F).db'" && find /var/backups/komari -name 'komari-*.db' -mtime +14 -delete
```

建议同时备份 `data/theme` 和 `data/favicon.ico`，并定期做恢复演练。

### 6.3 恢复

先停服务，再同时恢复数据库和 `data/` 资源：

```sh
systemctl stop komari
cp /var/backups/komari/komari-YYYY-MM-DD.db /opt/komari/data/komari.db
rm -f /opt/komari/data/komari.db-wal /opt/komari/data/komari.db-shm
tar -xzf /var/backups/komari/data-YYYY-MM-DD.tar.gz -C /opt/komari
chown -R komari:komari /opt/komari/data
systemctl start komari
```

如果只备份了单个文件，则分别复制：

```sh
cp -a /var/backups/komari/theme /opt/komari/data/theme
cp -a /var/backups/komari/favicon.ico /opt/komari/data/favicon.ico
cp -a /var/backups/komari/GeoLite2-Country.mmdb /opt/komari/data/GeoLite2-Country.mmdb
chown -R komari:komari /opt/komari/data
```

不要只恢复 `komari.db` 而遗漏主题、favicon 或 mmdb；这些文件不在数据库中。

## 7. 健康检查

### 7.1 HTTP

Go 版和 Rust 版都提供：

```sh
curl -fsS http://127.0.0.1:25774/ping
# pong
```

systemd 可以用（需要主机已安装 `curl`）：

```ini
ExecStartPost=/bin/sh -c 'for i in $(seq 1 30); do curl -fsS http://127.0.0.1:25774/ping >/dev/null && exit 0; sleep 1; done; exit 1'
```

或使用独立的 timer/监控系统，避免 unit 因健康检查失败反复重启。

### 7.2 进程/端口

```sh
systemctl is-active komari
ss -ltnp | grep 25774
journalctl -u komari --since '10 min ago'
```

`ss` 应能看到 `25774`；若没有，检查启动日志中的 `Komari HTTP server assembled`
和监听地址。

### 7.3 数据库

```sh
sqlite3 /opt/komari/data/komari.db 'PRAGMA integrity_check;'
sqlite3 /opt/komari/data/komari.db 'PRAGMA journal_mode;'   # wal
```

不要在服务运行时执行写操作或 `VACUUM`；`komari-jobs` 会自行维护。

## 8. 端口与防火墙

- 默认监听：`0.0.0.0:25774`。
- 推荐：后端监听 `127.0.0.1:25774`，只对外开放反向代理的 80/443。
- 如果必须直接暴露：

```sh
ufw allow 25774/tcp
```

并确保前面有 TLS 终止或内网访问控制。不要暴露 SQLite 文件或 `data/` 目录。

## 9. 升级与回退

交互式脚本 `install.sh`（仓库根目录，移植自 Go 版 `install-komari.sh`）的
“升级”选项会自动完成以下步骤；也可手工执行：

```sh
systemctl stop komari
sqlite3 /opt/komari/data/komari.db ".backup '/var/backups/komari/pre-upgrade.db'"
install -m 0755 komari-new /opt/komari/komari
systemctl start komari
curl -fsS http://127.0.0.1:25774/ping
```

### 回退

```sh
systemctl stop komari
install -m 0755 /opt/komari/komari-go /opt/komari/komari
# 或恢复迁移前数据库备份
systemctl start komari
```

数据库兼容性与回退细节见 [MIGRATION.md](MIGRATION.md) 第 7 节。

## 10. 安全建议

- 使用专用非 root 用户运行，`UMask=0027`，数据目录 `0750`。
- 公网部署必须在反向代理终止 TLS。
- 设置至少 12 字符的 `api_key`；不需要 API 访问时保持为空。
- 需要登录才能查看面板时启用 `private_site=true`。
- 不要随意设置 `allow_cors=true` 或 `KOMARI_WS_DISABLE_ORIGIN=true`。
- 限制反向代理请求体大小，避免大文件耗尽磁盘/内存。
- 定期备份并验证恢复；保留 Go 二进制作为回退。

## 11. Go 回退部署

需要立即回退时，使用同一个 unit，只替换 `ExecStart`：

```ini
ExecStart=/opt/komari/komari-go server
```

Go 版默认数据库路径也是 `./data/komari.db`，因此 `WorkingDirectory=/opt/komari`
可以继续使用同一份数据。不要同时启动两个进程。
