import { useCallback, useEffect, useState } from 'react'
import { FileSearchOutlined } from '@ant-design/icons'
import { Alert, Button, Skeleton, Tabs } from 'antd'
import dayjs, { type Dayjs } from 'dayjs'
import { useNavigate } from 'react-router-dom'
import { api, formatError, type Overview } from '../api'
import UsageRangeToolbar from '../components/UsageRangeToolbar'
import { useAutoRefresh } from '../hooks/useAutoRefresh'
import { useCoalescedUsageEvents, useRealtime } from '../realtime'
import DashboardCharts from './dashboard/DashboardCharts'
import DashboardModelUsage from './dashboard/DashboardModelUsage'
import AnalyticsMetrics from './analytics/AnalyticsMetrics'
import AnalyticsProviderUsage from './analytics/AnalyticsProviderUsage'
import { withDevProviderMock } from './analytics/devProviderMock'

export default function Analytics() {
  const navigate = useNavigate()
  const [data, setData] = useState<Overview>()
  const [providerDataMocked, setProviderDataMocked] = useState(false)
  const [error, setError] = useState('')
  const [dates, setDates] = useState<[Dayjs, Dayjs]>(() => [
    dayjs().subtract(13, 'day').startOf('day'),
    dayjs().endOf('day'),
  ])

  const load = useCallback(async () => {
    try {
      const offset = -new Date().getTimezoneOffset()
      const params = new URLSearchParams({
        tz_offset_minutes: String(offset),
        from: dates[0].startOf('day').toISOString(),
        to: dates[1].add(1, 'day').startOf('day').toISOString(),
        include_session_metrics: 'true',
      })
      const loaded = await api.get<Overview>(`/api/overview?${params}`)
      const resolved = withDevProviderMock(loaded)
      setData(resolved.data)
      setProviderDataMocked(resolved.mocked)
      setError('')
    } catch (reason) {
      setError(formatError(reason))
    }
  }, [dates])

  useEffect(() => {
    void load()
  }, [load])

  const autoRefresh = useAutoRefresh({
    intervalMs: 30_000,
    onRefresh: load,
  })
  const { connected: realtimeConnected } = useRealtime()
  useCoalescedUsageEvents(
    () => {
      void autoRefresh.manualRefresh()
    },
    { enabled: !autoRefresh.paused },
  )

  if (error) {
    return <Alert type="error" showIcon message="无法加载用量分析" description={error} />
  }
  if (!data) return <Skeleton active paragraph={{ rows: 10 }} />

  return (
    <>
      <UsageRangeToolbar
        title="用量分析"
        description="按时间、模型和提供商分析请求、Token、费用与性能"
        dates={dates}
        lastUpdated={autoRefresh.lastUpdated}
        refreshing={autoRefresh.refreshing}
        paused={autoRefresh.paused}
        realtimeConnected={realtimeConnected}
        onDatesChange={setDates}
        onTogglePaused={() => autoRefresh.setPaused((value) => !value)}
        onRefresh={() => void autoRefresh.manualRefresh()}
        extraActions={
          <Button icon={<FileSearchOutlined />} onClick={() => navigate('/usage')}>
            请求日志
          </Button>
        }
      />
      <AnalyticsMetrics data={data} />
      <Tabs
        className="section-row"
        defaultActiveKey="trend"
        items={[
          {
            key: 'trend',
            label: '趋势与分布',
            children: (
              <DashboardCharts
                data={data}
                providerDataMocked={providerDataMocked}
              />
            ),
          },
          {
            key: 'models',
            label: '模型',
            children: <DashboardModelUsage data={data} />,
          },
          {
            key: 'providers',
            label: '提供商',
            children: (
              <AnalyticsProviderUsage
                data={data}
                providerDataMocked={providerDataMocked}
              />
            ),
          },
        ]}
      />
    </>
  )
}
