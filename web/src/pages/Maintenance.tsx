import { useEffect, useState } from 'react'
import {
  DatabaseOutlined,
  ReloadOutlined,
  ToolOutlined,
} from '@ant-design/icons'
import {
  Alert,
  App,
  Button,
  Card,
  Descriptions,
  Input,
  Space,
  Tag,
  Typography,
} from 'antd'
import {
  api,
  formatError,
  type BackupInfo,
  type DatabaseFileReport,
  type DatabaseVacuumResult,
  type MigrationReport,
  type Settings,
  type UpgradeStatus,
} from '../api'
import PageHeader from '../components/PageHeader'
import BackupRecoveryCard from './maintenance/BackupRecoveryCard'
import ConfigTransferCard from './maintenance/ConfigTransferCard'
import DatabaseStatusCard from './maintenance/DatabaseStatusCard'
import { formatTimestamp, humanSize } from './maintenance/format'

const upgradeMeta: Record<UpgradeStatus['status'], { color: string; label: string }> = {
  up_to_date: { color: 'green', label: '已是最新' },
  update_available: { color: 'blue', label: '有可用更新' },
  ahead: { color: 'purple', label: '领先于候选版本' },
  unknown: { color: 'default', label: '无法判断' },
}

export default function MaintenancePage() {
  const { message } = App.useApp()
  const [migrations, setMigrations] = useState<MigrationReport>()
  const [backups, setBackups] = useState<BackupInfo[]>([])
  const [settings, setSettings] = useState<Settings>()
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState<string>()
  const [vacuuming, setVacuuming] = useState(false)
  const [downloadingDatabase, setDownloadingDatabase] = useState(false)
  const [runningMaintenance, setRunningMaintenance] = useState(false)
  const [report, setReport] = useState<DatabaseFileReport>()
  const [reportName, setReportName] = useState('')
  const [upgrade, setUpgrade] = useState<UpgradeStatus>()
  const [latest, setLatest] = useState('')

  const load = async (showLoading = true) => {
    if (showLoading) setLoading(true)
    try {
      const [status, list, currentSettings] = await Promise.all([
        api.get<MigrationReport>('/api/maintenance/migrations'),
        api.get<BackupInfo[]>('/api/maintenance/backups'),
        api.get<Settings>('/api/settings'),
      ])
      setMigrations(status)
      setBackups(list)
      setSettings(currentSettings)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      if (showLoading) setLoading(false)
    }
  }

  useEffect(() => {
    void load()
  }, [])

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

  const downloadDatabase = async () => {
    setDownloadingDatabase(true)
    try {
      const blob = await api.download('/api/database/backup')
      const url = URL.createObjectURL(blob)
      const link = document.createElement('a')
      link.href = url
      link.download = `openllm-backup-${new Date()
        .toISOString()
        .slice(0, 19)
        .replaceAll(':', '-')}.db`
      link.click()
      URL.revokeObjectURL(url)
      message.success('数据库备份已下载')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setDownloadingDatabase(false)
    }
  }

  const runMaintenance = async () => {
    setRunningMaintenance(true)
    try {
      await api.post('/api/maintenance/run')
      message.success('维护任务已运行')
      await load(false)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setRunningMaintenance(false)
    }
  }

  const vacuum = async () => {
    setVacuuming(true)
    try {
      const result = await api.post<DatabaseVacuumResult>('/api/database/vacuum')
      setSettings((current) =>
        current ? { ...current, database_stats: result.database_stats } : current
      )
      message.success(
        result.reclaimed_bytes > 0
          ? `数据库已整理，释放 ${humanSize(result.reclaimed_bytes)}`
          : '数据库已整理，无需释放空间'
      )
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setVacuuming(false)
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
      const query = latest.trim() ? `?latest=${encodeURIComponent(latest.trim())}` : '?official=true'
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
        description="数据库维护、备份与恢复、配置迁移和版本升级检查"
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
            key="maintenance"
            icon={<ToolOutlined />}
            loading={runningMaintenance}
            onClick={() => void runMaintenance()}
          >
            立即维护
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

      <div style={{ marginBottom: 16 }}>
        <DatabaseStatusCard
          stats={settings?.database_stats}
          vacuuming={vacuuming}
          downloadLoading={downloadingDatabase}
          onVacuum={() => void vacuum()}
          onDownload={() => void downloadDatabase()}
        />
      </div>

      <ConfigTransferCard onImported={() => load(false)} />

      <BackupRecoveryCard
        backups={backups}
        loading={loading}
        busy={busy}
        dataDirectory={dataDirectory}
        report={report}
        reportName={reportName}
        onVerify={(name) => void verifyBackup(name)}
      />

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
            留空检查 GitHub 正式发布版本；离线环境可填写候选版本。
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
