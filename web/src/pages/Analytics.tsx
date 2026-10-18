import { useCallback, useEffect, useState } from 'react'
import { ClearOutlined, FileSearchOutlined } from '@ant-design/icons'
import { Alert, App, Button, Card, Segmented, Skeleton, Space, Tabs } from 'antd'
import dayjs, { type Dayjs } from 'dayjs'
import { useNavigate } from 'react-router-dom'
import {
  api,
  formatError,
  type AnalyticsGranularity,
  type ApiKey,
  type GatewayRoute,
  type Overview,
  type Provider,
} from '../api'
import UsageDimensionFilters, {
  type UsageDimensionFilterValues,
} from '../components/UsageDimensionFilters'
import UsageRangePicker from '../components/UsageRangePicker'
import UsageRangeToolbar from '../components/UsageRangeToolbar'
import { useAutoRefresh } from '../hooks/useAutoRefresh'
import { useCoalescedUsageEvents, useRealtime } from '../realtime'
import AnalyticsErrors from './analytics/AnalyticsErrors'
import AnalyticsDimensions from './analytics/AnalyticsDimensions'
import AnalyticsMetrics from './analytics/AnalyticsMetrics'
import AnalyticsProviderUsage from './analytics/AnalyticsProviderUsage'
import AnalyticsRouteQuality from './analytics/AnalyticsRouteQuality'
import { withDevProviderMock } from './analytics/devProviderMock'
import { buildAnalyticsParams } from './analytics/queryParams'
import DashboardCharts from './dashboard/DashboardCharts'
import DashboardModelUsage from './dashboard/DashboardModelUsage'

const EMPTY_FILTERS: UsageDimensionFilterValues = {
  model: '',
  endpoint: '',
  statusFilter: 'all',
  onlyAdjusted: false,
}

const GRANULARITY_OPTIONS: Array<{
  label: string
  value: AnalyticsGranularity
}> = [
  { label: '自动', value: 'auto' },
  { label: '小时', value: 'hour' },
  { label: '日', value: 'day' },
  { label: '周', value: 'week' },
  { label: '月', value: 'month' },
]

export default function Analytics() {
  const { message } = App.useApp()
  const navigate = useNavigate()
  const [data, setData] = useState<Overview>()
  const [providerDataMocked, setProviderDataMocked] = useState(false)
  const [error, setError] = useState('')
  const [dates, setDates] = useState<[Dayjs, Dayjs]>(() => [
    dayjs().subtract(13, 'day').startOf('day'),
    dayjs().endOf('day'),
  ])
  const [filters, setFilters] = useState<UsageDimensionFilterValues>(EMPTY_FILTERS)
  const [granularity, setGranularity] = useState<AnalyticsGranularity>('auto')
  const [fullBreakdown, setFullBreakdown] = useState(false)
  const [providers, setProviders] = useState<Provider[]>([])
  const [apiKeys, setApiKeys] = useState<ApiKey[]>([])
  const [routes, setRoutes] = useState<GatewayRoute[]>([])

  useEffect(() => {
    void Promise.all([
      api.get<Provider[]>('/api/providers').then(setProviders),
      api.get<ApiKey[]>('/api/api-keys').then(setApiKeys),
      api.get<GatewayRoute[]>('/api/routes').then(setRoutes),
    ]).catch((reason) => message.error(formatError(reason)))
  }, [])

  const filtersActive = Boolean(
    filters.model.trim() ||
      filters.endpoint.trim() ||
      filters.providerId ||
      filters.providerApiKeyId ||
      filters.apiKeyId ||
      filters.routeId ||
      filters.statusFilter !== 'all' ||
      filters.onlyAdjusted,
  )

  const load = useCallback(async () => {
    try {
      const params = buildAnalyticsParams(dates, filters, granularity)
      if (fullBreakdown) params.set('full_breakdown', 'true')
      const loaded = await api.get<Overview>(`/api/overview?${params}`)
      // Preview data would hide a genuinely empty filtered result in dev.
      const resolved = filtersActive
        ? { data: loaded, mocked: false as const }
        : withDevProviderMock(loaded)
      setData(resolved.data)
      setProviderDataMocked(resolved.mocked)
      setError('')
    } catch (reason) {
      setError(formatError(reason))
    }
  }, [dates, filters, filtersActive, fullBreakdown, granularity])

  const hourlyDisabled = dates[1].diff(dates[0], 'day') + 1 > 31
  useEffect(() => {
    if (hourlyDisabled && granularity === 'hour') {
      setGranularity('auto')
    }
  }, [granularity, hourlyDisabled])

  useEffect(() => {
    // Text filters should not fire one request per keystroke.
    const timer = window.setTimeout(() => {
      void load()
    }, 250)
    return () => window.clearTimeout(timer)
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

  const changeProvider = (value?: number) => {
    setFilters((current) => {
      if (value == null) return { ...current, providerId: undefined }
      const keyOwner = providers.find((provider) =>
        provider.api_keys.some((key) => key.id === current.providerApiKeyId),
      )
      return {
        ...current,
        providerId: value,
        providerApiKeyId:
          keyOwner && keyOwner.id !== value ? undefined : current.providerApiKeyId,
      }
    })
  }

  const changeProviderApiKey = (value?: number) => {
    setFilters((current) => {
      if (value == null) return { ...current, providerApiKeyId: undefined }
      const keyOwner = providers.find((provider) =>
        provider.api_keys.some((key) => key.id === value),
      )
      return {
        ...current,
        providerApiKeyId: value,
        providerId: keyOwner ? keyOwner.id : current.providerId,
      }
    })
  }

  if (error) {
    return <Alert type="error" showIcon message="无法加载用量分析" description={error} />
  }
  if (!data) return <Skeleton active paragraph={{ rows: 10 }} />

  return (
    <>
      <UsageRangeToolbar
        title="用量分析"
        description="按时间与维度分析请求、Token、费用、性能与错误"
        lastUpdated={autoRefresh.lastUpdated}
        refreshing={autoRefresh.refreshing}
        paused={autoRefresh.paused}
        realtimeConnected={realtimeConnected}
        onTogglePaused={() => autoRefresh.setPaused((value) => !value)}
        onRefresh={() => void autoRefresh.manualRefresh()}
        extraActions={
          <Button icon={<FileSearchOutlined />} onClick={() => navigate('/usage')}>
            请求日志
          </Button>
        }
      />
      <Card bordered={false} className="filter-card">
        <Space wrap>
          <UsageDimensionFilters
            values={filters}
            providers={providers}
            apiKeys={apiKeys}
            routes={routes}
            onChange={(patch) =>
              setFilters((current) => ({ ...current, ...patch }))
            }
            onProviderChange={changeProvider}
            onProviderApiKeyChange={changeProviderApiKey}
            includePending={false}
          />
          <UsageRangePicker dates={dates} onChange={setDates} />
          <Segmented
            size="small"
            value={granularity}
            onChange={(value) => setGranularity(value as AnalyticsGranularity)}
            options={GRANULARITY_OPTIONS.map((option) => ({
              ...option,
              disabled: option.value === 'hour' && hourlyDisabled,
            }))}
          />
          <Button icon={<ClearOutlined />} onClick={() => setFilters(EMPTY_FILTERS)}>
            重置
          </Button>
        </Space>
      </Card>
      <AnalyticsMetrics data={data} />
      <Tabs
        className="section-row"
        defaultActiveKey="trend"
        items={[
          {
            key: 'trend',
            label: '趋势',
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
            children: (
              <DashboardModelUsage
                data={data}
                fullBreakdown={fullBreakdown}
                onToggleFullBreakdown={() => setFullBreakdown((value) => !value)}
                loading={autoRefresh.refreshing}
              />
            ),
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
          {
            key: 'dimensions',
            label: '维度',
            children: (
              <AnalyticsDimensions
                dates={dates}
                filters={filters}
                granularity={granularity}
              />
            ),
          },
          {
            key: 'errors',
            label: '错误',
            children: <AnalyticsErrors data={data} />,
          },
          {
            key: 'routing',
            label: '路由',
            children: (
              <AnalyticsRouteQuality
                dates={dates}
                filters={filters}
                granularity={granularity}
              />
            ),
          },
        ]}
      />
    </>
  )
}
