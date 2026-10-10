import { useEffect, useState } from 'react'
import { App, Form } from 'antd'
import dayjs from 'dayjs'
import { api, formatError, type ApiKey } from '../api'
import ApiKeyCreateModal from './apiKeys/ApiKeyCreateModal'
import ApiKeyLimitsModal from './apiKeys/ApiKeyLimitsModal'
import ApiKeysTable from './apiKeys/ApiKeysTable'
import ApiKeysToolbar from './apiKeys/ApiKeysToolbar'
import type { ApiKeyForm } from './apiKeys/types'

type ApiKeyStatus = 'all' | 'enabled' | 'disabled' | 'expired'

export default function ApiKeys() {
  const { message } = App.useApp()
  const [items, setItems] = useState<ApiKey[]>([])
  const [loading, setLoading] = useState(true)
  const [search, setSearch] = useState('')
  const [status, setStatus] = useState<ApiKeyStatus>('all')
  const [page, setPage] = useState(1)
  const [open, setOpen] = useState(false)
  const [saving, setSaving] = useState(false)
  const [createdKey, setCreatedKey] = useState('')
  const [editing, setEditing] = useState<ApiKey>()
  const [limitsOpen, setLimitsOpen] = useState(false)
  const [limitsSaving, setLimitsSaving] = useState(false)
  const [form] = Form.useForm<ApiKeyForm>()
  const [limitsForm] = Form.useForm<ApiKeyForm>()

  const load = async () => {
    setLoading(true)
    try {
      setItems(await api.get<ApiKey[]>('/api/api-keys'))
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void load()
  }, [])

  const create = async () => {
    const values = await form.validateFields()
    setSaving(true)
    try {
      const result = await api.post<{ key: string; item: ApiKey }>('/api/api-keys', {
        name: values.name,
        daily_token_limit: values.daily_token_limit ?? 0,
        daily_cost_limit_micros:
          values.daily_cost_limit_usd != null
            ? Math.round(values.daily_cost_limit_usd * 1_000_000)
            : 0,
        requests_per_minute: values.requests_per_minute ?? 0,
        max_concurrency: values.max_concurrency ?? 0,
        allowed_models: (values.allowed_models_text || '')
          .split('\n')
          .map((value) => value.trim())
          .filter(Boolean),
        expires_at: values.expires_at ? values.expires_at.toISOString() : '',
      })
      setCreatedKey(result.key)
      form.resetFields()
      await load()
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setSaving(false)
    }
  }

  const closeModal = () => {
    setOpen(false)
    setCreatedKey('')
    form.resetFields()
  }

  const remove = async (id: number) => {
    try {
      await api.delete(`/api/api-keys/${id}`)
      message.success('密钥已删除')
      await load()
    } catch (error) {
      message.error(formatError(error))
    }
  }

  const rotate = async (id: number) => {
    try {
      const result = await api.post<{ key: string; item: ApiKey }>(
        `/api/api-keys/${id}/rotate`,
      )
      setCreatedKey(result.key)
      setOpen(true)
      await load()
    } catch (error) {
      message.error(formatError(error))
    }
  }

  const toggle = async (id: number, enabled: boolean) => {
    try {
      await api.put(`/api/api-keys/${id}`, { enabled })
      message.success(enabled ? '密钥已启用' : '密钥已停用')
      await load()
    } catch (error) {
      message.error(formatError(error))
    }
  }

  const openLimits = (record: ApiKey) => {
    setEditing(record)
    limitsForm.setFieldsValue({
      daily_token_limit: record.daily_token_limit ?? null,
      daily_cost_limit_usd:
        record.daily_cost_limit_micros != null
          ? record.daily_cost_limit_micros / 1_000_000
          : null,
      requests_per_minute: record.requests_per_minute ?? null,
      max_concurrency: record.max_concurrency ?? null,
      allowed_models_text: record.allowed_models.join('\n'),
      expires_at: record.expires_at ? dayjs(record.expires_at) : null,
    })
    setLimitsOpen(true)
  }

  const saveLimits = async () => {
    if (!editing) return
    const values = await limitsForm.validateFields()
    setLimitsSaving(true)
    try {
      await api.put(`/api/api-keys/${editing.id}`, {
        enabled: editing.enabled,
        daily_token_limit: values.daily_token_limit ?? 0,
        daily_cost_limit_micros:
          values.daily_cost_limit_usd != null
            ? Math.round(values.daily_cost_limit_usd * 1_000_000)
            : 0,
        requests_per_minute: values.requests_per_minute ?? 0,
        max_concurrency: values.max_concurrency ?? 0,
        allowed_models: (values.allowed_models_text || '')
          .split('\n')
          .map((value) => value.trim())
          .filter(Boolean),
        expires_at: values.expires_at ? values.expires_at.toISOString() : '',
      })
      message.success('访问策略已保存')
      setLimitsOpen(false)
      await load()
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setLimitsSaving(false)
    }
  }

  const filteredItems = items.filter((item) => {
    const query = search.trim().toLowerCase()
    if (query) {
      const searchable = [
        item.name,
        item.key_prefix,
        item.key_suffix,
        ...item.allowed_models,
      ]
        .join(' ')
        .toLowerCase()
      if (!searchable.includes(query)) return false
    }
    const expired = item.expires_at ? dayjs(item.expires_at).isBefore(dayjs()) : false
    if (status === 'enabled') return item.enabled && !expired
    if (status === 'disabled') return !item.enabled && !expired
    if (status === 'expired') return expired
    return true
  })

  return (
    <>
      <ApiKeysToolbar
        search={search}
        status={status}
        itemCount={items.length}
        filteredCount={filteredItems.length}
        onSearchChange={(value) => {
          setSearch(value)
          setPage(1)
        }}
        onStatusChange={(value) => {
          setStatus(value)
          setPage(1)
        }}
        onCreate={() => setOpen(true)}
      />
      <ApiKeysTable
        items={filteredItems}
        itemCount={items.length}
        loading={loading}
        page={page}
        onPageChange={setPage}
        onToggle={(id, enabled) => void toggle(id, enabled)}
        onEditLimits={openLimits}
        onRotate={(id) => void rotate(id)}
        onRemove={(id) => void remove(id)}
      />
      <ApiKeyCreateModal
        open={open}
        createdKey={createdKey}
        saving={saving}
        form={form}
        onCancel={closeModal}
        onCreate={() => void create()}
      />
      <ApiKeyLimitsModal
        open={limitsOpen}
        editing={editing}
        saving={limitsSaving}
        form={limitsForm}
        onCancel={() => setLimitsOpen(false)}
        onSave={() => void saveLimits()}
      />
    </>
  )
}
