import { Drawer, Tabs } from 'antd'
import type { UsageLog } from '../../api'
import ContextBreakdownPanel from '../../components/ContextBreakdownPanel'
import useRequestActivity from '../../hooks/useRequestActivity'
import useCompletedRequest from '../../hooks/useCompletedRequest'
import ActivityDetailPanel from '../dashboard/ActivityDetailPanel'
import RouteTracePanel from './RouteTracePanel'
import UsageOverview from './UsageOverview'

type UsageDetailDrawerProps = {
  detail?: UsageLog
  loading: boolean
  onClose: () => void
  onDiagnose: (detail: UsageLog) => void
}

export default function UsageDetailDrawer({
  detail,
  loading,
  onClose,
  onDiagnose,
}: UsageDetailDrawerProps) {
  const { activity, missing } = useRequestActivity(detail?.request_id)
  const completed = missing || Boolean(detail && (!detail.in_flight || (activity && activity.outcome !== 'running')))
  const persisted = useCompletedRequest(detail?.request_id, completed)
  const currentDetail = persisted.detail && !persisted.detail.in_flight ? persisted.detail : detail

  return (
    <Drawer
      title="请求详情"
      width={760}
      open={Boolean(detail)}
      onClose={onClose}
      loading={loading}
    >
      {detail && (
        <Tabs
          items={[
            {
              key: 'overview',
              label: '请求详情',
              children: (
                <UsageOverview detail={currentDetail || detail} onDiagnose={onDiagnose} />
              ),
            },
            {
              key: 'trace',
              label: '路由轨迹',
              children: (
                <RouteTracePanel trace={persisted.trace} loading={persisted.loading && !persisted.trace} error={persisted.traceError || undefined} />
              ),
            },
            {
              key: 'context',
              label: '上下文',
              children: <ContextBreakdownPanel context={currentDetail?.context || activity?.context} />,
            },
            ...(activity ? [{
              key: 'activity', label: completed ? '请求链路' : '实时状态',
              children: <ActivityDetailPanel activity={activity} />,
            }] : []),
          ]}
        />
      )}
    </Drawer>
  )
}
