//! Connecting an AI agent with one command (#400): `kubach mcp install
//! <client>` and the Connect / Disconnect buttons in Settings → AI agent
//! add or remove the server `kubach` in the client's own config file. The
//! server is `<absolute path of this kubach> mcp` over stdio, without a
//! token: the process reaches the app through the bridge (ADR 0005).
//!
//! The files belong to other programs, so:
//! - nothing is written without the user's yes to the path and the diff
//!   ([`Plan::diff`]); the CLI asks unless `--yes`, the UI shows a dialog;
//! - only the `kubach` entry changes: other servers, settings and the key
//!   order stay (JSON through `serde_json` with `preserve_order`, TOML
//!   through `toml_edit`, which keeps comments and formatting);
//! - a backup of the previous file goes next to it (`<file>.bak-kubach`);
//! - a file that does not parse is never touched, the error says why;
//! - installing twice changes nothing, and a file changed between the
//!   preview and the yes is not overwritten;
//! - a symlinked config (dotfiles) is written through to its target.
//!
//! Where each client keeps its servers (checked against the official docs
//! on 2026-10-25):
//!
//! | Client | File | Key | Docs |
//! |--------|------|-----|------|
//! | Claude Code | `~/.claude.json` (user scope; `%USERPROFILE%` on Windows) | `mcpServers.kubach`: `type: stdio`, `command`, `args` | <https://code.claude.com/docs/en/mcp> |
//! | Codex | `$CODEX_HOME/config.toml`, default `~/.codex/config.toml` | `[mcp_servers.kubach]`: `command`, `args` | <https://developers.openai.com/codex/mcp> |
//! | OpenCode | `$XDG_CONFIG_HOME/opencode/opencode.json`, default `~/.config/opencode/opencode.json` (all systems) | `mcp.kubach`: `type: local`, `command: [..]`, `enabled` | <https://opencode.ai/docs/mcp-servers/>, <https://opencode.ai/docs/config/> |
//! | Cursor | `~/.cursor/mcp.json` | `mcpServers.kubach`: `type: stdio`, `command`, `args` | <https://cursor.com/docs/context/mcp> |
//! | Claude Desktop | macOS `~/Library/Application Support/Claude/claude_desktop_config.json`, Windows `%APPDATA%\Claude\claude_desktop_config.json`, Linux `$XDG_CONFIG_HOME/Claude/claude_desktop_config.json` (not documented officially) | `mcpServers.kubach`: `command`, `args` | <https://modelcontextprotocol.io/quickstart/user> |
//!
//! Files are written directly rather than through `claude mcp add` or
//! `codex mcp add`: the same preview, backup and merge then hold for every
//! client, and tests run against a temporary home. All paths come from an
//! [`Env`], never from the process environment inside the logic.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// The name of the server in every client's config.
pub const SERVER_NAME: &str = "kubach";
/// Suffix of the backup written next to a changed file.
pub const BACKUP_SUFFIX: &str = ".bak-kubach";

/// An MCP client Kubach can connect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Client {
    ClaudeCode,
    Codex,
    OpenCode,
    Cursor,
    ClaudeDesktop,
}

impl Client {
    pub const ALL: [Client; 5] = [
        Self::ClaudeCode,
        Self::Codex,
        Self::OpenCode,
        Self::Cursor,
        Self::ClaudeDesktop,
    ];

    /// The id on the command line: `claude-code`, `codex`, …
    pub fn id(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Cursor => "cursor",
            Self::ClaudeDesktop => "claude-desktop",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::Cursor => "Cursor",
            Self::ClaudeDesktop => "Claude Desktop",
        }
    }

    /// `claude-code`, `claude`, `Claude Code`… → the client.
    pub fn parse(text: &str) -> Option<Self> {
        let key: String = text
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        match key.as_str() {
            "claudecode" | "claude" => Some(Self::ClaudeCode),
            "codex" => Some(Self::Codex),
            "opencode" => Some(Self::OpenCode),
            "cursor" => Some(Self::Cursor),
            "claudedesktop" | "desktop" => Some(Self::ClaudeDesktop),
            _ => None,
        }
    }

    fn format(self) -> Format {
        match self {
            Self::Codex => Format::Toml,
            _ => Format::Json,
        }
    }

    /// What to do after a change for the client to see it.
    pub fn restart_hint(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Start a new Claude Code session; /mcp shows the server.",
            Self::Codex => "Start a new Codex session; `codex mcp list` shows the server.",
            Self::OpenCode => "Restart OpenCode.",
            Self::Cursor => "Cursor picks it up in Settings → MCP; reload the window if not.",
            Self::ClaudeDesktop => "Quit Claude Desktop completely and start it again.",
        }
    }
}

impl fmt::Display for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Json,
    Toml,
}

/// The operating system, for paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Mac,
    Windows,
    Linux,
}

impl Os {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// Where the clients' files are: the home folder and the variables that
/// move them. Tests pass a temporary home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Env {
    pub os: Os,
    pub home: PathBuf,
    /// `%APPDATA%` (Windows).
    pub appdata: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME`.
    pub xdg_config: Option<PathBuf>,
    /// `$CODEX_HOME`.
    pub codex_home: Option<PathBuf>,
}

impl Env {
    /// The real environment of this process; `None` without a home folder.
    pub fn current() -> Option<Self> {
        let var = |name: &str| {
            kubach_core::env::var_os(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        Some(Self {
            os: Os::current(),
            home: kubach_core::env::home_dir()?,
            appdata: var("APPDATA"),
            xdg_config: var("XDG_CONFIG_HOME"),
            codex_home: var("CODEX_HOME"),
        })
    }

    /// A plain home folder, nothing moved.
    #[cfg(test)]
    pub fn with_home(os: Os, home: impl Into<PathBuf>) -> Self {
        Self {
            os,
            home: home.into(),
            appdata: None,
            xdg_config: None,
            codex_home: None,
        }
    }

    fn xdg(&self) -> PathBuf {
        self.xdg_config
            .clone()
            .unwrap_or_else(|| self.home.join(".config"))
    }

    /// The config file of `client`.
    pub fn config_path(&self, client: Client) -> PathBuf {
        match client {
            Client::ClaudeCode => self.home.join(".claude.json"),
            Client::Codex => self
                .codex_home
                .clone()
                .unwrap_or_else(|| self.home.join(".codex"))
                .join("config.toml"),
            Client::OpenCode => self.xdg().join("opencode").join("opencode.json"),
            Client::Cursor => self.home.join(".cursor").join("mcp.json"),
            Client::ClaudeDesktop => {
                let dir = match self.os {
                    Os::Mac => self
                        .home
                        .join("Library")
                        .join("Application Support")
                        .join("Claude"),
                    Os::Windows => self
                        .appdata
                        .clone()
                        .unwrap_or_else(|| self.home.join("AppData").join("Roaming"))
                        .join("Claude"),
                    Os::Linux => self.xdg().join("Claude"),
                };
                dir.join("claude_desktop_config.json")
            }
        }
    }

    /// Whether `client` seems installed: its config or its folder exists.
    pub fn client_found(&self, client: Client) -> bool {
        let path = self.config_path(client);
        if path.exists() {
            return true;
        }
        match client {
            // `~/.claude` holds the rest of Claude Code's state.
            Client::ClaudeCode => self.home.join(".claude").is_dir(),
            _ => path.parent().is_some_and(Path::is_dir),
        }
    }
}

/// Why a file was not changed.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(
        "{path} is not valid {format}: {reason}. Kubach did not change it; fix the file or add \
         the server by hand"
    )]
    Broken {
        path: PathBuf,
        format: &'static str,
        reason: String,
    },
    #[error(
        "{path} has comments (JSONC), which Kubach cannot keep. Add the server by hand: {snippet}"
    )]
    Jsonc { path: PathBuf, snippet: String },
    #[error("{path} changed since the preview; nothing was written. Try again")]
    Changed { path: PathBuf },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// The `kubach` entry of a client's config, as found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// The client's config or folder does not exist.
    NotFound,
    /// The client is there, without the server.
    NotConnected,
    /// Points at this kubach.
    Connected,
    /// Points elsewhere (the binary moved or was replaced): `command`.
    Stale { command: String },
    /// The file does not parse.
    Broken(String),
}

impl Status {
    pub fn label(&self) -> String {
        match self {
            Self::NotFound => "not installed".into(),
            Self::NotConnected => "not connected".into(),
            Self::Connected => "connected".into(),
            Self::Stale { command } => format!("connected to another kubach: {command}"),
            Self::Broken(error) => format!("config unreadable: {error}"),
        }
    }
}

/// What a change does to the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// The entry is added.
    Add,
    /// The entry is replaced; it ran `command` before.
    Update { command: String },
    /// The entry is removed.
    Remove,
    /// Nothing to do.
    Nothing,
}

/// A change previewed for the user: apply it only after their yes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub client: Client,
    /// The file shown to the user.
    pub path: PathBuf,
    pub change: Change,
    /// The file's text as read, `None` if it does not exist.
    pub before: Option<String>,
    /// The text to write.
    pub after: String,
}

impl Plan {
    /// A unified diff of the change, for the CLI and the dialog.
    pub fn diff(&self) -> String {
        let before = self.before.as_deref().unwrap_or("");
        let path = self.path.display().to_string();
        similar::TextDiff::from_lines(before, &self.after)
            .unified_diff()
            .context_radius(3)
            .header(
                if self.before.is_some() {
                    &path
                } else {
                    "/dev/null"
                },
                &path,
            )
            .to_string()
    }

    /// One line on what happens.
    pub fn summary(&self) -> String {
        let path = self.path.display();
        match &self.change {
            Change::Add if self.before.is_none() => {
                format!("Create {path} with the server `{SERVER_NAME}`")
            }
            Change::Add => format!("Add the server `{SERVER_NAME}` to {path}"),
            Change::Update { command } => {
                format!("Point the server `{SERVER_NAME}` in {path} here (it ran {command})")
            }
            Change::Remove => format!("Remove the server `{SERVER_NAME}` from {path}"),
            Change::Nothing => format!("{path} needs no change"),
        }
    }
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> InstallError + '_ {
    move |source| InstallError::Io {
        path: path.to_owned(),
        source,
    }
}

fn read(path: &Path) -> Result<Option<String>, InstallError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(path)(error)),
    }
}

/// The command line of the server: `<exe> mcp`.
fn server_args() -> Vec<String> {
    vec!["mcp".to_owned()]
}

/// The entry Kubach writes for `client`.
fn json_entry(client: Client, exe: &str) -> Value {
    match client {
        Client::OpenCode => json!({
            "type": "local",
            "command": [exe, "mcp"],
            "enabled": true,
        }),
        Client::ClaudeDesktop => json!({"command": exe, "args": server_args()}),
        _ => json!({"type": "stdio", "command": exe, "args": server_args()}),
    }
}

/// The key holding the servers.
fn servers_key(client: Client) -> &'static str {
    match client {
        Client::OpenCode => "mcp",
        _ => "mcpServers",
    }
}

/// `command args…` of an existing entry, for status and messages.
fn json_command(client: Client, entry: &Value) -> Option<(String, Vec<String>)> {
    let strings = |value: &Value| -> Vec<String> {
        value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    match client {
        Client::OpenCode => {
            let mut parts = strings(&entry["command"]);
            (!parts.is_empty()).then(|| {
                let command = parts.remove(0);
                (command, parts)
            })
        }
        _ => entry["command"]
            .as_str()
            .map(|command| (command.to_owned(), strings(&entry["args"]))),
    }
}

fn status_of(command: Option<(String, Vec<String>)>, exe: &str) -> Status {
    match command {
        None => Status::NotConnected,
        Some((command, args)) if command == exe && args == server_args() => Status::Connected,
        Some((command, args)) => Status::Stale {
            command: std::iter::once(command)
                .chain(args)
                .collect::<Vec<_>>()
                .join(" "),
        },
    }
}

/// The indentation of a JSON text, so a rewrite keeps it.
fn json_indent(text: &str) -> Vec<u8> {
    text.lines()
        .skip(1)
        .find_map(|line| {
            let indent: String = line
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            (!indent.is_empty()).then_some(indent)
        })
        .unwrap_or_else(|| "  ".to_owned())
        .into_bytes()
}

fn write_json(value: &Value, original: Option<&str>) -> String {
    use serde::Serialize as _;
    let indent = original.map_or_else(|| b"  ".to_vec(), json_indent);
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(&indent);
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    value
        .serialize(&mut serializer)
        .expect("a JSON value serializes");
    let mut text = String::from_utf8(out).expect("JSON is UTF-8");
    if original.is_none_or(|o| o.ends_with('\n')) {
        text.push('\n');
    }
    text
}

fn parse_json(client: Client, path: &Path, text: &str) -> Result<Map<String, Value>, InstallError> {
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(root)) => {
            let key = servers_key(client);
            match root.get(key) {
                None | Some(Value::Object(_)) => Ok(root),
                Some(_) => Err(InstallError::Broken {
                    path: path.to_owned(),
                    format: "JSON",
                    reason: format!("`{key}` is not an object"),
                }),
            }
        }
        Ok(_) => Err(InstallError::Broken {
            path: path.to_owned(),
            format: "JSON",
            reason: "the top level is not an object".into(),
        }),
        Err(error) if client == Client::OpenCode && looks_like_jsonc(text) => {
            let _ = error;
            Err(InstallError::Jsonc {
                path: path.to_owned(),
                snippet: snippet(client, "<path to kubach>"),
            })
        }
        Err(error) => Err(InstallError::Broken {
            path: path.to_owned(),
            format: "JSON",
            reason: error.to_string(),
        }),
    }
}

/// Comments or trailing commas, which JSONC allows and JSON does not.
fn looks_like_jsonc(text: &str) -> bool {
    text.contains("//") || text.contains("/*") || text.contains(",\n}") || text.contains(",\n]")
}

fn parse_toml(path: &Path, text: &str) -> Result<toml_edit::DocumentMut, InstallError> {
    let document: toml_edit::DocumentMut =
        text.parse()
            .map_err(|error: toml_edit::TomlError| InstallError::Broken {
                path: path.to_owned(),
                format: "TOML",
                reason: error.message().to_owned(),
            })?;
    if let Some(servers) = document.get("mcp_servers")
        && !servers.is_table_like()
    {
        return Err(InstallError::Broken {
            path: path.to_owned(),
            format: "TOML",
            reason: "`mcp_servers` is not a table".into(),
        });
    }
    Ok(document)
}

fn toml_command(document: &toml_edit::DocumentMut) -> Option<(String, Vec<String>)> {
    let entry = document.get("mcp_servers")?.get(SERVER_NAME)?;
    let command = entry.get("command")?.as_str()?.to_owned();
    let args = entry
        .get("args")
        .and_then(|args| args.as_array())
        .map(|args| {
            args.iter()
                .filter_map(|arg| arg.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Some((command, args))
}

/// How `client` is connected now.
pub fn status(env: &Env, client: Client, exe: &Path) -> Status {
    let exe = exe.display().to_string();
    let path = env.config_path(client);
    let text = match read(&path) {
        Ok(Some(text)) => text,
        Ok(None) if env.client_found(client) => return Status::NotConnected,
        Ok(None) => return Status::NotFound,
        Err(error) => return Status::Broken(error.to_string()),
    };
    match client.format() {
        Format::Json => match parse_json(client, &path, &text) {
            Ok(root) => status_of(
                root.get(servers_key(client))
                    .and_then(|servers| servers.get(SERVER_NAME))
                    .and_then(|entry| json_command(client, entry)),
                &exe,
            ),
            Err(error) => Status::Broken(error.to_string()),
        },
        Format::Toml => match parse_toml(&path, &text) {
            Ok(document) => status_of(toml_command(&document), &exe),
            Err(error) => Status::Broken(error.to_string()),
        },
    }
}

/// Previews connecting `client` to `exe`.
pub fn plan_install(env: &Env, client: Client, exe: &Path) -> Result<Plan, InstallError> {
    let exe = exe.display().to_string();
    let path = env.config_path(client);
    let before = read(&path)?;
    let (change, after) = match client.format() {
        Format::Json => {
            let mut root = parse_json(client, &path, before.as_deref().unwrap_or(""))?;
            let key = servers_key(client);
            let wanted = json_entry(client, &exe);
            let servers = root
                .entry(key)
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .expect("checked by parse_json");
            let change = match servers.get(SERVER_NAME) {
                None => Change::Add,
                Some(old) if *old == wanted => Change::Nothing,
                Some(old) => Change::Update {
                    command: json_command(client, old)
                        .map(|(command, _)| command)
                        .unwrap_or_else(|| "something else".into()),
                },
            };
            if change == Change::Nothing {
                (change, before.clone().unwrap_or_default())
            } else {
                servers.insert(SERVER_NAME.to_owned(), wanted);
                (change, write_json(&Value::Object(root), before.as_deref()))
            }
        }
        Format::Toml => {
            let mut document = parse_toml(&path, before.as_deref().unwrap_or(""))?;
            let change = match toml_command(&document) {
                None if document
                    .get("mcp_servers")
                    .and_then(|servers| servers.get(SERVER_NAME))
                    .is_none() =>
                {
                    Change::Add
                }
                Some((command, args)) if command == exe && args == server_args() => Change::Nothing,
                Some((command, _)) => Change::Update { command },
                None => Change::Update {
                    command: "something else".into(),
                },
            };
            if change == Change::Nothing {
                (change, before.clone().unwrap_or_default())
            } else {
                if document.get("mcp_servers").is_none() {
                    let mut servers = toml_edit::Table::new();
                    // Only `[mcp_servers.kubach]` appears, not an empty header.
                    servers.set_implicit(true);
                    document.insert("mcp_servers", toml_edit::Item::Table(servers));
                }
                let mut entry = toml_edit::Table::new();
                entry.insert("command", toml_edit::value(exe.as_str()));
                let mut args = toml_edit::Array::new();
                for arg in server_args() {
                    args.push(arg);
                }
                entry.insert("args", toml_edit::value(args));
                document["mcp_servers"][SERVER_NAME] = toml_edit::Item::Table(entry);
                let mut text = document.to_string();
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                (change, text)
            }
        }
    };
    Ok(Plan {
        client,
        path,
        change,
        before,
        after,
    })
}

/// Previews disconnecting `client`: only the `kubach` entry goes.
pub fn plan_uninstall(env: &Env, client: Client) -> Result<Plan, InstallError> {
    let path = env.config_path(client);
    let before = read(&path)?;
    let Some(text) = before.clone() else {
        return Ok(Plan {
            client,
            path,
            change: Change::Nothing,
            before: None,
            after: String::new(),
        });
    };
    let after = match client.format() {
        Format::Json => {
            let mut root = parse_json(client, &path, &text)?;
            let key = servers_key(client);
            let removed = root
                .get_mut(key)
                .and_then(Value::as_object_mut)
                .and_then(|servers| servers.shift_remove(SERVER_NAME))
                .is_some();
            removed.then(|| write_json(&Value::Object(root), Some(&text)))
        }
        Format::Toml => {
            let mut document = parse_toml(&path, &text)?;
            let removed = document
                .get_mut("mcp_servers")
                .and_then(|servers| servers.as_table_like_mut())
                .and_then(|servers| servers.remove(SERVER_NAME))
                .is_some();
            removed.then(|| document.to_string())
        }
    };
    Ok(match after {
        Some(after) => Plan {
            client,
            path,
            change: Change::Remove,
            before,
            after,
        },
        None => Plan {
            client,
            path,
            change: Change::Nothing,
            after: text,
            before,
        },
    })
}

/// Writes `plan` after the user's yes: checks the file did not change
/// since the preview, keeps a backup next to it and replaces it atomically
/// with the same permissions. Returns the backup's path, if one was made.
pub fn apply(plan: &Plan) -> Result<Option<PathBuf>, InstallError> {
    if plan.change == Change::Nothing {
        return Ok(None);
    }
    // A symlinked config (dotfiles) stays a symlink: write its target.
    let target = match fs::symlink_metadata(&plan.path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::canonicalize(&plan.path).map_err(io_error(&plan.path))?
        }
        _ => plan.path.clone(),
    };
    if read(&target)? != plan.before {
        return Err(InstallError::Changed {
            path: plan.path.clone(),
        });
    }
    let dir = target
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(io_error(dir))?;
    let backup = match &plan.before {
        Some(_) => {
            let mut name = target.as_os_str().to_owned();
            name.push(BACKUP_SUFFIX);
            let backup = PathBuf::from(name);
            fs::copy(&target, &backup).map_err(io_error(&backup))?;
            Some(backup)
        }
        None => None,
    };
    let mut name = std::ffi::OsString::from(".");
    name.push(target.file_name().unwrap_or_default());
    name.push(format!(".kubach-{}", std::process::id()));
    let temp = dir.join(name);
    let written = (|| {
        fs::write(&temp, &plan.after)?;
        if let Ok(meta) = fs::metadata(&target) {
            fs::set_permissions(&temp, meta.permissions())?;
        } else {
            private(&temp)?;
        }
        fs::rename(&temp, &target)
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(io_error(&target)(error));
    }
    Ok(backup)
}

/// A new config may hold other secrets later: readable by the user only.
#[cfg(unix)]
fn private(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn private(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// The entry to add by hand, for "Manual setup" and errors.
pub fn snippet(client: Client, exe: &str) -> String {
    match client.format() {
        Format::Json => {
            let mut servers = Map::new();
            servers.insert(SERVER_NAME.to_owned(), json_entry(client, exe));
            let mut root = Map::new();
            root.insert(servers_key(client).to_owned(), Value::Object(servers));
            write_json(&Value::Object(root), None)
        }
        Format::Toml => format!(
            "[mcp_servers.{SERVER_NAME}]\ncommand = {}\nargs = [\"mcp\"]\n",
            toml_edit::Value::from(exe)
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        BACKUP_SUFFIX, Change, Client, Env, InstallError, Os, Status, apply, plan_install,
        plan_uninstall, snippet, status,
    };

    const EXE: &str = "/Applications/Kubach.app/Contents/MacOS/kubach";

    fn env(home: &Path) -> Env {
        Env::with_home(Os::Mac, home)
    }

    fn exe() -> PathBuf {
        PathBuf::from(EXE)
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn install(env: &Env, client: Client) -> super::Plan {
        let plan = plan_install(env, client, &exe()).unwrap();
        apply(&plan).unwrap();
        plan
    }

    #[test]
    fn paths_follow_each_client_and_system() {
        let home = PathBuf::from("/home/me");
        let mac = Env::with_home(Os::Mac, &home);
        assert_eq!(
            mac.config_path(Client::ClaudeCode),
            home.join(".claude.json")
        );
        assert_eq!(
            mac.config_path(Client::Codex),
            home.join(".codex/config.toml")
        );
        assert_eq!(
            mac.config_path(Client::OpenCode),
            home.join(".config/opencode/opencode.json")
        );
        assert_eq!(
            mac.config_path(Client::Cursor),
            home.join(".cursor/mcp.json")
        );
        assert_eq!(
            mac.config_path(Client::ClaudeDesktop),
            home.join("Library/Application Support/Claude/claude_desktop_config.json")
        );
        let windows = Env {
            appdata: Some(PathBuf::from("/roaming")),
            codex_home: Some(PathBuf::from("/codex")),
            ..Env::with_home(Os::Windows, &home)
        };
        assert_eq!(
            windows.config_path(Client::ClaudeDesktop),
            PathBuf::from("/roaming/Claude/claude_desktop_config.json")
        );
        assert_eq!(
            windows.config_path(Client::Codex),
            PathBuf::from("/codex/config.toml")
        );
        let linux = Env {
            xdg_config: Some(PathBuf::from("/xdg")),
            ..Env::with_home(Os::Linux, &home)
        };
        assert_eq!(
            linux.config_path(Client::OpenCode),
            PathBuf::from("/xdg/opencode/opencode.json")
        );
        assert_eq!(
            linux.config_path(Client::ClaudeDesktop),
            PathBuf::from("/xdg/Claude/claude_desktop_config.json")
        );
        assert_eq!(Client::parse("Claude Code"), Some(Client::ClaudeCode));
        assert_eq!(Client::parse("claude-desktop"), Some(Client::ClaudeDesktop));
        assert_eq!(Client::parse("vim"), None);
    }

    #[test]
    fn json_merge_keeps_other_servers_settings_and_order() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let path = env.config_path(Client::ClaudeCode);
        let original = "{\n    \"numStartups\": 42,\n    \"mcpServers\": {\n        \"github\": {\n            \"command\": \"gh-mcp\"\n        }\n    },\n    \"projects\": {}\n}\n";
        write(&path, original);
        assert_eq!(
            status(&env, Client::ClaudeCode, &exe()),
            Status::NotConnected
        );

        let plan = install(&env, Client::ClaudeCode);
        assert_eq!(plan.change, Change::Add);
        assert!(
            plan.diff().contains("+        \"kubach\": {"),
            "{}",
            plan.diff()
        );
        let text = std::fs::read_to_string(&path).unwrap();
        // Indentation and key order kept; the other server untouched.
        let keys: Vec<usize> = ["numStartups", "mcpServers", "github", "kubach", "projects"]
            .iter()
            .map(|key| text.find(&format!("\"{key}\"")).unwrap())
            .collect();
        assert!(keys.windows(2).all(|w| w[0] < w[1]), "{text}");
        assert!(text.contains("\n    \"numStartups\": 42"), "{text}");
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["mcpServers"]["github"]["command"], "gh-mcp");
        assert_eq!(value["mcpServers"]["kubach"]["command"], EXE);
        assert_eq!(value["mcpServers"]["kubach"]["args"][0], "mcp");
        assert_eq!(value["mcpServers"]["kubach"]["type"], "stdio");
        // The backup is the file as it was.
        let backup = format!("{}{BACKUP_SUFFIX}", path.display());
        assert_eq!(std::fs::read_to_string(backup).unwrap(), original);
        assert_eq!(status(&env, Client::ClaudeCode, &exe()), Status::Connected);

        // Twice: nothing to do, nothing written.
        let again = plan_install(&env, Client::ClaudeCode, &exe()).unwrap();
        assert_eq!(again.change, Change::Nothing);
        assert_eq!(apply(&again).unwrap(), None);

        // Disconnect removes only kubach.
        let removal = plan_uninstall(&env, Client::ClaudeCode).unwrap();
        assert_eq!(removal.change, Change::Remove);
        apply(&removal).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(value["mcpServers"].get("kubach").is_none());
        assert_eq!(value["mcpServers"]["github"]["command"], "gh-mcp");
        assert_eq!(value["numStartups"], 42);
        assert_eq!(
            plan_uninstall(&env, Client::ClaudeCode).unwrap().change,
            Change::Nothing
        );
    }

    #[test]
    fn every_client_gets_its_own_format() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        for client in Client::ALL {
            assert_eq!(status(&env, client, &exe()), Status::NotFound, "{client}");
            let plan = install(&env, client);
            assert_eq!(plan.change, Change::Add);
            assert!(plan.before.is_none());
            assert_eq!(status(&env, client, &exe()), Status::Connected, "{client}");
        }
        let read = |client| std::fs::read_to_string(env.config_path(client)).unwrap();
        let opencode: serde_json::Value = serde_json::from_str(&read(Client::OpenCode)).unwrap();
        assert_eq!(
            opencode["mcp"]["kubach"],
            serde_json::json!({"type": "local", "command": [EXE, "mcp"], "enabled": true})
        );
        let desktop: serde_json::Value =
            serde_json::from_str(&read(Client::ClaudeDesktop)).unwrap();
        assert_eq!(
            desktop["mcpServers"]["kubach"],
            serde_json::json!({"command": EXE, "args": ["mcp"]})
        );
        let cursor: serde_json::Value = serde_json::from_str(&read(Client::Cursor)).unwrap();
        assert_eq!(cursor["mcpServers"]["kubach"]["type"], "stdio");
        assert_eq!(
            read(Client::Codex),
            format!("[mcp_servers.kubach]\ncommand = \"{EXE}\"\nargs = [\"mcp\"]\n")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(env.config_path(Client::Cursor))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn toml_keeps_comments_and_other_servers() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let path = env.config_path(Client::Codex);
        let original = "# my codex settings\nmodel = \"o4\"   # fast\n\n[mcp_servers.docs]\ncommand = \"npx\"\nargs = [\"-y\", \"docs-mcp\"]\n";
        write(&path, original);
        install(&env, Client::Codex);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(original), "{text}");
        assert!(text.contains("[mcp_servers.kubach]\ncommand = "), "{text}");
        let removal = plan_uninstall(&env, Client::Codex).unwrap();
        apply(&removal).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn a_moved_binary_shows_as_stale_and_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let path = env.config_path(Client::Cursor);
        write(
            &path,
            r#"{"mcpServers": {"kubach": {"command": "/old/kubach", "args": ["mcp", "--context", "x"]}}}"#,
        );
        assert_eq!(
            status(&env, Client::Cursor, &exe()),
            Status::Stale {
                command: "/old/kubach mcp --context x".into()
            }
        );
        let plan = install(&env, Client::Cursor);
        assert_eq!(
            plan.change,
            Change::Update {
                command: "/old/kubach".into()
            }
        );
        assert!(plan.summary().contains("/old/kubach"));
        assert_eq!(status(&env, Client::Cursor, &exe()), Status::Connected);
    }

    #[test]
    fn broken_files_are_never_touched() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let cases = [
            (Client::ClaudeCode, "{ \"mcpServers\": "),
            (Client::Cursor, "[1, 2]"),
            (Client::ClaudeDesktop, "{\"mcpServers\": 7}"),
            (Client::Codex, "model = \n"),
            (Client::Codex, "mcp_servers = 3\n"),
            (Client::OpenCode, "{\n  // mine\n  \"mcp\": {}\n}\n"),
        ];
        for (client, text) in cases {
            let path = env.config_path(client);
            write(&path, text);
            let error = plan_install(&env, client, &exe()).unwrap_err();
            assert!(
                matches!(
                    error,
                    InstallError::Broken { .. } | InstallError::Jsonc { .. }
                ),
                "{client}: {error}"
            );
            assert!(error.to_string().contains(&path.display().to_string()));
            assert!(plan_uninstall(&env, client).is_err(), "{client}");
            assert!(matches!(status(&env, client, &exe()), Status::Broken(_)));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
            assert!(!PathBuf::from(format!("{}{BACKUP_SUFFIX}", path.display())).exists());
        }
    }

    #[test]
    fn a_file_changed_after_the_preview_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let path = env.config_path(Client::Cursor);
        write(&path, "{}\n");
        let plan = plan_install(&env, Client::Cursor, &exe()).unwrap();
        write(
            &path,
            "{\"mcpServers\": {\"other\": {\"command\": \"x\"}}}\n",
        );
        assert!(matches!(apply(&plan), Err(InstallError::Changed { .. })));
        assert!(std::fs::read_to_string(&path).unwrap().contains("other"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_config_stays_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let real = dir.path().join("dotfiles/mcp.json");
        write(&real, "{}\n");
        let link = env.config_path(Client::Cursor);
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        install(&env, Client::Cursor);
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(std::fs::read_to_string(&real).unwrap().contains("kubach"));
    }

    #[test]
    fn snippets_for_manual_setup() {
        let codex: toml_edit::DocumentMut =
            snippet(Client::Codex, "C:\\kubach.exe").parse().unwrap();
        assert_eq!(
            codex["mcp_servers"]["kubach"]["command"].as_str(),
            Some("C:\\kubach.exe")
        );
        let desktop: serde_json::Value =
            serde_json::from_str(&snippet(Client::ClaudeDesktop, EXE)).unwrap();
        assert_eq!(desktop["mcpServers"]["kubach"]["args"][0], "mcp");
    }
}
