import {
  FileSearchOutlined,
  SafetyCertificateOutlined,
  SafetyOutlined,
} from '@ant-design/icons'
import { Alert, Space, Tabs } from 'antd'
import { Link, Navigate, useLocation, useNavigate } from 'react-router-dom'
import PageHeader from '../components/PageHeader'
import AccessSecurityTab from './settings/AccessSecurityTab'
import LoggingPrivacyTab from './settings/LoggingPrivacyTab'
import RequestPolicyTab from './settings/RequestPolicyTab'
import { SettingsEditsProvider, SettingsNavigationGuard } from './settings/SettingsEdits'
import useAccessSettings from './settings/useAccessSettings'

const tabs = ['requests', 'logging', 'access']

export default function SettingsPage() {
  return (
    <SettingsEditsProvider>
      <SettingsContent />
    </SettingsEditsProvider>
  )
}

function SettingsContent() {
  const location = useLocation()
  const navigate = useNavigate()
  const access = useAccessSettings()
  const pathTab = location.pathname.split('/')[2]
  const searchParams = new URLSearchParams(location.search)
  const activeTab = pathTab || searchParams.get('tab') || 'requests'

  if (!pathTab || !tabs.includes(pathTab)) {
    searchParams.delete('tab')
    const search = searchParams.toString()
    const tab = tabs.includes(activeTab) ? activeTab : 'requests'
    return <Navigate replace to={`/settings/${tab}${search ? `?${search}` : ''}`} />
  }

  return (
    <div className="settings-page">
      <SettingsNavigationGuard />
      <PageHeader
        title="设置"
        description="配置请求处理、日志记录和控制台访问策略"
      />
      {access.settings?.admin_auth_enabled === false && activeTab !== 'access' && (
        <Alert
          className="page-alert"
          type="warning"
          showIcon
          title="管理鉴权未开启"
          action={<Link to="/settings/access">查看访问安全</Link>}
        />
      )}
      <Tabs
        activeKey={activeTab}
        destroyOnHidden
        onChange={(key) => navigate(`/settings/${key}`)}
        items={[
          {
            key: 'requests',
            label: <Space><SafetyCertificateOutlined />请求策略</Space>,
            children: <RequestPolicyTab />,
          },
          {
            key: 'logging',
            label: <Space><FileSearchOutlined />日志与隐私</Space>,
            children: <LoggingPrivacyTab />,
          },
          {
            key: 'access',
            label: <Space><SafetyOutlined />访问安全</Space>,
            children: <AccessSecurityTab state={access} />,
          },
        ]}
      />
    </div>
  )
}
