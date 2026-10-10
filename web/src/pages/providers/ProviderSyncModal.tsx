import { useEffect, useState } from 'react'
import { App, Modal, Space, Tag, Typography } from 'antd'
import { api, formatError, type Provider } from '../../api'
import { syncFieldLabels, type ModelSyncPreview } from './types'

type ProviderSyncModalProps = {
  provider?: Provider
  onClose: () => void
  onApplied: () => Promise<void>
}

export default function ProviderSyncModal({
  provider,
  onClose,
  onApplied,
}: ProviderSyncModalProps) {
  const { message } = App.useApp()
  const [previewing, setPreviewing] = useState(false)
  const [applying, setApplying] = useState(false)
  const [preview, setPreview] = useState<ModelSyncPreview>()

  useEffect(() => {
    if (!provider) return
    let cancelled = false
    setPreview(undefined)
    setPreviewing(true)
    api
      .post<ModelSyncPreview>(`/api/providers/${provider.id}/models/preview`)
      .then((next) => {
        if (!cancelled) setPreview(next)
      })
      .catch((error) => {
        if (cancelled) return
        message.error(formatError(error))
        onClose()
      })
      .finally(() => {
        if (!cancelled) setPreviewing(false)
      })
    return () => {
      cancelled = true
    }
  }, [provider])

  const apply = async () => {
    if (!provider) return
    const key = `provider-sync-${provider.id}`
    setApplying(true)
    message.loading({ content: '正在应用模型同步...', key })
    try {
      const result = await api.post<{ ok: boolean; count: number; message: string }>(
        `/api/providers/${provider.id}/models/sync`,
      )
      message.success({ content: `已同步 ${result.count} 个模型`, key })
      onClose()
      await onApplied()
    } catch (error) {
      message.error({ content: formatError(error), key, duration: 6 })
      try {
        await onApplied()
      } catch {
        // Keep the original sync error visible when the refresh also fails.
      }
    } finally {
      setApplying(false)
    }
  }

  return (
    <Modal
      title={provider ? `同步预览 · ${provider.name}` : '同步预览'}
      open={Boolean(provider)}
      onCancel={onClose}
      onOk={() => void apply()}
      confirmLoading={previewing || applying}
      okButtonProps={{ disabled: !preview }}
      okText="应用同步"
      width={640}
      destroyOnHidden
    >
      {preview && (
        <Space direction="vertical" size={12} style={{ width: '100%' }}>
          <Space wrap>
            <Tag color="green">新增 {preview.added.length}</Tag>
            <Tag color="red">移除 {preview.removed.length}</Tag>
            <Tag color="orange">变更 {preview.changed.length}</Tag>
            <Tag color="blue">保留 {preview.retained}</Tag>
            <Tag>停用保留 {preview.disabled_retained}</Tag>
          </Space>
          {[
            ['新增模型', preview.added, 'green'],
            ['移除模型', preview.removed, 'red'],
          ].map(([label, models, color]) =>
            (models as string[]).length ? (
              <div key={label as string}>
                <Typography.Text strong>{label as string}</Typography.Text>
                <div
                  style={{
                    maxHeight: 180,
                    overflow: 'auto',
                    marginTop: 6,
                    border: '1px solid #f0f0f0',
                    borderRadius: 6,
                    padding: 8,
                  }}
                >
                  <Space wrap size={[4, 4]}>
                    {(models as string[]).map((model) => (
                      <Tag key={model} color={color as string}>
                        {model}
                      </Tag>
                    ))}
                  </Space>
                </div>
              </div>
            ) : null,
          )}
          {preview.changed.length > 0 && (
            <div>
              <Typography.Text strong>元数据变更</Typography.Text>
              <div
                style={{
                  maxHeight: 180,
                  overflow: 'auto',
                  marginTop: 6,
                  border: '1px solid #f0f0f0',
                  borderRadius: 6,
                  padding: 8,
                }}
              >
                <Space direction="vertical" size={6} style={{ width: '100%' }}>
                  {preview.changed.map((change) => (
                    <Space key={change.model_name} wrap>
                      <Typography.Text code>{change.model_name}</Typography.Text>
                      {change.fields.map((field) => (
                        <Tag key={field} color="orange">
                          {syncFieldLabels[field] || field}
                        </Tag>
                      ))}
                    </Space>
                  ))}
                </Space>
              </div>
            </div>
          )}
          {!preview.added.length && !preview.removed.length && !preview.changed.length && (
            <Typography.Text type="secondary">
              上游模型列表与本地一致，应用后不会改变模型数据。
            </Typography.Text>
          )}
        </Space>
      )}
    </Modal>
  )
}
