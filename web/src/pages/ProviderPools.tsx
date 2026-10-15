import { useEffect, useState } from 'react'
import { PlusOutlined, ReloadOutlined } from '@ant-design/icons'
import { App, Button, Form, Space } from 'antd'
import {
  api,
  formatError,
  type ApiKey,
  type PoolUsage,
  type Provider,
  type ProviderPool,
  type ProviderPoolInput,
} from '../api'
import PageHeader from '../components/PageHeader'
import { validateOrUndefined } from '../forms'
import ProviderPoolEditorModal from './providerPools/ProviderPoolEditorModal'
import ProviderPoolsTable from './providerPools/ProviderPoolsTable'
import type { ProviderPoolFormValues } from './providerPools/types'

export default function ProviderPoolsPage() {
  const { message } = App.useApp()
  const [items, setItems] = useState<ProviderPool[]>([])
  const [providers, setProviders] = useState<Provider[]>([])
  const [apiKeys, setApiKeys] = useState<ApiKey[]>([])
  const [usageByPool, setUsageByPool] = useState<Map<number, PoolUsage>>(
    new Map(),
  )
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [togglingId, setTogglingId] = useState<number>()
  const [editing, setEditing] = useState<ProviderPool>()
  const [open, setOpen] = useState(false)
  const [form] = Form.useForm<ProviderPoolFormValues>()

  // Live usage is supplementary: a failed lookup leaves the pool row intact
  // rather than hiding the configuration behind an error.
  const loadUsage = async (pools: ProviderPool[]) => {
    const providerIds = [...new Set(pools.map((pool) => pool.provider_id))]
    const next = new Map<number, PoolUsage>()
    await Promise.all(
      providerIds.map(async (providerId) => {
        try {
          const views = await api.get<PoolUsage[]>(
            `/api/providers/${providerId}/pool-usage`,
          )
          for (const view of views) next.set(view.pool_id, view)
        } catch {
          // Usage is best-effort; the configuration still renders.
        }
      }),
    )
    setUsageByPool(next)
  }

  const load = async (showLoading = true) => {
    if (showLoading) setLoading(true)
    try {
      const [pools, providerList, keyList] = await Promise.all([
        api.get<ProviderPool[]>('/api/provider-pools'),
        api.get<Provider[]>('/api/providers'),
        api.get<ApiKey[]>('/api/api-keys'),
      ])
      setItems(pools)
      setProviders(providerList)
      setApiKeys(keyList)
      if (showLoading) setLoading(false)
      await loadUsage(pools)
    } catch (error) {
      message.error(formatError(error))
      if (showLoading) setLoading(false)
    }
  }

  useEffect(() => {
    void load()
  }, [])

  const openEditor = (pool?: ProviderPool) => {
    setEditing(pool)
    form.setFieldsValue(
      pool
        ? {
            provider_id: pool.provider_id,
            name: pool.name,
            enabled: pool.enabled,
            window_seconds: pool.window_seconds,
            request_limit: pool.request_limit ?? null,
            token_limit: pool.token_limit ?? null,
            saturation_threshold_pct: pool.saturation_threshold_pct,
            allocations: pool.allocations.map((allocation) => ({
              api_key_id: allocation.api_key_id,
              weight_pct: allocation.weight_pct,
              policy: allocation.policy,
            })),
          }
        : {
            provider_id: undefined,
            name: '',
            enabled: true,
            window_seconds: 60,
            request_limit: null,
            token_limit: null,
            saturation_threshold_pct: 50,
            allocations: [],
          },
    )
    setOpen(true)
  }

  const save = async () => {
    const values = await validateOrUndefined(form)
    if (!values) return
    const payload: ProviderPoolInput = {
      name: values.name.trim(),
      enabled: values.enabled,
      window_seconds: values.window_seconds,
      request_limit: values.request_limit ?? null,
      token_limit: values.token_limit ?? null,
      saturation_threshold_pct: values.saturation_threshold_pct,
      allocations: values.allocations.map((allocation) => ({
        api_key_id: allocation.api_key_id as number,
        weight_pct: allocation.weight_pct ?? 100,
        policy: allocation.policy ?? 'hard',
      })),
    }

    setSaving(true)
    try {
      if (editing) {
        await api.put(`/api/provider-pools/${editing.id}`, payload)
      } else {
        await api.post('/api/provider-pools', {
          ...payload,
          provider_id: values.provider_id,
        })
      }
      message.success(editing ? '配额池已更新' : '配额池已创建')
      setOpen(false)
      await load(false)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setSaving(false)
    }
  }

  const toggle = async (pool: ProviderPool, enabled: boolean) => {
    setTogglingId(pool.id)
    try {
      await api.put(`/api/provider-pools/${pool.id}`, { enabled })
      message.success(enabled ? '配额池已启用' : '配额池已停用')
      await load(false)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setTogglingId(undefined)
    }
  }

  const remove = async (id: number) => {
    try {
      await api.delete(`/api/provider-pools/${id}`)
      message.success('配额池已删除')
      await load(false)
    } catch (error) {
      message.error(formatError(error))
    }
  }

  return (
    <>
      <PageHeader
        title="配额池"
        description="把一个提供商的上游容量按权重分配给网关访问密钥"
        extra={
          <Space>
            <Button
              icon={<ReloadOutlined />}
              onClick={() => void load()}
              loading={loading}
            >
              刷新
            </Button>
            <Button
              type="primary"
              icon={<PlusOutlined />}
              onClick={() => openEditor()}
            >
              新建配额池
            </Button>
          </Space>
        }
      />
      <ProviderPoolsTable
        items={items}
        loading={loading}
        togglingId={togglingId}
        usageByPool={usageByPool}
        onToggle={(pool, enabled) => void toggle(pool, enabled)}
        onEdit={openEditor}
        onRemove={(id) => void remove(id)}
      />
      <ProviderPoolEditorModal
        open={open}
        editing={editing}
        saving={saving}
        form={form}
        providers={providers}
        apiKeys={apiKeys}
        onCancel={() => setOpen(false)}
        onSave={() => void save()}
      />
    </>
  )
}
