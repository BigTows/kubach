# kubach-core 内置资源插件在界面中的文本：列标题、操作名称、详情分区标题和
# 健康状态。core 中保持英文（供 MCP 代理读取），应用按 id 翻译。
# Kubernetes 术语（Namespace、Node、Kind、Role、Chart…）不翻译。
# 机器翻译草稿，需要母语者校对。


## 列标题

column-access-modes = 访问模式
column-active = 活跃
column-address = 地址
column-addresses = 地址
column-age = 存在时间
column-allowed = 允许中断数
column-applications = 应用
column-available = 可用
column-backends = 后端
column-binding-mode = 绑定模式
column-capacity = 容量
column-chart = Chart
column-claim = Claim
column-class = 类
column-cluster-ip = Cluster IP
column-completions = 完成数
column-containers = 容器
column-controller = 控制器
column-cpu = CPU
column-current = 当前
column-data = 数据
column-default = 默认
column-desired = 期望
column-destination = 目标位置
column-duration = 持续时间
column-egress = 出站
column-expansion = 扩容
column-external-ip = 外部 IP
column-failure-policy = 失败策略
column-from = 来源
column-generators = 生成器
column-global-default = 全局默认
column-group = 组
column-health = 健康状况
column-healthy = 健康
column-hostnames = 主机名
column-hosts = 主机
column-images = 镜像
column-ingress = 入站
column-internal-ip = 内部 IP
column-ip = IP
column-kind = Kind
column-labels = 标签
column-last-schedule = 上次调度
column-listeners = 监听器
column-max = 最大
column-max-unavailable = 最大不可用
column-memory = 内存
column-min = 最小
column-min-available = 最小可用
column-mode = 模式
column-most-used = 最高用量
column-name = 名称
column-namespace = Namespace
column-node = Node
column-node-selector = Node 选择器
column-parameters = 参数
column-parents = 父级
column-path = 路径
column-pod-selector = Pod 选择器
column-policy-types = 策略类型
column-ports = 端口
column-preemption = 抢占
column-project = 项目
column-provisioner = 制备器
column-ready = 就绪
column-reclaim-policy = 回收策略
column-ref = Ref
column-reference = 引用
column-replicas = 副本
column-repo = 仓库
column-request = 请求
column-resources = 资源
column-restarts = 重启次数
column-revision = 修订版本
column-role = Role
column-roles = Role
column-rules = 规则
column-runtime = 容器运行时
column-schedule = 调度计划
column-scope = 作用域
column-selector = 选择器
column-source = 来源
column-status = 状态
column-storage-class = 存储类
column-subjects = 主体
column-suspend = 暂停
column-suspended = 已暂停
column-sync = 同步状态
column-target = 目标
column-targets = 目标
column-timezone = 时区
column-to = 去向
column-type = 类型
column-types = 类型
column-up-to-date = 已更新
column-url = URL
column-value = 值
column-version = 版本
column-versions = 版本
column-volume = 卷
column-webhooks = Webhook

## 操作

action-delete = 删除
action-scale = 扩缩容
action-restart = 滚动重启
action-trigger = 立即触发
action-suspend = 暂停
action-resume = 恢复
action-cordon = 封锁
action-uncordon = 解除封锁
action-evict = 驱逐
action-logs = 日志
action-exec = Exec
action-port-forward = 端口转发
action-debug = 调试
action-refresh = 刷新
action-hard-refresh = 强制刷新
action-sync = 同步
action-reconcile = 协调

## 详情分区

section-active-jobs = 活跃的 Job
section-addresses = 地址
section-allocatable = 可分配
section-allowed-topologies = 允许的拓扑
section-annotations = 注解
section-applications = 应用
section-backends = 后端
section-behavior = 行为
section-binary-data = 二进制数据
section-capacity = 容量
section-conditions = 状况
section-container-policies = 容器策略
section-containers = 容器
section-data = 数据
section-deletion-blocked-by = 阻止删除的对象
section-diagnostics = 诊断
section-disrupted-pods = 被中断的 Pod
section-egress-rules = 出站规则
section-finalizers = Finalizers
section-from = 来源
section-history = 历史
section-images = 镜像
section-ingress-rules = 入站规则
section-init-containers = Init 容器
section-inventory = 清单
section-labels = 标签
section-listeners = 监听器
section-metrics = 指标
section-operation = 操作
section-owner-references = 所有者引用
section-parameters = 参数
section-ports = 端口
section-probes = 探针
section-recommendations = 建议
section-resources = 资源
section-resources-summary = 资源摘要
section-rules = 规则
section-rules-aggregated = 规则（聚合）
section-scheduling = 调度
section-security = 安全
section-selector = 选择器
section-source = 来源
section-sources = 来源
section-subjects = 主体
section-sync-history = 同步历史
section-sync-result = 同步结果
section-system-info = 系统信息
section-taints = 污点
section-template-sources = 模板来源
section-to = 去向
section-usage = 使用情况
section-used-by-pods = 使用中的 Pod
section-versions = 版本
section-volume-claim-templates = 卷申领模板
section-volumes = 卷

## 健康状态

health-healthy = 健康
health-progressing = 进行中
health-degraded = 降级
health-failed = 失败
health-unknown = 未知

## 带数据的分区标题

section-limits = { $item } 限制
section-parent = 父级 { $parent }
