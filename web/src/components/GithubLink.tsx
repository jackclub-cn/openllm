import { GithubOutlined } from '@ant-design/icons'
import { Tooltip } from 'antd'
import { GITHUB_URL } from '../project'

export default function GithubLink({ collapsed }: { collapsed: boolean }) {
  return (
    <div className="sider-footer">
      <Tooltip title={collapsed ? 'GitHub' : undefined} placement="right">
        <a
          className={`sider-github${collapsed ? ' sider-github-collapsed' : ''}`}
          href={GITHUB_URL}
          target="_blank"
          rel="noopener noreferrer"
          aria-label="OpenLLM GitHub"
        >
          <GithubOutlined />
          {!collapsed && <span>jackclub-cn/openllm</span>}
        </a>
      </Tooltip>
    </div>
  )
}
