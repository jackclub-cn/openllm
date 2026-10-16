import {
  ClockCircleOutlined,
  DatabaseOutlined,
  DollarOutlined,
  NodeIndexOutlined,
  ThunderboltOutlined,
  ApiOutlined,
} from '@ant-design/icons'
import { Col, Row, Tooltip, Typography } from 'antd'
import { useNavigate } from 'react-router-dom'
import type { Overview } from '../../api'
import MetricCard from '../../components/MetricCard'
import { formatCompact, formatCostMicros } from '../../format'

type AnalyticsMetricsProps = {
  data: Overview
}

export default function AnalyticsMetrics({ data }: AnalyticsMetricsProps) {
  const navigate = useNavigate()
  const sessionsHint =
    data.range_sessions > 0
      ? `会话 ${formatCompact(data.range_sessions)} · 每会话 ${data.range_avg_requests_per_session.toFixed(1)} 次`
      : '暂无会话数据'

  return (
    <Row gutter={[16, 16]}>
      <Col xs={24} sm={12} xl={8}>
        <MetricCard
          label="请求数"
          value={data.range_requests}
          icon={<ThunderboltOutlined />}
          tone="blue"
          hint={sessionsHint}
        />
      </Col>
      <Col xs={24} sm={12} xl={8}>
        <MetricCard
          label="总 tokens"
          value={data.range_tokens}
          compact
          icon={<ApiOutlined />}
          tone="cyan"
          hint={`输入 ${formatCompact(data.range_prompt_tokens)} · 输出 ${formatCompact(data.range_completion_tokens)}`}
        />
      </Col>
      <Col xs={24} sm={12} xl={8}>
        <MetricCard
          label="费用"
          value={data.range_cost_micros / 1_000_000}
          precision={4}
          suffix="USD"
          icon={<DollarOutlined />}
          tone="purple"
          hint={
            data.range_unpriced > 0
              ? `${formatCostMicros(data.range_cost_micros)} · ${data.range_unpriced} 条未定价`
              : formatCostMicros(data.range_cost_micros)
          }
        />
      </Col>
      <Col xs={24} sm={12} xl={8}>
        <MetricCard
          label="成功率"
          value={data.range_success_rate}
          precision={1}
          suffix="%"
          icon={<NodeIndexOutlined />}
          tone="green"
          hint={
            data.range_gateway_adjusted > 0 ? (
              <Tooltip title="网关自动修复工具历史或调整上游请求参数的次数">
                <Typography.Link
                  onClick={() => navigate('/usage?gateway_adjusted=true')}
                >
                  网关调整 {data.range_gateway_adjusted} 次
                </Typography.Link>
              </Tooltip>
            ) : (
              '无网关调整'
            )
          }
        />
      </Col>
      <Col xs={24} sm={12} xl={8}>
        <MetricCard
          label="平均总用时"
          value={data.range_avg_latency_ms}
          precision={0}
          suffix="ms"
          icon={<ClockCircleOutlined />}
          tone="orange"
        />
      </Col>
      <Col xs={24} sm={12} xl={8}>
        <MetricCard
          label="缓存命中率"
          value={data.range_cache_hit_rate}
          precision={1}
          suffix="%"
          icon={<DatabaseOutlined />}
          tone="green"
          hint={`读取 ${formatCompact(data.range_cache_read)} · 写入 ${formatCompact(data.range_cache_write)}`}
        />
      </Col>
    </Row>
  )
}
