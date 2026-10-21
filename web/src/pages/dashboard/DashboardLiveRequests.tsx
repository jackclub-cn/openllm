import { ClockCircleOutlined, ThunderboltOutlined } from '@ant-design/icons'
import { Alert, Card, Empty, Space, Table, Tag, Tooltip, Typography } from 'antd'
import type { TableColumnsType } from 'antd'
import type { RequestActivity } from '../../api'
import useActivityClock, { activityElapsedMs } from '../../hooks/useActivityClock'

type DashboardLiveRequestsProps = {
  items: RequestActivity[]
  connected: boolean
  error?: string
  activeCount: number
  loading: boolean
  enabled: boolean
  paused: boolean
  onOpen: (item: RequestActivity) => void
}

const phaseLabels: Record<string, string> = {
  admitted: '已接收',
  preparing: '准备请求',
  routing: '选择路由',
  waiting: '等待容量',
  attempting: '连接上游',
  headers_received: '等待首字',
  streaming: '流式生成',
  completed: '完成',
  failed: '失败',
  cancelled: '已取消',
}

function phaseColor(item: RequestActivity) {
  if (item.outcome === 'running') return 'processing'
  if (item.outcome === 'success') return 'success'
  if (item.outcome === 'cancelled') return 'default'
  return 'error'
}

function phaseLabel(item: RequestActivity) {
  return phaseLabels[item.phase] || item.phase
}

function formatDuration(value: number) {
  return value >= 1000 ? `${(value / 1000).toFixed(2)} s` : `${Math.max(0, Math.round(value))} ms`
}

export default function DashboardLiveRequests({
  items,
  connected,
  error,
  activeCount,
  loading,
  enabled,
  paused,
  onOpen,
}: DashboardLiveRequestsProps) {
  const now = useActivityClock(!paused && activeCount > 0)

  const columns: TableColumnsType<RequestActivity> = [
    {
      title: '状态',
      key: 'status',
      width: 110,
      render: (_, item) => (
        <Tag color={phaseColor(item)} icon={item.outcome === 'running' ? <ClockCircleOutlined /> : undefined}>
          {phaseLabel(item)}
        </Tag>
      ),
    },
    {
      title: '模型',
      key: 'model',
      width: 190,
      render: (_, item) => (
        <div>
          <Typography.Text strong>{item.requested_model || '-'}</Typography.Text>
          {item.target?.upstream_model && item.target.upstream_model !== item.requested_model && (
            <div>
              <Typography.Text type="secondary">{item.target.upstream_model}</Typography.Text>
            </div>
          )}
        </div>
      ),
    },
    {
      title: '路由 / 提供商',
      key: 'route',
      width: 210,
      render: (_, item) => (
        <div>
          <Typography.Text>
            {item.route_name || (item.route_id ? `路由 #${item.route_id}` : '直连模型')}
          </Typography.Text>
          <div>
            <Typography.Text type="secondary">
              {item.target
                ? `${item.target.provider_name} #${item.target.provider_id}`
                : item.candidates[0]
                  ? `${item.candidates[0].provider_name || '候选'} #${item.candidates[0].provider_id}`
                  : '尚未选择上游'}
            </Typography.Text>
          </div>
        </div>
      ),
    },
    {
      title: '尝试',
      key: 'attempts',
      width: 110,
      render: (_, item) => (
        <Space size={4} wrap>
          <Tag>{item.attempt_count}</Tag>
          {item.fallback_count > 0 && <Tag color="orange">回退 {item.fallback_count}</Tag>}
        </Space>
      ),
    },
    {
      title: '耗时',
      key: 'elapsed',
      width: 100,
      render: (_, item) => formatDuration(activityElapsedMs(item, now)),
    },
    {
      title: '首字',
      key: 'first_token',
      width: 100,
      render: (_, item) =>
        !item.streamed ? '不适用' : item.timings.time_to_first_token_ms != null
          ? formatDuration(item.timings.time_to_first_token_ms)
          : '-',
    },
    {
      title: '上下文',
      key: 'context',
      width: 130,
      render: (_, item) => (
        <Tooltip title="进行中为请求 Token 估算，完成后以上游用量为准">
          <span>
            {item.prompt_tokens != null
              ? `${item.prompt_tokens} tokens`
              : `~${item.request_tokens_estimated} tokens`}
          </span>
        </Tooltip>
      ),
    },
    {
      title: '请求 ID',
      dataIndex: 'request_id',
      width: 130,
      render: (value: string) => (
        <Typography.Text copyable={{ text: value }}>{value.slice(0, 12)}…</Typography.Text>
      ),
    },
  ]

  return (
    <Card
      className="section-row activity-live-card"
      bordered={false}
      title={
        <Space>
          <ThunderboltOutlined />
          最近请求
          <Tag color={connected && !paused ? 'success' : 'default'}>{paused ? '已暂停' : connected ? '实时' : '轮询'}</Tag>
        </Space>
      }
      extra={
        <Space size={8} wrap>
          <Tag color="processing">{activeCount} 进行中</Tag>
          <Tag color="orange">回退 {items.filter((item) => item.fallback_count > 0).length}</Tag>
          <Tag color="error">错误 {items.filter((item) => item.outcome === 'failed').length}</Tag>
          <Typography.Text type="secondary">保留最近 {items.length} 条</Typography.Text>
        </Space>
      }
    >
      {error && (
        <Alert
          type="warning"
          showIcon
          message="无法刷新实时请求"
          description={error}
          style={{ marginBottom: 12 }}
        />
      )}
      {!enabled ? <Empty description="实时请求记录已关闭" image={Empty.PRESENTED_IMAGE_SIMPLE} /> : <Table
        rowKey="request_id"
        size="small"
        loading={loading}
        dataSource={items}
        pagination={false}
        scroll={{ x: 1080 }}
        onRow={(item) => ({
          onClick: () => onOpen(item),
          style: { cursor: 'pointer' },
        })}
        locale={{
          emptyText: (
            <Empty
              image={Empty.PRESENTED_IMAGE_SIMPLE}
              description="暂无最近请求"
            />
          ),
        }}
        columns={columns}
      />}
      {activeCount > items.filter((item) => item.outcome === 'running').length && (
        <Typography.Text type="secondary">当前列表显示 {items.filter((item) => item.outcome === 'running').length} / {activeCount} 个进行中请求</Typography.Text>
      )}
    </Card>
  )
}
