import { useEffect, useState } from 'react'
import {
  CheckCircleOutlined,
  ClockCircleOutlined,
  DatabaseOutlined,
  DownloadOutlined,
  SafetyCertificateOutlined,
} from '@ant-design/icons'
import {
  Alert,
  App,
  Button,
  Card,
  Descriptions,
  Form,
  Input,
  InputNumber,
  Space,
  Tag,
  Typography,
} from 'antd'
import {
  api,
  formatError,
  getAdminToken,
  type RuntimeSettings,
  type Settings,
} from '../api'
import PageHeader from '../components/PageHeader'

export default function SettingsPage({ onSave }: { onSave: (value: string) => void }) {
  const { message } = App.useApp()
  const [settings, setSettings] = useState<Settings>()
  const [saved, setSaved] = useState(false)
  const [token, setToken] = useState(getAdminToken())
  const [backingUp, setBackingUp] = useState(false)
  const [retentionDays, setRetentionDays] = useState<number | null>(null)
  const [savingRetention, setSavingRetention] = useState(false)

  useEffect(() => {
    api.get<Settings>('/api/settings').then(setSettings).catch(() => undefined)
    api
      .get<RuntimeSettings>('/api/settings/runtime')
      .then((runtime) => setRetentionDays(runtime.usage_retention_days ?? null))
      .catch(() => undefined)
  }, [])

  const backup = async () => {
    setBackingUp(true)
    try {
      const blob = await api.download('/api/database/backup')
      const url = URL.createObjectURL(blob)
      const link = document.createElement('a')
      link.href = url
      link.download = `openllm-backup-${new Date().toISOString().slice(0, 19).replaceAll(':', '-')}.db`
      link.click()
      URL.revokeObjectURL(url)
      message.success('数据库备份已下载')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setBackingUp(false)
    }
  }

  const saveRetention = async () => {
    setSavingRetention(true)
    try {
      const runtime = await api.put<RuntimeSettings>('/api/settings/runtime', {
        usage_retention_days: retentionDays && retentionDays > 0 ? retentionDays : null,
      })
      setRetentionDays(runtime.usage_retention_days ?? null)
      message.success('日志保留策略已保存')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setSavingRetention(false)
    }
  }

  return (
    <>
      <PageHeader title="设置" description="控制控制台访问并查看运行时信息" />
      {!settings?.admin_auth_enabled && (
        <Alert
          className="page-alert"
          type="warning"
          showIcon
          message="当前服务未配置 OPENLLM_ADMIN_TOKEN，管理 API 在本机或网络边界之外暴露时存在风险。"
        />
      )}
      <div className="settings-grid">
        <Card title="管理访问" bordered={false}>
          <Form layout="vertical">
            <Form.Item
              label="管理令牌"
              extra="当服务启动时设置了 OPENLLM_ADMIN_TOKEN，在此填写相同值。令牌仅保存在当前浏览器。"
            >
              <Input.Password
                value={token}
                onChange={(event) => { setToken(event.target.value); setSaved(false) }}
                placeholder="输入管理令牌"
              />
            </Form.Item>
            <Button
              type="primary"
              icon={<CheckCircleOutlined />}
              onClick={() => {
                onSave(token)
                setSaved(true)
              }}
            >
              保存到浏览器
            </Button>
            {saved && <Typography.Text type="success" className="save-note">已保存</Typography.Text>}
          </Form>
        </Card>
        <Card title="运行时" bordered={false}>
          <Descriptions column={1}>
            <Descriptions.Item label={<Space><SafetyCertificateOutlined />管理鉴权</Space>}>
              <Tag color={settings?.admin_auth_enabled ? 'success' : 'default'}>
                {settings?.admin_auth_enabled ? '已开启' : '未开启'}
              </Tag>
            </Descriptions.Item>
            <Descriptions.Item label={<Space><DatabaseOutlined />数据存储</Space>}>
              SQLite
            </Descriptions.Item>
            <Descriptions.Item label="版本">
              {settings?.version || '-'}
            </Descriptions.Item>
            <Descriptions.Item label="数据库备份">
              <Button
                icon={<DownloadOutlined />}
                loading={backingUp}
                onClick={() => void backup()}
              >
                下载备份
              </Button>
            </Descriptions.Item>
          </Descriptions>
        </Card>
      </div>
      <Card
        className="settings-retention"
        title={<Space><ClockCircleOutlined />日志保留</Space>}
        bordered={false}
      >
        <Form layout="vertical">
          <Form.Item
            label="保留天数"
            extra="留空或设为 0 表示永久保留。启用后每小时自动清理过期日志，最多保留 3650 天。"
          >
            <InputNumber
              min={0}
              max={3650}
              precision={0}
              value={retentionDays}
              onChange={(value) => setRetentionDays(value ?? null)}
              placeholder="永久保留"
              addonAfter="天"
              style={{ width: 220 }}
            />
          </Form.Item>
          <Button
            type="primary"
            icon={<CheckCircleOutlined />}
            loading={savingRetention}
            onClick={() => void saveRetention()}
          >
            保存保留策略
          </Button>
        </Form>
      </Card>
    </>
  )
}
