# Editors: the YAML editor with its review and apply, the ConfigMap keys
# view, the Secret editor and values, presets and templates for new
# resources, the "New namespace" dialog. Kinds, names, contexts, field
# paths and messages from the cluster are data: keep them as they are.

## Shared

editor-loading = Loading…
editor-reload = Reload
editor-review = Review changes
editor-apply = Apply
editor-create = Create
editor-creating = Creating…
editor-save = Save
editor-format = Format
editor-format-tooltip = Indent the JSON
editor-no-keys = No keys
editor-add-key = Add key
editor-remove-key = Remove this key
editor-key-placeholder = key
editor-value-placeholder = value
# Status of a key in the keys view.
editor-key-new = new
editor-key-changed = changed
editor-key-removed = removed
editor-key-unchanged = not changed
editor-key-renamed = renamed from { $key }
editor-key-twice = Key { $key } is used twice
# Kind of a value, by its content (JSON and YAML stay as they are).
editor-content-properties = Properties
editor-content-text = Text
editor-content-binary = Binary
editor-value-warning = { $problem }. It can still be saved: the value is plain text.
editor-size-bytes =
    { $count ->
        [one] { $count } byte
       *[other] { $count } bytes
    }
# $size: "12 bytes" or "1.5 KiB".
editor-binary-data = binary data, { $size }
editor-file-unreadable = Can't read { $name }: { $error }
editor-file-too-large-configmap = { $name } is larger than 1 MiB, the limit of a whole ConfigMap
editor-file-too-large-secret = { $name } is larger than 1 MiB, the limit of a whole Secret
editor-field-name = Name
editor-field-type = Type
editor-field-keys = Keys
editor-field-user = User
editor-field-password = Password
editor-field-email = Email
editor-field-server = Server
editor-field-certificate = Certificate
editor-field-private-key = Private key

## YAML editor: buttons of a resource

editor-edit-yaml = Edit YAML
editor-edit-values = Edit values
editor-edit-values-tooltip = Edit keys and values; only changed keys are sent
editor-secret-immutable-tooltip = This Secret is immutable: its data can't be changed
editor-secret-not-yaml = Secrets are edited in the Secret editor, not as YAML
editor-kind-not-patchable = This kind can't be patched
editor-new-secret = New Secret
editor-new-secret-tooltip = Opaque, docker registry or TLS Secret from a form

## YAML editor: titles and status

editor-title-new = New resources
editor-tab-new = New resource
editor-tab-new-kind = New { $kind }
# Comment line of the example shown in an empty editor.
editor-placeholder-more = more documents…
editor-subtitle-edit = Edit · { $context }
editor-subtitle-create = Create · { $context } · default namespace { $namespace }
editor-checking = Checking…
editor-errors-warnings =
    { $errors ->
        [one] { $errors } error
       *[other] { $errors } errors
    }, { $warnings ->
        [one] { $warnings } warning
       *[other] { $warnings } warnings
    }
editor-warnings =
    { $count ->
        [one] { $count } warning
       *[other] { $count } warnings
    }
editor-loading-schema = Loading schema…
editor-schema-unavailable = Schema unavailable: { $error }
editor-empty = Empty
editor-no-problems = No problems
# A problem's line in the list under the editor.
editor-line = Ln { $line }

## YAML editor: toolbar

editor-mode-keys = Keys
editor-mode-keys-tooltip = Each key in its own editor, JSON checked
editor-mode-yaml-tooltip = The whole object as YAML
editor-reload-tooltip = Discard edits and load the current version
editor-save-preset = Save as preset
editor-save-preset-tooltip = Keep this YAML as a preset for new resources of its kind
editor-review-tooltip = Server-side dry run, then a diff to confirm (⌘S)
editor-force-apply = Force apply
editor-back = Back to editor
editor-review-force = Review with force…
editor-review-force-tooltip = Dry-run again, taking over the conflicting fields

## YAML editor: banners

editor-disconnected = { $context } is disconnected; connect it again to review or apply.
editor-load-failed = Failed to load the object: { $error }
editor-discovery-pending = API discovery had not finished when this editor opened; reopen it to apply.
editor-nothing-to-apply = Nothing to apply.
editor-some-not-applied = Some objects were not applied; see below.
# $objects: the list of results below, joined with commas.
editor-applied = Applied: { $objects }
editor-applied-created = { $label } created
editor-applied-unchanged = { $label } unchanged
editor-applied-configured = { $label } configured
editor-context-closed = Disconnected from { $context }. This tab belongs to { $context }; connect it again (hotbar or sidebar) to review or apply. Edits are kept.
editor-context-foreign = This tab belongs to { $context }. Review and Apply go to { $context }, not to the cluster selected in the sidebar.

## YAML editor: checks of the text

editor-resolve-error = Document { $document }, line { $line }: { $message }
editor-missing-field = `{ $field }` is missing
editor-unknown-kind = The cluster does not serve { $kind } in { $version }
editor-missing-name = `metadata.name` is missing (generateName is not supported by apply)
editor-cluster-scoped = { $kind } is cluster-scoped but has namespace “{ $namespace }”
editor-problem-conflict = { $field }: conflict, managed by “{ $manager }”
# Completion detail of a required field; $schema_type is the type from the schema.
editor-field-required = { $schema_type } (required)
# Hover over a field with a fixed set of values.
editor-one-of = One of: { $values }

## YAML editor: review

editor-value-saved-anyway = { $warning }. It is saved as text anyway.
editor-force-warning = Force apply takes ownership of the conflicting fields from their current managers. Controllers, Helm or GitOps tools that manage them may revert this change or keep overwriting it.
editor-no-changes = No changes
editor-no-changes-description = The cluster already matches this manifest.
editor-not-applied = Not applied
editor-entry-create = create
editor-entry-no-changes = no changes
editor-entry-changes =
    { $count ->
        [one] { $count } change
       *[other] { $count } changes
    }
editor-entry-failed = failed
editor-retained = Removed but still present after apply: { $fields }. Server-side apply only removes fields Kubach alone manages; these are also managed by another field manager (or defaulted by the server).
editor-reload-stale = Reload (discards edits)
editor-failure-conflicts =
    { $count ->
        [one] { $label }: { $count } field is managed by other field managers
       *[other] { $label }: { $count } fields are managed by other field managers
    }
editor-failure-stale = { $label } changed in the cluster since it was loaded (resourceVersion mismatch). Reload it to edit the current version.

## Editor tabs

editor-close-title = Close the tab and discard the edits?
editor-close-detail = { $tab } has edits that were not applied.
editor-discard-edits = Discard edits

## ConfigMap keys

editor-keys-unavailable = Can't edit by keys: { $error }. Fix it in YAML.
editor-configmap-immutable = This ConfigMap is immutable (immutable: true): Kubernetes refuses changes to its data. To change it, delete it and create a new one.
editor-configmap-hint = Each key is a block: JSON and YAML values are checked, JSON can be formatted. A value that fails its check is still saved as text. Review shows the server's dry run before anything is applied.
editor-binary-from-file = { $size }, from file { $file }
editor-binary-not-text = { $size }; binary values can't be edited as text
editor-replace-from-file = Replace from file

## Templates and presets

editor-new = New
editor-new-tooltip = New resource from YAML
editor-new-kind-tooltip = New { $kind } from its template
editor-templates-tooltip = Templates and presets
editor-menu-empty = Empty
editor-menu-builtin = { $label } (built-in)
editor-menu-skeleton = Skeleton from schema
editor-menu-default = { $name } (default)
# Suggested name of a new preset.
editor-preset-suggested = My { $kind }
editor-preset-placeholder = Preset name
# $name and $namespace are the placeholders as written in a preset.
editor-preset-hint = The name and namespace become { $name } and { $namespace }.
editor-preset-name-first = Name the preset first.
editor-preset-saved = Saved preset “{ $name }” for { $kind }. It is listed by “+” on the resources page; rename it or make it the default in Settings → Presets.
editor-preset-secret = Secrets are not saved as presets: settings are stored as plain text.
editor-preset-no-kind = Can't save a preset: the first document needs apiVersion and kind.

## Secret editor

editor-secret-new-in = New Secret in { $namespace }
editor-secret-kind-docker = Docker registry
editor-secret-load-failed = Failed to load the Secret: { $error }
editor-secret-new-key-placeholder = new key
editor-secret-key-gone-reload = Key { $key } is no longer in the Secret; reload it.
editor-secret-immutable-error = This Secret is immutable; its data can't be changed.
editor-secret-not-loaded = The Secret is not loaded yet.
editor-no-changes-to-apply = No changes to apply.
editor-secret-choose-cert = Choose the certificate file
editor-secret-choose-key = Choose the private key file
editor-secret-created = Secret created.
editor-changes-applied = Changes applied.
editor-subtitle-edit-values = Edit values · { $context }
editor-subtitle-create-secret = Create · { $context }
editor-secret-reload-tooltip = Discard edits and load the keys again
editor-back-to-editing = Back to editing
editor-secret-hidden = Secret values are hidden on this context by its security settings.
editor-secret-hidden-load = { editor-secret-hidden } Current values can't be loaded; you can still replace a value without loading it, add or remove keys.
editor-secret-immutable-notice = This Secret is immutable (immutable: true): Kubernetes refuses any change to its data. To change it, delete it and create a new one.
editor-secret-review-patch = Only these keys are sent (a merge patch on data); other keys are not touched. Values are never shown in the review.
editor-secret-review-create = A new Secret of type { $schema_type } is created with these keys. Values are never shown in the review.
editor-secret-changes-to = Changes to { $title }
# What the review shows in place of a value.
editor-secret-hidden-added = <hidden, added>
editor-secret-hidden-changed = <hidden, changed>
editor-secret-removed = <removed>
editor-secret-load-value = Load value
editor-secret-load-value-tooltip = Fetch the current value to edit it
editor-secret-replace = Replace
editor-secret-replace-tooltip = Type a new value without loading the current one
editor-secret-rename-tooltip = Rename this key
editor-secret-rename-disabled = Load or replace the value to rename the key: the new key is set with it
editor-secret-undo-tooltip = Keep the current value and name
editor-secret-file-tooltip = Take the value from a file
editor-secret-keep = Keep
editor-secret-from-file = From file { $name } ({ $size }); the content is not shown
# $binary: "binary data, 3 bytes".
editor-secret-binary = { $binary }: can't be edited as text; take the new value from a file
editor-secret-cert-file = { $file } — { $subject }, until { $until }
editor-secret-cert-not-pem = { $file } — not a PEM certificate
editor-secret-key-file = { $file } — loaded, not shown
editor-secret-hint-opaque = Values are typed as plain text or taken from files; Kubach encodes them.
editor-secret-hint-docker = Creates a kubernetes.io/dockerconfigjson Secret. An empty server means Docker Hub.
editor-secret-hint-tls = Creates a kubernetes.io/tls Secret from PEM files (tls.crt, tls.key).
editor-secret-user-placeholder = user
editor-secret-password-placeholder = password or token
editor-secret-optional = optional
editor-secret-edit-hint = Keys you don't touch are not sent. Load a value to edit it, or replace it without loading. Values are typed as plain text; Kubach encodes them.

## Secret values (details panel)

editor-secret-key-gone = Key { $key } is no longer in the Secret
editor-secret-checking-access = Checking access…
# $message: why the cluster refuses (RBAC).
editor-secret-denied = { $message } Values can't be shown.
editor-secret-data = Data
editor-secret-immutable = Immutable
editor-secret-immutable-yes = yes — data can't be changed
editor-secret-hidden-keys = { editor-secret-hidden } Keys can still be changed without loading.
editor-secret-security-settings = Security settings
editor-secret-show = Show
editor-secret-hide = Hide
editor-secret-copy-tooltip = Copy; cleared from the clipboard after { $seconds } s
editor-secret-empty = (empty)
editor-secret-ttl-note = Values are fetched from the cluster when shown and hidden again after { $reveal } s. A copied value is cleared from the clipboard after { $clipboard } s, when the vault locks or Kubach quits.
editor-secret-truncated = … (truncated; Copy takes the whole value)
editor-secret-summary-registry = Registry credentials
editor-secret-summary-tls = TLS certificate
editor-secret-summary-basic-auth = Basic authentication
editor-secret-summary-what-tls = Reads tls.crt only; the private key is never fetched.
editor-secret-summary-what-basic-auth = Reads the user name only, not the password.
editor-secret-summary-what-registry = Shows registries and user names, never passwords.
editor-secret-show-summary = Show summary
editor-secret-tls-key-hidden = tls.key — hidden
editor-secret-value-hidden = hidden
editor-secret-password-set = set (hidden)
editor-secret-password-none = none
editor-cert-not-yet-valid = not yet valid
editor-cert-expired =
    { $days ->
        [one] expired { $days } day ago
       *[other] expired { $days } days ago
    }
editor-cert-expires =
    { $days ->
        [one] expires in { $days } day
       *[other] expires in { $days } days
    }
editor-cert-valid =
    { $days ->
        [one] valid, { $days } day left
       *[other] valid, { $days } days left
    }
editor-cert-leaf = Leaf certificate
editor-cert-chain = Chain certificate { $index }
editor-cert-subject = Subject
editor-cert-issuer = Issuer
editor-cert-self-signed = self-signed
editor-cert-valid-from = Valid from
editor-cert-valid-until = Valid until
editor-cert-status = Status

## New namespace

editor-namespace-new = New namespace
editor-namespace-new-in = New namespace in { $context }
editor-namespace-forbidden = No permission (RBAC): { $message }
editor-namespace-name-placeholder = e.g. team-web
editor-namespace-labels-placeholder = Optional: team=web, env=dev
editor-namespace-not-connected = Not connected to a cluster
editor-namespace-created = Namespace { $name } created
editor-namespace-name-hint = Lowercase letters, digits and '-', up to 63 characters.
editor-namespace-labels = Labels
editor-namespace-labels-hint = key=value, separated by commas.
