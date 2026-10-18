import { ClockCircleOutlined, WalletOutlined } from '@ant-design/icons'
import { Card, Progress, Skeleton, Space, Tag, Tooltip, Typography } from 'antd'
import dayjs from 'dayjs'
import type { Provider, ProviderQuotaOverview } from '../../api'
import {
  formatQuotaAmount,
  providerQuotaWindowGroups,
  quotaStateColors,
  quotaStateLabels,
  quotaWindowLabel,
  type ProviderQuotaWindowGroup,
} from './quotaPresentation'

type ProviderQuotaCardsProps = {
  providers: Provider[]
  overviews: ProviderQuotaOverview[]
  loading: boolean
  onOpenQuota: (provider: Provider) => void
}

function stateStrokeColor(state: string) {
  if (state === 'exhausted') return '#cf1322'
  if (state === 'low' || state === 'reset_due') return '#d46b08'
  if (state === 'healthy') return '#389e0d'
  return '#8c8c8c'
}

function windowValue(group: ProviderQuotaWindowGroup) {
  if (group.remaining == null) return '—'
  const remaining = formatQuotaAmount(group.remaining, group.unit)
  return group.limit == null
    ? remaining
    : `${remaining} / ${formatQuotaAmount(group.limit, group.unit)}`
}

export default function ProviderQuotaCards({
  providers,
  overviews,
  loading,
  onOpenQuota,
}: ProviderQuotaCardsProps) {
  const supportedProviders = providers.filter((provider) => provider.quota_kind)
  if (supportedProviders.length === 0) return null

  const overviewByProvider = new Map(
    overviews.map((overview) => [overview.provider_id, overview]),
  )

  return (
    <section className="provider-quota-overview">
      <div className="provider-quota-overview-header">
        <Typography.Title level={5}>提供商配额</Typography.Title>
        <Tag>{supportedProviders.length} 个</Tag>
      </div>
      <div className="provider-quota-overview-grid">
        {supportedProviders.map((provider) => {
          if (loading) {
            return (
              <Card key={provider.id} bordered={false} className="provider-quota-overview-card">
                <Skeleton active paragraph={{ rows: 3 }} title={{ width: '52%' }} />
              </Card>
            )
          }

          const overview = overviewByProvider.get(provider.id)
          const snapshots = overview?.snapshots ?? []
          const windows = providerQuotaWindowGroups(snapshots)
          const singleBalance =
            windows.length === 1 && windows[0].windowKey === 'balance'
          const state =
            windows.find((window) => window.state === 'exhausted')?.state ??
            windows.find((window) => window.state === 'low')?.state ??
            windows.find((window) => window.state === 'reset_due')?.state ??
            windows.find((window) => window.state === 'unknown')?.state ??
            windows[0]?.state ??
            'unknown'
          const observedAt = windows
            .map((window) => window.observedAt)
            .sort((left, right) => Date.parse(right) - Date.parse(left))[0]

          return (
            <Card
              key={provider.id}
              hoverable
              bordered={false}
              className={`provider-quota-overview-card${
                singleBalance ? ' provider-quota-overview-card-balance' : ''
              }`}
              role="button"
              tabIndex={0}
              onClick={() => onOpenQuota(provider)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' || event.key === ' ') {
                  event.preventDefault()
                  onOpenQuota(provider)
                }
              }}
            >
              <div className="provider-quota-overview-card-top">
                <Space size={8}>
                  <WalletOutlined />
                  <Typography.Text strong ellipsis style={{ maxWidth: 220 }}>
                    {provider.name}
                  </Typography.Text>
                  {!provider.enabled && <Tag>已停用</Tag>}
                </Space>
                <Tag color={quotaStateColors[state]}>
                  {quotaStateLabels[state]}
                </Tag>
              </div>

              {windows.length > 0 ? (
                <>
                  <div className="provider-quota-overview-windows">
                    {windows.map((window) => (
                      <div
                        key={`${window.model}-${window.windowKey}-${window.unit}`}
                        className="provider-quota-overview-window"
                      >
                        <div className="provider-quota-overview-window-head">
                          <Space wrap size={[6, 4]}>
                            <Typography.Text strong>
                              {quotaWindowLabel(window.windowKey)}
                            </Typography.Text>
                            {window.model && <Tag>{window.model}</Tag>}
                            {window.keyCount > 1 && <Tag>{window.keyCount} Key</Tag>}
                          </Space>
                          <Typography.Text className="provider-quota-overview-window-value">
                            {windowValue(window)}
                          </Typography.Text>
                        </div>
                        {window.percent != null && (
                          <Progress
                            percent={Math.round(window.percent)}
                            showInfo={false}
                            size="small"
                            strokeColor={stateStrokeColor(window.state)}
                            trailColor="#f0f0f0"
                          />
                        )}
                      </div>
                    ))}
                  </div>
                  <div className="provider-quota-overview-meta">
                    <Tooltip
                      title={dayjs(observedAt).format('YYYY-MM-DD HH:mm:ss')}
                    >
                      <Typography.Text type="secondary">
                        <ClockCircleOutlined />{' '}
                        {dayjs(observedAt).format('MM-DD HH:mm')} 更新
                      </Typography.Text>
                    </Tooltip>
                  </div>
                </>
              ) : (
                <div className="provider-quota-overview-empty">
                  <Typography.Text strong>暂无额度快照</Typography.Text>
                  <Typography.Text type="secondary">
                    等待定时更新或点击查询
                  </Typography.Text>
                </div>
              )}
            </Card>
          )
        })}
      </div>
    </section>
  )
}
