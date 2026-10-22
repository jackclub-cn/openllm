import { Tabs } from 'antd'
import type { Dayjs } from 'dayjs'
import type { AnalyticsGranularity, Overview } from '../../api'
import type { UsageDimensionFilterValues } from '../../components/UsageDimensionFilters'
import DashboardModelUsage from '../dashboard/DashboardModelUsage'
import AnalyticsContext from './AnalyticsContext'
import AnalyticsDimensions from './AnalyticsDimensions'
import AnalyticsOverview from './AnalyticsOverview'
import AnalyticsProviderUsage from './AnalyticsProviderUsage'
import AnalyticsRequests from './AnalyticsRequests'
import AnalyticsRouteQuality from './AnalyticsRouteQuality'
import AnalyticsSessions from './AnalyticsSessions'

type AnalyticsTabsProps = {
  data: Overview
  dates: [Dayjs, Dayjs]
  filters: UsageDimensionFilterValues
  granularity: AnalyticsGranularity
  fullBreakdown: boolean
  providerDataMocked: boolean
  refreshing: boolean
  onToggleFullBreakdown: () => void
}

export default function AnalyticsTabs({
  data,
  dates,
  filters,
  granularity,
  fullBreakdown,
  providerDataMocked,
  refreshing,
  onToggleFullBreakdown,
}: AnalyticsTabsProps) {
  return (
    <Tabs
      className="section-row"
      defaultActiveKey="overview"
      items={[
        {
          key: 'overview',
          label: '概览',
          children: (
            <AnalyticsOverview
              data={data}
              providerDataMocked={providerDataMocked}
            />
          ),
        },
        {
          key: 'requests',
          label: '请求',
          children: <AnalyticsRequests data={data} dates={dates} />,
        },
        {
          key: 'context',
          label: '上下文',
          children: (
            <AnalyticsContext
              dates={dates}
              filters={filters}
              granularity={granularity}
            />
          ),
        },
        {
          key: 'sessions',
          label: '会话',
          children: (
            <AnalyticsSessions
              data={data}
              dates={dates}
              filters={filters}
              granularity={granularity}
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
              onToggleFullBreakdown={onToggleFullBreakdown}
              loading={refreshing}
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
          label: '分组',
          children: (
            <AnalyticsDimensions
              dates={dates}
              filters={filters}
              granularity={granularity}
            />
          ),
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
  )
}
