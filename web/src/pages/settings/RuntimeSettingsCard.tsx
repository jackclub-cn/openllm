import {
  DatabaseOutlined,
  DownloadOutlined,
  ReloadOutlined,
  SafetyCertificateOutlined,
} from '@ant-design/icons'
import { Button, Card, Descriptions, Space, Tag } from 'antd'
import type { SettingsState } from './useSettings'

type Props = Pick<
  SettingsState,
  'settings' | 'backingUp' | 'runningMaintenance' | 'limits' | 'backup' | 'runMaintenance'
>
export default function RuntimeSettingsCard({ state }: { state: Props }) {
  const { settings, backingUp, runningMaintenance, limits, backup, runMaintenance } =
    state
  return (
    <Card title="运行时" bordered={false}>
      <Descriptions column={1}>
        <Descriptions.Item
          label={
            <Space>
              <SafetyCertificateOutlined />
              管理鉴权
            </Space>
          }
        >
          <Tag color={settings?.admin_auth_enabled ? 'success' : 'default'}>
            {settings?.admin_auth_enabled ? '已开启' : '未开启'}
          </Tag>
        </Descriptions.Item>
        <Descriptions.Item
          label={
            <Space>
              <DatabaseOutlined />
              数据存储
            </Space>
          }
        >
          SQLite
        </Descriptions.Item>
        <Descriptions.Item label="版本">{settings?.version || '-'}</Descriptions.Item>
        <Descriptions.Item label="数据库连接池">
          {limits
            ? `${limits.db_max_connections} 连接 / 等待 ${limits.db_acquire_timeout_secs} 秒 / 忙等 ${limits.db_busy_timeout_secs} 秒`
            : '-'}
        </Descriptions.Item>
        <Descriptions.Item label="上游空闲超时">
          {limits ? `${limits.upstream_idle_timeout_secs} 秒` : '-'}
        </Descriptions.Item>
        <Descriptions.Item label="全局并发上限">
          {limits
            ? limits.max_concurrent_requests > 0
              ? limits.max_concurrent_requests
              : '不限'
            : '-'}
        </Descriptions.Item>
        <Descriptions.Item label="在途请求体上限">
          {limits
            ? limits.max_inflight_request_mib > 0
              ? `${limits.max_inflight_request_mib} MiB`
              : '不限'
            : '-'}
        </Descriptions.Item>
        <Descriptions.Item label="准入等待">
          {limits
            ? limits.admission_wait_ms > 0
              ? `${limits.admission_wait_ms} 毫秒`
              : '不等待'
            : '-'}
        </Descriptions.Item>
        <Descriptions.Item label="流最长存活 / 请求体读取上限">
          {limits
            ? `${
                limits.stream_max_secs != null ? `${limits.stream_max_secs} 秒` : '不限'
              } / ${
                limits.body_read_timeout_secs != null
                  ? `${limits.body_read_timeout_secs} 秒`
                  : '不限'
              }`
            : '-'}
        </Descriptions.Item>
        <Descriptions.Item label="请求总预算">
          {limits
            ? limits.request_timeout_secs != null
              ? `${limits.request_timeout_secs} 秒`
              : '不限'
            : '-'}
        </Descriptions.Item>
        <Descriptions.Item label="提供商熔断阈值">
          {limits
            ? limits.provider_open_threshold > 0
              ? `连续 ${limits.provider_open_threshold} 次失败后短路`
              : '关闭（仅软冷却）'
            : '-'}
        </Descriptions.Item>
        <Descriptions.Item label="熔断状态持久化">
          {limits
            ? limits.resilience_state_persist
              ? '开启（重启后保留冷却与熔断）'
              : '关闭（重启后清空）'
            : '-'}
        </Descriptions.Item>
        <Descriptions.Item label="会话粘性">
          {limits
            ? limits.session_affinity_ttl_secs != null
              ? `TTL ${limits.session_affinity_ttl_secs} 秒 / 最多 ${
                  limits.session_affinity_capacity
                } 条${limits.session_affinity_persist ? '，重启后保留' : '，重启后清空'}`
              : '关闭'
            : '-'}
        </Descriptions.Item>
        <Descriptions.Item label="内存压力保护">
          {limits
            ? limits.memory_limit_mib > 0
              ? `${limits.memory_limit_mib} MiB（${
                  limits.memory_limit_source === 'cgroup' ? '容器限制' : '手动设置'
                }，达 ${limits.memory_shed_ratio_pct}% 时拒绝新请求）`
              : '未开启'
            : '-'}
        </Descriptions.Item>
        <Descriptions.Item label="SSE 心跳">
          {limits
            ? limits.sse_keepalive_secs != null
              ? `${limits.sse_keepalive_secs} 秒`
              : '关闭'
            : '-'}
        </Descriptions.Item>
        <Descriptions.Item label="请求体 / 上游响应上限">
          {limits
            ? `${limits.max_request_body_mib} MiB / ${limits.max_upstream_body_mib} MiB`
            : '-'}
        </Descriptions.Item>
        <Descriptions.Item label="退出等待">
          {limits
            ? limits.shutdown_grace_secs > 0
              ? `${limits.shutdown_grace_secs} 秒`
              : '一直等待'
            : '-'}
        </Descriptions.Item>
        <Descriptions.Item label="数据库备份">
          <Button
            icon={<DownloadOutlined />}
            loading={backingUp}
            onClick={() => void backup()}
          >
            下载备份
          </Button>
        </Descriptions.Item>
        <Descriptions.Item label="定时维护">
          <Button
            icon={<ReloadOutlined />}
            loading={runningMaintenance}
            onClick={() => void runMaintenance()}
          >
            立即运行
          </Button>
        </Descriptions.Item>
      </Descriptions>
    </Card>
  )
}
