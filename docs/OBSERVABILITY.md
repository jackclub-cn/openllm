# OpenLLM 可观测性、指标与告警

本文覆盖四件事：抓取配置、`GET /metrics` 的指标语义、随仓库提供的告警规则与 Grafana 面板、以及从告警回到单条请求的追踪路径。

网关仍然是零依赖单二进制，可观测性产物全部是可选的旁路组件：不部署 Prometheus/Grafana 时网关功能不受任何影响，只是少一层趋势与告警能力。

## 目录

- [1. 产物清单](#1-产物清单)
- [2. 抓取配置](#2-抓取配置)
- [3. 指标速查](#3-指标速查)
- [4. 告警规则](#4-告警规则)
- [5. Grafana 面板](#5-grafana-面板)
- [6. /api/status 与 /api/ready 的分工](#6-apistatus-与-apiready-的分工)
- [7. 请求 ID 追踪串联](#7-请求-id-追踪串联)
- [8. 排障速查](#8-排障速查)
- [9. 最近请求实时状态](#9-最近请求实时状态)
- [10. 历史阶段耗时与归因](#10-历史阶段耗时与归因)

## 1. 产物清单

| 文件 | 用途 |
| --- | --- |
| `deploy/prometheus/prometheus.example.yml` | 最小可用的 Prometheus 抓取配置示例 |
| `deploy/prometheus/openllm-alerts.yml` | 告警规则（4 组 12 条），可直接被 Prometheus 加载 |
| `deploy/prometheus/openllm-alerts.test.yml` | 规则单元测试：用合成序列验证 12 条规则的表达式、`for` 时长与告警文案 |
| `deploy/grafana/openllm-dashboard.json` | Grafana 面板（24 个面板，含实例/提供商变量），导入即用 |

规则文件用官方工具校验：

```bash
cd deploy/prometheus
docker run --rm --entrypoint promtool -v "$(pwd):/rules" -w /rules \
  prom/prometheus:latest check rules openllm-alerts.yml
# SUCCESS: 12 rules found

docker run --rm --entrypoint promtool -v "$(pwd):/rules" -w /rules \
  prom/prometheus:latest test rules openllm-alerts.test.yml
# SUCCESS
```

规则单元测试与实测互补：实测（下面「验证记录」一节）证明规则在真实网关上会被触发，单元测试则用合成序列把每一条规则都钉死—— 包括物理上难以复现的几条（内存压力采样目前只在 Linux 生效、运行时阻塞需要卡住 worker 线程、`openllm_ready = 0` 只在排空瞬间可能出现而那时抓取端口已经关闭）。每条规则都有正反两例：正向证明会触发，反向证明健康实例不会误报。

> 测试文件里的 `exp_annotations` 是完整断言，因此改告警文案后需要同步更新：promtool 失败信息会直接打印新旧文本，复制过去即可。

仓库内另有 `scripts/check-monitoring.sh`，一次性完成「规则语法校验」「规则单元测试」「面板 JSON 校验」与「规则/测试/面板引用的指标名确实由 `/metrics` 导出」四项检查，适合放进 CI。不加参数时指标名对照 `src/api/metrics/` 各领域模块里的声明（无需起网关）；设 `OPENLLM_METRICS_URL` 则改为对照真实抓取结果。

## 2. 抓取配置

`GET /metrics` 是 Prometheus 文本格式（text/plain; version=0.0.4）。默认无需鉴权；一旦设置了 `OPENLLM_ADMIN_TOKEN`，该接口与 `/api/status` 一样要求 `Authorization: Bearer <token>` 或 `x-admin-token: <token>`。

`deploy/prometheus/prometheus.example.yml` 的等价片段：

```yaml
scrape_configs:
  - job_name: openllm
    metrics_path: /metrics
    scrape_interval: 15s
    static_configs:
      - targets: ["127.0.0.1:8080"]
    # 仅在设置了 OPENLLM_ADMIN_TOKEN 时需要
    authorization:
      credentials_file: /etc/prometheus/openllm-admin-token
```

两点注意：

- **`job_name` 请保持 `openllm`**，`OpenLlmTargetDown` 规则用的是 `up{job="openllm"} == 0`。改名需要同步改规则。
- **单实例部署**：同一 SQLite 文件只允许一个写进程，所以 Prometheus 侧也没有副本聚合可言。多实例属于不受支持的用法，`sum()`/`max()` 聚合在单实例下等价于直读。

## 3. 指标速查

全部指标名以 `openllm_` 开头。除特别标注外均为 gauge；`*_total` 是自进程启动起的 counter（进程重启即归零，因此趋势要用 `rate()`/`increase()`，不要直接看绝对值）。

### 3.1 进程与就绪

| 指标 | 含义 |
| --- | --- |
| `openllm_info{version}` | 恒为 1，`version` 标签是构建版本，用于「当前跑的是哪个版本」 |
| `openllm_ready` | 1 = 正在接受新请求；0 = 已进入排空（收到关闭信号） |
| `openllm_uptime_seconds` | 运行时长；归零即进程重启过 |
| `openllm_start_time_seconds` | 启动时的 Unix 时间戳 |
| `openllm_runtime_lag_millis` | 异步运行时定时器的迟到毫秒数；持续偏大说明有 worker 被阻塞 |

### 3.2 流量与开销

| 指标 | 含义 |
| --- | --- |
| `openllm_requests_total` | 累计请求数（counter） |
| `openllm_requests_successful_total` / `openllm_requests_failed_total` | 累计成功/失败（counter） |
| `openllm_requests_in_flight` | 当前在途请求数 |
| `openllm_tokens_total{type}` | 累计 Token，`type` 为 `prompt`/`completion`/`total`/`cache_read`/`cache_write` |
| `openllm_average_latency_ms` | 平均延迟（毫秒） |
| `openllm_estimated_cost_usd_total` | 累计估算费用（counter，美元） |
| `openllm_unpriced_requests_total` | 因模型未配置价格而无法计费的请求数（counter） |

### 3.3 容量、准入与内存

| 指标 | 含义 |
| --- | --- |
| `openllm_request_capacity_limit` | 全局并发上限；0 表示未限制 |
| `openllm_request_capacity_available` | 仍可用的全局并发槽位 |
| `openllm_requests_shed_total` | 被本地准入/内存压力拒绝的请求数（counter） |
| `openllm_inflight_request_bytes` / `openllm_inflight_request_bytes_limit` | 在途请求体字节预算的当前占用与上限 |
| `openllm_request_bytes_shed_total` | 因在途字节预算触顶被拒绝的请求数（counter） |
| `openllm_memory_used_bytes` | 进程内存占用 |
| `openllm_memory_limit_bytes` / `openllm_memory_threshold_bytes` | 内存上限与触发卸载的阈值；0 表示该保护关闭 |
| `openllm_memory_pressure` | 1 = 正在因内存压力卸载新请求 |
| `openllm_memory_shed_total` | 因内存压力被拒绝的请求数（counter） |

### 3.4 熔断与冷却

| 指标 | 含义 |
| --- | --- |
| `openllm_provider_health{provider,state}` | 提供商健康分组计数 |
| `openllm_provider_circuit_state{provider,state}` | 熔断状态 one-hot，`state` 为 `closed`/`open`/`half_open`，对应值为 1 的系列即当前状态 |
| `openllm_provider_circuit_trips_total{provider}` | 熔断跳闸次数（counter） |
| `openllm_provider_failure_streak{provider}` | 当前连续失败次数 |
| `openllm_provider_cooling{provider}` | 1 = 该提供商因运行期冷却被排除在路由之外 |
| `openllm_model_cooling{provider,model}` | 1 = 该模型处于冷却 |
| `openllm_provider_key_cooling{provider,key,key_id}` | 1 = 该上游 Key 被暂时移出轮转 |
| `openllm_provider_concurrency_limit{provider}` / `openllm_provider_inflight{provider}` | 提供商并发上限与当前占用 |
| `openllm_model_concurrency_limit{provider,model}` / `openllm_model_inflight{provider,model}` | 模型级并发上限与当前占用 |

熔断状态、连续失败与冷却状态都**持久化在 SQLite**：进程重启后不会「忘记」故障提供商而立刻把流量打回坏上游。因此面板上看到的 `open` 可能是上一个进程记录的、尚未探测成功的状态，这是预期行为。

### 3.5 上游用量（按提供商）

| 指标 | 含义 |
| --- | --- |
| `openllm_provider_requests_total{provider}` | 转发给该提供商的请求数（counter） |
| `openllm_provider_successes_total{provider}` | 其中成功的数量（counter） |
| `openllm_provider_prompt_tokens_total{provider}` / `openllm_provider_completion_tokens_total{provider}` | 输入/输出 Token（counter） |
| `openllm_provider_average_latency_ms{provider}` | 该提供商平均延迟 |
| `openllm_provider_last_test_latency_ms{provider}` | 最近一次健康探测的延迟 |
| `openllm_provider_models{provider}` / `openllm_provider_models_disabled{provider}` | 该提供商模型总数与已禁用数 |

### 3.6 提供商配额池（公平分配）

一个「配额池」把某个提供商被共享的上游额度按权重切给各网关 API Key。

| 指标 | 含义 |
| --- | --- |
| `openllm_pool_requests_window{pool,provider_id}` | 滚动窗口内已消耗的请求数 |
| `openllm_pool_request_limit{pool,provider_id}` | 请求上限；0 表示不限 |
| `openllm_pool_tokens_window{pool,provider_id}` / `openllm_pool_token_limit{pool,provider_id}` | 同一窗口的 Token 用量与上限 |
| `openllm_pool_saturated{pool,provider_id}` | 1 = 达到饱和阈值（此后只做公平分配，不再放行无归属流量） |
| `openllm_pool_denials_total{pool,provider_id}` | 池拒绝的请求数（counter） |
| `openllm_pool_key_requests_window{pool,provider_id,key_id}` / `openllm_pool_key_request_share{...}` | 单个 Key 在窗口内的请求用量与它分到的额度 |
| `openllm_pool_key_tokens_window{pool,provider_id,key_id}` / `openllm_pool_key_token_share{...}` | 同上，Token 维度 |

池是「工作保留（work-conserving）」的：没有池的 Key 也发来的流量在未饱和时可以借用空闲额度，饱和后才受各自配额约束。所以 `openllm_pool_saturated` 从 0 跳到 1 时分到的份额才开始产生拒绝，这是设计意图而不是故障。

### 3.7 配置清单与数据库

| 指标 | 含义 |
| --- | --- |
| `openllm_providers{state}`、`openllm_provider_keys{state}`、`openllm_models{state}`、`openllm_routes{state}`、`openllm_api_keys{state}` | 各类配置对象按状态分组计数 |
| `openllm_audit_logs` | 审计日志条数 |
| `openllm_db_pool_connections` / `openllm_db_pool_idle` / `openllm_db_pool_max_connections` | SQLite 连接池占用/空闲/上限 |
| `openllm_database_size_bytes` / `openllm_database_free_bytes` | 数据库文件大小与可回收空间 |
| `openllm_maintenance_runs_total` | 维护周期执行次数（counter） |
| `openllm_maintenance_last_run_timestamp_seconds` | 最近一次维护完成的 Unix 时间戳 |

## 4. 告警规则

`deploy/prometheus/openllm-alerts.yml` 共 4 组 12 条，全部只用上表中的指标名。

| 组 | 告警 | 触发条件（要点） | 处置方向 |
| --- | --- | --- | --- |
| availability | `OpenLlmNotReady` | `openllm_ready == 0` 持续 2m | 确认是否在升级；否则进程异常，看日志与 `/api/status` |
| availability | `OpenLlmTargetDown` | `up{job="openllm"} == 0` 持续 2m | 排空瞬间断开属预期，持续不可达即进程已退出 |
| resilience | `OpenLlmProviderCircuitOpen` | `openllm_provider_circuit_state{state="open"} == 1` 持续 5m | 查上游状态/凭据/限流，或把流量改走其它提供商 |
| resilience | `OpenLlmProviderCircuitTrips` | 15m 内跳闸 `>= 3` | 反复跳闸多为间歇性上游故障，而非硬故障 |
| resilience | `OpenLlmProviderKeyCooling` | 某 Key 冷却持续 10m | 单 Key 反复 401/403/429：吊销或配额 |
| resilience | `OpenLlmPoolDenials` | 10m 内出现池拒绝 | 池/权重过小，或突发流量不在预期内 |
| capacity | `OpenLlmRequestsShed` | 10m 内出现本地卸载 | 调大并发上限/内存上限，或扩容 |
| capacity | `OpenLlmMemoryPressure` | `openllm_memory_pressure == 1` 持续 5m | 对照 used/limit 与 `/api/status` 的 `memory_limit_source` |
| capacity | `OpenLlmAdmissionSaturated` | 并发槽位 10m 内持续占满（`limit > 0` 且 `available == 0`） | 延迟会爬升，随后排队或卸载 |
| capacity | `OpenLlmRuntimeLag` | `openllm_runtime_lag_millis > 5000` 持续 5m | 热路径上有阻塞调用或内存吃紧 |
| maintenance | `OpenLlmMaintenanceStalled` | 距上次维护完成 `> 300s` 持续 5m | 60s 维护周期停摆：自愈、模型同步、对账全部暂停 |
| maintenance | `OpenLlmMaintenanceNotRunning` | `runs_total == 0` 且运行时长 `> 300s` | 维护 worker 可能没起来 |

阈值都是「单实例、默认配置」下的保守取值，请按自己的 SLA 调整。`for` 时长故意留出一次正常重启的窗口（排空 + 重启 + 迁移），避免升级期间误报。

### 4.1 验证记录

规则不只是「promtool 说语法对」。实测过一轮：真网关 + 一个可控上游桩（返回 401 / 挂住 N 秒 / 监听一个没人听的端口），把 Prometheus 的 `for` 临时改 0，然后观察 `/api/v1/alerts`。

当时实际被触发的（括号内是制造方式）：

| 告警 | 制造方式 | 现场证据 |
| --- | --- | --- |
| `OpenLlmProviderCircuitOpen` | 提供商指向死端口，跳闸后设 `configured_cooldown_seconds: 600` 让开路状态跨越抓取点 | 跳闸到第 3 次，`circuit_trips_total` 3 |
| `OpenLlmProviderCircuitTrips` | 同上（15m 内跳闸 3+） | 同上 |
| `OpenLlmProviderKeyCooling` | 让上游返回 401（`invalid_api_key`） | `provider_key_cooling == 1`，`/api/resilience/cooldowns` 报 `remaining_seconds: 300` |
| `OpenLlmPoolDenials` | 对提供商建 `request_limit: 1` 的 hard 池，第二个请求被拒 | `pool_denials_total` 2，客户端收到 429 + `Retry-After` |
| `OpenLlmRequestsShed` | `OPENLLM_MAX_CONCURRENT_REQUESTS=2` + `OPENLLM_ADMISSION_WAIT_MS=0`，并发发起 6 个长请求 | 4 个 429，`requests_shed_total` 4 |
| `OpenLlmAdmissionSaturated` | 同上，且保持两个槽位在抓取时刻仍被占住 | `request_capacity_available 0 / limit 2` |
| `OpenLlmTargetDown` | 直接停掉网关进程 | 抓取失败持续 2m 后触发 |

一个容易踩的坑：`OpenLlmProviderCircuitOpen` 看的是**抓取那一瞬间**的 `state="open"`。默认冷却只有几十秒，若在冷却结束时才抓取，状态已经是 `half_open`，告警不会触发——这是符合设计的行为（半开表示正在保探针），不是漏报。要让它稳定可观测，把该提供商的 `configured_cooldown_seconds` 调到远大于抓取间隔。

剩下 5 条无法在真实进程上稳定复现，改由 `openllm-alerts.test.yml` 用合成序列锁定：`OpenLlmNotReady`（只在排空瞬间为 0，而那一刻抓取端口已关闭）、`OpenLlmMemoryPressure`（内存采样目前在 Linux 生效，Windows 上 `OPENLLM_MEMORY_LIMIT_MIB` 被忽略并在启动日志里提示）、`OpenLlmRuntimeLag`（需要把 worker 线程阻塞 5s 以上）、`OpenLlmMaintenanceStalled` / `OpenLlmMaintenanceNotRunning`（都是健康实例上**故意为假**的负向条件，实测确认不误报，且依赖序列都存在）。

## 5. Grafana 面板

导入 `deploy/grafana/openllm-dashboard.json`（Dashboards → New → Import → 上传 JSON），选择绑定的 Prometheus 数据源即可。JSON 里声明了 `DS_PROMETHEUS` 输入，导入时提示选择数据源。

面板按四行组织：

1. **Health** — 就绪状态、运行时长、在途请求、内存占用/上限、距上次维护的时长（超过 180s 转黄、300s 转红）。
2. **Traffic** — 成功/失败请求速率、按类型的 Token 速率。
3. **Resilience** — 各提供商熔断状态、跳闸次数、冷却中的提供商/模型/Key 数量、上游成功率、提供商延迟、配额池饱和与拒绝速率、池窗口用量对上限。
4. **Capacity, database and runtime** — 准入与卸载、内存压力、数据库大小与连接池、运行时延迟、平均延迟与费用、配置清单。

两个模板变量：`instance`（多实例时筛选）与 `provider`（只看某几个提供商）。

## 6. `/api/status` 与 `/api/ready` 的分工

三条接口的用途不同，不要混用：

| 接口 | 鉴权 | 用途 |
| --- | --- | --- |
| `GET /api/health` | 无 | 存活探针，恒 200 |
| `GET /api/ready` | 无 | 就绪探针，带 2s 数据库探测；正常 200，数据库不可用或正在排空时 503 |
| `GET /api/status` | 管理鉴权 | 运行期快照 JSON：版本、运行时长、是否排空、运行时延迟、内存（上限/已用/是否压力/来源）、准入状态、SQLite 连接池、维护次数与时间、提供商熔断摘要 |
| `GET /metrics` | 管理鉴权（若已设置） | 时序指标 |

`/api/status` 的价值在于**一次请求拿到结论**：脚本或看板无需解析 Prometheus 文本就能判断实例是否健康以及不健康的直接原因（内存吃紧？熔断？排空中？）。`/metrics` 负责趋势与告警，`/api/status` 负责「现在到底怎么了」。控制台仪表盘的「运行状态」卡片就是 `/api/status` 的可视化。

## 7. 请求 ID 追踪串联

每一次网关请求都会生成一个 `request_id`，并在三处可查，形成告警 → 单请求的闭合链路：

```
x-request-id / x-openllm-request-id  响应头
      │
      ├─► GET /api/usage/{request_id}        单条请求明细（延迟、首字、Token、状态码、错误）
      ├─► GET /api/events  (SSE, event: usage) data.request_id   实时事件流
      └─► GET /api/usage/export (CSV) 的 request_id 列           离线分析
```

- 客户端侧：所有响应都带 `x-request-id` 头（同值的 `x-openllm-request-id` 也在），SSE 流式响应同样带；把它写进调用方日志即可与服务端对齐。
- 服务端侧：控制台「请求记录」里点开某一条即是 `/api/usage/{request_id}` 的明细视图。
- 事件侧：`/api/events` 的 `usage` 事件带 `request_id`、`success`、`streamed`，是「事件 → 请求」的连接点。

需要注意：**审计日志（audit_logs）是配置变更史，不携带 `request_id`**。它按 `entity`/`entity_id`（provider / route / api_key / settings / database / usage）组织，回答的是「谁在什么时候改了什么」，与请求平面是两条独立的追溯线索。请求平面用 `request_id`，配置平面用 `entity_id`。

典型闭环：`OpenLlmPoolDenials` 或 `OpenLlmProviderCircuitOpen` 报警 → 在控制台按时间/提供商筛请求记录 → 复制失败请求的 `request_id` → `/api/usage/{request_id}` 看到具体错误码与上游响应 → 对照 `/api/events` 确认是否成片失败。

## 8. 排障速查

| 症状 | 先看 | 再看 |
| --- | --- | --- |
| 上游大面积 503 | `openllm_provider_circuit_state`、`openllm_provider_cooling` | 提供商凭据与上游状态；是否需要改路由 |
| 请求被拒（429/503 且无上游延迟） | `openllm_requests_shed_total`、`openllm_request_capacity_available` | 调大 `OPENLLM_MAX_CONCURRENT_REQUESTS` / 内存上限 |
| 只有部分 Key 受影响 | `openllm_pool_denials_total`、`openllm_pool_key_*` | 池权重是否与业务量匹配 |
| 延迟整体抬升 | `openllm_runtime_lag_millis`、`openllm_average_latency_ms` | 内存压力、热路径阻塞调用 |
| 健康检查开始失败但仍能访问 | `openllm_ready`、`/api/ready` | 是否在排空；数据库是否可达 |
| 自愈不生效 | `openllm_maintenance_last_run_timestamp_seconds` | 维护 worker 是否在跑 |
| 磁盘增长 | `openllm_database_size_bytes` / `_free_bytes` | 日志保留天数、手动整理数据库 |

## 9. 最近请求实时状态

运行概览的“实时状态”直接展示一条请求的链路、阶段耗时、候选选择原因、各次尝试、
回退、上下文分类和缓存，无需点击列表。优先展示最新发起且仍在进行中的请求；
全部结束时展示按开始时间排序的最近一条，并显示进行中总数。暂停自动刷新时，
活动快照与本地递增耗时一并暂停。请求日志入口跳转到用量页，最终日志和路由轨迹
从 SQLite 读取。原“最近请求”历史表与 60 秒请求时间轴继续保留。

| 接口 | 语义 |
| --- | --- |
| `GET /api/activity/recent?limit=50` | 最近快照；`limit` 为 1-200，含全局活动数、`enabled`、进程 ID 和事件水位 |
| `GET /api/activity/{request_id}` | 当前进程的单请求快照；过期返回 404，可回退到 `/api/usage/{request_id}` |
| `GET /api/events` | 复用原连接，保留 `usage`，增加 `activity` 和 `activity_resync` |

这些接口沿用管理鉴权。快照和活动事件不包含提示词、响应、工具参数、工具结果或
密钥正文。`instance_id` 区分重启，`sequence` 是全局事件序号，`revision` 是请求
版本。客户端在订阅后拉快照，回放快照水位之后的事件；遇缺口、缓冲落后或重连
重新同步。控制台断线时每 4 秒轮询，已连接时每 30 秒校准。

活动记录保留到完成；完成记录最多保留 256 条、5 分钟。候选最多 32 个，尝试保留
最近 32 次并另存总计数和截断标记。列表最多 200 条不代表全体进行中请求，活动数
来自全存储。存储位于内存，进程重启后历史仍由用量日志与路由轨迹提供。

上下文分类包括系统提示、工具定义、工具调用、工具结果、推理历史和会话内容，
分类始终是估算。Responses 的加密推理历史会把无法直接读取的 Token 差额归入
推理历史；只有多模态或无法识别的内容保留未计量标记及未归因部分。窗口来自最终
目标的模型元数据，有效输入上限单独显示；缺数据时显示未知。
缓存读取占模型窗口。流中已返回的缓存 usage 立即展示，缺字段与真实零分开；
旧记录没有字段来源时不能认定零命中。分类与缓存来源随迁移 0057 持久化。

阶段耗时使用单调时钟，累计准备、路由与等待、上游响应头、响应读取/首输出等待、
流式生成；首 Token 是里程碑，不加入阶段合计。非流式不展示首 Token 和生成阶段。
不拆分不可可靠测量的 DNS、TCP、TLS 或供应商内部排队时间。

设置 `OPENLLM_LIVE_REQUEST_TRACE=0` 后关闭活动记录与事件，控制台显示已关闭；
原有请求日志、路由轨迹和路由行为保持不变。

## 10. 历史阶段耗时与归因

迁移 `0058_usage_performance.sql` 在 `usage_logs.performance_json` 保存最终性能摘要，
随原有完成日志一起写入，不增加每阶段的数据库写操作。完成、失败和取消都保留
可观测的阶段；活动记录淘汰或网关重启后，历史请求详情仍可显示这些数据。

`GET /api/usage/{request_id}` 和请求列表中的 `performance` 包括：

- `timings`：准备、路由与等待、上游响应头、首输出等待/响应读取、生成、首 Token 和总耗时。
- `attempt_count` / `fallback_count`：总尝试和回退次数。
- `failed_attempts_ms`：已保留失败尝试的耗时合计；不含退避，也不单独代表全部回退开销。
- `attempts_truncated`：尝试摘要有截断时，失败耗时只覆盖已保留部分，页面明确标记。

实时状态与历史请求详情共用耗时组件，指出已测量阶段中耗时最长的部分及其占比，
并显示显著的未归因时间。首 Token 是里程碑，不与各阶段重复相加；非流式不显示
首输出等待和生成阶段。最长阶段表示耗时构成，不自动认定为故障，也不推测供应商
内部排队或底层网络原因。旧日志、活动记录关闭或无法解析的摘要显示无阶段数据，
不回填估算值。该摘要不包含提示词、响应、工具参数或密钥正文。
