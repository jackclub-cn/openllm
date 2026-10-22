import {
  ApiOutlined,
  ClockCircleOutlined,
  DatabaseOutlined,
  DollarOutlined,
  NodeIndexOutlined,
  ThunderboltOutlined,
} from '@ant-design/icons'
import { Col, Row } from 'antd'
import type { Overview } from '../../api'
import MetricCard from '../../components/MetricCard'
import { formatCompact } from '../../format'

type DashboardMetricsProps = {
  data: Overview
}

export default function DashboardMetrics({ data }: DashboardMetricsProps) {
  return (
    <>
      <Row gutter={[16, 16]}>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="今日请求"
            value={data.range_requests}
            icon={<ThunderboltOutlined />}
            tone="blue"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="今日 tokens"
            value={data.range_tokens}
            compact
            icon={<ApiOutlined />}
            tone="cyan"
            hint={`输入 ${formatCompact(data.range_prompt_tokens)} · 输出 ${formatCompact(data.range_completion_tokens)}`}
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="今日成功率"
            value={data.range_success_rate}
            precision={1}
            suffix="%"
            icon={<NodeIndexOutlined />}
            tone="green"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="今日平均总用时"
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
            label="今日缓存命中率"
            value={data.range_cache_hit_rate}
            precision={1}
            suffix="%"
            icon={<DollarOutlined />}
            tone="purple"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="今日缓存读取"
            value={data.range_cache_read}
            compact
            icon={<DatabaseOutlined />}
            tone="green"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="今日缓存写入"
            value={data.range_cache_write}
            compact
            icon={<DatabaseOutlined />}
            tone="cyan"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="今日费用"
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
