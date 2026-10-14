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

日志默认输出人类可读格式；设置 `OPENLLM_LOG_FORMAT=json` 可改为每行一个 JSON 对象，便于接入 Loki、ELK 等日志管道。日志级别仍由 `RUST_LOG` 控制。

可选的调优环境变量：

- `OPENLLM_MAX_BODY_MIB`：请求体上限，默认 `32`（MiB）。网关会整体缓冲请求体，因此这是内存保护值；多图或 PDF 场景可适当调大。
- `OPENLLM_UPSTREAM_IDLE_TIMEOUT_SECS`：上游两次读取之间的空闲上限，默认 `300` 秒。它只限制空闲间隔，不会截断长时间流式生成；上游首字节较慢时可调大。
- `OPENLLM_SSE_KEEPALIVE_SECS`：下游 SSE 流在静默时发送心跳注释的间隔，默认 `15` 秒；设为 `0` 可关闭。模型长时间思考、上游暂未吐字节时，心跳可避免反向代理或客户端因空闲而断开连接。
- `OPENLLM_MAX_CONCURRENT_REQUESTS`：全局在途请求上限，默认 `0`（不限）。设为正整数后，网关在准入阶段拒绝超出上限的请求并返回 `429` 与 `Retry-After`，避免突发的长流把内存或连接耗尽；它独立于按 API Key、按提供商和按模型的并发限制。
- `OPENLLM_MAX_INFLIGHT_REQUEST_MIB`：全局在途请求体字节预算，默认 `0`（不限）。设为正整数后，网关按请求体的实际字节数记账（读完即释放已缓冲的差值，请求结束时释放全部），超出预算的请求会在准入阶段被拒绝并返回 `429` 与 `Retry-After`。请求数少但体积大的编程代理流量更适合用它来保护内存，与并发上限互补。
- `OPENLLM_ADMISSION_WAIT_MS`：准入等待时长（毫秒），默认 `0`（立即拒绝）。设为正整数后，触及并发上限或字节预算的请求会先等待该时长以获取名额，超时仍返回 `429` 与 `Retry-After`。编程代理常会同时扇出多个子请求，短暂等待可让突发请求排队通过，而不是让客户端瞬间耗尽重试预算。
- `OPENLLM_STREAM_MAX_SECS`：单个上游流的最长存活时间，默认 `1260` 秒（21 分钟）；设为 `0` 关闭。空闲超时只约束两次读取之间的间隔，无法拦住"持续缓慢吐字节"的流；该上限保证一个卡住的流不会一直占住提供商名额与内存，超时后网关会结束该流并按上游故障冷却提供商。
- `OPENLLM_BODY_READ_TIMEOUT_SECS`：读取完整请求体的最长时间，默认 `600` 秒；设为 `0` 关闭。它防止慢速或恶意客户端通过持续缓慢发送请求体一直占住准入名额；超时后该请求会以错误结束。若确有超大文件在慢速链路上传，可适当调大。
- `OPENLLM_REQUEST_TIMEOUT_SECS`：单次请求回退循环的总时长预算，默认 `600` 秒；设为 `0` 关闭。预算耗尽后网关不再向后续目标发起新尝试，而是立刻以 `504` 结束并返回最后的错误（若最后是上游限流则仍返回 `429`）。它不会打断已经开始的上游调用，因此慢但可用的大模型不受影响，只是无法让一条全是失败目标的路线把准入名额占满数分钟。
- `OPENLLM_PROVIDER_OPEN_THRESHOLD`：提供商熔断阈值，默认 `3`；设为 `0` 关闭硬熔断、只保留软冷却。连续失败未达阈值时，运行时冷却只做降权，请求仍可尝试，避免偶发一次失败把单提供商路线变成故障；达到阈值后该提供商会短路，直接返回 `503` 与 `Retry-After`，不再向上游发起可能长时间挂起的调用。冷却结束后网关只放行一个探测请求验证恢复：成功即恢复正常，失败则带着升级后的冷却重新打开。后台健康检查成功时也会立即关闭熔断并清零失败计数，让恢复立刻生效。每次跳闸与恢复都会记录日志，并通过 `openllm_provider_circuit_trips_total{provider}` 计数，便于对反复跳闸的提供商告警。
- `OPENLLM_SLOW_TARGET_MS`：首字软降权阈值（毫秒），默认 `15000`；设为 `0` 关闭。一个请求即使返回 `200`，只要首字耗时达到阈值，该提供商模型会在后续请求中被软降权一段时间；存在健康同级目标时优先走同级，只有唯一目标或全部目标都被降权时仍会继续尝试。重复出现慢响应会阶梯式延长降权，最长 `5` 分钟。这用于处理“没有报错，但额度或上游压力把首字拖到几十秒”的情况，不会触发硬熔断，也不会中断已经成功开始的响应。
- `OPENLLM_RESILIENCE_STATE_PERSIST`：熔断与冷却状态的持久化开关，默认 `1`（开启）；设为 `0`、`false`、`no` 或 `off` 关闭。开启时，提供商的连续失败计数与冷却到期时间、模型冷却和上游密钥冷却会异步镜像到 SQLite（写在单独的 `provider_circuit_state`、`target_cooldown_state`、`provider_key_cooldown_state` 三张表），写操作由后台任务合并成事务，不占用请求路径；重启后会按绝对到期时间恢复，因此部署、升级或崩溃后不会把流量立刻打回刚刚失败的提供商。冷却到期时间本身就是 TTL，进程停机期间也会自然过期；重启时已经过期的冷却会恢复成半开状态，只放行一个探测请求；连续失败计数超过一小时未被刷新（即长期停机后只剩半开标记）则视为陈旧不再恢复。你手动清除的冷却会同时从持久化表中删除，重启不会把它带回来。该机制只适用于当前的单实例部署模型：多个网关实例共用同一个数据库文件并不受支持（启动时会拒绝，见 `OPENLLM_MULTI_INSTANCE`），互相干扰的冷却状态只会让路由更难预测。控制台“设置”页的运行限制里，“熔断状态持久化”一行显示当前取值（对应 `GET /api/settings/runtime` 的 `resilience_state_persist` 字段）。
- `OPENLLM_MULTI_INSTANCE`：同一数据库上已有活着的实例时的行为，默认 `refuse`（拒绝启动）；设为 `warn`、`allow`、`1`、`true` 或 `yes` 时改为记录警告后继续启动。SQLite 只支持单写者，同一数据库文件上跑两个网关并不受支持，默认拒绝启动是把问题在启动时暴露出来，而不是等到 Webhook 重复投递、配额窗口互相打乱才发现；租约由后台心跳刷新（默认 60 秒一次），进程崩溃后同一主机上已消失的 PID 会被自动接管，因此正常重启和崩溃重启都不会被自己的旧租约挡住。
- `OPENLLM_INSTANCE_LEASE_TTL_SECS`：实例租约的有效期，默认 `300` 秒，取值范围 30–86400。心跳间隔由该值推导（TTL 的三分之一，最小 5 秒、最大 60 秒），因此把 TTL 调到 60 秒以下时心跳会同步变密，不会出现“租约在两次心跳之间过期、健康实例被误判为崩溃”的情况。超过该时间没有心跳的租约会失效（例如进程被强杀后换了主机名）。
- `OPENLLM_UPGRADE_FEED_URL`：版本升级检查的数据源，默认未设置。设置后 `GET /api/maintenance/upgrade-check`（或控制台“运维”页）会请求该地址并在 5 秒超时内读取 `tag_name`、`version` 或 `latest` 字段作为候选版本，与当前版本比较后给出 `up_to_date`／`update_available`／`ahead`／`unknown` 结论；地址应返回 GitHub Release 形状的 JSON。未设置且未手动指定候选版本时结论为 `unknown`（网关不会主动外联）。
- `OPENLLM_MEMORY_LIMIT_MIB`：进程内存上限（MiB）。留空时（默认）网关会自动读取容器 cgroup 内存限制作为上限，因此在受限容器中开箱即用；显式填 `0` 可关闭该保护，填正整数则手动指定上限。达到上限的指定比例时，网关会采样本进程常驻内存并对新的代理请求返回 `503` 与 `Retry-After`，把内存留给已在途的请求，避免被 OOM 杀掉。采样在 Linux 与 Windows 上可用，cgroup 自动读取仅 Linux，其它平台该保护保持不生效，除非手动指定上限。
- `OPENLLM_MEMORY_SHED_RATIO`：触发内存压力保护的用量比例，默认 `0.9`（即用满 90% 上限时开始拒绝新请求）；取值范围 `(0, 1]`。
- `OPENLLM_DB_MAX_CONNECTIONS`：SQLite 连接池大小，默认 `10`，上限 `256`。高并发读取场景可适当调大；SQLite 单库写入仍会串行化，写入压力下应结合 `busy_timeout` 一起评估。
- `OPENLLM_DB_BUSY_TIMEOUT_SECS`：SQLite 写入竞争时的等待时间，默认 `10` 秒。多个写入并发时，等待超过该值才会返回 `SQLITE_BUSY`。
- `OPENLLM_DB_ACQUIRE_TIMEOUT_SECS`：从连接池获取连接的等待时间，默认 `15` 秒；连接池被占满且超过该时间时，该请求会以错误结束。以上三项都在启动时读取，修改后需重启。
- `OPENLLM_MAX_UPSTREAM_BODY_MIB`：非流式上游响应的缓冲上限，默认 `64`（MiB）。这是内存保护值，超过后网关会以错误结束该请求，避免异常上游把整个响应读进内存。
- `OPENLLM_SHUTDOWN_GRACE_SECS`：收到退出信号后等待在途请求结束的时间，默认 `30` 秒；设为 `0` 表示一直等待。超时后网关会关闭剩余连接，避免个别长流把重启或升级一直卡住。

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
- 支持按提供商配置配额池：把上游容量按权重分配给多个网关访问密钥，低于饱和阈值时可借用空闲容量，达到阈值后按 `hard`/`soft`/`burst` 策略限制；超出分配的候选项会被排除并回退到有余量的提供商，全部被拒时返回 `429` 与 `Retry-After`，判定结果写入请求日志。
- 支持请求事件 Webhook：可订阅成功或失败事件，向外部 HTTP 端点发送带 HMAC-SHA256 签名的 JSON，自动重试临时故障并保留投递历史。
- 记录配置变更审计日志：提供商、路由、访问密钥、Webhook、运行时设置和数据库操作都会写入审计轨迹，记录时间、操作、对象、摘要和变更详情。
- 会话 ID 用于请求详情追溯和同会话亲和路由；仪表盘聚焦请求、Token、缓存、成功率和总用时等核心指标。
- 支持额度接口的提供商可直接查看额度窗口和价格，多密钥时可切换具体 Key 查询；例如 Command Code 的 5 小时、周、月额度，OpenCode Go 的滚动、周、月窗口，以及 DeepSeek 余额和模型价格。Command Code 的月总额会结合当前周期的已用额度和剩余额度实时反推；模型级月额度从当前套餐的官方公开页解析，缓存 15 分钟，并与上游模型 ID 做匹配后显示基础额度和促销额度，不再依赖套餐 ID 的固定表。

## 使用说明

预编译版本请从 [GitHub Releases](https://github.com/jackclub-cn/openllm/releases) 下载；从源码运行请先完成构建步骤。

### 配置流程

1. 在“提供商”页面先选择模板卡片，再添加上游并填写 API Key，完成后可立即测试连接。
2. 同步模型或手动维护模型列表，在“模型管理”中检查上下文、输出上限、支持接口和价格。
3. 创建路由，或为提供商填写 `vendor/` 前缀后直接通过 `vendor/model` 调用。
4. 在“访问密钥”页面创建网关 API Key，按需设置模型权限、到期时间和调用限额。

提供商可以配置多把上游密钥。列表会显示每把密钥的启用状态、冷却时间、请求数、成功率、平均总用时和输入/输出 Token；支持单密钥检测、全部密钥批量检测和定时健康检查。健康检查模型可以手动指定，留空时使用第一个已启用模型。每个提供商还可以设置单次上游请求超时、失败冷却基准、最大并发和有界排队时间：超时留空时沿用全局空闲超时，冷却留空时沿用内置策略；并发留空时不限流，达到上限后请求会在队列中等待并自动切换其他提供商，排队超时按本地限流返回且不会误伤提供商熔断。每个模型还可以单独设置并发上限和排队时间；模型槽达到上限时只跳过该模型并切到同提供商的其他模型，不会把整个提供商标记为容量耗尽。请求超时和冷却最长 3600 秒，最大并发为 1000，排队时间最长 300 秒；上游返回更长的 `Retry-After` 时冷却时间不会短于该值。列表还会给出每个提供商的熔断状态（正常 / 已熔断 / 探测恢复中），可按状态筛选，并可对单个异常提供商执行“恢复路由”以提前解除冷却与熔断。

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

路由诊断（`POST /api/routes/diagnose`）同时返回一份可解释的候选评分，且全程不向上游发起任何请求：

- 每个候选包含 `score`（0–100，越高越优先）与 `factors` 逐项拆解：`circuit`（熔断/半开状态）、`capacity`（实时并发余量，取提供商与模型两个维度中更紧张的一个）、`allocation`（配额池余量与公平份额）、`health`（最近一次健康检查）、`reliability`（近 24 小时由上游原因导致的成功率，客户端 4xx 不计入）、`latency`（相对最快候选）、`cost`（相对最便宜候选）与 `strategy`（当前策略自身偏好）。每项都带 `weight`、`contribution`（`weight × 得分`）和可读的 `detail`，如“并发余量 0/1”“池 'shared' 请求余量 0/6”。
- 没有数据的因子按中性值（0.5）计入并标注 `score: null`，因此未配置配额池、未公布价格或从未来过流量的候选不会被默默判负，仍保持可选。
- `eligible: false` 表示请求路径当下不会使用该候选，`exclusion` 给出原因（配额池拒绝、提供商熔断中、模型冷却且有可用替代、目标/提供商/模型被禁用或接口不支持）；`deprioritized: true` 表示仍可使用但会被排到后面（软份额、健康检查失败、并发已满）。已排除的候选始终列在可用候选之后，与请求路径的过滤顺序一致。
- 评分不只是解释：实时并发已满、配额池软份额超限或健康检查失败的候选会在实际排序中被稳定地降到最后（策略内部相对顺序不变），配额池硬拒绝的候选与预算过滤一样在排序前被移除。因此饱和或额度耗尽的候选不会被选中，而单目标路线仍会进入排队重试，不会因为“降权”而丢失。
- 可选传入 `api_key_id` 会把候选评分按该网关访问密钥的配额池份额计算（不传时为匿名/借用者视角）；`session_id` 存在时同时返回会话实际顺序。控制台“路由”页的诊断弹窗会展示这份候选评分与评分依据。

提供商模型名保存为上游原始名称。前缀只用于对外命名空间，不会拼接到发送给上游的 `model` 字段。如果多个无前缀提供商暴露同名模型，请设置提供商前缀或创建显式路由。

### 提供商配额池

同一个上游提供商的容量由所有访问它的网关密钥共享。没有显式分配时，单个客户端的突发可以先吃满上游窗口，之后其他密钥即使几乎没用过也会收到上游 `429`。配额池把这份共享容量声明出来，并按权重为指定密钥保留份额：

- `POST /api/provider-pools` 创建池：`provider_id`、`name`、`window_seconds`（默认 60，上限 86400）、`request_limit` 与/或 `token_limit`（至少填一个，否则池不约束任何东西）、`saturation_threshold_pct`（默认 50），以及可选的 `allocations`。`GET /api/provider-pools` 查看（含每个密钥根据权重得出的实际份额），`PUT /api/provider-pools/{id}` 更新（未提供的字段保持原值，提供了 `allocations` 就整体替换），`DELETE /api/provider-pools/{id}` 删除。
- 每条分配包含 `api_key_id`、`weight_pct`（该密钥保留的池容量百分比，1–100）和 `policy`：`hard` 在池饱和时拒绝超出份额的请求，`soft` 不拒绝、只把该密钥的流量排在份额内的密钥之后，`burst` 仍可使用剩余余量。
- 判定沿用工作守恒的公平分配：用量低于 `saturation_threshold_pct` 时池处于宽松模式，任何密钥都可以借用空闲容量（最多到池上限）；达到阈值后转为严格模式，有分配的密钥被限制在 `limit × weight%`；没有分配记录的密钥（包括未创建访问密钥时的匿名调用）是借用者，只能用剩余余量，不能把池推过上限。
- 请求维度在发起上游尝试时记账（失败的尝试同样占用过上游容量，只记成功会让故障切换绕过配额），token 维度在响应完成、拿到实际用量后记账。窗口是进程内的滚动窗口，重启后归零；上游自身的窗口才是真正持久的那一层。
- 超出分配的候选会被直接排除，请求继续回退到有余量的提供商；如果所有候选都被拒绝，网关返回 `429` 并带上 `Retry-After`（指向窗口释放时间），错误信息里写明池名与原因。池判定属于本地路由决策，不会把提供商标记为失败、也不会触发熔断。
- 判定结果会写进该请求的请求日志告警（`warning_message`），形如 `provider pool allocation: excluded provider 'name': provider pool 'p' fair share exceeded ...`；`GET /api/providers/{id}/pool-usage` 返回池的实时用量、是否饱和、每条分配的份额与实际消耗，便于确认是谁在用满池子。

### Webhook 事件

在控制台“Webhook”页面可以创建订阅，或直接管理 `/api/webhooks`。支持的事件：

- `request.completed`：请求成功完成，身份、路由、提供商、Token、费用、延迟和状态码等字段会写入 `data`。
- `request.failed`：请求最终失败，`data.error_message` 和 `data.status_code` 可用于告警。
- `provider.circuit_open`：提供商连续失败达到熔断阈值，或半开探测再次失败时触发；`data.reason` 为 `failure_threshold` 或 `probe_failed`，同时包含提供商、连续失败次数、冷却秒数和阈值。
- `provider.circuit_closed`：提供商恢复、熔断关闭时触发；`data.reason` 为 `successful_request`、`reachable_response` 或 `health_check`，此时 `data.failure_streak` 为 `0`。`reachable_response` 表示半开探测收到了上游的客户端错误或限流响应，证明提供商已恢复连通；对应的 Key/模型冷却仍会保留。

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
- `entity`：`provider`、`provider_pool`、`route`、`api_key`、`webhook`、`settings`、`database` 或 `usage`。
- `summary` 与 `detail`：可读描述和结构化变更字段，`detail` 不包含密钥等敏感值。
- `actor`：`admin` 表示请求经过管理令牌鉴权，`local` 表示管理接口处于开放状态。

审计写入失败不会中断被审计的操作，只会记录一条警告日志，因此磁盘或锁竞争不会把一次成功的配置变更变成报错。默认保留最近 5000 条记录，写入时自动裁剪更早的历史；查询支持按对象、操作和关键词过滤并分页。控制台“审计日志”页的“导出”按钮（或 `GET /api/audit-logs/export`）会按当前过滤条件导出 CSV，便于留档或拉进表格审阅；导出受服务器行数上限约束，达到上限时会带上截断标记，控制台会明确提示而不是静默输出不完整的文件（用量导出同理）。

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
- 上游若用 `400`、`402` 或 `403` 表示额度不足／余额耗尽，网关会把这几种“看起来像客户端错误”的响应归一为 `429`，再进入冷却与故障切换；真正的鉴权失败和请求参数错误仍保持原状态码。
- 同一目标的有界重试：连接错误、超时、`408`/`425` 与全部 `5xx` 会先按“指数退避 + 最多 25% 正向抖动”重试当前目标，再切换到下一个路由目标，避免大量请求在同一时刻回流，同时保证配置的退避下限不被缩短。默认重试 1 次、起始退避 200ms、上限 2s，可在控制台“设置”页或 `GET/PUT /api/settings/resilience` 调整（`max_retries` 取 0–5，取 0 即关闭）。`429` 不参与同目标重试：它由冷却与故障切换处理，立即重试只会浪费上游额度。重试成功会同时清除本次尝试写入的提供商/目标冷却；重试策略读取失败时按默认策略继续服务，不会中断推理。
- 可选的流式首包恢复默认关闭。开启后，网关会短暂保留首个 SSE 窗口，一旦窗口内出现有效内容、正常终止事件，或累计达到 `64KiB`，就立即原样放行，因此正常流式请求不会增加首包延迟；只有窗口始终为空时才会等到 `750ms` 上限。真正的重试条件只有一个：上游返回 200 之后、在送出任何内容之前就断开或结束。上游在内容之后直接关流而不发 `[DONE]`（部分 OpenAI 兼容服务如此）属于正常结束，不会被重放。重试预算由 `stream_recovery_max_retries`（0–4，默认 2）单独控制，与 `max_retries` 相互独立，因此关闭普通重试时首包恢复依然生效。建议仅对经常在流开始时截断的上游开启。
- 客户端主动断开流式响应时，网关会立即取消对应的上游读取并释放提供商/模型并发槽，不会继续等待上游产生下一块数据。
- 上游返回临时失败时会优先读取 `Retry-After` / `retry-after-ms`，并继续识别 `x-ratelimit-reset-after`、OpenAI 风格的 `x-ratelimit-reset-requests` / `x-ratelimit-reset-tokens`、Unix 时间戳以及 Anthropic 统一 reset 时间；多个限流窗口同时存在时按最晚恢复时间冷却，避免提前重试。模型级 `429`、`408`、`425` 和 `5xx` 先隔离具体模型，同一供应商有多个模型同时冷却后才升级为供应商级熔断，避免一个模型故障误伤健康兄弟模型；连接类故障仍直接按供应商隔离，响应头送出后中途断流（空闲超时、连接重置等）也按连接类故障处理，避免下个请求又落到同一个不稳定目标。连续失败会按指数退避并封顶。
- 熔断与冷却默认镜像到 SQLite，重启不雪崩：进程重启（部署、升级或崩溃恢复）后，提供商的连续失败计数、冷却到期时间、模型冷却和上游密钥冷却会从数据库恢复，因此第一个请求不会被直接发回刚刚失败的提供商——之前这些状态只存在于进程内，重启即清零，等于把一次重启变成对上游客群的突发流量。写入由后台任务合并成事务，不在请求路径上锁；到期时间本身就是 TTL，进程停机期间也会自然过期。重启时已过期的冷却恢复为半开（只放行一个探测），超过一小时未刷新的半开标记视为陈旧不再恢复；恢复不会重发 `provider.circuit_open` / `provider.circuit_closed` Webhook，它们只描述运行期间的跳闸与恢复。可用 `OPENLLM_RESILIENCE_STATE_PERSIST=0` 关闭。
- 所有路由目标都被上游限流时，网关会把最后一次的 `429` 与 `Retry-After` 透传给客户端，而不是折叠成 `502`，方便客户端按上游给出的窗口退避。
- 控制台“设置”页的“冷却管理”会列出当前正处于冷却的提供商、模型和上游密钥及剩余时间，并支持按范围提前解除：`GET /api/resilience/cooldowns` 返回快照，`POST /api/resilience/cooldowns/clear` 接受 `{ "all": true }`、`{ "provider_id": 1 }`、`{ "provider_id": 1, "model": "gpt-4.1" }` 或 `{ "provider_key_id": 3 }`。清除模型冷却不会动该提供商的熔断状态；清除提供商冷却会解除该提供商及其全部模型的冷却，并把连续失败计数和探测租约一并清除。清除操作会同步到持久化表，重启后不会恢复刚清除的冷却；冷却状态本身默认跨重启保留（见 `OPENLLM_RESILIENCE_STATE_PERSIST`），但探测租约始终只存在于进程内。
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

## 部署

完整步骤、升级与回滚检查清单见 [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md)，模板与示例全部放在 `deploy/` 下。

容器（Dockerfile 三阶段构建：前端产物 → Rust 静态编译 → 精简运行镜像，控制台已嵌入二进制）：

```bash
cp deploy/openllm.env.example .env   # 至少设置 OPENLLM_ADMIN_TOKEN
docker compose up -d --build
docker compose ps                    # healthcheck 走 /api/ready
```

镜像默认以非 root 用户（uid/gid 10001）运行，数据目录 `/data` 挂到命名卷 `openllm-data`，因此升级重建容器不会丢数据。`docker-compose.yml` 的 `stop_grace_period` 要大于 `OPENLLM_SHUTDOWN_GRACE_SECS`，否则排空还没结束进程就被强杀。

systemd：`deploy/systemd/openllm.service` 提供 `StateDirectory=openllm`、`Restart=on-failure` 与一组内核/文件系统加固项，安装步骤写在文件头部注释里，也可直接在 `deploy/openllm.env.example` 基础上生成 `/etc/openllm/openllm.env`。配置文件不支持 `systemctl reload`：设置保存在数据库和进程环境里，改动后需要排空重启。

反向代理与 TLS 示例：

- nginx：`deploy/nginx/openllm.conf`
- Caddy：`deploy/caddy/Caddyfile`

两者都按 SSE 要求配置：关闭响应缓冲（nginx `proxy_buffering off`、`add_header X-Accel-Buffering always no`；Caddy `flush_interval -1`）、把读写超时设到分钟级（限制的是空闲间隔而不是总时长）、保持 HTTP/1.1 长连接并把 `client_max_body_size` 与 `OPENLLM_MAX_BODY_MIB` 对齐。来回切换上游流式响应时若出现“先卡住再一次性刷出”，基本都是代理缓冲没关。

同一数据库文件只能由一个进程写入，所以升级是“排空后重启”而不是滚动更新：收到关闭信号后网关立刻停止接受新连接并开始排空，已在途的请求在 `OPENLLM_SHUTDOWN_GRACE_SECS`（默认 30 秒）内跑完，超时后剩余连接被强制关闭、进程以退出码 0 结束（日志里是 `shutdown signal received; draining in-flight requests`，超预算时追加 `shutdown grace period elapsed; closing remaining connections`）。排空期间进程不再对外提供任何 HTTP 接口，所以负载均衡看到的摘除信号是**连接被拒绝/连接被关闭**，健康检查要把“连不上”和 `503` 同等当作不可用；就绪探针的 `503 {"status":"draining"}` 只在连接已经建立时可见（探针与关闭信号竞争，或探针复用长连接时）。用 `OPENLLM_MULTI_INSTANCE=warn` 强行起第二个实例属于不受支持的用法，会导致 Webhook 重复投递和配额窗口互相覆盖。

### 运维命令行（无需启动网关）

升级前可以先做一次无副作用的预检，把迁移与备份情况打印出来：

```bash
openllm --data-dir ./data --check-migrations          # 退出码 0=已是最新，2=有待执行迁移，1=需人工处理
openllm --data-dir ./data --check-migrations --json   # 输出 JSON，便于脚本判断
```

备份与恢复：

```bash
openllm --data-dir ./data --backup                 # 立即写一份手动备份到 data/backups/
openllm --data-dir ./data --list-backups           # 列出现有备份（手动与迁移前）
openllm --data-dir ./data --restore-backup <文件名>  # 必须在网关停止后执行
```

`--backup` 与 `--list-backups` 使用只读连接，可以在网关正常服务时安全执行；`--restore-backup` 会先拒绝已实例正在服务的数据目录，并自动保留原库为 `openllm.db.pre-restore`。恢复完成后启动网关，确认 `GET /api/ready` 返回 200 后再把流量放回来。

### 配置即代码

把一套网关的设置、提供商、路由、配额池与 Webhook 导出成可版本控制的 JSON：

```bash
curl -H "x-admin-token: $TOKEN" http://127.0.0.1:8080/api/config/export > openllm-config.json
```

导入到另一套网关时先干跑一次，确认计划再做写入：

```bash
curl -H "x-admin-token: $TOKEN" -H 'content-type: application/json' \
  -d "{\"document\": $(cat openllm-config.json), \"apply\": false}" \
  http://127.0.0.1:8080/api/config/import
```

导出默认不含明文凭据（掩码为 `***`，也不会被导入覆盖）；需要迁移上游密钥时加上 `?include_secrets=true`，并把导出的文件当作密钥材料保护。控制台“运维”页提供同样的流程（导出、选择文件、仅校验、导入）。

### 运维路径：部署 → 观测 → 排障 → 升级回滚

| 阶段 | 控制台 | 命令行 / 接口 | 入口文档 |
| --- | --- | --- | --- |
| 部署 | — | `docker compose up -d`、systemd 单元、反向代理示例 | [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) 第 1–3 节 |
| 观测 | 仪表盘、请求日志、Webhook | `GET /metrics`、`GET /api/status` | [docs/OBSERVABILITY.md](docs/OBSERVABILITY.md)、随附的告警规则与 Grafana 看板 |
| 排障 | 诊断、设置（冷却管理） | `GET /api/diagnostics/self-check`、`GET /api/diagnostics/snapshot`、`GET /api/resilience/cooldowns`、`POST /api/routes/diagnose` | [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) 第 7 节、[docs/OBSERVABILITY.md](docs/OBSERVABILITY.md) 排障小节 |
| 升级与回滚 | 运维 | `openllm --check-migrations`／`--backup`／`--list-backups`／`--restore-backup`、`GET /api/maintenance/*`、`GET /api/config/export` | [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) 第 4–5 节、本文件「运维命令行」与「配置即代码」 |

一次典型的下线—升级—上线流程：`openllm --check-migrations`（确认待执行迁移与备份计划）→ 停库（排空）→ 换二进制 → 启动并确认 `GET /api/ready` 为 `200` → 抽查流式请求与 `GET /api/status`；回滚用 `openllm --restore-backup <备份>` 恢复数据库后启动旧二进制。全程不依赖外部服务，只有 SQLite 文件与二进制。

### 扩展功能候选（本轮不实现）

下一轮可以做什么、为什么有些事现在不做，都写进了 [docs/EXTENSIONS.md](docs/EXTENSIONS.md)：17 项候选（会话粘性、精确响应缓存、告警通道、端点透传、配额遥测、计费账单、密钥策略、多模态桥接、提示压缩、OTel 导出、流式请求体等）逐项给出收益、成本、依赖与风险、推荐结论（做／待定／不做），并单列「本轮明确不做」的原因（多实例 Redis 共享状态、插件系统、桌面端、免费额度目录、语义缓存、19 种策略全量对齐）。该文档只给结论，不含实现计划；每项的触发条件写在正文，推进时以它作为评审材料单独立项。


## 数据与安全

- 上游 API Key 和 Webhook 自定义请求头按原值保存在 SQLite，用于向上游或接收端发起请求；网关访问密钥只保存 SHA-256 摘要。
- 设置页支持在线一致性备份，不需要停服或直接复制 WAL 文件，并显示数据库文件位置、占用空间、可回收空间和各主要数据表记录数；可手动整理数据库回收已删除日志占用的空间。
- 日志保留策略支持自动清理历史请求记录。数据长期增长时，建议根据审计需求和磁盘容量设置合理的保留天数。
- 提供 Prometheus 文本格式的 `GET /metrics`，导出请求、成功/失败、在途请求、Token（总量/输入/输出/缓存）、估算费用、未定价请求、平均延迟、提供商与密钥健康分组、每个提供商的模型数、请求数、Token、平均延迟、提供商/模型/密钥冷却状态、提供商熔断状态与连续失败次数、提供商与模型并发上限和当前在途请求数、全局准入上限与已拒绝数量、在途请求体字节预算与当前占用、内存压力与已拒绝数量、异步运行时调度延迟、SQLite 连接池占用、进程启动时间与运行时长、就绪状态（排空时归零）、配额池的池级与 Key 级窗口用量/额度/饱和度/拒绝数、Webhook 与投递结果数量、审计日志数量，以及数据库大小和可回收空间。开启 `OPENLLM_ADMIN_TOKEN` 时该接口需要相同的鉴权（`Authorization: Bearer` 或 `x-admin-token`）。
- 仓库随附可直接使用的可观测性产物：`deploy/prometheus/openllm-alerts.yml` 是 4 组 12 条告警规则，覆盖就绪失败与实例失联、上游熔断跳闸与 Key 冷却、配额池拒绝、准入与内存卸载、运行时阻塞、维护停摆；`deploy/prometheus/openllm-alerts.test.yml` 是这些规则的单元测试（每条都有「会触发」与「健康实例不误报」两例，含物理上难复现的内存压力/运行时阻塞/未就绪）；`deploy/prometheus/prometheus.example.yml` 是抓取配置示例（`job_name` 需保持 `openllm`）；`deploy/grafana/openllm-dashboard.json` 是 24 面板看板，导入时选择 Prometheus 数据源即可。指标语义、阈值取舍、实测触发记录与「告警 → 单条请求」的追踪路径见 [docs/OBSERVABILITY.md](docs/OBSERVABILITY.md)。`scripts/check-monitoring.sh` 一次性校验规则语法、规则单元测试、面板 JSON，以及规则/测试/面板引用的指标名是否真由 `/metrics` 导出（设置 `OPENLLM_METRICS_URL` 时改为对照真实抓取结果）。
- 每个网关响应都带 `x-request-id`（以及同值的 `x-openllm-request-id`），同一个 ID 可在 `GET /api/usage/{request_id}`、`/api/events` 的 `usage` 事件、Webhook 投递载荷与用量导出 CSV 中检索；审计日志属于配置变更史，按 `entity`/`entity_id` 组织，不携带 `request_id`。
- `GET /api/health` 是始终返回 `200` 的存活探针；`GET /api/ready` 是就绪探针，会带 2 秒超时探测数据库，正常返回 `200`，数据库不可用时返回 `503`，适合作为反向代理或负载均衡的健康检查，避免把流量发到无法服务的实例。收到关闭信号、进入排空阶段后，就绪探针改为 `503`（`status: "draining"`）而存活探针仍是 `200`，同时已在途的请求继续完成。注意网关在收到信号的同时就停止接受新连接（实测：信号后新建连接被拒绝，`/api/health`、`/api/ready`、`/api/status` 均不可达），因此负载均衡拿到的实际摘除信号是连接失败；`draining` 标记主要用于单元验证与关闭前的状态观测。
- 后台任务（定时维护、Webhook 投递）带监督重启：任一步骤发生 panic 或意外退出时会被记录并以短暂退避重新拉起，避免一次性故障让维护或投递永久停摆。
- `GET /api/status`（需管理鉴权）把运行期信号汇总成一份 JSON：版本与运行时长、是否正在排空、异步运行时延迟、进程内存（上限/已用/是否触发压力保护/来源）、准入状态（在途请求、并发与字节预算、已拒绝数量）、SQLite 连接池占用、维护运行次数与最近时间，以及提供商熔断摘要（短路、半开数量与阈值）。它始终返回 `200`，便于脚本或看板在一个请求里判断实例是否健康、以及不健康时的原因，而无需解析 Prometheus 文本；控制台仪表盘的“运行状态”卡片就是它的可视化，每 15 秒自动刷新，内存接近上限或熔断/排空时以高亮提示。
- `GET /api/diagnostics/self-check`（需管理鉴权）一次性跑完 13 项结构化自检，覆盖启动（数据库可读写与占用、迁移是否已全部应用、实例租约、关闭配置）、韧性（熔断状态持久化、提供商健康、上游凭据）、路由（可路由模型、网关密钥）、容量（并发与内存上限、运行时阻塞）与可观测性（维护轮次、Webhook 投递）。每项返回 `id`/`category`/`title`/`status`（`pass`/`warn`/`fail`）/`detail`/`hint`，并在 `overall` 与 `counts` 里给出汇总判定；接口设计成不会因故障返回 `5xx`——数据库损坏会表现为某项 `fail`，而不是把自检本身变成 500。
- `GET /api/diagnostics/snapshot`（需管理鉴权）导出一份自包含的诊断快照：版本、主机名、运行时长、实例租约、`/api/status` 的运行状态、运行限制与设置、自检报告、冷却快照、配额池用量、提供商清单（默认把 `authorization`/`api-key`/`token`/`secret`/`cookie` 一类的请求头值掩码成 `***`）、最近 20 条失败请求和 24 小时请求成败统计，并附一段说明导出内容的 `notes`。请求体与响应预览永远不会进入快照。加 `include_secrets=true` 时才会带上上游密钥与未掩码的请求头，该次导出会在审计日志里记录为 `export`/`diagnostics`（只记录动作，不含任何凭据值）。控制台“诊断”页把这些数据一次呈现（自检结果、冷却与熔断、在途容量与并发、配额池分配、最近失败、实例租约），并提供“一键自检”与快照导出/下载按钮。
- 定时维护每 60 秒运行一次（提供商健康检查、模型同步、中断请求对账、用量保留清理）。可在设置页点击"立即运行"，或调用 `POST /api/maintenance/run`（需管理鉴权）立即触发一轮；`openllm_maintenance_runs_total` 与 `openllm_maintenance_last_run_timestamp_seconds` 可确认维护是否在正常执行。
- 配置即代码：`GET /api/config/export`（需管理鉴权）导出一份可导入另一套网关的配置文档，含设置项、提供商（含模型的启用与停用状态）、路由、配额池与 Webhook，所有引用都用名称而非自增 ID（路由目标写提供商名，配额池分配写网关密钥名），因此换一套数据库也能对上。默认情况下上游密钥、Webhook 签名与 `authorization`/`api-key`/`token`/`secret`/`cookie` 一类请求头的值统一掩码成 `***`，并且掩码值永远不会被导入覆盖（导入时直接跳过这些字段），拿掩码文件导入不会把真实凭据写成 `***`；需要迁移凭据时用 `?include_secrets=true` 显式导出，响应里会带明文并在审计日志写入一条 `export`/`config` 记录（只记录动作与条数，不含任何密钥值）。网关 API Key 只保存哈希，文档里以名称作为提示，导入时报告为 `skip` 而不能恢复。`POST /api/config/import` 接受 `{ "document": {...}, "apply": false }`，默认只校验并返回计划：逐项给出 `create`/`update`/`unchanged`/`skip`/`error` 动作与说明，写入顺序为设置 → 提供商 → 配额池 → 路由 → Webhook（池与路由依赖提供商已存在）；引用不到的提供商或网关密钥只把该项标为 `skip` 并附原因，不会因此中断整份导入；只有文档版本高于本进程（`openllm_config_version`）才算致命问题，此时不写任何配置。`apply: true` 才真正写入，重复导入是幂等的：内容相同的项报告为 `unchanged`，不会产生噪声变更。计划里还会给出 `notes` 说明本次导入的注意事项：含密钥导入时上游密钥按“密钥值”匹配现有记录（名字在同一个提供商内并不唯一），未匹配到的密钥会新建、其历史用量统计从零开始；逐项写入不是单个事务，中途失败会保留已写入的部分。控制台“运维”页提供同样的导出／选择文件／仅校验／导入流程。
- 备份与恢复：`POST /api/maintenance/backup`（需管理鉴权）立即写一份备份到数据目录下的 `backups/`（手动备份保留最近 5 份，迁移前备份也是 5 份），`GET /api/maintenance/backups` 列出名称、大小、类型与创建时间，`POST /api/maintenance/backups/verify` 对指定备份做只读校验（SQLite 文件头魔数、`PRAGMA integrity_check`、必备数据表、已应用迁移与各类数据条数），返回 `ok`／`missing_tables`／`problems`。备份用 `VACUUM INTO` 生成，因此拿到的是事务一致的单文件快照（WAL 模式下直接复制文件可能拷到未完成检查点的数据库）。恢复必须停机进行：`openllm --data-dir <目录> --list-backups` 查看，`openllm --restore-backup <文件名>` 执行；恢复前会再次校验备份、拒绝在实例仍在服务时写入（会打印占用数据库的主机名与 PID），并保留原库为 `openllm.db.pre-restore`；恢复时先把原库连同它的 `-wal`/`-shm` 一起改名为 `openllm.db.pre-restore`（因此未完成检查点的已提交数据不会丢，原库仍可打开），再让暂存文件入库，最后清除备份里的实例租约并移除目标路径上的残留 sidecar，因此备份可以直接恢复到全新数据目录，启动后 `GET /api/ready` 返回 200 即表示可用。
- 迁移预检与版本升级检查：`GET /api/maintenance/migrations`（需管理鉴权）在启动前就能报告数据库文件路径、已应用迁移、待执行迁移（含版本与描述）、未知迁移（数据库比二进制新，属于降级风险）、是否含用户数据以及“启动时是否会先写一份迁移前备份”，并且完全只读、不会创建数据库文件。命令行等价的 `openllm --check-migrations` 按退出码区分结果（`0` 已是最新、`2` 有待执行迁移、`1` 需要人工处理），适合放进部署脚本的 pre-flight 步骤；`--backup`／`--list-backups`／`--restore-backup` 与 `--json` 可在不启动网关的情况下完成备份运维。`GET /api/maintenance/upgrade-check` 返回当前版本与候选版本对比（可用 `?latest=0.4.0` 手动指定，或设置 `OPENLLM_UPGRADE_FEED_URL` 从升级源读取），并同时给出待执行迁移数量，避免“升级了二进制但数据库还没迁移”。
- SQLite 默认启用 WAL、`synchronous=NORMAL` 和 busy timeout，适合单实例部署。启动时会读取 `instance_leases` 表：如果同一数据库上还有一个心跳未过期的实例，本进程默认拒绝启动并打印对方的主机名、PID 与心跳时间（可用 `OPENLLM_MULTI_INSTANCE=warn` 强制继续，但两写一库不受支持）。同一主机上进程已经消失的租约会自动接管，因此崩溃重启不会被挡住；Linux 与 Windows 能精确区分刚退出、尚未被父进程回收的僵尸进程，macOS 没有等价的廉价探测，这类未回收进程会按存活处理、由租约 TTL 兜底（默认 300 秒，可用 `OPENLLM_INSTANCE_LEASE_TTL_SECS` 调整）。租约在正常关闭时删除，过期后自动清理。
- 升级前自动备份、迁移失败不启动：启动时若发现数据库还没有应用完所有迁移且库里已有数据，会先用 `VACUUM INTO` 写一份一致性备份到 `<数据目录>/backups/openllm-pre-migration-<目标版本>-<时间戳>.db`，只保留最近 5 份。迁移失败会中止启动并返回错误（附带备份路径），此时监听端口不会打开、失败的那次迁移不会写入 `_sqlx_migrations`、已有数据保持原样，用旧二进制回滚或修好问题后重试即可；备份文件属于普通数据文件，恢复时停服后用备份替换 `openllm.db` 即可（首次启动会接管已过期的租约）。
- 开启 `OPENLLM_ADMIN_TOKEN` 时，SSE 事件接口通过 `admin_token` 查询参数鉴权，因为浏览器 EventSource 不支持自定义请求头。
- `data/`、`dist/`、`target/` 和 `web/node_modules/` 默认不纳入版本控制。

## 发布流程

GitHub Actions 不会在普通分支推送时构建。Pull Request 和手动触发会执行测试与构建；推送 `v*` 格式的标签时会创建 GitHub Release，并上传 Linux、Windows 和 macOS 二进制及校验文件。

```bash
git tag v0.3.18
git push origin v0.3.18
```

发布后的文件会出现在 Releases 页面：

<https://github.com/jackclub-cn/openllm/releases>

## License

MIT License，Copyright (c) 2026 jackclub-cn。完整条款见 [LICENSE](LICENSE)。
