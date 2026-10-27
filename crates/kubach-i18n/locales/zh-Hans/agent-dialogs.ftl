# AI 代理的 MCP 会话在 Kubach 窗口中提出的问题：批准更改、权限、Helm
# release、上下文或会话，Secret 表单和通知。请求的摘要、详情、权限和限制
# 由 kubach-mcp 以英文提供给代理和审计，此处为数据。机器翻译草稿，需母语者校对。

## 通知

agent-security-settings = 安全设置
agent-notice-reveal-off = 已关闭显示 Secrets
agent-notice-refused = 已拒绝代理
agent-notice-text = { $text }
agent-notice-mcp-off = { $summary }：此上下文已关闭 AI 代理访问。
agent-notice-protected-context = { $summary }：该上下文是受保护的集群。
agent-notice-protected = 受保护的集群
agent-notice-protected-ceiling = { $summary } 该上下文是受保护的集群，因此 Kubach 未经询问直接拒绝。
agent-notice-session-only = 仅在本会话中允许
agent-notice-session-only-text = 无法写入设置文件，因此提升的上限仅在会话结束前有效。

## 对话框标题

agent-title-read = 允许代理读取？
agent-title-reveal = 向代理显示 Secret？
agent-title-modify = 允许代理进行更改？
agent-title-exec = 运行代理的命令？
agent-title-ceiling = 提升代理的上限？
agent-title-helm-install = 安装代理的 Helm release？
agent-title-helm-upgrade = 升级代理的 Helm release？
agent-title-helm-rollback = 回滚 Helm release？
agent-title-helm-uninstall = 卸载 Helm release？
agent-title-chart-source = 允许 chart 源？
agent-title-use-context = 允许代理在此上下文中工作？
agent-title-call = 批准代理的更改？
agent-title-secret-create = 为新 Secret 输入值
agent-title-secret-update = 更改 Secret
agent-title-secret-copy = 为代理复制 Secret？
agent-title-secret-delete = 为代理删除 Secret？

## 确认

agent-arg-empty = （空）
agent-command-header = { $count } 个参数，{ $bytes } 字节 · ␣ 表示空格，\n 表示换行，\u{"{"}…{"}"} 表示不可见字符
agent-no-longer-pending = 此请求已不再等待。
agent-dangerous-title = 越过 namespace 边界或读取 Secrets
agent-dangerous-line = { $message } — { $path }
agent-dangerous-line-document = { $message } — 文档 { $document }：{ $path }
agent-acknowledge = 我了解这会让工作负载越过 namespace 边界或读取这些 Secrets，仍然批准
agent-secret-leaves = 该值将离开你的计算机
agent-secret-no-take-back = 代理一旦获得，Kubach 无法收回。
agent-deny = 拒绝
agent-reject = 驳回
agent-approve = 批准
agent-allow-always = 始终允许
agent-allow-session = 本会话允许
agent-helm-install = 安装
agent-helm-upgrade = 升级
agent-helm-rollback = 回滚
agent-helm-uninstall = 卸载
agent-dont-reveal = 不显示
agent-reveal = 向代理显示
agent-keep-secret = 保留
agent-delete-secret = 删除 Secret
agent-copy-secret = 复制 Secret
agent-field-agent = 代理
agent-client-unverified = 名称由客户端报告，未经验证
agent-field-context = 上下文
agent-field-namespace = Namespace
agent-field-right = 权限
agent-field-release = Release
agent-field-chart = Chart
agent-field-version = 版本
agent-field-revision = 修订版本
agent-field-runs-as = 运行身份
agent-field-asks-for = 请求
agent-field-ceiling = 当前上限
agent-field-tool = 工具
agent-field-session = 会话
agent-helm-values = Values（敏感键已遮蔽）
agent-helm-objects = 对象
agent-always = 在此集群上始终允许
agent-always-note = 永久提升 { $context } 在 设置 → Security 中的代理上限，并记录时间和来源会话。只能在设置中再次降低。
agent-limits-off = 服务器端限制已关闭
agent-limits-off-note = 调用使用你自己的凭据运行；只有 Kubach 会检查它。
agent-client-only = 仅在 Kubach 中设边界：调用使用你自己的凭据运行。
agent-auto-reject = { $left } 后自动拒绝
agent-auto-reject-queued = { $left } 后自动拒绝 · 还有 { $queued } 个在等待

## Secret 表单

agent-writes-to = 写入到
agent-secret = Secret
agent-copied-from = 复制自
agent-secret-type = 类型
agent-secret-keys = 键
agent-secret-unverified = 名称和键来自代理，未经验证。
agent-explanation = 代理的说明，未经验证
agent-still-referenced = 仍被引用：这些对象的新 Pod 将无法启动
agent-used-by = 使用者（重启它们以获取新值）
agent-placeholder-user = 用户
agent-placeholder-password = 密码或令牌
agent-placeholder-optional = 可选
agent-placeholder-key = 键
agent-placeholder-value = 值
agent-enter-value = 请为 { $key } 输入值、生成值或不添加该键
agent-enter-value-new = 请为新键输入值、生成值或不添加该键
agent-key-exists = 键 { $key } 已存在；请在其所在行中更改
agent-key-twice = 键 { $key } 被设置了两次
agent-choose-tls = 请选择证书和私钥
agent-new-key = 新键
agent-value-unchanged = 未更改，{ $size }
agent-value-type = 请输入值
agent-value-typed = 已输入，{ $count } 个字符
agent-value-generated = 已生成，{ $count } 个字符，已隐藏
agent-value-file = 来自 { $name }（{ $size }），不显示
agent-value-removed = 将被删除
agent-badge-asked = 代理请求
agent-badge-remove = 代理建议删除
agent-keep-key = 保留此键
agent-keep = 保留
agent-type-new = 输入新值
agent-type = 输入
agent-generate-tooltip = 生成随机值；任何人都看不到
agent-generate = 生成
agent-copy-generated = 复制生成的值；剪贴板将在 30 秒后清空
agent-from-file = 从文件获取值
agent-keep-current = 保留当前值
agent-remove-key = 删除此键
agent-leave-out = 不添加此键
agent-add-key = 添加键
agent-tls-cert = 证书（.crt）
agent-tls-key = 私钥（.key）
agent-registry = Registry
agent-user = 用户
agent-password = 密码
agent-email = 邮箱
agent-hint-tls = 请选择 PEM 证书及其私钥；Kubach 会检查它们是否匹配。
agent-hint-docker = Kubach 会根据这些字段生成 .dockerconfigjson。registry 为空表示 Docker Hub。
agent-hint-update = 未更改的键不会发送。请为要替换的键输入、生成或选择文件。
agent-hint-keys = 输入每个值、生成它或从文件获取。
agent-create-secret = 创建 Secret
agent-apply = 应用更改
agent-client = 代理：{ $client }
agent-client-unverified-note = （名称由客户端报告，未经验证）
agent-secret-info = 这些值由 Kubach 写入；代理只会得到键和大小。只输入用于此上下文和 namespace 的值。
agent-secret-limits = { $warning }。写入使用你自己的凭据运行。
agent-auto-cancel = { $left } 后自动取消
agent-cancel = 取消
agent-file-unreadable = 无法读取 { $name }：{ $error }
agent-file-too-large = { $name } 超过 1 MiB，即整个 Secret 的上限
agent-cert-valid = { $subject }，有效期至 { $time }
agent-cert-none = 没有证书
agent-key-none = 此文件中没有 PEM 私钥
agent-key-loaded = 已加载私钥，不显示
agent-key-matches = 与证书匹配
agent-key-differs = 与证书不匹配
agent-key-unknown = Kubach 无法判断它是否与证书匹配

## 开始会话

agent-use-context-summary = { $client } 想在上下文 { $context } 中工作
agent-use-context-details = 同意后将在 { $context } 上开始代理会话：开始时没有 namespace 和权限。读取 namespace、更改它以及提升上限都会在调用需要时在此单独询问，最高到 { $profile } 配置，持续 { $minutes } 分钟。
agent-consent-summary = 使用 Kubach 保管库中的凭据在 `{ $context }` 上开始 AI 代理会话？
agent-consent-session = 新会话，pid { $pid }
agent-consent-tool = 开始会话
agent-consent-namespaces = Namespace：{ $namespaces }
agent-consent-namespaces-any = 代理指定的任意 namespace，每个都会在此询问
agent-consent-profile = 配置：最高 { $profile }；每项权限都会询问
agent-consent-time = 时间：{ $time }，或直到保管库锁定
agent-consent-reading = 读取：{ $reading }
agent-consent-reading-granted = 从一开始就允许（--grant-read）
agent-consent-reading-asked = 代理需要时在此询问
agent-consent-process = 进程：{ $process }
agent-consent-unknown-process = pid { $pid }（操作系统未显示此进程）
agent-consent-warning = pid 由进程自己报告。如果你刚才没有为此上下文启动代理，请驳回。

