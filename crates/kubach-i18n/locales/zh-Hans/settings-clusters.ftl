# 设置 → 集群：“通用”页面（kubeconfig 文件、导入的 kubeconfig）和“按集群”页面
# （图块、namespace、Prometheus、Helm、安全、移除）；设置 → 预设。
# 机器翻译草稿，需要母语者审校。

## 通用：kubeconfig 文件

settings-kubeconfig-loading = 正在加载 kubeconfig…
settings-kubeconfig-not-loaded = 未加载 kubeconfig：{ $error }
settings-kubeconfig-files = kubeconfig 文件
settings-kubeconfig-files-description = 始终读取 KUBECONFIG（或 ~/.kube/config）。关联的文件在原位置读取，并在其后合并。
settings-link-file = 关联文件…
settings-link-file-tooltip = 在原位置读取；凭据保留在你的文件中
settings-import = 导入…
settings-import-tooltip = 复制到加密保险库
settings-startup-context-missing = { $name }（不在 kubeconfig 中）
settings-startup-context = 启动时的 context
settings-startup-context-description = Kubach 启动时打开。
settings-keep-connected = 保持连接的 context 数
settings-keep-connected-description = 最近使用的 context 保持连接，切换回去时立即可用。
settings-find-again = 重新查找…
settings-unlink = 取消关联…
settings-no-contexts = 没有 context
settings-no-contexts-match = 没有匹配的 context
settings-file-contexts = { $count } 个 context：{ $contexts }
settings-restrict-permissions = 限制为 0600

## context 的来源

settings-origin-linked = 关联的文件
settings-origin-imported = 已导入
settings-origin-unknown = 未知
settings-origin-your-kubeconfig = 你的 kubeconfig
settings-badge-linked = 关联
settings-badge-vault = 保险库

## 导入的 kubeconfig（保险库）

settings-group-vault = 导入的 kubeconfig
settings-vault-group-description = 副本加密保存在设置文件夹的 vault.kubach 中。使用保险库密码或系统钥匙串解锁。
settings-vault = 保险库
settings-vault-missing = 尚未创建：第一次导入时创建。
settings-vault-locked = 已锁定。
settings-vault-unlocked = 已解锁：导入的 context 可用。
settings-vault-unavailable = 操作系统没有设置文件夹。
settings-vault-lock-now = 立即锁定
settings-vault-unlock = 解锁…
settings-vault-update = 从文件更新…
settings-vault-rename = 重命名…
settings-vault-export = 导出…
settings-vault-remove = 移除
settings-vault-import-file = 导入文件…
settings-vault-paste = 粘贴
settings-vault-paste-tooltip = 从剪贴板导入 kubeconfig YAML
settings-vault-keychain = 记住在系统钥匙串中
settings-vault-keychain-busy = 记住在系统钥匙串中…
settings-vault-keychain-checking = 正在检查系统钥匙串…
settings-vault-keychain-description = 将保险库密钥保存在钥匙串（macOS）、凭据管理器（Windows）或 Secret Service（Linux）中，因此不再询问密码。
settings-vault-asks-password = 更改此项需要先输入保险库密码。
settings-vault-biometric = 使用 { $method } 解锁
settings-vault-biometric-busy = 使用 { $method } 解锁…
settings-vault-biometric-checking = 正在检查 { $method }…
settings-vault-biometric-description = 用 { $method } 代替密码。密码仍然有效，在取消 { $method } 或指纹变化时会询问密码。
settings-vault-set-password-first = 请先设置保险库密码。
settings-vault-lock-screen = 锁屏时锁定
settings-vault-lock-screen-description = 操作系统锁屏出现时立即锁定保险库。
settings-vault-lock-idle = 闲置后锁定
settings-vault-lock-idle-description = 无输入多少分钟后锁定保险库；0 表示从不锁定。
settings-vault-password = 保险库密码
settings-vault-password-description = 所有内容将用新密码重新加密。
settings-vault-no-password-description = 保险库没有密码，只有系统钥匙串。设置密码后可在任何地方解锁。
settings-vault-encrypting = 正在加密…
settings-vault-change-password = 更改…
settings-vault-set-password = 设置…
settings-vault-reset = 重置保险库
settings-vault-reset-description = 删除保险库和所有导入的 kubeconfig，例如忘记密码或文件损坏后。
settings-vault-deleting = 正在删除…
settings-vault-reset-button = 重置…

## 按集群

settings-group-cluster = 集群
settings-no-contexts-notice = 没有 kubeconfig context。请在“集群：通用”中添加 kubeconfig；其 context 在这里设置。
settings-group-contexts = Context
settings-context-shown-below = 显示在下方
settings-context-open = 打开
settings-context-hidden = 已从快捷栏隐藏
settings-no-api-server = kubeconfig 中没有 API 服务器
settings-source = 来源
settings-location-imported = 以“{ $entry }”加密保存在保险库中。
settings-location-linked = { $path }，在原位置读取。
settings-location-own = { $path }，你的 kubeconfig。
settings-show-in-hotbar = 在快捷栏中显示
settings-show-in-hotbar-description = 隐藏的 context 仍在快捷栏的 … 菜单中。

## 快捷栏图块

settings-group-hotbar-tile = 快捷栏图块
settings-hotbar-tile-description = context 在快捷栏中图块的标签和颜色。
settings-tile-initials-placeholder = 首字母
settings-tile-reset = 恢复默认
settings-tile-preview = 预览
settings-tile-default-color = 默认颜色：{ $color }。
settings-tile-preset-color = 预设：{ $color }。
settings-tile-custom-color-value = 自定义颜色 { $rgb }。
settings-tile-presets = 预设
settings-tile-presets-description = 主题颜色；随深色和浅色主题变化。
settings-tile-custom = 自定义颜色
settings-tile-custom-description = 任意颜色，在两种主题中保持不变。从当前颜色开始。
settings-tile-label = 标签
settings-tile-label-description = 图块上最多 { $max } 个字符；留空显示首字母。也可在图块菜单中使用“重命名…”。

## 默认 namespace、Prometheus、Helm

settings-group-default-namespace = 默认 namespace
settings-default-namespace-description = 每个 context 打开的 namespace。留空则使用 kubeconfig 中的。
settings-prometheus-description = 详情中的用量历史，以及没有 metrics-server 时表格中的 CPU / Memory。“自动”在集群中搜索一次并记住结果；“重新检测”会重新搜索。地址为 http(s)://host:port 或 namespace/service:port（通过 API 服务器访问）。
settings-prometheus-placeholder = http(s)://host:port 或 namespace/service:port
settings-prometheus-auto = 自动
settings-prometheus-unusual = { $address }（不常见的 namespace）
settings-prometheus-address = 地址
settings-prometheus-off = 关闭
settings-prometheus-searching = 正在搜索集群…
settings-prometheus-search-failed = 搜索失败：{ $error }
settings-prometheus-not-searched = 尚未搜索
settings-prometheus-searched-none = { $age } 前搜索过：未找到 Prometheus
settings-prometheus-searched-found = { $age } 前搜索过：{ $found }
settings-prometheus-status-off = 关闭：不搜索，不发请求
settings-prometheus-status-ready = 已连接到 { $endpoint }
settings-prometheus-status-unreachable = { $endpoint } 没有响应
settings-prometheus-status-connecting = 使用 { $endpoint }
settings-prometheus-status-no-address = 请输入地址
settings-prometheus-status-invalid = 地址无效：{ $address }
settings-prometheus-status-unconfirmed = 等待确认 { $endpoint }
settings-prometheus-unusual-warning = 仅在 namespace { $namespace } 中找到，而 Prometheus 通常不安装在那里。任何能在那里创建 Service 的人都可能显示伪造的用量。仅在信任时使用。
settings-prometheus-use-it = 使用
settings-prometheus-use-it-tooltip = 为 { $name } 使用 { $endpoint }
settings-prometheus-source-label = { $name } 的 Prometheus：{ $label }
settings-prometheus-detect-again = 重新检测
settings-prometheus-detect-tooltip = 在集群中搜索 Prometheus 服务器
settings-group-helm-driver = Helm 存储驱动
settings-helm-driver-description = Helm 保存 release 的位置（HELM_DRIVER）。“检测”使用已有 release 的存储，否则使用 Secret。
settings-helm-driver-detect = 检测

## 移除 context

settings-group-remove = 移除
settings-remove-from-kubach = 从 Kubach 中移除
settings-remove-from-kubach-description = 从保险库中删除导入的副本。其 context 将从 Kubach 中消失：{ $contexts }。
settings-remove-from-kubach-button = 从 Kubach 中移除…
settings-unlink-file = 取消关联文件
settings-unlink-file-button = 取消关联文件…
settings-unlink-description-single = Kubach 将停止读取 { $path }。文件不会被修改。
settings-unlink-description = Kubach 将停止读取 { $path }，因此其全部 { $count } 个 context 将消失：{ $contexts }。文件不会被修改。
settings-hide = 隐藏
settings-hide-button = 隐藏…
settings-hidden = 已隐藏
settings-hide-description = { $name } 来自 { $origin }，Kubach 不会修改它。请在那里用 kubectl config delete-context 删除，或从快捷栏隐藏。
settings-hide-title = 隐藏 { $name }？
settings-hide-message = 图块将离开快捷栏；kubeconfig 不会被修改。可以从快捷栏的 … 菜单或这里重新显示。

## context 的安全设置

settings-profile-read-only = 只读
settings-profile-debug = 调试
settings-profile-deploy = 部署
settings-deploy-dry-run = 仅试运行
settings-deploy-apply = 批准后应用
settings-exec-never = 从不
settings-exec-single = 单条命令
settings-exec-shell = 单条命令和 shell
settings-namespaces-one = 一个
settings-namespaces-several = 多个
settings-agent-secrets-none = 无
settings-agent-secrets-metadata = 元数据和检查
settings-agent-secrets-reveal = 元数据、检查和按请求显示
settings-security-preset = 预设
settings-security-preset-strict = 严格：默认值。
settings-security-preset-development = 开发：全部允许，每次更改需确认，显示 Secret 值并按请求向代理显示。
settings-security-preset-custom = 自定义值。
settings-preset-strict = 严格
settings-preset-development = 开发
settings-group-security = 安全
settings-security-description-no-server = 通过 MCP 连接的 AI 代理可以做什么。新的 context 默认严格。kubeconfig 中没有 API 服务器。
settings-security-description = 通过 MCP 连接的 AI 代理可以做什么。新的 context 默认严格。设置绑定到此名称和服务器 { $server }。
settings-security-loosened = { $name } 比默认值宽松：{ $list }。
settings-security-relaxed = 安全已放宽
settings-security-suggest-development = 这看起来是本地集群。建议使用“开发”预设；不会自动应用。
settings-group-secrets = Secret
settings-hide-secret-values = 隐藏 Secret 值
settings-hide-secret-values-description = 此 context 的 Secret 没有“显示”“复制”或“加载值”，Helm 和 Kustomize 中也不显示。仍可在不加载的情况下替换值。
settings-group-agent-mcp = AI 代理（MCP）
settings-agent-mcp-description = 代理开始时没有任何权限。它指定一个 namespace，Kubach 会在它第一次读取、显示 Secret 或更改内容时询问你。这些设置是它在这里最多能获得的权限。
settings-mcp-enabled = AI 代理访问
settings-mcp-enabled-description = 关闭：此集群上不会启动任何代理会话。
settings-ceiling-raised = 应代理请求，于 { $time } 由会话 { $session } 提升：{ $changes }。
settings-ceiling-raised-client = 应代理请求，于 { $time } 由会话 { $session }（{ $client }）提升：{ $changes }。
settings-ceiling-raised-earlier = （设置文件中还有 { $count } 次更早的提升）
settings-ceiling-raised-title = 已从会话提升上限
settings-protected = 受保护的集群
settings-protected-description = 代理不能从会话中请求提升以下限制：此类调用会被拒绝并附说明，不会询问你。只有此设置能关闭它。适用于生产环境。
settings-max-profile = 代理最多可请求
settings-max-profile-description = 只读：查看资源。调试：另加日志和 exec。部署：另加更改。超过此级别时，代理可以在 Kubach 中请你提升（一次会话或永久），除非集群受保护。
settings-deploy-mode = 更改
settings-deploy-mode-description = 需要“部署”。试运行：代理看到 diff，什么都不变。应用：每次更改在你批准后执行，包括你在 Kubach 中输入值的 Secret。
settings-exec-mode = 容器中的命令
settings-exec-mode-description = 需要“调试”或“部署”。每条命令都由你批准。
settings-agent-secrets = 代理可见的 Secret
settings-agent-secrets-hidden-description = 仅在 Kubach 内显示名称、键、大小和检查，从不显示值。要显示值需关闭“隐藏 Secret 值”：代理永远不会得到你不能看到的值。
settings-agent-secrets-description = 显示时会针对每个 Secret 询问你，并提醒其值将发送给模型提供商。其他工具始终隐藏值。
settings-agent-other-kinds = 自定义资源（CRD）
settings-agent-other-kinds-description = 关闭：仅内置 kind，如 Pod、Deployment、Service、ConfigMap、Job 和 Ingress。开启：也包括自定义资源，它们可能包含凭据，或使控制器（Flux、Argo CD）以自身权限执行操作。Secret、ServiceAccount 和 RBAC 仍不可用。
settings-namespaces = 每个会话的 namespace
settings-namespaces-description = 一个：你允许的第一个 namespace 是会话唯一能得到的。多个：每个新的 namespace 都会单独询问。
settings-session-ttl = 会话时长
settings-session-ttl-description = 会话及其中所有权限在此时间后结束，最多 8 小时，或在 Kubach 退出时结束。`kubach mcp` 会话持续 30 分钟，除非以更长的 --ttl 启动。
settings-group-enforcement = 由集群强制执行
settings-enforcement-description = Kubach 自己检查代理的每次调用。这些设置增加 API 服务器的第二道检查，这样 Kubach 中的错误也不会让代理获得超出你允许的权限。
settings-temporary-objects = 为代理使用独立身份
settings-temporary-objects-description = 在你允许代理读取的每个 namespace 中，Kubach 创建一个 ServiceAccount，其 Role 恰好包含你允许的内容，并在会话结束时删除。需要该 namespace 的管理员权限。
settings-require-server-enforced = 必须启用
settings-require-server-enforced-description = 拒绝集群无法强制执行限制的 namespace。关闭：此类 namespace 标记为“仅在 Kubach 中限制”，其中每次更改都需逐一批准。

## 预设

settings-preset-name-placeholder = 预设名称
settings-presets-none = 还没有预设
settings-presets-none-description = 在 YAML 编辑器中打开清单并使用“另存为预设”。预设可在资源页面通过“+”使用，按 kind 分组。
settings-presets-kind-description = { $api_version }。默认预设是在 { $kind } 页面上“+”打开的内容。
settings-preset-default = 默认
settings-preset-delete = 删除预设
settings-preset-lines = { $lines } 行 YAML。
settings-preset-lines-default = { $lines } 行 YAML。在 { $kind } 页面上由“+”打开。
