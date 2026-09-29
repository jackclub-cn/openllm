import { lazy, Suspense, useEffect, useState } from 'react'
import {
  ApiOutlined,
  DashboardOutlined,
  KeyOutlined,
  MessageOutlined,
  MenuFoldOutlined,
  MenuUnfoldOutlined,
  NodeIndexOutlined,
  SettingOutlined,
} from '@ant-design/icons'
import { Button, Layout, Menu, Skeleton, Space, Tag, Tooltip, Typography } from 'antd'
import { Navigate, Route, Routes, useLocation, useNavigate } from 'react-router-dom'
import { api, setAdminToken, type Settings } from './api'
import { RealtimeProvider, useRealtime } from './realtime'

const { Header, Sider, Content } = Layout

const Dashboard = lazy(() => import('./pages/Dashboard'))
const Providers = lazy(() => import('./pages/Providers'))
const RoutesPage = lazy(() => import('./pages/Routes'))
const ApiKeys = lazy(() => import('./pages/ApiKeys'))
const Usage = lazy(() => import('./pages/Usage'))
const SettingsPage = lazy(() => import('./pages/Settings'))
const Playground = lazy(() => import('./pages/Playground'))

const navigation = [
  { key: '/', icon: <DashboardOutlined />, label: '仪表盘' },
  { key: '/providers', icon: <ApiOutlined />, label: '提供商' },
  { key: '/routes', icon: <NodeIndexOutlined />, label: '路由' },
  { key: '/playground', icon: <MessageOutlined />, label: '模型调试' },
  { key: '/keys', icon: <KeyOutlined />, label: '访问密钥' },
  { key: '/usage', icon: <DashboardOutlined />, label: '请求日志' },
  { key: '/settings', icon: <SettingOutlined />, label: '设置' },
]

export default function App() {
  const location = useLocation()
  const navigate = useNavigate()
  const [collapsed, setCollapsed] = useState(false)
  const [settings, setSettings] = useState<Settings>()
  const [tokenRevision, setTokenRevision] = useState(0)

  useEffect(() => {
    api.get<Settings>('/api/settings').then(setSettings).catch(() => setSettings(undefined))
  }, [location.pathname])

  const saveAdminToken = (value: string) => {
    setAdminToken(value)
    setTokenRevision((revision) => revision + 1)
  }

  return (
    <RealtimeProvider revision={tokenRevision}>
      <AppLayout
        collapsed={collapsed}
        setCollapsed={setCollapsed}
        settings={settings}
        location={location}
        navigate={navigate}
        onSaveAdminToken={saveAdminToken}
      />
    </RealtimeProvider>
  )
}

function AppLayout({
  collapsed,
  setCollapsed,
  settings,
  location,
  navigate,
  onSaveAdminToken,
}: {
  collapsed: boolean
  setCollapsed: (value: boolean | ((value: boolean) => boolean)) => void
  settings?: Settings
  location: ReturnType<typeof useLocation>
  navigate: ReturnType<typeof useNavigate>
  onSaveAdminToken: (value: string) => void
}) {
  const realtime = useRealtime()

  return (
    <Layout className="app-shell">
      <Sider
        className="app-sider"
        collapsed={collapsed}
        collapsedWidth={64}
        width={224}
        breakpoint="lg"
        onBreakpoint={setCollapsed}
        theme="dark"
      >
        <div className={`brand ${collapsed ? 'brand-collapsed' : ''}`}>
          <div className="brand-mark">O</div>
          {!collapsed && (
            <div>
              <div className="brand-name">OpenLLM</div>
              <div className="brand-caption">Gateway Console</div>
            </div>
          )}
        </div>
        <Menu
          mode="inline"
          theme="dark"
          selectedKeys={[location.pathname]}
          items={navigation}
          onClick={({ key }) => {
            navigate(key)
            if (window.matchMedia('(max-width: 992px)').matches) setCollapsed(true)
          }}
        />
      </Sider>
      <Layout>
        <Header className="app-header">
          <Space size={12}>
            <Button
              type="text"
              aria-label={collapsed ? '展开导航' : '收起导航'}
              icon={collapsed ? <MenuUnfoldOutlined /> : <MenuFoldOutlined />}
              onClick={() => setCollapsed((value) => !value)}
            />
            <Typography.Text strong>
              {navigation.find((item) => item.key === location.pathname)?.label || 'OpenLLM'}
            </Typography.Text>
          </Space>
          <Space>
            <Tooltip title={realtime.connected ? '实时事件已连接' : '实时事件重连中'}>
              <Tag color={realtime.connected ? 'success' : 'default'}>
                {realtime.connected ? '实时在线' : '实时重连'}
              </Tag>
            </Tooltip>
            <Tag color={settings?.admin_auth_enabled ? 'blue' : 'default'}>
              {settings?.admin_auth_enabled ? '管理鉴权已开启' : '本地模式'}
            </Tag>
            {settings && <Typography.Text type="secondary">v{settings.version}</Typography.Text>}
          </Space>
        </Header>
        <Content className="app-content">
          <Suspense fallback={<Skeleton active paragraph={{ rows: 8 }} />}>
            <Routes>
              <Route path="/" element={<Dashboard />} />
              <Route path="/providers" element={<Providers />} />
              <Route path="/routes" element={<RoutesPage />} />
              <Route path="/playground" element={<Playground />} />
              <Route path="/keys" element={<ApiKeys />} />
              <Route path="/usage" element={<Usage />} />
              <Route path="/settings" element={<SettingsPage onSave={onSaveAdminToken} />} />
              <Route path="*" element={<Navigate to="/" replace />} />
            </Routes>
          </Suspense>
        </Content>
      </Layout>
    </Layout>
  )
}
