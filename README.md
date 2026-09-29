# OpenLLM Gateway

一个自托管的 LLM 网关，后端使用 Rust + Axum，管理界面使用 React + Ant Design，数据保存在 SQLite，前端静态资源会嵌入最终可执行文件。

## 功能

- 添加 OpenAI 兼容、Anthropic、Ollama 和自定义上游提供商。
- 可从 OpenAI `/models`、Anthropic `/v1/models` 或 Ollama `/api/tags` 同步提供商模型。
- 同步前可预览新增、移除和保留的模型，确认后再覆盖本地模型列表。
- 提供商列表保留最近一次连接测试结果，支持一键并发测试和可配置的定时健康检查，显示状态、延迟、检测时间和错误摘要。
- 路由会优先使用最近检测正常或尚未检测的目标，把明确异常的目标排到最后；仅当其他目标都失败时才继续尝试它们。
- 可搜索并启用、停用单个模型，也可覆盖上下文、输入和输出上限；设置值在重新同步后仍保留，并同时作用于 `/v1/models` 与路由能力聚合。
- 可为提供商设置 `vendor/` 形式的模型前缀，直接用 `vendor/model` 调用而不必先建路由。
- 按模型通配符创建路由，例如 `gpt-*`、`claude-*` 或 `*`。
- 每个路由可配置多个上游目标，支持优先级、加权随机和轮询策略。
- 上游失败时按候选顺序自动切换，并记录最终结果。
- 提供 OpenAI 兼容的 `/v1/models`、`/v1/chat/completions`、`/v1/completions`、`/v1/embeddings` 和 `/v1/responses` 接口。
- 支持流式响应转发；Anthropic 流会转换为 OpenAI SSE 格式。
- 控制台通过 SSE 实时获取新请求事件，日志与仪表盘自动刷新。
- 控制台内置模型调试台，可直接验证路由、鉴权与流式输出。
- 控制台按页面懒加载，图表、页面代码与常用依赖分包，减少非当前页面的下载量。
- 使用网关 API Key 鉴权，密钥只保存 SHA-256 摘要。
- 访问密钥列表汇总每个密钥的请求数、token、预估费用和今日限额进度，并支持每日 token、费用限额及模型通配符权限。
- 记录请求、模型、提供商、路由、token、缓存、预估费用、延迟、状态码和错误；首页汇总今日与累计费用。
- 请求日志支持按当前筛选条件导出 CSV，包含 token、缓存、费用、延迟、TPS 和错误信息。
- 提供仪表盘、提供商、路由、访问密钥和请求日志管理界面。
- SQLite 自动创建与迁移，Release 构建生成单个可执行文件。
- 设置页支持下载 SQLite 在线一致性备份，不需要停服或直接复制 WAL 文件。

## 快速开始

直接运行已构建的 Windows 单文件版本：

```powershell
.\dist\openllm.exe
```

默认监听 `http://127.0.0.1:8080`，数据保存在 `data/openllm.db`。打开浏览器访问控制台，先添加提供商，可以勾选“保存后自动同步模型”，也可以稍后在提供商列表点击同步按钮。模型前缀可选填，例如 `openai/`。

在提供商列表点击“模型管理”可以搜索模型、控制可用性，并在上游或 models.dev 的上限不准确时填写覆盖值。上限留空表示继续使用同步值。

访问密钥可设置每日 token 和每日费用上限。限额按 UTC 自然日统计，并在请求发往上游前检查；费用上限只统计已有价格数据的请求。并发请求可能在完成前短暂越过软上限。
被限额拒绝的请求会以 429、0 token、0 费用的记录写入请求日志，便于审计调用方行为。

```powershell
.\dist\openllm.exe --bind 0.0.0.0:8080 --data-dir D:\openllm-data
```

也可以设置环境变量：

```powershell
$env:OPENLLM_BIND = "0.0.0.0:8080"
$env:OPENLLM_DATA_DIR = "D:\openllm-data"
$env:OPENLLM_ADMIN_TOKEN = "replace-with-a-long-random-value"
.\dist\openllm.exe
```

如果不设置 `OPENLLM_ADMIN_TOKEN`，管理接口默认开放，适合本机使用。一旦设置该变量，启动参数仍然是命令行参数，环境变量已经生效。控制台“设置”页可保存管理令牌到浏览器。

## 调用示例

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

只要创建过任意网关访问密钥，所有 `/v1` 请求都必须携带 `Authorization: Bearer sk-openllm-...`。没有创建任何密钥时，网关允许匿名调用，便于本地快速验证。密钥可以在“访问密钥”页随时启用或停用；停用后该密钥立即失效，但不会重新打开匿名访问。

控制台“模型调试”页可以直接选择模型并发起流式对话，实时显示生成内容、延迟和 token 用量。若已创建访问密钥，在该页填写网关 API Key 即可。

## 路由逻辑

路由按以下顺序匹配：

1. 显式路由中的精确模型名优先。
2. 显式路由中的通配符模型名按长度从长到短匹配。
3. 没有显式路由时，按提供商的 `model_prefix` 自动匹配已同步模型，例如 `openai/gpt-4.1`。
4. 同一个路由内的上游按所选策略排序。
5. `priority` 使用较小优先级值优先；`weighted` 按权重随机；`round_robin` 在每个网关进程内轮询。
6. 收到连接错误或 HTTP `408`、`409`、`425`、`429`、`5xx` 时尝试下一个上游。

提供商模型名保存为上游原始名称。前缀只用于对外命名空间和自动路由，不会拼接到发送给上游的 `model` 字段。

如果多个无前缀提供商暴露同名模型，网关会拒绝自动路由；此时请设置提供商前缀或创建显式路由。

Anthropic 上游目前用于转换 `/v1/chat/completions`，其他 OpenAI 兼容端点不向 Anthropic 目标转发。

## 从源码构建

依赖 Rust、Node.js 和 npm。Windows：

```powershell
.\build.ps1
```

Linux/macOS：

```bash
chmod +x build.sh
./build.sh
```

脚本会先构建 `web/dist`，再执行 `cargo build --release`，最后复制为：

- Windows：`dist/openllm.exe`
- Linux/macOS：`dist/openllm`

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

- 上游 API Key 按原值保存在 SQLite，用于向提供商发起请求；网关访问密钥只保存 SHA-256 摘要。
- 开启 `OPENLLM_ADMIN_TOKEN` 时，SSE 事件接口使用 `admin_token` 查询参数鉴权，因为浏览器 EventSource 不支持自定义请求头。
- 生产环境应设置 `OPENLLM_ADMIN_TOKEN`，并建议放在反向代理或私网之后。
- SQLite 默认启用 WAL 和 10 秒 busy timeout，适合单实例部署。多个网关实例不应同时写入同一个数据库文件。
- `data/`、`dist/`、`target/` 和 `web/node_modules/` 默认不纳入版本控制。
