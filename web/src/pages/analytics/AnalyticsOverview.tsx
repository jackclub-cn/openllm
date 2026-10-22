import { ApiOutlined } from '@ant-design/icons'
import { Card, Col, Empty, Progress, Row, Space, Tag, Typography } from 'antd'
import { useNavigate } from 'react-router-dom'
import type { Overview } from '../../api'
import { formatCompact, formatCostMicros } from '../../format'
import DashboardCharts from '../dashboard/DashboardCharts'

export default function AnalyticsOverview({
  data,
  providerDataMocked,
}: {
  data: Overview
  providerDataMocked: boolean
}) {
  const navigate = useNavigate()
  const totalRequests = data.provider_usage.reduce(
    (sum, provider) => sum + provider.requests,
    0,
  )

  return (
    <div className="analytics-overview">
      {data.provider_usage.length > 0 && (
        <>
          <Space size={8} className="analytics-subheading">
            <Typography.Text strong>提供商</Typography.Text>
            {providerDataMocked && <Tag color="gold">演示数据</Tag>}
          </Space>
          <Row gutter={[16, 16]}>
            {data.provider_usage.slice(0, 4).map((provider) => {
              const share =
                totalRequests > 0 ? (provider.requests / totalRequests) * 100 : 0
              return (
                <Col xs={24} sm={12} xl={6} key={provider.provider_id}>
                  <Card
                    size="small"
                    bordered={false}
                    className="analytics-provider-card"
                    onClick={() => {
                      if (provider.provider_id > 0) {
                        navigate(`/usage?provider_id=${provider.provider_id}`)
                      }
                    }}
                  >
                    <div className="analytics-provider-title">
                      <span className="analytics-provider-icon">
                        <ApiOutlined />
                      </span>
                      <Typography.Text strong ellipsis={{ tooltip: provider.provider_name }}>
                        {provider.provider_name}
                      </Typography.Text>
                      <Tag color={provider.success_rate >= 99 ? 'success' : 'warning'}>
                        {provider.success_rate.toFixed(1)}%
                      </Tag>
                    </div>
                    <div className="analytics-provider-values">
                      <span>
                        <Typography.Text type="secondary">请求</Typography.Text>
                        <strong>{formatCompact(provider.requests)}</strong>
                      </span>
                      <span>
                        <Typography.Text type="secondary">Token</Typography.Text>
                        <strong>{formatCompact(provider.tokens)}</strong>
                      </span>
                      <span>
                        <Typography.Text type="secondary">费用</Typography.Text>
                        <strong>{formatCostMicros(provider.cost_micros)}</strong>
                      </span>
                    </div>
                    <Progress
                      percent={share}
                      size="small"
                      showInfo={false}
                      strokeColor="#1677ff"
                    />
                  </Card>
                </Col>
              )
            })}
          </Row>
        </>
      )}
      {data.provider_usage.length === 0 && (
        <Card bordered={false} className="section-row">
          <Empty
            image={Empty.PRESENTED_IMAGE_SIMPLE}
            description="当前范围暂无提供商调用"
          />
        </Card>
      )}
      <DashboardCharts data={data} providerDataMocked={providerDataMocked} />
    </div>
  )
}
