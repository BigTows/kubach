# Helm：Release 与 Chart 页面、Release 详情、回滚、卸载、升级与安装、OCI Chart
# 版本、设置 → 外部工具（helm 程序与 Kubach 下载的副本），以及在其他上下文中
# 请求的更改为何未执行。Release、Chart、仓库、namespace 与上下文的名称、版本、
# 状态（deployed、pending-upgrade）、helm 命令及其输出均为数据，保持原样。
# Kubernetes 与 Helm 术语（Helm、release、chart、values、manifest、hook、
# Secret、ConfigMap、PVC、PersistentVolumeClaim、CRD、OCI）不翻译。
# 机器翻译草稿，需要母语者校对。

## 页面

helm-page-releases = Helm Release
helm-page-charts = Helm Chart
helm-no-cluster = 未连接集群
helm-connecting = 正在连接…
helm-not-connected = 未连接
helm-loading = 正在加载…
helm-cancel = 取消
helm-back = 返回
helm-dismiss = 关闭

## Release 列表

helm-reload-releases = 重新加载 Release
helm-reloading-releases = 正在重新加载 Release…
helm-search-releases = 按名称搜索 Release…
helm-noun-releases = Release
helm-driver-detected = 存储于 { $kind }
helm-driver-settings = 存储于 { $kind }（按设置）
helm-driver-default = 在 { $kind } 中查找
helm-releases-scope =
    { $scope ->
        [all] 位于所有 namespace
        [one] 位于 namespace { $namespace }
       *[many] 位于 { $namespaces } 个 namespace
    }
helm-releases-loading = 正在加载 Helm Release…
helm-releases-list-error = 无法列出 Helm Release
helm-all-namespaces = 所有 namespace
helm-releases-forbidden = 无权访问 { $namespaces } 中的 Helm Release { $kind }：你的凭据无法在那里列出 { $kind }。
helm-releases-list-failed = 无法列出 { $namespaces } 中的 Release：{ $error }
helm-releases-none = 没有 Helm Release
helm-releases-none-description = { $scope }未找到任何内容（{ $driver }）。可在「设置 → 集群」中为每个上下文设置存储驱动。
helm-releases-none-shown = 未显示 Helm Release
helm-releases-summary = { $count } 个 Release { $scope } · { $driver }
helm-releases-summary-filtered = { $shown } / { $count } 个 Release { $scope } · { $driver }
helm-cluster-serves-no = 集群不提供 { $kind }
helm-listing-interrupted = 列出被中断：{ $error }

## Release 详情

helm-back-releases = Release
helm-release-loading = 正在加载 Release…
helm-release-load-error = 无法加载 Release
helm-release-not-found = 未找到 Release
helm-loading-interrupted = 加载被中断：{ $error }
helm-revision = 修订版 { $revision }
helm-revision-latest = { $revision }（最新）
helm-revision-of = { $revision } / { $latest }
helm-field-status = 状态
helm-field-revision = 修订版
helm-field-chart = Chart
helm-field-version = 版本
helm-field-app-version = 应用版本
helm-field-updated = 更新时间
helm-field-first-deployed = 首次部署
helm-field-description = 描述
helm-field-labels = 标签
helm-field-repository = 仓库
helm-field-home = 主页
helm-field-sources = 源码
helm-field-keywords = 关键词
helm-field-maintainers = 维护者
helm-field-created = 创建时间
helm-column-name = 名称
helm-column-namespace = Namespace
helm-showing-old-revision = 正在显示修订版 { $revision }——不是 Release 的当前状态。
helm-show-latest = 显示最新
helm-update-available = 仓库 { $repository } 中有新版本 { $version }
helm-update-available-app = 仓库 { $repository } 中有新版本 { $version }（应用 { $app }）
helm-upgrade-to-tooltip = 从 { $repository } 升级到 { $version }
helm-tab-values = Values
helm-tab-computed-values = 计算后的 Values
helm-tab-manifest = Manifest
helm-tab-hooks = Hook
helm-tab-notes = 说明
helm-tab-history = 历史
helm-tab-diff = 差异
helm-hidden-secrets = 已隐藏：可能包含机密
helm-hidden-secrets-description = Values、manifest 和 hook 中常含有密码和令牌。请只在无人旁观时显示。
helm-secret-values-hidden = 此上下文的安全设置隐藏了 Secret 的值。
helm-reveal = 显示
helm-notes-none = 没有说明
helm-notes-none-description = 该 Chart 没有 NOTES.txt。
helm-history-hint = 选择一个修订版，可在其他标签页查看或回滚到该版本。
helm-values-warning = 警告：{ $warning }
helm-hooks-none = 此修订版没有 hook。
helm-hook-header = { $kind } { $name } · { $events } · 权重 { $weight }
helm-hook-delete-policy = 删除策略：{ $policies }
helm-hook-last-run = 上次运行：{ $phase }

## 差异标签页

helm-diff-nothing = 没有可比较的内容
helm-diff-single-revision = 该 Release 只有一个修订版。
helm-diff-from = 从
helm-diff-to = 到
helm-diff-none = 没有差异
helm-diff-identical = 修订版 { $from } 与 { $to } 的 { $source } 完全相同。

## 回滚、升级、卸载

helm-rollback-button = 回滚到 { $revision }
helm-rollback-title = 将 { $name } 回滚到修订版 { $revision }？
helm-rollback-message = Helm 将在 { $namespace } 中为 { $name } 创建修订版 { $next }，使用修订版 { $revision } 的 manifest 和 values；Chart 的回滚 hook 会运行。
helm-rollback-confirm = 回滚
helm-rollback-toast = 将 { $name } 回滚到修订版 { $revision }
helm-upgrade-button = 升级…
helm-upgrade-tooltip = 编辑 values、预览 manifest 差异并升级
helm-upgrade-title = 升级 { $name }？
helm-upgrade-confirm-message = Helm 将按预览在 { $namespace } 中为 { $name } 创建新修订版。
helm-upgrade-confirm-message-moved = Release 在修订版 { $revision } 之后已更改；预览可能已过时。Helm 将按预览在 { $namespace } 中为 { $name } 创建新修订版。
helm-upgrade-confirm = 升级
helm-upgrade-toast = 升级 { $name }
helm-upgrade-release-moved = Release 自修订版 { $revision } 以来已更改；预览可能已过时。
helm-uninstall-button = 卸载…
helm-uninstall-title = 卸载 { $name }？
helm-uninstall-keep-history = 保留历史（--keep-history）：Release 将以已卸载状态保留在列表中，并可回滚
helm-uninstall-delete-claims =
    { $count ->
       *[other] 卸载后同时删除这些 PVC（其数据将丢失）
    }
helm-uninstall-confirm = 卸载
helm-uninstall-toast = 卸载 { $name }
helm-uninstall-delete-hooks = 将先运行 Chart 的 { $count } 个删除 hook。
helm-uninstall-resources = Helm 将删除 { $namespace } 中的 Release { $name } 及其 { $count } 个资源：
helm-uninstall-no-resources = Helm 将删除 { $namespace } 中的 Release { $name }；其 manifest 中没有资源。
helm-claims-stay = 以下 { $count } 个 PersistentVolumeClaim 会保留：Helm 不删除由 StatefulSet 创建或被 Chart 保留的 claim，因此其卷和数据仍在。
helm-claims-check-failed = 无法检查卸载后会保留哪些 PersistentVolumeClaim：{ $error }
helm-lookup-interrupted = 查找被中断：{ $error }
helm-delete-claim-toast = 删除 PVC { $name }
helm-delete-claims-toast = 删除 { $count } 个 PVC
helm-delete-claims-detail = 位于 { $namespace }：{ $names }
helm-claims-not-deleted = PVC 未删除：{ $reason }
helm-claims-not-deleted-count = { $count } 个 PVC 未删除
helm-delete-interrupted = 删除被中断：{ $error }
helm-stop-install-title = 停止安装 { $release }？
helm-stop-upgrade-title = 停止升级 { $release }？
helm-stop-message = 将要求 Helm 停止等待，它会把 Release 标记为 failed；已创建的资源会保留，正在运行的 hook Job 会继续。如果 helm 在 { $seconds } 秒内未退出，将被强制结束，Release 可能一直处于 { $status }，直到被回滚或卸载。
helm-call-interrupted = helm 调用被中断：{ $error }
helm-installed = 已在 { $namespace } 中安装 { $name }
helm-output-show = 显示 helm 输出
helm-output-hide = 隐藏 helm 输出

## 在其他上下文中请求的更改不再执行

helm-context-changed = 上下文已切换到 { $context }；操作已取消
helm-context-reconnected = 已重新连接到 { $context }；操作已取消
helm-context-disconnected = 已断开与 { $context } 的连接；操作已取消

## Chart 页面

helm-search-charts = 搜索 Chart…
helm-no-home = 找不到 Helm 配置：没有主目录。
helm-repositories = 仓库
helm-repo-charts = { $count } 个 Chart
helm-repo-index-missing = 未下载：请更新
helm-repo-index-invalid = 索引无法读取：{ $error }
helm-repo-show-all = 显示所有仓库的 Chart
helm-repo-show-only = 仅显示此仓库的 Chart
helm-repo-update-tooltip = 更新索引（helm repo update）
helm-repo-remove-tooltip = 移除仓库（helm repo remove）
helm-repo-update-all-tooltip = 更新所有索引（helm repo update）
helm-repo-add-tooltip = 添加仓库（helm repo add）
helm-repo-reading = 正在读取仓库…
helm-repo-none-yet = 还没有仓库。通过 URL 添加一个。
helm-repo-name-placeholder = 名称，例如 prometheus-community
helm-repo-username-placeholder = 用户名（可选）
helm-repo-password-placeholder = 密码（可选）
helm-repo-private-note = 私有仓库：helm 将登录信息保存在其仓库文件中；密码通过 stdin 传递（--password-stdin）。凭据只会发送到 https:// 仓库。
helm-repo-add = 添加
helm-repo-exists = 名为 { $name } 的仓库已存在。
helm-repo-username-missing = 请输入与密码对应的用户名。
helm-repo-config-from = 来自 { $path }
helm-repo-adding = 正在添加仓库 { $name }
helm-repo-updating-all = 正在更新 Chart 仓库
helm-repo-updating = 正在更新仓库 { $name }
helm-repo-updating-count = 正在更新 { $count } 个仓库
helm-repo-removing = 正在移除仓库 { $name }
helm-repo-remove-title = 移除仓库 { $name }？
helm-repo-remove-message = 将从你的 Helm 仓库中移除 { $name }（终端中的 helm 也一样），并删除其缓存索引。已安装的 Release 不受影响。
helm-repo-remove-confirm = 移除
helm-charts-reading = 正在读取 Chart 仓库…
helm-charts-no-repositories = 没有 Chart 仓库
helm-charts-no-repositories-description = 通过 URL 添加仓库；Kubach 运行 helm repo add，因此这就是你的 Helm 列表，与终端共享。来自 OCI 仓库或 URL 的 Chart 无需仓库即可安装：「从 OCI 或 URL 安装…」。
helm-charts-no-indexes = 未下载索引
helm-charts-no-indexes-description = 更新仓库（helm repo update）以搜索其中的 Chart。
helm-charts-repo-no-index = { $name } 没有已下载的索引：更新后即可列出其 Chart。
helm-charts-repo-no-match = { $name } 中没有匹配项。清除仓库筛选以搜索全部仓库。
helm-charts-no-match = 缓存索引中没有匹配项。更新索引以获取新 Chart。
helm-charts-none-found = 未找到 Chart
helm-charts-summary =
    { $what ->
        [versions] 缓存索引中有 { $count } 个版本
       *[charts] 缓存索引中有 { $count } 个 Chart
    }
helm-charts-summary-cut =
    { $what ->
        [versions] { $count } 个版本；仅显示前 { $shown } 个，请细化搜索
       *[charts] { $count } 个 Chart；仅显示前 { $shown } 个，请细化搜索
    }
helm-charts-repo-chip = 仓库：{ $name }
helm-charts-all-versions = 所有版本
helm-install-oci = 从 OCI 或 URL 安装…
helm-install-oci-tooltip = 无需仓库，安装 oci:// Chart、路径或 URL
helm-charts-refresh-tooltip = 重新读取仓库和缓存索引
helm-chart-deprecated = 已弃用。{ $description }
helm-chart-status-deprecated = 已弃用

## Chart 卡片

helm-back-charts = Chart
helm-chart-not-found = 未找到 Chart
helm-chart-not-found-description = 它已不在该仓库的缓存索引中。
helm-chart-version = 版本 { $version }
helm-chart-version-app = { $version }（应用 { $app }）
helm-install-button = 安装…
helm-card-readme = README
helm-card-default-values = 默认 Values
helm-readme-source = 源码
helm-readme-source-tooltip = 显示 Markdown 源码
helm-card-needs-helm = 需要 helm CLI
helm-card-needs-helm-description = README 和默认 values 来自 helm show。
helm-card-load-error = 无法加载
helm-card-no-readme = 没有 README
helm-card-no-values = 没有默认 Values

## 安装面板

helm-install-chart-title = 安装 Chart
helm-install-chart-placeholder = oci://ghcr.io/org/charts/name、路径或 URL
helm-install-release-placeholder = Release 名称
helm-install-namespace-placeholder = 已有或新的 namespace
helm-version-latest = 最新
helm-label-version = 版本
helm-label-release = Release
helm-label-namespace = namespace
helm-install-pick-namespace = 选择已有的 namespace
helm-install-create-namespace = 创建 namespace（--create-namespace）
helm-install-namespace-new = 新 namespace { $namespace }：安装时会创建它（--create-namespace）。
helm-install-namespaces-loading = 正在加载 namespace 以确认 { $namespace } 是否存在…
helm-install-namespace-checking = 正在检查 namespace { $namespace } 是否存在…
helm-install-namespace-unverified-forbidden = 无法检查 namespace { $namespace } 是否存在（没有读取权限）：如不存在，安装时会创建它（--create-namespace）。如果它已存在而你无权创建 namespace，请取消勾选。
helm-install-namespace-unverified = 无法检查 namespace { $namespace } 是否存在（检查失败）：如不存在，安装时会创建它（--create-namespace）。如果它已存在而你无权创建 namespace，请取消勾选。
helm-values-user = 用户 values。加载 Chart 默认值以便从其开始。
helm-values-defaults-loading = 正在加载 { $chart } 的默认值…
helm-values-from-defaults = 用户 values，基于 Chart 默认值（helm show values）。
helm-values-edited = Values 已编辑；最初来自 { $chart } 的默认值。
helm-values-defaults-failed = 无法加载 { $chart } 的默认值：{ $error }
helm-values-defaults-button = Chart 默认值
helm-values-defaults-tooltip = 用 Chart 默认值替换 values
helm-values-defaults-loading-title = 正在加载 Chart 默认值…
helm-values-error = Values：{ $error }
helm-install-enter-chart = 请输入 Chart：oci://…、路径或 URL。
helm-install-fix-values = 请修正 values：不是有效的 YAML。
helm-no-preview = helm 未返回预览
helm-preview-interrupted = 预览被中断：{ $error }
helm-preview = 预览
helm-preview-failed = 预览失败
helm-install-failed = 安装失败
helm-install-failed-note = 修改输入后重新安装，或先进行预览。如果 helm 已创建 Release，它会以 failed 状态出现在 Release 列表中。
helm-install-preview-title = 预览安装（可选）
helm-install-preview-description = 使用这些 values 渲染 Chart（helm install --dry-run=server），并显示它将创建的对象。集群不会有任何更改。也可以直接安装。
helm-install-rendering = 正在渲染 Chart…
helm-install-preview-failed-note = 集群未发生任何更改。无需预览也可以安装。
helm-install-preview-stale = 自此次预览后输入已更改。
helm-install-preview-summary = { $name } 位于 { $namespace } · 修订版 { $revision } · Chart { $chart } · { $count } 个对象
helm-install-no-objects = 该 Chart 不渲染任何对象
helm-install-no-objects-description = 只会运行 hook 或说明（如果有）。
helm-install-title = 安装 { $name }？
helm-install-confirm-message =
    { $create ->
        [yes]
            { $previewed ->
                [yes] Helm 将按预览把 { $chart } 安装为 namespace { $namespace }（不存在时创建）中的 Release { $release }。
               *[no] Helm 将把 { $chart } 安装为 namespace { $namespace }（不存在时创建）中的 Release { $release }，未经预览：Chart 未事先在集群上渲染。
            }
       *[no]
            { $previewed ->
                [yes] Helm 将按预览把 { $chart } 安装为 namespace { $namespace } 中的 Release { $release }。
               *[no] Helm 将把 { $chart } 安装为 namespace { $namespace } 中的 Release { $release }，未经预览：Chart 未事先在集群上渲染。
            }
    }
helm-install-confirm = 安装
helm-install-toast = 安装 { $name }
helm-missing-crds =
    { $count ->
       *[other] 该 Chart 会创建 { $kinds } 资源，但集群中没有对应的 CRD。通常需要先以单独的 Chart 安装定义它们的 operator。如果此 Chart 自带 CRD（crds/ 目录），则只有预览会失败：dry run 不会创建 CRD，因此可以不经预览直接安装。
    }

## 升级面板

helm-upgrade-chart-placeholder = repo/chart、oci://…、路径或 URL
helm-version-app = 应用 { $app }
helm-version-prerelease = 预发布
helm-version-current = 当前
helm-upgrade-stored-loading = 正在加载 Release 中保存的 Chart…
helm-upgrade-stored-error = 保存的 Chart：{ $reason }
helm-upgrade-no-repository = 没有仓库包含此 Chart。
helm-upgrade-pick-version = 请选择 Chart 版本。
helm-upgrade-enter-chart = 请输入 Chart：repo/chart、oci://…、路径或 URL。
helm-upgrade-source-stored-loading = Release 中保存的 Chart（加载中…）
helm-upgrade-source-stored = Release 中保存的 Chart：{ $chart }
helm-upgrade-source-stored-unavailable = Release 中保存的 Chart（不可用）
helm-upgrade-source-repository = 来自仓库 { $repository } 的 Chart
helm-upgrade-source-any-repository = 来自仓库的 Chart
helm-upgrade-source-reference = 来自 OCI 仓库、路径或 URL 的 Chart
helm-upgrade-stored-unusable = Release 中保存的 Chart 无法使用：{ $reason }。
helm-upgrade-note-repository = 已部署：{ $chart }。版本来自仓库的缓存索引（helm repo update 会刷新它）；Chart 由 helm 下载。
helm-upgrade-note-reference = 已部署：{ $chart }。你的仓库中都没有此 Chart；helm 会使用你的仓库登录信息解析该引用。
helm-upgrade-preview-title = 预览升级
helm-upgrade-preview-description = 在服务器上使用这些 values 渲染 Chart（helm upgrade --dry-run=server），并显示 manifest 的变化。集群不会有任何更改。
helm-upgrade-rendering = 正在渲染升级…
helm-upgrade-preview-stale = 自此次预览后输入已更改；请重新预览后再升级。
helm-upgrade-preview-summary = 修订版 { $from } → { $to } · Chart { $chart }
helm-upgrade-no-change = Manifest 没有变化
helm-upgrade-no-change-description = Hook、说明和保存的 values 仍可能变化。
helm-upgrade-values-note = 用户 values。它们会在 Chart 默认值之上替换 Release 的 values（--reset-values）。
helm-upgrade-preview-button = 预览更改

## OCI Chart 版本

helm-oci-listing = 正在从仓库列出版本…
helm-oci-list-failed = 无法列出版本：请手动输入
helm-oci-no-versions = 该仓库中没有此 Chart 的版本
helm-oci-pick = 从仓库中选择版本
helm-oci-list-error = 无法列出 Chart 版本：{ $error }。请输入版本。

## 表单校验

helm-invalid-repository-name-empty = 请输入仓库名称。
helm-invalid-repository-name-chars = 仓库名称不能包含「/」或空格。
helm-invalid-release-empty = 请输入 Release 名称。
helm-invalid-release-too-long = Release 名称超过 { $max } 个字符。
helm-invalid-release-chars = Release 名称只能包含小写字母、数字、「-」和「.」，且必须以字母或数字开头和结尾。
helm-invalid-namespace-empty = 请输入 namespace。
helm-invalid-namespace-chars = Namespace 只能包含小写字母、数字和「-」，最多 63 个字符。
helm-invalid-oci-repository = OCI 仓库不能作为 Chart 仓库添加；请按引用安装 oci:// Chart。
helm-invalid-url-scheme = 请输入 http:// 或 https:// URL。
helm-invalid-url = 请输入有效的仓库 URL。
helm-invalid-insecure-credentials = 用户名和密码只会通过 https:// 发送。请使用 https:// URL，或将凭据留空。
helm-invalid-yaml-line = 第 { $line } 行：{ $message }
helm-invalid-values-documents = values 必须是单个 YAML 文档
helm-invalid-values-mapping = 第 { $line } 行：values 必须是映射
helm-invalid-no-chart-metadata = Release 中没有 Chart 元数据
helm-invalid-subcharts = 该 Chart 包含子 Chart，而 Helm 不会将其保存在 Release 中；请从仓库或路径升级

## 下载 helm（Helm 页面上的提示）

helm-tools-downloading-helm = 正在下载 Helm { $version }
helm-tools-download-helm = 下载 Helm { $version }
helm-tools-download-line = 下载 { $line }
helm-tools-settings = 设置…
helm-tools-helm-needed = 更改需要 helm CLI。
helm-tools-download-tooltip = 来自 get.helm.sh 的官方版本，经 Helm 维护者签名和 SHA-256 校验，保存在 Kubach 的数据文件夹中；不会在系统范围内安装任何内容
helm-tools-no-data-dir = 没有可下载到的数据目录。
helm-tools-check-stopped = 检查已停止：{ $error }
helm-tools-download-stopped = 下载已停止：{ $error }
helm-tools-remove-failed = 无法删除已下载的 helm：{ $error }

## 设置 → 外部工具

helm-tools-auto-downloaded = 自动：{ $path }（已下载）
helm-tools-auto = 自动：{ $path }
helm-tools-auto-not-found = 自动：在 PATH 中未找到
helm-tools-helm-binary = helm 程序
helm-tools-helm-binary-description = 用于需要 CLI 的 Helm 操作（ADR 0006）。留空时先在 PATH 中查找，再使用 Kubach 下载的副本。
helm-tools-helm-in-use = 正在使用的 helm
helm-tools-helm-in-use-missing = 未找到：无法升级、回滚和卸载；查看 Release 仍可用。
helm-tools-helm-in-use-description =
    { $source ->
        [settings] { $path } — 上方设置，{ $version }
        [path] { $path } — 在 PATH 中找到，{ $version }
       *[downloaded] { $path } — 由 Kubach 下载，{ $version }
    }
helm-tools-version-unknown = 版本未知
helm-tools-helm-line = 下载的 Helm 系列
helm-tools-helm-line-description = Helm 4 是当前系列；Helm 3 仅获得修复。Kubach 两者都支持，包括在 PATH 中找到的 helm。
helm-tools-chart-icons = 加载 Chart 图标
helm-tools-chart-icons-description = 从 Chart 提供的 URL 下载 Chart 图标和 README 图片，并缓存在设置旁边。关闭后，Kubach 不会发送此类请求（ADR 0006）。
helm-tools-downloaded = 由 Kubach 下载
helm-tools-check-updates = 检查更新
helm-tools-remove = 删除
helm-tools-remove-title = 删除已下载的 Helm？
helm-tools-remove-message = 将删除 Kubach 数据文件夹中的 Helm 副本。之后 Helm 操作会使用 PATH 中的 helm 或上方设置的路径（如有）；可以在此重新下载。
helm-tools-signed-by = 签名者：{ $signer }。
helm-tools-checked-pinned-unsigned = 已按 Kubach 内置的 SHA-256 校验；签名未验证：{ $note }。
helm-tools-checked-pinned = 已按 Kubach 内置的 SHA-256 校验。
helm-tools-checked-sha = 已按 SHA-256 校验。
helm-tools-checking = 正在检查更新…
helm-tools-check-failed = 无法检查更新：{ $error }。
helm-tools-checked-ago = { $age } 前已检查。
helm-tools-latest-unknown = get.helm.sh 未给出最新的 { $line }。
helm-tools-not-checked = 尚未检查更新。
helm-tools-unsupported = 没有适用于此操作系统和 CPU 的 Helm 下载：请自行安装 helm。
helm-tools-no-data-folder = 没有可下载 Helm 的数据文件夹。
helm-tools-absent = 未下载。可用：Helm { $available }。{ $checked } 来自 get.helm.sh 的官方版本，经 Helm 维护者的 OpenPGP 签名和 SHA-256 校验，保存在 Kubach 的数据文件夹中；不会在系统范围内安装任何内容。
helm-tools-current = Helm { $version } 位于 { $path }。{ $verification } 已是最新。{ $checked }
helm-tools-outdated = Helm { $version } 位于 { $path }。{ $verification } 可用：Helm { $available }。{ $checked }
helm-tools-update-to = 更新到 { $version }

## 状态栏中的变更通知 (#459)

helm-notice-install-started = 已开始安装 { $name }
helm-notice-install-done = 已安装 { $name }
helm-notice-install-failed = 安装 { $name } 失败：{ $error }
helm-notice-upgrade-started = 已开始升级 { $name }
helm-notice-upgrade-done = 已升级 { $name }
helm-notice-upgrade-failed = 升级 { $name } 失败：{ $error }
helm-notice-rollback-started = 已开始回滚 { $name }
helm-notice-rollback-done = 已回滚 { $name }
helm-notice-rollback-failed = 回滚 { $name } 失败：{ $error }
helm-notice-uninstall-started = 已开始卸载 { $name }
helm-notice-uninstall-done = 已卸载 { $name }
helm-notice-uninstall-failed = 卸载 { $name } 失败：{ $error }
