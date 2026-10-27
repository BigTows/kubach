# kubach-core 内置插件在详情面板中的文本：健康原因与备注、容器与副本状态、字段标签。


## 健康原因

state-active = 活跃
state-available = 可用
state-ready = 就绪
state-scaled-to-zero = 已缩容至零
state-rollout-paused = 发布已暂停
state-waiting-rollout = 等待控制器观察到发布
state-waiting-controller = 等待控制器
state-waiting-status = 等待状态
state-replicas-updated = 已更新 { $updated }/{ $desired } 个副本
state-old-replicas = { $count } 个旧副本等待终止
state-updated-available = 已更新副本中 { $available }/{ $updated } 个可用
state-replicas-ready = { $ready }/{ $desired } 个副本就绪
state-pods-updated = 已更新 { $updated }/{ $desired } 个 Pod
state-pods-available = { $available }/{ $desired } 个 Pod 可用
state-pods-misscheduled = { $count } 个 Pod 调度错误
state-hpa-disabled = 扩缩容已停用：目标已缩容至零
state-hpa-waiting-metrics = 等待首批指标
state-hpa-limited = 受限：{ $reason }
state-hpa-scaling = 正在扩缩容
state-crd-established = 已建立
state-crd-not-established = 尚未建立
state-gateway-accepted = 已接受
state-gateway-programmed = 已编程
state-gateway-listener = 监听器 { $name }：{ $reason }
state-route-not-accepted = 尚未被任何 Gateway 接受
state-route-waiting-gateways = 等待 Gateway
state-deleting = 正在删除
state-suspended = 已暂停
state-job-failing = 正在失败
state-cronjob-active = { $count } 个活跃 Job
state-cronjob-last-failed = 上一个调度的 Job 未成功
state-cronjob-never-succeeded = 尚无调度的 Job 成功
state-cronjob-not-scheduled = 尚未调度
state-cronjob-scheduled = 已调度
state-service-waiting-lb = 等待负载均衡器
state-ingress-waiting-address = 等待地址
state-node-scheduling-disabled = 就绪，已禁止调度
state-pod-containers-ready = { $ready }/{ $total } 个容器就绪
state-quota-not-computed = 尚未计算
state-quota-within = 在配额内
state-quota-at-limit = 已达上限：{ $resources }
state-pdb-not-observed = 尚未观察
state-pdb-no-pods = 未选中任何 Pod
state-pdb-unhealthy = 所需 Pod 中 { $current }/{ $desired } 个健康
state-pdb-no-disruptions = 不允许中断：驱逐需等待
state-pdb-disruptions = 允许 { $count } 次中断
state-claim-protected = 正在终止：有 Pod 使用该 Claim 时 { $finalizer } 会保留它
state-volume-protected = 正在终止：有 Claim 绑定时 { $finalizer } 会保留该卷
state-claim-used-by = 正在终止：被 Pod { $pods } 使用
state-vpa-provided = 已提供建议
state-vpa-provided-low = 已提供建议（置信度低）
state-vpa-no-recommendation = 尚无建议
state-container-running = 运行中
state-container-not-ready = 未就绪
state-container-unknown = 未知


## 关联对象备注

brief-pod =
    { $ready }/{ $total } 就绪{ $restarts ->
        [0] {""}
       *[other] {" · "}重启 { $restarts } 次
    }
brief-pod-on-node =
    { $ready }/{ $total } 就绪{ $restarts ->
        [0] {""}
       *[other] {" · "}重启 { $restarts } 次
    } · { $node }
brief-revision-ready = 修订版本 { $revision } · { $ready }/{ $desired } 就绪
brief-ready = { $ready }/{ $desired } 就绪
brief-keys = { $count } 个键


## 应用自行描述的状态

container-state-running = 运行中
container-state-waiting = 等待中：{ $reason }
container-state-waiting-message = 等待中：{ $reason }（{ $message }）
container-state-terminated = 已终止：{ $reason }（退出码 { $code }）
container-state-unknown = 未知
container-state-not-ready = { $state }，未就绪
replicas-desired = 期望 { $count }
replicas-current = 当前 { $count }
replicas-ready = 就绪 { $count }
replicas-updated = 已更新 { $count }
replicas-available = 可用 { $count }


## 带编号的标签

field-rule-n = 规则 { $n }
field-revision-of = 修订版本 { $revision }

## Labels of detail fields, key/value rows and table columns (by their English text)

field-access-modes = 访问模式
field-address = 地址
field-addresses = 地址
field-affinity = 亲和性
field-aggregates = 聚合
field-allowed-disruptions = 允许中断数
field-api-discovery-failed = API 发现失败
field-api-group-version-unreadable = 无法解析 API 组版本
field-api-groups = API 组
field-applications-sync-policy = 应用同步策略
field-architecture = 架构
field-artifact-revision = 制品修订版本
field-artifact-updated = 制品更新时间
field-auto-sync = 自动同步
field-backoff-limit = 重试上限
field-binding-mode = 绑定模式
field-capacity = 容量
field-categories = 类别
field-chart = Chart
field-claim = Claim
field-class = 类
field-cluster-ips = Cluster IP
field-completed = 完成时间
field-completions = 完成数
field-concurrency-policy = 并发策略
field-container = 容器
field-container-runtime = 容器运行时
field-controller = 控制器
field-conversion = 转换
field-conversion-webhook = 转换 Webhook
field-data-source = 数据源
field-default = 默认
field-default-backend = 默认后端
field-delete = 删除
field-deleting-objects-failed = 删除对象失败
field-deletion-requested = 请求删除时间
field-description = 描述
field-destination = 目标位置
field-duration = 持续时间
field-external-ip = 外部 IP
field-external-traffic-policy = 外部流量策略
field-extras = 附加
field-failure-policy = 失败策略
field-finalizers-remaining = 剩余 finalizer
field-finished = 完成时间
field-from = 来源
field-generators = 生成器
field-global-default = 全局默认
field-group = 组
field-health = 健康状况
field-health-message = 健康消息
field-healthy-pods = 健康 Pod
field-history-failed = 历史（失败）
field-history-successful = 历史（成功）
field-host-ip = 主机 IP
field-hostnames = 主机名
field-immutable = 不可变
field-initiated-by = 发起者
field-internal-traffic-policy = 内部流量策略
field-interval = 间隔
field-ip-families = IP 协议族
field-kernel = 内核
field-kind = Kind
field-kubelet = Kubelet
field-last-applied-revision = 最近应用的修订版本
field-last-attempted-revision = 最近尝试的修订版本
field-last-handled-reconcile = 最近处理的 reconcile
field-last-scaled = 上次扩缩容
field-last-schedule = 上次调度
field-last-successful = 上次成功
field-limit = 限制
field-lower = 下限
field-match-conditions = 匹配条件
field-match-policy = 匹配策略
field-max-replicas = 最大副本数
field-max-unavailable = 最大不可用
field-message = 消息
field-min-available = 最小可用
field-min-replicas = 最小副本数
field-misscheduled = 错误调度
field-mount-options = 挂载选项
field-name = 名称
field-names = 名称
field-namespace = Namespace
field-namespace-selector = Namespace 选择器
field-node = Node
field-node-selector = Node 选择器
field-object-selector = 对象选择器
field-objects = 对象
field-objects-remaining = 剩余对象
field-os-image = 操作系统镜像
field-parallelism = 并行度
field-parameters = 参数
field-parent = 父级
field-path = 路径
field-paused = 已暂停
field-phase = 阶段
field-pod-cidr = Pod CIDR
field-pod-ip = Pod IP
field-pod-ips = Pod IP 列表
field-pod-management = Pod 管理策略
field-pod-selector = Pod 选择器
field-pods = Pod
field-policy-types = 策略类型
field-preemption = 抢占
field-priority = 优先级
field-project = 项目
field-provider = 提供方
field-provider-id = 提供方 ID
field-provisioner = 制备器
field-prune = 清理
field-qos-class = QoS 类
field-ready = 就绪
field-reason = 原因
field-reclaim-policy = 回收策略
field-recommenders = 推荐器
field-reconciled-at = 协调时间
field-ref = Ref
field-reference = 引用
field-reinvocation = 重新调用
field-release-name = Release 名称
field-replicas = 副本
field-repo = 仓库
field-request = 请求
field-resource = 资源
field-resource-names = 资源名称
field-resources = 资源
field-restart-policy = 重启策略
field-retries = 重试次数
field-review-versions = Review 版本
field-revision = 修订版本
field-roles = 角色
field-rule = 规则
field-rules = 规则
field-scale-down = 缩容
field-scale-up = 扩容
field-schedule = 调度计划
field-schema = 模式
field-scope = 作用域
field-scope-selector = 作用域选择器
field-scopes = 作用域
field-secret = Secret
field-selector = 选择器
field-served = 已提供
field-service = Service
field-service-account = 服务账号
field-serviceaccount = ServiceAccount
field-session-affinity = 会话亲和性
field-short-names = 简称
field-side-effects = 副作用
field-source = 来源
field-started = 开始时间
field-starting-deadline = 启动截止时间
field-status = 状态
field-storage = 存储
field-storage-class = 存储类
field-stored-versions = 存储版本
field-strategy = 策略
field-subjects = 主体
field-suspend = 暂停
field-suspended = 已暂停
field-sync-status = 同步状态
field-synced-revision = 已同步修订版本
field-target = 目标
field-target-namespace = 目标 Namespace
field-target-revision = 目标修订版本
field-template-name = 模板名称
field-timeout = 超时
field-timezone = 时区
field-to = 去向
field-tolerations = 容忍度
field-total = 总计
field-type = 类型
field-types = 类型
field-uncapped = 未封顶
field-unhealthy-pod-eviction = 不健康 Pod 驱逐
field-unschedulable = 不可调度
field-update-mode = 更新模式
field-update-strategy = 更新策略
field-upper = 上限
field-url = URL
field-value = 值
field-verbs = 动作
field-version = 版本
field-volume = 卷
field-volume-expansion = 卷扩容
field-volume-mode = 卷模式
field-webhooks = Webhook

## Values of detail fields that are texts of ours (by their English text)

value-yes = 是
value-no = 否
value-not-allowed-kubernetes-keeps-this-namespace = 不允许：Kubernetes 保留此 Namespace
value-system-namespace-deleting-it-asks-to-type-its-name = 系统 Namespace：删除时需要输入其名称
value-deleting-the-objects-inside-nothing-reported-as-stuck-yet = 正在删除其中的对象；尚未报告卡住的对象
value-no-rule-all-ingress-is-denied = 无规则：拒绝所有入站流量
value-no-rule-all-egress-is-denied = 无规则：拒绝所有出站流量
value-all-pods = 所有 Pod
value-all-namespaces = 所有 Namespace

