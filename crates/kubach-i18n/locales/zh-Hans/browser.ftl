# 资源浏览器：某个 kind 的列表及筛选栏、详情面板、Pods 与 Connections 区块、
# namespace 选择器。复数形式的 kind（$title：“Deployments”“Pods”）、名称、
# namespace 和集群返回的错误均为数据，保持原样。Kubernetes 术语（Pod、Node、
# namespace、kind、HPA、PodDisruptionBudget、Eviction API、emptyDir）不翻译。
# 机器翻译草稿，需要母语者校对。

## kind 列表

browser-resources = 资源
browser-place-object = { $kind } { $name }
browser-search-placeholder = 按名称搜索…
browser-list-count =
    { $scope ->
        [any] { $title }：{ $count }（所有 namespace）
        [one] { $title }：{ $count }（namespace { $namespace }）
        [many] { $title }：{ $count }（{ $namespaces } 个 namespace）
       *[cluster] { $title }：{ $count }
    }
browser-shown-of = { $shown } / { $total }
browser-cached-ago = { $age }前的缓存

## 无内容可显示

browser-no-cluster = 未连接集群
browser-connecting = 正在连接…
browser-not-connected = 未连接
browser-discovering = 正在发现 API 资源…
browser-discovery-failed = API 发现失败
browser-not-served = 不受支持
browser-not-served-description = 此集群不提供该 kind。请在侧边栏中选择其他 kind。
browser-loading = 正在加载 { $title }…
browser-no-access = 无权访问 { $title }
browser-no-access-description =
    { $scope ->
        [any] 你的凭据不允许在所有 namespace 中列出 { $title }。{ $error }
        [one] 你的凭据不允许在 namespace { $namespace } 中列出 { $title }。{ $error }
        [many] 你的凭据不允许在 { $namespaces } 个 namespace 中列出 { $title }。{ $error }
       *[cluster] 你的凭据不允许列出 { $title }。{ $error }
    }
browser-cant-load = 无法加载 { $title }
browser-lost-connection-retrying = 与集群的连接已断开。正在重试…（{ $error }）
browser-error-retrying = { $error }。正在重试…
browser-empty = 没有 { $title }
browser-nothing-found =
    { $scope ->
        [any] 在所有 namespace 中均未找到。
        [one] 在 namespace { $namespace } 中未找到。
        [many] 在 { $namespaces } 个 namespace 中未找到。
       *[cluster] 未找到任何内容。
    }

## 表格上方的横幅

browser-banner-connection-lost = 与集群的连接已断开，正在显示缓存数据。正在重试…
browser-banner-cant-refresh = 无法刷新 { $title }，正在显示缓存数据。正在重试…
browser-banner-no-access = 无权访问 { $namespaces } 中的 { $title }；正在显示其他 namespace。
browser-banner-cant-load = 无法加载 { $namespaces } 中的 { $title }。正在重试…
browser-all-namespaces = 所有 namespace
browser-offline-error = 未连接，正在显示缓存中的最近列表：{ $error }
browser-offline-disconnected = 已断开连接，正在显示缓存中的最近列表。
browser-offline-connecting = 连接期间正在显示缓存中的最近列表…
browser-pruned =
    { $count ->
       *[other] Namespace { $names } 已不存在，已从选择中移除。
    }

## 批量操作

browser-selected = 已选择：{ $count }
browser-no-common-action = 没有适用于所有所选对象的操作
browser-clear-selection = 清除选择

## namespace 选择器

picker-all-namespaces = 所有 namespace
picker-cluster-scoped = 集群级
picker-no-permission = 无权列出 namespace；请输入名称。
picker-unavailable = namespace 不可用；请输入名称。

## 详情面板

details-ago = { $age }前
details-back-tooltip = 返回 { $place }（Esc）
details-evict-tooltip = 驱逐：通过 Eviction API，遵守 PodDisruptionBudget
details-drain-tooltip = Drain…：封锁该 Node 并驱逐其中的 Pod
details-node-shell-tooltip = Node shell：在主机上运行带 shell 的特权 Pod
details-scale-down = 减少一个副本
details-scale-up = 增加一个副本
details-replicas = { $count } 个副本
details-scale-hpa = 由 HPA { $name } 管理：副本数将被覆盖
details-scale-offline = 未连接：显示的副本数来自缓存
details-scale-loading = 正在加载实时副本数…
details-scale-reconnecting = 正在重新连接：显示的副本数可能已过时
details-show-all = 显示所有 { $kind } 对象
details-open = 打开 { $kind } { $name }
details-name = 名称
details-namespace = Namespace
details-created = 创建时间
details-uid = UID
details-copy-name = 复制名称
details-copy-uid = 复制 UID
details-reason = 原因
details-security = 安全
details-owner = 所有者
details-managed-by = 管理者
details-labels = 标签
details-annotations = 注解
details-loading = 正在加载…
details-unavailable = 不可用：{ $error }
details-none = 无
details-not-created-yet = 尚未创建
details-not-served = 集群不提供 { $kind }
details-not-found = 未找到：该对象已不存在。
details-no-access = 无权访问此处该 kind 的对象。
details-cant-load = 无法加载：{ $error }

## 详情中的容器

details-container-ready = 就绪
details-container-not-ready = 未就绪
details-container-restarts = 重启 { $count } 次
details-image = 镜像
details-state = 状态
details-last-state = 上次状态
details-ports = 端口
details-requests = 请求
details-limits = 限制
details-environment = 环境变量（{ $count }）
details-mounts = 挂载（{ $count }）

## 容器端口

details-port-tcp-only = { $protocol }：端口转发仅支持 TCP
details-port-forwarded-open = 已转发到 { $address }。点击打开 { $url }
details-port-forwarded-copy = 已转发到 { $address }。点击复制地址
details-port-forward = 端口转发 { $port }：选择本地端口
details-port-open = 打开 { $url }
details-copy-address = 复制地址
details-stop-forward = 停止端口转发

## 从详情启动的操作

details-run-scale-down = 缩容 { $name }
details-run-scale-up = 扩容 { $name }
details-action-interrupted = 操作被中断
node-drain-run = Drain { $node }
node-drain-cordoning = 正在封锁并列出 Pod…
node-drain-no-pods = 没有需要驱逐的 Pod
node-drain-finishing = 正在完成…
node-drain-evicting = 正在驱逐：剩余 { $count } 个 Pod
node-drain-aborted = Drain 已中止：{ $error }

## 工作负载的 Pod

pods-none = 没有 Pod
pods-not-scheduled = 未调度

## namespace 内容

namespace-contents = 内容
namespace-counting = 正在统计…
namespace-no-permission = 无权限
namespace-not-counted = 未统计：{ $error }
namespace-contents-empty = 没有 Pod、工作负载、服务、配置或存储申领
namespace-count-again = 重新统计

## “管理者”区块

managed-by-missing = 在此集群中未找到
managed-by-not-served = 此集群未安装该 kind
managed-by-failed = 无法检查：{ $error }
managed-by-open = 打开 · 来自 { $via }
managed-by-note = { $note } · 来自 { $via }

## Connections 区块

connections-title = 关联
connections-none = 未与其他对象关联
