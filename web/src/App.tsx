import { lazy, Suspense, useEffect, useState } from 'react'
import {
  ApiOutlined,
  AppstoreOutlined,
  AuditOutlined,
  DashboardOutlined,
  FileSearchOutlined,
  KeyOutlined,
  LineChartOutlined,
  MessageOutlined,
  MenuFoldOutlined,
  MenuUnfoldOutlined,
  NodeIndexOutlined,
  SafetyCertificateOutlined,
  SettingOutlined,
  ToolOutlined,
} from '@ant-design/icons'
import { Button, Layout, Menu, Skeleton, Space, Spin, Tag, Tooltip, Typography } from 'antd'
import { Navigate, Route, Routes, useLocation, useNavigate } from 'react-router-dom'
import {
  api,
  ApiError,
  getAdminToken,
  setAdminToken,
  type RuntimeStatus,
  type Settings,
} from './api'
import { RealtimeProvider, useRealtime } from './realtime'
import AdminLogin from './pages/AdminLogin'
import GithubLink from './components/GithubLink'
import VersionStatus from './components/VersionStatus'

const { Header, Sider, Content } = Layout

const Dashboard = lazy(() => import('./pages/Dashboard'))
const Analytics = lazy(() => import('./pages/Analytics'))
const ModelCatalog = lazy(() => import('./pages/ModelCatalog'))
const Providers = lazy(() => import('./pages/Providers'))
const RoutesPage = lazy(() => import('./pages/Routes'))
const ApiKeys = lazy(() => import('./pages/ApiKeys'))
const ProviderPools = lazy(() => import('./pages/ProviderPools'))
const Usage = lazy(() => import('./pages/Usage'))
const Webhooks = lazy(() => import('./pages/Webhooks'))
const AuditLogs = lazy(() => import('./pages/AuditLogs'))
const SettingsPage = lazy(() => import('./pages/Settings'))
const Playground = lazy(() => import('./pages/Playground'))
const DiagnosticsPage = lazy(() => import('./pages/Diagnostics'))
const MaintenancePage = lazy(() => import('./pages/Maintenance'))

const navigation = [
  { key: '/', icon: <DashboardOutlined />, label: '仪表盘' },
  { key: '/analytics', icon: <LineChartOutlined />, label: '用量分析' },
  { key: '/keys', icon: <KeyOutlined />, label: '访问密钥' },
  { key: '/providers', icon: <ApiOutlined />, label: '提供商' },
  { key: '/models', icon: <AppstoreOutlined />, label: '模型目录' },
  { key: '/playground', icon: <MessageOutlined />, label: '模型调试' },
  { key: '/routes', icon: <NodeIndexOutlined />, label: '路由' },
  { key: '/usage', icon: <FileSearchOutlined />, label: '请求日志' },
  { key: '/audit', icon: <AuditOutlined />, label: '审计日志' },
  { key: '/diagnostics', icon: <SafetyCertificateOutlined />, label: '诊断' },
  { key: '/maintenance', icon: <ToolOutlined />, label: '运维' },
  { key: '/settings', icon: <SettingOutlined />, label: '设置' },
]

export default function App() {
  const location = useLocation()
  const navigate = useNavigate()
  const [collapsed, setCollapsed] = useState(false)
  const [settings, setSettings] = useState<Settings>()
  const [tokenRevision, setTokenRevision] = useState(0)
  const [access, setAccess] = useState<'checking' | 'authenticated' | 'required'>(
    'checking'
  )
  const [accessError, setAccessError] = useState('')

  useEffect(() => {
    let cancelled = false
    api
      .get<Settings>('/api/settings')
      .then((next) => {
        if (cancelled) return
        setSettings(next)
        if (!next.admin_auth_enabled) setAccess('authenticated')
      })
      .catch(() => {
        if (!cancelled) setAccess('authenticated')
      })
    return () => {
      cancelled = true
    }
  }, [location.pathname])

  useEffect(() => {
    if (!settings?.admin_auth_enabled) return
    const token = getAdminToken()
    if (!token) {
      setAccess('required')
      return
    }
    let cancelled = false
    setAccess('checking')
    api
      .get<RuntimeStatus>('/api/status')
      .then(() => {
        if (!cancelled) setAccess('authenticated')
      })
      .catch((error) => {
        if (cancelled) return
        if (error instanceof ApiError && error.status === 401) {
          setAdminToken('')
          setAccessError('浏览器中保存的管理令牌无效，请重新输入。')
          setAccess('required')
          return
        }
        // Keep the console usable when the gateway itself is transiently unavailable.
        setAccess('authenticated')
      })
    return () => {
      cancelled = true
    }
  }, [settings?.admin_auth_enabled])

  useEffect(() => {
    if (access !== 'required' || location.pathname === '/login') return
    navigate('/login', {
      replace: true,
      state: {
        from: `${location.pathname}${location.search}${location.hash}`,
      },
    })
  }, [access, location.hash, location.pathname, location.search, navigate])

  const saveAdminToken = async (value: string) => {
    const previous = getAdminToken()
    setAdminToken(value)
    try {
      await api.get<RuntimeStatus>('/api/status')
    } catch (error) {
      setAdminToken(previous)
      throw error
    }
    setAccessError('')
    setAccess('authenticated')
    setTokenRevision((revision) => revision + 1)
    const from = (location.state as { from?: string } | null)?.from
    navigate(from && from !== '/login' ? from : '/', { replace: true })
  }

  if (access === 'checking') {
    return (
      <div className="auth-screen">
        <Spin size="large" />
      </div>
    )
  }

  if (access === 'required' || (location.pathname === '/login' && settings?.admin_auth_enabled)) {
    return (
      <AdminLogin
        version={settings?.version}
        updating={access === 'authenticated'}
        initialError={accessError}
        onSubmit={saveAdminToken}
        onBack={access === 'authenticated' ? () => navigate('/') : undefined}
      />
    )
  }

  return (
    <RealtimeProvider revision={tokenRevision}>
      <AppLayout
        collapsed={collapsed}
        setCollapsed={setCollapsed}
        settings={settings}
        location={location}
        navigate={navigate}
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
}: {
  collapsed: boolean
  setCollapsed: (value: boolean | ((value: boolean) => boolean)) => void
  settings?: Settings
  location: ReturnType<typeof useLocation>
  navigate: ReturnType<typeof useNavigate>
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
          selectedKeys={[location.pathname.startsWith('/settings/') ? '/settings' : location.pathname]}
          items={navigation}
          onClick={({ key }) => {
            navigate(key)
            if (window.matchMedia('(max-width: 992px)').matches) setCollapsed(true)
          }}
        />
        <GithubLink collapsed={collapsed} />
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
              {navigation.find((item) =>
                item.key === location.pathname ||
                (item.key === '/settings' && location.pathname.startsWith('/settings/'))
              )?.label || 'OpenLLM'}
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
            {settings && <VersionStatus version={settings.version} />}
          </Space>
        </Header>
        <Content className="app-content">
          <Suspense fallback={<Skeleton active paragraph={{ rows: 8 }} />}>
            <Routes>
              <Route path="/" element={<Dashboard />} />
              <Route path="/analytics" element={<Analytics />} />
              <Route path="/providers" element={<Providers />} />
              <Route path="/models" element={<ModelCatalog />} />
              <Route path="/routes" element={<RoutesPage />} />
              <Route path="/playground" element={<Playground />} />
              <Route path="/keys" element={<ApiKeys />} />
              <Route path="/pools" element={<ProviderPools />} />
              <Route path="/usage" element={<Usage />} />
              <Route path="/webhooks" element={<Webhooks />} />
              <Route path="/audit" element={<AuditLogs />} />
              <Route path="/diagnostics" element={<DiagnosticsPage />} />
              <Route path="/maintenance" element={<MaintenancePage />} />
              <Route path="/settings/*" element={<SettingsPage />} />
              <Route path="*" element={<Navigate to="/" replace />} />
            </Routes>
          </Suspense>
        </Content>
      </Layout>
    </Layout>
  )
}
