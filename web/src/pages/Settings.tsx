import { useEffect, useState } from 'react'
import {
  ClearOutlined,
  CheckCircleOutlined,
  ClockCircleOutlined,
  CompressOutlined,
  DatabaseOutlined,
  DownloadOutlined,
  FileSearchOutlined,
  ReloadOutlined,
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
  Popconfirm,
  Space,
  Switch,
  Table,
  Tag,
  Tooltip,
  Typography,
} from 'antd'
import {
  api,
  formatError,
  getAdminToken,
  type DatabaseVacuumResult,
  type ClearCooldownsResult,
  type CooldownSnapshot,
  type GuardrailSettings,
  type InspectorSettings,
  type ResilienceSettings,
  type RuntimeLimits,
  type RuntimeSettings,
  type Settings,
} from '../api'
import PageHeader from '../components/PageHeader'
import { formatBytes, formatCompact, formatExact } from '../format'

export default function SettingsPage({ onSave }: { onSave: (value: string) => void }) {
  const { message } = App.useApp()
  const [settings, setSettings] = useState<Settings>()
  const [saved, setSaved] = useState(false)
  const [token, setToken] = useState(getAdminToken())
  const [backingUp, setBackingUp] = useState(false)
  const [vacuuming, setVacuuming] = useState(false)
  const [runningMaintenance, setRunningMaintenance] = useState(false)
  const [retentionDays, setRetentionDays] = useState<number | null>(null)
  const [savingRetention, setSavingRetention] = useState(false)
  const [limits, setLimits] = useState<RuntimeLimits>()
  const [blockedTerms, setBlockedTerms] = useState('')
  const [maxPromptTokens, setMaxPromptTokens] = useState<number | null>(null)
  const [savingGuardrails, setSavingGuardrails] = useState(false)
  const [captureRequestPreviews, setCaptureRequestPreviews] = useState(false)
  const [requestPreviewMaxChars, setRequestPreviewMaxChars] = useState(4000)
  const [savingInspector, setSavingInspector] = useState(false)
  const [streamRecoveryEnabled, setStreamRecoveryEnabled] = useState(false)
  const [streamRecoveryMaxRetries, setStreamRecoveryMaxRetries] = useState(2)
  const [maxRetries, setMaxRetries] = useState(1)
  const [retryBackoffMs, setRetryBackoffMs] = useState(200)
  const [retryMaxBackoffMs, setRetryMaxBackoffMs] = useState(2000)
  const [savingResilience, setSavingResilience] = useState(false)
  const [cooldowns, setCooldowns] = useState<CooldownSnapshot>()
  const [loadingCooldowns, setLoadingCooldowns] = useState(false)
  const [clearingCooldowns, setClearingCooldowns] = useState(false)

  const loadCooldowns = () => {
    setLoadingCooldowns(true)
    api
      .get<CooldownSnapshot>('/api/resilience/cooldowns')
      .then(setCooldowns)
      .catch(() => undefined)
      .finally(() => setLoadingCooldowns(false))
  }

  useEffect(() => {
    api.get<Settings>('/api/settings').then(setSettings).catch(() => undefined)
    api
      .get<RuntimeSettings>('/api/settings/runtime')
      .then((runtime) => {
        setRetentionDays(runtime.usage_retention_days ?? null)
        setLimits(runtime.limits)
      })
      .catch(() => undefined)
    api
      .get<GuardrailSettings>('/api/settings/guardrails')
      .then((guardrails) => {
        setBlockedTerms(guardrails.blocked_terms.join('\n'))
        setMaxPromptTokens(guardrails.max_prompt_tokens ?? null)
      })
      .catch(() => undefined)
    api
      .get<InspectorSettings>('/api/settings/inspector')
      .then((inspector) => {
        setCaptureRequestPreviews(inspector.capture_request_previews)
        setRequestPreviewMaxChars(inspector.request_preview_max_chars)
      })
      .catch(() => undefined)
    api
      .get<ResilienceSettings>('/api/settings/resilience')
      .then((resilience) => {
        setStreamRecoveryEnabled(resilience.stream_recovery_enabled)
        setStreamRecoveryMaxRetries(resilience.stream_recovery_max_retries)
        setMaxRetries(resilience.max_retries)
        setRetryBackoffMs(resilience.retry_backoff_ms)
        setRetryMaxBackoffMs(resilience.retry_max_backoff_ms)
      })
      .catch(() => undefined)
    loadCooldowns()
  }, [])

  const clearCooldowns = async (body: Record<string, unknown>) => {
    setClearingCooldowns(true)
    try {
      const result = await api.post<ClearCooldownsResult>(
        '/api/resilience/cooldowns/clear',
        body,
      )
      const total =
        result.cleared_provider_cooldowns +
        result.cleared_model_cooldowns +
        result.cleared_provider_key_cooldowns
      message.success(total ? `已清除 ${total} 条冷却` : '没有匹配的冷却')
      loadCooldowns()
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setClearingCooldowns(false)
    }
  }

  type CooldownRow = {
    key: string
    scope: string
    target: string
    detail: string
    remaining: number
    clear: Record<string, unknown>
  }
  const cooldownRows: CooldownRow[] = [
    ...(cooldowns?.provider_cooldowns ?? []).map((item) => ({
      key: `provider-${item.provider_id}`,
      scope: '提供商',
      target: item.provider_name ?? `#${item.provider_id}`,
      detail: item.failure_streak ? `连续失败 ${item.failure_streak} 次` : '',
      remaining: item.remaining_seconds,
      clear: { provider_id: item.provider_id },
    })),
    ...(cooldowns?.model_cooldowns ?? []).map((item) => ({
      key: `model-${item.provider_id}-${item.model}`,
      scope: '模型',
      target: `${item.provider_name ?? `#${item.provider_id}`} · ${item.model}`,
      detail: '',
      remaining: item.remaining_seconds,
      clear: { provider_id: item.provider_id, model: item.model },
    })),
    ...(cooldowns?.provider_key_cooldowns ?? []).map((item) => ({
      key: `key-${item.provider_key_id}`,
      scope: '上游密钥',
      target: `${item.provider_name ?? '-'} · ${item.key_name ?? `#${item.provider_key_id}`}`,
      detail: '',
      remaining: item.remaining_seconds,
      clear: { provider_key_id: item.provider_key_id },
    })),
  ]

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

  const runMaintenance = async () => {
    setRunningMaintenance(true)
    try {
      await api.post('/api/maintenance/run')
      message.success('维护任务已运行')
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
        current ? { ...current, database_stats: result.database_stats } : current,
      )
      message.success(
        result.reclaimed_bytes > 0
          ? `数据库已整理，释放 ${formatBytes(result.reclaimed_bytes)}`
          : '数据库已整理，无需释放空间',
      )
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setVacuuming(false)
    }
  }

  const saveGuardrails = async () => {
    setSavingGuardrails(true)
    try {
      const guardrails = await api.put<GuardrailSettings>('/api/settings/guardrails', {
        blocked_terms: blockedTerms
          .split(/\r?\n/)
          .map((term) => term.trim())
          .filter(Boolean),
        max_prompt_tokens: maxPromptTokens && maxPromptTokens > 0 ? maxPromptTokens : null,
      })
      setBlockedTerms(guardrails.blocked_terms.join('\n'))
      setMaxPromptTokens(guardrails.max_prompt_tokens ?? null)
      message.success('请求防护策略已保存')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setSavingGuardrails(false)
    }
  }

  const saveInspector = async () => {
    setSavingInspector(true)
    try {
      const inspector = await api.put<InspectorSettings>('/api/settings/inspector', {
        capture_request_previews: captureRequestPreviews,
        request_preview_max_chars: requestPreviewMaxChars,
      })
      setCaptureRequestPreviews(inspector.capture_request_previews)
      setRequestPreviewMaxChars(inspector.request_preview_max_chars)
      message.success('请求检查设置已保存')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setSavingInspector(false)
    }
  }

  const saveResilience = async () => {
    setSavingResilience(true)
    try {
      const resilience = await api.put<ResilienceSettings>('/api/settings/resilience', {
        stream_recovery_enabled: streamRecoveryEnabled,
        stream_recovery_max_retries: streamRecoveryMaxRetries,
        max_retries: maxRetries,
        retry_backoff_ms: retryBackoffMs,
        retry_max_backoff_ms: retryMaxBackoffMs,
      })
      setStreamRecoveryEnabled(resilience.stream_recovery_enabled)
      setStreamRecoveryMaxRetries(resilience.stream_recovery_max_retries)
      setMaxRetries(resilience.max_retries)
      setRetryBackoffMs(resilience.retry_backoff_ms)
      setRetryMaxBackoffMs(resilience.retry_max_backoff_ms)
      message.success('重试策略已保存')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setSavingResilience(false)
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
            <Descriptions.Item label="数据库连接池">
              {limits
                ? `${limits.db_max_connections} 连接 / 等待 ${limits.db_acquire_timeout_secs} 秒 / 忙等 ${limits.db_busy_timeout_secs} 秒`
                : '-'}
            </Descriptions.Item>
            <Descriptions.Item label="上游空闲超时">
              {limits ? `${limits.upstream_idle_timeout_secs} 秒` : '-'}
            </Descriptions.Item>
            <Descriptions.Item label="全局并发上限">
              {limits ? (limits.max_concurrent_requests > 0 ? limits.max_concurrent_requests : '不限') : '-'}
            </Descriptions.Item>
            <Descriptions.Item label="在途请求体上限">
              {limits ? (limits.max_inflight_request_mib > 0 ? `${limits.max_inflight_request_mib} MiB` : '不限') : '-'}
            </Descriptions.Item>
            <Descriptions.Item label="准入等待">
              {limits ? (limits.admission_wait_ms > 0 ? `${limits.admission_wait_ms} 毫秒` : '不等待') : '-'}
            </Descriptions.Item>
            <Descriptions.Item label="流最长存活 / 请求体读取上限">
              {limits
                ? `${limits.stream_max_secs != null ? `${limits.stream_max_secs} 秒` : '不限'} / ${limits.body_read_timeout_secs != null ? `${limits.body_read_timeout_secs} 秒` : '不限'}`
                : '-'}
            </Descriptions.Item>
            <Descriptions.Item label="请求总预算">
              {limits ? (limits.request_timeout_secs != null ? `${limits.request_timeout_secs} 秒` : '不限') : '-'}
            </Descriptions.Item>
            <Descriptions.Item label="内存压力保护">
              {limits
                ? limits.memory_limit_mib > 0
                  ? `${limits.memory_limit_mib} MiB（${limits.memory_limit_source === 'cgroup' ? '容器限制' : '手动设置'}，达 ${limits.memory_shed_ratio_pct}% 时拒绝新请求）`
                  : '未开启'
                : '-'}
            </Descriptions.Item>
            <Descriptions.Item label="SSE 心跳">
              {limits
                ? limits.sse_keepalive_secs != null
                  ? `${limits.sse_keepalive_secs} 秒`
                  : '关闭'
                : '-'}
            </Descriptions.Item>
            <Descriptions.Item label="请求体 / 上游响应上限">
              {limits ? `${limits.max_request_body_mib} MiB / ${limits.max_upstream_body_mib} MiB` : '-'}
            </Descriptions.Item>
            <Descriptions.Item label="退出等待">
              {limits
                ? limits.shutdown_grace_secs > 0
                  ? `${limits.shutdown_grace_secs} 秒`
                  : '一直等待'
                : '-'}
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
            <Descriptions.Item label="定时维护">
              <Button
                icon={<ReloadOutlined />}
                loading={runningMaintenance}
                onClick={() => void runMaintenance()}
              >
                立即运行
              </Button>
            </Descriptions.Item>
          </Descriptions>
        </Card>
      </div>
      <Card
        className="settings-retention"
        title={<Space><DatabaseOutlined />数据库状态</Space>}
        bordered={false}
      >
        {settings?.database_stats ? (
          <>
            <Descriptions column={2} size="small">
              <Descriptions.Item label="数据库文件" span={2}>
                {settings.database_stats.path ? (
                  <Typography.Text copyable={{ text: settings.database_stats.path }}>
                    {settings.database_stats.path}
                  </Typography.Text>
                ) : (
                  '内存数据库'
                )}
              </Descriptions.Item>
              <Descriptions.Item label="占用空间">
                <Tooltip title={`${formatExact(settings.database_stats.size_bytes)} bytes`}>
                  {formatBytes(settings.database_stats.size_bytes)}
                </Tooltip>
              </Descriptions.Item>
              <Descriptions.Item label="可回收空间">
                <Tooltip title={`${formatExact(settings.database_stats.free_bytes)} bytes`}>
                  {formatBytes(settings.database_stats.free_bytes)}
                </Tooltip>
              </Descriptions.Item>
            </Descriptions>
            <Space wrap style={{ marginTop: 16 }}>
              <Tag>提供商 {formatCompact(settings.database_stats.providers)}</Tag>
              <Tag>模型 {formatCompact(settings.database_stats.provider_models)}</Tag>
              <Tag>上游密钥 {formatCompact(settings.database_stats.provider_api_keys)}</Tag>
              <Tag>路由 {formatCompact(settings.database_stats.routes)}</Tag>
              <Tag>访问密钥 {formatCompact(settings.database_stats.access_keys)}</Tag>
              <Tag>Webhook {formatCompact(settings.database_stats.webhooks)}</Tag>
              <Tag>投递记录 {formatCompact(settings.database_stats.webhook_deliveries)}</Tag>
              <Tag>审计日志 {formatCompact(settings.database_stats.audit_logs)}</Tag>
              <Tag>请求日志 {formatCompact(settings.database_stats.usage_logs)}</Tag>
              <Tag color={settings.database_stats.in_flight_requests ? 'processing' : 'default'}>
                请求中 {formatCompact(settings.database_stats.in_flight_requests)}
              </Tag>
            </Space>
            <Space wrap style={{ marginTop: 16 }}>
              <Popconfirm
                title="整理数据库并回收空间？"
                description="操作期间会短暂锁定数据库写入；数据量较大时可能耗时，建议先下载备份。"
                okText="开始整理"
                cancelText="取消"
                onConfirm={() => void vacuum()}
              >
                <Button
                  icon={<CompressOutlined />}
                  loading={vacuuming}
                  disabled={!settings.database_stats.free_bytes}
                >
                  整理数据库
                </Button>
              </Popconfirm>
            </Space>
          </>
        ) : (
          <Typography.Text type="secondary">加载中</Typography.Text>
        )}
      </Card>
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
      <Card
        className="settings-retention"
        title={<Space><SafetyCertificateOutlined />请求防护</Space>}
        bordered={false}
      >
        <Form layout="vertical">
          <Form.Item
            label="阻止词"
            extra="每行一个，大小写不敏感；只扫描提示词文本，不扫描图片和 Base64 内容。"
          >
            <Input.TextArea
              rows={5}
              value={blockedTerms}
              onChange={(event) => setBlockedTerms(event.target.value)}
              placeholder={'例如：\ninternal-secret\n禁止分享的客户名'}
            />
          </Form.Item>
          <Form.Item
            label="最大提示词 Token"
            extra="按网关估算值在路由前拦截；留空或设为 0 表示不限制。"
          >
            <InputNumber
              min={0}
              max={10000000}
              precision={0}
              value={maxPromptTokens}
              onChange={(value) => setMaxPromptTokens(value ?? null)}
              placeholder="不限制"
              addonAfter="Token"
              style={{ width: 220 }}
            />
          </Form.Item>
          <Button
            type="primary"
            icon={<CheckCircleOutlined />}
            loading={savingGuardrails}
            onClick={() => void saveGuardrails()}
          >
            保存请求防护
          </Button>
        </Form>
      </Card>
      <Card
        className="settings-retention"
        title={<Space><FileSearchOutlined />请求检查</Space>}
        bordered={false}
      >
        <Form layout="vertical">
          <Form.Item
            label="捕获请求内容"
            extra="默认关闭。开启后可在请求详情中查看脱敏后的请求体，便于排查上游问题；内容按下方上限截断。"
          >
            <Switch
              checked={captureRequestPreviews}
              onChange={setCaptureRequestPreviews}
            />
          </Form.Item>
          <Form.Item
            label="最大保留字符数"
            extra="范围 256 到 65536，过长内容会截断。"
          >
            <InputNumber
              min={256}
              max={65536}
              precision={0}
              value={requestPreviewMaxChars}
              onChange={(value) => setRequestPreviewMaxChars(value ?? 4000)}
              addonAfter="字符"
              style={{ width: 220 }}
              disabled={!captureRequestPreviews}
            />
          </Form.Item>
          <Button
            type="primary"
            icon={<CheckCircleOutlined />}
            loading={savingInspector}
            onClick={() => void saveInspector()}
          >
            保存请求检查
          </Button>
        </Form>
      </Card>
      <Card
        className="settings-retention"
        title={<Space><SafetyCertificateOutlined />失败重试</Space>}
        bordered={false}
      >
        <Form layout="vertical">
          <Form.Item
            label="流式首包恢复"
            extra="开启后网关只在上游尚未送出任何内容时保留首包窗口（最多 750ms 或 64KiB）；一旦有内容就立即放行，所以正常流式请求不会增加首包延迟。只有 200 之后、内容送达之前就断开的上游才会按同目标重试次数重新请求。"
          >
            <Switch
              checked={streamRecoveryEnabled}
              onChange={setStreamRecoveryEnabled}
            />
          </Form.Item>
          <Form.Item
            label="首包恢复重试次数"
            extra="仅在上面的开关开启时生效，范围 0 到 4，与“同目标重试次数”相互独立，因此关闭普通重试时首包恢复仍然可用。"
          >
            <InputNumber
              min={0}
              max={4}
              precision={0}
              disabled={!streamRecoveryEnabled}
              value={streamRecoveryMaxRetries}
              onChange={(value) => setStreamRecoveryMaxRetries(value ?? 0)}
              addonAfter="次"
              style={{ width: 220 }}
            />
          </Form.Item>
          <Form.Item
            label="同目标重试次数"
            extra="连接错误、超时和可重试 5xx 会先按指数退避重试当前目标，再切换到下一个目标；0 表示关闭。"
          >
            <InputNumber
              min={0}
              max={5}
              precision={0}
              value={maxRetries}
              onChange={(value) => setMaxRetries(value ?? 0)}
              addonAfter="次"
              style={{ width: 220 }}
            />
          </Form.Item>
          <Form.Item
            label="首次退避"
            extra="范围 0 到 60000 毫秒，之后每次重试翻倍。"
          >
            <InputNumber
              min={0}
              max={60000}
              precision={0}
              value={retryBackoffMs}
              onChange={(value) => setRetryBackoffMs(value ?? 0)}
              addonAfter="ms"
              style={{ width: 220 }}
            />
          </Form.Item>
          <Form.Item
            label="退避上限"
            extra="范围 0 到 60000 毫秒，且不得小于首次退避。"
          >
            <InputNumber
              min={0}
              max={60000}
              precision={0}
              value={retryMaxBackoffMs}
              onChange={(value) => setRetryMaxBackoffMs(value ?? 0)}
              addonAfter="ms"
              style={{ width: 220 }}
            />
          </Form.Item>
          <Button
            type="primary"
            icon={<CheckCircleOutlined />}
            loading={savingResilience}
            onClick={() => void saveResilience()}
          >
            保存重试策略
          </Button>
        </Form>
      </Card>
      <Card
        className="settings-retention"
        title={<Space><ClockCircleOutlined />冷却管理</Space>}
        bordered={false}
        extra={
          <Space>
            <Button icon={<ReloadOutlined />} loading={loadingCooldowns} onClick={loadCooldowns}>
              刷新
            </Button>
            <Popconfirm
              title="清除全部冷却？"
              onConfirm={() => void clearCooldowns({ all: true })}
            >
              <Button
                danger
                icon={<ClearOutlined />}
                disabled={!cooldownRows.length}
                loading={clearingCooldowns}
              >
                全部清除
              </Button>
            </Popconfirm>
          </Space>
        }
      >
        <Typography.Paragraph type="secondary">
          冷却只存在于当前进程内，处于冷却的提供商、模型或上游密钥会暂时退出路由。上游已经恢复时可以在这里提前解除。
        </Typography.Paragraph>
        <Table
          rowKey="key"
          size="small"
          loading={loadingCooldowns}
          dataSource={cooldownRows}
          pagination={false}
          locale={{ emptyText: '当前没有冷却中的目标' }}
          columns={[
            {
              title: '范围',
              dataIndex: 'scope',
              width: 100,
              render: (value: string) => <Tag>{value}</Tag>,
            },
            {
              title: '目标',
              dataIndex: 'target',
            },
            {
              title: '备注',
              dataIndex: 'detail',
              width: 160,
              render: (value: string) =>
                value || <Typography.Text type="secondary">-</Typography.Text>,
            },
            {
              title: '剩余',
              dataIndex: 'remaining',
              width: 100,
              render: (value: number) => `${value}s`,
            },
            {
              title: '操作',
              width: 100,
              render: (_, record) => (
                <Button
                  size="small"
                  loading={clearingCooldowns}
                  onClick={() => void clearCooldowns(record.clear)}
                >
                  清除
                </Button>
              ),
            },
          ]}
        />
      </Card>
    </>
  )
}
