# 发版流程

本文约定 openllm 从版本准备到 GitHub Release 验收的完整流程。
自动化行为以 [build.yml](../.github/workflows/build.yml) 为准，
开发检查遵循 [CODE_STANDARDS.md](CODE_STANDARDS.md)，
部署、迁移和回滚见 [DEPLOYMENT.md](DEPLOYMENT.md)。

## 1. 发布时机与范围

| 用户请求 | 执行范围 |
| --- | --- |
| 修复、继续、整理代码或文档 | 完成工作和验证，不自动提交或发版 |
| 提交 | 提交指定范围，不自动打标签或发布 |
| 发版、发一个小版本 | 准备版本、验证、提交、推送、打标签、等待 CI 并验收 Release |

明确要求发版后，按本流程完成，无需逐步重复询问。部署到运行环境另按部署要求执行。
日常修复可以积累后一起发布，不要求每次修改都增加版本。

| 项目 | 约定 |
| --- | --- |
| 发布分支 | `master` |
| 小版本 | 在当前 `0.3.x` 系列增加补丁号，例如 `0.3.30` → `0.3.31` |
| Git 标签 | `v<版本号>`，例如 `v0.3.31` |
| 发布提交标题 | `Release v<版本号>` |
| 版本选择 | 同时检查远端标签、已发布 Release 和本地版本，使用尚未占用的递增版本 |

先检查工作区、并行任务和上次发布以来的差异：

```powershell
git status --short
git fetch origin --tags
git tag --list "v*" --sort=-version:refname
gh release list --limit 10
```

确认哪些改动属于本轮，保留其它未完成工作，不使用 `git add .` 将它们一起发布。
并行修改尚未完成时，先确定完整发布范围；需要隔离时使用独立 worktree，
并在最终发布提交上验证。隔离的部分补丁测试不能替代完整发布版本的检查。

## 2. 同步版本

所有版本位置必须一致：

| 文件 | 更新位置 |
| --- | --- |
| `Cargo.toml` | `[package].version` |
| `Cargo.lock` | `name = "openllm"` 对应的包版本，由 Cargo 更新 |
| `web/package.json` | 顶层 `version` |
| `web/package-lock.json` | 顶层 `version` 和 `packages[""].version`，由 npm 更新 |

下面的版本号仅作示例，每次发布先按远端实际状态选择：

```powershell
$version = "0.3.31"
```

修改 `Cargo.toml` 后，在仓库根目录执行：

```powershell
cargo update --workspace --offline
npm --prefix web version $version --no-git-tag-version --ignore-scripts
git diff -- Cargo.toml Cargo.lock web/package.json web/package-lock.json
```

确认锁文件只包含预期变化，避免为升项目版本顺带更新依赖。
运行时版本来自 `CARGO_PKG_VERSION`，仅改标签无法改变二进制报告的版本号。
历史文档中的版本基线保留原值，不全仓替换旧版本字符串。

## 3. 本地验证

环境使用 Node.js 22 或以上、npm、支持 Rust 2024 edition 的 stable Rust，
并安装 `rustfmt`、`clippy`。GitHub 操作需要可用的 Git 和已认证的 `gh`。

前端资源嵌入 Rust 二进制，干净检出和临时 worktree 都必须先生成 `web/dist`。
在仓库根目录逐条执行，每条成功后再继续：

```powershell
npm --prefix web ci
npm --prefix web run build
node scripts/check-code-structure.mjs
node --test scripts/check-code-structure.test.mjs
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
git diff --check
```

| 补充检查 | 要求 |
| --- | --- |
| 前端行为测试 | 存在 `web/tests/` 时运行相关 Node 测试；活动状态测试用 `node --experimental-strip-types --test web/tests/activityState.test.mjs` |
| 监控产物 | 在可用 Bash 中执行 `bash scripts/check-monitoring.sh`；完整规则检查需要 `promtool` 或可用 Docker |
| 测试数量 | 记录测试总数；移动模块或测试后不能无故减少 |
| 文件规模 | 不提高规模上限或增加例外以绕过结构检查 |
| 迁移 | 保留已发布迁移内容和顺序；新增迁移验证旧库升级和迁移前备份 |
| 前端界面 | 改动页面检查桌面、空状态及实际交互；窄屏不作为发布门禁 |

控制台当前以桌面操作为主。发布验收固定检查桌面布局、空状态和实际交互，
不要求为窄屏重新排版，也不把窄屏溢出作为阻断发版的问题。

监控脚本出现 `skip` 时记录具体未执行项，不能写成完整监控验证通过。
本机无法完成的平台检查由相同提交的 CI 补齐；本机已知的代码错误应先修复。
检查后如果源码、版本、锁文件或嵌入的前端资源发生变化，补跑对应检查和构建。

发布前至少检查本机 release 二进制的 `--version`，并用独立测试数据目录做启动抽查。

| 抽查范围 | 验收内容 |
| --- | --- |
| 基础服务 | `/api/health`、`/api/ready` 正常，`/api/status` 的版本与目标一致 |
| 控制台 | 嵌入页面可打开，本轮改动的页面可以正常操作 |
| 请求链路 | 本轮涉及的协议、流式输出、错误返回和故障切换符合预期 |
| 提供商适配 | 同时检查正常调用、提供商测试、密钥测试及自动健康检查的共享路径 |
| OpenCode Go 会话头 | 推理转发和健康探针均覆盖，非 Go 提供商及自定义请求头优先级不回归 |

回归测试使用本地模拟上游不等于真实提供商实测；汇报时区分两者。
真实调用使用获准的测试配置，避免把凭据、数据库或诊断快照加入发布内容。

`build.ps1` 和 `build.sh` 可生成本机 `dist/` 二进制，但仅执行前端和 Rust 构建，
不能代替上述验证。GitHub 上传的产物由 CI 构建。

## 4. 提交与推送标签

发布到 `master` 的提交应包含完整的本轮改动、四个版本文件和必要文档。
按明确路径暂存，然后检查暂存区：

```powershell
git diff --cached --stat
git diff --cached --check
git diff --cached
git commit -m "Release v$version" -m "填写本轮实际变更摘要"
git status --short
$releaseCommit = git rev-parse HEAD
```

再次确认测试对应 `$releaseCommit`，发布范围内没有遗漏的未提交文件。
提交后的其它未发布改动应保持独立。功能分支先按项目协作方式合入 `master`，
若合入改变了最终内容，验证最终提交后再创建标签。以下命令从最终的
`master` 发布提交执行，重新记录其提交 ID：

```powershell
$releaseCommit = git rev-parse HEAD
git push origin HEAD:master
git tag -a "v$version" -m "Release v$version"
git push origin "v$version"
git rev-parse "v$version^{commit}"
git ls-remote origin "refs/tags/v$version" "refs/tags/v$version^{}"
```

本地标签对应的提交、远端标签解析后的提交和 `$releaseCommit` 必须一致。
只推本次标签，不批量推送所有本地标签，也不强推覆盖已发布标签。

## 5. 等待 Actions

当前工作流触发行为如下：

| 触发方式 | 行为 |
| --- | --- |
| Push 到 `master`、Pull Request | 质量检查、监控检查、三平台测试与构建 |
| Push `v*` 标签 | 上述任务全部成功后，创建或更新该标签的 GitHub Release |
| 手动触发 | 执行检查与构建；若选择的是 `v*` 标签，发布条件同样成立 |

| 任务 | 当前检查 |
| --- | --- |
| `quality` | 文件规模、结构检查脚本测试、Rust 格式 |
| `build` | Linux、Windows、macOS 分别安装并构建前端，执行严格 Clippy、Rust 测试、release 构建，生成校验文件 |
| `monitoring` | 告警规则、规则测试、Grafana JSON、引用指标名检查；注意脚本输出是否跳过检查 |
| `release` | 依赖以上所有任务，合并下载三平台产物，通过 `softprops/action-gh-release` 上传；标签推送时使用发布提交说明，并附自动生成的变更记录 |

工作流目前没有执行 `web/tests/`，存在相关测试时仍需按第 3 节本地执行。

按发布提交查询运行记录，选择 `headBranch` 对应本次标签的那一次，
不要将 `master` 的绿色运行误认为标签发布已完成：

```powershell
gh run list --workflow build.yml --commit $releaseCommit --event push --json databaseId,headBranch,headSha,status,conclusion,url
gh release view "v$version" --json tagName,isDraft,isPrerelease,publishedAt,url,assets
```

使用查到的运行 ID 执行 `gh run watch <run-id> --exit-status`，等待发布任务结束。
Actions 尚在运行时只报告“已推送，构建中”；必须确认 `release` 成功后再报告发布完成。

## 6. 发布验收与说明

Release 应为目标标签的正式发布，且以下六个附件齐全、大小非零：

| 平台 | 二进制 | SHA-256 文件 |
| --- | --- | --- |
| Linux | `openllm-linux` | `openllm-linux.sha256` |
| Windows | `openllm-windows.exe` | `openllm-windows.exe.sha256` |
| macOS | `openllm-macos` | `openllm-macos.sha256` |

下载本机平台的二进制与对应校验文件，比较 SHA-256，并执行 `--version`。
Windows 用 `Get-FileHash -Algorithm SHA256`，Linux 用 `sha256sum -c`，
macOS 用 `shasum -a 256 -c`；校验值应与附件一致。

Actions 在标签推送时使用发布提交正文，并附自动生成的变更记录；手动触发时沿用
自动说明。发布提交正文应覆盖实际变化、兼容性和验证。检查说明能否让用户看懂本轮变化，需要时用 UTF-8
Markdown 文件整理后执行 `gh release edit <tag> --notes-file <说明文件>`。
文件使用真实换行，不把字面量 `\n` 当作段落。

| 发布说明内容 | 写法 |
| --- | --- |
| 功能和修复 | 描述用户遇到的问题和修复后的行为，聚焦本轮实际内容 |
| 兼容性 | 说明默认行为、配置、数据迁移及升级注意事项的实际变化 |
| 验证 | 本地测试数量、构建与静态检查结果、三平台 CI 结果 |
| 已知限制 | 如仍有未验证的真实上游、平台或未执行检查，明确写出 |

完成后向用户报告版本号、主要变化、验证结果和 Release 地址。
发布二进制不代表已部署；升级和回滚按 [DEPLOYMENT.md](DEPLOYMENT.md) 执行。

## 7. 失败处理

| 情况 | 处理 |
| --- | --- |
| 本地验证失败 | 修复并重跑，尚未满足发布条件 |
| CI 失败 | 用 `gh run view <run-id> --log-failed` 定位平台、任务和错误 |
| CI 临时基础设施故障 | 确认无需改代码后可重跑失败任务，完成后重新验收 |
| 已推标签后发现代码错误 | 修复为新提交并递增补丁版本，重新执行流程；不把旧标签移到新提交 |
| Release 缺附件或发布任务失败 | 先检查标签运行和上传步骤；附件补齐并核验前不宣布完成 |
| 并行工作导致当前工作区未完成 | 保留其它修改，在包含完整发布范围的最终提交上验证 |
| 运行环境升级失败 | 按部署文档排空、恢复对应二进制和必要的数据备份，不改发布标签掩盖问题 |
