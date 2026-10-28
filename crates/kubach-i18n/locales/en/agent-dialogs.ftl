# Questions an AI agent's MCP session asks in the Kubach window: approving
# a change, a right, a Helm release, a context or a session, the Secret
# form, and notices. The request's summary, details, rights and limits
# come from kubach-mcp in English for the agent and the audit: they are
# data here, like names, namespaces, keys, tools and session ids. Secret,
# Helm, release, chart, MCP, Pod, PATH stay in English.

## Notices

agent-security-settings = Security settings
agent-notice-reveal-off = Revealing Secrets is off
agent-notice-refused = Agent refused
# What the agent asked, in English.
agent-notice-text = { $text }
# $summary is what the agent asked, in English.
agent-notice-mcp-off = { $summary }: AI agent access is off for this context.
agent-notice-session-only = Allowed for this session only
agent-notice-session-only-text = The settings file cannot be written, so the access holds only until the session ends.

## Dialog titles

agent-title-read = Allow the agent to read?
agent-title-reveal = Reveal a Secret to the agent?
agent-title-modify = Allow the agent to make changes?
agent-title-exec = Run the agent's command?
agent-title-access = Give the agent more access?
agent-title-helm-install = Install the agent's Helm release?
agent-title-helm-upgrade = Upgrade the agent's Helm release?
agent-title-helm-rollback = Roll back the Helm release?
agent-title-helm-uninstall = Uninstall the Helm release?
agent-title-chart-source = Allow a chart source?
agent-title-use-context = Let the agent work in this context?
agent-title-call = Approve the agent's change?
agent-title-secret-create = Enter values for a new Secret
agent-title-secret-update = Change a Secret
agent-title-secret-copy = Copy a Secret for the agent?
agent-title-secret-delete = Delete a Secret for the agent?

## Confirmation

# An empty argument of a command.
agent-arg-empty = (empty)
# ␣, \n and \u{…} are drawn as they are.
agent-command-header =
    { $count ->
        [one] { $count } argument, { $bytes } bytes · ␣ marks a space, \n a line break, \u{"{"}…{"}"} an invisible character
       *[other] { $count } arguments, { $bytes } bytes · ␣ marks a space, \n a line break, \u{"{"}…{"}"} an invisible character
    }
agent-no-longer-pending = This request is no longer pending.
agent-dangerous-title = Escapes the namespace boundary or reads Secrets
# $message is what a finding does, $path where; both in English.
agent-dangerous-line = { $message } — { $path }
agent-dangerous-line-document = { $message } — document { $document }: { $path }
agent-acknowledge = I understand that this lets the workload escape the namespace boundary or read these Secrets, and approve it anyway
agent-secret-leaves = The value leaves your machine
agent-secret-no-take-back = Kubach cannot take it back once the agent has it.
agent-deny = Deny
agent-reject = Reject
agent-approve = Approve
agent-allow-always = Allow by default
agent-allow-session = Allow for this session
agent-helm-install = Install
agent-helm-upgrade = Upgrade
agent-helm-rollback = Roll back
agent-helm-uninstall = Uninstall
agent-dont-reveal = Don't reveal
agent-reveal = Reveal to the agent
agent-keep-secret = Keep it
agent-delete-secret = Delete Secret
agent-copy-secret = Copy Secret
agent-field-agent = Agent
agent-client-unverified = name reported by the client, not verified
agent-field-context = Context
agent-field-namespace = Namespace
agent-field-right = Right
agent-field-release = Release
agent-field-chart = Chart
agent-field-version = Version
agent-field-revision = Revision
agent-field-runs-as = Runs as
agent-field-asks-for = Asks for
agent-field-access-now = Has now
agent-access-details = Allowing adds this until the session ends. With “Allow by default” the minimal access of { $context } is raised in Settings → Security for good. Either way every change, deletion, Helm action and command is still approved one by one, and nothing exceeds your own Kubernetes rights.
agent-access-secrets = Secrets (metadata and checks)
agent-access-crds = custom resources
agent-field-tool = Tool
agent-field-session = Session
agent-helm-values = Values (sensitive keys masked)
agent-helm-objects = Objects
agent-always = Allow by default
agent-always-note = Raises the minimal access in Settings → Security of { $context } for good, with a note of when and from which session. “Reset to read” there takes it back.
agent-limits-off = Server-side limits are off
agent-limits-off-note = The call runs with your own credentials; only Kubach checks it.
agent-client-only = Boundary only in Kubach: the call runs with your own credentials.
# $left is a countdown like 4:59.
agent-auto-reject = Rejected automatically in { $left }
agent-auto-reject-queued = Rejected automatically in { $left } · { $queued } more waiting

## Secret form

agent-writes-to = Writes to
agent-secret = Secret
agent-copied-from = Copied from
agent-secret-type = Type
agent-secret-keys = Keys
agent-secret-unverified = The name and keys come from the agent and are not verified.
agent-explanation = The agent's explanation, not verified
agent-still-referenced = Still referenced: new pods of these will fail to start
agent-used-by = Used by (restart them to pick up new values)
agent-placeholder-user = user
agent-placeholder-password = password or token
agent-placeholder-optional = optional
agent-placeholder-key = key
agent-placeholder-value = value
agent-enter-value = Enter a value for { $key }, generate one or leave the key out
agent-enter-value-new = Enter a value for the new key, generate one or leave the key out
agent-key-exists = Key { $key } already exists; change it in its row
agent-key-twice = Key { $key } is set twice
agent-choose-tls = Choose the certificate and the private key
agent-new-key = new key
# $size is like "1.2 KiB".
agent-value-unchanged = unchanged, { $size }
agent-value-type = type a value
agent-value-typed =
    { $count ->
        [one] typed, { $count } character
       *[other] typed, { $count } characters
    }
agent-value-generated =
    { $count ->
        [one] generated, { $count } character, hidden
       *[other] generated, { $count } characters, hidden
    }
agent-value-file = from { $name } ({ $size }), not shown
agent-value-removed = will be removed
agent-badge-asked = asked by the agent
agent-badge-remove = agent suggests removing
agent-keep-key = Keep this key
agent-keep = Keep
agent-type-new = Type a new value
agent-type = Type
agent-generate-tooltip = Generate a random value; nobody sees it
agent-generate = Generate
agent-copy-generated = Copy the generated value; the clipboard is cleared in 30 s
agent-from-file = Take the value from a file
agent-keep-current = Keep the current value
agent-remove-key = Remove this key
agent-leave-out = Leave this key out
agent-add-key = Add key
agent-tls-cert = Certificate (.crt)
agent-tls-key = Private key (.key)
agent-registry = Registry
agent-user = User
agent-password = Password
agent-email = Email
agent-hint-tls = Choose the PEM certificate and its private key; Kubach checks that they belong together.
agent-hint-docker = Kubach builds .dockerconfigjson from these fields. An empty registry means Docker Hub.
agent-hint-update = Keys you leave unchanged are not sent. Type, generate or pick a file for the keys to replace.
agent-hint-keys = Type each value, generate it or take it from a file.
agent-create-secret = Create Secret
agent-apply = Apply changes
agent-client = Agent: { $client }
agent-client-unverified-note = (name reported by the client, not verified)
agent-secret-info = Kubach writes these values; the agent gets only the keys and sizes. Enter only values meant for this context and namespace.
# $warning is why the server-side limits are off, in English.
agent-secret-limits = { $warning }. The write runs with your own credentials.
agent-auto-cancel = Cancelled automatically in { $left }
agent-cancel = Cancel
agent-file-unreadable = Can't read { $name }: { $error }
agent-file-too-large = { $name } is larger than 1 MiB, the limit of a whole Secret
agent-cert-valid = { $subject }, valid until { $time }
agent-cert-none = no certificate
agent-key-none = No PEM private key in this file
agent-key-loaded = Private key loaded, not shown
agent-key-matches = Belongs to the certificate
agent-key-differs = Does not belong to the certificate
agent-key-unknown = Kubach can't tell whether it belongs to the certificate

## Starting a session

agent-use-context-summary = { $client } wants to work in context { $context }
# $access: the minimal access, e.g. "Read".
agent-use-context-details = A yes starts an agent session on { $context } until Kubach quits, at most 8 h. Without asking it has: { $access }. Anything more is asked for here.
agent-consent-summary = Start an AI agent session on `{ $context }` with the credentials from the Kubach vault?
agent-consent-session = new, for pid { $pid }
agent-consent-tool = start a session
agent-consent-namespaces = Namespaces: { $namespaces }
agent-consent-namespaces-any = any the agent names
agent-consent-access = Without asking: { $access }; more is asked for here
agent-consent-time = Time: { $time }, or until the vault locks
agent-consent-reading = Reading: { $reading }
agent-consent-reading-granted = allowed from the start (--grant-read)
agent-consent-reading-asked = within the minimal access
agent-consent-process = Process: { $process }
agent-consent-unknown-process = pid { $pid } (the OS does not show this process)
agent-consent-warning = The pid is reported by the process itself. If you did not just start an agent for this context, reject.

