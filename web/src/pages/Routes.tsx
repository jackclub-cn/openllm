import { useEffect, useRef, useState } from 'react'
import { App, Form } from 'antd'
import { useSearchParams } from 'react-router-dom'
import {
  api,
  formatError,
  type GatewayRoute,
  type Provider,
} from '../api'
import RouteDiagnosisModal, {
  type RouteDiagnosisRequest,
} from './routes/RouteDiagnosisModal'
import RouteEditorModal from './routes/RouteEditorModal'
import RoutesTable from './routes/RoutesTable'
import RoutesToolbar from './routes/RoutesToolbar'
import type { RouteFormValues } from './routes/types'

export default function RoutesPage() {
  const { message } = App.useApp()
  const [items, setItems] = useState<GatewayRoute[]>([])
  const [providers, setProviders] = useState<Provider[]>([])
  const [loading, setLoading] = useState(true)
  const [search, setSearch] = useState('')
  const [status, setStatus] = useState<'all' | 'enabled' | 'disabled'>('all')
  const [strategy, setStrategy] = useState<'all' | GatewayRoute['strategy']>('all')
  const [page, setPage] = useState(1)
  const [saving, setSaving] = useState(false)
  const [togglingId, setTogglingId] = useState<number>()
  const [editing, setEditing] = useState<GatewayRoute>()
  const [open, setOpen] = useState(false)
  const [diagnosisRequest, setDiagnosisRequest] = useState<RouteDiagnosisRequest>()
  const [searchParams, setSearchParams] = useSearchParams()
  const handledDiagnosisQuery = useRef('')
  const [form] = Form.useForm<RouteFormValues>()

  const load = async () => {
    setLoading(true)
    try {
      const [routes, providerItems] = await Promise.all([
        api.get<GatewayRoute[]>('/api/routes'),
        api.get<Provider[]>('/api/providers'),
      ])
      setItems(routes)
      setProviders(providerItems)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void load()
  }, [])

  const openEditor = (item?: GatewayRoute) => {
    setEditing(item)
    form.setFieldsValue(
      item
        ? {
            name: item.name,
            model_pattern: item.model_pattern,
            strategy: item.strategy,
            enabled: item.enabled,
            targets: item.targets.map((target) => ({ ...target })),
          }
        : ({
            name: '',
            model_pattern: '',
            strategy: 'priority',
            enabled: true,
            targets: [
              {
                provider_id: providers[0]?.id,
                upstream_model: '',
                weight: 100,
                priority: 0,
                enabled: true,
              },
            ],
          } as RouteFormValues),
    )
    setOpen(true)
  }

  const save = async () => {
    const values = await form.validateFields()
    setSaving(true)
    try {
      if (editing) await api.put(`/api/routes/${editing.id}`, values)
      else await api.post('/api/routes', values)
      message.success(editing ? '路由已更新' : '路由已创建')
      setOpen(false)
      await load()
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setSaving(false)
    }
  }

  const remove = async (id: number) => {
    try {
      await api.delete(`/api/routes/${id}`)
      message.success('路由已删除')
      await load()
    } catch (error) {
      message.error(formatError(error))
    }
  }

  const toggleEnabled = async (record: GatewayRoute, enabled: boolean) => {
    setTogglingId(record.id)
    try {
      await api.put(`/api/routes/${record.id}`, { enabled })
      message.success(enabled ? '路由已启用' : '路由已停用')
      await load()
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setTogglingId(undefined)
    }
  }

  useEffect(() => {
    const queryKey = searchParams.toString()
    if (!queryKey || handledDiagnosisQuery.current === queryKey) return
    const model = searchParams.get('diagnose_model')?.trim()
    if (!model) return
    handledDiagnosisQuery.current = queryKey
    setDiagnosisRequest({
      model,
      endpoint: searchParams.get('diagnose_endpoint') || '/v1/chat/completions',
      sessionId: searchParams.get('diagnose_session_id')?.trim() || '',
    })
    setSearchParams({}, { replace: true })
  }, [searchParams, setSearchParams])

  const filteredItems = items.filter((item) => {
    const query = search.trim().toLowerCase()
    if (query) {
      const searchable = [
        item.name,
        item.model_pattern,
        ...item.targets.flatMap((target) => [
          providers.find((provider) => provider.id === target.provider_id)?.name || '',
          target.upstream_model,
        ]),
      ]
        .join(' ')
        .toLowerCase()
      if (!searchable.includes(query)) return false
    }
    if (status === 'enabled' && !item.enabled) return false
    if (status === 'disabled' && item.enabled) return false
    if (strategy !== 'all' && item.strategy !== strategy) return false
    return true
  })

  return (
    <>
      <RoutesToolbar
        search={search}
        status={status}
        strategy={strategy}
        routeCount={items.length}
        filteredCount={filteredItems.length}
        onSearchChange={(value) => {
          setSearch(value)
          setPage(1)
        }}
        onStatusChange={(value) => {
          setStatus(value)
          setPage(1)
        }}
        onStrategyChange={(value) => {
          setStrategy(value)
          setPage(1)
        }}
        onDiagnose={() => setDiagnosisRequest({})}
        onCreate={() => openEditor()}
      />
      <RoutesTable
        routes={filteredItems}
        routeCount={items.length}
        loading={loading}
        page={page}
        togglingId={togglingId}
        onPageChange={setPage}
        onToggleEnabled={(record, enabled) => void toggleEnabled(record, enabled)}
        onEdit={openEditor}
        onRemove={(id) => void remove(id)}
      />

      <RouteEditorModal
        open={open}
        editing={editing}
        saving={saving}
        providers={providers}
        form={form}
        onCancel={() => setOpen(false)}
        onSave={() => void save()}
      />
      <RouteDiagnosisModal
        request={diagnosisRequest}
        onClose={() => setDiagnosisRequest(undefined)}
      />
    </>
  )
}
