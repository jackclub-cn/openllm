import { useEffect, useRef, useState } from 'react'
import {
  CloudDownloadOutlined,
  CloudUploadOutlined,
  DatabaseOutlined,
  ReloadOutlined,
  SafetyOutlined,
} from '@ant-design/icons'
import {
  Alert,
  App,
  Button,
  Card,
  Checkbox,
  Descriptions,
  Empty,
  Input,
  Space,
  Table,
  Tag,
  Typography,
} from 'antd'
import {
  api,
  formatError,
  type BackupInfo,
  type ConfigDocument,
  type DatabaseFileReport,
  type ImportAction,
  type ImportItem,
  type ImportPlan,
  type MigrationReport,
  type UpgradeStatus,
} from '../api'
import PageHeader from '../components/PageHeader'

const actionMeta: Record<ImportAction, { color: string; label: string }> = {
  create: { color: 'green', label: '新增' },
  update: { color: 'blue', label: '更新' },
  unchanged: { color: 'default', label: '未变化' },
  skip: { color: 'orange', label: '跳过' },
  error: { color: 'red', label: '错误' },
}

const upgradeMeta: Record<UpgradeStatus['status'], { color: string; label: string }> = {
  up_to_date: { color: 'green', label: '已是最新' },
  update_available: { color: 'blue', label: '有可用更新' },
  ahead: { color: 'purple', label: '领先于候选版本' },
  unknown: { color: 'default', label: '无法判断' },
}

function humanSize(bytes: number) {
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`
}

function formatTimestamp(value: string) {
  const parsed = new Date(value)
  return Number.isNaN(parsed.getTime()) ? value : parsed.toLocaleString()
}

export default function MaintenancePage() {
  const { message } = App.useApp()
  const [migrations, setMigrations] = useState<MigrationReport>()
  const [backups, setBackups] = useState<BackupInfo[]>([])
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState<string>()
  const [includeSecrets, setIncludeSecrets] = useState(false)
  const [configDocument, setConfigDocument] = useState<ConfigDocument>()
  const [documentName, setDocumentName] = useState('')
  const [apply, setApply] = useState(false)
  const [plan, setPlan] = useState<ImportPlan>()
  const [report, setReport] = useState<DatabaseFileReport>()
  const [reportName, setReportName] = useState('')
  const [upgrade, setUpgrade] = useState<UpgradeStatus>()
  const [latest, setLatest] = useState('')
  const fileInput = useRef<HTMLInputElement>(null)

  const load = async (showLoading = true) => {
    if (showLoading) setLoading(true)
    try {
      const [status, list] = await Promise.all([
        api.get<MigrationReport>('/api/maintenance/migrations'),
        api.get<BackupInfo[]>('/api/maintenance/backups'),
      ])
      setMigrations(status)
      setBackups(list)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      if (showLoading) setLoading(false)
    }
  }

  useEffect(() => {
    void load()
  }, [])

  const exportConfig = async () => {
    setBusy('export')
    try {
      const query = includeSecrets ? '?include_secrets=true' : ''
      const blob = await api.download(`/api/config/export${query}`)
      const url = URL.createObjectURL(blob)
      const anchor = window.document.createElement('a')
      anchor.href = url
      anchor.download = `openllm-config-${new Date().toISOString().replace(/[:.]/g, '-')}.json`
      anchor.click()
      URL.revokeObjectURL(url)
      message.success(includeSecrets ? '已导出（包含明文凭据，请妥善保管）' : '已导出（凭据已掩码）')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setBusy(undefined)
    }
  }

  const pickDocument = async (file?: File) => {
    if (!file) return
    try {
      const text = await file.text()
      const parsed = JSON.parse(text) as ConfigDocument
      if (typeof parsed.openllm_config_version !== 'number') {
        message.error('该文件不是 OpenLLM 配置文档（缺少 openllm_config_version）')
        return
      }
      setConfigDocument(parsed)
      setDocumentName(file.name)
      setPlan(undefined)
      message.success(
        `已载入 ${file.name}：${parsed.providers?.length ?? 0} 个提供商 / ${parsed.routes?.length ?? 0} 条路由 / ${parsed.secrets_included ? '包含明文凭据' : '凭据已掩码'}`,
      )
    } catch (error) {
      message.error(`读取失败：${formatError(error)}`)
    }
  }

  const importDocument = async (write: boolean) => {
    if (!configDocument) {
      message.warning('请先选择配置文件')
      return
    }
    setBusy(write ? 'import' : 'preview')
    try {
      const result = await api.post<ImportPlan>('/api/config/import', {
        document: configDocument,
        apply: write,
      })
      setPlan(result)
      if (result.problems.length > 0) {
        message.warning(write ? '文档有问题，未写入任何配置' : '文档有问题，请先修正')
      } else if (write) {
        message.success(`已写入：新增 ${result.counts.create} / 更新 ${result.counts.update}`)
      } else {
        message.success('校验通过，未写入任何配置')
      }
      if (write) await load(false)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setBusy(undefined)
    }
  }

  const createBackup = async () => {
    setBusy('backup')
    try {
      const info = await api.post<BackupInfo>('/api/maintenance/backup', {})
      message.success(`已写入备份 ${info.name}（${humanSize(info.size_bytes)}）`)
      await load(false)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setBusy(undefined)
    }
  }

  const verifyBackup = async (name: string) => {
    setBusy(`verify:${name}`)
    try {
      setReport(await api.post<DatabaseFileReport>('/api/maintenance/backups/verify', { name }))
      setReportName(name)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setBusy(undefined)
    }
  }

  const checkUpgrade = async () => {
    setBusy('upgrade')
    try {
      const query = latest.trim() ? `?latest=${encodeURIComponent(latest.trim())}` : ''
      setUpgrade(await api.get<UpgradeStatus>(`/api/maintenance/upgrade-check${query}`))
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setBusy(undefined)
    }
  }

  const dataDirectory = migrations?.database
    ? migrations.database.replace(/[\\/][^\\/]*$/, '')
    : '<数据目录>'

  return (
    <>
      <PageHeader
        title="运维"
        description="配置即代码、备份与恢复、迁移预检与版本升级检查"
        extra={[
          <Button
            key="refresh"
            icon={<ReloadOutlined />}
            loading={loading}
            onClick={() => void load()}
          >
            刷新
          </Button>,
          <Button
            key="backup"
            type="primary"
            icon={<DatabaseOutlined />}
            loading={busy === 'backup'}
            onClick={() => void createBackup()}
          >
            立即备份
          </Button>,
        ]}
      />

      <Card
        title="配置导入 / 导出"
        style={{ marginBottom: 16 }}
        extra={
          <Space>
            <Checkbox
              checked={includeSecrets}
              onChange={(event) => setIncludeSecrets(event.target.checked)}
            >
              包含明文上游凭据
            </Checkbox>
            <Button
              icon={<CloudDownloadOutlined />}
              loading={busy === 'export'}
              onClick={() => void exportConfig()}
            >
              导出配置
            </Button>
          </Space>
        }
      >
        <Typography.Paragraph type="secondary" style={{ marginBottom: 8 }}>
          导出的文档包含设置项、提供商（含模型启用状态）、路由、配额池与 Webhook，引用一律使用名称，
          因此可以导入到另一套网关。默认情况下上游密钥、Webhook 签名与凭据类请求头都会掩码为
          <Typography.Text code>***</Typography.Text>，掩码值永远不会被导入覆盖。
          网关 API Key 只保存哈希，只能按名称提示，无法导出或导入。
        </Typography.Paragraph>
        {includeSecrets && (
          <Alert
            type="warning"
            showIcon
            style={{ marginBottom: 8 }}
            message="勾选后导出的文件包含明文凭据"
            description="导出与导入都会写入审计日志，但文件本身就是密钥材料：请限制访问权限，并避免提交到代码仓库。"
          />
        )}
        <Space wrap>
          <input
            ref={fileInput}
            type="file"
            accept=".json,application/json"
            style={{ display: 'none' }}
            onChange={(event) => {
              void pickDocument(event.target.files?.[0])
              event.target.value = ''
            }}
          />
          <Button icon={<CloudUploadOutlined />} onClick={() => fileInput.current?.click()}>
            选择配置文件
          </Button>
          <Typography.Text type={configDocument ? undefined : 'secondary'}>
            {configDocument ? `${documentName}（v${configDocument.openllm_config_version}）` : '尚未选择文件'}
          </Typography.Text>
          <Button onClick={() => void importDocument(false)} loading={busy === 'preview'}>
            仅校验
          </Button>
          <Checkbox checked={apply} onChange={(event) => setApply(event.target.checked)}>
            写入配置
          </Checkbox>
          <Button
            type="primary"
            disabled={!apply}
            loading={busy === 'import'}
            onClick={() => void importDocument(true)}
          >
            导入
          </Button>
        </Space>
        {plan && (
          <div style={{ marginTop: 16 }}>
            {plan.problems.length > 0 && (
              <Alert
                type="error"
                showIcon
                style={{ marginBottom: 12 }}
                message={plan.applied ? '已写入可用部分' : '文档存在问题，未写入任何配置'}
                description={
                  <ul style={{ margin: 0, paddingLeft: 18 }}>
                    {plan.problems.map((problem) => (
                      <li key={problem}>{problem}</li>
                    ))}
                  </ul>
                }
              />
            )}
            {plan.notes.length > 0 && (
              <Alert
                type="info"
                showIcon
                style={{ marginBottom: 12 }}
                message="导入说明"
                description={
                  <ul style={{ margin: 0, paddingLeft: 18 }}>
                    {plan.notes.map((note) => (
                      <li key={note}>{note}</li>
                    ))}
                  </ul>
                }
              />
            )}
            <Space wrap style={{ marginBottom: 12 }}>
              <Tag color="green">新增 {plan.counts.create}</Tag>
              <Tag color="blue">更新 {plan.counts.update}</Tag>
              <Tag>未变化 {plan.counts.unchanged}</Tag>
              <Tag color="orange">跳过 {plan.counts.skip}</Tag>
              <Tag color={plan.counts.error > 0 ? 'red' : 'default'}>错误 {plan.counts.error}</Tag>
              <Tag>{plan.applied ? '已写入' : '仅校验'}</Tag>
              <Tag>{plan.secrets_included ? '含明文凭据' : '凭据已掩码'}</Tag>
            </Space>
            <Table<ImportItem>
              size="small"
              rowKey={(row) => `${row.kind}:${row.name}`}
              dataSource={plan.items}
              pagination={{ pageSize: 10, hideOnSinglePage: true }}
              columns={[
                { title: '类型', dataIndex: 'kind', width: 120 },
                { title: '名称', dataIndex: 'name', width: 200 },
                {
                  title: '动作',
                  dataIndex: 'action',
                  width: 100,
                  render: (action: ImportAction) => (
                    <Tag color={actionMeta[action].color}>{actionMeta[action].label}</Tag>
                  ),
                },
                { title: '说明', dataIndex: 'detail' },
              ]}
            />
          </div>
        )}
      </Card>

      <Card title="备份与恢复" style={{ marginBottom: 16 }}>
        <Alert
          type="info"
          showIcon
          style={{ marginBottom: 12 }}
          message="恢复必须在网关停止时进行"
          description={
            <>
              单实例网关的数据库只能被一个进程写入。恢复步骤：停止网关（等待排空完成）→
              执行 <Typography.Text code>openllm --data-dir {dataDirectory} --list-backups</Typography.Text> 查看备份 →
              执行 <Typography.Text code>openllm --data-dir {dataDirectory} --restore-backup &lt;文件名&gt;</Typography.Text> →
              启动网关并确认 <Typography.Text code>GET /api/ready</Typography.Text> 返回 200。
              备份中的实例租约会被自动清除，因此可以直接恢复到全新数据目录。
            </>
          }
        />
        <Table<BackupInfo>
          size="small"
          rowKey="name"
          loading={loading}
          dataSource={backups}
          locale={{
            emptyText: (
              <Empty
                image={Empty.PRESENTED_IMAGE_SIMPLE}
                description="还没有备份：点击右上角“立即备份”，或让启动迁移前自动生成的备份累积"
              />
            ),
          }}
          pagination={{ pageSize: 10, hideOnSinglePage: true }}
          columns={[
            {
              title: '文件',
              dataIndex: 'name',
              render: (name: string) => <Typography.Text code>{name}</Typography.Text>,
            },
            {
              title: '类型',
              dataIndex: 'kind',
              width: 120,
              render: (kind: string) => (
                <Tag color={kind === 'manual' ? 'blue' : 'default'}>
                  {kind === 'manual' ? '手动' : '迁移前'}
                </Tag>
              ),
            },
            {
              title: '大小',
              dataIndex: 'size_bytes',
              width: 110,
              render: (bytes: number) => humanSize(bytes),
            },
            {
              title: '创建时间',
              dataIndex: 'created_at',
              width: 200,
              render: (value: string) => formatTimestamp(value),
            },
            {
              title: '操作',
              width: 100,
              render: (_, row) => (
                <Button
                  size="small"
                  icon={<SafetyOutlined />}
                  loading={busy === `verify:${row.name}`}
                  onClick={() => void verifyBackup(row.name)}
                >
                  校验
                </Button>
              ),
            },
          ]}
        />
        {report && (
          <div style={{ marginTop: 16 }}>
            <Alert
              type={report.ok ? 'success' : 'error'}
              showIcon
              message={
                report.ok
                  ? `${reportName} 可以用于恢复`
                  : `${reportName} 存在问题，不要用它恢复`
              }
              description={
                <>
                  <Descriptions size="small" column={3} style={{ marginTop: 8 }}>
                    <Descriptions.Item label="完整性">{report.integrity}</Descriptions.Item>
                    <Descriptions.Item label="大小">{humanSize(report.size_bytes)}</Descriptions.Item>
                    <Descriptions.Item label="表">{report.tables.length} 个</Descriptions.Item>
                    <Descriptions.Item label="已应用迁移">{report.applied_migrations}</Descriptions.Item>
                    <Descriptions.Item label="提供商">{report.providers}</Descriptions.Item>
                    <Descriptions.Item label="路由">{report.routes}</Descriptions.Item>
                    <Descriptions.Item label="API Key">{report.api_keys}</Descriptions.Item>
                    <Descriptions.Item label="Webhook">{report.webhooks}</Descriptions.Item>
                    <Descriptions.Item label="配额池">{report.provider_pools}</Descriptions.Item>
                  </Descriptions>
                  {report.missing_tables.length > 0 && (
                    <Typography.Paragraph type="danger" style={{ marginBottom: 0 }}>
                      缺少数据表：{report.missing_tables.join('、')}
                    </Typography.Paragraph>
                  )}
                  {report.problems.map((problem) => (
                    <Typography.Paragraph key={problem} type="danger" style={{ marginBottom: 0 }}>
                      {problem}
                    </Typography.Paragraph>
                  ))}
                </>
              }
            />
          </div>
        )}
      </Card>

      <Card title="迁移预检与版本升级">
        {migrations ? (
          <>
            <Descriptions size="small" column={3} style={{ marginBottom: 12 }}>
              <Descriptions.Item label="数据文件">{migrations.database}</Descriptions.Item>
              <Descriptions.Item label="状态">
                <Tag color={migrations.up_to_date ? 'green' : 'orange'}>
                  {migrations.up_to_date ? '已是最新' : `待执行 ${migrations.pending.length} 条`}
                </Tag>
              </Descriptions.Item>
              <Descriptions.Item label="含用户数据">
                {migrations.data_present ? '是' : '否'}
              </Descriptions.Item>
              <Descriptions.Item label="已应用迁移">{migrations.applied.length}</Descriptions.Item>
              <Descriptions.Item label="启动时先备份">
                {migrations.backup_would_be_written ? '会（数据 + 待执行迁移）' : '不会'}
              </Descriptions.Item>
              <Descriptions.Item label="未知迁移">
                {migrations.unknown_applied.length > 0
                  ? migrations.unknown_applied.join('、')
                  : '无'}
              </Descriptions.Item>
            </Descriptions>
            {migrations.pending.length > 0 && (
              <Alert
                type="warning"
                showIcon
                style={{ marginBottom: 12 }}
                message={`启动时会执行 ${migrations.pending.length} 条迁移，并先写入一份迁移前备份`}
                description={
                  <ul style={{ margin: 0, paddingLeft: 18 }}>
                    {migrations.pending.map((entry) => (
                      <li key={entry.version}>
                        {entry.version} · {entry.description}
                      </li>
                    ))}
                  </ul>
                }
              />
            )}
            {migrations.problems.map((problem) => (
              <Alert
                key={problem}
                type="error"
                showIcon
                style={{ marginBottom: 12 }}
                message={problem}
              />
            ))}
          </>
        ) : (
          <Typography.Text type="secondary">正在读取迁移状态…</Typography.Text>
        )}
        <Space wrap style={{ marginBottom: 12 }}>
          <Input
            style={{ width: 220 }}
            value={latest}
            placeholder="候选版本，例如 0.4.0"
            onChange={(event) => setLatest(event.target.value)}
            onPressEnter={() => void checkUpgrade()}
          />
          <Button loading={busy === 'upgrade'} onClick={() => void checkUpgrade()}>
            检查更新
          </Button>
          <Typography.Text type="secondary">
            离线环境可手动填写候选版本；也可以设置 OPENLLM_UPGRADE_FEED_URL 后留空检查。
          </Typography.Text>
        </Space>
        {upgrade && (
          <Descriptions size="small" column={3}>
            <Descriptions.Item label="当前版本">{upgrade.current_version}</Descriptions.Item>
            <Descriptions.Item label="候选版本">{upgrade.latest_version ?? '—'}</Descriptions.Item>
            <Descriptions.Item label="结论">
              <Tag color={upgradeMeta[upgrade.status].color}>
                {upgradeMeta[upgrade.status].label}
              </Tag>
            </Descriptions.Item>
            <Descriptions.Item label="来源">
              {upgrade.source === 'query' ? '手动指定' : upgrade.source === 'feed' ? '升级源' : '无'}
            </Descriptions.Item>
            <Descriptions.Item label="待执行迁移">{upgrade.pending_migrations}</Descriptions.Item>
            <Descriptions.Item label="检查时间">{formatTimestamp(upgrade.checked_at)}</Descriptions.Item>
            <Descriptions.Item label="说明" span={3}>
              {upgrade.message}
            </Descriptions.Item>
          </Descriptions>
        )}
      </Card>
    </>
  )
}
