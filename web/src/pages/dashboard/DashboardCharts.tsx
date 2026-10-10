import { Area } from '@ant-design/plots'
import { Card, Col, Empty, Row, Segmented, Table, Tag, Tooltip, Typography } from 'antd'
import { useNavigate } from 'react-router-dom'
import type { Overview } from '../../api'
import { formatCompact, formatCostMicros, formatExact } from '../../format'

export type DashboardChartMetric =
  | 'requests'
  | 'tokens'
  | 'prompt_tokens'
  | 'completion_tokens'

type DashboardChartsProps = {
  data: Overview
  metric: DashboardChartMetric
  onMetricChange: (metric: DashboardChartMetric) => void
}

export default function DashboardCharts({
  data,
  metric,
  onMetricChange,
}: DashboardChartsProps) {
  const navigate = useNavigate()

  return (
    <Row gutter={[16, 16]} className="section-row">
      <Col xs={24} xl={15}>
        <Card title="用量趋势" bordered={false}>
          <Segmented
            block
            size="small"
            value={metric}
            onChange={(value) => onMetricChange(value as DashboardChartMetric)}
            options={[
              { label: '请求数', value: 'requests' },
              { label: '总 tokens', value: 'tokens' },
              { label: '输入 tokens', value: 'prompt_tokens' },
              { label: '输出 tokens', value: 'completion_tokens' },
            ]}
            style={{ marginBottom: 16 }}
          />
          {data.daily_usage.length ? (
            <Area
              data={data.daily_usage}
              xField="day"
              yField={metric}
              height={260}
              axis={{ x: { title: false }, y: { title: false } }}
              style={{ fill: 'linear-gradient(-90deg, white 0%, #1677ff 100%)' }}
              line={{ style: { stroke: '#1677ff', strokeWidth: 2 } }}
            />
          ) : (
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无请求数据" />
          )}
        </Card>
      </Col>
      <Col xs={24} xl={9}>
        <Card title="提供商使用分布" bordered={false} className="full-height-card">
          {data.provider_usage.length ? (
            <Table
              rowKey="provider_id"
              size="small"
              pagination={false}
              dataSource={data.provider_usage}
              onRow={(record) => ({
                onClick: () => navigate(`/usage?provider_id=${record.provider_id}`),
                style: { cursor: 'pointer' },
              })}
              columns={[
                {
                  title: '提供商',
                  dataIndex: 'provider_name',
                  ellipsis: true,
                },
                { title: '请求', dataIndex: 'requests', width: 70 },
                {
                  title: 'tokens',
                  dataIndex: 'tokens',
                  width: 82,
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
                  width: 86,
                  render: (value: number | null) => formatCostMicros(value),
                },
                {
                  title: '成功率',
                  dataIndex: 'success_rate',
                  width: 90,
                  render: (value: number, record) =>
                    record.requests === 0 ? (
                      <Typography.Text type="secondary">无调用</Typography.Text>
                    ) : (
                      <Tag
                        color={value >= 99 ? 'success' : value >= 90 ? 'warning' : 'error'}
                      >
                        {value.toFixed(1)}%
                      </Tag>
                    ),
                },
                {
                  title: '平均总用时',
                  dataIndex: 'avg_latency_ms',
                  width: 90,
                  render: (value: number, record) =>
                    record.requests === 0 ? '-' : `${Math.round(value)} ms`,
                },
              ]}
            />
          ) : (
            <Empty
              image={Empty.PRESENTED_IMAGE_SIMPLE}
              description="请先添加上游提供商"
            />
          )}
        </Card>
      </Col>
    </Row>
  )
}
