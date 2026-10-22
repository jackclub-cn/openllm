import { useCallback, useEffect, useState } from 'react'
import { BranchesOutlined, ReloadOutlined } from '@ant-design/icons'
import {
  Alert,
  App,
  Button,
  Card,
  Col,
  Empty,
  Progress,
  Row,
  Skeleton,
  Space,
  Statistic,
  Tag,
  Tooltip,
  Typography,
} from 'antd'
import dayjs, { type Dayjs } from 'dayjs'
import { useNavigate } from 'react-router-dom'
import {
  api,
  formatError,
  type AnalyticsGranularity,
  type ContextAgentUsage,
  type ContextAnalytics,
} from '../../api'
import type { UsageDimensionFilterValues } from '../../components/UsageDimensionFilters'
import { formatCompact, formatExact } from '../../format'
import { buildAnalyticsParams } from './queryParams'

const CATEGORY_LABELS: Record<string, string> = {
  system: '系统提示',
  conversation: '会话内容',
  tools: '工具定义',
  tool_calls: '工具调用',
  tool_results: '工具结果',
  reasoning: '推理',
  unattributed: '未归因',
}

const CATEGORY_COLORS: Record<string, string> = {
  system: '#722ed1',
  conversation: '#1677ff',
  tools: '#13c2c2',
  tool_calls: '#2f54eb',
  tool_results: '#fa8c16',
  reasoning: '#eb2f96',
  unattributed: '#8c8c8c',
}

function contextShare(agent: ContextAgentUsage) {
  return agent.requests > 0 ? (agent.context_samples / agent.requests) * 100 : 0
}

function measuredShare(agent: ContextAgentUsage) {
  return agent.context_samples > 0
    ? (agent.measured_samples / agent.context_samples) * 100
    : 0
}

function AgentContextCard({
  agent,
  dates,
}: {
  agent: ContextAgentUsage
  dates: [Dayjs, Dayjs]
}) {
  const navigate = useNavigate()
  const total = agent.category_tokens.reduce(
    (sum, category) => sum + category.tokens,
    0,
  )
  const open = () => {
    const params = new URLSearchParams({
      from: dates[0].startOf('day').toISOString(),
      to: dates[1].add(1, 'day').startOf('day').toISOString(),
    })
    if (agent.api_key_id) params.set('api_key_id', String(agent.api_key_id))
    navigate(`/usage?${params}`)
  }

  return (
    <Card
      bordered={false}
      className="context-agent-card"
      onClick={open}
      title={
        <Space size={8}>
          <BranchesOutlined />
          <Typography.Text strong ellipsis={{ tooltip: agent.label }}>
            {agent.label}
          </Typography.Text>
        </Space>
      }
      extra={
        <Tooltip title={`${agent.context_samples} / ${agent.requests} 条请求包含上下文分类`}>
          <Tag color={contextShare(agent) >= 90 ? 'success' : 'default'}>
            覆盖 {contextShare(agent).toFixed(0)}%
          </Tag>
        </Tooltip>
      }
    >
      <div className="context-agent-meta">
        <span>{formatExact(agent.requests)} 请求</span>
        <span>{formatExact(agent.sessions)} 会话</span>
        <span>{dayjs(agent.last_seen).format('MM-DD HH:mm')}</span>
      </div>
      <Row gutter={[12, 12]} className="context-agent-metrics">
        <Col span={8}>
          <Statistic
            title="平均输入"
            value={agent.avg_prompt_tokens}
            precision={0}
            formatter={(value) => formatCompact(Math.round(Number(value)))}
          />
        </Col>
        <Col span={8}>
          <Statistic
            title="最大输入"
            value={agent.max_prompt_tokens}
            formatter={(value) => formatCompact(Math.round(Number(value)))}
          />
        </Col>
        <Col span={8}>
          <Statistic
            title="缓存"
            value={agent.cache_hit_rate}
            precision={1}
            suffix="%"
          />
        </Col>
      </Row>
      {agent.category_tokens.length > 0 ? (
        <div className="context-categories">
          {agent.category_tokens.map((category) => {
            const share = total > 0 ? (category.tokens / total) * 100 : 0
            return (
              <div className="context-category" key={category.key}>
                <div className="context-category-label">
                  <span>
                    {CATEGORY_LABELS[category.key] ?? category.key}
                  </span>
                  <span>
                    {formatCompact(category.tokens)} · {share.toFixed(1)}%
                  </span>
                </div>
                <Progress
                  percent={share}
                  showInfo={false}
                  size="small"
                  strokeColor={CATEGORY_COLORS[category.key] ?? '#8c8c8c'}
                />
              </div>
            )
          })}
        </div>
      ) : (
        <Empty
          image={Empty.PRESENTED_IMAGE_SIMPLE}
          description="当前范围没有上下文分类样本"
        />
      )}
      <div className="context-agent-footer">
        <Space size={4} wrap>
          {agent.models.map((model) => (
            <Tag key={model}>{model}</Tag>
          ))}
        </Space>
        <Tooltip title={`${agent.measured_samples} 条实测 / ${agent.estimated_samples} 条估算`}>
          <Typography.Text type="secondary">
            实测 {measuredShare(agent).toFixed(0)}%
          </Typography.Text>
        </Tooltip>
      </div>
    </Card>
  )
}

export default function AnalyticsContext({
  dates,
  filters,
  granularity,
}: {
  dates: [Dayjs, Dayjs]
  filters: UsageDimensionFilterValues
  granularity: AnalyticsGranularity
}) {
  const { message } = App.useApp()
  const [data, setData] = useState<ContextAnalytics>()
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState('')

  const load = useCallback(async () => {
    setLoading(true)
    try {
      const params = buildAnalyticsParams(dates, filters, granularity)
      setData(await api.get<ContextAnalytics>(`/api/analytics/context?${params}`))
      setError('')
    } catch (reason) {
      const text = formatError(reason)
      setError(text)
      message.error(text)
    } finally {
      setLoading(false)
    }
  }, [dates, filters, granularity, message])

  useEffect(() => {
    const timer = window.setTimeout(() => void load(), 250)
    return () => window.clearTimeout(timer)
  }, [load])

  if (error && !data) {
    return <Alert type="error" showIcon message="无法加载上下文分析" description={error} />
  }
  if (!data) return <Skeleton active paragraph={{ rows: 10 }} />

  const coverage =
    data.requests > 0 ? (data.context_requests / data.requests) * 100 : 0
  const measured =
    data.context_requests > 0
      ? (data.measured_requests / data.context_requests) * 100
      : 0

  return (
    <>
      <Card
        bordered={false}
        className="section-row"
        title="上下文概览"
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
          <Col xs={12} xl={4}>
            <Statistic title="请求" value={data.requests} />
          </Col>
          <Col xs={12} xl={4}>
            <Statistic title="会话" value={data.sessions} />
          </Col>
          <Col xs={12} xl={4}>
            <Statistic
              title="上下文覆盖"
              value={coverage}
              precision={1}
              suffix="%"
            />
          </Col>
          <Col xs={12} xl={4}>
            <Statistic title="分类样本" value={data.context_requests} />
          </Col>
          <Col xs={12} xl={4}>
            <Statistic title="上游实测" value={data.measured_requests} />
          </Col>
          <Col xs={12} xl={4}>
            <Statistic
              title="实测占比"
              value={measured}
              precision={1}
              suffix="%"
            />
          </Col>
        </Row>
      </Card>

      {data.agents.length > 0 ? (
        <Row gutter={[16, 16]} className="section-row">
          {data.agents.map((agent) => (
            <Col xs={24} xl={12} xxl={8} key={agent.api_key_id ?? 'anonymous'}>
              <AgentContextCard agent={agent} dates={dates} />
            </Col>
          ))}
        </Row>
      ) : (
        <Card bordered={false} className="section-row">
          <Empty
            image={Empty.PRESENTED_IMAGE_SIMPLE}
            description="当前筛选下没有调用记录"
          />
        </Card>
      )}
    </>
  )
}
