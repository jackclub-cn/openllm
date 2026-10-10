import { useEffect, useState } from 'react'
import { PlusOutlined, ReloadOutlined } from '@ant-design/icons'
import { App, Button, Form, Space } from 'antd'
import { api, formatError, type Webhook, type WebhookInput } from '../api'
import PageHeader from '../components/PageHeader'
import WebhookDeliveriesDrawer from './webhooks/WebhookDeliveriesDrawer'
import WebhookEditorModal from './webhooks/WebhookEditorModal'
import WebhooksTable from './webhooks/WebhooksTable'
import type { WebhookFormValues } from './webhooks/types'

export default function WebhooksPage() {
  const { message } = App.useApp()
  const [items, setItems] = useState<Webhook[]>([])
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [togglingId, setTogglingId] = useState<number>()
  const [testingId, setTestingId] = useState<number>()
  const [editing, setEditing] = useState<Webhook>()
  const [open, setOpen] = useState(false)
  const [deliveryWebhook, setDeliveryWebhook] = useState<Webhook>()
  const [form] = Form.useForm<WebhookFormValues>()

  const load = async (showLoading = true) => {
    if (showLoading) setLoading(true)
    try {
      setItems(await api.get<Webhook[]>('/api/webhooks'))
    } catch (error) {
      message.error(formatError(error))
    } finally {
      if (showLoading) setLoading(false)
    }
  }

  useEffect(() => {
    void load()
  }, [])

  const openEditor = (webhook?: Webhook) => {
    setEditing(webhook)
    form.setFieldsValue(
      webhook
        ? {
            name: webhook.name,
            url: webhook.url,
            secret: '',
            headers: JSON.stringify(webhook.headers || {}, null, 2),
            clear_secret: false,
            event_types: webhook.event_types,
            enabled: webhook.enabled,
          }
        : {
            name: '',
            url: '',
            secret: '',
            headers: '{}',
            clear_secret: false,
            event_types: ['request.completed', 'request.failed'],
            enabled: true,
          },
    )
    setOpen(true)
  }

  const save = async () => {
    const values = await form.validateFields()
    const input: WebhookInput = {
      name: values.name,
      url: values.url,
      event_types: values.event_types,
      enabled: values.enabled,
    }
    if (values.secret?.trim()) input.secret = values.secret.trim()
    if (values.headers?.trim()) input.headers = JSON.parse(values.headers)
    if (values.clear_secret) input.clear_secret = true

    setSaving(true)
    try {
      if (editing) await api.put(`/api/webhooks/${editing.id}`, input)
      else await api.post('/api/webhooks', input)
      message.success(editing ? 'Webhook 已更新' : 'Webhook 已创建')
      setOpen(false)
      await load(false)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setSaving(false)
    }
  }

  const toggle = async (webhook: Webhook, enabled: boolean) => {
    setTogglingId(webhook.id)
    try {
      await api.put(`/api/webhooks/${webhook.id}`, { enabled })
      message.success(enabled ? 'Webhook 已启用' : 'Webhook 已停用')
      await load(false)
    } catch (error) {
      message.error(formatError(error))
    } finally {
      setTogglingId(undefined)
    }
  }

  const test = async (webhook: Webhook) => {
    setTestingId(webhook.id)
    try {
      await api.post(`/api/webhooks/${webhook.id}/test`)
      message.success('测试事件投递成功')
      await load(false)
    } catch (error) {
      message.error(formatError(error))
      await load(false)
    } finally {
      setTestingId(undefined)
    }
  }

  const remove = async (id: number) => {
    try {
      await api.delete(`/api/webhooks/${id}`)
      message.success('Webhook 已删除')
      await load(false)
    } catch (error) {
      message.error(formatError(error))
    }
  }

  return (
    <>
      <PageHeader
        title="Webhook"
        description="管理请求事件的外部订阅"
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
              新建 Webhook
            </Button>
          </Space>
        }
      />
      <WebhooksTable
        items={items}
        loading={loading}
        togglingId={togglingId}
        testingId={testingId}
        onToggle={(webhook, enabled) => void toggle(webhook, enabled)}
        onEdit={openEditor}
        onTest={(webhook) => void test(webhook)}
        onDeliveries={setDeliveryWebhook}
        onRemove={(id) => void remove(id)}
      />
      <WebhookEditorModal
        open={open}
        editing={editing}
        saving={saving}
        form={form}
        onCancel={() => setOpen(false)}
        onSave={() => void save()}
      />
      <WebhookDeliveriesDrawer
        webhook={deliveryWebhook}
        onClose={() => setDeliveryWebhook(undefined)}
      />
    </>
  )
}
