# The vault of imported kubeconfigs: adding a cluster, the import preview,
# unlocking, changing the password, reset, remove, rename and export, the
# "Add manually…" form, and errors of kubach-core/src/vault. Names of
# entries, contexts, files and servers, and reasons worded by the OS are
# data. $method is "Touch ID", "Windows Hello" or "biometrics".
# kubeconfig, Kubernetes, exec, OIDC, PEM, CA, PATH, YAML, EKS/GKE/AKS and
# command lines stay in English.

## Adding a cluster

vault-choose = Choose
vault-link-text = Read in place: updates by aws, gcloud or other tools are picked up. Credentials stay in your file.
vault-import-text = Copied into Kubach's encrypted vault, unlocked with a password or the system keychain. For files you'd otherwise keep lying around.
vault-add-title = Add cluster
vault-add-link = Link kubeconfig file…
vault-add-import = Import kubeconfig file…
vault-add-paste = Paste kubeconfig from clipboard
vault-add-paste-text = Same as import, from YAML you copied (e.g. from a cloud console).
vault-add-manual = Add manually…
vault-add-manual-text = Enter the API server and credentials (token, client certificate or exec plugin), or write the kubeconfig YAML. Stored in the encrypted vault.
# Files dropped onto the window; $count is how many.
vault-dropped-title =
    { $count ->
        [one] Add kubeconfig file
       *[other] Add kubeconfig files
    }
vault-dropped-link =
    { $count ->
        [one] Link file
       *[other] Link files
    }
vault-dropped-import =
    { $count ->
        [one] Import file
       *[other] Import files
    }
vault-dropped-import-several = Each file is copied into Kubach's encrypted vault as its own entry, with a preview for each one in turn.
vault-clipboard-empty = The clipboard has no text. Copy the kubeconfig YAML first.

## Import preview

vault-import-title = Import kubeconfig
vault-update-title = Update imported kubeconfig
# Default name of an entry imported from the clipboard.
vault-pasted-name = Pasted kubeconfig
vault-name = Name
vault-password = Vault password
vault-password-repeat = Repeat the password
vault-context-name = Context name
vault-confirm-read-files = Confirm reading the local files first
vault-no-contexts = The kubeconfig has no contexts
vault-fix-names = Fix the context names first
vault-enter-name = Enter a name
vault-choose-password = Choose a password for the vault
vault-passwords-differ = The passwords don't match
vault-no-settings-folder = The OS has no settings folder for the vault
vault-imported =
    { $count ->
        [one] { $count } context in the encrypted vault
       *[other] { $count } contexts in the encrypted vault
    }
vault-renamed-taken = Renamed: "{ $original }" is already used
vault-renamed = Renamed from "{ $original }"
vault-adds = Adds { $contexts ->
        [one] { $contexts } context
       *[other] { $contexts } contexts
    }, { $clusters ->
        [one] { $clusters } cluster
       *[other] { $clusters } clusters
    } and { $users ->
        [one] { $users } user
       *[other] { $users } users
    }.
vault-new-note = Imported kubeconfigs are stored encrypted. Choose the vault password; Kubach asks for it once per session.
vault-remember = Remember in the system keychain (no password needed)
vault-locked-biometric = The vault is locked. Enter its password, or leave it empty to use { $method }.
vault-locked-keychain = The vault is locked. Enter its password, or leave it empty to use the system keychain.
vault-locked-password = The vault is locked. Enter its password to add this config.
vault-locked-keychain-only = The vault is locked; it unlocks with the system keychain.
# $error says what is wrong with the vault file.
vault-broken = { $error } Reset the vault in Settings → Clusters to import again.
vault-source-manual = Entered manually
vault-source-clipboard = From the clipboard
# $source is a path or vault-source-*.
vault-source-queue = File { $current } of { $total } · { $source }
vault-read-files = Read these files into the vault
vault-back = Back
vault-cancel = Cancel
vault-skip = Skip
vault-skip-tooltip = Go to the next file without importing this one
vault-encrypting = Encrypting…
vault-update = Update
vault-import = Import
vault-import-unreadable-file = Can't read { $path } ({ $error }); the imported config keeps referring to it.
vault-name-empty = Enter a context name
vault-name-duplicate = Another context in this import has the same name
vault-name-taken = A context with this name already exists

## Unlocking

vault-unlock-title = Unlock imported clusters
vault-unlock-text = Imported kubeconfigs are encrypted. Unlocking keeps them in memory until you lock the vault, stay idle, or quit.
vault-unlock-waiting = Confirm with { $method }…
vault-use-password = Use password
vault-use-biometric = Use { $method }
vault-use-keychain = Use system keychain
vault-unlocking = Unlocking…
vault-unlock = Unlock
vault-enter-password = Enter the password
vault-busy = The vault is busy; try again in a moment
vault-wrong-password-wait = Wrong password. Try again in { $seconds } s.
vault-too-many-attempts = Too many attempts. Try again in { $seconds } s.
vault-missing = There is no vault yet
vault-closing = Kubach is closing
vault-locked = The vault is locked

## Password, reset, remove, rename, export

vault-password-title = Change vault password
vault-new-password = New password
vault-new-password-repeat = Repeat the new password
vault-enter-new-password = Enter a password
vault-password-changed = Vault password changed
vault-password-text = Everything in the vault is encrypted again with a new key.
vault-change = Change
vault-reset-title = Reset the vault?
vault-reset-description =
    { $count ->
        [one] The encrypted vault and its { $count } imported kubeconfig are deleted, and their clusters disappear from Kubach, along with copies of damaged vault files (vault.kubach.corrupt-…) and its keys in the system keychain. This can't be undone.
       *[other] The encrypted vault and its { $count } imported kubeconfigs are deleted, and their clusters disappear from Kubach, along with copies of damaged vault files (vault.kubach.corrupt-…) and its keys in the system keychain. This can't be undone.
    }
vault-reset-ok = Delete vault
vault-reset-keys-left = The vault is deleted, but its key may be left in the system keychain: the vault file was unreadable or the keychain refused to delete it. Search the keychain (Keychain Access, Credential Manager, Passwords and Keys) for "{ $keychain }" and delete the "{ $keychain }" and "{ $biometric }-…" entries.
vault-remove-title = Remove { $name }?
vault-remove-description = Its contexts ({ $contexts }) disappear from Kubach. The vault keeps no other copy.
vault-remove = Remove
vault-rename-title = Rename imported kubeconfig
vault-export-title = Export as plain kubeconfig?
vault-export-description = The exported file is NOT encrypted: anyone who can read it gets the credentials. Kubach writes it readable by you only (0600); keep it out of backups and shared folders.
vault-export-ok = Export…
vault-export-locked = The vault got locked; unlock it and export again.
vault-exported = Exported to { $path }
vault-export-failed = Can't write { $path }: { $error }

## Errors of the vault (kubach-core)

vault-error-wrong-password = Wrong password
vault-error-corrupted = The vault file is damaged ({ $reason }).
vault-error-corrupted-backup = The vault file is damaged ({ $reason }). A copy was saved to { $path }.
vault-error-newer-format = This vault was written by a newer Kubach (format { $version }); update Kubach to open it
vault-error-no-password = This vault has no password; unlock it with the system keychain
vault-error-no-keychain = This vault is not stored in the system keychain
vault-error-keychain-missing = The vault key is not in the system keychain; unlock with the password
vault-error-keychain-mismatch = The key in the system keychain doesn't open this vault; unlock with the password
vault-error-no-key = A vault needs a password, the system keychain or both
vault-error-no-biometric = { $method } unlock isn't turned on for this vault
vault-error-biometric-needs-password = { $method } unlock needs a vault password to fall back on; set one first
vault-error-biometric-mismatch = The { $method } key doesn't open this vault; unlock with the password
vault-error-biometric-cancelled = { $method } was cancelled
vault-error-biometric-invalidated = { $method } changed since the vault was set up for it (fingerprints added or removed), so its key no longer works. Enter the password, then turn { $method } unlock on again in Settings.
vault-error-biometric-failed = { $method } failed: { $reason }
# A reason already worded by the OS or the keychain.
vault-error-text = { $text }
vault-error-empty-password = The password is empty
vault-error-exists = A vault already exists at { $path }
vault-error-unknown-entry = No imported config with id { $id }
vault-error-serialize = Can't store the config: { $error }
vault-error-context-names = Context names must be non-empty, unique and not used by other contexts
vault-error-unread-files = Confirm reading the local files the config refers to first
vault-error-io = Can't access the vault file: { $error }

## "Add manually…"

manual-title = Add cluster manually
manual-auth-token = Token
manual-auth-cert = Client certificate
manual-auth-basic = Password
manual-auth-oidc = OIDC
manual-auth-exec = Exec plugin
manual-context-placeholder = e.g. prod-eu
manual-token-placeholder = Bearer token
manual-password-placeholder = Password
manual-client-secret-placeholder = Only for confidential clients
manual-region-placeholder = eu-west-1 (default: the profile's region)
# $why is manual-yaml-*.
manual-yaml-not-shown = { $why }. The form keeps its own values; the YAML tab still has yours. The open tab is what gets tested and saved.
manual-write-yaml = Write the kubeconfig YAML first
manual-connecting = Connecting to { $server }…
manual-connected = Connected: Kubernetes { $version }
manual-test-stopped = The test stopped unexpectedly
manual-load-file = Load from file…
manual-ca-custom = CA certificate
manual-ca-system = System trust store
manual-ca-insecure = Skip verification
manual-ca-pem = CA certificate (PEM)
manual-ca-system-note = For servers with a certificate from a public authority (most managed clusters behind a load balancer don't have one).
manual-insecure-warning = The server's certificate is not checked: anyone between you and the cluster can pose as it and read the credentials. Use only for local test clusters.
manual-token = Token
manual-cert-pem = Client certificate (PEM)
manual-key-pem = Client key (PEM)
manual-username = User name
manual-password = Password
manual-basic-note = HTTP basic auth. Kubernetes 1.19 removed it from the API server; it works only with older clusters or behind an authenticating proxy.
manual-issuer = Issuer URL
manual-client-id = Client ID
manual-client-secret = Client secret (optional)
manual-scopes = Extra scopes (optional, space-separated)
manual-oidc-install = Install kubelogin: kubectl krew install oidc-login (or brew install int128/kubelogin/kubelogin).
manual-oidc-note = Runs kubelogin (kubectl oidc-login get-token) as an exec plugin: it opens the browser to sign in, then caches and refreshes the tokens.
manual-context = Context name
manual-server = API server URL
manual-namespace = Default namespace (optional)
manual-server-certificate = Server certificate
manual-sign-in = Sign in with
# $names: programs not found, $count how many, $hint: what to do.
manual-missing =
    { $count ->
        [one] { $names } was not found in PATH. { $hint }
       *[other] { $names } were not found in PATH. { $hint }
    }
manual-missing-shell =
    { $count ->
        [one] { $names } was not found in PATH. { $hint } The login shell environment was not loaded ({ $error }), so PATH may be incomplete.
       *[other] { $names } were not found in PATH. { $hint } The login shell environment was not loaded ({ $error }), so PATH may be incomplete.
    }
manual-template-custom = Custom
manual-command = Command
manual-args = Arguments
manual-env = Environment (NAME=value, space-separated)
manual-api-version = API version
manual-eks-cluster = EKS cluster name
manual-region = Region (optional)
manual-profile = AWS profile (optional)
manual-gke-note = Uses your gcloud sign-in (gcloud auth login). The API server URL and CA certificate are in gcloud container clusters describe <cluster>.
manual-server-id = Server ID
manual-aks-note = Signs in with the Azure CLI (az login). The server ID is the same for every cluster with AKS-managed Entra ID.
manual-runs = Runs: { $line }
manual-install-custom = Install it, or enter its full path.
manual-install-eks = Install the AWS CLI v2 and sign in (aws configure / aws sso login).
manual-install-gke = Install it with: gcloud components install gke-gcloud-auth-plugin, then gcloud auth login.
manual-install-aks = Install kubelogin (az aks install-cli) and sign in with az login.
manual-install-yc = Install the Yandex Cloud CLI and sign in with yc init.
manual-yc-note = Signs in with the yc CLI (yc init); an empty profile means the active one. The API server URL and CA certificate are in yc managed-kubernetes cluster get <cluster>.
manual-exec-note = The command runs on this computer when Kubach connects, with the PATH of your login shell.
manual-template = Template
manual-yaml-note = A complete kubeconfig; certificates and keys inline (*-data). Several contexts are fine; the test uses current-context. Exec plugins in it run when you test.
manual-tab-form = Form
manual-testing = Testing…
manual-test = Test connection
manual-next = Review and save…
manual-file-unreadable = Can't read { $name }: { $error }
manual-file-too-large = { $name } is too large for a certificate or key
manual-file-not-pem = { $name } is not a PEM text file (DER certificates aren't supported)

## Problems of the form (kubach-core/src/vault/manual.rs)

manual-problem-empty-context = Enter a context name
manual-problem-empty-server = Enter the API server URL
manual-problem-invalid-server = Not a valid server URL: { $error }
manual-problem-empty-ca = Paste the CA certificate (PEM) or choose its file
manual-problem-invalid-ca = The CA is not a PEM certificate (-----BEGIN CERTIFICATE-----)
manual-problem-empty-token = Enter the token
manual-problem-invalid-cert = The client certificate is not PEM (-----BEGIN CERTIFICATE-----)
manual-problem-invalid-key = The client key is not a PEM private key (-----BEGIN … PRIVATE KEY-----)
manual-problem-empty-username = Enter the user name
manual-problem-empty-password = Enter the password
manual-problem-empty-issuer = Enter the issuer URL
manual-problem-invalid-issuer = Not a valid issuer URL: { $error }
manual-problem-empty-client-id = Enter the client ID
manual-problem-empty-cluster-name = Enter the cluster name
manual-problem-empty-server-id = Enter the server ID
manual-problem-empty-command = Enter the plugin command
manual-problem-trailing-backslash = A backslash at the end escapes nothing
# $quote is ' or ".
manual-problem-unclosed-quote = Unclosed { $quote } quote
manual-problem-invalid-env = Environment variables are NAME=value: "{ $entry }"
manual-yaml-invalid = Not a valid kubeconfig: { $error }
manual-yaml-contexts = The form shows one context; this config has { $count }
manual-yaml-missing-cluster = The context's cluster is not in the config
# $feature is manual-feature-*.
manual-yaml-unsupported = The config uses { $feature }, which the form can't show
manual-feature-ca-file = a CA file path
manual-feature-proxy = a proxy
manual-feature-tls-server-name = tls-server-name
manual-feature-basic-with-others = basic auth with other credentials
manual-feature-auth-provider = an auth provider
manual-feature-credential-files = credential file paths
manual-feature-impersonation = impersonation
