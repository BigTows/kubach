//! What a kubeconfig does on this computer besides talking to its API
//! servers: programs it runs to get credentials (exec plugins and the `gcp`
//! auth provider's `cmd-path`, with their environment) and local files whose
//! contents it sends (certificates, keys, token files). Shown before a
//! foreign kubeconfig is imported or linked, so nothing runs or leaves the
//! computer unnoticed.

use std::path::{Component, Path, PathBuf};

use kube::config::Kubeconfig;

/// What runs and what is read when connecting with a kubeconfig.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigReview {
    pub programs: Vec<Program>,
    pub files: Vec<LocalFile>,
}

impl ConfigReview {
    /// Looks at `config`; `folder` is the folder of the file it was read
    /// from (`None` for pasted YAML: then every file counts as outside).
    pub fn of(config: &Kubeconfig, folder: Option<&Path>) -> Self {
        let mut review = Self::default();
        for named in &config.clusters {
            let Some(cluster) = &named.cluster else {
                continue;
            };
            if cluster.certificate_authority_data.is_none()
                && let Some(path) = &cluster.certificate_authority
            {
                review.add_file(&named.name, FileField::CertificateAuthority, path, folder);
            }
        }
        for named in &config.auth_infos {
            let Some(user) = &named.auth_info else {
                continue;
            };
            if user.client_certificate_data.is_none()
                && let Some(path) = &user.client_certificate
            {
                review.add_file(&named.name, FileField::ClientCertificate, path, folder);
            }
            if user.client_key_data.is_none()
                && let Some(path) = &user.client_key
            {
                review.add_file(&named.name, FileField::ClientKey, path, folder);
            }
            if user.token.is_none()
                && let Some(path) = &user.token_file
            {
                review.add_file(&named.name, FileField::TokenFile, path, folder);
            }
            if let Some(exec) = &user.exec
                && let Some(command) = &exec.command
            {
                let env = exec
                    .env
                    .iter()
                    .flatten()
                    .map(|pair| {
                        EnvVar::new(
                            pair.get("name").cloned().unwrap_or_default(),
                            pair.get("value").cloned().unwrap_or_default(),
                        )
                    })
                    .collect();
                review.programs.push(Program {
                    user: named.name.clone(),
                    kind: ProgramKind::Exec,
                    command: command.clone(),
                    args: exec.args.clone().unwrap_or_default(),
                    env,
                });
            }
            // kube-rs runs `cmd-path` for the `gcp` provider; any provider
            // with one is listed, whatever its name.
            if let Some(provider) = &user.auth_provider
                && let Some(command) = provider.config.get("cmd-path")
            {
                let args = provider
                    .config
                    .get("cmd-args")
                    .map(|args| {
                        // Split the way kube-rs does: by single spaces.
                        args.trim()
                            .split(' ')
                            .filter(|arg| !arg.is_empty())
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                review.programs.push(Program {
                    user: named.name.clone(),
                    kind: ProgramKind::AuthProvider(provider.name.clone()),
                    command: command.clone(),
                    args,
                    env: Vec::new(),
                });
            }
        }
        review
    }

    fn add_file(&mut self, owner: &str, field: FileField, path: &str, folder: Option<&Path>) {
        self.files.push(LocalFile {
            owner: owner.to_owned(),
            field,
            path: path.to_owned(),
            outside: is_outside(path, folder),
        });
    }

    pub fn is_empty(&self) -> bool {
        self.programs.is_empty() && self.files.is_empty()
    }

    /// Whether some program gets an environment variable that changes how
    /// programs run.
    pub fn has_dangerous_env(&self) -> bool {
        self.programs.iter().any(Program::has_dangerous_env)
    }

    /// Whether some file lies outside the kubeconfig's folder.
    pub fn has_outside_files(&self) -> bool {
        self.files.iter().any(|file| file.outside)
    }
}

/// How a program gets started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProgramKind {
    /// `user.exec`, a credential plugin.
    Exec,
    /// `user.auth-provider.config.cmd-path` of the named provider.
    AuthProvider(String),
}

/// A program the config runs on connect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    /// The kubeconfig user it belongs to.
    pub user: String,
    pub kind: ProgramKind,
    pub command: String,
    pub args: Vec<String>,
    /// Variables added to its environment (exec plugins only).
    pub env: Vec<EnvVar>,
}

impl Program {
    /// `command arg1 arg2`, as shown to the user, with an OIDC client
    /// secret masked.
    pub fn command_line(&self) -> String {
        use crate::vault::manual::OIDC_CLIENT_SECRET_FLAG;
        std::iter::once(self.command.clone())
            .chain(self.args.iter().map(|arg| {
                if arg.starts_with(OIDC_CLIENT_SECRET_FLAG) {
                    format!("{OIDC_CLIENT_SECRET_FLAG}•••")
                } else {
                    arg.clone()
                }
            }))
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn has_dangerous_env(&self) -> bool {
        self.env.iter().any(|var| var.dangerous)
    }
}

/// An environment variable of an exec plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
    /// See [`is_dangerous_env`].
    pub dangerous: bool,
}

impl EnvVar {
    pub fn new(name: String, value: String) -> Self {
        Self {
            dangerous: is_dangerous_env(&name),
            name,
            value,
        }
    }
}

/// Variables that make an otherwise harmless program (`aws`, `gcloud`,
/// `kubelogin`) run other code: load libraries or modules, start a
/// "browser" or editor, look programs up elsewhere, or read another tool
/// config that runs commands (`credential_process`).
pub fn is_dangerous_env(name: &str) -> bool {
    const EXACT: &[&str] = &[
        "PATH",
        "BROWSER",
        "EDITOR",
        "VISUAL",
        "PAGER",
        "SHELL",
        "BASH_ENV",
        "ENV",
        "PS4",
        "PROMPT_COMMAND",
        "IFS",
        "HOME",
        "NODE_OPTIONS",
        "NODE_PATH",
        "PERL5OPT",
        "PERL5LIB",
        "PERLLIB",
        "RUBYOPT",
        "RUBYLIB",
        "JAVA_TOOL_OPTIONS",
        "_JAVA_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "CLASSPATH",
        "GCONV_PATH",
        "AWS_CONFIG_FILE",
        "AWS_SHARED_CREDENTIALS_FILE",
        "CLOUDSDK_CONFIG",
        "CLOUDSDK_PYTHON",
        "GIT_SSH_COMMAND",
        "SSL_CERT_FILE",
        "REQUESTS_CA_BUNDLE",
        "AWS_CA_BUNDLE",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "ALL_PROXY",
    ];
    const PREFIXES: &[&str] = &["PYTHON", "LD_", "DYLD_", "NODE_", "GIT_CONFIG"];
    // Environment names are case-insensitive on Windows.
    let upper = name.trim().to_ascii_uppercase();
    EXACT.contains(&upper.as_str()) || PREFIXES.iter().any(|prefix| upper.starts_with(prefix))
}

/// Which field of the kubeconfig names a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileField {
    CertificateAuthority,
    ClientCertificate,
    ClientKey,
    TokenFile,
}

impl FileField {
    /// The key as written in kubeconfig.
    pub fn key(self) -> &'static str {
        match self {
            Self::CertificateAuthority => "certificate-authority",
            Self::ClientCertificate => "client-certificate",
            Self::ClientKey => "client-key",
            Self::TokenFile => "tokenFile",
        }
    }

    /// Whether the field is on a cluster (otherwise on a user).
    pub fn is_cluster(self) -> bool {
        matches!(self, Self::CertificateAuthority)
    }
}

/// A local file the config reads (and sends to the server, for keys and
/// tokens).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalFile {
    /// Name of the cluster or user with the field.
    pub owner: String,
    pub field: FileField,
    pub path: String,
    /// Outside the folder of the kubeconfig (or the config was pasted, or
    /// the path is relative and can't be resolved).
    pub outside: bool,
}

fn is_outside(path: &str, folder: Option<&Path>) -> bool {
    let Some(folder) = folder else {
        return true;
    };
    let path = Path::new(path);
    if path.is_relative() {
        return true;
    }
    // Symlinks and `..` must not make a file look like it is next to the
    // config.
    let resolve = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| normalize(path));
    !resolve(path).starts_with(resolve(folder))
}

/// `path` with `.` and `..` resolved lexically.
fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
apiVersion: v1
kind: Config
clusters:
  - name: c
    cluster: { server: "https://c.invalid", certificate-authority: /etc/ca.crt }
  - name: inline
    cluster:
      server: "https://inline.invalid"
      certificate-authority: /ignored.crt
      certificate-authority-data: Q0E=
users:
  - name: eks
    user:
      exec:
        apiVersion: client.authentication.k8s.io/v1beta1
        command: aws
        args: [eks, get-token]
        env:
          - { name: AWS_PROFILE, value: prod }
          - { name: BROWSER, value: /tmp/x }
          - { name: PYTHONWARNINGS, value: "ignore" }
  - name: gcp
    user:
      auth-provider:
        name: gcp
        config: { cmd-path: /usr/bin/true, cmd-args: "-c  echo hi" }
  - name: azure
    user:
      auth-provider: { name: azure, config: { tenant-id: x } }
  - name: files
    user: { tokenFile: /home/u/.npmrc, client-key: /k.key, client-certificate: /c.crt }
contexts:
  - name: x
    context: { cluster: c, user: eks }
"#;

    #[test]
    fn programs_include_auth_provider_commands_and_env() {
        let config = Kubeconfig::from_yaml(CONFIG).unwrap();
        let review = ConfigReview::of(&config, None);
        assert_eq!(review.programs.len(), 2);
        let exec = &review.programs[0];
        assert_eq!(exec.kind, ProgramKind::Exec);
        assert_eq!(exec.command_line(), "aws eks get-token");
        let env: Vec<_> = exec
            .env
            .iter()
            .map(|v| (v.name.as_str(), v.dangerous))
            .collect();
        assert_eq!(
            env,
            [
                ("AWS_PROFILE", false),
                ("BROWSER", true),
                ("PYTHONWARNINGS", true)
            ]
        );
        assert!(exec.has_dangerous_env());
        let gcp = &review.programs[1];
        assert_eq!(gcp.user, "gcp");
        assert_eq!(gcp.kind, ProgramKind::AuthProvider("gcp".into()));
        assert_eq!(gcp.command_line(), "/usr/bin/true -c echo hi");
        assert!(!gcp.has_dangerous_env());
        assert!(review.has_dangerous_env());
    }

    #[test]
    fn files_are_listed_unless_inline_data_wins() {
        let config = Kubeconfig::from_yaml(CONFIG).unwrap();
        let review = ConfigReview::of(&config, None);
        let files: Vec<_> = review
            .files
            .iter()
            .map(|f| (f.owner.as_str(), f.field.key(), f.path.as_str(), f.outside))
            .collect();
        assert_eq!(
            files,
            [
                ("c", "certificate-authority", "/etc/ca.crt", true),
                ("files", "client-certificate", "/c.crt", true),
                ("files", "client-key", "/k.key", true),
                ("files", "tokenFile", "/home/u/.npmrc", true),
            ]
        );
        assert!(review.has_outside_files());
    }

    #[test]
    fn files_next_to_the_config_are_inside() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("kube");
        std::fs::create_dir_all(folder.join("certs")).unwrap();
        std::fs::write(folder.join("certs/ca.crt"), "CA").unwrap();
        let inside = folder.join("certs/ca.crt");
        let sneaky = folder.join("certs/../../secret");
        let elsewhere = dir.path().join("other/token");
        assert!(!is_outside(inside.to_str().unwrap(), Some(&folder)));
        assert!(is_outside(sneaky.to_str().unwrap(), Some(&folder)));
        assert!(is_outside(elsewhere.to_str().unwrap(), Some(&folder)));
        assert!(is_outside("relative/token", Some(&folder)));
        assert!(is_outside(inside.to_str().unwrap(), None));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_out_of_the_folder_are_outside() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("kube");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(dir.path().join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(dir.path().join("secret"), folder.join("token")).unwrap();
        assert!(is_outside(
            folder.join("token").to_str().unwrap(),
            Some(&folder)
        ));
    }

    #[test]
    fn dangerous_env_names() {
        for name in [
            "PATH",
            "path",
            "BROWSER",
            "NODE_OPTIONS",
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "PYTHONPATH",
            "PYTHONWARNINGS",
            "AWS_CONFIG_FILE",
        ] {
            assert!(is_dangerous_env(name), "{name}");
        }
        for name in [
            "AWS_PROFILE",
            "AWS_REGION",
            "KUBECONFIG",
            "CLOUDSDK_CORE_PROJECT",
        ] {
            assert!(!is_dangerous_env(name), "{name}");
        }
    }

    #[test]
    fn plain_config_has_nothing_to_review() {
        let config = Kubeconfig::from_yaml(
            "apiVersion: v1\nkind: Config\nusers:\n  - name: u\n    user: { token: t }\n",
        )
        .unwrap();
        assert!(ConfigReview::of(&config, None).is_empty());
    }
}
