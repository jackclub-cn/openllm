import { useCallback, useEffect, useState } from 'react'
import { Alert, Button, Skeleton } from 'antd'
import { LineChartOutlined } from '@ant-design/icons'
import dayjs, { type Dayjs } from 'dayjs'
import { useNavigate } from 'react-router-dom'
import { api, formatError, type Overview } from '../api'
import UsageRangeToolbar from '../components/UsageRangeToolbar'
import { useAutoRefresh } from '../hooks/useAutoRefresh'
import { useCoalescedUsageEvents, useRealtime } from '../realtime'
import DashboardDetails from './dashboard/DashboardDetails'
import DashboardMetrics from './dashboard/DashboardMetrics'
import DashboardRuntimeStatus from './dashboard/DashboardRuntimeStatus'

export default function Dashboard() {
  const navigate = useNavigate()
  const [data, setData] = useState<Overview>()
  const [error, setError] = useState('')
  const [dates, setDates] = useState<[Dayjs, Dayjs]>(() => [
    dayjs().subtract(13, 'day').startOf('day'),
    dayjs().endOf('day'),
  ])

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
      <UsageRangeToolbar
        title="运行概览"
        description="所选时间范围内的流量、运行健康与最近请求"
        dates={dates}
        lastUpdated={autoRefresh.lastUpdated}
        refreshing={autoRefresh.refreshing}
        paused={autoRefresh.paused}
        realtimeConnected={realtimeConnected}
        onDatesChange={setDates}
        onTogglePaused={() => autoRefresh.setPaused((value) => !value)}
        onRefresh={() => void autoRefresh.manualRefresh()}
        extraActions={
          <Button
            icon={<LineChartOutlined />}
            onClick={() => navigate('/analytics')}
          >
            用量分析
          </Button>
        }
      />
      <DashboardMetrics data={data} />
      <DashboardRuntimeStatus />
      <DashboardDetails data={data} />
    </>
  )
}
