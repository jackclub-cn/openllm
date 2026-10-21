import { Alert, Drawer, Tabs } from 'antd'
import type { RequestActivity } from '../../api'
import useRequestActivity from '../../hooks/useRequestActivity'
import useCompletedRequest from '../../hooks/useCompletedRequest'
import RouteTracePanel from '../usage/RouteTracePanel'
import UsageOverview from '../usage/UsageOverview'
import ActivityDetailPanel from './ActivityDetailPanel'
import { useNavigate } from 'react-router-dom'

export default function ActivityDetailDrawer({
  initial,
  onClose,
}: {
  initial?: RequestActivity
  onClose: () => void
}) {
  const { activity, missing } = useRequestActivity(initial?.request_id, initial)
  const completed = missing || Boolean(activity && activity.outcome !== 'running')
  const persisted = useCompletedRequest(initial?.request_id, completed)
  const navigate = useNavigate()
  return (
    <Drawer title="请求详情" width={760} open={Boolean(initial)} onClose={onClose} destroyOnHidden>
      {activity && <Tabs items={[
        { key: 'live', label: completed ? '请求链路' : '实时状态', children:
          missing ? <Alert type="info" message="实时记录已过期，可查看请求日志和路由轨迹" /> : <ActivityDetailPanel activity={activity} /> },
        ...(completed ? [
          {
            key: 'usage', label: '请求日志',
            children: persisted.detail && !persisted.detail.in_flight
              ? <UsageOverview detail={persisted.detail} onDiagnose={(detail) => {
                navigate(`/usage?request_id=${encodeURIComponent(detail.request_id)}`)
              }} />
              : <Alert type="info" message={persisted.loading ? '正在同步最终请求日志' : '请求日志暂不可用'} description={persisted.detailError || undefined} />,
          },
          {
            key: 'trace', label: '路由轨迹',
            children: <RouteTracePanel trace={persisted.trace} loading={persisted.loading && !persisted.trace} error={persisted.traceError || undefined} />,
          },
        ] : []),
      ]} />}
    </Drawer>
  )
}
