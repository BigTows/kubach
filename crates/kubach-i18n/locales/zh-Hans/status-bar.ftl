# 主窗口底部的状态栏。Kubernetes 术语（Pod、Namespace、context、watch）保留英文。
# 机器翻译草稿，需要母语者审校。

status-loading-kubeconfig = 正在加载 kubeconfig…
status-switch-context = 切换 context
status-error = 错误：{ $error }

status-no-cluster = 无集群
status-cluster = { $context }
status-cluster-version = { $context } · { $version }
status-cluster-connecting = { $context } · 正在连接…
status-cluster-error = { $context } · 错误

status-all-namespaces = 全部 Namespace
status-no-namespace = 无 Namespace
status-namespace = { $namespace }
status-namespaces = { $count } 个 Namespace
status-namespaces-tooltip = 显示的 Namespace
status-namespaces-tooltip-change = 显示的 Namespace · 更改（⌘⇧N / Ctrl+Shift+N）

status-syncing = 正在同步…
status-reconnecting = 正在重新连接…
status-sync-loading = { $count } 个 watch 正在加载
status-sync-reconnecting = { $count } 个 watch 正在重新连接
status-sync-both = { $loading } 个 watch 正在加载，{ $reconnecting } 个正在重新连接

status-operations = { $count } 个操作正在运行
status-operations-tooltip = 显示操作：时间、阶段和输出

status-forwards = { $count } 个端口转发
# 导航栏尚未翻译，因此引用其英文名称。
status-forwards-tooltip = 打开「网络 → 端口转发」

status-terminal = 终端
status-terminal-tooltip = 使用此 context 新建本地终端（Ctrl+`）

status-mcp-off = MCP 已关闭
status-mcp-failed = MCP 启动失败
status-mcp-pending = MCP · { $count } 项待确认
status-mcp-no-sessions = MCP · 无会话
status-mcp-sessions = MCP · { $count } 个会话
status-mcp-tooltip = AI 代理会话

status-version-tooltip = 复制 Kubach 版本、操作系统和集群版本
status-version-copied = 版本信息已复制到剪贴板
