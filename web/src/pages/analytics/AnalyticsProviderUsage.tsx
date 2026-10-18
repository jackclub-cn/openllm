import { Card, Progress, Space, Table, Tag, Tooltip, Typography } from 'antd'
import { useNavigate } from 'react-router-dom'
import type { Overview } from '../../api'
import { formatCompact, formatCostMicros, formatExact } from '../../format'

type AnalyticsProviderUsageProps = {
  data: Overview
  providerDataMocked?: boolean
}

export default function AnalyticsProviderUsage({
  data,
  providerDataMocked = false,
}: AnalyticsProviderUsageProps) {
  const navigate = useNavigate()
  const totalRequests = data.provider_usage.reduce(
    (sum, record) => sum + record.requests,
    0,
  )

  return (
    <Card
      title={
        <Space size={8}>
          <span>提供商用量</span>
          {providerDataMocked && <Tag color="gold">演示数据</Tag>}
        </Space>
      }
      bordered={false}
    >
      <Table
        rowKey="provider_id"
        size="middle"
        pagination={{ pageSize: 10, hideOnSinglePage: true }}
        dataSource={data.provider_usage}
        onRow={(record) =>
          record.provider_id > 0
            ? {
                onClick: () => navigate(`/usage?provider_id=${record.provider_id}`),
                style: { cursor: 'pointer' },
              }
            : {}
        }
        locale={{ emptyText: '暂无提供商用量' }}
        scroll={{ x: 1040 }}
        columns={[
          {
            title: '提供商',
            dataIndex: 'provider_name',
            render: (value: string) => <Typography.Text strong>{value}</Typography.Text>,
          },
          {
            title: '请求数',
            dataIndex: 'requests',
            width: 110,
            sorter: (left, right) => left.requests - right.requests,
          },
          {
            title: '请求占比',
            width: 150,
            sorter: (left, right) => left.requests - right.requests,
            render: (_, record) => {
              const percent =
                totalRequests > 0 ? (record.requests / totalRequests) * 100 : 0
              return (
                <Tooltip title={`${percent.toFixed(1)}%`}>
                  <Progress
                    percent={Math.round(percent)}
                    size="small"
                    showInfo={false}
                    strokeColor="#1677ff"
                  />
                </Tooltip>
              )
            },
          },
          {
            title: 'tokens',
            dataIndex: 'tokens',
            width: 110,
            sorter: (left, right) => left.tokens - right.tokens,
            render: (value: number, record) => (
              <Tooltip
                title={`输入 ${formatExact(record.prompt_tokens)} / 输出 ${formatExact(record.completion_tokens)}`}
              >
                <span>{formatCompact(value)}</span>
              </Tooltip>
            ),
          },
          {
            title: '费用',
            dataIndex: 'cost_micros',
            width: 110,
            sorter: (left, right) => (left.cost_micros ?? 0) - (right.cost_micros ?? 0),
            render: (value: number | null) => formatCostMicros(value),
          },
          {
            title: '缓存',
            dataIndex: 'cache_hit_rate',
            width: 120,
            sorter: (left, right) => left.cache_hit_rate - right.cache_hit_rate,
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
            width: 110,
            sorter: (left, right) => left.success_rate - right.success_rate,
            render: (value: number, record) =>
              record.requests === 0 ? (
                <Typography.Text type="secondary">无调用</Typography.Text>
              ) : (
                <Tag color={value >= 99 ? 'success' : value >= 90 ? 'warning' : 'error'}>
                  {value.toFixed(1)}%
                </Tag>
              ),
          },
          {
            title: '平均总用时',
            dataIndex: 'avg_latency_ms',
            width: 130,
            sorter: (left, right) => left.avg_latency_ms - right.avg_latency_ms,
            render: (value: number, record) =>
              record.requests === 0 ? '-' : `${Math.round(value)} ms`,
          },
        ]}
      />
    </Card>
  )
}
