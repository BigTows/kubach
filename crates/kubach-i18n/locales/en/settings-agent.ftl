# Settings → AI agent: the MCP HTTP server, connecting clients, active
# sessions and the audit log.

settings-copy = Copy
settings-copied = Copied to the clipboard

## HTTP server

settings-group-http-server = HTTP server
settings-http-server-description = Needed for every agent: Kubach asks you here when an agent wants a right or a change. HTTP agents also connect to it directly (127.0.0.1, with the token below).
settings-server-off = Off.
settings-server-starting = Starting…
settings-server-listening = Listening on { $url }
settings-server-not-running = Not running.
settings-mcp-http = Serve MCP over HTTP
settings-mcp-port = Port
settings-mcp-port-description = Fixed, so agent configs keep working after a restart.
settings-mcp-token = Token
# $token: the masked token, e.g. ••••••••3456.
settings-mcp-token-description = { $token } · kept in the settings folder, readable only by you
settings-mcp-token-none = Created when the server first starts.
settings-mcp-token-regenerate = Regenerate
settings-mcp-token-regenerate-tooltip = Agents configured with the old token stop working

## Connecting an agent

settings-group-connect = Connect an agent
settings-connect-description = Connect writes the server `kubach` (this app, `kubach mcp`, no token) into the agent's own config after you see the change. The agent then works on the startup context, names namespaces, and Kubach asks you here the first time it reads each.
settings-connect-unavailable = Kubach cannot tell its own path or your home folder; use manual setup.
settings-manual-setup = Manual setup
settings-manual-setup-description = For other clients, or to connect over HTTP.
settings-manual-show = Show
settings-manual-hide = Hide
settings-snippet-stdio = Any client, stdio
settings-snippet-stdio-description = Most clients take this under their MCP servers. The agent starts `kubach mcp` itself; no token is needed.
settings-snippet-claude-http = Claude Code over HTTP
settings-snippet-claude-http-description = The agent picks the context with use_context, and Kubach asks you. The token stays in its file: put the export line in your shell profile so Claude Code gets it. Never add it with --scope project: .mcp.json is committed with the repository.
settings-client-not-installed = not installed
settings-client-not-connected = not connected
settings-client-connected = connected
settings-client-stale = connected to another kubach: { $command }
settings-client-broken = config unreadable: { $error }
settings-client-connect = Connect
settings-client-reconnect = Reconnect
settings-client-disconnect = Disconnect
settings-client-connect-title = Connect { $client }?
settings-client-disconnect-title = Disconnect { $client }?
# $summary: one of settings-plan-*; $suffix: e.g. .bak-kubach.
settings-client-change-description = { $summary }. The file belongs to { $client }: other servers and settings stay, the previous version is saved next to it as { $suffix }.
settings-client-apply = Apply
# $hint: one of settings-restart-*.
settings-client-connected-done = Connected. { $hint }
settings-client-disconnected-done = Disconnected. { $hint }
# $server: the server name, kubach.
settings-plan-create = Create { $path } with the server `{ $server }`
settings-plan-add = Add the server `{ $server }` to { $path }
settings-plan-update = Point the server `{ $server }` in { $path } here (it ran { $command })
settings-plan-remove = Remove the server `{ $server }` from { $path }
settings-plan-nothing = { $path } needs no change
settings-restart-claude-code = Start a new Claude Code session; /mcp shows the server.
settings-restart-codex = Start a new Codex session; `codex mcp list` shows the server.
settings-restart-opencode = Restart OpenCode.
settings-restart-cursor = Cursor picks it up in Settings → MCP; reload the window if not.
settings-restart-claude-desktop = Quit Claude Desktop completely and start it again.

## Active sessions

settings-group-sessions = Active sessions
settings-sessions-description = HTTP and `kubach mcp` sessions with the rights you granted them. Revoking a session or a right denies the next calls; a `kubach mcp` process learns about it within a few seconds. A revoked right is not asked for again in that session.
settings-sessions-http-waiting = HTTP agents: no context chosen yet. The agent picks one with use_context, and Kubach asks you before it starts.
settings-sessions-none = No active sessions.
# $profile: one of settings-minimum-*.
settings-session-title = { $context } · { $profile }
settings-session-no-calls = no calls yet
settings-session-pid = kubach mcp, pid { $pid }
settings-session-vault = vault, ends when it locks
settings-session-namespace-not-read = { $namespace }: not read yet
# $since: a time, e.g. 10:00:00; $left: e.g. 25 min.
settings-session-details =
    { $calls ->
        [one] { $origin } · { $clients } · { $calls } call · since { $since } · ends in { $left }
       *[other] { $origin } · { $clients } · { $calls } calls · since { $since } · ends in { $left }
    }
settings-session-no-namespace = No namespace yet: set up when the agent first works in one.
# $raised: what was allowed beyond the minimum, e.g. "Changes".
settings-session-raised = Allowed for this session: { $raised }
settings-session-no-grants = No rights granted yet.
settings-boundary-client-only = limits only in Kubach
settings-boundary-impersonation = limits in Kubach and the API server (impersonation)
settings-boundary-temporary-sa = limits in Kubach and the API server (temporary ServiceAccount)
# $who: one of settings-boundary-*.
settings-boundary = { $namespace }: { $who }
settings-boundary-warning = { $namespace }: { $who }. { $warning }
settings-grant-read = Read { $namespace }
settings-grant-secret = Secret { $namespace }/{ $name }
settings-grant-modify = Change { $namespace }
settings-grant-allowed-at-launch = allowed by --grant-read
settings-grant-allowed = allowed
settings-grant-denied = denied
settings-revoke = Revoke
settings-revoking = Revoking…

## Audit log

settings-group-audit = Audit log
settings-audit-description = Every call of every session: tool, arguments with secrets masked, decision and result. The only record of an agent's calls when the boundary is only in Kubach.
settings-audit-recent = Recent calls
settings-audit-file = Newest first. File: { $path }
settings-audit-no-folder = The OS has no settings folder: nothing is recorded.
settings-audit-show-file = Show file
