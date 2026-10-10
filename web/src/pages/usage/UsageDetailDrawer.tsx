import {
  CheckCircleOutlined,
  CloseCircleOutlined,
  CopyOutlined,
  ExperimentOutlined,
  LoadingOutlined,
} from '@ant-design/icons'
import {
  Alert,
  App,
  Button,
  Descriptions,
  Drawer,
  Space,
  Tag,
  Tooltip,
  Typography,
} from 'antd'
import dayjs from 'dayjs'
import type { UsageLog } from '../../api'
import { formatCompact, formatCostMicros, formatExact } from '../../format'

type UsageDetailDrawerProps = {
  detail?: UsageLog
  loading: boolean
  onClose: () => void
  onDiagnose: (detail: UsageLog) => void
}

export default function UsageDetailDrawer({
  detail,
  loading,
  onClose,
  onDiagnose,
}: UsageDetailDrawerProps) {
  const { message } = App.useApp()

  return (
    <Drawer
      title="请求详情"
      width={520}
      open={Boolean(detail)}
      onClose={onClose}
      loading={loading}
    >
      {detail && (
        <Space direction="vertical" size={16} style={{ width: '100%' }}>
          <div
            className={`detail-status ${
              detail.in_flight
                ? 'detail-status-pending'
                : detail.success
                  ? 'detail-status-success'
                  : 'detail-status-error'
            }`}
          >
            {detail.in_flight ? (
              <LoadingOutlined spin />
            ) : detail.success ? (
              <CheckCircleOutlined />
            ) : (
              <CloseCircleOutlined />
            )}
            <span>
              {detail.in_flight
                ? '请求处理中'
                : detail.success
                  ? '请求成功'
                  : `请求失败 · HTTP ${detail.status_code}`}
            </span>
          </div>
          <Button icon={<ExperimentOutlined />} onClick={() => onDiagnose(detail)}>
            诊断此请求路由
          </Button>
          <Descriptions column={1} size="small" bordered>
            <Descriptions.Item label="请求 ID">
              <Typography.Text copyable={{ text: detail.request_id }}>
                {detail.request_id}
              </Typography.Text>
            </Descriptions.Item>
            <Descriptions.Item label="会话 ID">
              {detail.session_id ? (
                <Typography.Text copyable={{ text: detail.session_id }}>
                  {detail.session_id}
                </Typography.Text>
              ) : (
                '-'
              )}
            </Descriptions.Item>
            <Descriptions.Item label="时间">
              {dayjs(detail.created_at).format('YYYY-MM-DD HH:mm:ss.SSS')}
            </Descriptions.Item>
            <Descriptions.Item label="接口">
              <Typography.Text code>{detail.endpoint}</Typography.Text>
            </Descriptions.Item>
            <Descriptions.Item label="请求模型">
              <Typography.Text code>{detail.requested_model}</Typography.Text>
            </Descriptions.Item>
            <Descriptions.Item label="上游模型">
              {detail.upstream_model || '-'}
            </Descriptions.Item>
            <Descriptions.Item label="提供商">
              {detail.provider_name || (detail.provider_id ? `#${detail.provider_id}` : '-')}
            </Descriptions.Item>
            <Descriptions.Item label="上游密钥">
              {detail.provider_api_key_name ||
                (detail.provider_api_key_id ? `#${detail.provider_api_key_id}` : '-')}
            </Descriptions.Item>
            <Descriptions.Item label="路由">
              {detail.route_name || (detail.route_id ? `#${detail.route_id}` : '自动路由')}
            </Descriptions.Item>
            <Descriptions.Item label="访问密钥">
              {detail.api_key_name || (detail.api_key_id ? `#${detail.api_key_id}` : '匿名调用')}
            </Descriptions.Item>
            <Descriptions.Item label="流式">
              <Tag>{detail.streamed ? '是' : '否'}</Tag>
            </Descriptions.Item>
            <Descriptions.Item label="总用时">{detail.latency_ms} ms</Descriptions.Item>
            <Descriptions.Item label="首 token 用时">
              {detail.first_token_ms != null ? (
                <Tooltip
                  title={
                    detail.streamed
                      ? '从请求开始到收到首个输出片段'
                      : '标准响应没有增量时间戳，按完整响应总用时记录'
                  }
                >
                  <span>{detail.first_token_ms} ms</span>
                </Tooltip>
              ) : (
                '不适用'
              )}
            </Descriptions.Item>
            <Descriptions.Item label="生成速度 (TPS)">
              {detail.output_tps != null ? `${detail.output_tps.toFixed(1)} tok/s` : '-'}
            </Descriptions.Item>
            <Descriptions.Item label="输入 tokens">
              <Tooltip title={formatExact(detail.prompt_tokens)}>
                {formatCompact(detail.prompt_tokens)}
              </Tooltip>
            </Descriptions.Item>
            <Descriptions.Item label="缓存读取 tokens">
              {detail.cache_read_tokens > 0 ? (
                <Tooltip title={formatExact(detail.cache_read_tokens)}>
                  {formatCompact(detail.cache_read_tokens)}
                </Tooltip>
              ) : (
                '-'
              )}
            </Descriptions.Item>
            <Descriptions.Item label="缓存写入 tokens">
              {detail.cache_write_tokens > 0 ? (
                <Tooltip title={formatExact(detail.cache_write_tokens)}>
                  {formatCompact(detail.cache_write_tokens)}
                </Tooltip>
              ) : (
                '-'
              )}
            </Descriptions.Item>
            <Descriptions.Item label="输出 tokens">
              <Tooltip title={formatExact(detail.completion_tokens)}>
                {formatCompact(detail.completion_tokens)}
              </Tooltip>
            </Descriptions.Item>
            <Descriptions.Item label="总 tokens">
              <Tooltip title={formatExact(detail.total_tokens)}>
                {formatCompact(detail.total_tokens)}
              </Tooltip>
            </Descriptions.Item>
            <Descriptions.Item label="预估费用">
              {formatCostMicros(detail.estimated_cost_micros)}
            </Descriptions.Item>
          </Descriptions>
          {detail.warning_message && (
            <Alert
              type="warning"
              showIcon
              message="网关请求调整"
              description={<pre className="response-block">{detail.warning_message}</pre>}
            />
          )}
          {detail.error_message && (
            <div>
              <Typography.Text strong>错误信息</Typography.Text>
              <pre className="error-block">{detail.error_message}</pre>
            </div>
          )}
          {detail.response_preview && (
            <div>
              <Space style={{ width: '100%', justifyContent: 'space-between' }}>
                <Typography.Text strong>响应内容预览</Typography.Text>
                <Button
                  type="link"
                  size="small"
                  icon={<CopyOutlined />}
                  onClick={() => {
                    void navigator.clipboard.writeText(detail.response_preview || '')
                    message.success('已复制')
                  }}
                >
                  复制
                </Button>
              </Space>
              <pre className="response-block">{detail.response_preview}</pre>
            </div>
          )}
        </Space>
      )}
    </Drawer>
  )
}
