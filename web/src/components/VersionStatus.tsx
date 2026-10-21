import { ArrowUpOutlined, ReloadOutlined } from '@ant-design/icons'
import { Button, Space, Tooltip, Typography } from 'antd'
import { useCallback, useEffect, useState } from 'react'
import { api, formatError, type UpgradeStatus } from '../api'
import { RELEASES_URL } from '../project'

export default function VersionStatus({ version }: { version: string }) {
  const [status, setStatus] = useState<UpgradeStatus>()
  const [error, setError] = useState('')
  const [checking, setChecking] = useState(false)
  const check = useCallback(async () => {
    setChecking(true)
    try {
      setStatus(await api.get<UpgradeStatus>('/api/maintenance/upgrade-check?official=true'))
      setError('')
    } catch (reason) {
      setError(formatError(reason))
    } finally {
      setChecking(false)
    }
  }, [])

  useEffect(() => {
    void check()
    const handle = window.setInterval(() => {
      if (document.visibilityState === 'visible') void check()
    }, 3_600_000)
    return () => window.clearInterval(handle)
  }, [check, version])

  return (
    <Space size={8} className="version-status">
      <Typography.Text type="secondary">v{version}</Typography.Text>
      {status?.update_available ? (
        <Tooltip title={`当前 v${version}，最新 v${status.latest_version}`}>
          <Button
            size="small"
            type="link"
            icon={<ArrowUpOutlined />}
            href={RELEASES_URL}
            target="_blank"
            rel="noopener noreferrer"
          >
            更新至 v{status.latest_version}
          </Button>
        </Tooltip>
      ) : (
        <Tooltip title={error || status?.message || '检查版本更新'}>
          <Button
            size="small"
            type="text"
            icon={<ReloadOutlined spin={checking} />}
            loading={checking}
            onClick={() => void check()}
          >
            {checking ? '检查中' : error || status?.status === 'unknown'
              ? '检查更新' : status?.status === 'up_to_date' ? '已是最新' : '检查更新'}
          </Button>
        </Tooltip>
      )}
    </Space>
  )
}
