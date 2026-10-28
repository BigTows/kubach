# Bottom dock: log and terminal tabs, local terminal, node shell, the
# port-forward dialog, the "Port forwards" screen and the buttons that open
# them from the details panel. Pod, container, namespace, context, node and
# port names, selectors and images are data: keep them as they are.
# Kubernetes terms (Pod, Service, init container, port-forward, kubectl)
# stay in English.

## Shared

dock-connecting = Connecting…
dock-not-connected = Not connected to a cluster
# A tab opened in $context can't (re)connect.
dock-context-not-connected = Context { $context } is not connected; connect to it to reconnect this tab
dock-cancel = Cancel
# A container in menus and tooltips.
dock-container = Container { $container }

## Tab strip

dock-tab-close = Close
dock-collapse = Collapse
dock-expand = Expand
# Status of a logs tab; $lines is how many lines it shows.
dock-logs-streaming =
    { $lines ->
        [one] Streaming · { $lines } line
       *[other] Streaming · { $lines } lines
    }
dock-logs-ended =
    { $lines ->
        [one] Ended · { $lines } line
       *[other] Ended · { $lines } lines
    }

## Stream buttons in the details panel

dock-forward-title = Port forward
# Tooltips of icon-only buttons: the action, then what it does.
dock-entry-logs-all = Logs: Stream logs of all containers in the bottom panel
dock-entry-logs = Logs: Stream logs in the bottom panel
dock-entry-shell-default = Shell: Open a shell in { $container } (default container)
dock-entry-shell = Shell: Open a shell in the container
dock-entry-debug-default = Debug: Start an ephemeral busybox container targeting { $container } (default container) and attach to it
dock-entry-debug = Debug: Start an ephemeral busybox container next to the app and attach to it
dock-entry-forward-selector = Port forward: Forward a local port to a ready pod of this resource
dock-entry-forward = Port forward: Forward a local port to this pod
dock-entry-forward-port = Port forward: port { $port }, pick the local port
dock-choose-container = Choose a container
dock-choose-port = Choose a port
dock-choose-local-port = Choose local port…

## Container menus

dock-all-containers = All containers
dock-containers = Containers
dock-init-containers = Init containers

## Debug confirmation

dock-debug-title = Debug pod { $pod }?
# $image: docker.io/library/busybox:1.37.
dock-debug-message = Adds an ephemeral debug container ({ $image }) to pod { $namespace }/{ $pod }. It cannot be removed: it stays in the pod until the pod is deleted.
dock-debug-message-target = Adds an ephemeral debug container ({ $image }) to pod { $namespace }/{ $pod }, sharing processes with { $container }. It cannot be removed: it stays in the pod until the pod is deleted.
dock-debug-dont-ask = Don't ask again
dock-debug-confirm = Add debug container

## Logs tab

dock-logs-search = Search
dock-logs-no-containers = No containers to read
dock-logs-waiting = Waiting for log lines…
dock-logs-no-lines = No log lines
dock-logs-loading-earlier = Loading earlier lines…
dock-logs-earlier-failed = Couldn't load earlier lines: { $error }
dock-logs-beginning = Beginning of log
# $lines: 50,000 (already formatted); $mib: a number of MiB.
dock-logs-capped = Showing the latest lines (up to { $lines } lines or { $mib } MiB); use kubectl logs for older ones
dock-logs-reconnecting = Reconnecting…
dock-logs-dropped =
    { $count ->
        [one] { $count } line dropped
       *[other] { $count } lines dropped
    }
dock-logs-ended-indicator = Stream ended
# Toggle: logs of the previous (crashed) container instance.
dock-logs-previous = Previous
dock-logs-timestamps = Timestamps
dock-logs-wrap = Wrap
dock-logs-prev-match = Previous match (Shift+Enter)
dock-logs-next-match = Next match (Enter)
dock-logs-pause = Pause
dock-logs-follow = Follow
dock-logs-copy = Copy all lines
dock-logs-clear = Clear

## Shell and debug terminals

dock-terminal-running = Running
dock-terminal-starting-debug = Starting debug container…
dock-terminal-attached = Attached to debug container { $container }. If you don't see a prompt, press Enter.
# $shells: /bin/bash, /bin/sh, /bin/ash, /busybox/sh.
dock-terminal-no-shell = Container { $container } has no shell: none of { $shells } found (distroless or scratch image?). Use Debug to attach a busybox container instead
# The first shell of the list is missing; $shell: /bin/sh, $missing: /bin/bash.
dock-terminal-shell-fallback = Started { $shell }: { $missing } not found in the image
dock-terminal-debug-tooltip = Attach an ephemeral busybox container that shares processes with this container
dock-terminal-container-tooltip = Container: switching reconnects the shell
dock-terminal-target-tooltip = Target container: switching starts a new debug container
dock-terminal-reconnect = Reconnect
dock-terminal-new-debug = New debug container
# How a session ended.
dock-exit-success = Exited
dock-exit-code = Exited with code { $code }
dock-exit-failed = Failed
dock-exit-failed-message = Failed: { $message }
dock-exit-disconnected = Disconnected

## Local terminal

dock-local-title = Terminal — { $context }/{ $namespace }
dock-local-title-plain = Terminal
dock-local-starting = Starting shell…
dock-local-context-unavailable = Context { $context } is not available (is the vault locked?)
dock-local-no-cluster = No cluster is open in Kubach: KUBECONFIG is left as your shell sets it.
dock-local-no-temp-dir = No folder for the temporary kubeconfig
dock-local-temp-write-failed = Can't write the temporary kubeconfig: { $error }
dock-local-restart = Restart

## Node shell

dock-node-title = Node shell — { $node }
dock-node-creating = Creating pod…
dock-node-creating-in = Creating pod in { $namespace }…
dock-node-created = Created privileged pod { $namespace }/{ $pod }; waiting for it to run…
dock-node-waiting = Waiting for pod…
dock-node-new-pod = New pod

## Port-forward dialog and toasts

dock-forward-enter-port = Enter a port
dock-forward-invalid-port = { $text } is not a port (1–65535)
# Menu item of a UDP port; $port: 53 → 53 · dns · UDP.
dock-forward-menu-tcp-only = { $port } (port-forward is TCP only)
dock-forward-started = Port forward started
# $route: svc/web:80 → http://127.0.0.1:8080.
dock-forward-started-via = { $route } (pod { $pod }:{ $port })
dock-forward-open = Open
dock-forward-open-in-browser = Open { $url } in the browser
dock-forward-copy = Copy
dock-forward-copy-tooltip = Copy the address
dock-forward-service-port = Service port
dock-forward-pod-port = Pod port
dock-forward-service-ports = Service ports
dock-forward-container-ports = Container ports
dock-forward-local-placeholder = Random free port
dock-forward-route = → pod { $pod }:{ $port }
dock-forward-port-not-tcp = { $protocol } ports cannot be forwarded: port-forward is TCP only
dock-forward-service-to-pod-port = Service port → pod port { $port }
dock-forward-service-to-named-port = Service port → container port named { $name }
dock-forward-looking = Looking for a ready pod…
dock-forward-ready-pod = Ready pod: { $pod }
dock-forward-pod = Pod: { $pod }
dock-forward-pods = Pods: { $selector }
dock-forward-udp-disabled = UDP ports are disabled: port-forward tunnels TCP only.
dock-forward-local-port = Local port on 127.0.0.1
dock-forward-local-random = Empty: a random free port. A port you enter is remembered for next time.
dock-forward-local-remembered = This port is remembered for this forward next time.
dock-forward-port-busy = Port { $port } is already in use on this computer. Enter another port or leave the field empty for a random one.
dock-forward-port-denied = Port { $port } cannot be used without administrator rights. Enter a port above 1024 or leave the field empty.
dock-forward-start = Start
dock-forward-starting = Starting…

## "Port forwards" screen

dock-forwards-ready = Working
dock-forwards-failing = Failing
dock-forwards-reconnecting = Reconnecting
dock-forwards-ready-tooltip = Listening on the local port; the last connection reached the pod
dock-forwards-failing-tooltip = The last connection to the pod failed. The local port still listens: the next connection tries again
dock-forwards-reconnecting-tooltip = After a failure, a new connection is reaching the pod
dock-forwards-last-error = Last error: { $error }
dock-forwards-stop-tooltip = Stop the forward and close its connections
dock-forwards-errors =
    { $count ->
        [one] { $count } error
       *[other] { $count } errors
    }
dock-forwards-open = Open { $url }
dock-forwards-copy = Copy { $address }
dock-forwards-description = Forwards keep running across context switches until stopped.
dock-forwards-pod = Pod
dock-forwards-context-current = Current context
dock-forwards-connections = { $active } open · { $total } total
dock-forwards-agent = agent · { $client }
dock-forwards-agent-tooltip = Opened by the AI agent { $client }, session { $session }. It stops when the session ends; you can stop it now.
dock-forwards-connections-tooltip = Connections open now, and all connections since the forward started
dock-forwards-sort = Sort
dock-forwards-sort-port = Port
dock-forwards-sort-status = Status
dock-forwards-sort-target = Target
dock-forwards-paused = Paused
dock-forwards-paused-tooltip = The local port stays taken, but nothing is forwarded: connections are closed
dock-forwards-pause = Pause: keep the local port, close the connections
dock-forwards-resume = Resume forwarding on { $address }
dock-forwards-reveal = Open { $target }
dock-forwards-empty = No port forwards
dock-forwards-empty-description = Select a pod, Service or workload and click Port forward.
dock-forwards-restore-waiting = Waiting
dock-forwards-restore-waiting-tooltip = Running when Kubach quit: starts again on the same local port once its context is connected
dock-forwards-restore-waiting-context = Starts once context { $context } is connected
dock-forwards-restore-unknown-context = Context { $context } is not available: unlock the vault or add its kubeconfig
dock-forwards-restore-starting = Starting
dock-forwards-restore-starting-detail = Starting on the same local port…
dock-forwards-restore-failed = Not started
dock-forwards-restore-failed-tooltip = Running when Kubach quit, but could not start again. Retry, or remove it so it is not restored
dock-forwards-restore-port-busy = Port { $port } is already in use on this computer. Free it and retry, or remove this forward and start a new one.
dock-forwards-restore-gone = { $source } no longer exists in the cluster
dock-forwards-restore-cannot-connect = Cannot connect to context { $context }: { $error }
dock-forwards-restore-disconnected = Context { $context } disconnected before the forward started
dock-forwards-retry-tooltip = Try to start the forward again
dock-forwards-remove-tooltip = Remove the forward: it will not be restored again
