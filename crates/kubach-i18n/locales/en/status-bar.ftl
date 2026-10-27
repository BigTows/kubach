# Status bar at the bottom of the main window. Context and namespace names
# and Kubernetes versions are data: keep them as they are.

status-loading-kubeconfig = Loading kubeconfig…
status-switch-context = Switch context
# $error: the connection error, from the cluster or kube-rs (English).
status-error = Error: { $error }

status-no-cluster = No cluster
status-cluster = { $context }
status-cluster-version = { $context } · { $version }
status-cluster-connecting = { $context } · connecting…
status-cluster-error = { $context } · error

status-all-namespaces = All namespaces
status-no-namespace = No namespace
status-namespace = { $namespace }
status-namespaces =
    { $count ->
        [one] { $count } namespace
       *[other] { $count } namespaces
    }
status-namespaces-tooltip = Namespaces shown
status-namespaces-tooltip-change = Namespaces shown · change (⌘⇧N / Ctrl+Shift+N)

# Watches: Kubernetes API watch streams of the resource cache.
status-syncing = Syncing…
status-reconnecting = Reconnecting…
status-sync-loading =
    { $count ->
        [one] { $count } watch loading
       *[other] { $count } watches loading
    }
status-sync-reconnecting =
    { $count ->
        [one] { $count } watch reconnecting
       *[other] { $count } watches reconnecting
    }
status-sync-both =
    { $loading ->
        [one] { $loading } watch loading
       *[other] { $loading } watches loading
    }, { $reconnecting ->
        [one] { $reconnecting } watch reconnecting
       *[other] { $reconnecting } watches reconnecting
    }

status-operations =
    { $count ->
        [one] { $count } operation running
       *[other] { $count } operations running
    }
status-operations-tooltip = Show operations: time, phase and output

status-forwards =
    { $count ->
        [one] { $count } port forward
       *[other] { $count } port forwards
    }
status-forwards-tooltip = Network → Port forwards

status-terminal = Terminal
status-terminal-tooltip = New local terminal with this context (Ctrl+`)

status-mcp-off = MCP off
status-mcp-failed = MCP failed
status-mcp-pending = MCP · { $count } to confirm
status-mcp-no-sessions = MCP · no sessions
status-mcp-sessions =
    { $count ->
        [one] MCP · { $count } session
       *[other] MCP · { $count } sessions
    }
status-mcp-tooltip = AI agent sessions

status-version-tooltip = Copy version, OS and cluster version
status-version-copied = Version copied to the clipboard
