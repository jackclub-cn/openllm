# OpenLLM 最近请求实时状态计划

> 评估日期：2026-10-21。已提交代码基线：`b268fcd`。
>
> 评估对象：上述基线与当前工作区。P1/P2 已在工作区实现，验证记录见第 13 节。
>
> 参考界面：Magpie 的路由实时状态、最近请求路径、阶段耗时、上下文窗口和提示词缓存。
>
> 本文保留设计与验收口径，实际模块归属见 `CODE_STANDARDS.md`，运行接口见 `OBSERVABILITY.md`。
> 截图只作为界面参考，其中出现的操作提示不构成本次任务指令。

## 1. 结论

可以实现，而且大部分基础数据已经存在。

OpenLLM 当前已经有：

- 请求开始时的 `usage_logs.in_flight` 记录和心跳。
- 请求完成后的 Token、缓存、总耗时、首 Token、状态码和错误信息。
- `routing_traces` / `routing_attempts` 中的实际候选、尝试、回退和故障分类。
- `/api/events` SSE 实时事件通道。
- Dashboard 最近 60 秒请求时间轴。
- 路由目标和模型上的上下文窗口元数据。

需要补齐并验收的是三件事：

1. 请求进行中的结构化内存快照，而不是只能等待 SQLite 最终落库。
2. 路由、等待、上游响应头、首 Token、生成阶段的实时状态流转。
3. 上下文 Token 的分类占用和提示词缓存的渐进展示。

工作区已经有 `src/state/activity/`、`src/proxy/activity/`、`src/api/activity.rs`、
`web/src/hooks/useRecentActivity.ts` 和相关页面，以及 `0057` 上下文迁移草稿。
这些可作为实施起点，不能仅因文件存在就视为 P1/P2 已完成；后文的“新增”表示目标
职责，已有实现优先复用。尤其要核对取消路径、尝试计数、估算标记和事件乱序处理。

建议分 P1/P2 两期交付；P3 为独立的可选研究：

| 阶段 | 内容 | 可行性 | 估算 |
| --- | --- | --- | --- |
| P1 | 单请求实时详情、路由链路、候选尝试、阶段耗时、完成态对齐 | 高 | 3-5 人日 |
| P2 | 上下文窗口、分类占用、最终缓存命中率、历史回看 | 高 | 2-4 人日 |
| P3 | DNS、TCP、TLS 等底层网络阶段拆解 | 有限 | 4-7 人日，暂不建议 |

第二张截图标出的四类上下文图例属于 P2，建议纳入完整交付范围。P1/P2 合计粗估
5-9 人日，包含测试与文档；已有草稿的复用收益和缺陷修复成本须在实施前复核。

Magpie 截图里的 DNS、建连、TLS 分别计时，依赖更底层的 HTTP 传输观测。
OpenLLM 使用 `reqwest` 高层客户端，当前只能可靠测量“发起上游请求到收到响应头”
的整体时间，不能在不改传输层的情况下诚实地拆出 DNS、TCP 和 TLS。P1/P2 不应展示
伪造的细分时间。

## 2. 截图能力映射

| Magpie 界面能力 | OpenLLM 当前来源 | 结论 |
| --- | --- | --- |
| 最近请求的实时路由链路 | 基线无统一快照，工作区已有草稿 | 复核并补齐内存活动记录 |
| 请求数、回退数、客户端错误数 | `usage_logs`、`routing_traces` | 复用，增加实时聚合 |
| 最终提供商、模型、Key | `usage_logs`、`route_trace` | 已有 |
| 为什么选择当前候选 | `routing_traces.candidates_json` | 已有，扩展到实时 |
| 每个实际尝试的失败原因 | `routing_attempts` | 已有，扩展到实时 |
| 总耗时、首 Token、生成阶段 | `latency_ms`、`first_token_ms` | 部分已有 |
| 网关开销与上游等待拆分 | 无结构化阶段记录 | 新增轻量阶段埋点 |
| 上下文已用 Token 和窗口 | `prompt_tokens`、模型能力元数据 | 完成态已有，进行中需估算 |
| 系统提示/工具/工具结果/会话占比 | 基线仅总量，工作区已有分类器草稿 | 补齐协议分类和估算标记 |
| 提示词缓存命中率 | `cache_read_tokens` / 归一化 `prompt_tokens` | 上游返回 usage 后可见 |
| 回答后可见的缓存明细 | 流中或最终 usage | 到达即更新，完成态对齐 |

## 3. 产品设计

### 3.1 主入口

P1 不新增顶级导航，直接增强现有 Dashboard：

1. 保留现有“请求时间轴”。
2. 在时间轴下方增加“实时状态”，直接展示一条请求的完整链路和阶段耗时。
3. 优先展示最新发起且仍在进行中的请求；无进行中请求时展示最近一条。
4. 保留原“最近请求”历史表；实时详情的“请求日志”入口跳转用量页查看日志与路由轨迹。

这样可以复用现有 `/`、`/usage` 和请求详情抽屉，不增加新的运维页面入口。

实时模块标题栏展示：

| 指标 | 含义 |
| --- | --- |
| 当前进行中 | 内存活动记录中尚未完成的请求数 |
| 当前展示 | 进行中请求或最近一条 |
| 请求日志 | 当前请求对应的用量页详情入口 |
| 实时状态 | SSE 已连接或轮询降级 |

实时模块直接展示当前请求的详情：

- 请求 ID。
- 模型和端点。
- 路由、提供商和上游模型。
- 当前阶段：接收、排队、路由、等待上游、流式生成、完成、失败、取消。
- 已运行时间。
- 尝试次数和回退次数。
- 首 Token 时间。
- 上下文窗口占用。

多个请求并发时选择开始时间最新的进行中请求。当前请求完成时，若仍有其他请求
运行则切换到最新进行中请求；全部完成后展示按开始时间排序的最近一条。活动总数
独立于所展示的这一条。切换请求时清除上一条的分类筛选状态。

### 3.2 最近请求详情

仪表盘直接展示三个实时区块，历史请求详情复用相同组件：

1. **路由链路**
   - 调用方或访问密钥。
   - 路由策略。
   - 最终候选和 Key。
   - 每个候选的顺序和选择原因。
   - 每个实际尝试的结果和失败分类。

2. **阶段耗时**
   - 网关接收和准备。
   - 路由与容量等待。
   - 上游请求到响应头。
   - 首 Token 等待。
   - 流式生成或完整响应读取。
   - 当前总耗时。

3. **上下文与缓存**
   - 已用上下文 / 模型窗口。
   - 系统提示、工具、工具结果、会话内容占比。
   - 缓存读取、缓存写入和命中率。
   - 数据来源标记：估算、上游确认、未知。

上下文区块采用与截图一致的信息顺序：先显示总量、窗口状态和数据来源，再显示
四类占用的比例色块、图例和 Token 数，最后显示可切换分类的统计明细。明细只含
分类、条目数量和 Token 数，不返回正文。窗口
未知时只显示已用 Token 和分类比例，不绘制伪造的总容量分母。

这里表示“本次请求提交的输入上下文”，不代表 Agent 未发送的完整会话或供应商内部
隐藏上下文。缓存命中也占模型窗口，不能从上下文用量中扣除。

进行中显示实时数据，完成后保留最终活动快照；通过“请求日志”入口查看最终
`usage_logs` 和持久化路由轨迹，避免估算值覆盖真实值。

### 3.3 降级行为

- SSE 正常时，活动快照由事件驱动更新。
- SSE 断开时，前端显示“轮询”，每 3-5 秒拉取一次活动快照。
- SSE 重连后立即拉取全量快照并继续增量，不依赖断线期间的事件补偿。
- 单个请求丢失更新时，下一次快照会按 `revision` 覆盖旧状态。
- 进程重启时按 `instance_id` 清空旧内存快照，避免将旧进程请求永久显示为进行中。
- 活动存储不可用时，历史请求、路由轨迹和现有 Dashboard 不受影响。

## 4. 数据设计

### 4.1 内存活动记录

新增进程内 `RequestActivityStore`，保存最近请求的可展示快照。

建议记录字段：

| 字段 | 说明 |
| --- | --- |
| `request_id` | 网关请求 ID |
| `revision` | 单请求单调递增版本，用于忽略过期事件 |
| `started_at` / `updated_at` / `finished_at` | UTC 时间 |
| `phase` | 当前生命周期阶段 |
| `outcome` | `running`、`success`、`failed`、`cancelled` |
| `endpoint` | OpenAI / Anthropic 端点 |
| `streamed` | 是否流式 |
| `api_key_id` / `api_key_name` | 调用方 |
| `route_id` / `route_name` / `strategy` | 路由信息 |
| `requested_model` | 客户端请求模型 |
| `selected_target` | 已选中的提供商、上游 Key、上游模型 |
| `context` | 上下文窗口和分类估算 |
| `timings` | 阶段耗时 |
| `candidates` | 有上限的候选摘要 |
| `attempts` | 有上限的尝试摘要 |
| `fallback_count` | 回退次数 |
| `first_token_ms` | 首 Token 时间 |
| `cache` | 缓存读取、写入和命中率 |
| `error_code` | 稳定错误码，不保存完整错误正文 |

不在活动记录中保存：

- 提示词正文。
- 响应正文。
- 工具参数和工具结果正文。
- 原始请求头、响应头和 Secret。
- 未经脱敏的上游错误全文。

### 4.2 生命周期

| 阶段 | 进入条件 | 主要事件 |
| --- | --- | --- |
| `admitted` | 请求进入网关处理 | 创建活动记录 |
| `preparing` | 解析请求、认证、策略检查 | 模型、端点、访问密钥 |
| `routing` | 开始解析候选 | 路由和策略 |
| `waiting` | 因容量、sink 或全忙等待 | 等待原因和预计重试时间 |
| `attempting` | 开始一个上游尝试 | 候选顺序、提供商、Key、模型 |
| `headers_received` | 收到上游响应头 | 状态码、响应头耗时 |
| `streaming` | 收到首个有效输出片段 | 首 Token、生成阶段开始 |
| `completed` | 响应成功结束 | 最终 Token、缓存、总耗时 |
| `failed` | 返回失败响应 | 稳定错误码和失败分类 |
| `cancelled` | 客户端断开、取消或请求兜底被中断 | 499 和释放原因 |

所有阶段转换必须幂等，相同更新可直接忽略；终态不得回退为运行中。`attempt_count`
统计实际尝试，不能在候选列表刷新时累加；回退计数沿用持久化路由轨迹的口径，
不能用被截断的 `attempts.len()` 替代。

返回流式 `Response` 不代表请求完成，状态必须跟随响应体消费生命周期。只有客户端
断开沿用 499；上游超时、网关超时和流异常分别保留现有状态码与失败分类。

### 4.3 阶段耗时语义

| 指标 | 起点 | 终点 | 说明 |
| --- | --- | --- | --- |
| `admission_wait_ms` | 进入 admission | 获得全局容量 | 仅在可以取得请求 ID 时统计 |
| `prepare_ms` | 请求处理开始 | 首轮候选解析完成 | 包含解析、认证、策略和初次路由解析 |
| `route_wait_ms` | 候选解析完成或重试间隙 | 真正发起上游请求 | 累计排序、容量和全忙等待，不含传输时间 |
| `upstream_headers_ms` | 本次发起上游请求 | 本次收到响应头 | 每次尝试分别记录，包含网络和上游等待 |
| `time_to_first_token_ms` | 请求开始 | 首个有效输出片段 | 流式使用真实首片段 |
| `generation_ms` | 首个输出片段 | 流结束 | 非流式不展示该阶段 |
| `total_ms` | 请求开始 | 响应结束或失败 | 前端按 `started_at` 本地递增 |

`time_to_first_token_ms` 和 `total_ms` 是里程碑，不与其他阶段相加。可加总的耗时条
必须使用不重叠区间，并单独计入失败尝试、重试间隙和响应头至首输出之间的等待。
服务端使用单调时钟计时；前端以快照的 `total_ms` 加本地单调时钟增量显示，避免
浏览器与服务器时钟偏差。keepalive、角色空帧和 usage 事件不算有效输出；推理和
工具调用增量沿用现有解析器的首 Token 口径。

`upstream_headers_ms` 不能继续拆成 DNS、TCP、TLS 和供应商首字节。P3 若要做，
必须引入自定义连接器或传输层观测，不能从现有总耗时中猜测。

对于非流式请求，现有系统把完整响应时间写入 `first_token_ms`。新界面必须明确显示
“首 Token 不适用”，只在流式请求上展示真实首 Token。

### 4.4 上下文分类

新增协议感知的 `ContextBreakdown`，只持久化 Token 数，不持久化正文。

| 分类 | OpenAI Chat | Responses | Anthropic Messages |
| --- | --- | --- | --- |
| `system` | `system` / `developer` 消息 | `instructions` | 顶层 `system` |
| `tools` | `tools`、`functions` 定义 | `tools` 定义 | `tools` 定义 |
| `tool_results` | `role=tool` 消息 | `function_call_output` | `tool_result` 内容块 |
| `conversation` | 其他 user/assistant 消息 | 其他 input 项 | 其他 message 内容 |
| `media` | 图片、音频等非文本输入 | 其他非文本 input | 图片、文档等非文本块 |

P2 复用现有 `count_text_chars` 和字符数除以 4 的估算器，不新增 Tokenizer
依赖。分类估算在请求开始时计算，上游返回真实 `prompt_tokens` 后等比校准：

- 进行中显示“估算”。
- 返回真实用量后，总量标记“上游确认”，分类仍标记“按估算比例校准”。
- 上游没有返回真实 Prompt Token 时持续标记为估算。
- `input_estimated` 与分类 `estimated` 分开保存；校准不能把分类标成精确分词结果。
- 图片、音频和未知字段记录存在性与数量，不把 URL、Base64 长度当作媒体 Token。
  无可靠媒体用量时显示“多模态用量未单独计量”，不能将真实总量全部强摊入四类文本。
- 需要闭合分类总数时增加 `unattributed`（未归因），旧历史记录无分类时显示暂无数据。

模型窗口分母使用最终目标的完整上下文容量。能力经过保守输入上限归一化时，优先
读取 `total_context_tokens`，并将有效 `input_limit` 单独显示；只有确定仍为原始
总窗口时才使用 `context_limit`。尚未选中目标时，路由 barrel 的保守值须标记为
“路由参考容量”，选中后更新。没有元数据时显示“模型窗口未知”，不能猜测。

### 4.5 缓存语义

缓存取决于供应商返回的 usage，不能从请求里的 `cache_control` 推断已经命中。
Anthropic 可在流式 `message_start.message.usage` 返回输入与缓存数据，不必一律
等到流结束；其他协议沿用当前 usage 解析器支持的事件和字段。

- 尚无字段时显示“等待供应商返回”，已返回时立即更新并记录来源。
- 以现有解析器归一化后的总输入 `prompt_tokens` 为分母，计算缓存读取占比；
  Anthropic 总输入包含未缓存输入、缓存读取和缓存创建，不直接使用原始 `input_tokens`。
- 缓存字段为零时区分“真实零命中”和“供应商未返回”，不要都显示成 0%。
- 分母为零或尚未确认时命中率未知，保留缓存读取/写入读数。
- 历史详情复用已有数值，并持久化字段是否返回的标记；旧记录的零值无法证明真实零命中。

### 4.6 容量和保留

建议默认值：

| 项目 | 默认值 |
| --- | --- |
| 活动请求 | 全部保留到完成 |
| 已完成最近记录 | 256 条或 5 分钟，以先到者为准 |
| 单请求候选摘要 | 最多 32 个 |
| 单请求尝试摘要 | 最多 32 次 |
| API 单次返回 | 最多 200 条 |
| SSE 广播缓冲 | 1024 条 |

候选和尝试截断时返回实际总数与 `truncated` 标记；计数继续准确。活动记录随既有
准入约束保留，完成记录的容量上限不等于整个存储有硬上限；高并发时还需验证内存
开销，并对名称、原因和单事件字节数设限，不能只按数组长度判断事件大小。

提供环境变量 `OPENLLM_LIVE_REQUEST_TRACE=0` 关闭活动记录和 SSE 更新。关闭后原有
历史请求和路由轨迹保持不变。

## 5. 后端结构

### 5.1 新模块

按当前代码组织规范拆分：

| 路径 | 职责 |
| --- | --- |
| `src/state/activity.rs` | 活动记录类型、固定容量存储、快照读取 |
| `src/proxy/activity.rs` | OpenAI / Anthropic 共用生命周期桥接和阶段更新 |
| `src/proxy/usage/context.rs` | 协议感知的上下文分类和 Token 估算 |
| `src/api/activity.rs` | 最近活动查询和单请求活动查询 |

不要继续把实时状态追加到 `src/proxy/route_trace.rs`。该文件的职责是持久化路由轨迹；
把事件广播、快照序列化和内存生命周期也塞进去，会削弱这一单一职责，也会把完成态
存储和进行态观测耦合成一个持续膨胀的入口。

`AppState` 增加：

- `activity: Arc<RequestActivityStore>`
- `activity_events: broadcast::Sender<ActivityEvent>`

活动存储使用短临界区的同步互斥锁，不持有锁执行 `await`，不在热路径写 SQLite。

### 5.2 请求接入点

OpenAI 和 Anthropic 两条入口都需要接入，不能只改其中一条：

- `src/proxy/openai_api.rs`
- `src/proxy/anthropic_api/inbound.rs`
- `src/proxy/anthropic_api/attempts.rs`
- `src/proxy/upstream/forward.rs`
- `src/proxy/usage/parser.rs`
- `src/proxy/translation/common.rs`

建议在各入口创建轻量 `ActivityGuard`：

1. 入口开始创建活动记录。
2. 解析完成后填充模型、端点、流式标记和上下文分类。
3. 路由完成后填充候选和策略。
4. 每次尝试开始和结束时记录状态、状态码、失败域和耗时。
5. 收到响应头后进入 `headers_received`。
6. 流解析器首次看到有效内容时进入 `streaming`。
7. 最终 `log_usage` 时用真实 Token、缓存和状态完成记录。
8. Guard 被丢弃且未完成时标记 `cancelled`，与现有 499 取消路径一致。

活动状态更新失败不能影响请求主路径。所有更新只记录警告，不返回错误给调用方。

### 5.3 与现有路由轨迹的关系

`routing_traces` / `routing_attempts` 继续作为完成态事实来源：

- 请求完成后仍异步持久化，保持现有行为和保留策略。
- 活动记录只提供进行中和最近窗口视图。
- 最终详情优先读取持久化轨迹。
- 持久化尚未完成时，详情可暂时显示活动快照。
- 不使用活动内存替代数据库历史。

### 5.4 数据库变更

上下文分类需要可回看时，增加一次轻量迁移。工作区已有
`0057_usage_context_breakdown.sql` 草稿，实施时先核对迁移登记与最高编号，保留已有
迁移顺序，不重复添加相同列。

建议字段：

```sql
ALTER TABLE usage_logs ADD COLUMN context_breakdown_json TEXT;
-- NULL: 旧记录未知；0: 本次未返回缓存字段；1: 返回过缓存字段（包括真实零）。
ALTER TABLE usage_logs ADD COLUMN cache_usage_reported INTEGER;
```

可选增加：

```sql
ALTER TABLE routing_attempts ADD COLUMN first_token_ms INTEGER;
```

`context_breakdown_json` 只保存窗口、分类 Token 数、各层估算标记和来源，不保存
任何提示词内容。若 P1 不要求历史上下文回看，可以先不迁移，等 P2 一并加入。

## 6. API 和事件

### 6.1 最近活动

```text
GET /api/activity/recent?limit=50
```

返回：

- `generated_at`
- `active_count`
- `instance_id` / `sequence`（进程标识和全局事件水位）
- `items`

第一版每项与详情使用同一个受限 `RequestActivitySnapshot` 结构，便于直接合并 SSE。
`active_count` 是全存储计数，不是当前列表数量；列表上限不保证展示全部活动请求，
需显式显示数量差异，后续再按实际需求增加活动筛选或分页。

### 6.2 单请求活动

```text
GET /api/activity/{request_id}
```

- 活动记录存在时返回实时快照。
- 已从内存淘汰时返回 404，并提示改查 `/api/usage/{request_id}`。
- 不保存或返回请求正文。

### 6.3 SSE 扩展

继续复用 `/api/events`，不新增独立 EventSource。

保留现有：

```text
event: usage
```

新增：

```text
event: activity
data: {"kind":"updated","instance_id":"process-uuid","sequence":42,"active_count":1,"snapshot":{...}}
```

建议 `kind`：

- `started`
- `updated`
- `attempt_started`
- `attempt_finished`
- `first_token`
- `completed`
- `failed`
- `cancelled`

事件携带完整快照，前端按 `instance_id + request_id + revision` 覆盖。最多保留
32 次尝试的摘要，实际尝试数不受展示上限影响；字符串和总字节数也须设限。
仅阶段、尝试、首 Token、usage 可用性变化和最终结果触发事件，不按每个 chunk 发送。

活动广播发生 `Lagged` 时发送 `event: activity_resync`，前端收到后重新拉取
`/api/activity/recent`。先建立事件订阅，再拉全量快照；加载期间缓冲事件，快照返回
后仅回放同进程且 `sequence` 大于快照水位的事件，避免快照覆盖刚收到的新状态。
事件序号的分配和发布顺序必须一致；无事件重放保证，发现序号缺口时重新拉快照。

两个活动接口与 SSE 都沿用现有管理鉴权。关闭功能时列表应返回 `enabled: false`，
前端显示功能已关闭，不能把关闭误报为没有请求。

## 7. 前端结构

| 路径 | 职责 |
| --- | --- |
| `web/src/api/activity.ts` | 活动快照类型和查询接口 |
| `web/src/realtime.tsx` | 增加 `activity` 和 `activity_resync` 事件订阅 |
| `web/src/hooks/useRecentActivity.ts` | 快照加载、事件合并、断线轮询 |
| `web/src/hooks/useRequestActivity.ts` | 单请求状态与淘汰后的历史回退 |
| `web/src/pages/dashboard/DashboardRequestActivity.tsx` | 单请求内联详情、活动总数和历史入口 |
| `web/src/pages/dashboard/requestActivity.ts` | 优先最新进行中请求，无进行中时选择最近一条 |
| `web/src/pages/dashboard/ActivityDetailPanel.tsx` | 客户端、路由、目标链路和阶段耗时；增长时按职责拆分 |
| `web/src/components/ContextBreakdownPanel.tsx` | 实时与历史共用的分类、窗口和来源展示 |
| `web/src/styles/activity.css` | 实时请求领域样式 |

`Dashboard.tsx` 只负责页面组装，必须在接近 TypeScript 建议行数前把新增逻辑放入
子组件。`UsageDetailDrawer.tsx` 同样只保留抽屉编排，实时链路、阶段耗时和上下文
区块各自独立，不能继续堆入抽屉入口。

前端行为：

- 初次进入拉取 `/api/activity/recent`。
- 连接 SSE 后按事件合并快照。
- 运行时长基于快照耗时和本地单调时钟递增，不依赖服务端每秒推送。
- 仪表盘完成后展示最终活动快照，日志入口连接现有用量页的持久化详情与路由轨迹。
- 用量页详情在持久化尚未完成时保留已有信息并有限重试；已淘汰活动回退到 usage 详情。
- 同进程同请求 `revision` 不大于当前值的重复和旧事件直接丢弃。
- 页面不可见时暂停无意义的动画，但保留事件合并。
- 暂停自动刷新时，同时暂停活动订阅、轮询和详情的本地耗时递增。

## 8. 实施步骤

### 阶段 P1：实时请求状态

1. 新增 `RequestActivityStore` 和快照类型。
2. 在 `AppState` 中接入活动存储和广播。
3. 在 OpenAI 和 Anthropic 入口创建活动记录。
4. 在候选排序、尝试开始/结束、响应头、首 Token、最终完成处更新阶段。
5. 扩展 `/api/events` 的 `activity` 事件。
6. 新增 `/api/activity/recent` 和 `/api/activity/{request_id}`。
7. 在 `web/src/api/` 增加活动类型和客户端。
8. 扩展 `realtime.tsx` 订阅活动事件。
9. 新增 Dashboard 单请求实时详情，自动选择最新进行中请求或最近一条。
10. 仪表盘直接展示链路和耗时，用量页详情承接完成后的日志与路由轨迹。
11. 增加 SSE 断线轮询和 `activity_resync`。
12. 更新 `docs/CODE_STANDARDS.md` 的模块地图和 `docs/OBSERVABILITY.md` 的实时接口说明。

P1 完成后应能实时回答：

- 现在有哪些请求正在运行？
- 每个请求现在卡在哪个阶段？
- 它选择了哪个路由、提供商、Key 和模型？
- 是否发生过回退，为什么回退？
- 已经等待了多久，首 Token 是否出现？

### 阶段 P2：上下文和缓存

1. 新增协议感知的上下文分类器。
2. 在请求开始时计算分类估算。
3. 在详情中展示上下文窗口和分类占用。
4. 完成后用真实 `prompt_tokens` 校准分类。
5. 展示已返回的缓存读取、写入和命中率，最终 usage 到达后对齐。
6. 补齐上下文分类、缓存字段来源的迁移和历史详情读取。
7. 为 OpenAI Chat、Responses 和 Anthropic Messages 增加分类回归测试。

P2 完成后应能回答：

- 本次请求占用了多少上下文窗口？
- 系统提示、工具定义、工具结果和会话内容各占多少？
- 这是精确值还是估算值？
- 提示词缓存是否命中，节省了多少输入 Token？

### 阶段 P3：底层网络阶段

只有在运维明确需要定位 DNS、TCP 或 TLS 故障时才评估。

可选方案：

- 自定义 `reqwest` 连接器。
- 平台相关连接观测。
- 仅在显式开启时采样，不默认影响热路径。

P3 必须证明不会破坏连接池复用、TLS 校验、代理支持、超时和现有 SSE 行为，否则不做。

## 9. 测试计划

### 9.1 Rust 单元测试

新增活动存储测试：

- 活跃记录不会被最近记录淘汰。
- 已完成记录按容量和 TTL 淘汰。
- `revision` 只增不减。
- 重复完成事件幂等。
- 取消和超时记录最终进入终态。
- 候选和尝试数量被正确截断。
- 超过 32 次尝试后总计数仍准确，重复路由刷新不增加尝试数。

新增上下文分类测试：

- OpenAI Chat 的 system、tools、tool result 和普通消息。
- Responses 的 instructions、function call output 和普通 input。
- Anthropic 的顶层 system、tools 和 tool_result 内容块。
- 图片、音频和未知字段进入非文本分类。
- 真实 `prompt_tokens` 校准后的分类总和一致。
- 校准后总量可为真实值，分类始终保留估算标记；多模态未归因部分单独显示。
- 没有窗口元数据时返回未知，不返回伪造容量。

### 9.2 代理集成测试

- 非流式请求从开始到完成的活动状态序列。
- 流式请求在首 Token 前处于 `attempting` 或 `headers_received`，首 Token 后进入
  `streaming`。
- 第一个候选失败、第二个成功时，活动快照包含两次尝试和一次回退。
- 客户端取消后活动记录为 `cancelled`，最终 usage 仍为 499。
- `log_usage` 失败或活动存储写入失败不影响上游响应。
- 同时存在 SSE keepalive、全忙等待和活动更新时不发生死锁。
- Anthropic `message_start` 已返回缓存时立即可见；缓存字段缺失与真实零分开。

### 9.3 API 测试

- `/api/activity/recent` 的数量限制和排序。
- `/api/activity/{request_id}` 命中活动记录、已完成记录和淘汰记录。
- `/api/events` 同时收到 `usage` 和 `activity` 事件。
- 活动广播 lag 后收到 `activity_resync`。
- 管理鉴权开启时两个活动接口都要求有效 Token。
- 全量快照与 SSE 并发、乱序、重复、进程重启后状态正确，开关关闭有明确状态。

### 9.4 前端验证

- `npm run build`。
- SSE 正常、断线、重连和轮询降级。
- 1 个活动请求和多个并发请求下直接展示详情，选择规则稳定。
- 较新的已完成请求不抢占较早的进行中请求；全部完成后显示最近一条。
- 详情内容不因事件乱序回退，切换请求时清除分类筛选，暂停后耗时停止增长。
- 移动端和窄屏下路由链路、耗时条和 token 卡片不重叠。

### 9.5 验收场景

1. 发起一个至少 10 秒的流式请求。
2. Dashboard 在 1 秒内出现该请求，无需等待现有 5 秒轮询。
3. 请求显示当前模型、路由和上游目标。
4. 首 Token 到达后阶段切换为 `streaming`，总耗时继续增长。
5. 完成后活动数减一；仍有请求运行时切换过去，全部完成后保留最近一条。
6. 无需点击即可看到 Token 和缓存，“请求日志”入口可查看持久化日志与路由轨迹。
7. 人为断开 SSE，页面显示轮询并在重连后恢复实时。
8. 强制一个候选返回 5xx，另一个成功，详情完整展示回退原因。
9. 客户端中途断开，记录显示取消，不残留永久活动状态。
10. 新活动接口、事件和新增数据库字段不记录正文；现有日志设置与行为保持不变。
11. P2 的四类占用色块和图例可见，进行中显示估算，上游用量返回后总数校准，
    分类仍保留估算标记，未知窗口不显示虚假比例。

## 10. 风险和约束

| 风险 | 处理 |
| --- | --- |
| 高并发事件量过大 | 只在阶段变化时广播，前端合并快照，广播 lag 时全量重同步 |
| 内存增长 | 完成容量与 TTL、Guard 清理、既有准入约束及单记录字节限制 |
| 活动数据与最终数据不一致 | 最终以 `usage_logs` 和 `routing_traces` 为准 |
| 请求取消后状态残留 | 复用 Guard Drop 和现有 499 路径 |
| Token 分类不准确 | 明确标注估算，完成后用真实 Prompt Token 校准 |
| 缓存数据尚未返回 | 显示“等待供应商”，不伪造 0% |
| DNS/TCP/TLS 无法拆分 | P1/P2 只展示可信的聚合阶段，P3 单独评估 |
| 活动更新拖慢请求 | 内存短锁、无同步 I/O、错误只告警 |
| 单实例边界被破坏 | 活动数据只存在当前进程，不引入 Redis 或跨实例共享 |
| 新模块超过规模限制 | 从开始按 store、API、组件和样式拆分 |

## 11. 明确不做

- 不保存提示词、响应、工具参数或工具结果正文用于实时分析。
- 不把活动内存当成历史日志。
- 不为了实时页面增加每请求同步数据库写。
- 不引入 Redis、WebSocket、Node.js 运行时或新的前端状态库。
- 不展示无法证明的供应商内部排队时间。
- 不默认开启底层网络采样。
- 不改变现有路由策略、冷却、重试和 SSE 响应语义。

## 12. 推荐交付范围

按 P1、P2 顺序交付，覆盖两张截图的核心需求。P1 作为首个可验收里程碑，预留
上下文字段；P2 实现第二张截图重点标出的分类占用和缓存展示。

P1 交付标准：

- Dashboard 有单请求实时详情和活动总数，原最近请求历史表继续保留。
- 每个请求能看到路由链路、当前阶段、尝试和回退。
- 流式请求能实时看到首 Token 和持续增长的总耗时。
- 请求完成后自动对齐现有历史详情和路由轨迹。
- SSE 断开后自动降级到轮询。
- 活动接口不返回请求正文。
- `node scripts/check-code-structure.mjs`、`cargo fmt --check`、
  `cargo test --locked`、`cargo clippy --locked --all-targets -- -D warnings`
  和 `web/` 下的 `npm run build` 全部通过。

P2 交付标准是四类图例与数量、可信的窗口分母、估算来源、可提前显示的缓存数据，
以及完成后的历史回看；同样完成相关后端与前端检查。P3 独立评估，不影响 P1/P2 验收。

## 13. 实施与验证记录

2026-10-21：P1/P2 已接入 OpenAI 与 Anthropic 请求入口、原 `/api/events` 和运行
概览。活动存储按快照、尝试、计时和 usage 合并拆分；历史请求使用迁移 0057 保存
分类统计和缓存字段来源。完成记录保留 256 条/5 分钟，尝试摘要保留最近 32 次，
实际总数独立记录。流式容量等待恢复时沿用原轨迹和尝试计数，避免历史漏记。

| 验收项 | 对应证据 |
| --- | --- |
| 生命周期、取消、上游异常、回退与非流式首 Token | `tests/unit/proxy/activity.rs`，真实本地 HTTP 上游 |
| 流中缓存提前可见及完成后持久化 | 原生 Anthropic `message_start` 到 `message_stop` 集成用例 |
| SSE 鉴权、活动/用量事件及 lag 重同步 | `tests/unit/api/activity.rs`，经实际 Router 与响应流验证 |
| 保留、禁用、幂等、顺序和摘要上限 | `tests/unit/state/activity.rs`，含并发广播与 UTF-8 名称 |
| 三协议分类、校准与多模态未归因 | `tests/unit/proxy/usage_context.rs` |
| 乱序、缺口、快照竞态、进程重启与单调计时 | `node --test web/tests/activityState.test.mjs` |
| 桌面/390px 窄屏、分类筛选、历史回看、SSE 错误后轮询与重连 | 本地 Playwright 验证及截图 |
| 100 个并发请求后的列表与浏览器运行错误 | Playwright + 本地模拟上游；无页面异常，列表保留最近 50 条 |

验证输出与截图位于本地忽略目录 `.pi/activity-rehearsal/`，包括
`rust-tests.log`、`clippy.log`、`web-build.log`、`browser-result.json` 和
`desktop-live.png` / `mobile-context.png`。该目录的模拟上游与数据库仅用于本地验收。
构建保留现有依赖分块大于 500kB 的 Vite 提示；没有通过放宽阈值或禁用警告处理。
P3 的底层网络拆解保持未实施，分类 Token 仍明确标记为估算。

2026-10-21 界面调整：实时区域由列表及抽屉改为“实时状态”内联详情，优先最新
进行中请求，空闲展示最近一条。历史“最近请求”表继续保留，日志入口统一进入
用量页；实时选择规则另以 `web/tests/dashboardActivity.test.mjs` 验证。

本次验证：前端状态测试 11 项通过，结构检查、`cargo fmt --check` 和 `web/`
构建通过。桌面 Playwright 的 9 组场景覆盖内联详情、暂停计时、进行中优先、并发
选择与分类重置、取消后切换、空闲回看、历史日志跳转、断线轮询与重连，以及
关闭/空记录/刷新错误。验证结果在 `.pi/activity-rehearsal/inline-browser-result.json`，
桌面截图在 `inline-desktop-live.png`；没有页面运行异常。按用户要求，本次后续验收
以桌面布局为准。构建仍保留原有依赖分块体积提示；未改动 Rust 代码。
