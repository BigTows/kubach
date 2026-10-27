# 编辑器：YAML 编辑器（审查与应用）、ConfigMap 键视图、Secret 编辑器与值、
# 新资源的预设与模板、“新建 namespace”对话框。kind、名称、上下文、字段路径
# 和集群消息属于数据，保持原样。草稿，需母语者校对。

## 通用

editor-loading = 正在加载…
editor-reload = 重新加载
editor-review = 审查更改
editor-apply = 应用
editor-create = 创建
editor-creating = 正在创建…
editor-save = 保存
editor-format = 格式化
editor-format-tooltip = 缩进 JSON
editor-no-keys = 没有键
editor-add-key = 添加键
editor-remove-key = 删除此键
editor-key-placeholder = 键
editor-value-placeholder = 值
editor-key-new = 新增
editor-key-changed = 已更改
editor-key-removed = 已删除
editor-key-unchanged = 未更改
editor-key-renamed = 由 { $key } 重命名
editor-key-twice = 键 { $key } 重复使用
editor-content-properties = Properties
editor-content-text = 文本
editor-content-binary = 二进制
editor-value-warning = { $problem }。仍可保存：该值是纯文本。
editor-size-bytes = { $count } 字节
editor-binary-data = 二进制数据，{ $size }
editor-file-unreadable = 无法读取 { $name }：{ $error }
editor-file-too-large-configmap = { $name } 超过 1 MiB，即整个 ConfigMap 的上限
editor-file-too-large-secret = { $name } 超过 1 MiB，即整个 Secret 的上限
editor-field-name = 名称
editor-field-type = 类型
editor-field-keys = 键
editor-field-user = 用户
editor-field-password = 密码
editor-field-email = 邮箱
editor-field-server = 服务器
editor-field-certificate = 证书
editor-field-private-key = 私钥

## YAML 编辑器：资源按钮

editor-edit-yaml = 编辑 YAML
editor-edit-values = 编辑值
editor-edit-values-tooltip = 编辑键和值；只发送更改过的键
editor-secret-immutable-tooltip = 此 Secret 不可变：无法更改其数据
editor-secret-not-yaml = Secret 在 Secret 编辑器中编辑，而不是作为 YAML
editor-kind-not-patchable = 此 kind 无法 patch
editor-new-secret = 新建 Secret
editor-new-secret-tooltip = 通过表单创建 Opaque、docker registry 或 TLS Secret

## YAML 编辑器：标题与状态

editor-title-new = 新资源
editor-tab-new = 新资源
editor-tab-new-kind = 新建 { $kind }
editor-placeholder-more = 更多文档…
editor-subtitle-edit = 编辑 · { $context }
editor-subtitle-create = 创建 · { $context } · 默认 namespace { $namespace }
editor-checking = 正在检查…
editor-errors-warnings = { $errors } 个错误，{ $warnings } 个警告
editor-warnings = { $count } 个警告
editor-loading-schema = 正在加载 schema…
editor-schema-unavailable = schema 不可用：{ $error }
editor-empty = 空
editor-no-problems = 没有问题
editor-line = 第 { $line } 行

## YAML 编辑器：工具栏

editor-mode-keys = 键
editor-mode-keys-tooltip = 每个键单独编辑，检查 JSON
editor-mode-yaml-tooltip = 以 YAML 显示整个对象
editor-reload-tooltip = 放弃编辑并加载当前版本
editor-save-preset = 另存为预设
editor-save-preset-tooltip = 将此 YAML 保存为该 kind 新资源的预设
editor-review-tooltip = 服务器端试运行，然后用 diff 确认（⌘S）
editor-force-apply = 强制应用
editor-back = 返回编辑器
editor-review-force = 强制审查…
editor-review-force-tooltip = 再次试运行，接管冲突的字段

## YAML 编辑器：提示

editor-disconnected = { $context } 已断开；请重新连接后再审查或应用。
editor-load-failed = 加载对象失败：{ $error }
editor-discovery-pending = 打开此编辑器时 API discovery 尚未完成；请重新打开后再应用。
editor-nothing-to-apply = 没有可应用的内容。
editor-some-not-applied = 部分对象未应用；详见下方。
editor-applied = 已应用：{ $objects }
editor-applied-created = { $label } 已创建
editor-applied-unchanged = { $label } 未更改
editor-applied-configured = { $label } 已配置
editor-context-closed = 已与 { $context } 断开。此标签页属于 { $context }；请重新连接（快捷栏或侧边栏）后再审查或应用。编辑内容已保留。
editor-context-foreign = 此标签页属于 { $context }。审查和应用都发往 { $context }，而不是侧边栏中选中的集群。

## YAML 编辑器：文本检查

editor-resolve-error = 文档 { $document }，第 { $line } 行：{ $message }
editor-missing-field = 缺少 `{ $field }`
editor-unknown-kind = 集群未在 { $version } 中提供 { $kind }
editor-missing-name = 缺少 `metadata.name`（apply 不支持 generateName）
editor-cluster-scoped = { $kind } 是集群范围的，但设置了 namespace “{ $namespace }”
editor-problem-conflict = { $field }：冲突，由 “{ $manager }” 管理
editor-field-required = { $schema_type }（必填）
editor-one-of = 可选值：{ $values }

## YAML 编辑器：审查

editor-value-saved-anyway = { $warning }。仍会以文本保存。
editor-force-warning = 强制应用会从当前管理者手中接管冲突的字段。管理这些字段的控制器、Helm 或 GitOps 工具可能会撤销此更改或持续覆盖它。
editor-no-changes = 没有更改
editor-no-changes-description = 集群已与此清单一致。
editor-not-applied = 未应用
editor-entry-create = 创建
editor-entry-no-changes = 没有更改
editor-entry-changes = { $count } 处更改
editor-entry-failed = 失败
editor-retained = 已删除但应用后仍存在：{ $fields }。服务器端 apply 只删除仅由 Kubach 管理的字段；这些字段还由其他字段管理者管理（或由服务器设置了默认值）。
editor-reload-stale = 重新加载（放弃编辑）
editor-failure-conflicts = { $label }：{ $count } 个字段由其他字段管理者管理
editor-failure-stale = { $label } 自加载后已在集群中更改（resourceVersion 不匹配）。请重新加载以编辑当前版本。

## 编辑器标签页

editor-close-title = 关闭标签页并放弃编辑？
editor-close-detail = { $tab } 有尚未应用的编辑。
editor-discard-edits = 放弃编辑

## ConfigMap 键

editor-keys-unavailable = 无法按键编辑：{ $error }。请在 YAML 中修复。
editor-configmap-immutable = 此 ConfigMap 不可变（immutable: true）：Kubernetes 拒绝更改其数据。如需更改，请删除后重新创建。
editor-configmap-hint = 每个键是一个块：会检查 JSON 和 YAML 值，JSON 可以格式化。未通过检查的值仍以文本保存。审查会在应用前显示服务器的试运行结果。
editor-binary-from-file = { $size }，来自文件 { $file }
editor-binary-not-text = { $size }；二进制值无法以文本编辑
editor-replace-from-file = 从文件替换

## 模板与预设

editor-new = 新建
editor-new-tooltip = 从 YAML 新建资源
editor-new-kind-tooltip = 按模板新建 { $kind }
editor-templates-tooltip = 模板和预设
editor-menu-empty = 空白
editor-menu-builtin = { $label }（内置）
editor-menu-skeleton = 根据 schema 生成骨架
editor-menu-default = { $name }（默认）
editor-preset-suggested = 我的 { $kind }
editor-preset-placeholder = 预设名称
editor-preset-hint = 名称和 namespace 将变为 { $name } 和 { $namespace }。
editor-preset-name-first = 请先为预设命名。
editor-preset-saved = 已保存 { $kind } 的预设“{ $name }”。它会出现在资源页面的“+”菜单中；可在 设置 → 预设 中重命名或设为默认。
editor-preset-secret = Secret 不会保存为预设：设置以明文存储。
editor-preset-no-kind = 无法保存预设：第一个文档需要 apiVersion 和 kind。

## Secret 编辑器

editor-secret-new-in = 在 { $namespace } 中新建 Secret
editor-secret-kind-docker = Docker registry
editor-secret-load-failed = 加载 Secret 失败：{ $error }
editor-secret-new-key-placeholder = 新键
editor-secret-key-gone-reload = 键 { $key } 已不在该 Secret 中；请重新加载。
editor-secret-immutable-error = 此 Secret 不可变；无法更改其数据。
editor-secret-not-loaded = Secret 尚未加载。
editor-no-changes-to-apply = 没有要应用的更改。
editor-secret-choose-cert = 请选择证书文件
editor-secret-choose-key = 请选择私钥文件
editor-secret-created = Secret 已创建。
editor-changes-applied = 更改已应用。
editor-subtitle-edit-values = 编辑值 · { $context }
editor-subtitle-create-secret = 创建 · { $context }
editor-secret-reload-tooltip = 放弃编辑并重新加载键
editor-back-to-editing = 返回编辑
editor-secret-hidden = 此上下文的安全设置隐藏了 Secret 值。
editor-secret-hidden-load = { editor-secret-hidden } 无法加载当前值；仍可在不加载的情况下替换值、添加或删除键。
editor-secret-immutable-notice = 此 Secret 不可变（immutable: true）：Kubernetes 拒绝对其数据的任何更改。如需更改，请删除后重新创建。
editor-secret-review-patch = 只发送这些键（对 data 的 merge patch）；其他键不受影响。审查中从不显示值。
editor-secret-review-create = 将使用这些键创建一个类型为 { $schema_type } 的新 Secret。审查中从不显示值。
editor-secret-changes-to = 对 { $title } 的更改
editor-secret-hidden-added = <已隐藏，新增>
editor-secret-hidden-changed = <已隐藏，已更改>
editor-secret-removed = <已删除>
editor-secret-load-value = 加载值
editor-secret-load-value-tooltip = 获取当前值以便编辑
editor-secret-replace = 替换
editor-secret-replace-tooltip = 输入新值而不加载当前值
editor-secret-rename-tooltip = 重命名此键
editor-secret-rename-disabled = 请先加载或替换值再重命名键：新键会与值一同设置
editor-secret-undo-tooltip = 保留当前值和名称
editor-secret-file-tooltip = 从文件获取值
editor-secret-keep = 保留
editor-secret-from-file = 来自文件 { $name }（{ $size }）；不显示内容
editor-secret-binary = { $binary }：无法以文本编辑；请从文件获取新值
editor-secret-cert-file = { $file } — { $subject }，有效期至 { $until }
editor-secret-cert-not-pem = { $file } — 不是 PEM 证书
editor-secret-key-file = { $file } — 已加载，不显示
editor-secret-hint-opaque = 值以纯文本输入或从文件获取；由 Kubach 负责编码。
editor-secret-hint-docker = 创建 kubernetes.io/dockerconfigjson Secret。服务器留空表示 Docker Hub。
editor-secret-hint-tls = 从 PEM 文件（tls.crt、tls.key）创建 kubernetes.io/tls Secret。
editor-secret-user-placeholder = 用户
editor-secret-password-placeholder = 密码或令牌
editor-secret-optional = 可选
editor-secret-edit-hint = 未改动的键不会发送。加载值以编辑，或不加载直接替换。值以纯文本输入；由 Kubach 负责编码。

## Secret 值（详情面板）

editor-secret-key-gone = 键 { $key } 已不在该 Secret 中
editor-secret-checking-access = 正在检查访问权限…
editor-secret-denied = { $message } 无法显示值。
editor-secret-data = 数据
editor-secret-immutable = 不可变
editor-secret-immutable-yes = 是 — 无法更改数据
editor-secret-hidden-keys = { editor-secret-hidden } 仍可在不加载的情况下更改键。
editor-secret-security-settings = 安全设置
editor-secret-show = 显示
editor-secret-hide = 隐藏
editor-secret-copy-tooltip = 复制；{ $seconds } 秒后从剪贴板清除
editor-secret-empty = （空）
editor-secret-ttl-note = 值在显示时从集群获取，并在 { $reveal } 秒后再次隐藏。复制的值会在 { $clipboard } 秒后、保险库锁定或 Kubach 退出时从剪贴板清除。
editor-secret-truncated = …（已截断；复制会获取完整值）
editor-secret-summary-registry = 镜像仓库凭据
editor-secret-summary-tls = TLS 证书
editor-secret-summary-basic-auth = 基本认证
editor-secret-summary-what-tls = 只读取 tls.crt；从不获取私钥。
editor-secret-summary-what-basic-auth = 只读取用户名，不读取密码。
editor-secret-summary-what-registry = 显示镜像仓库和用户名，从不显示密码。
editor-secret-show-summary = 显示摘要
editor-secret-tls-key-hidden = tls.key — 已隐藏
editor-secret-value-hidden = 已隐藏
editor-secret-password-set = 已设置（已隐藏）
editor-secret-password-none = 无
editor-cert-not-yet-valid = 尚未生效
editor-cert-expired = 已于 { $days } 天前过期
editor-cert-expires = { $days } 天后过期
editor-cert-valid = 有效，剩余 { $days } 天
editor-cert-leaf = 叶证书
editor-cert-chain = 链证书 { $index }
editor-cert-subject = 主题
editor-cert-issuer = 颁发者
editor-cert-self-signed = 自签名
editor-cert-valid-from = 生效时间
editor-cert-valid-until = 到期时间
editor-cert-status = 状态

## 新建 namespace

editor-namespace-new = 新建 namespace
editor-namespace-new-in = 在 { $context } 中新建 namespace
editor-namespace-forbidden = 没有权限（RBAC）：{ $message }
editor-namespace-name-placeholder = 例如 team-web
editor-namespace-labels-placeholder = 可选：team=web, env=dev
editor-namespace-not-connected = 未连接到集群
editor-namespace-created = Namespace { $name } 已创建
editor-namespace-name-hint = 小写字母、数字和“-”，最多 63 个字符。
editor-namespace-labels = Labels
editor-namespace-labels-hint = key=value，用逗号分隔。
