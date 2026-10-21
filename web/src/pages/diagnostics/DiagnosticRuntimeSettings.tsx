import { Card, Collapse, Descriptions, Skeleton, Space, Tag } from 'antd'
import { SettingOutlined } from '@ant-design/icons'
import type { RuntimeLimits } from '../../api'

function seconds(value?: number | null) {
  return value === undefined || value === null ? '不限' : `${value} 秒`
}

function optionalSeconds(value?: number | null) {
  return value === undefined || value === null ? '关闭' : `${value} 秒`
}

export default function DiagnosticRuntimeSettings({
  limits,
}: {
  limits?: RuntimeLimits
}) {
  return (
    <Card
      title={
        <Space>
          <SettingOutlined />
          运行参数
        </Space>
      }
      extra={<Tag>环境配置 · 重启后生效</Tag>}
    >
      {!limits ? (
        <Skeleton active paragraph={{ rows: 4 }} />
      ) : (
        <Collapse
          defaultActiveKey={['capacity']}
          items={[
            {
              key: 'capacity',
              label: '请求与容量',
              children: (
                <Descriptions column={1} size="small">
                  <Descriptions.Item label="全局并发上限">
                    {limits.max_concurrent_requests || '不限'}
                  </Descriptions.Item>
                  <Descriptions.Item label="在途请求体上限">
                    {limits.max_inflight_request_mib
                      ? `${limits.max_inflight_request_mib} MiB`
                      : '不限'}
                  </Descriptions.Item>
                  <Descriptions.Item label="准入等待">
                    {limits.admission_wait_ms > 0
                      ? `${limits.admission_wait_ms} 毫秒`
                      : '不等待'}
                  </Descriptions.Item>
                  <Descriptions.Item label="请求体 / 上游响应上限">
                    {limits.max_request_body_mib} MiB / {limits.max_upstream_body_mib} MiB
                  </Descriptions.Item>
                  <Descriptions.Item label="请求总预算">
                    {seconds(limits.request_timeout_secs)}
                  </Descriptions.Item>
                  <Descriptions.Item label="流最长存活 / 请求体读取上限">
                    {seconds(limits.stream_max_secs)} /{' '}
                    {seconds(limits.body_read_timeout_secs)}
                  </Descriptions.Item>
                  <Descriptions.Item label="上游空闲超时">
                    {limits.upstream_idle_timeout_secs} 秒
                  </Descriptions.Item>
                  <Descriptions.Item label="SSE 心跳">
                    {optionalSeconds(limits.sse_keepalive_secs)}
                  </Descriptions.Item>
                </Descriptions>
              ),
            },
            {
              key: 'recovery',
              label: '恢复与会话',
              children: (
                <Descriptions column={1} size="small">
                  <Descriptions.Item label="提供商熔断阈值">
                    {limits.provider_open_threshold > 0
                      ? `连续 ${limits.provider_open_threshold} 次失败后短路`
                      : '关闭（仅软冷却）'}
                  </Descriptions.Item>
                  <Descriptions.Item label="熔断状态持久化">
                    {limits.resilience_state_persist ? '开启' : '关闭'}
                  </Descriptions.Item>
                  <Descriptions.Item label="会话粘性">
                    {limits.session_affinity_ttl_secs != null
                      ? `TTL ${limits.session_affinity_ttl_secs} 秒 / 最多 ${limits.session_affinity_capacity} 条`
                      : '关闭'}
                  </Descriptions.Item>
                  <Descriptions.Item label="会话粘性持久化">
                    {limits.session_affinity_persist ? '重启后保留' : '重启后清空'}
                  </Descriptions.Item>
                  <Descriptions.Item label="内存压力保护">
                    {limits.memory_limit_mib > 0
                      ? `${limits.memory_limit_mib} MiB（达 ${limits.memory_shed_ratio_pct}% 时拒绝新请求）`
                      : '未开启'}
                  </Descriptions.Item>
                  <Descriptions.Item label="内存限制来源">
                    {limits.memory_limit_source === 'cgroup'
                      ? '容器限制'
                      : limits.memory_limit_source === 'env'
                      ? '手动设置'
                      : '未开启'}
                  </Descriptions.Item>
                </Descriptions>
              ),
            },
            {
              key: 'storage',
              label: '存储与退出',
              children: (
                <Descriptions column={1} size="small">
                  <Descriptions.Item label="数据库连接池">
                    {limits.db_max_connections} 连接 / 等待 {limits.db_acquire_timeout_secs}{' '}
                    秒 / 忙等 {limits.db_busy_timeout_secs} 秒
                  </Descriptions.Item>
                  <Descriptions.Item label="退出等待">
                    {limits.shutdown_grace_secs > 0
                      ? `${limits.shutdown_grace_secs} 秒`
                      : '一直等待'}
                  </Descriptions.Item>
                </Descriptions>
              ),
            },
          ]}
        />
      )}
    </Card>
  )
}
