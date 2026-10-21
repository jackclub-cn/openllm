import {
  ApiOutlined,
  CheckCircleOutlined,
  ClockCircleOutlined,
  CloseCircleOutlined,
  LoadingOutlined,
  NodeIndexOutlined,
} from '@ant-design/icons'
import { Empty, Table, Tag, Typography } from 'antd'
import type { TableColumnsType } from 'antd'
import type { ActivityAttempt, RequestActivity } from '../../api'
import ContextBreakdownPanel from '../../components/ContextBreakdownPanel'
import RequestTimingPanel from '../../components/RequestTimingPanel'
import { formatTimingMs as formatMs } from '../../components/requestTimings'
import { traceReasonLabel } from '../analytics/routingLabels'
import useActivityClock, { activityElapsedMs, activityLiveTimings } from '../../hooks/useActivityClock'
import ActivityRequestSummary from './ActivityRequestSummary'

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

export default function ActivityDetailPanel({
  activity,
  paused = false,
}: {
  activity: RequestActivity
  paused?: boolean
}) {
  const now = useActivityClock(!paused && activity.outcome === 'running')
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
<div className="activity-detail-panel">
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

          <section className="activity-detail-chain">
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

          <div className="activity-overview-grid">
            <RequestTimingPanel
              timings={{ ...timings, total_ms: total }}
              streamed={activity.streamed}
              running={activity.outcome === 'running'}
              performance={{
                timings,
                attempt_count: activity.attempt_count,
                fallback_count: activity.fallback_count,
                failed_attempts_ms: activity.attempts
                  .filter((attempt) => !attempt.success && attempt.reason_code)
                  .reduce((sum, attempt) => sum + Math.max(0, attempt.latency_ms), 0),
                attempts_truncated: activity.attempts_truncated,
              }}
            />
            <ActivityRequestSummary activity={activity} totalMs={total} />
          </div>

          <section className="activity-context-section">
            <Typography.Title level={5}>上下文窗口</Typography.Title>
            <ContextBreakdownPanel context={activity.context} />
          </section>

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
        </div>
  )
}
