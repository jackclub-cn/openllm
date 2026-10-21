import { LoginOutlined, ReloadOutlined, SafetyCertificateOutlined } from '@ant-design/icons'
import { Alert, Button, Descriptions, Skeleton, Space, Tag, Typography } from 'antd'
import { useNavigate } from 'react-router-dom'
import type useAccessSettings from './useAccessSettings'

export default function AccessSecurityTab({
  state,
}: {
  state: ReturnType<typeof useAccessSettings>
}) {
  const navigate = useNavigate()
  const { settings, loading, error, reload } = state

  return (
    <section className="settings-section" aria-label="管理鉴权">
      <div className="settings-section-heading">
        <Typography.Title level={4}>
          <Space><SafetyCertificateOutlined />管理鉴权</Space>
        </Typography.Title>
      </div>
      {loading ? (
        <Skeleton active paragraph={{ rows: 3 }} />
      ) : error ? (
        <Alert
          type="error"
          title="管理鉴权状态加载失败"
          description={error}
          action={<Button icon={<ReloadOutlined />} onClick={reload}>重试</Button>}
        />
      ) : (
        <>
          {settings?.admin_auth_enabled ? (
            <Alert
              type="success"
              showIcon
              message="管理鉴权已开启"
              description="管理令牌在独立登录页中填写和更新，不在设置页保存。"
              action={
                <Button icon={<LoginOutlined />} onClick={() => navigate('/login')}>
                  打开管理登录
                </Button>
              }
            />
          ) : (
            <Alert
              type="warning"
              showIcon
              message="当前服务未启用管理鉴权"
              description="配置 OPENLLM_ADMIN_TOKEN 并重启服务后生效。管理 API 暴露在本机或网络边界之外时存在风险。"
            />
          )}
          <Descriptions column={1} size="small" style={{ marginTop: 20 }}>
            <Descriptions.Item label="鉴权状态">
              <Tag color={settings?.admin_auth_enabled ? 'success' : 'default'}>
                {settings?.admin_auth_enabled ? '已开启' : '未开启'}
              </Tag>
            </Descriptions.Item>
            <Descriptions.Item label="配置方式">
              服务启动时通过环境变量设置，控制台内不可修改
            </Descriptions.Item>
          </Descriptions>
        </>
      )}
    </section>
  )
}
