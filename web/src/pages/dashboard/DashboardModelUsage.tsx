import { Button, Card, Space, Table, Tag, Tooltip, Typography } from 'antd'
import { useNavigate } from 'react-router-dom'
import type { Overview } from '../../api'
import { formatCompact, formatCostMicros, formatExact } from '../../format'

type DashboardModelUsageProps = {
  data: Overview
  fullBreakdown: boolean
  onToggleFullBreakdown: () => void
  loading?: boolean
}

export default function DashboardModelUsage({
  data,
  fullBreakdown,
  onToggleFullBreakdown,
  loading = false,
}: DashboardModelUsageProps) {
  const navigate = useNavigate()
  const totalRequests = data.model_usage.reduce(
    (sum, record) => sum + record.requests,
    0,
  )

  return (
    <Card
      title={
        <Space size={8}>
          <span>模型用量</span>
          <Button
            size="small"
            type="link"
            loading={loading}
            onClick={onToggleFullBreakdown}
          >
            {fullBreakdown ? '收起长尾' : '显示全部'}
          </Button>
        </Space>
      }
      bordered={false}
      className="section-row"
    >
      <Table
        rowKey="model"
        size="middle"
        pagination={{ pageSize: 20, hideOnSinglePage: true }}
        dataSource={data.model_usage}
        onRow={(record) => ({
          onClick: () => navigate(`/usage?model=${encodeURIComponent(record.model)}`),
          style: { cursor: 'pointer' },
        })}
        locale={{ emptyText: '暂无模型用量' }}
        scroll={{ x: 1050 }}
        columns={[
          {
            title: '模型',
            dataIndex: 'model',
            render: (value: string) => <Typography.Text strong>{value}</Typography.Text>,
          },
          { title: '请求数', dataIndex: 'requests', width: 100 },
          {
            title: '请求占比',
            width: 100,
            sorter: (a, b) => a.requests - b.requests,
            render: (_, record) =>
              totalRequests > 0
                ? `${((record.requests / totalRequests) * 100).toFixed(1)}%`
                : '-',
          },
          {
            title: '输入 tokens',
            dataIndex: 'prompt_tokens',
            width: 120,
            sorter: (a, b) => a.prompt_tokens - b.prompt_tokens,
            render: (value: number) => (
              <Tooltip title={formatExact(value)}>{formatCompact(value)}</Tooltip>
            ),
          },
          {
            title: '输出 tokens',
            dataIndex: 'completion_tokens',
            width: 120,
            sorter: (a, b) => a.completion_tokens - b.completion_tokens,
            render: (value: number) => (
              <Tooltip title={formatExact(value)}>{formatCompact(value)}</Tooltip>
            ),
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
            width: 120,
            sorter: (a, b) => a.cache_hit_rate - b.cache_hit_rate,
            render: (value: number, record) => (
              <Tooltip
                title={`读取 ${formatExact(record.cache_read)} · 写入 ${formatExact(record.cache_write)}`}
              >
                <span>{value.toFixed(1)}%</span>
              </Tooltip>
            ),
          },
          {
            title: '成功率',
            dataIndex: 'success_rate',
            width: 160,
            render: (value: number) => (
              <Tag color={value >= 99 ? 'success' : value >= 90 ? 'warning' : 'error'}>
                {value.toFixed(1)}%
              </Tag>
            ),
          },
          {
            title: '平均总用时',
            dataIndex: 'avg_latency_ms',
            width: 120,
            sorter: (a, b) => a.avg_latency_ms - b.avg_latency_ms,
            render: (value: number) => `${Math.round(value)} ms`,
          },
        ]}
      />
    </Card>
  )
}
