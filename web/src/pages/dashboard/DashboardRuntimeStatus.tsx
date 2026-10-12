import { useCallback, useEffect, useState } from 'react'
import { DashboardOutlined, ReloadOutlined } from '@ant-design/icons'
import { Alert, Button, Card, Descriptions, Space, Tag, Typography } from 'antd'
import { api, formatError, type RuntimeStatus } from '../../api'
import { useAutoRefresh } from '../../hooks/useAutoRefresh'

function formatUptime(totalSeconds: number): string {
  const days = Math.floor(totalSeconds / 86400)
  const hours = Math.floor((totalSeconds % 86400) / 3600)
  const minutes = Math.floor((totalSeconds % 3600) / 60)
  if (days > 0) return `${days} 天 ${hours} 小时`
  if (hours > 0) return `${hours} 小时 ${minutes} 分`
  return `${minutes} 分钟`
}

/// Live runtime signals in one place: memory pressure, admission saturation,
/// the SQLite pool, maintenance, and the provider circuit summary.
export default function DashboardRuntimeStatus() {
  const [status, setStatus] = useState<RuntimeStatus>()
  const [error, setError] = useState('')

  const load = useCallback(async () => {
    try {
      setStatus(await api.get<RuntimeStatus>('/api/status'))
      setError('')
    } catch (reason) {
      setError(formatError(reason))
    }
  }, [])

  useEffect(() => {
    void load()
  }, [load])

  const autoRefresh = useAutoRefresh({ intervalMs: 15_000, onRefresh: load })

  const memory = status?.memory
  const memoryText = (() => {
    if (!memory) return '-'
    if (memory.used_mib == null) return '不可用'
    if (memory.limit_mib > 0) {
      const percent = Math.round((memory.used_ratio ?? 0) * 100)
      return `${memory.used_mib} / ${memory.limit_mib} MiB（${percent}%）`
    }
    return `${memory.used_mib} MiB`
  })()

  const circuits = status?.circuits
  const circuitText = (() => {
    if (!circuits) return '-'
    if (circuits.threshold === 0) return '未启用'
    if (circuits.open === 0 && circuits.half_open === 0) return '全部正常'
    const parts: string[] = []
    if (circuits.open > 0) parts.push(`${circuits.open} 个短路`)
    if (circuits.half_open > 0) parts.push(`${circuits.half_open} 个半开探测`)
    return parts.join(' · ')
  })()

  const admission = status?.admission
  const admissionText = (() => {
    if (!admission) return '-'
    const concurrency =
      admission.max_concurrent_requests > 0
        ? `${admission.in_flight} / ${admission.max_concurrent_requests}`
        : `${admission.in_flight}（不限）`
    const bytes =
      admission.max_inflight_request_mib > 0
        ? ` · 请求体 ${admission.in_flight_request_mib} / ${admission.max_inflight_request_mib} MiB`
        : ''
    return concurrency + bytes
  })()

  const memoryWarning =
    memory != null && memory.limit_mib > 0 && (memory.pressure || (memory.used_ratio ?? 0) >= 0.8)

  return (
    <Card
      bordered={false}
      className="section-row"
      title={
        <Space>
          <DashboardOutlined />
          运行状态
        </Space>
      }
      extra={
        <Space size={8}>
          {status ? (
            <Tag color={status.draining ? 'orange' : 'green'}>
              {status.draining ? '排空中' : '正常'}
            </Tag>
          ) : null}
          <Button
            size="small"
            icon={<ReloadOutlined />}
            loading={autoRefresh.refreshing}
            onClick={() => void autoRefresh.manualRefresh()}
          >
            刷新
          </Button>
        </Space>
      }
    >
      {error ? (
        <Alert type="error" showIcon message="无法获取运行状态" description={error} />
      ) : (
        <Descriptions size="small" column={{ xs: 1, sm: 2, xl: 4 }}>
          <Descriptions.Item label="版本 / 运行时长">
            {status ? `${status.version} · ${formatUptime(status.uptime_seconds)}` : '-'}
          </Descriptions.Item>
          <Descriptions.Item label="内存">
            {memoryWarning ? (
              <Typography.Text type="warning">{memoryText}</Typography.Text>
            ) : (
              memoryText
            )}
            {memory && memory.pressure ? (
              <Tag color="red" style={{ marginLeft: 8 }}>
                已触发保护
              </Tag>
            ) : null}
          </Descriptions.Item>
          <Descriptions.Item label="在途 / 准入">{admissionText}</Descriptions.Item>
          <Descriptions.Item label="提供商熔断">
            {circuits && (circuits.open > 0 || circuits.half_open > 0) ? (
              <Typography.Text type="warning">{circuitText}</Typography.Text>
            ) : (
              circuitText
            )}
          </Descriptions.Item>
          <Descriptions.Item label="SQLite 连接池">
            {status
              ? `${status.database.connections} 占用 / ${status.database.idle} 空闲 / ${status.database.max_connections} 上限`
              : '-'}
          </Descriptions.Item>
          <Descriptions.Item label="运行时调度延迟">
            {status ? `${status.runtime_lag_millis} ms` : '-'}
          </Descriptions.Item>
          <Descriptions.Item label="维护运行">
            {status ? `${status.maintenance.runs_total} 次` : '-'}
          </Descriptions.Item>
          <Descriptions.Item label="累计拒绝（并发 / 字节 / 内存）">
            {status
              ? `${status.admission.requests_shed_total} / ${status.admission.request_bytes_shed_total} / ${status.memory.shed_total}`
              : '-'}
          </Descriptions.Item>
        </Descriptions>
      )}
    </Card>
  )
}
