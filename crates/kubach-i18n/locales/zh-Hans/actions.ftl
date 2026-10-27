# 对象操作：确认对话框、运行提示、缩容到 0、Node 的 Drain 与 Shell。
# 名称、kind、参数和 kubectl 命令为数据，保持原样。草稿，需母语者校对。

## 操作确认

action-confirm-title-one = { $action } { $kind } { $name }？
action-confirm-title-many = { $action } { $count } 个 { $title }？
action-confirm-label-many = { $action } { $count } 个
action-confirm-one = { $action } { $object }？
action-confirm-many =
    { $scope ->
        [any] { $action }所有 namespace 中的 { $count } 个 { $title }？
        [one] { $action } namespace { $namespace } 中的 { $count } 个 { $title }？
        [many] { $action } { $namespaces } 个 namespace 中的 { $count } 个 { $title }？
       *[cluster] { $action } { $count } 个 { $title }？
    }
action-delete-one = 这将永久删除 { $object }。
action-delete-many =
    { $scope ->
        [any] 这将永久删除所有 namespace 中的 { $count } 个 { $title }。
        [one] 这将永久删除 namespace { $namespace } 中的 { $count } 个 { $title }。
        [many] 这将永久删除 { $namespaces } 个 namespace 中的 { $count } 个 { $title }。
       *[cluster] 这将永久删除 { $count } 个 { $title }。
    }
action-evict-one = 通过 Eviction API 驱逐 Pod { $object }（遵守 PodDisruptionBudget）。
action-evict-many = 通过 Eviction API 驱逐 { $count } 个 Pod（遵守 PodDisruptionBudget）。
action-trigger-one = 立即从 CronJob { $object } 创建一个 Job，不按计划时间。
action-trigger-many = 立即从 { $count } 个 CronJob 各创建一个 Job，不按计划时间。
action-cordon-many = 在解除封锁之前，不会再向这 { $count } 个节点调度新的 Pod。
action-claim-used-by = 被 Pod { $pods } 使用：在它们被删除之前，该 claim 会保持 Terminating（{ $finalizer }）。
action-claim-of = { $object }：{ $note }
action-claim-maybe-used = 仍被 Pod 挂载的 claim 会保持 Terminating（{ $finalizer }），直到该 Pod 被删除。
action-namespace-holds = { $namespace } 包含 { $contents }。删除 namespace 会删除其中的所有对象，且无法撤销。
action-namespace-empty = { $namespace } 中没有主要 kind 的对象（Pod、工作负载、服务、配置、claim）。删除 namespace 会删除其中的所有对象，且无法撤销。
action-namespace-unknown = { $namespace } 中的所有内容都会随之删除。删除 namespace 会删除其中的所有对象，且无法撤销。
action-namespace-count =
    { $count ->
        [1] 1 个 { $kind }
       *[other] { $count } 个 { $title }
    }

## 操作运行（提示）

action-run-title =
    { $count ->
        [1] { $action } 1 个 { $kind }
       *[other] { $action } { $count } 个 { $title }
    }
action-run-aborted = 批量操作已中止：{ $error }
action-rbac-hint = Kubernetes RBAC 拒绝了部分对象（HTTP 403）：你的凭据在那里缺少此权限。请用 kubectl auth can-i 检查，或联系集群管理员。
action-error-forbidden = 无权限（RBAC）：{ $error }
action-error-not-found = 未找到：{ $error }
action-error-text = { $error }

## 缩容到 0

action-scale-zero-title = 将 { $kind } { $name } 缩容到 0？
action-scale-zero-message = 这会停止 { $object } 的所有 Pod；在再次扩容之前它不再提供任何服务。
action-scale-zero-confirm = 缩容到 0

## Node 的 Drain

drain-confirm-title = Drain Node { $node }？
drain-confirm-message = 先封锁 { $node }，再通过 Eviction API 驱逐其 Pod：遵守 PodDisruptionBudget，被拒绝时最多重试 { $minutes } 分钟。DaemonSet Pod 和静态 Pod 会保留。
drain-list-failed = 无法列出该节点的 Pod（{ $error }）；drain 运行时会重新列出。
drain-confirm = Drain
drain-option-ignore-daemonsets = 忽略 DaemonSet（--ignore-daemonsets）
drain-option-delete-emptydir = 删除 emptyDir 数据（--delete-emptydir-data）
drain-option-force = 强制：没有控制器的 Pod（--force）
drain-blocked-notice = 这些 Pod 会阻止 drain；请勾选它们需要的选项：
drain-emptydir-notice = 被驱逐 Pod 的 emptyDir 卷中的数据将丢失。
drain-plan-line = { $pod } — { $what }
drain-verdict-evict = 驱逐
drain-verdict-delete = 删除（已结束）
drain-verdict-wait = 正在终止
drain-verdict-skip = 跳过：{ $reason }
drain-verdict-blocked = 阻止：{ $reason }
drain-skip-mirror = 静态（mirror）Pod
drain-skip-daemonset = DaemonSet Pod
drain-block-daemonset = DaemonSet Pod：请勾选“忽略 DaemonSet”
drain-block-no-controller = 不受控制器管理：请勾选“强制”
drain-block-emptydir = 使用 emptyDir（数据将丢失）：请勾选“删除 emptyDir 数据”
drain-left-on-node = 留在节点上：{ $pods }。
drain-skipped-mirror = { $count } 个静态（mirror）Pod
drain-skipped-daemonset = { $count } 个 DaemonSet Pod
drain-cordon-failed = 封锁失败：{ $error }
drain-list-pods-failed = 列出 Pod 失败：{ $error }
drain-cancelled-evicting = 驱逐时已取消
drain-eviction-timeout = drain 超时时驱逐请求仍未得到响应
drain-gave-up = { $error }；drain 超时，已放弃
drain-cancelled = 已取消：{ $error }
drain-terminating-timeout = 已驱逐，但 drain 超时时仍在终止
drain-cancelled-waiting = 等待 Pod 终止时已取消

## Node 上的 Shell

node-shell-confirm-title = 在 Node { $node } 上打开 Shell？
node-shell-confirm-message = Kubach 会在 { $node } 上的 namespace { $namespace } 中创建一个特权 Pod（使用主机 PID、网络和 IPC，容忍所有 taint），并通过 nsenter 在主机上打开 root shell。关闭标签页时会删除该 Pod。Namespace 和镜像可在 设置 → 常规 → 终端 中修改。
node-shell-image = 镜像：{ $image }
node-shell-root-notice = 该 shell 在节点上拥有 root 权限：它可以读取任何 Pod 的数据并修改主机。
node-shell-confirm = 打开 Shell
