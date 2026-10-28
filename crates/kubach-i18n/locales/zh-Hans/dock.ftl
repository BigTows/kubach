# 底部面板：日志和终端标签页、本地终端、节点 Shell、端口转发对话框、
# “端口转发”页面以及详情面板中的按钮。Pod、容器、namespace、context、
# 节点和端口名称、选择器和镜像均为数据，保持原样。
# Kubernetes 术语（Pod、Service、init 容器、port-forward、kubectl）不翻译。
# 机器翻译草稿，需要母语者校对。

## 通用

dock-connecting = 正在连接…
dock-not-connected = 未连接到集群
dock-context-not-connected = Context { $context } 未连接；连接后即可重新连接此标签页
dock-cancel = 取消
dock-container = 容器 { $container }

## 标签栏

dock-tab-close = 关闭
dock-collapse = 收起
dock-expand = 展开
dock-logs-streaming = 正在接收 · { $lines } 行
dock-logs-ended = 已结束 · { $lines } 行

## 详情面板中的流按钮

dock-forward-title = 端口转发
dock-entry-logs-all = 日志：在底部面板中查看所有容器的日志流
dock-entry-logs = 日志：在底部面板中查看日志流
dock-entry-shell-default = Shell：在 { $container }（默认容器）中打开 Shell
dock-entry-shell = Shell：在容器中打开 Shell
dock-entry-debug-default = 调试：启动以 { $container }（默认容器）为目标的临时 busybox 容器并连接
dock-entry-debug = 调试：在应用旁启动临时 busybox 容器并连接
dock-entry-forward-selector = 端口转发：将本地端口转发到此资源的就绪 Pod
dock-entry-forward = 端口转发：将本地端口转发到此 Pod
dock-entry-forward-port = 端口转发：端口 { $port }，选择本地端口
dock-choose-container = 选择容器
dock-choose-port = 选择端口
dock-choose-local-port = 选择本地端口…

## 容器菜单

dock-all-containers = 所有容器
dock-containers = 容器
dock-init-containers = Init 容器

## 调试确认

dock-debug-title = 调试 Pod { $pod }？
dock-debug-message = 将向 Pod { $namespace }/{ $pod } 添加临时调试容器（{ $image }）。该容器无法移除：它会一直保留在 Pod 中，直到 Pod 被删除。
dock-debug-message-target = 将向 Pod { $namespace }/{ $pod } 添加临时调试容器（{ $image }），与 { $container } 共享进程。该容器无法移除：它会一直保留在 Pod 中，直到 Pod 被删除。
dock-debug-dont-ask = 不再询问
dock-debug-confirm = 添加调试容器

## 日志标签页

dock-logs-search = 搜索
dock-logs-no-containers = 没有可读取的容器
dock-logs-waiting = 正在等待日志行…
dock-logs-no-lines = 没有日志行
dock-logs-loading-earlier = 正在加载更早的日志行…
dock-logs-earlier-failed = 无法加载更早的日志行：{ $error }
dock-logs-beginning = 日志开头
dock-logs-capped = 仅显示最新的日志行（最多 { $lines } 行或 { $mib } MiB）；更早的日志请使用 kubectl logs
dock-logs-reconnecting = 正在重新连接…
dock-logs-dropped = 已丢弃 { $count } 行
dock-logs-ended-indicator = 日志流已结束
dock-logs-previous = 上一个实例
dock-logs-timestamps = 时间戳
dock-logs-wrap = 自动换行
dock-logs-prev-match = 上一个匹配（Shift+Enter）
dock-logs-next-match = 下一个匹配（Enter）
dock-logs-pause = 暂停
dock-logs-follow = 跟随
dock-logs-copy = 复制所有行
dock-logs-clear = 清除

## Shell 和调试终端

dock-terminal-running = 运行中
dock-terminal-starting-debug = 正在启动调试容器…
dock-terminal-attached = 已连接到调试容器 { $container }。如果看不到提示符，请按 Enter。
# $shells: /bin/bash, /bin/sh, /bin/ash, /busybox/sh.
dock-terminal-no-shell = 容器 { $container } 中没有 Shell：{ $shells } 均未找到（distroless 或 scratch 镜像？）。请改用“调试”连接 busybox 容器
# 列表中的第一个 Shell 不存在；$shell: /bin/sh，$missing: /bin/bash。
dock-terminal-shell-fallback = 已启动 { $shell }：镜像中没有 { $missing }
dock-terminal-debug-tooltip = 连接一个与此容器共享进程的临时 busybox 容器
dock-terminal-container-tooltip = 容器：切换后将重新连接 Shell
dock-terminal-target-tooltip = 目标容器：切换后将启动新的调试容器
dock-terminal-reconnect = 重新连接
dock-terminal-new-debug = 新建调试容器
dock-exit-success = 已退出
dock-exit-code = 已退出，代码 { $code }
dock-exit-failed = 失败
dock-exit-failed-message = 失败：{ $message }
dock-exit-disconnected = 已断开

## 本地终端

dock-local-title = 终端 — { $context }/{ $namespace }
dock-local-title-plain = 终端
dock-local-starting = 正在启动 Shell…
dock-local-context-unavailable = Context { $context } 不可用（保险库是否已锁定？）
dock-local-no-cluster = Kubach 中没有打开的集群：KUBECONFIG 保持为 Shell 中的设置。
dock-local-no-temp-dir = 没有用于临时 kubeconfig 的文件夹
dock-local-temp-write-failed = 无法写入临时 kubeconfig：{ $error }
dock-local-restart = 重新启动

## 节点 Shell

dock-node-title = 节点 Shell — { $node }
dock-node-creating = 正在创建 Pod…
dock-node-creating-in = 正在 { $namespace } 中创建 Pod…
dock-node-created = 已创建特权 Pod { $namespace }/{ $pod }；正在等待其运行…
dock-node-waiting = 正在等待 Pod…
dock-node-new-pod = 新建 Pod

## 端口转发对话框和通知

dock-forward-enter-port = 请输入端口
dock-forward-invalid-port = { $text } 不是有效端口（1–65535）
dock-forward-menu-tcp-only = { $port }（port-forward 仅支持 TCP）
dock-forward-started = 端口转发已启动
dock-forward-started-via = { $route }（Pod { $pod }:{ $port }）
dock-forward-open = 打开
dock-forward-open-in-browser = 在浏览器中打开 { $url }
dock-forward-copy = 复制
dock-forward-copy-tooltip = 复制地址
dock-forward-service-port = Service 端口
dock-forward-pod-port = Pod 端口
dock-forward-service-ports = Service 端口
dock-forward-container-ports = 容器端口
dock-forward-local-placeholder = 随机空闲端口
dock-forward-route = → Pod { $pod }:{ $port }
dock-forward-port-not-tcp = 无法转发 { $protocol } 端口：port-forward 仅支持 TCP
dock-forward-service-to-pod-port = Service 端口 → Pod 端口 { $port }
dock-forward-service-to-named-port = Service 端口 → 名为 { $name } 的容器端口
dock-forward-looking = 正在查找就绪的 Pod…
dock-forward-ready-pod = 就绪 Pod：{ $pod }
dock-forward-pod = Pod：{ $pod }
dock-forward-pods = Pod：{ $selector }
dock-forward-udp-disabled = UDP 端口不可用：port-forward 仅传输 TCP。
dock-forward-local-port = 127.0.0.1 上的本地端口
dock-forward-local-random = 留空：使用随机空闲端口。输入的端口会被记住，下次使用。
dock-forward-local-remembered = 此端口会被记住，下次转发时使用。
dock-forward-port-busy = 本机端口 { $port } 已被占用。请输入其他端口，或留空以使用随机端口。
dock-forward-port-denied = 没有管理员权限无法使用端口 { $port }。请输入大于 1024 的端口，或留空。
dock-forward-start = 启动
dock-forward-starting = 正在启动…

## “端口转发”页面

dock-forwards-ready = 运行中
dock-forwards-failing = 失败
dock-forwards-reconnecting = 正在重连
dock-forwards-ready-tooltip = 本地端口正在监听；上一个连接已到达 Pod
dock-forwards-failing-tooltip = 上一个到 Pod 的连接失败。本地端口仍在监听：下一个连接会重试
dock-forwards-reconnecting-tooltip = 失败后，新的连接正在尝试连接 Pod
dock-forwards-last-error = 最近的错误：{ $error }
dock-forwards-stop-tooltip = 停止转发并关闭其连接
dock-forwards-errors = { $count } 个错误
dock-forwards-open = 打开 { $url }
dock-forwards-copy = 复制 { $address }
dock-forwards-description = 切换上下文时转发会继续运行，直到被停止。
dock-forwards-pod = Pod
dock-forwards-context-current = 当前上下文
dock-forwards-connections = { $active } 个打开 · 共 { $total } 个
dock-forwards-agent = 代理 · { $client }
dock-forwards-agent-tooltip = 由 AI 代理 { $client } 打开，会话 { $session }。会话结束时停止；也可以现在停止。
dock-forwards-connections-tooltip = 当前打开的连接，以及转发启动以来的所有连接
dock-forwards-sort = 排序
dock-forwards-sort-port = 端口
dock-forwards-sort-status = 状态
dock-forwards-sort-target = 目标
dock-forwards-paused = 已暂停
dock-forwards-paused-tooltip = 本地端口仍被占用，但不转发任何流量：连接会被关闭
dock-forwards-pause = 暂停：保留本地端口，关闭连接
dock-forwards-resume = 在 { $address } 上继续转发
dock-forwards-reveal = 打开 { $target }
dock-forwards-empty = 没有端口转发
dock-forwards-empty-description = 选择一个 Pod、Service 或工作负载，然后点击“端口转发”。
dock-forwards-restore-waiting = 等待中
dock-forwards-restore-waiting-tooltip = Kubach 退出时正在运行：上下文连接后将在同一本地端口重新启动
dock-forwards-restore-waiting-context = 上下文 { $context } 连接后启动
dock-forwards-restore-unknown-context = 上下文 { $context } 不可用：请解锁保险库或添加其 kubeconfig
dock-forwards-restore-starting = 正在启动
dock-forwards-restore-starting-detail = 正在同一本地端口上启动…
dock-forwards-restore-failed = 未启动
dock-forwards-restore-failed-tooltip = Kubach 退出时正在运行，但未能重新启动。请重试，或将其移除以免再次恢复
dock-forwards-restore-port-busy = 本机端口 { $port } 已被占用。请释放后重试，或移除此转发并新建一个。
dock-forwards-restore-gone = 集群中已不存在 { $source }
dock-forwards-restore-cannot-connect = 无法连接到上下文 { $context }：{ $error }
dock-forwards-restore-disconnected = 转发启动前上下文 { $context } 已断开
dock-forwards-retry-tooltip = 再次尝试启动转发
dock-forwards-remove-tooltip = 移除转发：之后不再恢复
