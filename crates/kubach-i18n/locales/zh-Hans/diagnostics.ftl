## Notes of the Diagnostics and Security sections

details-note-title = { $severity }：{ $title }
details-note-hint = 提示：{ $hint }
diagnostic-severity-error = 错误
diagnostic-severity-warning = 警告
diagnostic-severity-info = 信息


## Pod diagnostics (kubach-core/src/diagnostics.rs)

diagnostic-container = 容器 `{ $name }`
diagnostic-init-container = Init 容器 `{ $name }`
diagnostic-restarts = 重启 { $count } 次
diagnostic-exit-code = 退出码 { $code }
diagnostic-exit-reason = { $reason }，退出码 { $code }
diagnostic-last-run = 上次运行以退出码 { $code } 结束。
diagnostic-last-run-signal = 上次运行以退出码 { $code }（{ $signal }）结束。
diagnostic-last-run-reason = 上次运行以退出码 { $code } 结束，原因：{ $reason }。
diagnostic-last-run-signal-reason = 上次运行以退出码 { $code }（{ $signal }）结束，原因：{ $reason }。

diagnostic-terminating = Pod 正在终止
diagnostic-terminating-detail = 已请求删除；宽限期将在 { $duration } 后结束。
diagnostic-stuck = Pod 终止卡住已 { $duration }
diagnostic-stuck-detail = 宽限期已在 { $duration } 前结束，但 Pod 仍然存在。
diagnostic-stuck-finalizers-detail = 宽限期已在 { $duration } 前结束；仍有未完成的 finalizer：{ $finalizers }。
diagnostic-stuck-hint = 检查节点是否可达、kubelet 是否正常；强制删除是最后手段
diagnostic-stuck-finalizers-hint = 检查拥有这些 finalizer 的控制器；如果控制器已不存在，可移除 finalizer

diagnostic-evicted = Pod 已被驱逐
diagnostic-evicted-hint = 节点资源不足；请按实际用量设置 requests
diagnostic-pod-failed = Pod 失败：{ $reason }

diagnostic-gated = Pod 正在等待移除调度门控（scheduling gates）
diagnostic-gated-hint = 检查 spec.schedulingGates 及管理它们的控制器
diagnostic-unschedulable = Pod 无法调度
diagnostic-unschedulable-causes = Pod 无法调度：{ $causes }
diagnostic-unschedulable-hint = 阅读调度器消息，并将 Pod 规格与可用节点进行对比
diagnostic-cause-cpu = CPU 不足
diagnostic-cause-memory = 内存不足
diagnostic-cause-resource = { $resource } 不足
diagnostic-cause-taints = 存在未容忍的污点
diagnostic-cause-selector = 没有节点匹配 selector/affinity
diagnostic-cause-pod-affinity = 不满足 Pod 亲和性规则
diagnostic-cause-pvc = PersistentVolumeClaim 未绑定
diagnostic-cause-volume-affinity = 卷的节点亲和性冲突
diagnostic-cause-ports = 主机端口已被占用
diagnostic-resources-hint = 降低容器 requests，或为集群增加容量
diagnostic-taints-hint = 添加匹配的容忍度，或调度到没有这些污点的节点
diagnostic-selector-hint = 对照节点标签检查 nodeSelector/affinity
diagnostic-pod-affinity-hint = 对照正在运行的 Pod 检查 podAffinity/podAntiAffinity 规则
diagnostic-pvc-hint = 检查 PVC 是否存在，以及其 StorageClass 能否创建卷
diagnostic-volume-affinity-hint = 已绑定的卷位于 Pod 无法调度到的区域或节点
diagnostic-ports-hint = 去掉 hostPort，或释放节点上的该端口

diagnostic-crash-looping = { $container } 循环崩溃（{ $restarts }）
diagnostic-crash-looping-exit = { $container } 循环崩溃（{ $exit }，{ $restarts }）
diagnostic-previous-logs-hint = 查看上次运行的日志
diagnostic-image-pull = { $container } 无法拉取镜像 `{ $image }`（{ $reason }）
diagnostic-image-name-hint = 修正 Pod 规格中的镜像引用
diagnostic-never-pull-hint = imagePullPolicy 为 Never，但节点上没有该镜像
diagnostic-credentials-hint = 检查 imagePullSecrets 和镜像仓库凭据
diagnostic-image-missing-hint = 检查镜像仓库中是否存在该镜像名和标签
diagnostic-image-hint = 检查镜像名、标签，以及节点能否访问镜像仓库
diagnostic-missing-configmap = { $container } 引用了不存在的 ConfigMap `{ $name }`
diagnostic-missing-secret = { $container } 引用了不存在的 Secret `{ $name }`
diagnostic-invalid-config = { $container } 的配置无效
diagnostic-config-hint = 创建所引用的 ConfigMap/Secret，或修正 env/envFrom 引用
diagnostic-waiting = { $container } 正在等待：{ $reason }
diagnostic-failed = { $container } 失败（{ $exit }）
diagnostic-logs-hint = 查看容器日志
diagnostic-restarted = { $container } 曾重启（{ $exit }，{ $restarts }）

diagnostic-oom = { $container } 因 OOMKilled 被终止（{ $restarts }）
diagnostic-oom-looping = { $container } 因 OOMKilled 被终止（{ $restarts }，循环崩溃）
diagnostic-oom-limit = { $container } 因 OOMKilled 被终止（限制 { $limit }，{ $restarts }）
diagnostic-oom-limit-looping = { $container } 因 OOMKilled 被终止（限制 { $limit }，{ $restarts }，循环崩溃）
diagnostic-oom-limit-detail = 容器超出内存限制 { $limit }，已被终止。
diagnostic-oom-limit-hint = 将内存限制提高到 { $limit } 以上，或降低内存用量
diagnostic-oom-detail = 容器因占用内存过多被终止；它没有内存限制，因此是节点本身内存耗尽。
diagnostic-oom-hint = 按容器实际用量设置内存 requests/limits

diagnostic-startup-probe = { $container } 尚未通过启动探针
diagnostic-startup-probe-hint = 检查启动探针和容器日志
diagnostic-not-ready = { $container } 正在运行但未就绪
diagnostic-not-ready-hint = 检查就绪探针和容器日志
diagnostic-pod-not-ready = Pod 正在运行但未就绪
diagnostic-pod-not-ready-hint = 检查 spec.readinessGates 和 Pod 条件

diagnostic-volume-mount = 无法挂载卷
diagnostic-volume-configmap-hint = 创建 ConfigMap `{ $name }`，或修正卷引用
diagnostic-volume-secret-hint = 创建 Secret `{ $name }`，或修正卷引用
diagnostic-volume-hint = 检查卷、其 PVC 和存储驱动
diagnostic-sandbox = 无法创建 Pod 沙箱
diagnostic-sandbox-hint = 检查节点上的 CNI 插件和容器运行时


## Phrases of detail fields with data in them

details-joined = { $first } · { $second }
gateway-listener-port = { $protocol } :{ $port }
gateway-listener-host = 主机 { $host }
gateway-listener-tls = TLS { $mode }
gateway-listener-routes-from = 路由来源 { $from }
gateway-listener-routes-attached = 已关联 { $count } 个路由
crd-show-objects = 显示 { $plural }
crd-version-notes = { $version }（{ $notes }）
crd-version-storage = 存储
crd-version-not-served = 未提供服务
crd-version-deprecated = 已弃用
namespace-condition-hint = { $message } — { $hint }
namespace-finalizers-hint = 拥有这些 finalizer 的控制器必须移除它们（或手动移除）
namespace-discovery-hint = 某个聚合 API（通常是 metrics.k8s.io）没有响应；请修复或删除其 APIService
