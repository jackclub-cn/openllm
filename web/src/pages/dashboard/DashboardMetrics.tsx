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
  const cacheHitRate =
    data.prompt_tokens_total > 0
      ? (data.cache_read_total / data.prompt_tokens_total) * 100
      : 0

  return (
    <>
      <Row gutter={[16, 16]}>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="累计请求"
            value={data.requests_total}
            icon={<ThunderboltOutlined />}
            tone="blue"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="累计 tokens"
            value={data.tokens_total}
            compact
            icon={<ApiOutlined />}
            tone="cyan"
            hint={`输入 ${formatCompact(data.prompt_tokens_total)} · 输出 ${formatCompact(data.completion_tokens_total)}`}
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="累计成功率"
            value={data.success_rate}
            precision={1}
            suffix="%"
            icon={<NodeIndexOutlined />}
            tone="green"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="累计平均总用时"
            value={data.avg_latency_ms}
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
            label="累计缓存命中率"
            value={cacheHitRate}
            precision={1}
            suffix="%"
            icon={<DollarOutlined />}
            tone="purple"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="累计缓存读取"
            value={data.cache_read_total}
            compact
            icon={<DatabaseOutlined />}
            tone="green"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="累计缓存写入"
            value={data.cache_write_total}
            compact
            icon={<DatabaseOutlined />}
            tone="cyan"
          />
        </Col>
        <Col xs={24} sm={12} xl={6}>
          <MetricCard
            label="累计费用"
            value={data.cost_total_micros / 1_000_000}
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
