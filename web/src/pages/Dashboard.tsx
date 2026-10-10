import { useCallback, useEffect, useState } from 'react'
import { Alert, Skeleton } from 'antd'
import dayjs, { type Dayjs } from 'dayjs'
import { api, formatError, type Overview } from '../api'
import { useAutoRefresh } from '../hooks/useAutoRefresh'
import { useCoalescedUsageEvents, useRealtime } from '../realtime'
import DashboardCharts, {
  type DashboardChartMetric,
} from './dashboard/DashboardCharts'
import DashboardDetails from './dashboard/DashboardDetails'
import DashboardMetrics from './dashboard/DashboardMetrics'
import DashboardModelUsage from './dashboard/DashboardModelUsage'
import DashboardToolbar from './dashboard/DashboardToolbar'

export default function Dashboard() {
  const [data, setData] = useState<Overview>()
  const [error, setError] = useState('')
  const [dates, setDates] = useState<[Dayjs, Dayjs]>(() => [
    dayjs().subtract(13, 'day').startOf('day'),
    dayjs().endOf('day'),
  ])
  const [chartMetric, setChartMetric] = useState<DashboardChartMetric>('requests')

  const load = useCallback(async () => {
    try {
      // Let the server bucket "today" and the daily chart by the viewer's
      // timezone rather than UTC.
      const offset = -new Date().getTimezoneOffset()
      const params = new URLSearchParams({
        tz_offset_minutes: String(offset),
        from: dates[0].startOf('day').toISOString(),
        to: dates[1].add(1, 'day').startOf('day').toISOString(),
        include_session_metrics: 'false',
      })
      setData(await api.get<Overview>(`/api/overview?${params}`))
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
    return <Alert type="error" showIcon message="无法加载仪表盘" description={error} />
  }
  if (!data) return <Skeleton active paragraph={{ rows: 10 }} />

  return (
    <>
      <DashboardToolbar
        dates={dates}
        lastUpdated={autoRefresh.lastUpdated}
        refreshing={autoRefresh.refreshing}
        paused={autoRefresh.paused}
        realtimeConnected={realtimeConnected}
        onDatesChange={setDates}
        onTogglePaused={() => autoRefresh.setPaused((value) => !value)}
        onRefresh={() => void autoRefresh.manualRefresh()}
      />
      <DashboardMetrics data={data} />
      <DashboardCharts
        data={data}
        metric={chartMetric}
        onMetricChange={setChartMetric}
      />
      <DashboardDetails data={data} />
      <DashboardModelUsage data={data} />
    </>
  )
}
