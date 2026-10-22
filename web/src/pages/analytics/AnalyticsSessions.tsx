import { useCallback, useEffect, useState } from 'react'
import { BarChartOutlined, ReloadOutlined } from '@ant-design/icons'
import { Column } from '@ant-design/plots'
import {
  App,
  Button,
  Card,
  Col,
  Empty,
  Progress,
  Row,
  Statistic,
  Typography,
} from 'antd'
import type { Dayjs } from 'dayjs'
import { useNavigate } from 'react-router-dom'
import {
  api,
  formatError,
  type AnalyticsDimensions as AnalyticsDimensionsData,
  type AnalyticsGranularity,
  type Overview,
  type SessionUsage,
} from '../../api'
import type { UsageDimensionFilterValues } from '../../components/UsageDimensionFilters'
import { formatCompact, formatCostMicros, formatExact } from '../../format'
import { buildAnalyticsParams } from './queryParams'
import SessionUsageTable from './SessionUsageTable'

export default function AnalyticsSessions({
  data,
  dates,
  filters,
  granularity,
}: {
  data: Overview
  dates: [Dayjs, Dayjs]
  filters: UsageDimensionFilterValues
  granularity: AnalyticsGranularity
}) {
  const { message } = App.useApp()
  const navigate = useNavigate()
  const [sessions, setSessions] = useState<SessionUsage[]>([])
  const [loading, setLoading] = useState(false)

  const load = useCallback(async () => {
    setLoading(true)
    try {
      const params = buildAnalyticsParams(dates, filters, granularity)
      const dimensions = await api.get<AnalyticsDimensionsData>(
        `/api/analytics/dimensions?${params}`,
      )
      setSessions(dimensions.session_usage)
    } catch (reason) {
      message.error(formatError(reason))
    } finally {
      setLoading(false)
    }
  }, [dates, filters, granularity, message])

  useEffect(() => {
    const timer = window.setTimeout(() => void load(), 250)
    return () => window.clearTimeout(timer)
  }, [load])

  const trend = data.daily_usage.map((row) => ({
    bucket: row.day,
    tokens: row.tokens,
    requests: row.requests,
  }))
  const totalModelRequests = data.model_usage.reduce(
    (sum, model) => sum + model.requests,
    0,
  )

  return (
    <>
      <Card
        bordered={false}
        className="section-row"
        title="会话概览"
        extra={
          <Button
            size="small"
            icon={<ReloadOutlined />}
            loading={loading}
            onClick={() => void load()}
          >
            刷新
          </Button>
        }
      >
        <Row gutter={[24, 16]}>
          <Col xs={12} md={8} xl={4}>
            <Statistic title="会话数" value={data.range_sessions} />
          </Col>
          <Col xs={12} md={8} xl={4}>
            <Statistic
              title="Token"
              value={formatCompact(data.range_tokens)}
            />
          </Col>
          <Col xs={12} md={8} xl={4}>
            <Statistic title="费用" value={formatCostMicros(data.range_cost_micros)} />
          </Col>
          <Col xs={12} md={8} xl={4}>
            <Statistic
              title="缓存读取"
              value={formatCompact(data.range_cache_read)}
            />
          </Col>
          <Col xs={12} md={8} xl={4}>
            <Statistic
              title="会话缓存"
              value={data.range_session_cache_hit_rate}
              precision={1}
              suffix="%"
            />
          </Col>
          <Col xs={12} md={8} xl={4}>
            <Statistic
              title="每会话请求"
              value={data.range_avg_requests_per_session}
              precision={1}
            />
          </Col>
        </Row>
      </Card>

      <Row gutter={[16, 16]} className="section-row">
        <Col xs={24} xl={15}>
          <Card
            bordered={false}
            className="full-height-card"
            title={
              <span>
                <BarChartOutlined /> 每日 Token
              </span>
            }
          >
            {trend.length > 0 ? (
              <Column
                data={trend}
                xField="bucket"
                yField="tokens"
                height={280}
                axis={{
                  x: {
                    title: false,
                    labelAutoHide: true,
                    labelAutoRotate: false,
                    labelFormatter: (value: string) =>
                      data.granularity === 'hour'
                        ? value.slice(11, 16)
                        : value.slice(5),
                  },
                  y: {
                    title: false,
                    labelFormatter: (value: number) => formatCompact(value),
                  },
                }}
                style={{ fill: '#1677ff', radiusTopLeft: 3, radiusTopRight: 3 }}
                tooltip={{
                  items: [
                    {
                      channel: 'y',
                      name: 'Token',
                      valueFormatter: (value: number) => formatExact(value),
                    },
                  ],
                }}
              />
            ) : (
              <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无会话趋势" />
            )}
          </Card>
        </Col>
        <Col xs={24} xl={9}>
          <Card bordered={false} className="full-height-card" title="模型分布">
            {data.model_usage.length > 0 ? (
              <div className="session-model-list">
                {data.model_usage.slice(0, 6).map((model) => {
                  const share =
                    totalModelRequests > 0
                      ? (model.requests / totalModelRequests) * 100
                      : 0
                  return (
                    <button
                      type="button"
                      className="session-model-row"
                      key={model.model}
                      onClick={() =>
                        navigate(`/usage?model=${encodeURIComponent(model.model)}`)
                      }
                    >
                      <div>
                        <Typography.Text strong ellipsis={{ tooltip: model.model }}>
                          {model.model}
                        </Typography.Text>
                        <span>
                          {formatCompact(model.tokens)} Token ·{' '}
                          {formatCostMicros(model.cost_micros)}
                        </span>
                      </div>
                      <Progress
                        percent={share}
                        showInfo={false}
                        size="small"
                        strokeColor="#1677ff"
                      />
                    </button>
                  )
                })}
              </div>
            ) : (
              <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="暂无模型用量" />
            )}
          </Card>
        </Col>
      </Row>

      <Card bordered={false} className="section-row" title="最耗会话">
        <SessionUsageTable
          sessions={sessions}
          loading={loading}
          dates={dates}
        />
      </Card>
    </>
  )
}
