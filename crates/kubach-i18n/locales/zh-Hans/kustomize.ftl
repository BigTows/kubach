# Kustomize：构建 kustomization 文件夹、构建产出的对象以及与集群的比较。
# 路径、kind、对象名称、API 版本以及 kustomize 和 kubectl 的输出均为数据。
# Kubernetes 术语（kind、namespace、Secret、kustomization、helmCharts）不翻译。
# 机器翻译草稿，需要母语者校对。

kustomize-description = 构建 kustomization 文件夹并与集群比较。
kustomize-recent = 最近
kustomize-recent-tooltip = 最近的文件夹
kustomize-enable-helm-tooltip = 展开 helmCharts（需要 PATH 中有 helm）
kustomize-rebuild-tooltip = 重新构建
kustomize-open-folder = 打开文件夹…
kustomize-no-folder = 未打开文件夹
kustomize-no-folder-description = 选择包含 kustomization.yaml 的文件夹。Kubach 在本地运行 `kustomize build`（或 `kubectl kustomize`）；不会应用任何内容。
kustomize-building = 正在构建…

## 构建错误

kustomize-error-no-tool = kustomize 不可用
kustomize-error-no-tool-hint = 安装 kustomize 或 kubectl，并确保它在 PATH 中（Kubach 从你的登录 shell 读取 PATH）。
kustomize-error-no-tool-details = PATH 中既没有 `kustomize` 也没有 `kubectl`
kustomize-error-not-kustomization = 不是 kustomization 文件夹
kustomize-error-not-kustomization-hint = 选择包含 kustomization.yaml、kustomization.yml 或 Kustomization 的文件夹。
kustomize-error-not-kustomization-details = { $path } 中没有 kustomization.yaml
kustomize-error-spawn = 无法运行 kustomize
kustomize-error-spawn-hint = 请检查该程序是否存在且可执行。
kustomize-error-spawn-details = 无法运行 { $program }：{ $error }
kustomize-error-timeout = 构建超时
kustomize-error-timeout-hint = 远程 base 和 helmCharts 需要网络访问；请检查 URL。
kustomize-error-timeout-details = kustomize build 未在 { $seconds } 秒内完成
kustomize-error-failed = 构建失败
kustomize-error-unreadable = 构建输出无法读取
kustomize-error-not-utf8 = kustomize 输出不是 UTF-8
kustomize-error-yaml = kustomize 输出无效：{ $error }
kustomize-error-interrupted = 构建被中断

## 对象

kustomize-column-kind = Kind
kustomize-column-name = 名称
kustomize-column-namespace = Namespace
kustomize-column-api-version = API 版本
kustomize-column-cluster = 集群中
kustomize-status-new = 新增
kustomize-status-unchanged = 未更改
kustomize-status-changed = 已更改
kustomize-status-error = 错误
kustomize-tab-resources = 资源
kustomize-tab-output = 输出
kustomize-tab-diff = 与集群比较
kustomize-objects = { $count } 个对象
kustomize-compare-connect = 连接集群以进行比较
kustomize-compare-running = 正在与集群比较…
kustomize-compare-result = 集群：{ $counts }
kustomize-built-with = 构建工具：{ $tool }
kustomize-count-new = { $count } 个新增
kustomize-count-changed = { $count } 个已更改
kustomize-count-unchanged = { $count } 个未更改
kustomize-count-failed = { $count } 个未比较
kustomize-count-nothing = 没有可比较的内容
kustomize-empty-build = 空构建
kustomize-empty-build-description = 该 kustomization 未产生任何对象。
kustomize-secrets-hidden = Secret 值已隐藏
kustomize-secrets-hidden-tooltip = 此上下文的安全设置隐藏了 Secret 的值。
kustomize-secrets-hide = 隐藏 Secret 值
kustomize-secrets-reveal = 显示 Secret 值

## 与集群比较

kustomize-compare-error = 无法比较
kustomize-comparing = 正在与集群比较…
kustomize-no-cluster = 未连接集群
kustomize-no-cluster-description = 连接集群以将构建结果与之比较。
kustomize-no-differences = 没有差异
kustomize-no-differences-description = 线上对象已包含构建设置的所有字段。
kustomize-diff-new = 集群中尚不存在：应用后会创建它。
kustomize-diff-fields = { $count } 个字段不同。只比较构建设置的字段；不包括服务器默认值和 status。
