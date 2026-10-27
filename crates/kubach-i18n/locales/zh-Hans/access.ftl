# 访问控制界面：权限（我能做什么、谁可以）和 pod 安全检查。动词（get、
# list）、资源（pods、deployments.apps）、RBAC kind（Role、
# ClusterRoleBinding、ServiceAccount、User、Group）、API 组、字段名
# （hostNetwork、runAsUser）以及集群返回的错误均为数据，保持原样。
# 机器翻译草稿，需要母语者校对。

## 权限

access-tab-mine = 我能做什么
access-tab-who-can = 谁可以
access-check-again = 重新检查
access-pick-namespace = 请选择 namespace
access-pick-namespace-description = 每次只检查一个 namespace 中的权限。
access-reviewing = 正在检查你的权限…
access-cant-review = 无法检查你的权限
access-noun-rules = 规则
access-noun-subjects = 主体
access-noun-findings = 发现项
access-incomplete = 列表可能不完整。缺少某条规则并不代表访问被拒绝。
access-incomplete-reason = 列表可能不完整：{ $error }。缺少某条规则并不代表访问被拒绝。
access-non-resource = 非资源 URL：{ $urls }
access-rules-note =
    { $selected ->
        [0] 你在 namespace { $namespace } 中有 { $count } 条规则，来自 SelfSubjectRulesReview
        [1] 你在 namespace { $namespace } 中有 { $count } 条规则，来自 SelfSubjectRulesReview
       *[other] 你在 namespace { $namespace }（所选 { $selected } 个中的第一个）中有 { $count } 条规则，来自 SelfSubjectRulesReview
    }
access-column-verbs = 动词
access-column-groups = API 组
access-column-names = 名称
access-any-name = 任意

## 谁可以

access-verb-placeholder = 动词
access-resource-placeholder = 资源，例如 deployments.apps
access-name-placeholder = 任意名称
access-check = 检查
access-who-can-invalid = 请输入动词和资源，例如 get pods。
access-checking = 正在检查…
access-checking-description = 正在列出集群的角色和绑定。
access-cant-check = 无法检查访问权限
access-cant-check-description = { $error }。列出角色和绑定需要在整个集群范围内对 rbac.authorization.k8s.io 资源具有 list 权限。
access-question-named = { $verb } { $resource }（名称 { $name }）
access-grants-note =
    { $selected ->
        [0] 在 namespace { $namespace } 中有 { $count } 个授权可 { $question }。仅 RBAC：不显示其他授权器和 system:masters。
        [1] 在 namespace { $namespace } 中有 { $count } 个授权可 { $question }。仅 RBAC：不显示其他授权器和 system:masters。
       *[other] 在 namespace { $namespace }（所选 { $selected } 个中的第一个）中有 { $count } 个授权可 { $question }。仅 RBAC：不显示其他授权器和 system:masters。
    }
access-grants-note-cluster = 在整个集群范围内有 { $count } 个授权可 { $question }（仅计算 ClusterRoleBinding）。仅 RBAC：不显示其他授权器和 system:masters。
access-nobody = RBAC 未授予任何人此权限
access-view-table = 表格
access-view-graph = 图
access-graph-subjects = 主体
access-graph-bindings = 绑定
access-graph-roles = 角色
access-graph-rules = 规则
access-subject = 主体
access-binding = 绑定
access-role = 角色
access-rule = 规则
access-rule-label = 对 { $resources } 执行 { $verbs }
access-rule-label-named = 对 { $resources }（名称 { $names }）执行 { $verbs }
access-rule-groups = API 组：{ $groups }
access-open-binding = 打开绑定
access-open-role = 打开角色
access-open-service-account = 打开 service account

## 安全检查

access-pods-not-served = 不支持 Pods
access-loading-pods = 正在加载 pod…
access-no-access-pods = 无权访问 pod
access-no-access-pods-description =
    { $scope ->
        [one] 你的凭据不允许列出 namespace { $namespace } 中的 pod。
        [many] 你的凭据不允许列出 { $namespaces } 个 namespace 中的 pod。
       *[any] 你的凭据不允许列出所有 namespace 中的 pod。
    }
access-cant-load-pods = 无法加载 pod
access-no-findings = 没有发现项
access-no-findings-description =
    { $scope ->
        [one] namespace { $namespace } 中没有 pod 违反此级别的检查。
        [many] { $namespaces } 个 namespace 中没有 pod 违反此级别的检查。
       *[any] 所有 namespace 中都没有 pod 违反此级别的检查。
    }
access-security-summary =
    { $scope ->
        [one] { $summary }（namespace { $namespace }）：特权容器、hostPath、缺少限制、:latest 镜像等
        [many] { $summary }（{ $namespaces } 个 namespace）：特权容器、hostPath、缺少限制、:latest 镜像等
       *[any] { $summary }（所有 namespace）：特权容器、hostPath、缺少限制、:latest 镜像等
    }
access-show-low = 显示低严重性
security-severity-high = 高
security-severity-medium = 中
security-severity-low = 低
security-count-high = { $count } 个高
security-count-medium = { $count } 个中
security-count-low = { $count } 个低
security-no-issues = 没有问题
security-column-severity = 严重性
security-column-pod = Pod
security-column-container = 容器
security-column-finding = 发现项
security-pod-level = pod
security-fix = 修复方法
security-open-pod = 打开 pod

## 发现项（kubach-core）

security-host-namespace = 已启用 { $field }
security-host-network-hint = Pod 可以看到主机的所有网络接口。除非绝对必要，请移除 hostNetwork。
security-host-pid-hint = Pod 可以看到主机进程并向其发送信号。除非绝对必要，请移除 hostPID。
security-host-ipc-hint = Pod 与主机共享 IPC 和共享内存。除非绝对必要，请移除 hostIPC。
security-host-path = hostPath 卷“{ $volume }”
security-host-path-hint = 从节点挂载 { $path }。请改用 PVC、configMap 或 emptyDir。
security-privileged = 特权容器
security-privileged-hint = 拥有对主机的完全访问权限。请设置 privileged: false。
security-privilege-escalation = 允许特权提升
security-privilege-escalation-hint = 请设置 allowPrivilegeEscalation: false。
security-runs-as-root = 以 root 运行
security-runs-as-root-hint = runAsUser 为 0。请使用非零 UID。
security-may-run-as-root = 可能以 root 运行
security-may-run-as-root-hint = 请设置 runAsNonRoot: true 或非零的 runAsUser。
security-capability = 添加了 capability { $capability }
security-capability-hint = 除非绝对必要，请移除；建议使用 drop: [ALL]。
security-no-limits = 没有资源限制
security-no-limits-hint = 请设置内存和 CPU 限制，以免单个 pod 耗尽节点资源。
security-no-memory-limit = 没有内存限制
security-no-memory-limit-hint = 请设置内存限制，以免单个 pod 耗尽节点资源。
security-no-requests = 没有资源请求
security-no-requests-hint = 请设置 CPU 和内存请求，以便调度可预测。
security-untagged-image = 镜像没有标签
security-latest-image = 镜像使用 :latest
security-mutable-tag-hint = { $image } 可能在你不知情时变化。请固定版本标签或 digest。
security-writable-root = 根文件系统可写
security-writable-root-hint = 请设置 readOnlyRootFilesystem: true，并显式挂载可写路径。
