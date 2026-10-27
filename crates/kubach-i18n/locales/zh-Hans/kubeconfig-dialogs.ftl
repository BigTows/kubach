# 已链接的 kubeconfig 文件：预览、取消链接、程序与文件说明、文件读取问题和
# “测试连接”失败。路径、名称、环境变量和操作系统错误是数据。机器翻译草稿，
# 需母语者校对。

## 文件读取问题

kubeconfig-file-missing = 找不到文件
kubeconfig-file-unreadable = 无法读取文件：{ $error }
kubeconfig-file-invalid = 不是有效的 kubeconfig：{ $error }
kubeconfig-file-locked = 保管库已锁定

## 取消链接

kubeconfig-unlink-title = 取消链接 { $name }？
kubeconfig-unlink-description =
    { $count ->
        [0] Kubach 将不再读取 { $path }。它在 Kubach 中没有上下文。文件本身不会改变，可随时重新链接。
       *[other] Kubach 将不再读取 { $path }。其 { $count } 个上下文将从 Kubach 中消失：{ $contexts }。文件本身不会改变，可随时重新链接。
    }
kubeconfig-unlink = 取消链接
kubeconfig-restrict-failed = 无法更改 { $path } 的权限：{ $error }

## 链接预览

kubeconfig-find-again-title = 重新查找 kubeconfig 文件
kubeconfig-link-title =
    { $count ->
        [one] 链接 kubeconfig 文件
       *[other] 链接 { $count } 个 kubeconfig 文件
    }
kubeconfig-link-description = Kubach 原地读取文件，因此 aws、gcloud 等工具的更新会被自动获取。凭据不会被复制。
kubeconfig-close = 关闭
kubeconfig-use-file = 使用此文件
kubeconfig-link-file = 链接文件
kubeconfig-link-files = 链接 { $count } 个文件
kubeconfig-permissions-warning = 此计算机上的其他用户可以读取此文件（{ $mode }）。它可能包含凭据；请将权限限制为 0600（chmod 600 { $path }）。
kubeconfig-already-linked = 此文件已链接。
kubeconfig-no-contexts = 文件中没有上下文。
kubeconfig-context-shadowed = 已定义，跳过此项
kubeconfig-no-server = 无 API 服务器
kubeconfig-contexts = 上下文（{ $count }）

## kubeconfig 运行和读取的内容

kubeconfig-review-programs =
    { $purpose ->
        [link] 连接时会在此计算机上运行这些程序。仅在你预期它们时才链接。
       *[import] 连接时会在此计算机上运行这些程序。仅在你预期它们时才导入。
    }
kubeconfig-review-programs-dangerous =
    { $purpose ->
        [link] 连接时会在此计算机上运行这些程序。仅在你预期它们时才链接。红色变量可能使程序加载或启动其他代码。
       *[import] 连接时会在此计算机上运行这些程序。仅在你预期它们时才导入。红色变量可能使程序加载或启动其他代码。
    }
kubeconfig-review-exec = 用户 { $user }，exec
kubeconfig-review-auth-provider = 用户 { $user }，auth-provider { $name }
kubeconfig-review-files-link = 连接时会读取这些本地文件；其中的密钥和令牌会发送到集群服务器。
kubeconfig-review-files-import = 这些本地文件会被复制到保管库；其中的密钥和令牌会发送到集群服务器。
kubeconfig-review-files-pasted = 粘贴的配置引用了这些本地文件。只有在你于下方确认后，Kubach 才会将它们读入保管库；其中的密钥和令牌会发送到集群服务器。
kubeconfig-review-file-cluster = 集群 { $owner }，{ $key }
kubeconfig-review-file-user = 用户 { $owner }，{ $key }
kubeconfig-review-file-cluster-outside = 集群 { $owner }，{ $key }（在 kubeconfig 所在文件夹之外）
kubeconfig-review-file-user-outside = 用户 { $owner }，{ $key }（在 kubeconfig 所在文件夹之外）

## “测试连接”失败

probe-unauthorized = 身份验证失败：服务器拒绝了凭据
probe-forbidden = 已登录，但无权读取服务器版本
probe-unknown-ca = TLS：服务器证书不受信任
probe-certificate-name = TLS：服务器证书签发给了其他主机名
probe-tls = TLS 握手失败
probe-dns = 无法解析服务器主机名
probe-refused = 连接被拒绝：此地址上没有服务在监听
probe-timeout = 服务器未及时响应
probe-exec-not-found = 找不到 exec 插件的命令
probe-exec-failed = exec 插件执行失败
probe-invalid-config = 连接设置无效
probe-server = 服务器返回 HTTP { $code }
probe-anonymous = 已连接，但服务器忽略了凭据（anonymous）
probe-other = 无法连接
probe-unauthorized-hint = 请检查凭据：它们可能已过期、被吊销或属于其他集群。大多数集群（Kubernetes 1.19+）根本不接受用户名和密码。
probe-forbidden-hint = 凭据有效，但 RBAC 连 /version 都不允许。请向集群管理员申请访问权限。
probe-unknown-ca-hint = 使用集群的 CA 证书（其 kubeconfig 中的 certificate-authority-data）；服务器证书过期也会导致此错误。
probe-certificate-name-hint = 请使用证书签发时的主机名连接，而不是 IP 或别名。
probe-tls-hint = 请检查 CA 证书、客户端证书和密钥，以及 URL 是否使用 https。
probe-dns-hint = 请检查主机名以及 VPN 或 DNS 设置。
probe-refused-hint = 请检查端口以及集群是否在运行。
probe-timeout-hint = 请检查地址、防火墙和 VPN；exec 插件可能在等待登录。
probe-exec-not-found-hint = 请安装插件（OIDC：kubectl krew install oidc-login）或提供完整路径；Kubach 使用你登录 shell 的 PATH。
probe-exec-failed-hint = 在终端中运行插件命令以查看失败原因。
probe-anonymous-hint = 服务器不支持这种登录方式：Kubernetes 1.19+ 会忽略用户名和密码。请使用令牌、客户端证书、OIDC 或 exec 插件。
