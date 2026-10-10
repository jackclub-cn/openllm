import {
  ApiOutlined,
  ClockCircleOutlined,
  DatabaseOutlined,
  DollarOutlined,
  NodeIndexOutlined,
  ThunderboltOutlined,
} from '@ant-design/icons'
import { Col, Row, Tooltip, Typography } from 'antd'
import { useNavigate } from 'react-router-dom'
import type { Overview } from '../../api'
import MetricCard from '../../components/MetricCard'
import { formatCompact } from '../../format'

type DashboardMetricsProps = {
  data: Overview
}

export default function DashboardMetrics({ data }: DashboardMetricsProps) {
  const navigate = useNavigate()

  return (
    <>
      <Row gutter={[16, 16]}>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="请求"
            value={data.range_requests}
            icon={<ThunderboltOutlined />}
            tone="blue"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="总 tokens"
            value={data.range_tokens}
            compact
            icon={<ApiOutlined />}
            tone="cyan"
            hint={`输入 ${formatCompact(data.range_prompt_tokens)} · 输出 ${formatCompact(data.range_completion_tokens)}`}
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="成功率"
            value={data.range_success_rate}
            precision={1}
            suffix="%"
            icon={<NodeIndexOutlined />}
            tone="green"
            hint={
              data.range_gateway_adjusted > 0 ? (
                <Tooltip title="兼容层自动修复工具历史或调整上游请求参数的次数">
                  <Typography.Link onClick={() => navigate('/usage?gateway_adjusted=true')}>
                    网关调整 {data.range_gateway_adjusted} 次
                  </Typography.Link>
                </Tooltip>
              ) : (
                '无兼容调整'
              )
            }
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="平均总用时"
            value={data.range_avg_latency_ms}
            precision={0}
            suffix="ms"
            icon={<ClockCircleOutlined />}
            tone="orange"
          />
        </Col>
      </Row>
      <Row gutter={[16, 16]} className="section-row">
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="缓存命中率"
            value={data.range_cache_hit_rate}
            precision={1}
            suffix="%"
            icon={<DollarOutlined />}
            tone="purple"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="缓存读取"
            value={data.range_cache_read}
            compact
            icon={<DatabaseOutlined />}
            tone="green"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="缓存写入"
            value={data.range_cache_write}
            compact
            icon={<DatabaseOutlined />}
            tone="cyan"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="费用"
            value={data.range_cost_micros / 1_000_000}
            precision={4}
            suffix="USD"
            icon={<DatabaseOutlined />}
            tone="blue"
          />
        </Col>
      </Row>
    </>
  )
}
