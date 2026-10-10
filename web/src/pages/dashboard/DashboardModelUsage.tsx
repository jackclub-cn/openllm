import { Card, Progress, Table, Tooltip, Typography } from 'antd'
import { useNavigate } from 'react-router-dom'
import type { Overview } from '../../api'
import { formatCompact, formatCostMicros, formatExact } from '../../format'

type DashboardModelUsageProps = {
  data: Overview
}

export default function DashboardModelUsage({ data }: DashboardModelUsageProps) {
  const navigate = useNavigate()

  return (
    <Card title="模型用量" bordered={false} className="section-row">
      <Table
        rowKey="model"
        size="middle"
        pagination={false}
        dataSource={data.model_usage}
        onRow={(record) => ({
          onClick: () => navigate(`/usage?model=${encodeURIComponent(record.model)}`),
          style: { cursor: 'pointer' },
        })}
        locale={{ emptyText: '暂无模型用量' }}
        scroll={{ x: 900 }}
        columns={[
          {
            title: '模型',
            dataIndex: 'model',
            render: (value: string) => <Typography.Text strong>{value}</Typography.Text>,
          },
          { title: '请求数', dataIndex: 'requests', width: 100 },
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
            title: '成功率',
            dataIndex: 'success_rate',
            width: 160,
            render: (value: number) => (
              <Progress
                percent={Math.round(value)}
                size="small"
                status={value >= 99 ? 'success' : value >= 90 ? 'normal' : 'exception'}
              />
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
