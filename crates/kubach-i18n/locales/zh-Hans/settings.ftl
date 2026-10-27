# 设置界面：侧边栏、搜索和“常规”页面。
# 机器翻译草稿，需要母语者审校。

settings-back = 返回 Kubach
settings-back-tooltip = 关闭设置（Esc）
settings-search-placeholder = 搜索设置
settings-filter-contexts-placeholder = 筛选 context
settings-no-match = 没有与“{ $query }”匹配的设置。
settings-saved-to = 保存在 { $path }
settings-not-saved = 没有配置目录：设置不会被保存。
settings-file-problem = 设置文件

## 侧边栏中的页面

settings-nav-clusters = 集群
settings-section-general = 常规
settings-section-clusters = 通用
settings-section-cluster = 按集群
settings-section-agent = AI 代理
settings-section-tools = 外部工具
settings-section-presets = 预设
settings-title-clusters = 集群：通用
settings-title-cluster = 集群：按集群
settings-description-general = 外观、语言和存储。
settings-description-clusters = 适用于所有集群：kubeconfig 文件、导入的 kubeconfig、启动时打开的 context 以及后台连接。
settings-description-cluster = 逐个设置 context：快捷栏图块、默认 Namespace、Prometheus、Helm、安全设置，以及从 Kubach 中移除。
settings-description-agent = 通过 MCP 连接 AI 代理：HTTP 服务器、会话、审批和审计日志。
settings-description-tools = Kubach 在本机上运行的程序。
settings-description-presets = 从 YAML 编辑器保存的资源预设，可在资源页面通过“+”使用。

## “常规”页面

settings-group-appearance = 外观
settings-theme = 主题
settings-theme-dark = 深色
settings-theme-light = 浅色
settings-theme-system = 跟随系统
settings-theme-description = “跟随系统”会随操作系统的浅色或深色外观切换。

settings-group-language = 语言
settings-language = 界面语言
settings-language-description = 部分翻译：目前已翻译状态栏、菜单和本页，其余内容仍为英文。Kubernetes 名称、集群数据和命令输出保持原样。

settings-group-terminal = 终端
settings-node-shell-namespace = 节点 Shell 的 Namespace
settings-node-shell-namespace-description = 节点 Shell 的特权 Pod 所在的 Namespace。该处的 Pod Security 必须允许特权 Pod。
settings-node-shell-image = 节点 Shell 的镜像
settings-node-shell-image-description = 节点 Shell Pod 使用的镜像；需要包含 nsenter（busybox 和 alpine 均已自带）。
settings-terminal-clipboard = 允许程序写入剪贴板
settings-terminal-clipboard-description = Shell 和 Debug 标签页中的程序可以替换剪贴板内容（OSC 52）。默认关闭：Pod 可能借此放入一条命令，等你粘贴执行。
settings-confirm-debug = 确认 Debug 容器
settings-confirm-debug-description = 在 Debug 向 Pod 添加临时容器之前询问：该容器在 Pod 被删除之前无法移除。

settings-group-storage = 存储
settings-cache = 缓存
settings-cache-counting = 正在计算…
settings-cache-description = { $size }：Chart 图标和 README 图片。这些内容会在需要时重新下载；设置和已保存的状态会保留。
settings-cache-description-in = { $size }，位于 { $dir }：Chart 图标和 README 图片。这些内容会在需要时重新下载；设置和已保存的状态会保留。
settings-cache-clear-failed = 未能全部清除：{ $error }
settings-clear-cache = 清除缓存

## 时长：`30 分钟`、`1 小时`、`1 小时 30 分钟`、`40 秒`

settings-duration-minutes = { $minutes } 分钟
settings-duration-hours = { $hours } 小时
settings-duration-hours-minutes = { $hours } 小时 { $minutes } 分钟
settings-duration-seconds = { $seconds } 秒
settings-unit-minutes = 分钟
