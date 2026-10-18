import { Card, Col, Empty, Row, Table, Tag, Tooltip, Typography } from 'antd'
import type { Overview } from '../../api'
import { formatExact } from '../../format'

const ADJUSTMENT_LABELS: Record<string, string> = {
  tool_history: '工具历史修复',
  compat: '参数调整',
  budget: '预算排除',
  pool: '提供商池分配',
  other: '其他',
}

function statusTone(statusCode: number) {
  if (statusCode >= 500) return 'error'
  if (statusCode >= 400) return 'warning'
  if (statusCode >= 200 && statusCode < 300) return 'success'
  return 'default'
}

export default function AnalyticsErrors({ data }: { data: Overview }) {
  return (
    <Row gutter={[16, 16]} className="section-row">
      <Col xs={24} xl={8}>
        <Card title="状态码分布" bordered={false} className="full-height-card">
          {data.status_breakdown.length ? (
            <Table
              rowKey="status_code"
              size="small"
              pagination={false}
              dataSource={data.status_breakdown}
              columns={[
                {
                  title: '状态码',
                  dataIndex: 'status_code',
                  render: (value: number) => (
                    <Tag color={statusTone(value)}>{value}</Tag>
                  ),
                },
                {
                  title: '请求数',
                  dataIndex: 'count',
                  align: 'right',
                  render: (value: number) => formatExact(value),
                },
              ]}
            />
          ) : (
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无请求数据" />
          )}
        </Card>
      </Col>
      <Col xs={24} xl={8}>
        <Card title="失败原因" bordered={false} className="full-height-card">
          {data.error_reasons.length ? (
            <Table
              rowKey="label"
              size="small"
              pagination={false}
              dataSource={data.error_reasons}
              columns={[
                {
                  title: '原因',
                  dataIndex: 'label',
                  render: (value: string) => (
                    <Typography.Text
                      ellipsis={{ tooltip: value }}
                      style={{ maxWidth: 260 }}
                    >
                      {value}
                    </Typography.Text>
                  ),
                },
                {
                  title: '次数',
                  dataIndex: 'count',
                  width: 80,
                  align: 'right',
                  render: (value: number) => formatExact(value),
                },
              ]}
            />
          ) : (
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无失败请求" />
          )}
        </Card>
      </Col>
      <Col xs={24} xl={8}>
        <Card
          title={
            <Tooltip title="每条调整请求归入一个主要类别">
              <span>网关调整</span>
            </Tooltip>
          }
          bordered={false}
          className="full-height-card"
        >
          {data.gateway_adjustments.length ? (
            <Table
              rowKey="category"
              size="small"
              pagination={false}
              dataSource={data.gateway_adjustments}
              columns={[
                {
                  title: '类别',
                  dataIndex: 'category',
                  render: (value: string) => ADJUSTMENT_LABELS[value] ?? value,
                },
                {
                  title: '请求数',
                  dataIndex: 'count',
                  width: 90,
                  align: 'right',
                  render: (value: number) => formatExact(value),
                },
              ]}
            />
          ) : (
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="无网关调整" />
          )}
        </Card>
      </Col>
    </Row>
  )
}
