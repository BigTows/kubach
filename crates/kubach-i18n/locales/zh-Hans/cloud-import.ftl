vault-add-cloud = 从云导入…
vault-add-cloud-text = Google GKE、Yandex Cloud 或 AWS EKS：选择云服务商 CLI 可见的集群。Kubach 不保存云令牌，上下文通过 CLI 登录。存储在加密保管库中。
cloud-title = 从云导入
cloud-refresh = 刷新
cloud-cli-cannot-download = Kubach 无法为此系统下载 { $tool }。请按 { $url } 安装，然后点击“刷新”。
cloud-cli-missing = 在 PATH 中未找到 { $tool }。
cloud-cli-download = 下载 { $tool }
cloud-cli-download-tooltip = 厂商的官方版本，在厂商发布校验和或签名时进行校验，下载到 Kubach 的数据文件夹。不会在系统中安装任何内容。
cloud-cli-in-path = 使用 { $path }（来自 PATH）
cloud-cli-downloaded = 使用 { $path }（由 Kubach 下载）
cloud-gke-plugin-missing = 未安装 gke-gcloud-auth-plugin，GKE 上下文需要它来登录。安装命令：gcloud components install gke-gcloud-auth-plugin
cloud-gke-plugin-install = 安装插件
cloud-gke-plugin-installing = 正在安装插件…
cloud-gke-plugin-failed = 未能安装 gke-gcloud-auth-plugin：{ $error }
cloud-gke-plugin-required = 请先安装 gke-gcloud-auth-plugin（Google GKE 标签页）：没有它 GKE 上下文无法登录。
cloud-gke-python-hint = 在 macOS 上 gcloud 需要 Python 3.10 或更高版本：请安装（python.org 或 brew install python）或通过 CLOUDSDK_PYTHON 指定，然后重试。
cloud-not-logged-in = { $tool } 尚未登录。请在终端中登录，然后点击“重试”：
cloud-error = { $tool } 出错：{ $error }
cloud-try-again = 重试
cloud-loading = 正在询问 { $tool }…
cloud-scopes-gke = 项目
cloud-scopes-yandex = 文件夹
cloud-scopes-eks = 区域
cloud-profile = 配置文件
cloud-account = 已登录为 { $account }
cloud-no-scopes = 此账号下未找到任何内容。
cloud-find-clusters = 查找集群
cloud-finding = 正在查找集群…
cloud-endpoint-external = 外部
cloud-endpoint-internal = 内部
cloud-endpoint-dns = DNS
cloud-clusters =
    { $count ->
       *[other] { $count } 个集群
    }
cloud-no-clusters = 所选位置中没有集群。
cloud-scope-failed = { $scope }：{ $error }
cloud-note = 每次 Kubach 连接时，上下文都会运行 { $tool } 登录；Kubach 不保存云令牌。
cloud-selected =
    { $count ->
       *[other] 已选择 { $count } 个集群
    }
cloud-next = 下一步
cloud-none-selected = 请至少勾选一个集群。
cloud-tools-group = 云 CLI
cloud-tools-downloading = 正在下载 { $tool }…
cloud-tools-in-path = 正在使用：{ $path }，来自 PATH，{ $version }。
cloud-tools-in-use-downloaded = 正在使用：{ $path }，由 Kubach 下载，{ $version }。
cloud-tools-missing = 未找到。“从云导入”需要它。
cloud-tools-downloaded = 已下载的副本：{ $version }。{ $verification }
cloud-tools-unsupported = Kubach 无法为此系统下载 { $tool }：请按 { $url } 安装。
cloud-tools-download = 下载
cloud-tools-update = 更新
cloud-tools-remove-title = 删除已下载的 { $tool }？
cloud-tools-remove-message = 删除 Kubach 数据文件夹中的副本。在重新安装或下载 { $tool } 之前，用它导入的上下文将无法登录。
cloud-tools-checked-published = 已按 { $source } 上发布的 SHA-256 校验。
cloud-tools-package-signed = 软件包签名者：{ $signer }。
cloud-tools-unchecked = 未校验：厂商不发布校验和或签名；已通过 HTTPS 从厂商服务器下载。
