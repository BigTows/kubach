# 设置 → AI 代理：MCP HTTP 服务器、连接客户端、活动会话和审计日志。
# 机器翻译草稿，需要母语者审校。

settings-copy = 复制
settings-copied = 已复制到剪贴板

## HTTP 服务器

settings-group-http-server = HTTP 服务器
settings-http-server-description = 所有代理都需要：代理需要权限或更改时，Kubach 在这里询问你。HTTP 代理也直接连接到它（127.0.0.1，使用下方的令牌）。
settings-server-off = 已关闭。
settings-server-starting = 正在启动…
settings-server-listening = 正在监听 { $url }
settings-server-not-running = 未运行。
settings-mcp-http = 通过 HTTP 提供 MCP
settings-mcp-port = 端口
settings-mcp-port-description = 固定端口，重启后代理配置仍然有效。
settings-mcp-token = 令牌
settings-mcp-token-description = { $token } · 保存在设置文件夹中，仅你可读
settings-mcp-token-none = 服务器首次启动时创建。
settings-mcp-token-regenerate = 重新生成
settings-mcp-token-regenerate-tooltip = 使用旧令牌配置的代理将停止工作

## 连接代理

settings-group-connect = 连接代理
settings-connect-description = “连接”会在你查看更改后，把服务器 `kubach`（本应用，`kubach mcp`，无需令牌）写入代理自己的配置。之后代理使用启动时的 context，指定 namespace，Kubach 会在它第一次读取每个 namespace 时在这里询问你。
settings-connect-unavailable = Kubach 无法确定自身路径或你的主文件夹；请手动设置。
settings-manual-setup = 手动设置
settings-manual-setup-description = 用于其他客户端，或通过 HTTP 连接。
settings-manual-show = 显示
settings-manual-hide = 隐藏
settings-snippet-stdio = 任意客户端，stdio
settings-snippet-stdio-description = 大多数客户端在其 MCP 服务器配置中接受此内容。代理会自行启动 `kubach mcp`；不需要令牌。
settings-snippet-claude-http = 通过 HTTP 使用 Claude Code
settings-snippet-claude-http-description = 代理用 use_context 选择 context，Kubach 会询问你。令牌保留在其文件中：把 export 行放进 shell 配置文件，让 Claude Code 获得它。切勿使用 --scope project 添加：.mcp.json 会随仓库提交。
settings-client-not-installed = 未安装
settings-client-not-connected = 未连接
settings-client-connected = 已连接
settings-client-stale = 已连接到另一个 kubach：{ $command }
settings-client-broken = 配置无法读取：{ $error }
settings-client-connect = 连接
settings-client-reconnect = 重新连接
settings-client-disconnect = 断开
settings-client-connect-title = 连接 { $client }？
settings-client-disconnect-title = 断开 { $client }？
settings-client-change-description = { $summary }。该文件属于 { $client }：其他服务器和设置保持不变，之前的版本保存在旁边，名为 { $suffix }。
settings-client-apply = 应用
settings-client-connected-done = 已连接。{ $hint }
settings-client-disconnected-done = 已断开。{ $hint }
settings-plan-create = 创建 { $path } 并加入服务器 `{ $server }`
settings-plan-add = 将服务器 `{ $server }` 添加到 { $path }
settings-plan-update = 将 { $path } 中的服务器 `{ $server }` 指向这里（之前运行 { $command }）
settings-plan-remove = 从 { $path } 中移除服务器 `{ $server }`
settings-plan-nothing = { $path } 无需更改
settings-restart-claude-code = 启动新的 Claude Code 会话；/mcp 会显示该服务器。
settings-restart-codex = 启动新的 Codex 会话；`codex mcp list` 会显示该服务器。
settings-restart-opencode = 重启 OpenCode。
settings-restart-cursor = Cursor 会在 Settings → MCP 中加载它；如果没有，请重新加载窗口。
settings-restart-claude-desktop = 完全退出 Claude Desktop 后重新启动。

## 活动会话

settings-group-sessions = 活动会话
settings-sessions-description = HTTP 和 `kubach mcp` 会话及你授予它们的权限。撤销会话或权限会拒绝之后的调用；`kubach mcp` 进程会在几秒内得知。已撤销的权限在该会话中不会再次询问。
settings-sessions-http-waiting = HTTP 代理：尚未选择 context。代理会用 use_context 选择，Kubach 会在开始前询问你。
settings-sessions-none = 没有活动会话。
# $profile: settings-minimum-* 之一。
settings-session-title = { $context } · { $profile }
settings-session-no-calls = 尚无调用
settings-session-pid = kubach mcp，pid { $pid }
settings-session-vault = 保险库，锁定时结束
settings-session-namespace-not-read = { $namespace }：尚未读取
settings-session-details = { $origin } · { $clients } · { $calls } 次调用 · 自 { $since } 起 · { $left } 后结束
settings-session-no-namespace = 还没有 namespace：代理第一次使用某个 namespace 时设置。
# $raised: 超出最低权限而允许的内容，例如“更改”。
settings-session-raised = 此会话已允许：{ $raised }
settings-session-no-grants = 尚未授予权限。
settings-boundary-client-only = 仅在 Kubach 中限制
settings-boundary-impersonation = 在 Kubach 和 API 服务器中限制（impersonation）
settings-boundary-temporary-sa = 在 Kubach 和 API 服务器中限制（临时 ServiceAccount）
settings-boundary = { $namespace }：{ $who }
settings-boundary-warning = { $namespace }：{ $who }。{ $warning }
settings-grant-read = 读取 { $namespace }
settings-grant-secret = Secret { $namespace }/{ $name }
settings-grant-modify = 更改 { $namespace }
settings-grant-allowed-at-launch = 由 --grant-read 允许
settings-grant-allowed = 已允许
settings-grant-denied = 已拒绝
settings-revoke = 撤销
settings-revoking = 正在撤销…

## 审计日志

settings-group-audit = 审计日志
settings-audit-description = 每个会话的每次调用：工具、隐藏机密的参数、决定和结果。当边界仅在 Kubach 中时，这是代理调用的唯一记录。
settings-audit-recent = 最近的调用
settings-audit-file = 最新的在前。文件：{ $path }
settings-audit-no-folder = 操作系统没有设置文件夹：不会记录任何内容。
settings-audit-show-file = 显示文件
