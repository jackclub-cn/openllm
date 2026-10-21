import { Descriptions, Tag, Tooltip, Typography } from 'antd'
import dayjs from 'dayjs'
import type { RequestActivity } from '../../api'
import { formatTimingMs } from '../../components/requestTimings'

function timestamp(value?: string | null) {
  if (!value) return '-'
  const parsed = dayjs(value)
  return parsed.isValid() ? parsed.format('YYYY-MM-DD HH:mm:ss.SSS') : value
}

export default function ActivityRequestSummary({
  activity,
  totalMs,
}: {
  activity: RequestActivity
  totalMs: number
}) {
  const cacheReported = activity.cache.state === 'reported'
  return (
    <section className="activity-request-summary">
      <Typography.Title level={5}>请求摘要</Typography.Title>
      <Descriptions
        className="activity-detail-metadata"
        column={2}
        size="small"
        bordered
      >
        <Descriptions.Item label="请求 ID" span={2}>
          <Typography.Text copyable={{ text: activity.request_id }}>
            {activity.request_id}
          </Typography.Text>
        </Descriptions.Item>
        <Descriptions.Item label="开始时间">
          {timestamp(activity.started_at)}
        </Descriptions.Item>
        <Descriptions.Item label={activity.outcome === 'running' ? '当前耗时' : '结束时间'}>
          {activity.outcome === 'running'
            ? formatTimingMs(totalMs)
            : timestamp(activity.finished_at)}
        </Descriptions.Item>
        <Descriptions.Item label="请求模型">
          {activity.requested_model || '-'}
        </Descriptions.Item>
        <Descriptions.Item label="流式">
          <Tag>{activity.streamed ? '是' : '否'}</Tag>
        </Descriptions.Item>
        <Descriptions.Item label="会话">
          {activity.session_id || '-'}
        </Descriptions.Item>
        <Descriptions.Item label="上游密钥">
          {activity.target?.provider_api_key_id
            ? `#${activity.target.provider_api_key_id}`
            : '-'}
        </Descriptions.Item>
        <Descriptions.Item label="尝试 / 回退">
          {activity.attempt_count} / {activity.fallback_count}
        </Descriptions.Item>
        <Descriptions.Item label="首 Token">
          <Tooltip
            title={activity.streamed ? '首个有效输出片段' : '非流式请求不适用'}
          >
            <span>
              {activity.streamed
                ? formatTimingMs(activity.timings.time_to_first_token_ms)
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
          {cacheReported ? activity.cache.read_tokens : '未知'}
        </Descriptions.Item>
        <Descriptions.Item label="缓存写入">
          {cacheReported ? activity.cache.write_tokens : '未知'}
        </Descriptions.Item>
        <Descriptions.Item label="缓存命中率">
          {activity.cache.hit_rate != null
            ? `${(activity.cache.hit_rate * 100).toFixed(1)}%`
            : activity.outcome === 'running'
              ? '等待供应商返回'
              : '未知'}
        </Descriptions.Item>
        <Descriptions.Item label="更新时间">
          {timestamp(activity.updated_at)}
        </Descriptions.Item>
        <Descriptions.Item label="等待原因" span={2}>
          {activity.waiting_reason || '-'}
        </Descriptions.Item>
        <Descriptions.Item label="错误" span={2}>
          {activity.error_code || '-'}
        </Descriptions.Item>
      </Descriptions>
    </section>
  )
}
