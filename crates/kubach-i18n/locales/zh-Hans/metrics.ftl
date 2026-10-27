# 指标：详情中的 Metrics 区块、Pods 和 Nodes 表格的用量列、上下文的
# Prometheus 以及 Overview 界面。metrics-server、Prometheus、
# kube-state-metrics、Pods、Nodes、pod 阶段（Running、Pending）、时间范围
# （“15m”）、数量（“250m”“512Mi”）和 Prometheus 地址均为数据，保持原样。
# 机器翻译草稿，需要母语者校对。

## 指标区块

metrics-memory = 内存
metrics-network = 网络
metrics-filesystem = 文件系统
metrics-line-working-set = 工作集
metrics-line-receive = 接收
metrics-line-transmit = 发送
metrics-line-read = 读取
metrics-line-write = 写入
metrics-total = 总计
metrics-by-container = 按容器
metrics-by-pod = 按 pod
metrics-now = 现在
metrics-request = 请求
metrics-limit = 限制
metrics-allocatable = 可分配
metrics-looking-for-prometheus = 正在查找 Prometheus…
metrics-no-source = 没有指标：未找到 Prometheus 或 metrics-server
metrics-needs-prometheus = 需要 Prometheus：metrics-server 只报告 CPU 和内存
metrics-waiting-server = 正在等待 metrics-server…
metrics-server-no-answer = metrics-server 没有响应
metrics-server-no-sample = metrics-server 尚无此对象的样本
metrics-loading-prometheus = 正在从 Prometheus 加载…
metrics-prometheus-no-answer = Prometheus 没有响应
metrics-no-data = Prometheus 中没有此时间范围的数据
metrics-server-while-open = metrics-server · 面板打开期间
metrics-unavailable = 指标不可用
metrics-coverage-less =
    { $subject ->
        [pods] Prometheus 中这些 pod 的数据不足 { $range }
       *[object] Prometheus 中此对象的数据不足 { $range }
    }
metrics-coverage-same =
    { $subject ->
        [pods] Prometheus 中这些 pod 只有 { $age } 的数据：与 { $range } 相同
       *[object] Prometheus 中此对象只有 { $age } 的数据：与 { $range } 相同
    }
metrics-live-collected =
    { $place ->
        [screen] 自界面打开以来已收集：{ $age }
       *[panel] 自面板打开以来已收集：{ $age }
    }

## 上下文的 Prometheus

prometheus-doesnt-answer = { $endpoint } 上的 Prometheus 没有响应
prometheus-invalid-address = Prometheus 地址 { $address } 无效
prometheus-unusual-namespace = Prometheus { $endpoint } 位于不常见的 namespace 中：确认前不会使用
prometheus-use-it = 使用
prometheus-use-it-tooltip = 为此上下文使用该 Prometheus
prometheus-detect-again = 重新检测
prometheus-settings = 设置
prometheus-cannot-connect = 无法连接：{ $error }

## Overview 界面

overview-all-namespaces = 所有 namespace
overview-namespaces = { $count } 个 namespace
overview-not-allowed = 无权列出
overview-cant-list = 无法列出
overview-ready = 就绪
overview-not-ready = 未就绪
overview-whole-cluster = 整个集群
overview-restarts = { $count } 次重启
overview-restarts-recent = { $count } 次重启 · 最近 { $range } 内 { $recent } 次
overview-restarts-since = { $count } 次重启 · 界面打开以来 { $since } 次
overview-of-slots = 槽位
overview-pods-of = { $count } / { $capacity } 个 pod
overview-in-scope = 范围：{ $scope }
overview-pod-slots-note = Pod 槽位：所有节点可分配的 pod 数（整个集群）
overview-used = 已用
overview-requested = 已请求
overview-unlimited = { $count } 个 pod 没有限制
overview-limits = 限制
overview-segment-used = 已用
overview-requests = 请求
overview-used-of-allocatable = 占可分配量
overview-usage-unknown = 用量未知
overview-pods-scope = pod：{ $scope }
overview-usage-from = 用量来自 { $source } · 上限：可分配量，整个集群
overview-max-allocatable = 上限：可分配量，整个集群
overview-cores = { $value } / { $max } 核
overview-usage-server-no-answer = 用量不可用：metrics-server 没有响应
overview-usage-loading-prometheus = 正在从 Prometheus 加载用量…
overview-usage-prometheus-no-answer = 用量不可用：Prometheus 没有响应
overview-usage-no-samples = 用量不可用：Prometheus 中没有 { $scope } 中 pod 的样本
overview-usage-see-problem = 用量不可用：请查看上方的 Prometheus 问题
overview-usage-none = 用量不可用：请安装 metrics-server 或在“设置 → 集群”中设置 Prometheus
overview-cpu-memory = CPU 和内存
overview-history = { $scope } 中的 Pod
overview-history-prometheus = { $scope } 中的 Pod · Prometheus · { $endpoint }
overview-history-session = { $scope } 中的 Pod · metrics-server · 界面打开期间
overview-pods-by-phase = 按阶段统计的 Pod
overview-phases-prometheus = { $scope } · Prometheus 中的 kube-state-metrics
overview-phases-session = { $scope } · 来自缓存，界面打开期间
overview-phases-collecting = 正在收集 pod 数量：界面打开期间图表会持续增长
overview-metrics-unavailable = 指标不可用
overview-metrics-unavailable-description = 此上下文没有 metrics-server 或 Prometheus：用量和图表需要其中之一。来自集群的计数仍然可用。
overview-prometheus-settings = Prometheus 设置
overview-cluster = 集群
overview-context = 上下文
overview-user = 用户
overview-server-version = 服务器版本
overview-api-resources-title = API 资源
overview-discovering = 正在发现…
overview-api-resources = { $groups } 个组中的 { $kinds } 个 kind
overview-custom-resources = 自定义资源
