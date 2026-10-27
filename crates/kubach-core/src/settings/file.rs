//! Loading and saving the settings file.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use toml::{Table, Value};

use super::migrate::{self, Outcome};
use super::model::{SCHEMA_VERSION, Settings};

pub const SETTINGS_FILE: &str = "settings.toml";

/// Overrides the config directory (tests, portable setups).
pub const CONFIG_DIR_ENV: &str = "KUBACH_CONFIG_DIR";

/// `$KUBACH_CONFIG_DIR`, else `kubach` in the OS config directory:
/// `~/Library/Application Support/kubach`, `%APPDATA%\kubach` or
/// `$XDG_CONFIG_HOME/kubach` (`~/.config/kubach`).
pub fn config_dir() -> Option<PathBuf> {
    match std::env::var_os(CONFIG_DIR_ENV) {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => dirs::config_dir().map(|dir| dir.join("kubach")),
    }
}

/// Path of the settings file in [`config_dir`].
pub fn default_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(SETTINGS_FILE))
}

/// Result of [`load`]: always usable settings, plus what went wrong.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loaded {
    pub settings: Settings,
    /// Errors to show the user; empty when the file loaded cleanly.
    pub problems: Vec<String>,
    /// Copy of the original file, made when it could not be fully read.
    pub backup: Option<PathBuf>,
    /// Saving would lose data (file from a newer Kubach, or unreadable), so
    /// the caller should not write it.
    pub read_only: bool,
}

/// Loads settings from `path`, never failing: a missing file gives the
/// defaults; an invalid one drops only the invalid values (fields, map entries,
/// security entries), with a problem naming each, and makes a backup of the original next to it.
pub fn load(path: &Path) -> Loaded {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Loaded::default(),
        Err(error) => {
            return Loaded {
                problems: vec![format!("cannot read {}: {error}", path.display())],
                read_only: true,
                ..Loaded::default()
            };
        }
    };
    let mut loaded = parse(&text);
    if !loaded.problems.is_empty() && !loaded.read_only {
        match backup(path) {
            Ok(backup) => loaded.backup = Some(backup),
            Err(error) => {
                loaded
                    .problems
                    .push(format!("cannot back up {}: {error}", path.display()));
                // Keep the only copy of the user's file.
                loaded.read_only = true;
            }
        }
    }
    loaded
}

/// Parses settings text; see [`load`].
fn parse(text: &str) -> Loaded {
    let mut table: Table = match toml::from_str(text) {
        Ok(table) => table,
        Err(error) => {
            return Loaded {
                problems: vec![format!("invalid settings file: {error}")],
                ..Loaded::default()
            };
        }
    };
    let mut loaded = Loaded::default();
    match migrate::migrate(&mut table) {
        Ok(Outcome::Current | Outcome::Migrated { .. }) => {}
        Ok(Outcome::Newer { version }) => {
            loaded.problems.push(format!(
                "settings were written by a newer Kubach (schema {version}, this one reads \
                 {SCHEMA_VERSION}); changes will not be saved"
            ));
            loaded.read_only = true;
        }
        Err(error) => {
            loaded.problems.push(error);
            table.remove("version");
        }
    }
    migrate::show_secrets_of_old_development_policies(&mut table);
    match Settings::deserialize_table(table.clone()) {
        Ok(settings) => loaded.settings = settings,
        Err(_) => {
            let table = salvage(table, &check, "", &mut loaded.problems);
            // What is left deserializes; the fallback is only a guard.
            loaded.settings = Settings::deserialize_table(table).unwrap_or_default();
        }
    }
    loaded.settings.version = SCHEMA_VERSION;
    loaded
}

impl Settings {
    fn deserialize_table(table: Table) -> Result<Self, toml::de::Error> {
        Value::Table(table).try_into()
    }
}

fn check(table: &Table) -> Result<(), String> {
    Settings::deserialize_table(table.clone())
        .map(drop)
        // The message ends with "in `path`", already in the problem.
        .map_err(|error| {
            error
                .to_string()
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned()
        })
}

/// Keeps the keys of `table` that `valid` accepts, one at a time, so an
/// invalid value drops only itself: a field, a map entry (one context's Helm
/// driver) or an array element (one security entry, which then falls back to
/// the strict policy). Each dropped value adds a problem naming its path.
/// `valid` checks the whole settings with this table in place.
fn salvage(
    table: Table,
    valid: &dyn Fn(&Table) -> Result<(), String>,
    path: &str,
    problems: &mut Vec<String>,
) -> Table {
    let mut kept = Table::new();
    for (key, value) in table {
        let key_path = join(path, &key);
        let with = |kept: &Table, value: Value| {
            let mut candidate = kept.clone();
            candidate.insert(key.clone(), value);
            candidate
        };
        let candidate = with(&kept, value.clone());
        let error = match valid(&candidate) {
            Ok(()) => {
                kept = candidate;
                continue;
            }
            Err(error) => error,
        };
        let reported = problems.len();
        let salvaged = match value {
            Value::Table(inner) => {
                let valid_inner = |inner: &Table| valid(&with(&kept, Value::Table(inner.clone())));
                Value::Table(salvage(inner, &valid_inner, &key_path, problems))
            }
            Value::Array(items) => {
                let mut good: Vec<Value> = Vec::new();
                for (ix, item) in items.into_iter().enumerate() {
                    let name = entry_name(&item);
                    good.push(item);
                    if let Err(error) = valid(&with(&kept, Value::Array(good.clone()))) {
                        good.pop();
                        problems.push(ignored(&format!("{key_path}[{ix}]{name}"), &error));
                    }
                }
                Value::Array(good)
            }
            _ => {
                problems.push(ignored(&key_path, &error));
                continue;
            }
        };
        let candidate = with(&kept, salvaged);
        match valid(&candidate) {
            Ok(()) => kept = candidate,
            Err(error) => {
                // Nothing inside could be kept: report the value as a whole.
                problems.truncate(reported);
                problems.push(ignored(&key_path, &error));
            }
        }
    }
    kept
}

fn ignored(path: &str, error: &str) -> String {
    format!("invalid value of {path} ignored: {error}")
}

/// ` (context "name")` for a security entry, so the user can find it.
fn entry_name(item: &Value) -> String {
    item.get("context")
        .and_then(Value::as_str)
        .map(|name| format!(" (context {name:?})"))
        .unwrap_or_default()
}

/// `parent.key`, quoting keys that are not bare TOML keys (context names
/// with dots or colons).
fn join(parent: &str, key: &str) -> String {
    let bare = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let key = if bare {
        key.to_owned()
    } else {
        format!("{key:?}")
    };
    if parent.is_empty() {
        key
    } else {
        format!("{parent}.{key}")
    }
}

/// Copies `path` to `<name>.<unix time>.bak` next to it.
fn backup(path: &Path) -> io::Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs());
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(format!(".{stamp}.bak"));
    let backup = path.with_file_name(name);
    fs::copy(path, &backup)?;
    Ok(backup)
}

/// Writes `settings` atomically: a temp file in the same directory, synced,
/// then renamed over `path`. Creates the directory if needed.
pub fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    let mut settings = settings.clone();
    settings.version = SCHEMA_VERSION;
    let body = toml::to_string_pretty(&settings).map_err(io::Error::other)?;
    let text = format!("# Kubach settings. Edits are picked up while Kubach runs.\n\n{body}");
    write_atomic(path, text.as_bytes())
}

/// Creates `dir` (and missing parents) readable by its owner only: `0700` on
/// Unix. An existing `dir` open to the group or others is narrowed to `0700`
/// too. Subfolders created later with the default umask (`cache/`) stay
/// unreachable for others through it. On Windows the per-user profile
/// folder already restricts access, and secret files get their own DACL.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let mode = fs::metadata(dir)?.permissions().mode();
        if mode & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(mode & 0o7700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    create_private_dir(dir)?;
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(".tmp-{}", std::process::id()));
    let temp = dir.join(name);
    let result = (|| {
        let mut file = fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        fs::remove_file(&temp).ok();
    }
    result
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::settings::{
        ContextKey, ContextPolicy, DEFAULT_MCP_PORT, Language, Profile, PrometheusSource,
        ThemePreference, security::ExecMode,
    };

    fn temp_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kubach").join(SETTINGS_FILE);
        (dir, path)
    }

    fn backups(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "bak"))
            .collect()
    }

    #[test]
    fn defaults() {
        let settings = Settings::default();
        assert_eq!(settings.version, SCHEMA_VERSION);
        assert_eq!(settings.general.theme, ThemePreference::Dark);
        assert_eq!(settings.general.language, Language::En);
        assert!(settings.clusters.kubeconfig_paths.is_empty());
        assert_eq!(settings.clusters.startup_context, None);
        assert_eq!(settings.tools.helm_path, None);
        assert_eq!(
            settings.tools.helm_line,
            crate::tools::helm::HelmLine::Helm4
        );
        assert!(settings.security.contexts.is_empty());
    }

    #[test]
    fn missing_file_gives_defaults_without_problems() {
        let (_dir, path) = temp_path();
        assert_eq!(load(&path), Loaded::default());
        assert!(!path.exists());
    }

    #[test]
    fn round_trip() {
        let (_dir, path) = temp_path();
        let mut settings = Settings::default();
        settings.general.theme = ThemePreference::System;
        settings.general.language = Language::Ru;
        settings.clusters.kubeconfig_paths = vec!["~/.kube/extra".into(), "/etc/kube".into()];
        settings.clusters.startup_context = Some("kind-dev".into());
        settings
            .clusters
            .set_default_namespace("prod.eu", Some("payments".into()));
        settings.tools.helm_path = Some("/opt/homebrew/bin/helm".into());
        settings.tools.helm_line = crate::tools::helm::HelmLine::Helm3;
        settings.security.set_policy(
            &ContextKey::new("minikube", "https://192.168.49.2:8443"),
            ContextPolicy::development(),
        );
        settings.security.set_policy(
            &ContextKey::new("staging", "https://staging.example.com"),
            ContextPolicy {
                mcp_enabled: true,
                max_profile: Profile::Debug,
                exec: ExecMode::ConfirmEach,
                ..ContextPolicy::strict()
            },
        );

        save(&path, &settings).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.problems, Vec::<String>::new());
        assert_eq!(loaded.settings, settings);

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("version = 1"), "{text}");
        assert!(text.contains("[[security.context]]"), "{text}");
        assert!(text.contains("theme = \"system\""), "{text}");
        assert!(text.contains("helm_line = \"3\""), "{text}");
        // No leftover temp files.
        let files: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn languages_are_stored_as_bcp47_tags() {
        let (_dir, path) = temp_path();
        for (language, tag) in [
            (Language::En, "en"),
            (Language::Ru, "ru"),
            (Language::ZhHans, "zh-Hans"),
        ] {
            let mut settings = Settings::default();
            settings.general.language = language;
            save(&path, &settings).unwrap();
            let text = fs::read_to_string(&path).unwrap();
            assert!(text.contains(&format!("language = \"{tag}\"")), "{text}");
            let loaded = load(&path);
            assert_eq!(loaded.problems, Vec::<String>::new());
            assert_eq!(loaded.settings.general.language, language);
        }
        // Tags are case-insensitive in BCP 47; a hand-written lower-case one loads.
        let loaded = parse("version = 1\n[general]\nlanguage = \"zh-hans\"\n");
        assert_eq!(loaded.problems, Vec::<String>::new());
        assert_eq!(loaded.settings.general.language, Language::ZhHans);
    }

    #[test]
    fn save_replaces_existing_file() {
        let (_dir, path) = temp_path();
        save(&path, &Settings::default()).unwrap();
        let mut settings = Settings::default();
        settings.general.theme = ThemePreference::Light;
        save(&path, &settings).unwrap();
        assert_eq!(load(&path).settings, settings);
    }

    #[test]
    fn unknown_and_missing_fields_are_tolerated() {
        let loaded = parse(
            r#"
            version = 1
            future_section = { a = 1 }
            [general]
            theme = "light"
            font_size = 14
            [[security.context]]
            context = "kind-dev"
            server = "https://127.0.0.1:6443"
            mcp_enabled = true
            new_option = "x"
            "#,
        );
        assert_eq!(loaded.problems, Vec::<String>::new());
        assert_eq!(loaded.settings.general.theme, ThemePreference::Light);
        assert_eq!(loaded.settings.general.language, Language::En);
        let policy = loaded
            .settings
            .security
            .policy(&ContextKey::new("kind-dev", "https://127.0.0.1:6443"));
        assert!(policy.mcp_enabled);
        // Missing keys of the entry keep strict defaults.
        assert_eq!(policy.session_ttl_minutes, 30);
        assert_eq!(policy.max_profile, Profile::ReadOnly);
        // Files from before #96 hide Secret values.
        assert!(policy.hide_secret_values);
    }

    #[test]
    fn invalid_values_reset_only_their_section() {
        let (dir, path) = temp_path();
        let text = r#"
            version = 1
            [general]
            theme = "purple"
            [clusters]
            startup_context = "prod"
            [[security.context]]
            context = "a"
            server = "https://a"
            max_profile = "root"
            [[security.context]]
            context = "b"
            server = "https://b"
            mcp_enabled = true
        "#;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();

        let loaded = load(&path);
        assert_eq!(loaded.problems.len(), 2, "{:?}", loaded.problems);
        assert!(
            loaded.problems[0].starts_with("invalid value of general.theme ignored: "),
            "{:?}",
            loaded.problems
        );
        assert!(
            loaded.problems[1]
                .starts_with("invalid value of security.context[0] (context \"a\") ignored: "),
            "{:?}",
            loaded.problems
        );
        assert!(!loaded.read_only);
        assert_eq!(loaded.settings.general.theme, ThemePreference::Dark);
        assert_eq!(
            loaded.settings.clusters.startup_context.as_deref(),
            Some("prod")
        );
        let contexts: Vec<_> = loaded
            .settings
            .security
            .contexts
            .iter()
            .map(|entry| entry.context.as_str())
            .collect();
        assert_eq!(contexts, ["b"]);

        let backup = loaded.backup.unwrap();
        assert_eq!(fs::read_to_string(&backup).unwrap(), text);
        assert_eq!(backups(&dir.path().join("kubach")), [backup]);
        // The original is left alone until the next save.
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }

    /// Paths of the values `parse` ignored.
    fn ignored_paths(problems: &[String]) -> Vec<String> {
        problems
            .iter()
            .map(|problem| {
                let rest = problem.strip_prefix("invalid value of ").unwrap();
                rest.split(" ignored: ").next().unwrap().to_owned()
            })
            .collect()
    }

    #[test]
    fn invalid_fields_drop_only_themselves_in_every_section() {
        let loaded = parse(
            r#"
            version = 1
            [general]
            theme = "light"
            language = "klingon"
            recent_kustomize_dirs = ["/a", 5, "/b"]
            [clusters]
            kubeconfig_paths = ["~/.kube/extra", false]
            startup_context = "prod"
            keep_connected = -2
            vault_lock_minutes = 5
            vault_lock_on_screen_lock = "yes"
            [clusters.default_namespaces]
            "prod.eu" = "payments"
            broken = 7
            [clusters.helm_drivers]
            kind-dev = "configmap"
            prod = "sql"
            [clusters.prometheus]
            prod = "monitoring/prometheus:web"
            "a.b" = ["x"]
            [clusters.hotbar]
            order = ["prod", "kind-dev"]
            hidden = "all"
            [clusters.hotbar.aliases]
            prod = "P"
            [tools]
            helm_path = 42
            [agent]
            http_enabled = true
            http_port = 70000
            [[security.context]]
            context = "kind-dev"
            server = "https://127.0.0.1:6443"
            mcp_enabled = true
            [[security.context]]
            context = "prod"
            server = "https://prod"
            exec = "always"
            [[security.context]]
            server = "https://nameless"
            "#,
        );
        assert_eq!(
            ignored_paths(&loaded.problems),
            [
                "agent.http_port",
                "clusters.default_namespaces.broken",
                "clusters.helm_drivers.prod",
                "clusters.hotbar.hidden",
                "clusters.keep_connected",
                "clusters.kubeconfig_paths[1]",
                "clusters.prometheus.\"a.b\"",
                "clusters.vault_lock_on_screen_lock",
                "general.language",
                "general.recent_kustomize_dirs[1]",
                "security.context[1] (context \"prod\")",
                "security.context[2]",
                "tools.helm_path",
            ],
            "{:#?}",
            loaded.problems
        );
        assert!(
            loaded.problems[2].ends_with("unknown variant `sql`, expected `secret` or `configmap`"),
            "{}",
            loaded.problems[2]
        );
        assert!(!loaded.read_only);

        let settings = &loaded.settings;
        let defaults = Settings::default();
        assert_eq!(settings.general.theme, ThemePreference::Light);
        assert_eq!(settings.general.language, Language::En);
        assert_eq!(
            settings.general.recent_kustomize_dirs,
            [PathBuf::from("/a"), PathBuf::from("/b")]
        );

        let clusters = &settings.clusters;
        assert_eq!(clusters.kubeconfig_paths, [PathBuf::from("~/.kube/extra")]);
        assert_eq!(clusters.startup_context.as_deref(), Some("prod"));
        assert_eq!(clusters.keep_connected, defaults.clusters.keep_connected);
        assert_eq!(clusters.vault_lock_minutes, 5);
        assert!(clusters.vault_lock_on_screen_lock);
        assert_eq!(clusters.default_namespace("prod.eu"), Some("payments"));
        assert_eq!(clusters.default_namespace("broken"), None);
        assert_eq!(
            clusters.helm_driver("kind-dev"),
            Some(crate::helm::Driver::ConfigMap)
        );
        assert_eq!(clusters.helm_driver("prod"), None);
        assert_eq!(
            clusters.prometheus("prod"),
            Some("monitoring/prometheus:web")
        );
        assert_eq!(clusters.prometheus("a.b"), None);
        assert_eq!(clusters.hotbar.order, ["prod", "kind-dev"]);
        assert_eq!(clusters.hotbar.aliases["prod"], "P");
        assert!(clusters.hotbar.hidden.is_empty());

        assert_eq!(settings.tools, defaults.tools);
        assert!(settings.agent.http_enabled);
        assert_eq!(settings.agent.http_port, DEFAULT_MCP_PORT);

        // The broken entry falls back to the strict policy.
        let contexts: Vec<_> = settings
            .security
            .contexts
            .iter()
            .map(|entry| entry.context.as_str())
            .collect();
        assert_eq!(contexts, ["kind-dev"]);
        assert_eq!(
            settings
                .security
                .policy(&ContextKey::new("prod", "https://prod")),
            ContextPolicy::strict()
        );
    }

    #[test]
    fn development_policies_from_before_109_keep_showing_secret_values() {
        use crate::settings::{Preset, SecretAction};

        // Written before `hide_secret_values` existed.
        let (_dir, path) = temp_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"
            version = 1
            [[security.context]]
            context = "minikube"
            server = "https://192.168.49.2:8443"
            mcp_enabled = true
            max_profile = "deploy"
            temporary_objects = true
            deploy = "apply-with-confirmation"
            exec = "confirm-each"
            session_ttl_minutes = 120
            namespaces = "multiple"
            [[security.context]]
            context = "staging"
            server = "https://staging.example.com"
            mcp_enabled = true
            max_profile = "debug"
            exec = "confirm-each"
            [[security.context]]
            context = "kind-dev"
            server = "https://127.0.0.1:6443"
            mcp_enabled = true
            max_profile = "deploy"
            temporary_objects = true
            deploy = "apply-with-confirmation"
            exec = "confirm-each"
            session_ttl_minutes = 120
            namespaces = "multiple"
            hide_secret_values = true
            "#,
        )
        .unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.problems, Vec::<String>::new());
        assert_eq!(loaded.backup, None);
        let security = &loaded.settings.security;

        let minikube = security.policy(&ContextKey::new("minikube", "https://192.168.49.2:8443"));
        // `agent_secrets` (#227) gets the default, not the preset's reveal.
        assert_eq!(
            minikube,
            ContextPolicy {
                agent_secrets: crate::settings::AgentSecrets::MetadataAndChecks,
                ..ContextPolicy::development()
            }
        );
        assert_eq!(minikube.matching_preset(), None);
        assert!(minikube.allows_secret(SecretAction::Show));

        // Other old policies keep hiding values.
        let staging = security.policy(&ContextKey::new("staging", "https://staging.example.com"));
        assert!(staging.hide_secret_values);
        assert_eq!(staging.matching_preset(), None);
        // An explicit choice is kept.
        let kind = security.policy(&ContextKey::new("kind-dev", "https://127.0.0.1:6443"));
        assert!(kind.hide_secret_values);
        assert_eq!(kind.matching_preset(), None);
        // Contexts without settings stay strict.
        let other = security.policy(&ContextKey::new("prod", "https://prod.example.com"));
        assert_eq!(other.matching_preset(), Some(Preset::Strict));
        assert!(!other.allows_secret(SecretAction::Show));

        // Saved with the key, the choice survives later loads.
        save(&path, &loaded.settings).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("hide_secret_values = false"), "{text}");
        assert_eq!(load(&path).settings, loaded.settings);
    }

    #[test]
    fn save_after_invalid_field_keeps_the_valid_ones() {
        let (_dir, path) = temp_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"
            [clusters]
            startup_context = "prod"
            kubeconfig_paths = ["/etc/kube"]
            [clusters.helm_drivers]
            kind-dev = "sql"
            prod = "secret"
            [clusters.default_namespaces]
            prod = "web"
            "#,
        )
        .unwrap();
        let mut loaded = load(&path);
        assert_eq!(
            ignored_paths(&loaded.problems),
            ["clusters.helm_drivers.kind-dev"]
        );
        assert!(loaded.backup.is_some());

        loaded.settings.general.theme = ThemePreference::Light;
        save(&path, &loaded.settings).unwrap();
        let again = load(&path);
        assert_eq!(again.problems, Vec::<String>::new());
        assert_eq!(again.settings, loaded.settings);
        let clusters = &again.settings.clusters;
        assert_eq!(clusters.startup_context.as_deref(), Some("prod"));
        assert_eq!(clusters.kubeconfig_paths, [PathBuf::from("/etc/kube")]);
        assert_eq!(clusters.default_namespace("prod"), Some("web"));
        assert_eq!(
            clusters.helm_driver("prod"),
            Some(crate::helm::Driver::Secret)
        );
        assert_eq!(clusters.helm_driver("kind-dev"), None);
    }

    #[test]
    fn broken_toml_gives_defaults_and_backup() {
        let (_dir, path) = temp_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[general\ntheme = ").unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.settings, Settings::default());
        assert_eq!(loaded.problems.len(), 1);
        assert!(loaded.problems[0].starts_with("invalid settings file"));
        assert!(loaded.backup.is_some_and(|backup| backup.exists()));
    }

    #[test]
    fn unversioned_file_is_migrated() {
        let loaded = parse("[general]\ntheme = \"light\"\n");
        assert!(loaded.problems.is_empty());
        assert_eq!(loaded.settings.version, SCHEMA_VERSION);
        assert_eq!(loaded.settings.general.theme, ThemePreference::Light);
    }

    #[test]
    fn newer_schema_is_read_only() {
        let (_dir, path) = temp_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "version = 7\n[general]\ntheme = \"light\"\n").unwrap();
        let loaded = load(&path);
        assert!(loaded.read_only);
        assert_eq!(loaded.problems.len(), 1);
        assert_eq!(loaded.backup, None);
        assert_eq!(loaded.settings.general.theme, ThemePreference::Light);
    }

    #[test]
    fn default_namespaces_with_dotted_context_names() {
        let mut settings = Settings::default();
        settings.clusters.default_namespaces =
            BTreeMap::from([("arn:aws:eks:eu-1:1:cluster/a.b".into(), "web".into())]);
        let text = toml::to_string_pretty(&settings).unwrap();
        assert_eq!(parse(&text).settings, settings);
        settings
            .clusters
            .set_default_namespace("arn:aws:eks:eu-1:1:cluster/a.b", Some("  ".into()));
        assert!(settings.clusters.default_namespaces.is_empty());
    }

    #[test]
    fn prometheus_addresses_per_context() {
        let mut settings = Settings::default();
        settings
            .clusters
            .set_prometheus("kind-a.b", Some(" monitoring/prometheus-k8s:web ".into()));
        assert_eq!(
            settings.clusters.prometheus("kind-a.b"),
            Some("monitoring/prometheus-k8s:web")
        );
        let text = toml::to_string_pretty(&settings).unwrap();
        assert_eq!(parse(&text).settings, settings);
        settings
            .clusters
            .set_prometheus("kind-a.b", Some(String::new()));
        assert_eq!(settings.clusters.prometheus("kind-a.b"), None);
        // Clearing the address of an old-style entry keeps it Manual.
        assert_eq!(
            settings.clusters.prometheus_source("kind-a.b"),
            PrometheusSource::Manual
        );
        settings.clusters.reset_prometheus("kind-a.b");
        let text = toml::to_string_pretty(&settings).unwrap();
        assert!(!text.contains("prometheus"), "{text}");
    }

    #[test]
    fn prometheus_sources_per_context() {
        // Files from before the choice existed: an address means Manual.
        let loaded = parse(
            r#"
            version = 1
            [clusters.prometheus]
            old = "monitoring/prometheus:web"
            "#,
        );
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        let mut clusters = loaded.settings.clusters;
        assert_eq!(clusters.prometheus_source("old"), PrometheusSource::Manual);
        assert_eq!(clusters.prometheus_source("new"), PrometheusSource::Auto);

        // Auto with a typed address is stored, so the address is kept.
        clusters.set_prometheus_source("old", PrometheusSource::Auto);
        assert_eq!(clusters.prometheus_source("old"), PrometheusSource::Auto);
        assert_eq!(
            clusters.prometheus("old"),
            Some("monitoring/prometheus:web")
        );
        clusters.set_prometheus_source("new", PrometheusSource::Off);
        clusters.set_prometheus_source(
            "eks:a/b.c",
            PrometheusSource::Found("monitoring/prometheus-operated:web".into()),
        );
        let settings = Settings {
            clusters,
            ..Settings::default()
        };
        let text = toml::to_string_pretty(&settings).unwrap();
        let loaded = parse(&text);
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(loaded.settings, settings, "{text}");

        // Auto without an address is the default and not stored.
        let mut clusters = settings.clusters;
        clusters.set_prometheus_source("new", PrometheusSource::Auto);
        assert!(!clusters.prometheus_sources.contains_key("new"));

        // A broken entry drops only itself.
        let loaded = parse(
            r#"
            version = 1
            [clusters.prometheus_sources]
            a = "off"
            b = "sometimes"
            c = { found = "monitoring/p:web" }
            "#,
        );
        assert_eq!(
            ignored_paths(&loaded.problems),
            ["clusters.prometheus_sources.b"]
        );
        let clusters = loaded.settings.clusters;
        assert_eq!(clusters.prometheus_source("a"), PrometheusSource::Off);
        assert_eq!(clusters.prometheus_source("b"), PrometheusSource::Auto);
        assert_eq!(
            clusters.prometheus_source("c"),
            PrometheusSource::Found("monitoring/p:web".into())
        );
    }

    #[test]
    fn helm_drivers_round_trip() {
        let mut settings = Settings::default();
        settings
            .clusters
            .set_helm_driver("kind-dev", Some(crate::helm::Driver::ConfigMap));
        let text = toml::to_string_pretty(&settings).unwrap();
        assert!(text.contains("kind-dev = \"configmap\""), "{text}");
        assert_eq!(parse(&text).settings, settings);
        assert_eq!(
            settings.clusters.helm_driver("kind-dev"),
            Some(crate::helm::Driver::ConfigMap)
        );
        settings.clusters.set_helm_driver("kind-dev", None);
        assert_eq!(settings.clusters.helm_driver("kind-dev"), None);
        assert!(
            !toml::to_string_pretty(&settings)
                .unwrap()
                .contains("helm_drivers")
        );
    }

    #[test]
    fn config_dir_honours_override() {
        // Only checks the computed path; nothing is written.
        let dir = config_dir().unwrap();
        assert!(dir.ends_with("kubach") || std::env::var_os(CONFIG_DIR_ENV).is_some());
        assert_eq!(default_path().unwrap(), dir.join(SETTINGS_FILE));
    }

    #[cfg(unix)]
    #[test]
    fn config_dir_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let (_dir, path) = temp_path();
        let config = path.parent().unwrap();
        save(&path, &Settings::default()).unwrap();
        assert_eq!(mode(config), 0o700);

        // An existing folder open to others is narrowed.
        fs::set_permissions(config, fs::Permissions::from_mode(0o755)).unwrap();
        create_private_dir(config).unwrap();
        assert_eq!(mode(config), 0o700);
        fs::set_permissions(config, fs::Permissions::from_mode(0o755)).unwrap();
        save(&path, &Settings::default()).unwrap();
        assert_eq!(mode(config), 0o700);
    }
}
