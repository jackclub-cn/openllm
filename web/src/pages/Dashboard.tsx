import { useCallback, useEffect, useState } from 'react'
import {
  LineChartOutlined,
  PauseCircleOutlined,
  PlayCircleOutlined,
  ReloadOutlined,
} from '@ant-design/icons'
import { Alert, Button, Skeleton, Tag, Tooltip, Typography } from 'antd'
import dayjs from 'dayjs'
import { useNavigate } from 'react-router-dom'
import {
  api,
  formatError,
  type Overview,
  type RequestActivity,
  type UsageLog,
} from '../api'
import PageHeader from '../components/PageHeader'
import { useAutoRefresh } from '../hooks/useAutoRefresh'
import useRecentActivity from '../hooks/useRecentActivity'
import {
  useCoalescedUsageEvents,
  useRealtime,
} from '../realtime'
import ActivityDetailDrawer from './dashboard/ActivityDetailDrawer'
import DashboardDetails from './dashboard/DashboardDetails'
import DashboardLiveRequests from './dashboard/DashboardLiveRequests'
import DashboardMetrics from './dashboard/DashboardMetrics'
import DashboardRequestTimeline from './dashboard/DashboardRequestTimeline'
import DashboardRuntimeStatus from './dashboard/DashboardRuntimeStatus'
import type { UsagePageResponse } from './usage/types'

export default function Dashboard() {
  const navigate = useNavigate()
  const [data, setData] = useState<Overview>()
  const [timelineItems, setTimelineItems] = useState<UsageLog[]>([])
  const [timelineError, setTimelineError] = useState('')
  const [selectedActivity, setSelectedActivity] = useState<RequestActivity>()
  const [error, setError] = useState('')

  const loadOverview = useCallback(async () => {
    try {
      const offset = -new Date().getTimezoneOffset()
      const params = new URLSearchParams({
        tz_offset_minutes: String(offset),
        include_session_metrics: 'false',
      })
      setData(await api.get<Overview>(`/api/overview?${params}`))
      setError('')
    } catch (reason) {
      setError(formatError(reason))
    }
  }, [])

  const loadTimeline = useCallback(async () => {
    try {
      const now = Date.now()
      const params = new URLSearchParams({
        page: '1',
        page_size: '200',
        from: new Date(now - 60_000).toISOString(),
        to: new Date(now + 1_000).toISOString(),
      })
      const inFlightParams = new URLSearchParams({
        page: '1',
        page_size: '200',
        in_flight: 'true',
      })
      const [recent, inFlight] = await Promise.all([
        api.get<UsagePageResponse>(`/api/usage?${params}`),
        api.get<UsagePageResponse>(`/api/usage?${inFlightParams}`),
      ])
      const merged = new Map<number, UsageLog>()
      for (const record of [...recent.items, ...inFlight.items]) {
        merged.set(record.id, record)
      }
      setTimelineItems([...merged.values()])
      setTimelineError('')
    } catch (reason) {
      setTimelineError(formatError(reason))
    }
  }, [])

  const load = useCallback(
    async () => {
      await Promise.all([loadOverview(), loadTimeline()])
    },
    [loadOverview, loadTimeline],
  )

  useEffect(() => {
    void load()
  }, [load])

  const autoRefresh = useAutoRefresh({
    intervalMs: 30_000,
    onRefresh: load,
  })
  const { connected: realtimeConnected } = useRealtime()
  const live = useRecentActivity(autoRefresh.paused)
  useCoalescedUsageEvents(
    () => {
      void autoRefresh.manualRefresh()
    },
    { enabled: !autoRefresh.paused },
  )

  useEffect(() => {
    if (autoRefresh.paused) return
    const timer = window.setInterval(() => {
      if (document.visibilityState === 'visible') void loadTimeline()
    }, 5_000)
    return () => window.clearInterval(timer)
  }, [autoRefresh.paused, loadTimeline])

  if (error) {
    return <Alert type="error" showIcon message="无法加载仪表盘" description={error} />
  }
  if (!data) return <Skeleton active paragraph={{ rows: 10 }} />

  return (
    <>
      <PageHeader
        title="运行概览"
        description="网关生命周期累计统计与最近 60 秒请求"
        extra={
          <>
            <Button
              icon={<LineChartOutlined />}
              onClick={() => navigate('/analytics')}
            >
              用量分析
            </Button>
            {autoRefresh.lastUpdated && (
              <Typography.Text type="secondary">
                更新于 {dayjs(autoRefresh.lastUpdated).format('HH:mm:ss')}
              </Typography.Text>
            )}
            <Tag color={realtimeConnected ? 'success' : 'default'}>
              {realtimeConnected ? '实时' : '轮询'}
            </Tag>
            <Tooltip title={autoRefresh.paused ? '恢复自动刷新' : '暂停自动刷新'}>
              <Button
                icon={
                  autoRefresh.paused ? (
                    <PlayCircleOutlined />
                  ) : (
                    <PauseCircleOutlined />
                  )
                }
                onClick={() => autoRefresh.setPaused((value) => !value)}
              >
                {autoRefresh.paused ? '已暂停' : '自动刷新'}
              </Button>
            </Tooltip>
            <Button
              icon={<ReloadOutlined spin={autoRefresh.refreshing} />}
              loading={autoRefresh.refreshing}
              onClick={() => { void autoRefresh.manualRefresh(); void live.reload() }}
            >
              刷新
            </Button>
          </>
        }
      />
      <DashboardMetrics data={data} />
      <DashboardRuntimeStatus />
      <DashboardLiveRequests
        items={live.items}
        connected={realtimeConnected}
        error={live.error}
        activeCount={live.activeCount}
        loading={live.loading}
        enabled={live.enabled}
        paused={autoRefresh.paused}
        onOpen={setSelectedActivity}
      />
      <DashboardRequestTimeline
        items={timelineItems}
        error={timelineError}
        onOpenDetail={(record) =>
          navigate(`/usage?request_id=${encodeURIComponent(record.request_id)}`)
        }
      />
      <DashboardDetails data={data} />
      <ActivityDetailDrawer
        initial={selectedActivity}
        onClose={() => setSelectedActivity(undefined)}
      />
    </>
  )
}
