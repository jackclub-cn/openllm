import { useEffect, useRef, useState } from 'react'
import { LinkOutlined } from '@ant-design/icons'
import {
  Alert,
  Card,
  Modal,
  Progress,
  Select,
  Space,
  Table,
  Tag,
  Typography,
} from 'antd'
import dayjs from 'dayjs'
import { api, formatError, type Provider, type ProviderQuota } from '../../api'
import { formatUnitPrice } from './types'

type ProviderQuotaModalProps = {
  provider?: Provider
  onClose: () => void
}

export default function ProviderQuotaModal({
  provider,
  onClose,
}: ProviderQuotaModalProps) {
  const [loading, setLoading] = useState(false)
  const [quota, setQuota] = useState<ProviderQuota>()
  const [keyId, setKeyId] = useState<number>()
  const [error, setError] = useState<string>()
  const requestSeq = useRef(0)

  const loadQuota = async (nextProvider: Provider, nextKeyId?: number) => {
    const request = ++requestSeq.current
    setLoading(true)
    setError(undefined)
    try {
      const query = nextKeyId == null ? '' : `?key_id=${nextKeyId}`
      const next = await api.get<ProviderQuota>(
        `/api/providers/${nextProvider.id}/quota${query}`,
      )
      if (request !== requestSeq.current) return
      setQuota(next)
    } catch (requestError) {
      if (request !== requestSeq.current) return
      setQuota(undefined)
      setError(formatError(requestError))
    } finally {
      if (request === requestSeq.current) setLoading(false)
    }
  }

  useEffect(() => {
    if (!provider) return
    const defaultKey = provider.api_keys.find((key) => key.enabled) ?? provider.api_keys[0]
    setQuota(undefined)
    setKeyId(defaultKey?.id)
    setError(undefined)
    void loadQuota(provider, defaultKey?.id)
    return () => {
      requestSeq.current += 1
    }
  }, [provider])

  const close = () => {
    requestSeq.current += 1
    onClose()
  }

  return (
    <Modal
      title={provider ? `额度与价格 · ${provider.name}` : '额度与价格'}
      open={Boolean(provider)}
      onCancel={close}
      footer={null}
      width={860}
      destroyOnHidden
    >
      {provider && provider.api_keys.length > 0 && (
        <Space wrap style={{ marginBottom: 16 }}>
          <Typography.Text type="secondary">查询密钥</Typography.Text>
          <Select
            value={keyId}
            loading={loading}
            style={{ minWidth: 280 }}
            onChange={(value) => {
              setKeyId(value)
              void loadQuota(provider, value)
            }}
            options={provider.api_keys.map((key) => ({
              value: key.id,
              label: `${key.name} (${key.api_key_suffix})${key.enabled ? '' : ' · 已停用'}`,
            }))}
          />
        </Space>
      )}
      {loading && <Typography.Text type="secondary">正在向上游查询额度...</Typography.Text>}
      {error && (
        <Alert
          type="error"
          showIcon
          message="额度查询失败"
          description={error}
          style={{ marginBottom: 16 }}
        />
      )}
      {quota && (
        <Space direction="vertical" size={18} style={{ width: '100%' }}>
          <Space wrap>
            <Typography.Text strong>{quota.title}</Typography.Text>
            {quota.plan_name && <Tag color="blue">{quota.plan_name}</Tag>}
            {quota.key_name && (
              <Tag>
                {quota.key_name} · {quota.key_suffix}
              </Tag>
            )}
            <Typography.Text type="secondary">
              查询于 {dayjs(quota.fetched_at).format('YYYY-MM-DD HH:mm:ss')}
            </Typography.Text>
            {quota.source_url && (
              <Typography.Link href={quota.source_url} target="_blank">
                <Space size={4}>
                  <LinkOutlined />
                  官方页
                </Space>
              </Typography.Link>
            )}
          </Space>

          {quota.items.length > 0 && (
            <div className="provider-quota-grid">
              {quota.items.map((item) => (
                <Card key={item.key} size="small" className="provider-quota-card">
                  <Typography.Text type="secondary">{item.label}</Typography.Text>
                  {item.percent != null ? (
                    <>
                      <Progress
                        percent={Math.round(item.percent)}
                        status={item.percent >= 100 ? 'exception' : 'normal'}
                        strokeColor={item.percent >= 80 ? '#d46b08' : '#1677ff'}
                      />
                      {item.used != null && item.limit != null && (
                        <Typography.Text type="secondary">
                          {item.unit === 'USD' ? '$' : ''}
                          {item.used.toFixed(2)} / {item.unit === 'USD' ? '$' : ''}
                          {item.limit.toFixed(2)} {item.unit !== 'USD' ? item.unit : ''}
                        </Typography.Text>
                      )}
                    </>
                  ) : item.remaining != null ? (
                    <div className="provider-quota-value">
                      <Typography.Title level={3}>
                        {item.unit === 'USD' ? '$' : item.unit === 'CNY' ? '¥' : ''}
                        {item.remaining.toFixed(2)}
                        <span>
                          {item.unit !== 'CNY' && item.unit !== 'USD' ? ` ${item.unit}` : ''}
                        </span>
                      </Typography.Title>
                    </div>
                  ) : (
                    <Typography.Text type="secondary">暂无数据</Typography.Text>
                  )}
                  {item.reset_at && (
                    <Typography.Text type="secondary" className="provider-quota-reset">
                      重置于 {dayjs(item.reset_at).format('MM-DD HH:mm')}
                    </Typography.Text>
                  )}
                </Card>
              ))}
            </div>
          )}

          {quota.details.length > 0 && (
            <Space wrap>
              {quota.details.map((detail) => (
                <Tag key={`${detail.label}-${detail.value}`}>
                  {detail.label}：{detail.value}
                </Tag>
              ))}
            </Space>
          )}

          <div>
            <Typography.Title level={5}>模型价格</Typography.Title>
            <Table
              rowKey="model_name"
              size="small"
              pagination={false}
              dataSource={quota.prices}
              locale={{ emptyText: '暂无已同步价格' }}
              columns={[
                {
                  title: '模型',
                  dataIndex: 'model_name',
                  render: (value: string) => <Typography.Text code>{value}</Typography.Text>,
                },
                {
                  title: '输入 / 1M',
                  dataIndex: 'input',
                  width: 120,
                  render: (value?: number | null) => formatUnitPrice(value),
                },
                {
                  title: '输出 / 1M',
                  dataIndex: 'output',
                  width: 120,
                  render: (value?: number | null) => formatUnitPrice(value),
                },
                {
                  title: '缓存读 / 1M',
                  dataIndex: 'cache_read',
                  width: 130,
                  render: (value?: number | null) => formatUnitPrice(value),
                },
                {
                  title: '缓存写 / 1M',
                  dataIndex: 'cache_write',
                  width: 130,
                  render: (value?: number | null) => formatUnitPrice(value),
                },
              ]}
            />
          </div>
        </Space>
      )}
    </Modal>
  )
}
