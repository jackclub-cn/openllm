import {
  ApiOutlined,
  CheckCircleOutlined,
  ClockCircleOutlined,
  CloseCircleOutlined,
  LoadingOutlined,
  NodeIndexOutlined,
} from '@ant-design/icons'
import { Descriptions, Empty, Space, Table, Tag, Tooltip, Typography } from 'antd'
import type { TableColumnsType } from 'antd'
import type { ActivityAttempt, RequestActivity } from '../../api'
import ContextBreakdownPanel from '../../components/ContextBreakdownPanel'
import { traceReasonLabel } from '../analytics/routingLabels'
import useActivityClock, { activityElapsedMs, activityLiveTimings } from '../../hooks/useActivityClock'

const phaseLabels: Record<string, string> = {
  admitted: '已接收',
  preparing: '准备请求',
  routing: '选择路由',
  waiting: '等待容量',
  attempting: '连接上游',
  headers_received: '等待首字',
  streaming: '流式生成',
  completed: '完成',
  failed: '失败',
  cancelled: '已取消',
}

function phaseLabel(value: string) {
  return phaseLabels[value] || value
}

function formatMs(value?: number | null) {
  if (value == null) return '-'
  return value >= 1000 ? `${(value / 1000).toFixed(2)} s` : `${Math.max(0, Math.round(value))} ms`
}

function TimingRow({
  label,
  value,
  total,
  color,
}: {
  label: string
  value?: number | null
  total: number
  color: string
}) {
  const width = value && total > 0 ? Math.min(100, Math.max(1, (value / total) * 100)) : 0
  return (
    <div className="activity-timing-row">
      <span>{label}</span>
      <div className="activity-timing-track">
        <i style={{ width: `${width}%`, background: color }} />
      </div>
      <strong>{formatMs(value)}</strong>
    </div>
  )
}

export default function ActivityDetailPanel({ activity }: { activity: RequestActivity }) {
  const now = useActivityClock(activity.outcome === 'running')
  const total = activityElapsedMs(activity, now)
  const timings = activityLiveTimings(activity, now)
  const attemptColumns: TableColumnsType<ActivityAttempt> = [
    { title: '#', dataIndex: 'attempt_index', width: 46 },
    {
      title: '候选',
      key: 'candidate',
      render: (_, item) =>
        `${item.provider_name || `#${item.provider_id ?? '-'}`} / ${item.upstream_model || '-'}`,
    },
    {
      title: '结果',
      key: 'result',
      width: 110,
      render: (_, item) =>
        !item.reason_code ? <Tag color="processing">进行中</Tag> : item.success ? (
          <Tag color="success">{item.status_code || '成功'}</Tag>
        ) : (
          <Tag color="error">{item.status_code || item.failure_scope || '失败'}</Tag>
        ),
    },
    {
      title: '耗时',
      dataIndex: 'latency_ms',
      width: 90,
      render: (value: number) => formatMs(value),
    },
    {
      title: '响应头',
      dataIndex: 'upstream_headers_ms',
      width: 90,
      render: (value?: number | null) => formatMs(value),
    },
    {
      title: '原因',
      key: 'reason',
      render: (_, item) =>
        item.success ? '-' : traceReasonLabel(item.reason_code || 'unknown'),
    },
  ]

  return (
<Space direction="vertical" size={18} style={{ width: '100%' }}>
          <div
            className={`detail-status ${
              activity.outcome === 'running'
                ? 'detail-status-pending'
                : activity.outcome === 'success'
                  ? 'detail-status-success'
                  : 'detail-status-error'
            }`}
          >
            {activity.outcome === 'running' ? (
              <LoadingOutlined spin />
            ) : activity.outcome === 'success' ? (
              <CheckCircleOutlined />
            ) : (
              <CloseCircleOutlined />
            )}
            <span>
              {phaseLabel(activity.phase)}
              {activity.status_code ? ` · HTTP ${activity.status_code}` : ''}
            </span>
            <Tag color="processing" style={{ marginLeft: 'auto' }}>
              {formatMs(total)}
            </Tag>
          </div>

          <section>
            <Typography.Title level={5}>请求链路</Typography.Title>
            <div className="activity-chain">
              <div className="activity-chain-node">
                <ApiOutlined />
                <strong>{activity.api_key_name || '匿名调用'}</strong>
                <span>{activity.endpoint}</span>
              </div>
              <i className="activity-chain-link" />
              <div className="activity-chain-node">
                <NodeIndexOutlined />
                <strong>{activity.route_name || (activity.route_id ? `路由 #${activity.route_id}` : '直连模型')}</strong>
                <span>{activity.strategy || '未选择策略'}</span>
              </div>
              <i className="activity-chain-link" />
              <div className="activity-chain-node activity-chain-target">
                <ClockCircleOutlined />
                <strong>{activity.target?.provider_name || '等待上游'}</strong>
                <span>{activity.target?.upstream_model || activity.requested_model || '-'}</span>
              </div>
            </div>
          </section>

          <section>
            <Typography.Title level={5}>阶段耗时</Typography.Title>
            <div className="activity-timings">
              <TimingRow
                label="网关准备"
                value={timings.prepare_ms}
                total={total}
                color="#1677ff"
              />
              <TimingRow
                label="路由与等待"
                value={timings.route_wait_ms}
                total={total}
                color="#13a8a8"
              />
              <TimingRow
                label="上游响应头"
                value={timings.upstream_headers_ms}
                total={total}
                color="#fa8c16"
              />
              <TimingRow
                label={activity.streamed ? '等待首输出' : '读取响应'}
                value={activity.streamed ? timings.response_wait_ms : timings.response_read_ms}
                total={total}
                color="#722ed1"
              />
              {activity.streamed && <TimingRow
                label="生成阶段"
                value={timings.generation_ms}
                total={total}
                color="#52c41a"
              />}
            </div>
          </section>

          <section>
            <Typography.Title level={5}>上下文窗口</Typography.Title>
            <ContextBreakdownPanel context={activity.context} />
          </section>

          <Descriptions column={2} size="small" bordered>
            <Descriptions.Item label="请求 ID" span={2}>
              <Typography.Text copyable={{ text: activity.request_id }}>
                {activity.request_id}
              </Typography.Text>
            </Descriptions.Item>
            <Descriptions.Item label="请求模型">
              {activity.requested_model || '-'}
            </Descriptions.Item>
            <Descriptions.Item label="流式">
              <Tag>{activity.streamed ? '是' : '否'}</Tag>
            </Descriptions.Item>
            <Descriptions.Item label="上游密钥">
              {activity.target?.provider_api_key_id ? `#${activity.target.provider_api_key_id}` : '-'}
            </Descriptions.Item>
            <Descriptions.Item label="尝试 / 回退">
              {activity.attempt_count} / {activity.fallback_count}
            </Descriptions.Item>
            <Descriptions.Item label="首 Token">
              <Tooltip title={activity.streamed ? '首个有效输出片段' : '非流式请求不适用'}>
                <span>
                  {activity.streamed
                    ? formatMs(activity.timings.time_to_first_token_ms)
                    : '不适用'}
                </span>
              </Tooltip>
            </Descriptions.Item>
            <Descriptions.Item label="输入 tokens">
              {activity.prompt_tokens ?? `~${activity.request_tokens_estimated}`}
            </Descriptions.Item>
            <Descriptions.Item label="输出 tokens">
              {activity.completion_tokens ?? '-'}
            </Descriptions.Item>
            <Descriptions.Item label="缓存读取">
              {activity.cache.state === 'reported' ? activity.cache.read_tokens : '未知'}
            </Descriptions.Item>
            <Descriptions.Item label="缓存写入">
              {activity.cache.state === 'reported' ? activity.cache.write_tokens : '未知'}
            </Descriptions.Item>
            <Descriptions.Item label="缓存命中率">
              {activity.cache.hit_rate != null
                ? `${(activity.cache.hit_rate * 100).toFixed(1)}%`
                : activity.outcome === 'running'
                  ? '等待供应商返回'
                  : '未知'}
            </Descriptions.Item>
            <Descriptions.Item label="等待原因">
              {activity.waiting_reason || '-'}
            </Descriptions.Item>
            <Descriptions.Item label="错误" span={2}>
              {activity.error_code || '-'}
            </Descriptions.Item>
          </Descriptions>

          <section>
            <Typography.Title level={5}>实际尝试 {activity.attempts_truncated ? `（最近 ${activity.attempts.length} / ${activity.attempt_count} 次）` : ''}</Typography.Title>
            {activity.attempts.length ? (
              <Table
                rowKey="attempt_index"
                size="small"
                pagination={false}
                columns={attemptColumns}
                dataSource={activity.attempts}
                scroll={{ x: 620 }}
              />
            ) : (
              <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="尚未开始上游尝试" />
            )}
          </section>
          <section>
            <Typography.Title level={5}>路由候选（{activity.candidates.length} / {activity.candidate_count}）</Typography.Title>
            <Table
              size="small" rowKey="order" pagination={false} scroll={{ x: 560 }}
              dataSource={activity.candidates}
              columns={[
                { title: '#', dataIndex: 'order', width: 45 },
                { title: '提供商 / 模型', render: (_, item) => `${item.provider_name || item.provider_id} / ${item.upstream_model}` },
                { title: 'Key', dataIndex: 'provider_api_key_id', render: (value) => value ?? '-' },
                { title: '原因', render: (_, item) => <Tag color={item.deprioritized ? 'orange' : 'default'}>{traceReasonLabel(item.reason_code)}</Tag> },
              ]}
            />
          </section>
        </Space>
  )
}
