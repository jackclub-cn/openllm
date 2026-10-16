# OpenLLM Gateway

OpenLLM Gateway 是一个自托管的 LLM 网关，提供多提供商聚合、模型路由、访问密钥、OpenAI 与 Anthropic 协议兼容、用量统计和管理控制台。后端使用 Rust + Axum，数据保存在 SQLite，前端资源嵌入单个可执行文件。

[![Release](https://img.shields.io/github/v/release/jackclub-cn/openllm)](https://github.com/jackclub-cn/openllm/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

## 快速开始

从 [GitHub Releases](https://github.com/jackclub-cn/openllm/releases) 下载对应文件，每个二进制都附带 `.sha256` 校验文件。

| 系统 | 文件 | 启动 |
| --- | --- | --- |
| Windows | `openllm-windows.exe` | `.\openllm-windows.exe` |
| Linux | `openllm-linux` | `chmod +x ./openllm-linux && ./openllm-linux` |
| macOS | `openllm-macos` | `chmod +x ./openllm-macos && ./openllm-macos` |

默认监听 `http://127.0.0.1:8080`，数据保存在 `./data/openllm.db`。首次打开控制台后，按“提供商 → 模型 → 路由 → 访问密钥”的顺序配置。

```powershell
# 自定义监听地址和数据目录
.\openllm-windows.exe --bind 0.0.0.0:8080 --data-dir D:\openllm-data

# 也可以使用环境变量
$env:OPENLLM_BIND = "0.0.0.0:8080"
$env:OPENLLM_DATA_DIR = "D:\openllm-data"
$env:OPENLLM_ADMIN_TOKEN = "replace-with-a-long-random-value"
.\openllm-windows.exe
```

> 未设置 `OPENLLM_ADMIN_TOKEN` 时管理接口默认开放，仅适合本机。对公网或局域网提供服务时，应设置管理令牌并放在反向代理或私网之后。

### 常用配置

完整环境变量见下方折叠表；更多部署参数见 [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md)。

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `OPENLLM_BIND` | `127.0.0.1:8080` | HTTP 监听地址 |
| `OPENLLM_DATA_DIR` | `./data` | SQLite 与备份目录 |
| `OPENLLM_ADMIN_TOKEN` | 空 | 管理接口和 `/metrics` 鉴权令牌 |
| `OPENLLM_LOG_FORMAT` | `text` | 设为 `json` 输出结构化日志 |
| `RUST_LOG` | `info` | 日志级别 |
| `OPENLLM_MAX_CONCURRENT_REQUESTS` | `0` | 全局在途请求上限，`0` 不限 |
| `OPENLLM_MAX_BODY_MIB` | `32` | 请求体上限，MiB；多图或 PDF 可调大 |
| `OPENLLM_MAX_UPSTREAM_BODY_MIB` | `64` | 非流式上游响应缓冲上限，MiB |
| `OPENLLM_STREAM_MAX_SECS` | `1260` | 上游流最长存活时间；`0` 关闭 |
| `OPENLLM_MEMORY_LIMIT_MIB` | 自动 | 进程内存上限；`0` 关闭压力保护 |
| `OPENLLM_SHUTDOWN_GRACE_SECS` | `30` | 关闭信号后的排空时间；`0` 表示一直等待 |

## 核心能力

| 模块 | 能力 |
| --- | --- |
| 上游 | OpenAI 兼容、Anthropic、Ollama、自定义协议；多密钥、健康检查、超时与并发控制 |
| 模板 | Command Code、Kimi Code、GLM Token Plan、火山引擎 Token Plan、OpenCode Zen、OpenCode Go、自定义 |
| 模型目录 | 从上游同步模型，结合 models.dev 补齐上下文、模态、接口与价格；支持手动覆盖 |
| 路由 | 精确与通配路由、`vendor/model` 前缀、故障切换、公共能力收敛、请求级策略与提供商固定 |
| 访问密钥 | 到期时间、模型权限、每日 Token/费用额度、RPM、并发上限、轮换、密钥级路由策略 |
| 用量 | 请求、会话、Token、缓存、费用、首 Token 用时、TPS、错误；支持筛选、导出和请求详情 |
| 会话粘性 | 按会话 ID 或稳定提示前缀保持健康目标，降低 prompt cache 中断 |
| 配额池 | 按权重为网关密钥分配上游容量，支持 `hard`、`soft`、`burst` 策略 |
| Webhook | 成功/失败、熔断/恢复事件；HMAC-SHA256 签名、自动重试、投递历史 |
| 安全 | 请求防护、可选请求预览、配置审计、数据库备份与恢复、单实例租约 |
| 可观测性 | Prometheus 指标、自检、诊断快照、告警规则和 Grafana 看板 |
| 协议 | `/v1/chat/completions`、`/v1/responses`、`/v1/completions`、`/v1/embeddings`、Anthropic Messages |

## 使用说明

| 步骤 | 操作 |
| --- | --- |
| 1 | 在“提供商”中选模板并填写上游 API Key，必要时立即测试连接 |
| 2 | 同步模型，在“模型管理”检查上下文、输出上限、接口、模态和价格 |
| 3 | 创建显式路由，或给提供商设置 `vendor/` 前缀后直接用 `vendor/model` |
| 4 | 在“访问密钥”创建网关密钥，并设置权限、额度、到期时间和路由策略 |

创建任意网关密钥后，所有 `/v1` 请求都必须携带 Bearer Token；没有创建密钥时允许匿名调用。

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H "Authorization: Bearer sk-openllm-..." \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4.1",
    "messages": [{"role": "user", "content": "Hello"}],
    "stream": false
  }'
```

控制台“模型调试”页可直接选择模型和网关密钥进行流式对话，并显示 Token 与总用时。

### 路由策略

路由按“显式精确路由 → 显式通配路由 → 提供商前缀”的顺序匹配；同一路由内按策略排序，失败后继续尝试下一个健康目标。

| 策略 | 行为 |
| --- | --- |
| `priority` | 数值越小越优先 |
| `weighted` | 按权重随机 |
| `round_robin` | 进程内轮询 |
| `cost_optimized` | 按模型输入/输出价格估算成本 |
| `latency_optimized` | 优先近 24 小时平均延迟更低的目标，并参考首字慢降权信号 |
| `p95_latency_optimized` | 优先近 24 小时 P95 延迟更低的目标，减少尾延迟抖动 |
| `least_used` | 优先近 24 小时完成请求更少的目标 |

支持会话 ID；没有显式会话 ID 时使用首个用户消息加系统提示的稳定前缀哈希，尽量保持上游 prompt cache。请求头 `x-openllm-strategy`、`x-openllm-provider`、`x-openllm-exclude-providers` 可按请求覆盖密钥默认策略。

### 协议兼容

| 项目 | 行为 |
| --- | --- |
| 请求追踪 | 响应返回 `x-request-id` 和 `x-openllm-request-id` |
| 路由回显 | 返回实际尝试次数、回退次数、命中的提供商和模型 |
| 能力上限 | 返回 `x-openllm-max-input-tokens`、`x-openllm-max-context-tokens`、`x-openllm-max-output-tokens` |
| 协议转换 | Anthropic Messages、Chat Completions、Responses 可按上游能力双向转换 |
| 推理内容 | Anthropic `thinking`、Chat `reasoning_content`、Responses reasoning 尽力互转 |
| 上游故障 | 临时错误、限流与余额耗尽进入冷却和故障切换；同目标按有界退避重试 |

### 配额池与 Webhook

| 功能 | 说明 |
| --- | --- |
| 配额池 | `POST /api/provider-pools` 创建；按密钥权重分配请求或 Token 容量 |
| `hard` | 池饱和后拒绝超出份额的请求 |
| `soft` | 不拒绝，只把超出份额的请求排到后面 |
| `burst` | 仍可借用剩余容量 |
| Webhook 事件 | `request.completed`、`request.failed`、`provider.circuit_open`、`provider.circuit_closed` |
| Webhook 格式 | `generic`、`slack`、`discord`、`telegram`、`feishu`、`dingtalk` |
| 签名 | `x-openllm-signature: t=<timestamp>,v1=<hmac>`，签名内容为 `"{timestamp}.{原始请求体}"` |

## 从源码构建

| 场景 | 命令 |
| --- | --- |
| Windows 发布构建 | `.\build.ps1` |
| Linux/macOS 发布构建 | `./build.sh` |
| 运行后端开发版 | `cargo run` |
| 运行前端开发版 | `cd web && npm install && npm run dev` |

发布构建输出到 `dist/openllm.exe` 或 `dist/openllm`。Vite 会把 `/api` 和 `/v1` 代理到 `127.0.0.1:8080`。

## 部署与运维

完整步骤、升级和回滚检查清单见 [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md)，相关模板都放在 `deploy/`。

| 方式 | 入口 | 说明 |
| --- | --- | --- |
| Docker Compose | `deploy/`、`docker-compose.yml` | 非 root 运行，数据卷持久化；`stop_grace_period` 应大于排空时间 |
| systemd | `deploy/systemd/openllm.service` | `StateDirectory=openllm`、失败重启和加固选项 |
| nginx | `deploy/nginx/openllm.conf` | 关闭缓冲，保持 HTTP/1.1，读写超时按空闲间隔设置 |
| Caddy | `deploy/caddy/Caddyfile` | `flush_interval -1`，适合 SSE |

升级采用“排空后重启”，不支持同一 SQLite 文件多实例写入。关闭信号后不再接受新连接，在途请求最多等待 `OPENLLM_SHUTDOWN_GRACE_SECS` 秒。负载均衡应把连接失败和 `503` 都视为不可用。

### 运维命令与接口

| 场景 | 命令 / 接口 |
| --- | --- |
| 迁移预检 | `openllm --check-migrations [--json]` |
| 离线自检 | `openllm --data-dir ./data --self-check [--json]` |
| 离线导出配置 | `openllm --data-dir ./data --export-config openllm-config.json` |
| 离线导入配置 | `openllm --data-dir ./data --import-config openllm-config.json [--apply]` |
| 离线诊断快照 | `openllm --data-dir ./data --snapshot openllm-snapshot.json` |
| 在线备份 | `openllm --data-dir ./data --backup` |
| 列出备份 | `openllm --data-dir ./data --list-backups` |
| 恢复 | `openllm --data-dir ./data --restore-backup <文件名>`，必须在停服后执行 |
| 配置导出 | `GET /api/config/export`，加 `?include_secrets=true` 才包含凭据 |
| 配置导入 | `POST /api/config/import`，先用 `{ "apply": false }` 干跑 |
| 自检 | `GET /api/diagnostics/self-check` |
| 诊断快照 | `GET /api/diagnostics/snapshot` |
| 运行状态 | `GET /api/status` |
| 指标 | `GET /metrics` |

## 数据与安全

| 项目 | 说明 |
| --- | --- |
| 凭据存储 | 上游密钥和 Webhook 请求头按原值保存在 SQLite；网关密钥只保存 SHA-256 摘要 |
| 请求防护 | 支持阻止词和提示词 Token 上限，拦截发生在路由与上游调用前 |
| 请求检查 | 默认关闭；开启后递归遮蔽常见凭据字段，但仍可能包含业务敏感内容 |
| 审计日志 | 记录提供商、路由、密钥、Webhook、设置与数据库操作；写入失败不影响主操作 |
| 备份 | 使用 `VACUUM INTO` 生成事务一致的单文件快照，手动和迁移前备份各保留最近 5 份 |
| 迁移 | 启动前检测待执行迁移；已有数据且需迁移时自动写迁移前备份，失败则不打开端口 |
| 探针 | `/api/health` 始终表示进程存活；`/api/ready` 探测数据库，排空时返回 `503` |
| SQLite | 默认 WAL、`synchronous=NORMAL` 和 busy timeout；仅支持单实例部署 |
| 日志保留 | 支持自动清理历史请求；按审计要求和磁盘容量设置保留天数 |

可观测性产物和指标说明见 [docs/OBSERVABILITY.md](docs/OBSERVABILITY.md)。后续候选功能与取舍见 [docs/EXTENSIONS.md](docs/EXTENSIONS.md)。

<details>
<summary>完整环境变量</summary>

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `OPENLLM_UPSTREAM_IDLE_TIMEOUT_SECS` | `300` | 上游两次读取之间的空闲上限；`0` 关闭 |
| `OPENLLM_SSE_KEEPALIVE_SECS` | `15` | 下游 SSE 静默心跳间隔；`0` 关闭 |
| `OPENLLM_MAX_INFLIGHT_REQUEST_MIB` | `0` | 全局在途请求体字节预算；`0` 不限 |
| `OPENLLM_ADMISSION_WAIT_MS` | `0` | 达到并发或字节预算后的等待时间；超时返回 `429` |
| `OPENLLM_BODY_READ_TIMEOUT_SECS` | `600` | 读取完整请求体的最长时间；`0` 关闭 |
| `OPENLLM_REQUEST_TIMEOUT_SECS` | `600` | 单次请求回退循环总预算；`0` 关闭 |
| `OPENLLM_PROVIDER_OPEN_THRESHOLD` | `3` | 提供商硬熔断阈值；`0` 仅保留软冷却 |
| `OPENLLM_SLOW_TARGET_MS` | `15000` | 首字软降权阈值，仅影响 `latency_optimized`；`0` 关闭 |
| `OPENLLM_RESILIENCE_STATE_PERSIST` | `1` | 将提供商、模型和上游密钥冷却镜像到 SQLite |
| `OPENLLM_SESSION_AFFINITY_TTL_SECS` | `1800` | 会话/提示前缀粘性 TTL；`0` 关闭 |
| `OPENLLM_SESSION_AFFINITY_CAPACITY` | `10000` | 进程内粘性条目上限 |
| `OPENLLM_SESSION_AFFINITY_PERSIST` | `0` | 是否把粘性映射镜像到 SQLite |
| `OPENLLM_MULTI_INSTANCE` | `refuse` | 同库已有活实例时默认拒绝启动 |
| `OPENLLM_INSTANCE_LEASE_TTL_SECS` | `300` | 实例租约有效期，范围 30–86400 |
| `OPENLLM_UPGRADE_FEED_URL` | 空 | 版本升级检查数据源；未设置时不主动外联 |
| `OPENLLM_MEMORY_SHED_RATIO` | `0.9` | 达到内存上限该比例后拒绝新请求 |
| `OPENLLM_DB_MAX_CONNECTIONS` | `10` | SQLite 连接池大小，上限 256 |
| `OPENLLM_DB_BUSY_TIMEOUT_SECS` | `10` | SQLite 写入竞争等待时间 |
| `OPENLLM_DB_ACQUIRE_TIMEOUT_SECS` | `15` | 获取数据库连接的等待时间 |

</details>

## 发布流程

GitHub Actions 在 Pull Request 和手动触发时执行测试与构建；推送 `v*` 标签时创建 Release，并上传三平台二进制和校验文件。

```bash
git tag v0.3.24
git push origin v0.3.24
```

Release 页面：<https://github.com/jackclub-cn/openllm/releases>

## License

MIT License，Copyright (c) 2026 jackclub-cn。完整条款见 [LICENSE](LICENSE)。
