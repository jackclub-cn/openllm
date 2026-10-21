import { useState } from 'react'
import {
  ClearOutlined,
  ReloadOutlined,
} from '@ant-design/icons'
import {
  App,
  Button,
  Card,
  Popconfirm,
  Space,
  Table,
  Typography,
} from 'antd'
import {
  api,
  formatError,
  type ClearCooldownsResult,
  type DiagnosticSnapshot,
} from '../../api'
import { formatFailureClassification } from '../../format'

type CooldownRow = {
  key: string
  kind: string
  target: string
  remaining: number
  detail: string
  halfOpen?: boolean
  clear: Record<string, unknown>
}

type Props = {
  snapshot?: DiagnosticSnapshot
  loading: boolean
  onRefresh: () => void | Promise<void>
}

export default function DiagnosticCooldowns({
  snapshot,
  loading,
  onRefresh,
}: Props) {
  const { message } = App.useApp()
  const [clearing, setClearing] = useState<string>()

  const clearCooldowns = async (
    body: Record<string, unknown>,
    key: string
  ) => {
    setClearing(key)
    try {
      const result = await api.post<ClearCooldownsResult>(
        '/api/resilience/cooldowns/clear',
        body
      )
      const total =
        result.cleared_provider_cooldowns +
        result.cleared_model_cooldowns +
        result.cleared_provider_key_cooldowns
      message.success(total ? `已清除 ${total} 条冷却` : '没有匹配的冷却')
      await onRefresh()
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setClearing(undefined)
    }
  }

  const rows: CooldownRow[] = [
    ...(snapshot?.resilience.provider_cooldowns || []).map((item) => ({
      key: `provider-${item.provider_id}`,
      kind: '提供商熔断',
      target: item.provider_name || `#${item.provider_id}`,
      remaining: item.remaining_seconds,
      halfOpen: item.state === 'half_open',
      detail: [
        `失败 ${item.failure_streak} 次`,
        item.state === 'open'
          ? '已熔断'
          : item.state === 'half_open'
          ? '探测恢复中'
          : '冷却中',
        formatFailureClassification(item),
      ]
        .filter(Boolean)
        .join(' · '),
      clear: { provider_id: item.provider_id },
    })),
    ...(snapshot?.resilience.model_cooldowns || []).map((item) => ({
      key: `model-${item.provider_id}-${item.model}`,
      kind: '模型冷却',
      target: `${item.provider_name || `#${item.provider_id}`} · ${item.model}`,
      remaining: item.remaining_seconds,
      detail: formatFailureClassification(item) || '该模型暂不参与路由',
      clear: { provider_id: item.provider_id, model: item.model },
    })),
    ...(snapshot?.resilience.provider_key_cooldowns || []).map((item) => ({
      key: `key-${item.provider_key_id}`,
      kind: '上游密钥冷却',
      target: `${item.provider_name || '未知提供商'} · ${
        item.key_name || `#${item.provider_key_id}`
      }`,
      remaining: item.remaining_seconds,
      detail: formatFailureClassification(item) || '该上游密钥暂不参与路由',
      clear: { provider_key_id: item.provider_key_id },
    })),
  ].sort((a, b) => b.remaining - a.remaining)

  return (
    <Card
      title="冷却与熔断"
      extra={
        <Space>
          <Typography.Text type="secondary">
            开环阈值 {snapshot?.resilience.provider_open_threshold ?? '—'} 次
          </Typography.Text>
          <Button
            icon={<ReloadOutlined />}
            loading={loading}
            onClick={() => void onRefresh()}
          >
            刷新
          </Button>
          <Popconfirm
            title="清除全部冷却？"
            onConfirm={() => void clearCooldowns({ all: true }, 'all')}
          >
            <Button
              danger
              icon={<ClearOutlined />}
              loading={clearing === 'all'}
              disabled={!rows.length || loading || Boolean(clearing)}
            >
              全部清除
            </Button>
          </Popconfirm>
        </Space>
      }
    >
      <Typography.Paragraph type="secondary">
        冷却与熔断会暂时将提供商、模型或上游密钥移出路由。熔断恢复后只放行一个探测请求验证上游状态，提前确认上游已恢复时可以在这里解除。
      </Typography.Paragraph>
      <Table<CooldownRow>
        size="small"
        scroll={{ x: 720 }}
        rowKey="key"
        pagination={false}
        loading={loading}
        dataSource={rows}
        locale={{ emptyText: '当前没有处于冷却或熔断状态的对象' }}
        columns={[
          { title: '类型', dataIndex: 'kind', width: 130 },
          { title: '对象', dataIndex: 'target' },
          {
            title: '剩余时间',
            dataIndex: 'remaining',
            width: 110,
            render: (value: number, record) =>
              record.halfOpen ? '待探测' : `${value}s`,
          },
          { title: '说明', dataIndex: 'detail' },
          {
            title: '操作',
            width: 100,
            render: (_, record) => (
              <Popconfirm
                title="清除这条冷却？"
                onConfirm={() => void clearCooldowns(record.clear, record.key)}
              >
                <Button
                  size="small"
                  icon={<ClearOutlined />}
                  loading={clearing === record.key}
                  disabled={loading || Boolean(clearing)}
                >
                  清除
                </Button>
              </Popconfirm>
            ),
          },
        ]}
      />
    </Card>
  )
}
