# Settings → Clusters: the common page (kubeconfig files, imported
# kubeconfigs) and the per-cluster page (tile, namespace, Prometheus, Helm,
# security, removal); Settings → Presets.

## Common page: kubeconfig files

settings-kubeconfig-loading = Loading kubeconfig…
settings-kubeconfig-not-loaded = Kubeconfig not loaded: { $error }
settings-kubeconfig-files = Kubeconfig files
settings-kubeconfig-files-description = KUBECONFIG (or ~/.kube/config) is always read. Linked files are read in place and merged after it.
settings-link-file = Link file…
settings-link-file-tooltip = Read in place; credentials stay in your file
settings-import = Import…
settings-import-tooltip = Copy into the encrypted vault
# $name: a context that is no longer in any kubeconfig.
settings-startup-context-missing = { $name } (not in kubeconfig)
settings-startup-context = Context at startup
settings-startup-context-description = Opened when Kubach starts.
settings-keep-connected = Contexts kept connected
settings-keep-connected-description = Recently used contexts stay connected, so switching back is instant.
settings-find-again = Find again…
settings-unlink = Unlink…
settings-no-contexts = No contexts
settings-no-contexts-match = No contexts match
# $contexts: context names joined with commas.
settings-file-contexts =
    { $count ->
        [one] { $count } context: { $contexts }
       *[other] { $count } contexts: { $contexts }
    }
settings-restrict-permissions = Restrict to 0600

## Where a context comes from

settings-origin-linked = Linked file
settings-origin-imported = Imported
settings-origin-unknown = Unknown
# Shown when the origin of a context is not known, in “… comes from { $origin }”.
settings-origin-your-kubeconfig = your kubeconfig
# Short badges in the context list.
settings-badge-linked = linked
settings-badge-vault = vault

## Imported kubeconfigs (the vault)

settings-group-vault = Imported kubeconfigs
settings-vault-group-description = Copies kept encrypted in vault.kubach in the settings folder. Unlock them with the vault password or the system keychain.
settings-vault = Vault
settings-vault-missing = Not created yet: the first import creates it.
settings-vault-locked = Locked.
settings-vault-unlocked = Unlocked: imported contexts are available.
settings-vault-unavailable = The OS has no settings folder.
settings-vault-lock-now = Lock now
settings-vault-unlock = Unlock…
settings-vault-update = Update from file…
settings-vault-rename = Rename…
settings-vault-export = Export…
settings-vault-remove = Remove
settings-vault-import-file = Import file…
settings-vault-paste = Paste
settings-vault-paste-tooltip = Import kubeconfig YAML from the clipboard
settings-vault-keychain = Remember in the system keychain
settings-vault-keychain-busy = Remember in the system keychain…
settings-vault-keychain-checking = Checking the system keychain…
settings-vault-keychain-description = Keeps the vault key in Keychain (macOS), Credential Manager (Windows) or Secret Service (Linux), so no password is asked.
settings-vault-asks-password = Changing it asks for the vault password first.
# $method: Touch ID or Windows Hello.
settings-vault-biometric = Unlock with { $method }
settings-vault-biometric-busy = Unlock with { $method }…
settings-vault-biometric-checking = Checking { $method }…
settings-vault-biometric-description = Asks for { $method } instead of the password. The password still works and is asked when { $method } is cancelled or fingerprints change.
settings-vault-set-password-first = Set a vault password first.
settings-vault-lock-screen = Lock when the screen locks
settings-vault-lock-screen-description = Locks the vault as soon as the OS lock screen comes up.
settings-vault-lock-idle = Lock after inactivity
settings-vault-lock-idle-description = Minutes without input before the vault locks; 0 never locks.
settings-vault-password = Vault password
settings-vault-password-description = Everything is encrypted again with the new password.
settings-vault-no-password-description = The vault has no password, only the system keychain. Set one to unlock it anywhere.
settings-vault-encrypting = Encrypting…
settings-vault-change-password = Change…
settings-vault-set-password = Set…
settings-vault-reset = Reset vault
settings-vault-reset-description = Deletes the vault and every imported kubeconfig, e.g. after a forgotten password or a damaged file.
settings-vault-deleting = Deleting…
settings-vault-reset-button = Reset…

## Per-cluster page

settings-group-cluster = Cluster
settings-no-contexts-notice = No kubeconfig contexts. Add a kubeconfig in Clusters: common; its contexts are set up here.
settings-group-contexts = Contexts
settings-context-shown-below = Shown below
settings-context-open = Open
settings-context-hidden = Hidden from the hotbar
settings-no-api-server = No API server in kubeconfig
settings-source = Source
# $entry: the name of the imported copy.
settings-location-imported = Kept encrypted in the vault as “{ $entry }”.
settings-location-linked = { $path }, read in place.
settings-location-own = { $path }, your kubeconfig.
settings-show-in-hotbar = Show in hotbar
settings-show-in-hotbar-description = A hidden context stays in the hotbar's … menu.

## Hotbar tile

settings-group-hotbar-tile = Hotbar tile
settings-hotbar-tile-description = Label and color of the context's tile in the hotbar.
settings-tile-initials-placeholder = Initials
settings-tile-reset = Reset to default
settings-tile-preview = Preview
# $color: a color name of the theme.
settings-tile-default-color = Default color: { $color }.
settings-tile-preset-color = Preset: { $color }.
# $rgb: e.g. #3b82f6.
settings-tile-custom-color-value = Custom color { $rgb }.
settings-tile-presets = Presets
settings-tile-presets-description = Colors of the theme; they follow Dark and Light.
settings-tile-custom = Custom color
settings-tile-custom-description = Any color, kept as picked in both themes. Starts from the current one.
settings-tile-label = Label
settings-tile-label-description = Up to { $max } characters on the tile; empty shows the initials. Also Rename… in the tile's menu.

## Default namespace, Prometheus, Helm

settings-group-default-namespace = Default namespace
settings-default-namespace-description = Namespace to open for each context. Empty uses the one from kubeconfig.
settings-prometheus-description = Usage history in the details and CPU / Memory in tables without metrics-server. Auto searches the cluster once and remembers what it found; Detect again searches anew. An address is http(s)://host:port or namespace/service:port (reached through the API server).
settings-prometheus-placeholder = http(s)://host:port or namespace/service:port
settings-prometheus-auto = Auto
settings-prometheus-unusual = { $address } (unusual namespace)
settings-prometheus-address = Address
settings-prometheus-off = Off
settings-prometheus-searching = Searching the cluster…
settings-prometheus-search-failed = Search failed: { $error }
settings-prometheus-not-searched = Not searched yet
# $age: e.g. 5m.
settings-prometheus-searched-none = Searched { $age } ago: no Prometheus found
# $found: addresses joined with commas.
settings-prometheus-searched-found = Searched { $age } ago: { $found }
settings-prometheus-status-off = Off: no search and no requests
# $endpoint: namespace/service:port or a URL.
settings-prometheus-status-ready = connected to { $endpoint }
settings-prometheus-status-unreachable = { $endpoint } doesn't answer
settings-prometheus-status-connecting = uses { $endpoint }
settings-prometheus-status-no-address = enter an address
settings-prometheus-status-invalid = not a valid address: { $address }
settings-prometheus-status-unconfirmed = waits for confirmation of { $endpoint }
settings-prometheus-unusual-warning = Found only in namespace { $namespace }, where Prometheus is not usually installed. Anyone who can create a Service there could show made-up usage. Use it only if you trust it.
settings-prometheus-use-it = Use it
settings-prometheus-use-it-tooltip = Use { $endpoint } for { $name }
# Screen reader label of the source picker; $label: the picked source.
settings-prometheus-source-label = Prometheus of { $name }: { $label }
settings-prometheus-detect-again = Detect again
settings-prometheus-detect-tooltip = Search the cluster for Prometheus servers
settings-group-helm-driver = Helm storage driver
settings-helm-driver-description = Where Helm keeps releases (HELM_DRIVER). Detect uses the storage that holds releases, else Secret.
settings-helm-driver-detect = Detect

## Removing a context

settings-group-remove = Remove
settings-remove-from-kubach = Remove from Kubach
# $contexts: context names joined with commas.
settings-remove-from-kubach-description = Deletes the imported copy from the vault. Its contexts disappear from Kubach: { $contexts }.
settings-remove-from-kubach-button = Remove from Kubach…
settings-unlink-file = Unlink file
settings-unlink-file-button = Unlink file…
settings-unlink-description-single = Kubach stops reading { $path }. The file is not changed.
settings-unlink-description =
    { $count ->
        [one] Kubach stops reading { $path }, so its { $count } context disappears: { $contexts }. The file is not changed.
       *[other] Kubach stops reading { $path }, so all { $count } of its contexts disappear: { $contexts }. The file is not changed.
    }
settings-hide = Hide
settings-hide-button = Hide…
settings-hidden = Hidden
# $origin: KUBECONFIG, ~/.kube/config or settings-origin-your-kubeconfig.
settings-hide-description = { $name } comes from { $origin }, which Kubach does not change. Remove it there with kubectl config delete-context, or hide it from the hotbar.
settings-hide-title = Hide { $name }?
settings-hide-message = The tile leaves the hotbar; the kubeconfig is not changed. Show it again from the hotbar's … menu or here.

## Security of a context

settings-profile-read-only = Read-only
settings-profile-debug = Debug
settings-profile-deploy = Deploy
settings-deploy-dry-run = Dry run only
settings-deploy-apply = Apply after approval
settings-exec-never = Never
settings-exec-single = Single commands
settings-exec-shell = Single commands and a shell
settings-namespaces-one = One
settings-namespaces-several = Several
settings-agent-secrets-none = None
settings-agent-secrets-metadata = Metadata and checks
settings-agent-secrets-reveal = Metadata, checks and reveal on request
settings-security-preset = Preset
settings-security-preset-strict = Strict: the defaults.
settings-security-preset-development = Development: everything allowed, each change confirmed, Secret values shown and revealed to the agent on request.
settings-security-preset-custom = Custom values.
settings-preset-strict = Strict
settings-preset-development = Development
settings-group-security = Security
settings-security-description-no-server = What an AI agent connected over MCP may do. New contexts are strict. No API server in kubeconfig.
settings-security-description = What an AI agent connected over MCP may do. New contexts are strict. Settings are bound to this name and server { $server }.
# $list: what is looser, joined with commas (hotbar-loosened-*).
settings-security-loosened = Looser than the defaults for { $name }: { $list }.
settings-security-relaxed = Relaxed security
settings-security-suggest-development = This looks like a local cluster. The Development preset is suggested; it is not applied automatically.
settings-group-secrets = Secrets
settings-hide-secret-values = Hide Secret values
settings-hide-secret-values-description = No Show, Copy or Load value for Secrets of this context, and no reveal in Helm and Kustomize. Values can still be replaced without loading them.
settings-group-agent-mcp = AI agent (MCP)
settings-agent-mcp-description = An agent starts with no rights. It names a namespace, and Kubach asks you the first time it reads it, reveals a Secret or changes something. These settings are the most it can ever get here.
settings-mcp-enabled = AI agent access
settings-mcp-enabled-description = Off: no agent session starts on this cluster.
# $time: e.g. 10:00:00; $changes: what was raised, as recorded in the settings file.
settings-ceiling-raised = Raised at an agent's request on { $time } by session { $session }: { $changes }.
settings-ceiling-raised-client = Raised at an agent's request on { $time } by session { $session } ({ $client }): { $changes }.
settings-ceiling-raised-earlier =
    { $count ->
        [one] ({ $count } earlier raise in the settings file)
       *[other] ({ $count } earlier raises in the settings file)
    }
settings-ceiling-raised-title = Ceiling raised from a session
settings-protected = Protected cluster
settings-protected-description = The agent cannot ask to raise the limits below from a session: such calls are refused with an explanation and you are not asked. Only this setting turns it off. For production.
settings-max-profile = Most the agent may ask for
settings-max-profile-description = Read-only: view resources. Debug: also logs and exec. Deploy: also changes. Above this the agent may ask you in Kubach to raise it, for one session or always, unless the cluster is protected.
settings-deploy-mode = Changes
settings-deploy-mode-description = With Deploy. Dry run: the agent sees the diff and nothing changes. Apply: each change runs after you approve it, including Secrets with values you type in Kubach.
settings-exec-mode = Commands in containers
settings-exec-mode-description = With Debug or Deploy. You approve every command.
settings-agent-secrets = Secrets for the agent
settings-agent-secrets-hidden-description = Names, keys, sizes and checks inside Kubach, never values. Revealing needs “Hide Secret values” off: the agent never gets a value you may not see.
settings-agent-secrets-description = Reveal asks you for each Secret and warns that its values go to the model's provider. Other tools always hide values.
settings-agent-other-kinds = Custom resources (CRDs)
settings-agent-other-kinds-description = Off: only built-in kinds such as Pods, Deployments, Services, ConfigMaps, Jobs and Ingresses. On: custom resources too, which may hold credentials or make a controller (Flux, Argo CD) act with its own rights. Secrets, ServiceAccounts and RBAC stay off-limits.
settings-namespaces = Namespaces per session
settings-namespaces-description = One: the first namespace you allow is the only one the session gets. Several: each new namespace is asked for.
settings-session-ttl = Session length
settings-session-ttl-description = A session and every right in it end after this, at most 8 h, or when Kubach quits. `kubach mcp` sessions last 30 minutes unless started with a longer --ttl.
settings-group-enforcement = Enforcement by the cluster
settings-enforcement-description = Kubach checks every agent call itself. These add a second check by the API server, so a mistake in Kubach cannot give the agent more than you allowed.
settings-temporary-objects = Separate identity for the agent
settings-temporary-objects-description = In each namespace you let the agent read, Kubach creates a ServiceAccount whose Role holds exactly what you allowed, and removes it when the session ends. Needs admin rights in the namespace.
settings-require-server-enforced = Require it
settings-require-server-enforced-description = Refuse a namespace where the cluster cannot enforce the limits. Off: such a namespace is marked “limits only in Kubach” and every change in it is approved one by one.

## Presets page

settings-preset-name-placeholder = Preset name
settings-presets-none = No presets yet
settings-presets-none-description = Open a manifest in the YAML editor and use “Save as preset”. Presets are offered by “+” on the resources page, grouped by kind.
# $api_version: e.g. apps/v1; $kind: e.g. Deployment.
settings-presets-kind-description = { $api_version }. The default preset is what “+” opens on the { $kind } page.
settings-preset-default = Default
settings-preset-delete = Delete preset
settings-preset-lines =
    { $lines ->
        [one] { $lines } line of YAML.
       *[other] { $lines } lines of YAML.
    }
settings-preset-lines-default =
    { $lines ->
        [one] { $lines } line of YAML. Opened by “+” on the { $kind } page.
       *[other] { $lines } lines of YAML. Opened by “+” on the { $kind } page.
    }
