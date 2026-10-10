import { useCallback, useEffect, useEffectEvent, useRef, useState } from 'react'
import { App } from 'antd'
import dayjs, { type Dayjs } from 'dayjs'
import { useNavigate, useSearchParams } from 'react-router-dom'
import {
  api,
  formatError,
  type ApiKey,
  type GatewayRoute,
  type Provider,
  type UsageLog,
} from '../api'
import { useAutoRefresh } from '../hooks/useAutoRefresh'
import { useCoalescedUsageEvents, useRealtime } from '../realtime'
import UsageCleanupModal from './usage/UsageCleanupModal'
import UsageDetailDrawer from './usage/UsageDetailDrawer'
import UsageFilters from './usage/UsageFilters'
import UsageTable from './usage/UsageTable'
import UsageToolbar from './usage/UsageToolbar'
import {
  positiveIntegerParam,
  statusParam,
  type UsagePageResponse,
  type UsageStatusFilter,
} from './usage/types'

export default function Usage() {
  const { message } = App.useApp()
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const [items, setItems] = useState<UsageLog[]>([])
  const [providers, setProviders] = useState<Provider[]>([])
  const [apiKeys, setApiKeys] = useState<ApiKey[]>([])
  const [routes, setRoutes] = useState<GatewayRoute[]>([])
  const [loading, setLoading] = useState(true)
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(20)
  const [total, setTotal] = useState(0)
  const [model, setModel] = useState(() => searchParams.get('model') || '')
  const [requestId, setRequestId] = useState(() => searchParams.get('request_id') || '')
  const [sessionId, setSessionId] = useState(() => searchParams.get('session_id') || '')
  const [endpoint, setEndpoint] = useState(() => searchParams.get('endpoint') || '')
  const [providerId, setProviderId] = useState<number | undefined>(() =>
    positiveIntegerParam(searchParams.get('provider_id')),
  )
  const [providerApiKeyId, setProviderApiKeyId] = useState<number | undefined>(() =>
    positiveIntegerParam(searchParams.get('provider_api_key_id')),
  )
  const [apiKeyId, setApiKeyId] = useState<number | undefined>(() =>
    positiveIntegerParam(searchParams.get('api_key_id')),
  )
  const [routeId, setRouteId] = useState<number | undefined>(() =>
    positiveIntegerParam(searchParams.get('route_id')),
  )
  const [statusFilter, setStatusFilter] = useState<UsageStatusFilter>(() =>
    statusParam(searchParams),
  )
  const [onlyAdjusted, setOnlyAdjusted] = useState(
    () => searchParams.get('gateway_adjusted') === 'true',
  )
  const [dates, setDates] = useState<[Dayjs, Dayjs] | undefined>(() => {
    const from = searchParams.get('from')
    const to = searchParams.get('to')
    return from && to ? [dayjs(from), dayjs(to)] : undefined
  })
  const [newRequests, setNewRequests] = useState(0)
  const [autoScroll, setAutoScroll] = useState(true)
  const [detail, setDetail] = useState<UsageLog>()
  const [detailLoading, setDetailLoading] = useState(false)
  const [cleanupOpen, setCleanupOpen] = useState(false)
  const [cleanupDays, setCleanupDays] = useState<number | null>(30)
  const [cleaning, setCleaning] = useState(false)
  const [exporting, setExporting] = useState(false)
  const known = useRef<{ filter: string; total: number } | undefined>(undefined)
  const queryRef = useRef({
    page,
    pageSize,
    model,
    requestId,
    sessionId,
    endpoint,
    providerId,
    providerApiKeyId,
    apiKeyId,
    routeId,
    statusFilter,
    onlyAdjusted,
    dates,
    autoScroll,
  })
  queryRef.current = {
    page,
    pageSize,
    model,
    requestId,
    sessionId,
    endpoint,
    providerId,
    providerApiKeyId,
    apiKeyId,
    routeId,
    statusFilter,
    onlyAdjusted,
    dates,
    autoScroll,
  }
  const filterKey = JSON.stringify([
    model,
    requestId,
    sessionId,
    endpoint,
    providerId,
    providerApiKeyId,
    apiKeyId,
    routeId,
    statusFilter,
    onlyAdjusted,
    dates?.[0]?.toISOString(),
    dates?.[1]?.toISOString(),
  ])

  useEffect(() => {
    // A different filter set produces a different total, so any pending
    // baseline would manufacture phantom "new requests".
    setNewRequests(0)
  }, [filterKey])

  useEffect(() => {
    const next = new URLSearchParams()
    if (model) next.set('model', model)
    if (requestId) next.set('request_id', requestId)
    if (sessionId) next.set('session_id', sessionId)
    if (endpoint) next.set('endpoint', endpoint)
    if (providerId) next.set('provider_id', String(providerId))
    if (providerApiKeyId) next.set('provider_api_key_id', String(providerApiKeyId))
    if (apiKeyId) next.set('api_key_id', String(apiKeyId))
    if (routeId) next.set('route_id', String(routeId))
    if (statusFilter === 'success') next.set('success', 'true')
    if (statusFilter === 'failed') next.set('success', 'false')
    if (statusFilter === 'pending') next.set('in_flight', 'true')
    if (onlyAdjusted) next.set('gateway_adjusted', 'true')
    if (dates?.[0]) next.set('from', dates[0].toISOString())
    if (dates?.[1]) next.set('to', dates[1].toISOString())
    if (next.toString() !== searchParams.toString()) {
      setSearchParams(next, { replace: true })
    }
  }, [
    apiKeyId,
    dates,
    endpoint,
    model,
    onlyAdjusted,
    providerApiKeyId,
    providerId,
    requestId,
    routeId,
    sessionId,
    searchParams,
    setSearchParams,
    statusFilter,
  ])

  const load = useCallback(
    async (
      nextPage = page,
      nextPageSize = pageSize,
      mode: 'manual' | 'auto' = 'manual',
    ) => {
      if (mode === 'manual') setLoading(true)
      const params = new URLSearchParams({
        page: String(nextPage),
        page_size: String(nextPageSize),
      })
      if (model) params.set('model', model)
      if (requestId) params.set('request_id', requestId)
      if (sessionId) params.set('session_id', sessionId)
      if (endpoint) params.set('endpoint', endpoint)
      if (providerId) params.set('provider_id', String(providerId))
      if (providerApiKeyId) params.set('provider_api_key_id', String(providerApiKeyId))
      if (apiKeyId) params.set('api_key_id', String(apiKeyId))
      if (routeId) params.set('route_id', String(routeId))
      if (statusFilter === 'success') params.set('success', 'true')
      if (statusFilter === 'failed') params.set('success', 'false')
      if (statusFilter === 'pending') params.set('in_flight', 'true')
      if (onlyAdjusted) params.set('gateway_adjusted', 'true')
      if (dates?.[0]) params.set('from', dates[0].startOf('day').toISOString())
      if (dates?.[1]) params.set('to', dates[1].endOf('day').toISOString())
      try {
        const result = await api.get<UsagePageResponse>(`/api/usage?${params}`)
        const currentFilter = JSON.stringify([
          model,
          requestId,
          sessionId,
          endpoint,
          providerId,
          providerApiKeyId,
          apiKeyId,
          routeId,
          statusFilter,
          onlyAdjusted,
          dates?.[0]?.toISOString(),
          dates?.[1]?.toISOString(),
        ])
        const previous = known.current
        const sameFilter = previous?.filter === currentFilter
        if (mode === 'auto') {
          // Compare the filtered row count instead of the current page's max id:
          // on any page other than the first, new rows never appear in the
          // fetched window, so an id-based check silently misses them.
          // Only rows added under the *same* filter are genuine new requests.
          const added =
            sameFilter && previous ? Math.max(0, result.total - previous.total) : 0
          if (added > 0) {
            if (autoScroll && nextPage === 1) {
              setNewRequests(0)
            } else {
              // Freeze the visible rows and surface a banner instead of
              // swapping content out from under the user.
              setNewRequests((value) => value + added)
              known.current = { filter: currentFilter, total: result.total }
              return
            }
          }
        }
        known.current = { filter: currentFilter, total: result.total }
        setItems(result.items)
        setTotal(result.total)
        setPage(result.page)
        setPageSize(result.page_size)
      } catch (error) {
        message.error(formatError(error))
      } finally {
        if (mode === 'manual') setLoading(false)
      }
    },
    [
      apiKeyId,
      autoScroll,
      dates,
      endpoint,
      model,
      onlyAdjusted,
      page,
      pageSize,
      providerApiKeyId,
      providerId,
      requestId,
      routeId,
      sessionId,
      statusFilter,
    ],
  )

  const loadFromRef = useCallback(
    async (mode: 'manual' | 'auto' = 'auto') => {
      const current = queryRef.current
      await load(current.page, current.pageSize, mode)
    },
    [load],
  )
  const reloadFiltered = useEffectEvent(() => {
    void load(1, pageSize, 'manual')
  })

  const filtersReady = useRef(false)
  useEffect(() => {
    if (!filtersReady.current) {
      filtersReady.current = true
      return
    }
    reloadFiltered()
  }, [
    apiKeyId,
    dates,
    providerApiKeyId,
    providerId,
    routeId,
    statusFilter,
  ])

  useEffect(() => {
    void Promise.all([
      api.get<Provider[]>('/api/providers').then(setProviders),
      api.get<ApiKey[]>('/api/api-keys').then(setApiKeys),
      api.get<GatewayRoute[]>('/api/routes').then(setRoutes),
      load(1, 20),
    ]).catch((error) => message.error(formatError(error)))
  }, [])

  const autoRefresh = useAutoRefresh({
    intervalMs: 15_000,
    onRefresh: () => loadFromRef('auto'),
  })
  const { connected: realtimeConnected } = useRealtime()
  useCoalescedUsageEvents(
    () => {
      void loadFromRef('auto')
    },
    { enabled: !autoRefresh.paused },
  )

  const showLatest = async () => {
    setNewRequests(0)
    await load(1, pageSize)
  }

  const resetFilters = () => {
    setModel('')
    setRequestId('')
    setSessionId('')
    setEndpoint('')
    setProviderId(undefined)
    setProviderApiKeyId(undefined)
    setApiKeyId(undefined)
    setRouteId(undefined)
    setStatusFilter('all')
    setOnlyAdjusted(false)
    setDates(undefined)
  }

  const changeProvider = (value?: number) => {
    setProviderId(value)
    if (value == null) return
    const keyOwner = providers.find((provider) =>
      provider.api_keys.some((key) => key.id === providerApiKeyId),
    )
    if (keyOwner && keyOwner.id !== value) {
      setProviderApiKeyId(undefined)
    }
  }

  const changeProviderApiKey = (value?: number) => {
    setProviderApiKeyId(value)
    if (value == null) return
    const keyOwner = providers.find((provider) =>
      provider.api_keys.some((key) => key.id === value),
    )
    if (keyOwner) setProviderId(keyOwner.id)
  }

  const openDetail = async (record: UsageLog) => {
    setDetail(record)
    setDetailLoading(true)
    try {
      setDetail(await api.get<UsageLog>(`/api/usage/${record.request_id}`))
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setDetailLoading(false)
    }
  }

  const runCleanup = async () => {
    if (!cleanupDays || cleanupDays < 1) {
      message.warning('保留天数至少为 1')
      return
    }
    setCleaning(true)
    try {
      const result = await api.post<{ deleted: number; older_than_days: number }>(
        '/api/usage/cleanup',
        { older_than_days: cleanupDays },
      )
      message.success(`已清理 ${result.deleted} 条早于 ${result.older_than_days} 天的记录`)
      setCleanupOpen(false)
      await load(1, pageSize)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setCleaning(false)
    }
  }

  const exportCsv = async () => {
    setExporting(true)
    try {
      const params = new URLSearchParams()
      if (model) params.set('model', model)
      if (requestId) params.set('request_id', requestId)
      if (sessionId) params.set('session_id', sessionId)
      if (endpoint) params.set('endpoint', endpoint)
      if (providerId) params.set('provider_id', String(providerId))
      if (providerApiKeyId) params.set('provider_api_key_id', String(providerApiKeyId))
      if (apiKeyId) params.set('api_key_id', String(apiKeyId))
      if (routeId) params.set('route_id', String(routeId))
      if (statusFilter === 'success') params.set('success', 'true')
      if (statusFilter === 'failed') params.set('success', 'false')
      if (statusFilter === 'pending') params.set('in_flight', 'true')
      if (onlyAdjusted) params.set('gateway_adjusted', 'true')
      if (dates?.[0]) params.set('from', dates[0].startOf('day').toISOString())
      if (dates?.[1]) params.set('to', dates[1].endOf('day').toISOString())
      const blob = await api.download(`/api/usage/export?${params}`)
      const url = URL.createObjectURL(blob)
      const link = document.createElement('a')
      link.href = url
      link.download = `openllm-usage-${dayjs().format('YYYYMMDD-HHmmss')}.csv`
      link.click()
      URL.revokeObjectURL(url)
      message.success('CSV 已导出')
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setExporting(false)
    }
  }

  return (
    <>
      <UsageToolbar
        lastUpdated={autoRefresh.lastUpdated}
        refreshing={autoRefresh.refreshing}
        paused={autoRefresh.paused}
        realtimeConnected={realtimeConnected}
        autoScroll={autoScroll}
        newRequests={newRequests}
        exporting={exporting}
        onAutoScrollChange={(value) => {
          setAutoScroll(value)
          if (value && page === 1) setNewRequests(0)
        }}
        onTogglePaused={() => autoRefresh.setPaused((value) => !value)}
        onRefresh={() => void autoRefresh.manualRefresh()}
        onExport={() => void exportCsv()}
        onCleanup={() => setCleanupOpen(true)}
        onShowLatest={() => void showLatest()}
        onDismissNew={() => setNewRequests(0)}
      />
      <UsageFilters
        values={{
          model,
          requestId,
          sessionId,
          endpoint,
          providerId,
          providerApiKeyId,
          apiKeyId,
          routeId,
          statusFilter,
          onlyAdjusted,
          dates,
        }}
        providers={providers}
        apiKeys={apiKeys}
        routes={routes}
        onChange={(patch) => {
          if ('model' in patch) setModel(patch.model ?? '')
          if ('requestId' in patch) setRequestId(patch.requestId ?? '')
          if ('sessionId' in patch) setSessionId(patch.sessionId ?? '')
          if ('endpoint' in patch) setEndpoint(patch.endpoint ?? '')
          if ('apiKeyId' in patch) setApiKeyId(patch.apiKeyId)
          if ('routeId' in patch) setRouteId(patch.routeId)
          if ('statusFilter' in patch && patch.statusFilter) {
            setStatusFilter(patch.statusFilter)
          }
          if ('onlyAdjusted' in patch) setOnlyAdjusted(Boolean(patch.onlyAdjusted))
          if ('dates' in patch) setDates(patch.dates)
        }}
        onProviderChange={changeProvider}
        onProviderApiKeyChange={changeProviderApiKey}
        onReset={resetFilters}
        onSearch={reloadFiltered}
      />
      <UsageTable
        items={items}
        loading={loading}
        page={page}
        pageSize={pageSize}
        total={total}
        onPageChange={(nextPage, nextPageSize) => void load(nextPage, nextPageSize)}
        onOpenDetail={(record) => void openDetail(record)}
      />
      <UsageDetailDrawer
        detail={detail}
        loading={detailLoading}
        onClose={() => setDetail(undefined)}
        onDiagnose={(record) => {
          const params = new URLSearchParams({
            diagnose_model: record.requested_model,
            diagnose_endpoint: record.endpoint,
          })
          if (record.session_id) {
            params.set('diagnose_session_id', record.session_id)
          }
          navigate(`/routes?${params}`)
        }}
      />
      <UsageCleanupModal
        open={cleanupOpen}
        days={cleanupDays}
        cleaning={cleaning}
        onDaysChange={setCleanupDays}
        onCancel={() => setCleanupOpen(false)}
        onConfirm={() => void runCleanup()}
      />
    </>
  )
}
