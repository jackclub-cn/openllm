import {
  CheckCircleOutlined,
  ClockCircleOutlined,
  CompressOutlined,
  DatabaseOutlined,
  FileSearchOutlined,
  SafetyCertificateOutlined,
} from '@ant-design/icons'
import {
  Alert,
  Button,
  Card,
  Descriptions,
  Form,
  Input,
  InputNumber,
  Popconfirm,
  Space,
  Switch,
  Tag,
  Tooltip,
  Typography,
} from 'antd'
import PageHeader from '../components/PageHeader'
import { formatBytes, formatCompact, formatExact } from '../format'
import useSettings from './settings/useSettings'
import RuntimeSettingsCard from './settings/RuntimeSettingsCard'
import SettingsCooldownsCard from './settings/SettingsCooldownsCard'

export default function SettingsPage({ onSave }: { onSave: (value: string) => void }) {
  const state = useSettings()
  const {
    settings,
    saved,
    setSaved,
    token,
    setToken,
    vacuuming,
    retentionDays,
    setRetentionDays,
    savingRetention,
    blockedTerms,
    setBlockedTerms,
    maxPromptTokens,
    setMaxPromptTokens,
    savingGuardrails,
    captureRequestPreviews,
    setCaptureRequestPreviews,
    requestPreviewMaxChars,
    setRequestPreviewMaxChars,
    savingInspector,
    streamRecoveryEnabled,
    setStreamRecoveryEnabled,
    streamRecoveryMaxRetries,
    setStreamRecoveryMaxRetries,
    maxRetries,
    setMaxRetries,
    retryBackoffMs,
    setRetryBackoffMs,
    retryMaxBackoffMs,
    setRetryMaxBackoffMs,
    savingResilience,
    saveRetention,
    vacuum,
    saveGuardrails,
    saveInspector,
    saveResilience,
  } = state
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
                onChange={(event) => {
                  setToken(event.target.value)
                  setSaved(false)
                }}
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
            {saved && (
              <Typography.Text type="success" className="save-note">
                已保存
              </Typography.Text>
            )}
          </Form>
        </Card>
        <RuntimeSettingsCard state={state} />
      </div>
      <Card
        className="settings-retention"
        title={
          <Space>
            <DatabaseOutlined />
            数据库状态
          </Space>
        }
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
                <Tooltip
                  title={`${formatExact(settings.database_stats.size_bytes)} bytes`}
                >
                  {formatBytes(settings.database_stats.size_bytes)}
                </Tooltip>
              </Descriptions.Item>
              <Descriptions.Item label="可回收空间">
                <Tooltip
                  title={`${formatExact(settings.database_stats.free_bytes)} bytes`}
                >
                  {formatBytes(settings.database_stats.free_bytes)}
                </Tooltip>
              </Descriptions.Item>
            </Descriptions>
            <Space wrap style={{ marginTop: 16 }}>
              <Tag>提供商 {formatCompact(settings.database_stats.providers)}</Tag>
              <Tag>模型 {formatCompact(settings.database_stats.provider_models)}</Tag>
              <Tag>
                上游密钥 {formatCompact(settings.database_stats.provider_api_keys)}
              </Tag>
              <Tag>路由 {formatCompact(settings.database_stats.routes)}</Tag>
              <Tag>访问密钥 {formatCompact(settings.database_stats.access_keys)}</Tag>
              <Tag>Webhook {formatCompact(settings.database_stats.webhooks)}</Tag>
              <Tag>
                投递记录 {formatCompact(settings.database_stats.webhook_deliveries)}
              </Tag>
              <Tag>审计日志 {formatCompact(settings.database_stats.audit_logs)}</Tag>
              <Tag>请求日志 {formatCompact(settings.database_stats.usage_logs)}</Tag>
              <Tag
                color={
                  settings.database_stats.in_flight_requests ? 'processing' : 'default'
                }
              >
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
        title={
          <Space>
            <ClockCircleOutlined />
            日志保留
          </Space>
        }
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
        title={
          <Space>
            <SafetyCertificateOutlined />
            请求防护
          </Space>
        }
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
        title={
          <Space>
            <FileSearchOutlined />
            请求检查
          </Space>
        }
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
          <Form.Item label="最大保留字符数" extra="范围 256 到 65536，过长内容会截断。">
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
        title={
          <Space>
            <SafetyCertificateOutlined />
            失败重试
          </Space>
        }
        bordered={false}
      >
        <Form layout="vertical">
          <Form.Item
            label="流式首包恢复"
            extra="开启后网关只在上游尚未送出任何内容时保留首包窗口（最多 750ms 或 64KiB）；一旦有内容就立即放行，所以正常流式请求不会增加首包延迟。只有 200 之后、内容送达之前就断开的上游才会按同目标重试次数重新请求。"
          >
            <Switch checked={streamRecoveryEnabled} onChange={setStreamRecoveryEnabled} />
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
          <Form.Item label="首次退避" extra="范围 0 到 60000 毫秒，之后每次重试翻倍。">
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
          <Form.Item label="退避上限" extra="范围 0 到 60000 毫秒，且不得小于首次退避。">
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
      <SettingsCooldownsCard state={state} />
    </>
  )
}
