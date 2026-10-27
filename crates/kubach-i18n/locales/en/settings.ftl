# Settings screen: sidebar, search and the General page.

settings-back = Back to Kubach
settings-back-tooltip = Close settings (Esc)
settings-search-placeholder = Search settings
settings-filter-contexts-placeholder = Filter contexts
# $query: what the user typed.
settings-no-match = No settings match “{ $query }”.
settings-saved-to = Saved to { $path }
settings-not-saved = No config directory: settings are not saved.
settings-file-problem = Settings file

## Pages in the sidebar

settings-nav-clusters = Clusters
settings-section-general = General
settings-section-clusters = Common
settings-section-cluster = Per cluster
settings-section-agent = AI agent
settings-section-tools = External tools
settings-section-presets = Presets
settings-title-clusters = Clusters: common
settings-title-cluster = Clusters: per cluster
settings-description-general = Appearance, language and storage.
settings-description-clusters = For every cluster: kubeconfig files, imported kubeconfigs, the context to open and background connections.
settings-description-cluster = One context at a time: hotbar tile, default namespace, Prometheus, Helm, security, and removing it from Kubach.
settings-description-agent = Connect an AI agent over MCP: the HTTP server, sessions, approvals and the audit log.
settings-description-tools = Programs Kubach runs on this machine.
settings-description-presets = Resource presets saved from the YAML editor, offered by “+” on the resources page.

## General page

settings-group-appearance = Appearance
settings-theme = Theme
settings-theme-dark = Dark
settings-theme-light = Light
settings-theme-system = System
settings-theme-description = System follows the OS light or dark appearance.

settings-group-language = Language
settings-language = Interface language
settings-language-description = Partially translated: the status bar, the menu and this page so far, the rest is still in English. Kubernetes names, cluster data and command output stay as they are.

settings-group-terminal = Terminal
settings-node-shell-namespace = Node shell namespace
settings-node-shell-namespace-description = Where the privileged pod of a node Shell goes. Pod Security must allow privileged pods there.
settings-node-shell-image = Node shell image
settings-node-shell-image-description = Image of the node Shell pod; it needs nsenter (busybox and alpine have it).
settings-terminal-clipboard = Let programs copy to the clipboard
settings-terminal-clipboard-description = Programs in Shell and Debug tabs may replace the clipboard (OSC 52). Off by default: a pod could put a command there for you to paste.
settings-confirm-debug = Confirm Debug containers
settings-confirm-debug-description = Ask before Debug adds an ephemeral container to a pod: it cannot be removed until the pod is deleted.

settings-group-storage = Storage
settings-cache = Cache
settings-cache-counting = Counting…
# $size: e.g. "12.4 MB".
settings-cache-description = { $size }: chart icons and README images. Everything there is downloaded again when needed; settings and saved state stay.
# $dir: the cache directory.
settings-cache-description-in = { $size } in { $dir }: chart icons and README images. Everything there is downloaded again when needed; settings and saved state stay.
settings-cache-clear-failed = Couldn't clear it all: { $error }
settings-clear-cache = Clear cache

## Durations: `30 min`, `1 h`, `1 h 30 min`, `40 s`

settings-duration-minutes = { $minutes } min
settings-duration-hours = { $hours } h
settings-duration-hours-minutes = { $hours } h { $minutes } min
settings-duration-seconds = { $seconds } s
settings-unit-minutes = min
