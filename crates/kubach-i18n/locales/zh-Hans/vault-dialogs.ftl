# 导入的 kubeconfig 保管库：添加集群、导入预览、解锁、修改密码、重置、
# 移除、重命名、导出、“手动添加…”表单以及 kubach-core 的保管库错误。
# 名称、路径和操作系统给出的原因是数据。机器翻译草稿，需母语者校对。

## 添加集群

vault-choose = 选择
vault-link-text = 原地读取：aws、gcloud 等工具的更新会被自动获取。凭据保留在你的文件中。
vault-import-text = 复制到 Kubach 的加密保管库，用密码或系统钥匙串解锁。适合那些原本会随处散落的文件。
vault-add-title = 添加集群
vault-add-link = 链接 kubeconfig 文件…
vault-add-import = 导入 kubeconfig 文件…
vault-add-paste = 从剪贴板粘贴 kubeconfig
vault-add-paste-text = 与导入相同，来源是你复制的 YAML（例如来自云控制台）。
vault-add-manual = 手动添加…
vault-add-manual-text = 输入 API 服务器和凭据（令牌、客户端证书或 exec 插件），或编写 kubeconfig YAML。存储在加密保管库中。
vault-dropped-title =
    { $count ->
       *[other] 添加 kubeconfig 文件
    }
vault-dropped-link =
    { $count ->
       *[other] 链接文件
    }
vault-dropped-import =
    { $count ->
       *[other] 导入文件
    }
vault-dropped-import-several = 每个文件都会作为单独条目复制到 Kubach 的加密保管库，并依次逐个预览。
vault-clipboard-empty = 剪贴板中没有文本。请先复制 kubeconfig YAML。

## 导入预览

vault-import-title = 导入 kubeconfig
vault-update-title = 更新已导入的 kubeconfig
vault-pasted-name = 粘贴的 kubeconfig
vault-name = 名称
vault-password = 保管库密码
vault-password-repeat = 再次输入密码
vault-context-name = 上下文名称
vault-confirm-read-files = 请先确认读取本地文件
vault-no-contexts = kubeconfig 中没有上下文
vault-fix-names = 请先修正上下文名称
vault-enter-name = 请输入名称
vault-choose-password = 请为保管库设置密码
vault-passwords-differ = 两次输入的密码不一致
vault-no-settings-folder = 操作系统没有可供保管库使用的设置文件夹
vault-imported = 加密保管库中有 { $count } 个上下文
vault-renamed-taken = 已重命名：“{ $original }”已被使用
vault-renamed = 由“{ $original }”重命名
vault-adds = 将添加 { $contexts } 个上下文、{ $clusters } 个集群和 { $users } 个用户。
vault-new-note = 导入的 kubeconfig 以加密方式存储。请设置保管库密码；Kubach 每个会话只询问一次。
vault-remember = 记住在系统钥匙串中（无需密码）
vault-locked-biometric = 保管库已锁定。请输入密码，或留空以使用 { $method }。
vault-locked-keychain = 保管库已锁定。请输入密码，或留空以使用系统钥匙串。
vault-locked-password = 保管库已锁定。请输入密码以添加此配置。
vault-locked-keychain-only = 保管库已锁定；它通过系统钥匙串解锁。
vault-broken = { $error } 请在 设置 → Clusters 中重置保管库后再导入。
vault-source-manual = 手动输入
vault-source-clipboard = 来自剪贴板
vault-source-queue = 第 { $current } 个文件，共 { $total } 个 · { $source }
vault-read-files = 将这些文件读入保管库
vault-back = 返回
vault-cancel = 取消
vault-skip = 跳过
vault-skip-tooltip = 不导入此文件，转到下一个
vault-encrypting = 正在加密…
vault-update = 更新
vault-import = 导入
vault-import-unreadable-file = 无法读取 { $path }（{ $error }）；导入的配置将继续引用它。
vault-name-empty = 请输入上下文名称
vault-name-duplicate = 此次导入中已有同名上下文
vault-name-taken = 已存在同名上下文

## 解锁

vault-unlock-title = 解锁导入的集群
vault-unlock-text = 导入的 kubeconfig 已加密。解锁后它们会保存在内存中，直到你锁定保管库、长时间空闲或退出。
vault-unlock-waiting = 请通过 { $method } 确认…
vault-use-password = 使用密码
vault-use-biometric = 使用 { $method }
vault-use-keychain = 使用系统钥匙串
vault-unlocking = 正在解锁…
vault-unlock = 解锁
vault-enter-password = 请输入密码
vault-busy = 保管库正忙；请稍后再试
vault-wrong-password-wait = 密码错误。请在 { $seconds } 秒后重试。
vault-too-many-attempts = 尝试次数过多。请在 { $seconds } 秒后重试。
vault-missing = 尚无保管库
vault-closing = Kubach 正在关闭
vault-locked = 保管库已锁定

## 密码、重置、移除、重命名、导出

vault-password-title = 修改保管库密码
vault-new-password = 新密码
vault-new-password-repeat = 再次输入新密码
vault-enter-new-password = 请输入密码
vault-password-changed = 保管库密码已修改
vault-password-text = 保管库中的所有内容将使用新密钥重新加密。
vault-change = 修改
vault-reset-title = 重置保管库？
vault-reset-description = 加密保管库及其 { $count } 个导入的 kubeconfig 将被删除，它们的集群将从 Kubach 中消失；同时删除损坏的保管库文件副本（vault.kubach.corrupt-…）以及系统钥匙串中的密钥。此操作无法撤销。
vault-reset-ok = 删除保管库
vault-reset-keys-left = 保管库已删除，但其密钥可能仍留在系统钥匙串中：保管库文件无法读取，或钥匙串拒绝删除。请在钥匙串（钥匙串访问、凭据管理器、密码和密钥）中搜索“{ $keychain }”，并删除“{ $keychain }”和“{ $biometric }-…”条目。
vault-remove-title = 移除 { $name }？
vault-remove-description = 其上下文（{ $contexts }）将从 Kubach 中消失。保管库中没有其他副本。
vault-remove = 移除
vault-rename-title = 重命名导入的 kubeconfig
vault-export-title = 导出为普通 kubeconfig？
vault-export-description = 导出的文件未加密：任何能读取它的人都会获得凭据。Kubach 会将其设为仅你可读（0600）；请勿放入备份或共享文件夹。
vault-export-ok = 导出…
vault-export-locked = 保管库已被锁定；请解锁后重新导出。
vault-exported = 已导出到 { $path }
vault-export-failed = 无法写入 { $path }：{ $error }

## 保管库错误（kubach-core）

vault-error-wrong-password = 密码错误
vault-error-corrupted = 保管库文件已损坏（{ $reason }）。
vault-error-corrupted-backup = 保管库文件已损坏（{ $reason }）。副本已保存到 { $path }。
vault-error-newer-format = 此保管库由更新版本的 Kubach 写入（格式 { $version }）；请更新 Kubach 后再打开
vault-error-no-password = 此保管库没有密码；请用系统钥匙串解锁
vault-error-no-keychain = 此保管库未存储在系统钥匙串中
vault-error-keychain-missing = 系统钥匙串中没有保管库密钥；请用密码解锁
vault-error-keychain-mismatch = 系统钥匙串中的密钥无法打开此保管库；请用密码解锁
vault-error-no-key = 保管库需要密码、系统钥匙串或两者兼有
vault-error-no-biometric = 此保管库未开启 { $method } 解锁
vault-error-biometric-needs-password = { $method } 解锁需要一个备用的保管库密码；请先设置密码
vault-error-biometric-mismatch = { $method } 密钥无法打开此保管库；请用密码解锁
vault-error-biometric-cancelled = { $method } 已取消
vault-error-biometric-invalidated = 自保管库设置以来 { $method } 已发生变化（指纹被添加或删除），其密钥已失效。请输入密码，然后在设置中重新开启 { $method } 解锁。
vault-error-biometric-failed = { $method } 失败：{ $reason }
vault-error-text = { $text }
vault-error-empty-password = 密码为空
vault-error-exists = 保管库已存在：{ $path }
vault-error-unknown-entry = 没有 id 为 { $id } 的导入配置
vault-error-serialize = 无法存储配置：{ $error }
vault-error-context-names = 上下文名称必须非空、唯一，且不能被其他上下文使用
vault-error-unread-files = 请先确认读取配置引用的本地文件
vault-error-io = 无法访问保管库文件：{ $error }

## “手动添加…”

manual-title = 手动添加集群
manual-auth-token = 令牌
manual-auth-cert = 客户端证书
manual-auth-basic = 密码
manual-auth-oidc = OIDC
manual-auth-exec = Exec 插件
manual-context-placeholder = 例如 prod-eu
manual-token-placeholder = Bearer 令牌
manual-password-placeholder = 密码
manual-client-secret-placeholder = 仅用于机密客户端
manual-region-placeholder = eu-west-1（默认：profile 的区域）
manual-yaml-not-shown = { $why }。表单保留自己的值；YAML 标签页仍保留你的内容。测试和保存的是当前打开的标签页。
manual-write-yaml = 请先编写 kubeconfig YAML
manual-connecting = 正在连接 { $server }…
manual-connected = 已连接：Kubernetes { $version }
manual-test-stopped = 测试意外中止
manual-load-file = 从文件加载…
manual-ca-custom = CA 证书
manual-ca-system = 系统信任存储
manual-ca-insecure = 跳过验证
manual-ca-pem = CA 证书（PEM）
manual-ca-system-note = 适用于证书由公共证书颁发机构签发的服务器（大多数位于负载均衡器后的托管集群没有这种证书）。
manual-insecure-warning = 不会检查服务器证书：你与集群之间的任何人都可以冒充它并读取凭据。仅用于本地测试集群。
manual-token = 令牌
manual-cert-pem = 客户端证书（PEM）
manual-key-pem = 客户端密钥（PEM）
manual-username = 用户名
manual-password = 密码
manual-basic-note = HTTP basic auth。Kubernetes 1.19 已从 API 服务器中移除它；仅适用于旧集群或位于认证代理之后的集群。
manual-issuer = Issuer URL
manual-client-id = Client ID
manual-client-secret = Client secret（可选）
manual-scopes = 额外的 scopes（可选，以空格分隔）
manual-oidc-install = 安装 kubelogin：kubectl krew install oidc-login（或 brew install int128/kubelogin/kubelogin）。
manual-oidc-note = 以 exec 插件方式运行 kubelogin（kubectl oidc-login get-token）：它会打开浏览器登录，然后缓存并刷新令牌。
manual-context = 上下文名称
manual-server = API 服务器 URL
manual-namespace = 默认 namespace（可选）
manual-server-certificate = 服务器证书
manual-sign-in = 登录方式
manual-missing =
    { $count ->
       *[other] 在 PATH 中找不到 { $names }。{ $hint }
    }
manual-missing-shell =
    { $count ->
       *[other] 在 PATH 中找不到 { $names }。{ $hint } 登录 shell 的环境未能加载（{ $error }），因此 PATH 可能不完整。
    }
manual-template-custom = 自定义
manual-command = 命令
manual-args = 参数
manual-env = 环境变量（NAME=value，以空格分隔）
manual-api-version = API 版本
manual-eks-cluster = EKS 集群名称
manual-region = 区域（可选）
manual-profile = AWS profile（可选）
manual-gke-note = 使用你的 gcloud 登录（gcloud auth login）。API 服务器 URL 和 CA 证书可在 gcloud container clusters describe <cluster> 中找到。
manual-server-id = Server ID
manual-aks-note = 通过 Azure CLI 登录（az login）。对于所有使用 AKS 托管 Entra ID 的集群，Server ID 都相同。
manual-runs = 运行：{ $line }
manual-install-custom = 请安装它，或输入完整路径。
manual-install-eks = 安装 AWS CLI v2 并登录（aws configure / aws sso login）。
manual-install-gke = 安装方法：gcloud components install gke-gcloud-auth-plugin，然后 gcloud auth login。
manual-install-aks = 安装 kubelogin（az aks install-cli）并通过 az login 登录。
manual-install-yc = 安装 Yandex Cloud CLI 并通过 yc init 登录。
manual-yc-note = 通过 yc CLI 登录（yc init）；配置文件留空则使用当前活动配置。API 服务器 URL 和 CA 证书可在 yc managed-kubernetes cluster get <cluster> 中找到。
manual-exec-note = Kubach 连接时会在此计算机上运行该命令，使用你登录 shell 的 PATH。
manual-template = 模板
manual-yaml-note = 完整的 kubeconfig；证书和密钥内联（*-data）。可以有多个上下文；测试使用 current-context。其中的 exec 插件会在测试时运行。
manual-tab-form = 表单
manual-testing = 正在测试…
manual-test = 测试连接
manual-next = 检查并保存…
manual-file-unreadable = 无法读取 { $name }：{ $error }
manual-file-too-large = { $name } 太大，不像证书或密钥
manual-file-not-pem = { $name } 不是 PEM 文本文件（不支持 DER 证书）

## 表单问题（kubach-core）

manual-problem-empty-context = 请输入上下文名称
manual-problem-empty-server = 请输入 API 服务器 URL
manual-problem-invalid-server = 无效的服务器 URL：{ $error }
manual-problem-empty-ca = 请粘贴 CA 证书（PEM）或选择其文件
manual-problem-invalid-ca = CA 不是 PEM 证书（-----BEGIN CERTIFICATE-----）
manual-problem-empty-token = 请输入令牌
manual-problem-invalid-cert = 客户端证书不是 PEM（-----BEGIN CERTIFICATE-----）
manual-problem-invalid-key = 客户端密钥不是 PEM 私钥（-----BEGIN … PRIVATE KEY-----）
manual-problem-empty-username = 请输入用户名
manual-problem-empty-password = 请输入密码
manual-problem-empty-issuer = 请输入 issuer URL
manual-problem-invalid-issuer = 无效的 issuer URL：{ $error }
manual-problem-empty-client-id = 请输入 Client ID
manual-problem-empty-cluster-name = 请输入集群名称
manual-problem-empty-server-id = 请输入 Server ID
manual-problem-empty-command = 请输入插件命令
manual-problem-trailing-backslash = 末尾的反斜杠没有可转义的内容
manual-problem-unclosed-quote = 引号 { $quote } 未闭合
manual-problem-invalid-env = 环境变量应为 NAME=value：“{ $entry }”
manual-yaml-invalid = 不是有效的 kubeconfig：{ $error }
manual-yaml-contexts = 表单只显示一个上下文；此配置有 { $count } 个
manual-yaml-missing-cluster = 配置中没有该上下文的集群
manual-yaml-unsupported = 配置使用了 { $feature }，表单无法显示
manual-feature-ca-file = CA 文件路径
manual-feature-proxy = 代理
manual-feature-tls-server-name = tls-server-name
manual-feature-basic-with-others = 与其他凭据并用的 basic auth
manual-feature-auth-provider = auth provider
manual-feature-credential-files = 凭据文件路径
manual-feature-impersonation = impersonation
