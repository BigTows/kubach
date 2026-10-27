# Topology 界面：Ingress、Service、工作负载及其依赖关系的图，网络策略层与
# 流量层，以及导出。kind 名称（Ingress、Service、Deployment）、
# “ingress”/“egress”、对象名称、标签选择器、Service 类型（headless、
# NodePort）以及 NetworkPolicy 规则均为数据，保持原样。
# 机器翻译草稿，需要母语者校对。

## 筛选与工具栏

topology-search-placeholder = 查找节点…
topology-labels-placeholder = 标签：app=web,tier!=db
topology-detail-workloads = 工作负载
topology-detail-workloads-tooltip = Pod 折叠到其工作负载中
topology-detail-pods = Pods
topology-detail-pods-tooltip = 每个 pod 一个节点
topology-class-routes = 路由
topology-class-workloads = 工作负载
topology-class-external = 外部
topology-class-hide = 隐藏
topology-class-show = 显示
topology-size = { $nodes } 个节点 · { $edges } 条连接
topology-no-matches = 无匹配
topology-match = 第 { $position } 个，共 { $count } 个
topology-policies = 网络策略
topology-policies-tooltip = 显示 NetworkPolicy 隔离和阻止的内容。{ $note }
topology-issues = 问题 · { $count }
topology-issues-tooltip = 图中发现的问题
topology-broken-references = { $count } 个失效引用

## 无内容

topology-nothing = 没有可显示的内容
topology-nothing-filtered = 没有符合筛选条件的对象。
topology-nothing-selected-namespaces = 所选 namespace 中没有 Ingress、Service 或工作负载。
topology-nothing-any = 没有 Ingress、Service 或工作负载。
topology-error-no-access = 无权访问 { $kinds }：图中缺少它们。
topology-error-cant-refresh = 无法刷新 { $kinds }，显示缓存数据。正在重试…

## 网络策略层

topology-policies-scope = 仅分析标准的 networking.k8s.io/v1 NetworkPolicy（不含 Cilium、Calico 或其他 CRD）；是否生效取决于 CNI。
topology-policies-loading = 网络策略：加载中…
topology-policies-none = 网络策略：所选 namespace 中没有。
topology-policies-count = 网络策略：{ $count } 条。
topology-policies-note = { $lead } 被隔离的工作负载带有盾牌标记；红色虚线是被策略阻止的 Service 流量，绿色点线是策略明确允许的流量。{ $note }
topology-policy-section = 网络策略
topology-not-isolated = 未隔离：允许所有 { $direction }
topology-isolated-by = 由 { $policies } 隔离
topology-policy-deny-all = { $direction }：全部拒绝
topology-reach = 能否访问…
topology-reach-target = 目标
topology-reach-target-label = 要访问的目标
topology-reach-search = 查找工作负载或 Service…
topology-port-placeholder = 端口：8080、http、53/udp
topology-port-empty = 请输入端口
topology-port-invalid = 不是端口：{ $text }
topology-port-out-of-range = 端口超出范围：{ $port }
topology-reach-no-pods = 该 Service 没有选中任何 pod：无可访问对象。
topology-reach-allowed = 允许
topology-reach-blocked = 阻止
topology-reach-via = { $port } · 经由 { $via }
topology-reach-egress = 此工作负载的 Egress
topology-reach-ingress = 目标的 Ingress

## 图

topology-presence-missing = 缺失
topology-presence-unknown = 未知
topology-isolated = 已隔离
topology-isolated-in = 入站隔离
topology-isolated-out = 出站隔离
topology-edge-allowed = 已允许
topology-edge-allows = 允许 { $ports }
topology-edge-blocked = 被策略阻止
topology-edge-ready = { $count } 个就绪
topology-edge-ready-of = { $count }/{ $total } 就绪
topology-traffic-requests = { $rate } 请求/秒
topology-traffic-flows = { $rate } 流/秒
topology-traffic-errors = { $percent }% 错误
topology-traffic-dropped = { $percent }% 丢弃
topology-traffic-idle = 空闲
topology-edge-routes-to = 路由到
topology-edge-selects = 选择
topology-edge-owns = 拥有
topology-edge-mounts = 挂载
topology-edge-env-from = 环境变量来自
topology-edge-service-account = service account
topology-edge-exposes = 暴露
topology-edge-tls = TLS 密钥
topology-edge-policy-allows = 策略允许
topology-edge-traffic = 流量
topology-edge-links-to = 链接到
topology-diagnostic-no-ready = { $node } 没有就绪的 endpoints
topology-diagnostic-not-ready = { $node } 没有就绪的 endpoints（{ $count } 个未就绪）
topology-diagnostic-selector = { $node } 的选择器未匹配任何 pod
topology-diagnostic-missing-target = { $node } 引用了缺失的 { $target }
topology-diagnostic-missing-port = { $node } 引用了 { $target } 没有的端口 { $port }

## 问题与详情

topology-no-problems = 未发现问题。
topology-open-details = 打开详情
topology-problems = 问题
topology-overview = 概览
topology-fact-pods-ready = 就绪的 pod
topology-of = { $count } / { $total }
topology-fact-replicas = 副本数
topology-fact-phase = 阶段
topology-fact-cluster-ip = Cluster IP
topology-fact-external-name = 外部名称
topology-fact-ports = 端口
topology-fact-hosts = 主机
topology-fact-class = 类
topology-show-on-graph = 在图中显示
topology-incoming = 入站
topology-outgoing = 出站
topology-missing = 缺失
topology-not-loaded = 未加载

## 流量层

traffic-button = 流量
traffic-button-tooltip = 根据 Istio、Linkerd 或 Cilium Hubble 的 Prometheus 指标显示实测流量
traffic-detect-again-tooltip = 重新在 Prometheus 中查找流量指标
traffic-needs-prometheus = 流量需要带有网格或 CNI 指标的 Prometheus：{ $reason }。
traffic-prometheus-off = 已在设置中为此上下文关闭 Prometheus
traffic-prometheus-looking = 正在查找 Prometheus…
traffic-prometheus-not-found = 集群中未找到 Prometheus
traffic-prometheus-no-address = 设置中未填写 Prometheus 地址
traffic-prometheus-unavailable = Prometheus 不可用
traffic-detecting = 流量：正在查找 Istio、Linkerd 或 Cilium Hubble 指标…
traffic-no-sources = 没有流量指标来源：集群中未安装 Istio、Linkerd 和 Cilium Hubble。图中仅显示声明的连接。
traffic-no-metrics = 没有流量指标来源：已安装 { $sources }（{ $count } 个），但 Prometheus 中没有其流量指标。图中仅显示声明的连接。
traffic-window-tooltip = 最近 { $window } 的速率
traffic-lead-loading = 流量：{ $source } · 加载中…
traffic-lead-none = 流量：{ $source } · 该时间段内无流量
traffic-lead = 流量：{ $source } · { $count } 条流
traffic-lead-busiest = 流量：{ $source } · { $count } 条流（仅显示最繁忙的）
traffic-also-found = 另外发现：{ $sources }（未使用）。
traffic-poll-failed = 上次轮询失败：{ $error }

## 导出

topology-export = 导出
topology-export-tooltip = 按当前显示导出图
topology-export-mermaid = 复制 Mermaid
topology-export-dot = 复制 Graphviz DOT
topology-export-svg = 保存 SVG…
topology-export-png = 保存 PNG…
topology-export-copied = 已将拓扑复制为 { $format }
topology-export-saved = 已保存 { $path }
topology-export-save-failed = 无法保存 { $path }：{ $error }
