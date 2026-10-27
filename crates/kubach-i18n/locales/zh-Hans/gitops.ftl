# GitOps 界面：Argo CD Applications、Flux Kustomizations、HelmReleases 及源。
# 同步与健康状态（Synced、OutOfSync、Healthy）、kind、名称、修订版本和错误
# 均为数据，保持原样。
# 机器翻译草稿，需要母语者校对。

## 关注级别（状态列与徽章）

gitops-attention-ok = 正常
gitops-attention-unknown = 未知
gitops-attention-suspended = 已暂停
gitops-attention-pending = 进行中
gitops-attention-warning = 警告
gitops-attention-error = 错误

## 列表

gitops-problems-only = 仅显示问题
gitops-objects = { $count } 个对象
gitops-count-failing = 失败
gitops-count-out-of-sync = 未同步
gitops-count-in-progress = 进行中
gitops-count-suspended = 已暂停
gitops-count-healthy = 健康
gitops-list-error = { $kind }：{ $error }
gitops-list-error-in = { $kind }（{ $namespace }）：{ $error }
gitops-cant-list-kinds = 无法列出 { $errors }
gitops-noun-problems = 问题
gitops-noun-objects = GitOps 对象
gitops-no-problems = 没有问题
gitops-no-problems-description = 一切均已同步、已协调或正在进行中。

## 无内容

gitops-no-tools = 没有 GitOps 工具
gitops-no-tools-description = 此集群既没有 Argo CD，也没有 Flux。
gitops-cant-list = 无法列出 GitOps 对象
gitops-empty = 没有 Applications、Kustomizations 或 HelmReleases
gitops-empty-any-namespace = 所有 namespace 中都没有。
gitops-empty-selected-namespaces = 所选 namespace 中没有。Argo CD Applications 通常位于 argocd，Flux 对象位于 flux-system。

## 详情

gitops-cluster-scoped = { $kind } · 集群
gitops-badge-sync = 同步：{ $state }
gitops-badge-ready = 就绪：{ $state }
gitops-badge-health = 健康：{ $state }
gitops-errors = 错误
gitops-overview = 概览
gitops-revision = 修订版本
gitops-attempted-revision = 尝试的修订版本
gitops-destination = 目标
gitops-ref = Ref
gitops-target-namespace = 目标 namespace
gitops-url = URL
gitops-source = 源
gitops-source-object = 源对象
gitops-status-changed = 状态变更时间
gitops-suspended = 已暂停
gitops-managed = 托管资源 · { $count }
