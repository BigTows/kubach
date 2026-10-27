# Main window frame: sidebar, hotbar of clusters, "Back", empty states,
# toasts and the debug overlay. Context, cluster, user, namespace and file
# names are data: keep them as they are.

## Sidebar sections

nav-overview = Overview
nav-events = Events
nav-topology = Topology
nav-cluster = Cluster
nav-workloads = Workloads
nav-network = Network
nav-gateway-api = Gateway API
nav-storage = Storage
nav-config = Config
nav-access-control = Access control
nav-custom-resources = Custom resources
nav-helm = Helm
nav-kustomize = Kustomize
nav-gitops = GitOps
nav-port-forwards = Port forwards
nav-permissions = Permissions
nav-security-checks = Security checks
# Leaves of the Helm group.
nav-helm-releases = Releases
nav-helm-charts = Charts
# The same leaves on the "← Back" button, outside their group.
nav-back-helm-releases = Helm releases
nav-back-helm-charts = Helm charts

## "← Back" in the titlebar

# $place: a section or a resource ("Topology", "Deployment web");
# $shortcut: ⌘[ or Alt+←.
workspace-back-tooltip = Back to { $place } ({ $shortcut })

## Toasts and banners

workspace-namespace-added = Namespace { $namespace } added to the shown namespaces
workspace-undo = Undo
# $error: why the login shell failed (English, from the OS).
workspace-shell-env-banner = Shell environment not loaded: { $error }. PATH and KUBECONFIG from your shell profile are missing, so exec auth plugins may not be found. Check your shell rc files or start Kubach from a terminal.

## Empty states of the main area

workspace-loading-kubeconfig = Loading kubeconfig…
workspace-kubeconfig-not-found = Kubeconfig not found
workspace-kubeconfig-unavailable = Kubeconfig files unavailable
workspace-kubeconfig-unavailable-description = Linked kubeconfig files can't be read. Find them again in settings.
workspace-open-settings = Open settings
workspace-no-contexts = No contexts in kubeconfig
workspace-no-contexts-description = Link a kubeconfig file or import one into the encrypted vault, or drop the file here.
workspace-add-cluster = Add cluster…
workspace-no-cluster = No cluster connected
workspace-no-cluster-description = Pick a context from your kubeconfig to browse resources.
workspace-disconnected = Disconnected from { $context }
workspace-connect = Connect
workspace-connecting = Connecting to { $context }…
workspace-connect-failed = Can't connect to { $context }
workspace-retry = Retry

## Hotbar tile tooltips

hotbar-row-cluster = Cluster
hotbar-row-server = Server
hotbar-row-user = User
hotbar-row-namespace = Namespace
hotbar-row-status = Status
hotbar-row-shortcut = Shortcut
hotbar-row-source = Source
hotbar-row-security = Security
hotbar-row-vault = Vault
hotbar-row-problem = Problem
hotbar-row-file = File
hotbar-row-fix = Fix
hotbar-fix-linked = Right-click → Find again… or Remove file
hotbar-vault-locked = Locked. Click to unlock.
hotbar-state-disconnected = Disconnected
hotbar-state-connecting = Connecting…
# $version: the Kubernetes version of the cluster.
hotbar-state-connected = Connected, { $version }
# $error: the connection error, from the cluster or kube-rs (English).
hotbar-state-error = Error: { $error }
# $list: what is looser, joined with commas (hotbar-loosened-*).
hotbar-security-looser = Looser than strict: { $list }
hotbar-loosened-mcp = AI agent access
hotbar-loosened-profile = profile
hotbar-loosened-temporary-objects = separate identity in the cluster
hotbar-loosened-deploy = applying changes
hotbar-loosened-exec = exec
hotbar-loosened-session-ttl = session length
hotbar-loosened-namespaces = several namespaces
hotbar-loosened-secret-values = Secret values shown
hotbar-loosened-agent-secret-values = Secret values revealed to the agent on request
hotbar-loosened-agent-other-kinds = custom resources for the agent

## Hotbar menus

hotbar-menu-connect = Connect
hotbar-menu-disconnect = Disconnect
hotbar-menu-rename = Rename…
hotbar-menu-set-color = Set color…
hotbar-menu-hide = Hide
hotbar-menu-security = Security settings…
hotbar-menu-lock-imported = Lock imported clusters
hotbar-menu-remove = Remove from Kubach…
hotbar-menu-unlock = Unlock…
hotbar-menu-kubeconfig-settings = Kubeconfig settings…
hotbar-menu-find-again = Find again…
hotbar-menu-remove-file = Remove file
# Title of the list of hidden tiles in the "more" menu.
hotbar-hidden = Hidden

## Rename dialog of a hotbar tile

hotbar-rename-title = Rename { $context }
hotbar-rename-description =
    { $max ->
        [one] Short label on the hotbar tile, up to { $max } character. Empty means initials.
       *[other] Short label on the hotbar tile, up to { $max } characters. Empty means initials.
    }

## Debug overlay (debug builds only)

debug-menu-overlay = Debug Overlay
debug-section-frames = Frames
debug-section-process = Process
debug-fps = FPS
debug-idle = idle
debug-frame = Frame
debug-avg-max = Avg / max 5 s
debug-cpu = CPU
debug-memory = Memory
debug-threads = Threads
debug-clusters = Clusters
debug-watches = Watches
debug-cached-objects = Cached objects
debug-port-forwards = Port forwards
debug-runtime-tasks = Runtime tasks
# $value: milliseconds, like 4.2.
debug-ms = { $value } ms
# $shortcut: ⌘⇧D or Ctrl+Shift+D.
debug-hide = { $shortcut } to hide
