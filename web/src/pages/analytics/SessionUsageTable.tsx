import { Empty, Table, Tag, Tooltip, Typography } from 'antd'
import dayjs, { type Dayjs } from 'dayjs'
import { useNavigate } from 'react-router-dom'
import type { SessionUsage } from '../../api'
import { formatCompact, formatCostMicros, formatExact } from '../../format'

export default function SessionUsageTable({
  sessions,
  loading,
  dates,
}: {
  sessions: SessionUsage[]
  loading: boolean
  dates: [Dayjs, Dayjs]
}) {
  const navigate = useNavigate()

  return (
    <Table
      rowKey="session_id"
      size="middle"
      loading={loading}
      pagination={false}
      dataSource={sessions}
      onRow={(record) => ({
        onClick: () => {
          const params = new URLSearchParams({
            session_id: record.session_id,
            from: dates[0].startOf('day').toISOString(),
            to: dates[1].add(1, 'day').startOf('day').toISOString(),
          })
          navigate(`/usage?${params}`)
        },
        style: { cursor: 'pointer' },
      })}
      locale={{
        emptyText: (
          <Empty
            image={Empty.PRESENTED_IMAGE_SIMPLE}
            description="当前筛选下暂无会话标识"
          />
        ),
      }}
      scroll={{ x: 920 }}
      columns={[
        {
          title: '会话',
          dataIndex: 'session_id',
          render: (value: string) => (
            <Typography.Text strong ellipsis={{ tooltip: value }}>
              {value}
            </Typography.Text>
          ),
        },
        { title: '请求', dataIndex: 'requests', width: 90 },
        {
          title: 'Token',
          dataIndex: 'tokens',
          width: 105,
          render: (value: number) => (
            <Tooltip title={formatExact(value)}>{formatCompact(value)}</Tooltip>
          ),
        },
        {
          title: '输入 / 输出',
          width: 155,
          render: (_: unknown, record: SessionUsage) =>
            `${formatCompact(record.prompt_tokens)} / ${formatCompact(record.completion_tokens)}`,
        },
        {
          title: '费用',
          dataIndex: 'cost_micros',
          width: 100,
          render: (value: number | null) => formatCostMicros(value),
        },
        {
          title: '缓存',
          dataIndex: 'cache_hit_rate',
          width: 100,
          render: (value: number) => `${value.toFixed(1)}%`,
        },
        {
          title: '成功率',
          dataIndex: 'success_rate',
          width: 100,
          render: (value: number) => (
            <Tag color={value >= 99 ? 'success' : value >= 90 ? 'warning' : 'error'}>
              {value.toFixed(1)}%
            </Tag>
          ),
        },
        {
          title: '最近活动',
          dataIndex: 'last_seen',
          width: 145,
          render: (value: string) => dayjs(value).format('MM-DD HH:mm:ss'),
        },
      ]}
    />
  )
}
