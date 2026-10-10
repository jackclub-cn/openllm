import { useEffect, useState } from 'react'
import { App, Modal, Space, Table, Tag, Tooltip, Typography } from 'antd'
import {
  api,
  formatError,
  type Provider,
  type ProviderKeyTestResult,
} from '../../api'

type ProviderKeyTestModalProps = {
  provider?: Provider
  onClose: () => void
}

export default function ProviderKeyTestModal({
  provider,
  onClose,
}: ProviderKeyTestModalProps) {
  const { message } = App.useApp()
  const [loading, setLoading] = useState(false)
  const [result, setResult] = useState<ProviderKeyTestResult>()

  useEffect(() => {
    if (!provider) return
    let cancelled = false
    setResult(undefined)
    setLoading(true)
    api
      .post<ProviderKeyTestResult>(`/api/providers/${provider.id}/keys/test`)
      .then((next) => {
        if (cancelled) return
        setResult(next)
        if (next.failed > 0) {
          message.warning(`密钥检测完成：${next.ok} 正常，${next.failed} 异常`)
        } else {
          message.success(`密钥检测完成：${next.ok} 把密钥全部正常`)
        }
      })
      .catch((error) => {
        if (cancelled) return
        message.error(formatError(error))
        onClose()
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [provider])

  return (
    <Modal
      title={provider ? `密钥检测 · ${provider.name}` : '密钥检测'}
      open={Boolean(provider)}
      onCancel={onClose}
      footer={null}
      width={780}
      destroyOnHidden
    >
      <Space direction="vertical" size={12} style={{ width: '100%' }}>
        {result && (
          <Space wrap>
            <Tag color="green">正常 {result.ok}</Tag>
            <Tag color={result.failed ? 'red' : 'default'}>异常 {result.failed}</Tag>
            <Tag>共 {result.total} 把</Tag>
            {result.model && (
              <Typography.Text type="secondary">检测模型 {result.model}</Typography.Text>
            )}
          </Space>
        )}
        <Table
          rowKey={(record) =>
            String(record.key_id ?? `${record.key_name}-${record.api_key_suffix}`)
          }
          size="small"
          loading={loading}
          dataSource={result?.results || []}
          pagination={false}
          locale={{ emptyText: loading ? '正在检测...' : '暂无启用密钥' }}
          columns={[
            {
              title: '密钥',
              dataIndex: 'key_name',
              width: 180,
              render: (value: string, record) => (
                <div>
                  <Typography.Text strong>
                    {value || `Key ${record.key_id ?? ''}`}
                  </Typography.Text>
                  <div>
                    <Typography.Text type="secondary">
                      尾号 {record.api_key_suffix || '****'}
                    </Typography.Text>
                  </div>
                </div>
              ),
            },
            {
              title: '结果',
              dataIndex: 'ok',
              width: 80,
              render: (value: boolean) => (
                <Tag color={value ? 'success' : 'error'}>{value ? '正常' : '异常'}</Tag>
              ),
            },
            {
              title: '检测方式',
              dataIndex: 'checked',
              width: 110,
              render: (value: string) =>
                value === 'inference' ? '推理凭证' : '仅主机可达',
            },
            {
              title: '耗时',
              dataIndex: 'latency_ms',
              width: 90,
              render: (value: number) => `${value} ms`,
            },
            {
              title: '详情',
              dataIndex: 'message',
              render: (value: string) => (
                <Tooltip title={value}>
                  <Typography.Text
                    type="secondary"
                    ellipsis
                    style={{ display: 'block', maxWidth: 260 }}
                  >
                    {value}
                  </Typography.Text>
                </Tooltip>
              ),
            },
          ]}
        />
      </Space>
    </Modal>
  )
}
