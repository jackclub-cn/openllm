# 代码组织与维护规范

本文约束新增代码和后续维护。仓库根目录的 `AGENTS.md` 是开发代理的入口；
`scripts/check-code-structure.mjs` 在本地和 CI 检查文件规模。

## 1. 文件规模

行数包含空行和注释，统一按物理行计算。

| 文件类型 | 建议目标 | 硬上限 | 超出时的处理 |
| --- | ---: | ---: | --- |
| Rust 业务代码 | 500 行 | 1000 行 | 按解析、查询、决策、转发、持久化等职责拆分 |
| Rust 测试 | 500 行 | 1000 行 | 按行为领域分测试模块，共享必要夹具 |
| TypeScript / TSX / JavaScript | 300 行 | 600 行 | 分离类型、请求、状态、展示和领域组件 |
| CSS | 300 行 | 600 行 | 按页面或组件领域拆分，保留样式加载顺序 |
| 生成文件、依赖、构建产物、锁文件 | 不适用 | 不适用 | 不纳入源码行数检查 |

建议目标是审查信号，硬上限由脚本强制执行。达到建议目标时先检查职责；
预计超过硬上限时，必须在当前任务中拆分，不把整理留给用户提醒。

存量超限文件记录在 `scripts/code-size-baseline.json`，每项注明原因和拆分方向。
其当前上限只能降低；文件进入正常范围后删除对应条目。不得随意增设白名单、
提高上限，或用压行、删除有用注释、拼接文件绕过检查。

## 2. 模块职责

| 目录 | 职责 | 约束 |
| --- | --- | --- |
| `src/api/` | 管理接口和查询编排 | 输入校验、数据库查询与运行时代理逻辑分开 |
| `src/api/helpers/` | 管理接口共享能力 | 按 models、credentials、views、validation 等明确归属组织 |
| `src/api/usage/` | 请求日志、分析和维护 | 日志、时间窗口、overview、维度、路由质量分别维护 |
| `src/api/lifecycle/` | 配置导入导出、备份、升级检查 | 导入器再按 settings、providers、routes、pools、webhooks 分工 |
| `src/api/metrics/` | Prometheus 快照和文本渲染 | 一次采集数据，各指标领域分别输出；指标名和统计口径保持稳定 |
| `src/proxy/routing/` | 路由决策 | 解析、诊断、指标、策略、粘性、并发、熔断、凭据、发送分别维护 |
| `src/proxy/upstream/` | 上游传输与响应生命周期 | HTTP 转发、流保护、错误处理、SSE、准入响应分开 |
| `src/proxy/translation/<protocol>/` | 协议转换 | 请求、响应、流状态和流事件处理分开 |
| `src/proxy/usage/` | 用量采集 | 流解析、估算、预览、能力回执、生命周期、落库分开 |
| `src/models/usage/` | 用量领域模型 | 日志、路由轨迹、分析、计费和持久化性能摘要分别维护；`usage.rs` 只保留导出 |
| `src/proxy/usage/performance.rs` | 最终性能摘要 | 完成、失败与取消后收集阶段耗时和已保留失败尝试成本，不记录正文 |
| `src/proxy/activity/` | 请求活动观测 | 生命周期 Guard 和上游 usage/首输出观测；不改变转发、路由或记账口径 |
| `src/state/activity/` | 实时活动内存状态 | 快照、尝试、阶段计时和 usage 合并分工；序号分配与广播在同一短锁内完成 |
| `src/api/activity.rs` | 最近活动管理查询 | 沿用管理鉴权，不返回请求或响应正文 |
| `src/state/` | 运行状态 | AppState 初始化、限制参数、内存、准入预算、事件分开 |
| `src/startup/` | 启动和数据恢复 | 租约、迁移、备份、校验、恢复分开 |
| `src/release_feed.rs` | 稳定发布源读取与缓存 | 合并并发检查，成功缓存一小时、失败缓存一分钟；不进入代理热路径 |
| `src/quota/` | 额度快照 | 存储、上游归一化、响应头、有效状态和调度信号分开 |
| `web/src/api/` | 前端接口客户端与领域类型 | `web/src/api.ts` 只保留稳定导出 |
| `web/src/pages/<domain>/` | 页面领域组件 | 页面负责组装，表单、表格、图表及状态按职责拆分 |
| `web/src/pages/settings/` | 请求策略、日志隐私、访问安全 | 设置入口管理标签地址；独立区域加载、保存与撤销，统一拦截未保存修改的导航；运行状态和运维动作由诊断、运维页面承接 |
| `web/src/pages/diagnostics/DiagnosticRuntimeSettings.tsx`、`DiagnosticCooldowns.tsx` | 生效运行参数与故障恢复操作 | 参数复用诊断快照；冷却集中提供单项与全部清除，保留半开探测状态 |
| `web/src/pages/maintenance/` | 数据库维护与备份恢复展示 | 数据库统计、下载备份与整理集中在运维；沿用现有维护、备份和校验接口 |
| `web/src/pages/AdminLogin.tsx` | 管理令牌登录入口 | 仅在启用 `OPENLLM_ADMIN_TOKEN` 时验证并进入控制台；令牌只保存在当前浏览器 |
| `web/src/styles/` | 领域样式 | `styles.css` 只按原有层叠顺序导入 |
| `web/src/hooks/activityState.ts` | 活动快照和事件合并 | 按进程、全局水位和请求版本对齐；缺口触发重同步 |
| `web/src/pages/dashboard/DashboardRequestActivity.tsx`、`requestActivity.ts` | 仪表盘单请求实时详情及选择 | 优先最新进行中请求，空闲显示最近一条；复用 `ActivityDetailPanel`，历史请求仍由用量页承接 |
| `web/src/pages/dashboard/ActivityDetailPanel.tsx`、`ActivityRequestSummary.tsx` | 实时请求详情编排与摘要 | 展示已观测链路、耗时和上下文；摘要只复用活动快照字段，不推测未上报数据；`styles/activity-detail.css` 只负责详情区域布局 |
| `web/src/components/RequestTimingPanel.tsx`、`requestTimings.ts` | 实时与历史耗时展示及归因 | 仅使用已测量阶段；TTFT 不重复计入阶段；旧记录显示未知 |
| `web/src/components/VersionStatus.tsx`、`GithubLink.tsx` | 全局版本提示与项目入口 | 复用管理升级接口，发布失败不误报已是最新 |
| `tests/unit/<domain>/` | 领域回归测试 | 入口只保存共享夹具和模块声明 |

优先使用 `<domain>.rs` 加 `<domain>/` 子模块的既有组织方式。入口文件负责
声明与导出；不要把代码继续堆到入口或泛用 `helpers` 文件。
不要按 `part1`、`part2`、行号区间或文件长度命名业务模块。

Rust 的 `use super::*` 可用于同一领域的父子模块及测试夹具导入，沿用仓库惯例。
跨领域依赖应明确指向所属模块。只有内部协作需要时才提升为 `pub(super)` 或
`pub(in crate::<domain>)`，不要为了移动代码把所有实现公开成 `pub`。

前端领域模块的类型依赖使用 `import type`，避免为了复用类型引入运行时循环。
共享抽象必须服务实际复用；单页面的表单和状态先留在页面领域目录。

## 3. 变更流程

1. 开始时检查工作区和相关文件规模，读清模块归属。
2. 在所属模块实现。接近文件上限时，先按职责拆开，再继续增加功能。
3. 结构调整保持外部 API、SQL、统计口径、重试策略和默认行为不变。
4. 测试跟随职责移动，保留断言、平台条件和共享夹具；不得用忽略测试换取通过。
5. 更新本文的模块地图，运行结构检查和相应测试，再汇报结果。

重构和功能修复可以保留在同一工作区，但须区分其目的，保留已有用户修改。
结构整理本身不要求升版本、提交或发布。

## 4. 验证命令

| 场景 | 必须执行 |
| --- | --- |
| 所有代码变更 | `node scripts/check-code-structure.mjs` |
| 规范检查脚本变更 | `node --test scripts/check-code-structure.test.mjs` |
| Rust 变更 | `cargo fmt --check`、`cargo test --locked` |
| 共享模块或测试移动 | 完整 Rust 测试，并比对测试数量 |
| 前端变更 | 在 `web/` 执行 `npm run build` |
| 指标声明或监控文件变更 | `bash scripts/check-monitoring.sh` |
| 静态检查 | `cargo clippy --locked --all-targets -- -D warnings`，不新增无说明的告警豁免 |

代码规模脚本无需安装 npm 依赖，在 Node.js 22 及以上运行。CI 的质量检查独立执行，
主分支推送、PR 和标签构建都会执行检查，三平台构建执行严格 Clippy 检查。
发布任务同时等待质量检查、监控产物校验和三平台构建通过。

控制台当前以桌面操作为主。前端发布验收固定检查桌面布局、空状态和实际交互，
窄屏布局不作为发布门禁，除非任务明确要求适配窄屏。
