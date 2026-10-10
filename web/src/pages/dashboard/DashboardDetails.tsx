import { Card, Col, Progress, Row, Space, Table, Tag, Tooltip, Typography } from 'antd'
import dayjs from 'dayjs'
import { useNavigate } from 'react-router-dom'
import type { Overview } from '../../api'
import { formatCompact, formatCostMicros, formatExact } from '../../format'

type DashboardDetailsProps = {
  data: Overview
}

export default function DashboardDetails({ data }: DashboardDetailsProps) {
  const navigate = useNavigate()

  return (
    <Row gutter={[16, 16]} className="section-row">
      <Col xs={24} xl={16}>
        <Card title="最近请求" bordered={false} className="full-height-card">
          <Table
            rowKey="id"
            size="middle"
            pagination={false}
            dataSource={data.recent_requests}
            onRow={(record) => ({
              onClick: () =>
                navigate(`/usage?request_id=${encodeURIComponent(record.request_id)}`),
              style: { cursor: 'pointer' },
            })}
            locale={{ emptyText: '暂无请求' }}
            columns={[
              {
                title: '模型',
                dataIndex: 'requested_model',
                render: (value: string, record) => (
                  <div>
                    <Typography.Text strong>{value}</Typography.Text>
                    {record.upstream_model && (
                      <div>
                        <Typography.Text type="secondary">
                          {record.upstream_model}
                        </Typography.Text>
                      </div>
                    )}
                  </div>
                ),
              },
              {
                title: '状态',
                dataIndex: 'success',
                width: 92,
                render: (value: boolean, record) =>
                  record.in_flight ? (
                    <Tag color="processing">请求中</Tag>
                  ) : (
                    <Tag color={value ? 'success' : 'error'}>
                      {value ? '成功' : record.status_code}
                    </Tag>
                  ),
              },
              {
                title: '输入',
                dataIndex: 'prompt_tokens',
                width: 80,
                render: (value: number, record) =>
                  record.in_flight ? (
                    <Typography.Text type="secondary">-</Typography.Text>
                  ) : (
                    <Tooltip title={formatExact(value)}>{formatCompact(value)}</Tooltip>
                  ),
              },
              {
                title: '输出',
                dataIndex: 'completion_tokens',
                width: 80,
                render: (value: number, record) =>
                  record.in_flight ? (
                    <Typography.Text type="secondary">-</Typography.Text>
                  ) : (
                    <Tooltip title={formatExact(value)}>{formatCompact(value)}</Tooltip>
                  ),
              },
              {
                title: '费用',
                dataIndex: 'estimated_cost_micros',
                width: 90,
                render: (value: number | null, record) =>
                  record.in_flight ? (
                    <Typography.Text type="secondary">-</Typography.Text>
                  ) : (
                    formatCostMicros(value)
                  ),
              },
              {
                title: '总用时',
                dataIndex: 'latency_ms',
                width: 100,
                render: (value: number, record) =>
                  record.in_flight ? (
                    <Typography.Text type="secondary">-</Typography.Text>
                  ) : (
                    `${value} ms`
                  ),
              },
              {
                title: '时间',
                dataIndex: 'created_at',
                width: 170,
                render: (value: string) => dayjs(value).format('MM-DD HH:mm:ss'),
              },
            ]}
          />
        </Card>
      </Col>
      <Col xs={24} xl={8}>
        <Card title="网关配置" bordered={false} className="full-height-card">
          <Space direction="vertical" size={24} style={{ width: '100%' }}>
            <div>
              <Typography.Text type="secondary">活跃提供商</Typography.Text>
              <div className="stat-line">
                <strong>{data.active_providers}</strong> 个
              </div>
            </div>
            <div>
              <Typography.Text type="secondary">活跃路由</Typography.Text>
              <div className="stat-line">
                <strong>{data.active_routes}</strong> 条
              </div>
            </div>
            <div>
              <Typography.Text type="secondary">当前请求</Typography.Text>
              <div className="stat-line">
                <strong>{data.in_flight_requests}</strong> 条
              </div>
            </div>
            <div>
              <Typography.Text type="secondary">提供商健康</Typography.Text>
              <div className="stat-line">
                正常{' '}
                <Typography.Link onClick={() => navigate('/providers?health=healthy')}>
                  <strong>{data.healthy_providers}</strong>
                </Typography.Link>{' '}
                · 异常{' '}
                <Typography.Link onClick={() => navigate('/providers?health=failed')}>
                  <strong>{data.failed_providers}</strong>
                </Typography.Link>{' '}
                · 未检测{' '}
                <Typography.Link onClick={() => navigate('/providers?health=untested')}>
                  <strong>{data.untested_providers}</strong>
                </Typography.Link>
              </div>
            </div>
            <div>
              <Typography.Text type="secondary">上游密钥健康</Typography.Text>
              <div className="stat-line">
                {data.provider_keys_total > 0 ? (
                  <>
                    正常 <strong>{data.healthy_provider_keys}</strong> · 异常{' '}
                    <Typography.Link
                      onClick={() => navigate('/providers?health=key_error')}
                    >
                      <strong>{data.failed_provider_keys}</strong>
                    </Typography.Link>{' '}
                    · 未检测{' '}
                    <Typography.Link
                      onClick={() => navigate('/providers?health=key_untested')}
                    >
                      <strong>{data.untested_provider_keys}</strong>
                    </Typography.Link>
                    {data.runtime_error_provider_keys > 0 && (
                      <>
                        {' '}
                        · 运行错误{' '}
                        <Typography.Link
                          onClick={() => navigate('/providers?health=key_error')}
                        >
                          <strong>{data.runtime_error_provider_keys}</strong>
                        </Typography.Link>
                      </>
                    )}
                  </>
                ) : (
                  '未配置'
                )}
              </div>
            </div>
            {data.cooling_provider_keys > 0 && (
              <div>
                <Typography.Text type="secondary">冷却中的上游密钥</Typography.Text>
                <div className="stat-line">
                  <strong>{data.cooling_provider_keys}</strong> 把
                </div>
              </div>
            )}
            {data.cooling_providers > 0 && (
              <div>
                <Typography.Text type="secondary">冷却中的提供商</Typography.Text>
                <div className="stat-line">
                  <strong>{data.cooling_providers}</strong> 个
                </div>
              </div>
            )}
            <div>
              <Typography.Text type="secondary">累计请求</Typography.Text>
              <div className="stat-line">
                <strong>{data.requests_total}</strong> 次
              </div>
            </div>
            <div>
              <Typography.Text type="secondary">累计输入 tokens</Typography.Text>
              <div className="stat-line">
                <Tooltip title={formatExact(data.prompt_tokens_total)}>
                  <strong>{formatCompact(data.prompt_tokens_total)}</strong>
                </Tooltip>
              </div>
            </div>
            <div>
              <Typography.Text type="secondary">累计输出 tokens</Typography.Text>
              <div className="stat-line">
                <Tooltip title={formatExact(data.completion_tokens_total)}>
                  <strong>{formatCompact(data.completion_tokens_total)}</strong>
                </Tooltip>
              </div>
            </div>
            <div>
              <Typography.Text type="secondary">累计缓存读取</Typography.Text>
              <div className="stat-line">
                <Tooltip title={formatExact(data.cache_read_total)}>
                  <strong>{formatCompact(data.cache_read_total)}</strong>
                </Tooltip>
              </div>
            </div>
            <div>
              <Typography.Text type="secondary">累计费用</Typography.Text>
              <div className="stat-line">
                <strong>{formatCostMicros(data.cost_total_micros)}</strong>
                {data.unpriced_total > 0 && (
                  <Typography.Text type="secondary">
                    {' '}
                    {data.unpriced_total} 条未定价
                  </Typography.Text>
                )}
              </div>
            </div>
            <Progress
              percent={Math.round(data.range_success_rate)}
              status="active"
              strokeColor="#52c41a"
            />
          </Space>
        </Card>
      </Col>
    </Row>
  )
}
