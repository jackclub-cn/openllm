# 部署与升级指南

本文覆盖 openllm 的单实例部署、反向代理、升级与回滚。设计前提：**一个数据库文件只服务一个进程**。网关的熔断、冷却、配额池窗口和并发槽都在进程内，SQLite 只支持单写者；同一数据库上启动第二个实例时网关会拒绝启动并在日志里指出对方的主机名与 PID（见 `OPENLLM_MULTI_INSTANCE`）。

因此升级是「先排空、再重启」，不是滚动更新。收到退出信号后网关立刻停止接受新连接并开始排空：已在途的请求继续写完，最多等待 `OPENLLM_SHUTDOWN_GRACE_SECS`（默认 30 秒），超预算时剩余连接被强制关闭，进程以退出码 `0` 结束。把 `OPENLLM_SHUTDOWN_GRACE_SECS` 设为 `0` 表示不设上限、一直等在途请求结束（此时容器/编排的停止超时必须自己兜底）。

**排空期间的可观察行为**（已用真实容器实测，下面的清单按此执行）：信号之后监听端口立即关闭，新建连接会被拒绝（`curl` 报连接失败），`/api/ready`、`/api/health`、`/api/status` 均不可达。反向代理与负载均衡拿到的摘除信号就是**连接失败**，健康检查必须把“连不上”与 `503` 同等当作不可用。`/api/ready` 的 `503 {"status":"draining"}`（此时 `/api/health` 仍为 `200`）只在连接已经建立、请求正在被处理时才会出现（探针与信号竞争，或探针复用长连接），用于进程内的状态验证；排空期间没有任何 HTTP 接口可用，所以判断排空结束要看日志与进程退出码。

## 0. 运维路径

四类运维动作的入口（控制台页面均需管理鉴权）：

| 阶段 | 控制台 | 命令行 / 接口 | 文档 |
| --- | --- | --- | --- |
| 部署 | — | `docker compose up -d`、systemd 单元 | 本文第 1–3 节；`deploy/` |
| 观测 | 仪表盘、请求日志、Webhook | `GET /metrics`、`GET /api/status` | [OBSERVABILITY.md](OBSERVABILITY.md)、README「运维路径」一节 |
| 排障 | 诊断、设置（冷却管理） | `GET /api/diagnostics/self-check`、`GET /api/diagnostics/snapshot`、`GET /api/resilience/cooldowns`、`POST /api/routes/diagnose` | 本文第 7 节 |
| 升级与回滚 | 运维 | `openllm --check-migrations`、`--backup`、`--list-backups`、`--restore-backup`、`GET /api/maintenance/*`、`GET /api/config/export` | 本文第 4–5 节 |


## 1. 容器部署

仓库根目录提供 `docker/Dockerfile`（三段构建：前端 → Rust → 精简运行镜像）和 `docker-compose.yml`。

```bash
cp deploy/openllm.env.example .env      # 至少设置 OPENLLM_ADMIN_TOKEN
docker compose up -d --build
curl -fsS http://127.0.0.1:8080/api/ready   # 200 = 可以服务
```

要点：

- 镜像默认以非 root 用户 `openllm`（uid/gid 10001）运行，数据目录 `VOLUME ["/data"]` 由 compose 挂到命名卷 `openllm-data`；换主机时把该卷（或 `/data` 目录）一起迁移即可。
- 健康检查用 `/api/ready` 而不是 `/api/health`：排空中的实例会让容器状态变为 unhealthy，编排器据此先摘流量再停止容器。
- 端口默认只发布到回环地址（`127.0.0.1:8080`），对外请用下面的反向代理终止 TLS；不要把明文 8080 直接暴露到公网。
- compose 里 `stop_grace_period: 60s` 必须大于 `OPENLLM_SHUTDOWN_GRACE_SECS`，否则 Docker 会在排空结束前 `SIGKILL` 掉进程。

启用仓库自带的 nginx TLS 示例（自签证书）：

```bash
mkdir -p deploy/nginx/certs
openssl req -x509 -newkey rsa:2048 -nodes -days 365 \
  -keyout deploy/nginx/certs/privkey.pem \
  -out deploy/nginx/certs/fullchain.pem \
  -subj "/CN=gateway.example.com"
docker compose --profile proxy up -d
curl -fsS -k https://127.0.0.1:8088/api/ready
```

## 2. systemd 部署

```bash
sudo install -m 0755 target/release/openllm /usr/local/bin/openllm
sudo install -d -o openllm -g openllm /var/lib/openllm
sudo install -d -o root -g root -m 0755 /etc/openllm
sudo install -m 0600 -o root -g root deploy/openllm.env.example /etc/openllm/openllm.env
sudo vi /etc/openllm/openllm.env                     # 设置 OPENLLM_ADMIN_TOKEN
sudo install -m 0644 deploy/systemd/openllm.service /etc/systemd/system/openllm.service
sudo systemctl daemon-reload
sudo systemctl enable --now openllm
systemctl status openllm --no-pager
curl -fsS http://127.0.0.1:8080/api/ready
```

单元文件里三处与可用性直接相关：

- `KillSignal=SIGTERM` 走正常的排空路径（`SIGKILL` 不会排空，等于丢在途请求）；
- `TimeoutStopSec=90` 高于默认的 30 秒排空预算，避免 systemd 在排空中途强杀；
- `StateDirectory=openllm` + `ReadWritePaths=/var/lib/openllm` + `ProtectSystem=strict`，让服务以非 root 身份只写自己的数据目录。

`systemctl reload` 不受支持：网关没有可重载的配置文件（设置存在数据库与环境变量里），改动环境变量后需要 `systemctl restart openllm`。

## 3. 反向代理与 TLS

SSE（`/v1/chat/completions`、`/v1/messages` 的流式响应）必须关闭代理缓冲，否则客户端要等模型生成完才收到内容；同时不要设置「整请求总超时」，只设置空闲超时，否则长生成会被切断。

- nginx：`deploy/nginx/openllm.conf`，关键指令是 `proxy_buffering off`、`proxy_read_timeout 3600s`、`proxy_http_version 1.1`、`proxy_set_header Connection ""`、`add_header X-Accel-Buffering no`。`client_max_body_size 32m` 与 `OPENLLM_MAX_BODY_MIB` 保持同步。
- Caddy：`deploy/caddy/Caddyfile`，`flush_interval -1` 即关闭缓冲，证书自动签发。
- 网关自身还会按 `OPENLLM_SSE_KEEPALIVE_SECS`（默认 15 秒）发送 SSE 心跳注释，模型长时间思考时不会让中间设备判定连接空闲。

## 4. 升级（先排空，再重启）

容器部署：

```bash
docker compose pull            # 或 git pull && docker compose build
docker compose up -d           # 重建并重启容器
docker compose ps              # 等 healthy
curl -fsS http://127.0.0.1:8080/api/ready
```

systemd 部署：

```bash
sudo install -m 0755 target/release/openllm /usr/local/bin/openllm.new
sudo systemctl stop openllm                 # 触发排空：立刻停止收新连接，等在途请求跑完
sudo mv /usr/local/bin/openllm.new /usr/local/bin/openllm
sudo systemctl start openllm
curl -fsS http://127.0.0.1:8080/api/ready
```

升级检查清单（每一步都有可观察的结果）：

1. **升级前**：`curl -fsS http://127.0.0.1:8080/api/status | jq '{version, draining, memory, admission}'` 确认实例健康且未在排空；记录当前版本。
2. **排空确实开始**：发起停止后日志出现 `shutdown signal received; draining in-flight requests`，并且监听端口立即关闭（此时 `curl -i http://127.0.0.1:8080/api/health` 报连接失败而不是 `200`）——这就是负载均衡看到的摘除信号。若探针在信号前刚抢到一次响应，会看到 `/api/ready` 返回 `503` 且 body 含 `draining`。
3. **等待在途请求**：长流可以在排空期间写完（受 `OPENLLM_SHUTDOWN_GRACE_SECS` 限制）。排空期间没有可用的 HTTP 接口，因此按日志与退出码确认：正常排空结束是退出码 `0`，超出预算会先记录 `shutdown grace period elapsed; closing remaining connections`。要定量观察只能在停止**之前**记录 `GET /api/status` 的 `admission.in_flight` 或 `openllm_requests_in_flight` 基线。
4. **迁移前自动备份**：若新版本带有未应用的迁移且库里已有数据，启动时会先写 `<数据目录>/backups/openllm-pre-migration-<目标版本>-<时间戳>.db`，日志中有 `wrote a pre-migration backup`（只保留最近 5 份）。
5. **启动新版本**：启动失败会以非零退出码结束且**不会打开监听端口**，错误信息里带上备份路径；此时数据未被修改，失败的迁移也不会写入 `_sqlx_migrations`。
6. **升级后**：`/api/ready` 返回 `200`、`/api/status` 的 `version` 与 `uptime_seconds` 已刷新、`/metrics` 可抓取、控制台仪表盘显示实例运行状态。升级前建议先跑一次 `openllm --data-dir <数据目录> --check-migrations`（退出码 `2` 表示有待执行迁移，此时启动会先自动备份），升级后可用 `--check-migrations` 或 `GET /api/maintenance/migrations` 确认已无待执行迁移。
7. **功能抽查**：发一个流式请求确认 SSE 正常（`curl -N` 应立刻看到增量），并确认 `/api/resilience/cooldowns` 与 `GET /api/providers/{id}/pool-usage` 返回的是预期状态。

### 回滚

1. 停掉新版本（同样先排空）。
2. 若要回到旧二进制：直接用旧二进制启动即可——迁移是向后兼容的增量变更，旧版本会忽略新表；若数据库结构已不兼容或新版本迁移中断，则用备份恢复（推荐用 `openllm --data-dir /var/lib/openllm --restore-backup <文件名>`，它会先校验备份、清除租约，并把原库连同它的 `-wal`/`-shm` 一起改名为 `openllm.db.pre-restore`）：
   ```bash
   systemctl stop openllm
   cp /var/lib/openllm/backups/openllm-pre-migration-<版本>-<时间戳>.db /var/lib/openllm/openllm.db
   rm -f /var/lib/openllm/openllm.db-wal /var/lib/openllm/openllm.db-shm
   systemctl start openllm
   ```
3. 恢复后确认 `/api/ready` 为 `200`、`/api/status` 的版本号是回滚目标版本，且 `instance_leases` 中没有残留租约（网关启动时会自动接管已过期或已消失进程的租约）。

## 5. 备份、恢复与配置即代码

### 备份

网关会在两种时机自动写备份到 `<数据目录>/backups/`（各保留最近 5 份）：启动执行迁移前（`openllm-pre-migration-*`）和手动触发（`openllm-manual-*`）。备份用 `VACUUM INTO` 生成，是事务一致的单文件快照；WAL 模式下直接 `cp` 数据库文件可能拷到未完成检查点的状态，所以请用下面任一方式：

```bash
# 服务运行中也可以执行（只读连接）
openllm --data-dir /var/lib/openllm --backup
openllm --data-dir /var/lib/openllm --list-backups

# 或走管理接口
curl -fsS -X POST -H "x-admin-token: $T" http://127.0.0.1:8080/api/maintenance/backup
curl -fsS -H "x-admin-token: $T" http://127.0.0.1:8080/api/maintenance/backups
```

恢复前先校验备份是否真的可用（只读检查文件头、`integrity_check`、必备数据表与数据条数）：

```bash
curl -fsS -X POST -H "x-admin-token: $T" -H 'content-type: application/json' \
  -d '{"name":"openllm-manual-20261013-111040.db"}' \
  http://127.0.0.1:8080/api/maintenance/backups/verify
```

### 恢复到新数据目录

备份自带实例租约，且网关不会接管其他主机的租约，所以直接恢复到全新目录是受支持的流程：

1. 把备份文件放进新数据目录的 `backups/` 下。
2. 执行恢复（此时不应有实例在服务这个目录，否则会被拒绝并提示占用者）：
   ```bash
   openllm --data-dir /var/lib/openllm-new --restore-backup openllm-manual-20261013-111040.db
   ```
3. 启动网关，确认 `GET /api/ready` 返回 `200`，再核对提供商、路由与请求日志。

恢复会先把原库连同它的 `-wal`/`-shm` 一起改名为 `openllm.db.pre-restore`（因此迁移中断前未完成检查点的已提交数据不会丢，原库随时可以拿回来），再让暂存文件入库，随后清除备份中的实例租约并移除目标路径上的 `-wal`/`-shm` 残留；失败时（校验不通过、有实例在服务）不会动现有数据。旧版本也支持手工恢复：停库后 `cp` 备份覆盖 `openllm.db`，删除 `openllm.db-wal`/`-shm` 再启动。

### 迁移预检

升级前先确认识别到多少待执行迁移、是否含用户数据、启动时会不会先备份；该检查完全只读，不会创建数据库文件：

```bash
openllm --data-dir /var/lib/openllm --check-migrations        # 0=已是最新 / 2=有待执行迁移 / 1=需人工处理
openllm --data-dir /var/lib/openllm --check-migrations --json # 脚本化使用
```

运行中的实例可读 `GET /api/maintenance/migrations`，输出同上；例如“未知迁移”非空说明数据库被更新的二进制迁移过，此时用当前二进制启动属于降级，报告会直接给出风险提示。

### 配置即代码

```bash
# 导出（默认掩码凭据，掩码值不会被导入覆盖）
curl -fsS -H "x-admin-token: $T" http://127.0.0.1:8080/api/config/export > openllm-config.json

# 先干跑：只返回逐项计划，不写任何配置
curl -fsS -H "x-admin-token: $T" -H 'content-type: application/json' \
  -d "{\"document\": $(cat openllm-config.json), \"apply\": false}" \
  http://127.0.0.1:8080/api/config/import
```

计划里每一项都有 `create`/`update`/`unchanged`/`skip`/`error` 动作与说明；内容相同的项报 `unchanged`，因此重复导入是幂等的。引用不到的提供商或网关密钥（比如文件是在另一套环境导出的）只会把该项标为 `skip` 并附原因，不会中断整份导入。需要把上游密钥一并迁移时加 `?include_secrets=true`，此时导出的文件就是密钥材料：限权保管，并避免提交到代码仓库（导入导出都会写入一条不含密钥值的审计记录）。

版本升级比较（当前版本 vs 候选版本，并附待执行迁移数）：

```bash
curl -fsS -H "x-admin-token: $T" "http://127.0.0.1:8080/api/maintenance/upgrade-check?latest=0.4.0"
```

未传 `latest` 时改用 `OPENLLM_UPGRADE_FEED_URL` 指定的升级源；增加 `official=true`
可在未配置自定义源时检查 `jackclub-cn/openllm` 的 GitHub 正式发布版本。不带这些
参数且未配置升级源时仍返回 `unknown`。右上角版本提示和运维页的留空检查会传
`official=true`；控制台打开后每小时检查一次，服务端成功缓存一小时、失败缓存一分钟，
并合并并发检查。连接超时、限流或版本无法解析时保留未知状态，不误报为已是最新。
有更新时提示链接指向项目发布页；只提供更新信息，不执行自动升级。

左侧菜单底部提供项目 GitHub 链接，导航收起时保留图标入口。

## 6. 日常巡检

| 检查项 | 命令 | 期望 |
| --- | --- | --- |
| 存活 | `curl -fsS http://127.0.0.1:8080/api/health` | `200` |
| 就绪 | `curl -fsS http://127.0.0.1:8080/api/ready` | `200`（排空中根本探测不到：端口已关闭） |
| 运行状态 | `curl -fsS -H "x-admin-token: $T" http://127.0.0.1:8080/api/status` | `draining=false`、无内存压力、熔断摘要正常 |
| 冷却与熔断 | `curl -fsS -H "x-admin-token: $T" http://127.0.0.1:8080/api/resilience/cooldowns` | 只有预期的短期冷却 |
| 配额池 | `curl -fsS -H "x-admin-token: $T" http://127.0.0.1:8080/api/providers/1/pool-usage` | 余量符合预期，无长期饱和 |
| 指标 | `curl -fsS -H "x-admin-token: $T" http://127.0.0.1:8080/metrics` | 可抓取，`openllm_maintenance_last_run_timestamp_seconds` 持续推进 |
| 备份 | `openllm --data-dir <数据目录> --list-backups` | 升级后出现预迁移备份，手动备份数量在预期内 |
| 迁移 | `openllm --data-dir <数据目录> --check-migrations` | 升级后退出码为 `0`（无待执行迁移） |

## 7. 故障排查

- **启动报 “another OpenLLM instance is already serving this database”**：同一数据库上还有活着的实例。停掉旧实例后重试；确认旧进程已消失（同一主机上已消失的 PID 会被自动接管），确实需要临时双跑时才用 `OPENLLM_MULTI_INSTANCE=warn`（不受支持）。
- **启动报 “database migration failed”**：按错误里的路径取预迁移备份；修好问题后用同一二进制重试即可（失败的迁移没有被记录）。
- **停止后 `curl` 立刻提示连接失败**：这是排空的正常表现，网关在收到关闭信号的同时就停止接受新连接；日志里的 `draining in-flight requests` 表示在途请求仍在收尾，正常结束应以退出码 `0` 收尾。
- **`/api/ready` 一直 `503` 但 `/api/health` 是 `200`**：只可能出现在连接已经建立的情况下。持续如此说明进程处于排空状态却没有退出，检查是否有长连接让在途请求超过 `OPENLLM_SHUTDOWN_GRACE_SECS`，或数据库探测失败（body 会带原因）。
- **流式响应一次性返回**：反向代理在缓冲，按第 3 节关闭缓冲；或客户端/中间件设置了整请求超时。
- **被 OOM 杀掉**：容器里网关会自动读取 cgroup 上限并采样内存，超过 `OPENLLM_MEMORY_SHED_RATIO` 后对新请求返回 `503`；仍然被杀说明上限过低，或需要收紧 `OPENLLM_MAX_CONCURRENT_REQUESTS` / `OPENLLM_MAX_INFLIGHT_REQUEST_MIB`。
