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

## 核心能力

- 聚合 OpenAI 兼容、Anthropic、Ollama 和自定义上游，支持提供商多密钥、优先级、加权随机、轮询和故障切换。
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
- 会话 ID 用于请求详情追溯和同会话亲和路由；仪表盘聚焦请求、Token、缓存、成功率和总用时等核心指标。
- 支持额度接口的提供商可直接查看额度窗口和价格，多密钥时可切换具体 Key 查询；例如 Command Code 的 5 小时、周、月额度，OpenCode Go 的滚动、周、月窗口，以及 DeepSeek 余额和模型价格。

## 使用说明

预编译版本请从 [GitHub Releases](https://github.com/jackclub-cn/openllm/releases) 下载；从源码运行请先完成构建步骤。

### 配置流程

1. 在“提供商”页面先选择模板卡片，再添加上游并填写 API Key，完成后可立即测试连接。
2. 同步模型或手动维护模型列表，在“模型管理”中检查上下文、输出上限、支持接口和价格。
3. 创建路由，或为提供商填写 `vendor/` 前缀后直接通过 `vendor/model` 调用。
4. 在“访问密钥”页面创建网关 API Key，按需设置模型权限、到期时间和调用限额。

提供商可以配置多把上游密钥。列表会显示每把密钥的启用状态、冷却时间、请求数、成功率、平均总用时和输入/输出 Token；支持单密钥检测、全部密钥批量检测和定时健康检查。健康检查模型可以手动指定，留空时使用第一个已启用模型。

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
5. `priority` 使用较小的优先级值优先；`weighted` 按权重随机；`round_robin` 在每个网关进程内轮询。
6. 收到连接错误或 HTTP `408`、`409`、`425`、`429`、`5xx` 时尝试下一个上游。

检测到会话 ID 时，网关会将同一会话稳定映射到优先提供商和优先上游 Key，降低缓存命中被随机切换打断的概率；无会话请求仍保持原有随机、轮询和 Key 轮转行为。路由诊断传入会话 ID 后可直接查看该会话的实际候选顺序。

提供商模型名保存为上游原始名称。前缀只用于对外命名空间和自动路由，不会拼接到发送给上游的 `model` 字段。如果多个无前缀提供商暴露同名模型，网关会拒绝自动路由，此时请设置提供商前缀或创建显式路由。

### 协议兼容

- OpenAI 与 Anthropic 请求都会在响应头返回 `x-request-id` 和 `x-openllm-request-id`，可与请求日志关联。
- 非流式与流式响应都会按可用能力返回 `x-openllm-max-input-tokens`、`x-openllm-max-context-tokens` 和 `x-openllm-max-output-tokens`，便于客户端在发起后续请求前读取实际上限。
- 只支持 Anthropic Messages 的上游可以承接 `/v1/chat/completions`、`/v1/completions` 和 `/v1/responses`，网关会在请求与流式/非流式响应之间做双向转换。
- 只支持 `/chat/completions` 的 OpenAI 兼容上游也可以承接 `/v1/responses`；只支持 `/v1/responses` 的上游则能承接 `/v1/chat/completions` 和 Anthropic `/v1/messages`，网关会在这些形态之间双向转换请求与流式/非流式响应（含 Responses SSE 与 Anthropic 事件互转）。
- `/v1/models` 返回的 `supported_endpoints` 是网关实际能承接的集合，而不是上游声明的原始集合：只要上游支持其中一种消息协议，其余消息协议都会标注为可用。
- 推理内容会尽力透传：Anthropic `thinking` 和 chat `reasoning_content` 会转成 Responses `reasoning` 摘要，Responses 的 reasoning summary 也会转成 Anthropic `thinking` 块。跨提供商的签名与加密封字段无法重建，因此只用于展示，不参与回传校验。
- 上游返回只带 trace id、没有明确参数的临时 4xx 时，网关会先重试一次，再决定是否切换到下一个路由目标。
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

- 上游 API Key 按原值保存在 SQLite，用于向提供商发起请求；网关访问密钥只保存 SHA-256 摘要。
- 设置页支持在线一致性备份，不需要停服或直接复制 WAL 文件，并显示数据库文件位置、占用空间、可回收空间和各主要数据表记录数；可手动整理数据库回收已删除日志占用的空间。
- 日志保留策略支持自动清理历史请求记录。数据长期增长时，建议根据审计需求和磁盘容量设置合理的保留天数。
- SQLite 默认启用 WAL、`synchronous=NORMAL` 和 busy timeout，适合单实例部署。多个网关实例不应同时写入同一个数据库文件。
- 开启 `OPENLLM_ADMIN_TOKEN` 时，SSE 事件接口通过 `admin_token` 查询参数鉴权，因为浏览器 EventSource 不支持自定义请求头。
- `data/`、`dist/`、`target/` 和 `web/node_modules/` 默认不纳入版本控制。

## 发布流程

GitHub Actions 不会在普通分支推送时构建。Pull Request 和手动触发会执行测试与构建；推送 `v*` 格式的标签时会创建 GitHub Release，并上传 Linux、Windows 和 macOS 二进制及校验文件。

```bash
git tag v0.3.4
git push origin v0.3.4
```

发布后的文件会出现在 Releases 页面：

<https://github.com/jackclub-cn/openllm/releases>

## License

MIT License，Copyright (c) 2026 jackclub-cn。完整条款见 [LICENSE](LICENSE)。
