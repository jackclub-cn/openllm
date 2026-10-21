import { formatSeconds, formatTimestamp } from './diagnostics/format'
import DiagnosticCapacity from './diagnostics/DiagnosticCapacity'
import DiagnosticCooldowns from './diagnostics/DiagnosticCooldowns'
import DiagnosticPools from './diagnostics/DiagnosticPools'
import DiagnosticFailures from './diagnostics/DiagnosticFailures'
import DiagnosticRuntimeSettings from './diagnostics/DiagnosticRuntimeSettings'
import { useCallback, useEffect, useState } from 'react'
import {
  Alert,
  App,
  Badge,
  Button,
  Card,
  Checkbox,
  Col,
  Descriptions,
  Empty,
  Row,
  Space,
  Table,
  Tag,
  Typography,
} from 'antd'
import {
  DownloadOutlined,
  ReloadOutlined,
  SafetyCertificateOutlined,
} from '@ant-design/icons'
import {
  api,
  formatError,
  type ApiKey,
  type DiagnosticSnapshot,
  type SelfCheckItem,
  type SelfCheckStatus,
} from '../api'

const statusMeta: Record<SelfCheckStatus, { label: string; color: string }> = {
  pass: { label: '通过', color: 'success' },
  warn: { label: '注意', color: 'warning' },
  fail: { label: '失败', color: 'error' },
}

const categoryLabels: Record<string, string> = {
  startup: '启动与存储',
  resilience: '熔断与上游',
  routing: '路由与密钥',
  capacity: '容量与内存',
  observability: '可观测与维护',
}

export default function DiagnosticsPage() {
  const { message } = App.useApp()
  const [snapshot, setSnapshot] = useState<DiagnosticSnapshot>()
  const [apiKeys, setApiKeys] = useState<ApiKey[]>([])
  const [loading, setLoading] = useState(true)
  const [exporting, setExporting] = useState(false)
  const [includeSecrets, setIncludeSecrets] = useState(false)

  const load = useCallback(
    async (silent = false) => {
      if (!silent) setLoading(true)
      try {
        const [data, keys] = await Promise.all([
          api.get<DiagnosticSnapshot>('/api/diagnostics/snapshot'),
          api.get<ApiKey[]>('/api/api-keys'),
        ])
        setSnapshot(data)
        setApiKeys(keys)
      } catch (error) {
        message.error(formatError(error))
      } finally {
        setLoading(false)
      }
    },
    [message]
  )

  useEffect(() => {
    void load()
  }, [load])

  const exportSnapshot = async () => {
    setExporting(true)
    try {
      const query = includeSecrets ? '?include_secrets=true' : ''
      const blob = await api.download(`/api/diagnostics/snapshot${query}`)
      const url = URL.createObjectURL(blob)
      const anchor = document.createElement('a')
      anchor.href = url
      anchor.download = `openllm-diagnostics-${new Date()
        .toISOString()
        .replace(/[:.]/g, '-')}.json`
      anchor.click()
      URL.revokeObjectURL(url)
      message.success(includeSecrets ? '已导出（包含明文上游凭据）' : '已导出诊断快照')
      // The export is an audited action when secrets are included; refresh so
      // the audit trail and the page agree.
      if (includeSecrets) void load(true)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setExporting(false)
    }
  }

  const check = snapshot?.self_check
  const keyNames = new Map(apiKeys.map((item) => [item.id, item.name]))

  return (
    <Space
      className="diagnostics-page"
      direction="vertical"
      size={16}
      style={{ width: '100%' }}
    >
      <Card
        title={
          <Space size={12}>
            <SafetyCertificateOutlined />
            <span>网关自检</span>
            {check ? (
              <Badge
                status={
                  check.overall === 'pass'
                    ? 'success'
                    : check.overall === 'warn'
                    ? 'warning'
                    : 'error'
                }
                text={
                  check.overall === 'pass'
                    ? '全部通过'
                    : check.overall === 'warn'
                    ? `${check.counts.warn} 项需要注意`
                    : `${check.counts.fail} 项失败`
                }
              />
            ) : null}
          </Space>
        }
        extra={
          <Space>
            <Typography.Text type="secondary">
              {check ? `自检时间 ${formatTimestamp(check.generated_at)}` : ''}
            </Typography.Text>
            <Button
              icon={<ReloadOutlined />}
              loading={loading}
              onClick={() => void load()}
            >
              重新自检
            </Button>
          </Space>
        }
      >
        {check && check.counts.fail > 0 ? (
          <Alert
            type="error"
            showIcon
            style={{ marginBottom: 12 }}
            message="存在阻碍服务的检查项"
            description={check.checks
              .filter((item) => item.status === 'fail')
              .map((item) => `${item.title}：${item.detail}`)
              .join('；')}
          />
        ) : null}
        <Table<SelfCheckItem>
          size="small"
          scroll={{ x: 720 }}
          rowKey="id"
          loading={loading}
          pagination={false}
          dataSource={check?.checks || []}
          columns={[
            {
              title: '分类',
              dataIndex: 'category',
              width: 110,
              render: (value: string) => categoryLabels[value] || value,
            },
            { title: '检查项', dataIndex: 'title', width: 150 },
            {
              title: '状态',
              dataIndex: 'status',
              width: 80,
              render: (value: SelfCheckStatus) => (
                <Tag color={statusMeta[value].color}>{statusMeta[value].label}</Tag>
              ),
            },
            {
              title: '结果',
              dataIndex: 'detail',
              render: (value: string, item) => (
                <>
                  <div>{value}</div>
                  {item.hint ? (
                    <Typography.Text type="secondary">建议：{item.hint}</Typography.Text>
                  ) : null}
                </>
              ),
            },
          ]}
        />
      </Card>

      <DiagnosticCapacity snapshot={snapshot} />

      <DiagnosticRuntimeSettings limits={snapshot?.limits} />

      <DiagnosticCooldowns
        snapshot={snapshot}
        loading={loading}
        onRefresh={() => load(true)}
      />

      <DiagnosticPools snapshot={snapshot} loading={loading} keyNames={keyNames} />

      <Row gutter={[16, 16]}>
        <Col xs={24} lg={14}>
          <DiagnosticFailures snapshot={snapshot} loading={loading} />
        </Col>
        <Col xs={24} lg={10}>
          <Space direction="vertical" size={16} style={{ width: '100%' }}>
            <Card title="实例租约">
              {snapshot?.instance_lease ? (
                <Descriptions column={1} size="small">
                  <Descriptions.Item label="主机">
                    {snapshot.instance_lease.host}
                  </Descriptions.Item>
                  <Descriptions.Item label="进程 PID">
                    {snapshot.instance_lease.pid}
                    {snapshot.instance_lease.ours ? '（当前进程）' : ''}
                  </Descriptions.Item>
                  <Descriptions.Item label="启动时间">
                    {formatTimestamp(snapshot.instance_lease.started_at)}
                  </Descriptions.Item>
                  <Descriptions.Item label="心跳">
                    {formatSeconds(snapshot.instance_lease.heartbeat_age_seconds)}前
                  </Descriptions.Item>
                </Descriptions>
              ) : (
                <Empty description="未发现实例租约记录" />
              )}
            </Card>
            <Card title="诊断快照导出">
              <Space direction="vertical" size={12} style={{ width: '100%' }}>
                <Typography.Text type="secondary">
                  导出当前运行时状态、自检结果、冷却、配额池、提供商与最近失败记录，便于离线排障与随单附上。
                </Typography.Text>
                <Checkbox
                  checked={includeSecrets}
                  onChange={(event) => setIncludeSecrets(event.target.checked)}
                >
                  包含明文上游凭据（仅排障时使用）
                </Checkbox>
                {includeSecrets ? (
                  <Alert
                    type="warning"
                    showIcon
                    message="导出文件将包含上游 API Key 与自定义请求头"
                    description="该行为会写入审计日志；请勿将文件提交到版本库或公开渠道。"
                  />
                ) : (
                  <Alert
                    type="info"
                    showIcon
                    message="默认遮蔽凭据"
                    description="上游 API Key 与授权类请求头会被替换为 ***。"
                  />
                )}
                <Button
                  type="primary"
                  icon={<DownloadOutlined />}
                  loading={exporting}
                  onClick={() => void exportSnapshot()}
                >
                  导出诊断快照
                </Button>
              </Space>
            </Card>
          </Space>
        </Col>
      </Row>
    </Space>
  )
}
