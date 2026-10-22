import { WarningOutlined } from '@ant-design/icons'
import { Button, Space, Tag, Tooltip, Typography } from 'antd'
import type { TableColumnsType } from 'antd'
import dayjs from 'dayjs'
import type { UsageAttempt, UsageLog } from '../../api'
import { formatCompact, formatCostMicros, formatExact } from '../../format'
import { traceReasonLabel } from '../analytics/routingLabels'

type UsageTableRecord = UsageLog | UsageAttempt

function isAttempt(record: UsageTableRecord): record is UsageAttempt {
  return 'attempt_index' in record
}

export function usageTableColumns(onOpenDetail: (record: UsageLog) => void) {
  const columns: TableColumnsType<UsageTableRecord> = [
    {
      title: '时间',
      key: 'created_at',
      width: 150,
      render: (_, record) => {
        const timestamp = dayjs(isAttempt(record) ? record.started_at : record.created_at)
        return (
          <div className="usage-time-cell">
            <span>{timestamp.format('YYYY-MM-DD')}</span>
            <Typography.Text type="secondary">
              {timestamp.format('HH:mm:ss')}
            </Typography.Text>
          </div>
        )
      },
    },
    {
      title: '模型',
      key: 'model',
      width: 190,
      render: (_, record) => (
        <div className="usage-model-cell">
          <Typography.Text strong>
            {isAttempt(record) ? record.upstream_model || '-' : record.requested_model}
          </Typography.Text>
          <div>
            <Typography.Text type="secondary">
              {isAttempt(record)
                ? `尝试 #${record.attempt_index}`
                : record.upstream_model || '-'}
            </Typography.Text>
          </div>
        </div>
      ),
    },
    {
      title: '提供商 / 路由',
      key: 'provider',
      width: 155,
      render: (_, record) => (
        <div>
          <Space size={4}>
            <span>
              {record.provider_name ||
                (record.provider_id ? `#${record.provider_id}` : '-')}
            </span>
            {record.provider_api_key_name && (
              <Tooltip title={`上游密钥 #${record.provider_api_key_id ?? '-'}`}>
                <Tag color="blue">{record.provider_api_key_name}</Tag>
              </Tooltip>
            )}
          </Space>
          <Typography.Text type="secondary">
            {isAttempt(record)
              ? '失败尝试'
              : record.route_name || (record.route_id ? `#${record.route_id}` : '直连模型')}
          </Typography.Text>
        </div>
      ),
    },
    {
      title: '访问密钥',
      key: 'api_key',
      width: 115,
      render: (_, record) => {
        if (isAttempt(record)) return '-'
        return record.api_key_name ? (
          <Tag color="geekblue">{record.api_key_name}</Tag>
        ) : (
          <Typography.Text type="secondary">
            {record.api_key_id ? `#${record.api_key_id}` : '匿名调用'}
          </Typography.Text>
        )
      },
    },
    {
      title: '接口',
      key: 'endpoint',
      width: 195,
      render: (_, record) =>
        isAttempt(record) ? (
          '-'
        ) : (
          <Typography.Text code>{record.endpoint}</Typography.Text>
        ),
    },
    {
      title: '状态',
      key: 'status',
      width: 85,
      render: (_, record) => {
        const statusCode = record.success ? '成功' : record.status_code ?? '失败'
        return (
          <Space size={4}>
            {!isAttempt(record) && record.in_flight ? (
              <Tag color="processing">请求中</Tag>
            ) : (
              <Tag color={record.success ? 'success' : 'error'}>{statusCode}</Tag>
            )}
            {!isAttempt(record) && record.warning_message && (
              <Tooltip title={record.warning_message}>
                <WarningOutlined style={{ color: '#d48806' }} />
              </Tooltip>
            )}
          </Space>
        )
      },
    },
    {
      title: 'tokens',
      key: 'tokens',
      width: 130,
      render: (_, record) => {
        if (isAttempt(record)) return '-'
        if (record.in_flight) return <Typography.Text type="secondary">-</Typography.Text>
        const cached = (record.cache_read_tokens || 0) + (record.cache_write_tokens || 0)
        return (
          <div>
            <Tooltip
              title={`输入 ${formatExact(record.prompt_tokens)} / 输出 ${formatExact(record.completion_tokens)} / 总 tokens ${formatExact(record.total_tokens)}`}
            >
              <div>
                <Typography.Text type="secondary">输入 </Typography.Text>
                {formatCompact(record.prompt_tokens)}
              </div>
              <div>
                <Typography.Text type="secondary">输出 </Typography.Text>
                {formatCompact(record.completion_tokens)}
              </div>
            </Tooltip>
            {cached > 0 && (
              <div>
                <Tooltip
                  title={`缓存读取 ${formatExact(record.cache_read_tokens)} / 缓存写入 ${formatExact(record.cache_write_tokens)}`}
                >
                  <Typography.Text type="success" style={{ fontSize: 12 }}>
                    缓存 {formatCompact(record.cache_read_tokens)}
                  </Typography.Text>
                </Tooltip>
              </div>
            )}
          </div>
        )
      },
    },
    {
      title: '总用时',
      dataIndex: 'latency_ms',
      width: 100,
      render: (value: number, record) =>
        !isAttempt(record) && record.in_flight ? '-' : `${value} ms`,
    },
    {
      title: '费用',
      key: 'cost',
      width: 75,
      render: (_, record) =>
        isAttempt(record) || record.in_flight
          ? '-'
          : formatCostMicros(record.estimated_cost_micros),
    },
    {
      title: '首 token 用时',
      key: 'first_token',
      width: 125,
      render: (_, record) => {
        if (isAttempt(record)) return '-'
        if (record.in_flight) return <Typography.Text type="secondary">-</Typography.Text>
        if (record.first_token_ms == null) {
          return <Typography.Text type="secondary">不适用</Typography.Text>
        }
        return (
          <Tooltip
            title={
              record.streamed
                ? '从请求开始到收到首个输出片段'
                : '标准响应没有增量时间戳，按完整响应总用时记录'
            }
          >
            <span>{record.first_token_ms} ms</span>
          </Tooltip>
        )
      },
    },
    {
      title: 'TPS',
      key: 'tps',
      width: 100,
      render: (_, record) => {
        if (isAttempt(record) || record.in_flight || record.output_tps == null) return '-'
        return (
          <Tooltip
            title={
              record.streamed
                ? '生成阶段速度（已排除首 token 等待）'
                : '含等待时间的整体速度'
            }
          >
            <span>{record.output_tps.toFixed(1)} tok/s</span>
          </Tooltip>
        )
      },
    },
    {
      title: '类型',
      key: 'streamed',
      width: 75,
      render: (_, record) =>
        isAttempt(record) ? (
          <Tag color="warning">失败尝试</Tag>
        ) : (
          <Tag>{record.streamed ? '流式' : '标准'}</Tag>
        ),
    },
    {
      title: '请求 ID',
      dataIndex: 'request_id',
      width: 145,
      render: (value: string) => (
        <Typography.Text copyable={{ text: value }}>
          {value.slice(0, 12)}…
        </Typography.Text>
      ),
    },
    {
      title: '错误',
      key: 'error',
      width: 150,
      render: (_, record) => {
        if (isAttempt(record)) return traceReasonLabel(record.reason_code)
        if (!record.error_message) return '-'
        return (
          <Tooltip title={record.error_message}>
            <Button
              type="link"
              size="small"
              danger
              onClick={(event) => {
                event.stopPropagation()
                onOpenDetail(record)
              }}
            >
              查看错误
            </Button>
          </Tooltip>
        )
      },
    },
  ]
  return columns
}
