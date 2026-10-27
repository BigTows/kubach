# 主窗口框架：侧边栏、集群快捷栏、“返回”、空状态、通知和调试浮层。
# 上下文、集群、用户、namespace 和文件名是数据，保持原样。
# Kubernetes 与 Helm 术语（namespace、kubeconfig、Gateway API、Helm、
# Release、Chart、exec、Secret）不翻译。机器草稿，需要母语者审校。

## 侧边栏分区

nav-overview = 概览
nav-events = 事件
nav-topology = 拓扑
nav-cluster = 集群
nav-workloads = 工作负载
nav-network = 网络
nav-gateway-api = Gateway API
nav-storage = 存储
nav-config = 配置
nav-access-control = 访问控制
nav-custom-resources = 自定义资源
nav-helm = Helm
nav-kustomize = Kustomize
nav-gitops = GitOps
nav-port-forwards = 端口转发
nav-permissions = 权限
nav-security-checks = 安全检查
nav-helm-releases = Release
nav-helm-charts = Chart
nav-back-helm-releases = Helm Release
nav-back-helm-charts = Helm Chart

## 标题栏中的“← 返回”

workspace-back-tooltip = 返回 { $place }（{ $shortcut }）

## 通知和横幅

workspace-namespace-added = 已将 namespace { $namespace } 添加到显示的 namespace
workspace-undo = 撤销
workspace-shell-env-banner = 未加载 shell 环境：{ $error }。缺少 shell 配置文件中的 PATH 和 KUBECONFIG，可能找不到 exec 认证插件。请检查 shell 的 rc 文件，或从终端启动 Kubach。

## 主区域的空状态

workspace-loading-kubeconfig = 正在加载 kubeconfig…
workspace-kubeconfig-not-found = 未找到 kubeconfig
workspace-kubeconfig-unavailable = kubeconfig 文件不可用
workspace-kubeconfig-unavailable-description = 无法读取已链接的 kubeconfig 文件。请在设置中重新查找。
workspace-open-settings = 打开设置
workspace-no-contexts = kubeconfig 中没有上下文
workspace-no-contexts-description = 链接 kubeconfig 文件或将其导入加密保险库，也可以将文件拖放到此处。
workspace-add-cluster = 添加集群…
workspace-no-cluster = 未连接集群
workspace-no-cluster-description = 从 kubeconfig 中选择一个上下文以浏览资源。
workspace-disconnected = 已断开与 { $context } 的连接
workspace-connect = 连接
workspace-connecting = 正在连接 { $context }…
workspace-connect-failed = 无法连接到 { $context }
workspace-retry = 重试

## 快捷栏磁贴提示

hotbar-row-cluster = 集群
hotbar-row-server = 服务器
hotbar-row-user = 用户
hotbar-row-namespace = Namespace
hotbar-row-status = 状态
hotbar-row-shortcut = 快捷键
hotbar-row-source = 来源
hotbar-row-security = 安全
hotbar-row-vault = 保险库
hotbar-row-problem = 问题
hotbar-row-file = 文件
hotbar-row-fix = 解决方法
hotbar-fix-linked = 右键 →“重新查找…”或“移除文件”
hotbar-vault-locked = 已锁定。点击解锁。
hotbar-state-disconnected = 未连接
hotbar-state-connecting = 正在连接…
hotbar-state-connected = 已连接，{ $version }
hotbar-state-error = 错误：{ $error }
hotbar-security-looser = 比严格设置宽松：{ $list }
hotbar-loosened-mcp = AI 代理访问
hotbar-loosened-profile = 配置档
hotbar-loosened-temporary-objects = 集群中的独立身份
hotbar-loosened-deploy = 应用更改
hotbar-loosened-exec = exec
hotbar-loosened-session-ttl = 会话时长
hotbar-loosened-namespaces = 多个 namespace
hotbar-loosened-secret-values = 显示 Secret 值
hotbar-loosened-agent-secret-values = 应请求向代理显示 Secret 值
hotbar-loosened-agent-other-kinds = 代理可用自定义资源

## 快捷栏菜单

hotbar-menu-connect = 连接
hotbar-menu-disconnect = 断开连接
hotbar-menu-rename = 重命名…
hotbar-menu-set-color = 设置颜色…
hotbar-menu-hide = 隐藏
hotbar-menu-security = 安全设置…
hotbar-menu-lock-imported = 锁定导入的集群
hotbar-menu-remove = 从 Kubach 中移除…
hotbar-menu-unlock = 解锁…
hotbar-menu-kubeconfig-settings = kubeconfig 设置…
hotbar-menu-find-again = 重新查找…
hotbar-menu-remove-file = 移除文件
hotbar-hidden = 已隐藏

## 快捷栏磁贴重命名对话框

hotbar-rename-title = 重命名 { $context }
hotbar-rename-description = 快捷栏磁贴上的短标签，最多 { $max } 个字符。留空则显示首字母。

## 调试浮层（仅调试构建）

debug-menu-overlay = 调试浮层
debug-section-frames = 帧
debug-section-process = 进程
debug-fps = FPS
debug-idle = 空闲
debug-frame = 帧时间
debug-avg-max = 平均 / 最大 5 秒
debug-cpu = CPU
debug-memory = 内存
debug-threads = 线程
debug-clusters = 集群
debug-watches = Watch
debug-cached-objects = 缓存对象
debug-port-forwards = 端口转发
debug-runtime-tasks = 运行时任务
debug-ms = { $value } 毫秒
debug-hide = 按 { $shortcut } 隐藏
