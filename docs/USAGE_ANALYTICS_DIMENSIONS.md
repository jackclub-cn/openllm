# 用量分析维度缺口盘点

> 基线：openllm v0.3.27（本文写于该版本，0.3.28 未改动用量分析相关代码）。
>
> 目的：盘点当前「用量分析」(`/analytics`)、「仪表盘」(`/dashboard`) 与「请求日志」
> (`/usage`) 三个视图已经具备的统计维度，列出仍缺失的维度、它们的数据就绪度，以及
> 建议的落地顺序。
>
> 本文只做盘点与排期建议，不改变任何运行时行为。除非另有说明，所有结论都来自当前
> 代码：`src/api/usage.rs`、`src/models/usage.rs`、`web/src/pages/Analytics.tsx`、
> `web/src/pages/dashboard/`、`web/src/pages/usage/`。

## 1. 背景与统计口径

用量分析页回答的是「这段时间钱花在哪、性能如何、有没有异常」。请求日志页回答的是
「具体哪一条请求出了问题」。两者共用同一张 `usage_logs` 表，但服务的目标不同：
分析页需要聚合与对比，日志页需要筛选与逐条下钻。

先固定统计口径，否则同一条数据在两个视图里会得出不同结论。

| 口径 | 定义 | 说明 |
| --- | --- | --- |
| 请求数 | `in_flight = 0` 的 `usage_logs` 行数 | 进行中的请求单独统计，不计入成功率与费用 |
| 会话数 | 去重后的非空 `session_id` 数量 | 仅在调用方传入会话标识时才有数据 |
| 总 tokens | `SUM(total_tokens)` | 归零时由 `prompt_tokens + completion_tokens` 兜底 |
| 费用 | `SUM(estimated_cost_micros)` | 单位微美元；未定价请求写入 `NULL` 并单独计数 |
| 未定价 | `estimated_cost_micros IS NULL` 的行数 | 缺失价格时不静默按 0 计价 |
| 成功率 | `AVG(success) * 100` | 分母是范围内的已完成请求 |
| 平均总用时 | `AVG(latency_ms)` | 端到端耗时，含排队与首字等待 |
| 首字用时 | `first_token_ms` | 流式请求是首 content token 时刻；非流式回落为总用时；失败为空 |
| 生成速度 | `output_tps` | `completion_tokens / 生成阶段秒数`；流式排除首字等待，无输出时为 `None` |
| 缓存命中率 | `cache_read / prompt_tokens` | 读写 token 分别统计 |
| 时间范围 | `from` / `to`，最长 366 天 | `MAX_OVERVIEW_RANGE_DAYS` |
| 时间颗粒度 | ≤ 24 小时按小时，否则按天 | 由范围长度自动决定，不可手选 |

时间戳统一以 UTC 存储，展示时按调用方 `tz_offset_minutes` 换算，所以「今天」和按天
分桶都遵循操作者本地时区。

## 2. 当前能力盘点

### 2.1 页面结构

分析页目前是「顶部时间筛选 + 一组指标卡 + 三个 Tab」：

| 区域 | 组件 | 内容 |
| --- | --- | --- |
| 顶部 | `UsageRangeToolbar` | 日期范围、自动刷新、实时连接状态、跳转请求日志 |
| 指标卡 | `AnalyticsMetrics` | 请求数、总 tokens、费用、成功率、平均总用时、缓存命中率 |
| Tab：趋势与分布 | `DashboardCharts` | 折线：请求数 / 总 tokens / 按模型 / 按提供商；饼图：模型与提供商请求分布 |
| Tab：模型 | `DashboardModelUsage` | 模型、请求数、占比、输入/输出 tokens、费用、成功率、平均总用时 |
| Tab：提供商 | `AnalyticsProviderUsage` | 提供商、请求数、占比、tokens、费用、成功率、平均总用时 |

### 2.2 指标明细

| 指标 | 现状 | 缺口 |
| --- | --- | --- |
| 请求数 | 有，含会话数与每会话请求数 | 缺环比、缺分维度拆分 |
| tokens | 总量 + 输入/输出 | 缺「每请求平均 tokens」 |
| 费用 | 有，含未定价计数 | 缺费用趋势、缺「每请求/每百万 token 费用」 |
| 成功率 | 有 | 缺失败原因、状态码分布 |
| 延迟 | 只有平均值 | 缺 p50 / p95 / p99，缺首字延迟 |
| 生成速度 | 明细里有 `output_tps` | 分析页完全没有聚合 |
| 缓存 | 有命中率与读写量 | 缺趋势、缺分模型/提供商拆分 |
| 网关调整 | 计数 + 可跳转日志 | 未分类（工具历史修复 vs 参数调整） |

### 2.3 请求日志已有的筛选维度

日志页已经支持按下列维度筛选，可作为分析页统一筛选器的直接参照：

| 维度 | 参数 | 字段 |
| --- | --- | --- |
| 模型 | `model` | `requested_model` |
| 请求 ID | `request_id` | `request_id` |
| 会话 ID | `session_id` | `session_id` |
| 接口 | `endpoint` | `endpoint` |
| 提供商 | `provider_id` | `provider_id` |
| 上游密钥 | `provider_api_key_id` | `provider_api_key_id` |
| 访问密钥 | `api_key_id` | `api_key_id` |
| 路由 | `route_id` | `route_id` |
| 状态 | `success` / `in_flight` | `success`、`in_flight` |
| 网关调整 | `gateway_adjusted` | `warning_message` 非空 |
| 时间 | `from` / `to` | `created_at` |

分析页目前**只有时间筛选**，上面这些维度一个都没有。

### 2.4 其他参考实现

- `src/api/diagnostics.rs` 已经有一个「按 `status_code` 统计失败」的近 24 小时聚合，
  分析页的状态码分布可以直接复用它的口径。
- `/metrics`（Prometheus）提供的是生命周期累计值与即时状态，没有时间窗内按维度的
  拆分，因此不能替代分析页的聚合接口。
- `web/src/pages/analytics/devProviderMock.ts` 是开发环境注入的假数据，只在缺失提供商
  数据时用于预览图表，正式路径不经过它。

## 3. 数据就绪度

### 3.1 `usage_logs` 字段

| 字段 | 可用于的维度 |
| --- | --- |
| `request_id` / `session_id` | 单请求追踪、会话分析 |
| `api_key_id` | 访问密钥维度 |
| `provider_id` / `provider_api_key_id` | 提供商与上游密钥维度 |
| `route_id` | 路由维度 |
| `requested_model` / `upstream_model` | 模型维度、请求模型到上游模型映射 |
| `endpoint` | 接口/协议维度 |
| `prompt_tokens` / `completion_tokens` / `total_tokens` | tokens 维度 |
| `cache_read_tokens` / `cache_write_tokens` | 缓存维度 |
| `estimated_cost_micros` | 费用维度（`NULL` 表示未定价） |
| `latency_ms` / `first_token_ms` | 延迟与首字维度 |
| `status_code` / `success` / `error_message` | 错误与状态维度 |
| `streamed` | 流式/非流式拆分 |
| `warning_message` | 网关调整分类 |
| `in_flight` / `last_activity_at` | 进行中请求 |
| `created_at` | 时间维度 |

### 3.2 `routing_traces` / `routing_attempts`

路由质量分析的数据已经落库，但**只用于单请求的轨迹视图，没有做聚合**：

| 字段 | 可用于的指标 |
| --- | --- |
| `strategy` | 各策略的使用量与成功率对比 |
| `session_decision` / `session_id` | 粘性命中率、粘性打破原因 |
| `attempt_count` / `fallback_count` | 平均尝试次数、fallback 率 |
| `candidates_json` | 候选命中/落空分析 |
| `attempts.reason_code` / `failure_scope` / `failure_retry` | 失败原因与故障域分类 |
| `attempts.latency_ms` / `status_code` | 逐次尝试的延迟与错误分布 |

### 3.3 现有索引

目前与聚合相关的索引：`created_at`、`provider_id + created_at`、`route_id + created_at`、
`requested_model + created_at`、`api_key_id + created_at`、`provider_api_key_id + created_at`、
`session_id + created_at`、`in_flight + created_at`。

**尚未建索引**、但新维度可能需要的列：`upstream_model`、`endpoint`、`status_code`、
`success`。在按这些列做大范围聚合前需要评估索引成本。

## 4. 维度缺口矩阵

| 优先级 | 缺口 | 数据情况 |
| --- | --- | --- |
| P0 | 分析页缺少全局维度筛选（模型、提供商、路由、Key、状态、接口） | 字段齐全，日志页已有筛选实现 |
| P0 | 费用趋势 | 可直接聚合 |
| P0 | 模型/提供商趋势只支持请求数，缺 tokens、费用、成功率 | 需扩 `overview` 聚合 |
| P0 | 延迟只有平均值，无 p50/p95/p99 | `latency_ms` 可用 |
| P0 | 首 token 延迟完全没有分析 | `first_token_ms` 可用 |
| P0 | TPS / 生成速度没有聚合分析 | 可从 tokens 与耗时计算，与明细口径一致 |
| P1 | 错误与状态码分布、失败原因 | `status_code`、`error_message` 可用 |
| P1 | 网关调整分类 | `warning_message` 可用，需要归类规则 |
| P1 | 访问密钥维度 | `api_key_id` 可用 |
| P1 | 上游密钥维度 | `provider_api_key_id` 可用 |
| P1 | 路由维度 | `route_id` 可用 |
| P1 | 接口/协议维度 | `endpoint` 可用 |
| P1 | 缓存趋势与按模型/提供商拆分 | 缓存字段可用 |
| P1 | 会话分析只有总量，缺 Top 会话与会话成本 | `session_id` 可用 |
| P1 | 路由质量：fallback、尝试次数、粘性命中、失败原因 | `routing_traces` / `routing_attempts` 已落库 |
| P2 | 请求模型到上游模型的映射聚合 | 字段可用 |
| P2 | 流式/非流式拆分 | `streamed` 可用 |
| P2 | 长尾模型丢失 | 模型聚合 `LIMIT 8`，趋势 `LIMIT 6` 后并入「其他」 |
| P2 | 平均 tokens/请求、费用/请求、费用每百万 token | 可计算 |
| P2 | 环比 / 上一周期对比 | 需要双区间查询 |
| P2 | 手选小时/日/周/月颗粒度 | 当前按范围自动决定 |
| P3 | 工具、图片、推理请求占比 | 当前未结构化存储 |
| P3 | reasoning tokens | 需要新增字段与上游返回解析 |
| P3 | 重试造成的 token/费用浪费 | 需要按 attempt 记录用量 |
| P3 | 队列等待、冷启动、并发拥堵 | 需要新增运行时埋点 |

## 5. 数据就绪度分级

把缺口按「改动成本」分成三档，方便决定先做哪一批。

| 档位 | 含义 | 覆盖缺口 |
| --- | --- | --- |
| A：现成可做 | 字段已存在，扩展聚合 SQL 与前端即可 | 全局筛选、费用趋势、延迟分位、首字延迟、TPS、状态码分布、网关调整分类、访问密钥/上游密钥/路由/接口维度、缓存拆分、会话深挖、流式拆分、长尾模型、平均指标 |
| B：需先补结构 | 数据在库但要新增索引或查询形态 | `upstream_model`/`endpoint`/`status_code` 组合聚合的索引成本；环比与周期对比的双区间接口 |
| C：需新增埋点 | 当前没有任何结构化来源 | reasoning tokens、工具/图片/推理占比、重试浪费、队列等待与冷启动 |

C 档需要同时改写出路径、迁移与上游解析，单独立项评估，不放进前两批。

## 6. 建议阶段

### M1：让分析页可用（A 档为主）

1. 统一筛选器：把日志页已有的维度提炼成共享组件，分析页与日志页复用。
2. 费用趋势：折线图新增「费用」指标。
3. 性能分位：p50/p95/p99 总用时 + 首字延迟分位 + TPS 聚合。
4. 错误分析：状态码分布、失败原因 Top N、网关调整分类。

目标：一屏之内能回答「花了多少钱、慢在哪、错在哪」。

### M2：补齐拆分维度（A 档剩余）

1. 访问密钥 / 上游密钥 / 路由 / 接口四个维度的请求数、tokens、费用、成功率。
2. 缓存趋势与按模型、提供商拆分。
3. 会话深挖：Top 会话、会话成本、会话缓存收益。
4. 长尾处理：模型与提供商的「其他」折叠策略与完整列表下钻。

目标：出现异常时能定位到具体是哪个 Key、哪条路由、哪个接口。

### M3：路由质量与周期对比（B 档）

1. 路由质量面板：策略分布、fallback 率、平均尝试次数、粘性命中率、失败原因分类。
2. 周期对比：环比与上一周期。
3. 颗粒度手选（小时/日/周/月）。
4. 视需要补 `upstream_model` / `endpoint` / `status_code` 的索引。

目标：把路由策略的调整效果用量化指标验证。

## 7. API 与前端改造影响

| 项 | 建议 |
| --- | --- |
| 接口形态 | 优先扩展 `/api/overview`，把筛选参数透传到各聚合查询；当查询数量明显变多时再拆出 `/api/analytics/*` |
| 筛选参数 | 对齐日志页命名（`provider_id`、`route_id`、`api_key_id`、`model`、`endpoint`、`success`），避免两套语义 |
| 分位计算 | SQLite 无内置分位函数，需用窗口函数或排序取样实现，注意大范围查询的代价 |
| 聚合上限 | 现有 `LIMIT 8` / `LIMIT 6` 与「其他」折叠要保留，但需要支持按需展开完整列表 |
| 索引 | 新维度聚合上线前先测查询计划，避免全表扫描拖慢日志页 |
| 前端 | 筛选器、指标卡、图表抽成共享组件，分析页与仪表盘按需组合，避免重复实现 |
| 空数据 | 会话、缓存、费用在无数据时都应显示占位而非 0 或空白，沿用现有 `None -> 短横线` 约定 |

## 8. 非目标与验收口径

非目标（本轮不做）：

- 不引入 `auto/*` 路由相关内容。
- 不改变现有路由策略语义与默认行为。
- 不改动 `/metrics` 的既有指标含义，新增指标另行命名。
- C 档埋点（reasoning tokens、工具占比、重试浪费、队列等待）不在 M1/M2 范围内。

验收口径：

- 分析页与日志页在同一筛选条件下，同一指标数值必须一致。
- 分位与平均值的口径在文档与 UI 提示里写清楚，避免与 Prometheus 累计值混淆。
- 未定价请求仍然显式计数，不摊入费用总额。
- 大范围（接近 366 天）查询的响应时间不应明显劣于当前。

## 9. 待确认事项

1. M1 的四项是否一次做完，还是先只做统一筛选器 + 费用趋势。
2. 性能分位用哪种实现（窗口函数 vs 应用层采样），这会决定接口的复杂度与延迟。
3. 路由质量面板是否要等 M1/M2 稳定后再做。
4. 长尾模型/提供商是默认折叠，还是提供「显示全部」的显式入口。
5. 会话维度是否需要区分「同一会话的多次重试」与「独立请求」。
