# OpenLLM Gateway

OpenLLM Gateway 是一个自托管的 LLM 网关，提供多提供商聚合、模型路由、访问密钥、OpenAI 与 Anthropic 协议兼容、用量统计和管理控制台。后端使用 Rust + Axum，数据保存在 SQLite，前端资源会嵌入单个可执行文件。

[![Release](https://img.shields.io/github/v/release/jackclub-cn/openllm)](https://github.com/jackclub-cn/openllm/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

## 快速开始

从 GitHub Releases 下载对应平台的二进制文件：

**下载地址：<https://github.com/jackclub-cn/openllm/releases>**

Release 目前提供：

- `openllm-windows.exe`：Windows x86_64
- `openllm-linux`：Linux x86_64
- `openllm-macos`：macOS

每个文件都附带对应的 `.sha256` 校验文件。下载后直接运行：

Windows：

```powershell
.\openllm-windows.exe
```

Linux 或 macOS：

```bash
chmod +x ./openllm-linux
./openllm-linux
```

默认监听 `http://127.0.0.1:8080`，数据保存在当前目录的 `data/openllm.db`。打开浏览器访问控制台，先添加提供商，再同步模型、配置路由和访问密钥。

```powershell
.\openllm-windows.exe --bind 0.0.0.0:8080 --data-dir D:\openllm-data
```

也可以使用环境变量：

```powershell
$env:OPENLLM_BIND = "0.0.0.0:8080"
$env:OPENLLM_DATA_DIR = "D:\openllm-data"
$env:OPENLLM_ADMIN_TOKEN = "replace-with-a-long-random-value"
.\openllm-windows.exe
```

未设置 `OPENLLM_ADMIN_TOKEN` 时，管理接口默认开放，适合本机使用。对公网或局域网提供服务时，应设置该变量，并建议部署在反向代理或私网之后。

可选的调优环境变量：

- `OPENLLM_MAX_BODY_MIB`：请求体上限，默认 `32`（MiB）。网关会整体缓冲请求体，因此这是内存保护值；多图或 PDF 场景可适当调大。
- `OPENLLM_UPSTREAM_IDLE_TIMEOUT_SECS`：上游两次读取之间的空闲上限，默认 `300` 秒。它只限制空闲间隔，不会截断长时间流式生成；上游首字节较慢时可调大。

## 核心能力

- 聚合 OpenAI 兼容、Anthropic、Ollama 和自定义上游，支持提供商多密钥、优先级、加权随机、轮询、成本优先、延迟优先、负载最少和故障切换。
- 添加提供商时先选择模板卡片，内置 Command Code、Kimi Code、GLM Token Plan、火山引擎 Token Plan、OpenCode Zen、OpenCode Go 和自定义协议模板。
- 从上游同步模型，并结合 models.dev 保存上下文、输出上限、接口、模态和价格等能力信息；支持手动覆盖并保留重新同步结果。
- 模型同步会记录开始与完成状态，避免手动同步和定时同步重复执行；网关重启后会标记未完成的同步，便于继续排查。
- 提供跨提供商的模型目录，可搜索模型、筛选能力与接口，并直接进入该模型的请求日志或路由诊断。
- 通过显式路由或 `vendor/model` 前缀对外提供稳定的模型列表，多目标路由会按最严格的公共能力限制输入和输出预算。
- 路由列表直接显示多目标的公共上下文、输入和输出上限，并在部分目标缺少能力元数据时标记不完整。
- 路由管理会同时标出目标、提供商或模型的停用状态，能力桶只按当前真正可路由的目标计算。
- 兼容 OpenAI `/v1/chat/completions`、`/v1/responses`、`/v1/completions`、`/v1/embeddings` 和 Anthropic Messages 协议。
- 提供网关访问密钥、到期时间、模型权限、每日 Token/费用额度、每分钟请求数、最大并发数和密钥轮换。
- 记录请求、会话 ID、Token、缓存、费用、总用时、首 Token 用时、TPS 和错误，并在仪表盘、请求日志和管理界面中查询与导出；仪表盘可下钻到带筛选条件的请求日志，日志筛选条件会同步到 URL。
- 支持请求事件 Webhook：可订阅成功或失败事件，向外部 HTTP 端点发送带 HMAC-SHA256 签名的 JSON，自动重试临时故障并保留投递历史。
- 记录配置变更审计日志：提供商、路由、访问密钥、Webhook、运行时设置和数据库操作都会写入审计轨迹，记录时间、操作、对象、摘要和变更详情。
- 会话 ID 用于请求详情追溯和同会话亲和路由；仪表盘聚焦请求、Token、缓存、成功率和总用时等核心指标。
- 支持额度接口的提供商可直接查看额度窗口和价格，多密钥时可切换具体 Key 查询；例如 Command Code 的 5 小时、周、月额度，OpenCode Go 的滚动、周、月窗口，以及 DeepSeek 余额和模型价格。

## 使用说明

预编译版本请从 [GitHub Releases](https://github.com/jackclub-cn/openllm/releases) 下载；从源码运行请先完成构建步骤。

### 配置流程

1. 在“提供商”页面先选择模板卡片，再添加上游并填写 API Key，完成后可立即测试连接。
2. 同步模型或手动维护模型列表，在“模型管理”中检查上下文、输出上限、支持接口和价格。
3. 创建路由，或为提供商填写 `vendor/` 前缀后直接通过 `vendor/model` 调用。
4. 在“访问密钥”页面创建网关 API Key，按需设置模型权限、到期时间和调用限额。

提供商可以配置多把上游密钥。列表会显示每把密钥的启用状态、冷却时间、请求数、成功率、平均总用时和输入/输出 Token；支持单密钥检测、全部密钥批量检测和定时健康检查。健康检查模型可以手动指定，留空时使用第一个已启用模型。每个提供商还可以设置单次上游请求超时、失败冷却基准、最大并发和有界排队时间：超时留空时沿用全局空闲超时，冷却留空时沿用内置策略；并发留空时不限流，达到上限后请求会在队列中等待并自动切换其他提供商，排队超时按本地限流返回且不会误伤提供商熔断。每个模型还可以单独设置并发上限和排队时间；模型槽达到上限时只跳过该模型并切到同提供商的其他模型，不会把整个提供商标记为容量耗尽。请求超时和冷却最长 3600 秒，最大并发为 1000，排队时间最长 300 秒；上游返回更长的 `Retry-After` 时冷却时间不会短于该值。

### 调用 API

创建访问密钥后，通过网关调用模型：

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

只要创建过任意网关访问密钥，所有 `/v1` 请求都必须携带 `Authorization: Bearer sk-openllm-...`。没有创建任何密钥时，网关允许匿名调用，便于本地快速验证。停用密钥后它会立即失效，但不会重新打开匿名访问。

控制台“模型调试”页可以直接选择模型和网关 API Key，并发起流式对话，实时显示生成内容、总用时和输入/输出 Token。

### 模型前缀与路由

路由按以下顺序匹配：

1. 显式路由中的精确模型名优先。
2. 显式路由中的通配符模型名按长度从长到短匹配。
3. 没有显式路由时，按提供商的 `model_prefix` 自动匹配已同步模型，例如 `openai/gpt-4.1`。
4. 同一路由内的上游按所选策略排序。
5. `priority` 使用较小的优先级值优先；`weighted` 按权重随机；`round_robin` 在每个网关进程内轮询；`cost_optimized` 按模型声明的输入/输出价格估算成本；`latency_optimized` 优先选择近 24 小时成功请求平均延迟更低的目标；`least_used` 优先选择近 24 小时完成请求更少的目标。
6. 成本和延迟未知的目标排在已知目标之后；同一策略内仍以目标优先级和 ID 作为稳定兜底顺序。
7. 收到连接错误或 HTTP `408`、`409`、`425`、`429`、`5xx` 时尝试下一个上游。

检测到会话 ID 时，网关会将同一会话稳定映射到优先提供商和优先上游 Key，降低缓存命中被随机切换打断的概率；无会话请求仍保持原有随机、轮询和 Key 轮转行为。路由诊断传入会话 ID 后可直接查看该会话的实际候选顺序，以及每个目标的成本、近期平均延迟、近期请求量和策略决策依据。

提供商模型名保存为上游原始名称。前缀只用于对外命名空间和自动路由，不会拼接到发送给上游的 `model` 字段。如果多个无前缀提供商暴露同名模型，网关会拒绝自动路由，此时请设置提供商前缀或创建显式路由。

### 自动路由（auto）

除了显式路由和 `vendor/model` 前缀，网关内置了一组虚拟模型，无需配置即可把所有已启用模型当作候选池：

- `auto`：默认，优先选择近 24 小时请求量最少的目标，把流量摊到空闲上游。
- `auto/cheap`：按模型公布的输入/输出价格估算成本，优先最便宜的目标。
- `auto/fast`：优先近 24 小时平均延迟最低的目标。
- `auto/reliable`：保持配置顺序，但健康提供商优先、失败提供商排到最后。

自动路由复用与显式路由相同的策略、健康过滤、冷却、故障切换、会话亲和和提供商多 Key 轮转，因此 `auto` 的目标失败后会自动切换到下一个候选。候选集合包含所有启用提供商下、能承接当前接口的启用模型；只支持向量等非消息接口的模型不会进入消息请求的候选池。请求日志中的 `requested_model` 记为 `auto`，`upstream_model` 记录实际命中的上游模型，响应头仍会返回 `x-openllm-routed-provider` 和 `x-openllm-routed-model`。

`auto` 会跨越不同厂商和不同上下文窗口的模型，因此不做公共能力上限收敛（能力桶对自动路由不适用）；请按需要为特定模型设置显式路由以固定上下文和输出上限。访问密钥的模型权限同样作用于自动路由：只有键允许的候选模型会参与选择，若没有任何候选项可用则返回 `403`。路由诊断输入 `auto` 可查看候选、命中顺序和决策依据。

### Webhook 事件

在控制台“Webhook”页面可以创建订阅，或直接管理 `/api/webhooks`。支持的事件：

- `request.completed`：请求成功完成，身份、路由、提供商、Token、费用、延迟和状态码等字段会写入 `data`。
- `request.failed`：请求最终失败，`data.error_message` 和 `data.status_code` 可用于告警。

投递请求为 JSON `POST`，包含以下头：

- `x-openllm-event`：事件类型。
- `x-openllm-delivery-attempt`：当前尝试次数。
- `x-openllm-signature`：仅在 Webhook 配置了签名密钥时返回。

可以为 Webhook 配置自定义请求头，例如 `Authorization: Bearer ...`；`Host`、`Content-Type`、`Content-Length` 和所有 `x-openllm-*` 头由网关保留，不能覆盖。

签名头格式为 `t=<unix_timestamp>,v1=<hex_digest>`，其中 `v1` 是对 `"{timestamp}.{原始请求体}"` 使用 Webhook 密钥计算的 HMAC-SHA256。接收端应使用常量时间比较摘要，并根据 `t` 拒绝过旧请求以降低重放风险。

连接错误、HTTP `408`、`425`、`429` 和 `5xx` 会再尝试两次；明确的其他 `4xx` 不会重试。投递采用有界并发和进程内事件总线，极端流量或进程退出时属于尽力投递，不替代持久化消息队列。每个 Webhook 最近保留 200 条投递记录，控制台可查看状态、尝试次数、耗时和错误，也可以发送测试事件。

### 审计日志

控制台“审计日志”页面或 `GET /api/audit-logs` 可查看配置变更历史。每次成功的写操作都会追加一条记录：

- `action`：`create`、`update`、`delete`、`rotate` 或 `action`（数据库整理等）。
- `entity`：`provider`、`route`、`api_key`、`webhook`、`settings`、`database` 或 `usage`。
- `summary` 与 `detail`：可读描述和结构化变更字段，`detail` 不包含密钥等敏感值。
- `actor`：`admin` 表示请求经过管理令牌鉴权，`local` 表示管理接口处于开放状态。

审计写入失败不会中断被审计的操作，只会记录一条警告日志，因此磁盘或锁竞争不会把一次成功的配置变更变成报错。默认保留最近 5000 条记录，写入时自动裁剪更早的历史；查询支持按对象、操作和关键词过滤并分页。

### 请求防护

控制台“设置”页或 `GET/PUT /api/settings/guardrails` 可以配置全局请求防护。策略在 API Key 权限检查之后、路由解析和上游调用之前执行，因此被拦截的请求不会消耗提供商额度。

- `blocked_terms`：字符串数组，最多 100 条，每条 2 到 200 个字符。匹配时大小写不敏感，按字面值查找，不提供正则表达式；只扫描提示词文本，不扫描图片 URL、Base64 媒体和其他非文本字段。
- `max_prompt_tokens`：可选的提示词 Token 上限，范围为 1 到 10000000。网关使用与预算估算相同的请求文本估算值，在路由前返回 `400`。

被阻止的请求会以 `400` 拒绝并写入请求日志；成功保存或清空策略会写入审计日志。将阻止词清空并取消 Token 上限即可关闭请求防护。

### 请求检查

控制台“设置”页或 `GET/PUT /api/settings/inspector` 可以按需开启请求内容捕获。该功能默认关闭，开启后接受路由的请求会在 `usage_logs.request_preview` 中保存一段有上限的请求体预览，并显示在“请求日志”详情抽屉中。

- 捕获前会递归遮蔽 `api_key`、`authorization`、`password`、`secret`、`token`、`access_token`、`refresh_token`、`cookie` 等常见凭据字段，但提示词和工具参数仍可能包含业务敏感信息，因此只应在确有排障需求时开启。
- 预览默认最多保留 4000 个字符，可在 256 到 65536 之间调整；超过上限的内容会被截断。
- 预览只覆盖已经进入上游调用流程的请求。认证、权限、路由或请求防护阶段的早期拒绝不会保存请求体，避免把无效或恶意请求长期持久化。

### 协议兼容

- OpenAI 与 Anthropic 请求都会在响应头返回 `x-request-id` 和 `x-openllm-request-id`，可与请求日志关联。
- 网关会返回 `x-openllm-routing-attempts`、`x-openllm-fallback-count`、`x-openllm-routed-provider` 和 `x-openllm-routed-model`，用于确认实际 fallback 次数与命中的上游。
- 非流式与流式响应都会按可用能力返回 `x-openllm-max-input-tokens`、`x-openllm-max-context-tokens` 和 `x-openllm-max-output-tokens`，便于客户端在发起后续请求前读取实际上限。
- 只支持 Anthropic Messages 的上游可以承接 `/v1/chat/completions`、`/v1/completions` 和 `/v1/responses`，网关会在请求与流式/非流式响应之间做双向转换。
- 只支持 `/chat/completions` 的 OpenAI 兼容上游也可以承接 `/v1/responses`；只支持 `/v1/responses` 的上游则能承接 `/v1/chat/completions` 和 Anthropic `/v1/messages`，网关会在这些形态之间双向转换请求与流式/非流式响应（含 Responses SSE 与 Anthropic 事件互转）。
- `/v1/models` 返回的 `supported_endpoints` 是网关实际能承接的集合，而不是上游声明的原始集合：只要上游支持其中一种消息协议，其余消息协议都会标注为可用。
- 推理内容会尽力透传：Anthropic `thinking` 和 chat `reasoning_content` 会转成 Responses `reasoning` 摘要，Responses 的 reasoning summary 也会转成 chat `reasoning_content`。跨提供商无法重建 Anthropic 的 thinking 签名或 OpenAI 的加密内容，因此转换到 Anthropic Messages 时不会伪造 `thinking` 块；原生协议仍保持原样透传。
- 可选的请求级路由控制支持 `x-openllm-strategy`、`x-openllm-provider` 和 `x-openllm-exclude-providers`。策略可取 `priority`、`weighted`、`round_robin`、`cost_optimized`、`latency_optimized` 或 `least_used`；提供商可用数字 ID 或提供商名称（大小写不敏感），排除多个提供商时用逗号分隔，例如 `x-openllm-exclude-providers: provider-a, 3`。这些控制会在访问权限、能力收敛和预算过滤之后、目标排序之前生效。无效策略、pinned 提供商不匹配或排除后没有剩余目标时返回 `400`，不会静默回退；成功响应会回显实际生效的三个请求头。
- 同样的三项控制可以固化到访问密钥上，在控制台“访问密钥”的创建或访问策略弹窗中配置，或直接写入 `POST /api/api-keys`、`PUT /api/api-keys/{id}` 的 `routing_policy` 字段。密钥策略是默认值，请求头按字段逐个覆盖：请求只带 `x-openllm-strategy` 时会保留密钥上的提供商固定与排除。密钥策略不会通过响应头回显，避免把调用方没有发送的配置伪装成请求头；实际命中结果仍由 `x-openllm-routed-provider` 反映。
- 可选的请求级成本上限：在 OpenAI 或 Anthropic 请求上带 `x-openllm-budget-usd`（正数，单位美元），网关会按目标的输入/输出价格与本次请求的输入估算、输出上限计算预估价，过滤掉超过上限的目标，再按路由策略排序。全部目标超预算、或没有任何目标公布价格时请求以 `400` 拒绝，并说明最便宜的已知估价；成功响应会回传 `x-openllm-budget-usd` 与 `x-openllm-budget-excluded-targets`，被过滤的次数也会写入该请求的网关告警中。
- 上游返回只带 trace id、没有明确参数的临时 4xx 时，网关会先重试一次，再决定是否切换到下一个路由目标。
- 同一目标的有界重试：连接错误、超时、`408`/`425` 与全部 `5xx` 会先按“指数退避 + 最多 25% 正向抖动”重试当前目标，再切换到下一个路由目标，避免大量请求在同一时刻回流，同时保证配置的退避下限不被缩短。默认重试 1 次、起始退避 200ms、上限 2s，可在控制台“设置”页或 `GET/PUT /api/settings/resilience` 调整（`max_retries` 取 0–5，取 0 即关闭）。`429` 不参与同目标重试：它由冷却与故障切换处理，立即重试只会浪费上游额度。重试成功会同时清除本次尝试写入的提供商/目标冷却；重试策略读取失败时按默认策略继续服务，不会中断推理。
- 可选的流式首包恢复默认关闭。开启后，网关会短暂保留首个 SSE 窗口，一旦窗口内出现有效内容、正常终止事件，或累计达到 `64KiB`，就立即原样放行，因此正常流式请求不会增加首包延迟；只有窗口始终为空时才会等到 `750ms` 上限。真正的重试条件只有一个：上游返回 200 之后、在送出任何内容之前就断开或结束。上游在内容之后直接关流而不发 `[DONE]`（部分 OpenAI 兼容服务如此）属于正常结束，不会被重放。建议仅对经常在流开始时截断的上游开启。
- 客户端主动断开流式响应时，网关会立即取消对应的上游读取并释放提供商/模型并发槽，不会继续等待上游产生下一块数据。
- 上游返回临时失败时会优先读取 `Retry-After` / `retry-after-ms`；模型级 `429`、`408`、`425` 和 `5xx` 先隔离具体模型，同一供应商有多个模型同时冷却后才升级为供应商级熔断，避免一个模型故障误伤健康兄弟模型；连接类故障仍直接按供应商隔离。连续失败会按指数退避并封顶。
- 所有路由目标都被上游限流时，网关会把最后一次的 `429` 与 `Retry-After` 透传给客户端，而不是折叠成 `502`，方便客户端按上游给出的窗口退避。
- 控制台“设置”页的“冷却管理”会列出当前正处于冷却的提供商、模型和上游密钥及剩余时间，并支持按范围提前解除：`GET /api/resilience/cooldowns` 返回快照，`POST /api/resilience/cooldowns/clear` 接受 `{ "all": true }`、`{ "provider_id": 1 }`、`{ "provider_id": 1, "model": "gpt-4.1" }` 或 `{ "provider_key_id": 3 }`。清除模型冷却不会动该提供商的熔断状态；清除提供商冷却会同时把连续失败计数归零。冷却只存在于进程内，重启后本就为空。
- 连接测试和定时健康检查会自动探测 `tool_search` 兼容性，不需要手动设置；上游拒绝该工具时，网关会记录兼容状态、移除该工具并自动重试。
- 上游模型只支持 `/responses` 时，健康检查会自动走 Responses 接口。
- 多目标路由会计算公共能力上限，并将超过共同输出上限的请求向下兼容到最严格目标允许的值。

## 从源码构建

依赖 Rust、Node.js 和 npm。Windows：

```powershell
.\build.ps1
```

Linux 或 macOS：

```bash
chmod +x build.sh
./build.sh
```

脚本会先构建 `web/dist`，再执行 `cargo build --release`，最后复制为：

- Windows：`dist/openllm.exe`
- Linux 或 macOS：`dist/openllm`

开发模式：

```powershell
cargo run
```

另开终端启动前端开发服务器：

```powershell
cd web
npm install
npm run dev
```

Vite 会把 `/api` 和 `/v1` 代理到 `127.0.0.1:8080`。

## 数据与安全

- 上游 API Key 和 Webhook 自定义请求头按原值保存在 SQLite，用于向上游或接收端发起请求；网关访问密钥只保存 SHA-256 摘要。
- 设置页支持在线一致性备份，不需要停服或直接复制 WAL 文件，并显示数据库文件位置、占用空间、可回收空间和各主要数据表记录数；可手动整理数据库回收已删除日志占用的空间。
- 日志保留策略支持自动清理历史请求记录。数据长期增长时，建议根据审计需求和磁盘容量设置合理的保留天数。
- 提供 Prometheus 文本格式的 `GET /metrics`，导出请求、成功/失败、在途请求、Token（总量/输入/输出/缓存）、估算费用、未定价请求、平均延迟、提供商与密钥健康分组、每个提供商的模型数、请求数、Token、平均延迟、提供商/模型/密钥冷却状态、提供商与模型并发上限和当前在途请求数、Webhook 与投递结果数量、审计日志数量，以及数据库大小和可回收空间。开启 `OPENLLM_ADMIN_TOKEN` 时该接口需要相同的鉴权（`Authorization: Bearer` 或 `x-admin-token`）。
- SQLite 默认启用 WAL、`synchronous=NORMAL` 和 busy timeout，适合单实例部署。多个网关实例不应同时写入同一个数据库文件。
- 开启 `OPENLLM_ADMIN_TOKEN` 时，SSE 事件接口通过 `admin_token` 查询参数鉴权，因为浏览器 EventSource 不支持自定义请求头。
- `data/`、`dist/`、`target/` 和 `web/node_modules/` 默认不纳入版本控制。

## 发布流程

GitHub Actions 不会在普通分支推送时构建。Pull Request 和手动触发会执行测试与构建；推送 `v*` 格式的标签时会创建 GitHub Release，并上传 Linux、Windows 和 macOS 二进制及校验文件。

```bash
git tag v0.3.9
git push origin v0.3.9
```

发布后的文件会出现在 Releases 页面：

<https://github.com/jackclub-cn/openllm/releases>

## License

MIT License，Copyright (c) 2026 jackclub-cn。完整条款见 [LICENSE](LICENSE)。
