//! Preparing a kubeconfig for the vault: what it adds (contexts, clusters,
//! users, programs it runs, local files it reads), context names that clash
//! with existing ones renamed (automatically, then by hand in the preview),
//! and certificate or token files inlined so the vault copy does not depend
//! on files next to the original.
//!
//! Files are inlined right away for a kubeconfig file: they are listed in
//! the preview, ones outside its folder highlighted. Pasted YAML has no
//! folder and comes from who knows where, so its files are not read until
//! the user confirms ([`ImportPlan::read_files`]); until then the plan can't
//! be stored and [`ImportPlan::to_contexts`] leaves them out.

use std::collections::HashSet;
use std::path::Path;

use base64::Engine as _;
use kubach_i18n::{Message, msg};
use kube::config::Kubeconfig;
use secrecy::SecretString;
use zeroize::Zeroizing;

use crate::kubeconfig::review::ConfigReview;
use crate::kubeconfig::{self, FileProblem};

/// A context the import adds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedContext {
    /// Name in Kubach; differs from `original` when renamed.
    pub name: String,
    pub original: String,
    pub server: Option<String>,
    pub user: Option<String>,
}

impl PlannedContext {
    pub fn is_renamed(&self) -> bool {
        self.name != self.original
    }
}

/// Why a context can't be stored under its planned name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameProblem {
    Empty,
    Duplicate,
    Taken,
}

impl NameProblem {
    /// The problem for the screen; [`Display`](std::fmt::Display) is its
    /// English text.
    pub fn message(self) -> Message {
        match self {
            NameProblem::Empty => msg!("vault-name-empty"),
            NameProblem::Duplicate => msg!("vault-name-duplicate"),
            NameProblem::Taken => msg!("vault-name-taken"),
        }
    }
}

impl std::fmt::Display for NameProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message().english())
    }
}

impl std::error::Error for NameProblem {}

/// A kubeconfig ready to be stored, with a summary for the preview.
#[derive(Clone, Debug)]
pub struct ImportPlan {
    config: Kubeconfig,
    /// Context names defined outside the import.
    taken: HashSet<String>,
    /// Index of the config's current context.
    current: Option<usize>,
    pub contexts: Vec<PlannedContext>,
    pub clusters: Vec<String>,
    pub users: Vec<String>,
    /// Programs run on connect and local files read into the config.
    pub review: ConfigReview,
    /// Whether the files in `review` were read (always for a file; for
    /// pasted YAML after [`read_files`](Self::read_files)).
    files_read: bool,
    /// Files referenced by the config that could not be inlined.
    pub warnings: Vec<Message>,
}

impl ImportPlan {
    /// Plans importing the file at `path`; context names in `taken` are
    /// renamed.
    pub fn from_file(path: &Path, taken: &HashSet<String>) -> Result<Self, FileProblem> {
        // `read` resolves relative certificate paths against the file.
        // It also turns away files that aren't kubeconfigs.
        let config = kubeconfig::read_added(path)?;
        let review = ConfigReview::of(&config, path.parent());
        let mut plan = Self::new(config, review, taken);
        plan.read_files();
        Ok(plan)
    }

    /// Plans importing pasted YAML. The files it refers to are listed in
    /// `review` but not read until [`read_files`](Self::read_files).
    pub fn from_yaml(yaml: &str, taken: &HashSet<String>) -> Result<Self, FileProblem> {
        let config =
            Kubeconfig::from_yaml(yaml).map_err(|error| FileProblem::Invalid(error.to_string()))?;
        let review = ConfigReview::of(&config, None);
        Ok(Self::new(config, review, taken))
    }

    /// Plans importing a config Kubach built itself (clusters imported
    /// from a cloud, #38); it refers to no local files.
    pub fn from_kubeconfig(config: Kubeconfig, taken: &HashSet<String>) -> Self {
        let review = ConfigReview::of(&config, None);
        Self::new(config, review, taken)
    }

    fn new(mut config: Kubeconfig, review: ConfigReview, taken: &HashSet<String>) -> Self {
        let current = config.current_context.as_deref().and_then(|current| {
            config
                .contexts
                .iter()
                .position(|named| named.name == current)
        });
        let mut used: HashSet<String> = taken.clone();
        let mut contexts = Vec::with_capacity(config.contexts.len());
        for (ix, named) in config.contexts.iter_mut().enumerate() {
            let original = named.name.clone();
            let name = unique_name(&original, "imported", &used);
            used.insert(name.clone());
            if name != original {
                if current == Some(ix) {
                    config.current_context = Some(name.clone());
                }
                named.name = name.clone();
            }
            let context = named.context.as_ref();
            let cluster = context.map(|c| c.cluster.as_str()).unwrap_or_default();
            contexts.push(PlannedContext {
                name,
                original,
                server: config
                    .clusters
                    .iter()
                    .find(|c| c.name == cluster)
                    .and_then(|c| c.cluster.as_ref()?.server.clone()),
                user: context.and_then(|c| c.user.clone()),
            });
        }
        Self {
            clusters: config.clusters.iter().map(|c| c.name.clone()).collect(),
            users: config.auth_infos.iter().map(|u| u.name.clone()).collect(),
            contexts,
            files_read: review.files.is_empty(),
            review,
            warnings: Vec::new(),
            config,
            taken: taken.clone(),
            current,
        }
    }

    /// Whether the config refers to local files the user hasn't agreed to
    /// read yet (pasted YAML). Such a plan can't be stored.
    pub fn unread_files(&self) -> bool {
        !self.files_read
    }

    /// Reads the files listed in `review` into the config; ones that can't
    /// be read are reported in `warnings` and stay referenced.
    pub fn read_files(&mut self) {
        if !self.files_read {
            self.warnings.extend(inline_files(&mut self.config));
            self.files_read = true;
        }
    }

    /// Stores context `index` under `name` (trimmed). Out-of-range indexes
    /// are ignored.
    pub fn rename(&mut self, index: usize, name: &str) {
        let name = name.trim();
        let (Some(planned), Some(named)) = (
            self.contexts.get_mut(index),
            self.config.contexts.get_mut(index),
        ) else {
            return;
        };
        planned.name = name.to_owned();
        named.name = name.to_owned();
        if self.current == Some(index) {
            self.config.current_context = Some(name.to_owned());
        }
    }

    /// What is wrong with the name of context `index`, if anything: empty,
    /// used twice in this import, or already defined outside it.
    pub fn problem(&self, index: usize) -> Option<NameProblem> {
        let name = &self.contexts.get(index)?.name;
        if name.trim().is_empty() {
            Some(NameProblem::Empty)
        } else if self.contexts.iter().filter(|c| &c.name == name).count() > 1 {
            Some(NameProblem::Duplicate)
        } else if self.taken.contains(name) {
            Some(NameProblem::Taken)
        } else {
            None
        }
    }

    /// Whether `name` is a context defined outside the import.
    pub fn is_taken(&self, name: &str) -> bool {
        self.taken.contains(name)
    }

    /// Whether every context name can be stored.
    pub fn names_valid(&self) -> bool {
        (0..self.contexts.len()).all(|index| self.problem(index).is_none())
    }

    /// Names of the contexts as they will be stored.
    pub fn context_names(&self) -> Vec<String> {
        self.contexts.iter().map(|c| c.name.clone()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.contexts.is_empty()
    }

    /// The config as it will be stored, to connect to before storing it.
    /// Files not read yet are left out, so testing a pasted config never
    /// sends a local file to its server.
    pub fn to_contexts(&self) -> crate::Contexts {
        let mut config = self.config.clone();
        if !self.files_read {
            drop_files(&mut config);
        }
        crate::Contexts::from_kubeconfig(config)
    }

    /// The config to store, as YAML.
    pub fn yaml(&self) -> Result<Zeroizing<String>, String> {
        serde_saphyr::to_string(&self.config)
            .map(Zeroizing::new)
            .map_err(|error| error.to_string())
    }
}

/// `name`, or `name-<suffix>`, `name-<suffix>-2`, … when taken.
pub(crate) fn unique_name(name: &str, suffix: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(name) {
        return name.to_owned();
    }
    let base = format!("{name}-{suffix}");
    std::iter::once(base.clone())
        .chain((2..).map(|n| format!("{base}-{n}")))
        .find(|candidate| !taken.contains(candidate))
        .expect("an unused name exists")
}

/// Renames clusters and users of `config` whose names are in `clusters` or
/// `users` (defined by kubeconfig files merged before it), so its contexts
/// never pick up another file's server or credentials. A context that
/// refers to a cluster or user the config doesn't define itself, but an
/// earlier file does, is pointed at an unused name instead: kube-rs then
/// reports the missing cluster, or connects without credentials, rather
/// than using the other file's.
pub(crate) fn isolate_names(
    config: &mut Kubeconfig,
    clusters: &HashSet<String>,
    users: &HashSet<String>,
    tag: &str,
) {
    let defined: HashSet<String> = config.clusters.iter().map(|c| c.name.clone()).collect();
    let mut taken = clusters.clone();
    taken.extend(defined.iter().cloned());
    let mut renamed = Vec::new();
    for named in &mut config.clusters {
        if clusters.contains(&named.name) {
            let new = unique_name(&named.name, tag, &taken);
            taken.insert(new.clone());
            renamed.push((named.name.clone(), new.clone()));
            named.name = new;
        }
    }
    for context in config
        .contexts
        .iter_mut()
        .filter_map(|c| c.context.as_mut())
    {
        if let Some((_, new)) = renamed.iter().find(|(old, _)| *old == context.cluster) {
            context.cluster = new.clone();
        } else if !defined.contains(&context.cluster) && clusters.contains(&context.cluster) {
            let new = unique_name(&context.cluster, tag, &taken);
            taken.insert(new.clone());
            renamed.push((context.cluster.clone(), new.clone()));
            context.cluster = new;
        }
    }

    let defined: HashSet<String> = config.auth_infos.iter().map(|u| u.name.clone()).collect();
    let mut taken = users.clone();
    taken.extend(defined.iter().cloned());
    let mut renamed = Vec::new();
    for named in &mut config.auth_infos {
        if users.contains(&named.name) {
            let new = unique_name(&named.name, tag, &taken);
            taken.insert(new.clone());
            renamed.push((named.name.clone(), new.clone()));
            named.name = new;
        }
    }
    for context in config
        .contexts
        .iter_mut()
        .filter_map(|c| c.context.as_mut())
    {
        let Some(user) = context.user.clone() else {
            continue;
        };
        if let Some((_, new)) = renamed.iter().find(|(old, _)| *old == user) {
            context.user = Some(new.clone());
        } else if !defined.contains(&user) && users.contains(&user) {
            let new = unique_name(&user, tag, &taken);
            taken.insert(new.clone());
            renamed.push((user, new.clone()));
            context.user = Some(new);
        }
    }
}

fn read_file(path: &str, warnings: &mut Vec<Message>) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            warnings.push(msg!(
                "vault-import-unreadable-file",
                path = path,
                error = error.to_string()
            ));
            None
        }
    }
}

/// Removes the references to certificate, key and token files.
fn drop_files(config: &mut Kubeconfig) {
    for cluster in config
        .clusters
        .iter_mut()
        .filter_map(|c| c.cluster.as_mut())
    {
        cluster.certificate_authority = None;
    }
    for user in config
        .auth_infos
        .iter_mut()
        .filter_map(|u| u.auth_info.as_mut())
    {
        user.client_certificate = None;
        user.client_key = None;
        user.token_file = None;
    }
}

/// Moves certificate, key and token files into the config.
fn inline_files(config: &mut Kubeconfig) -> Vec<Message> {
    let engine = base64::engine::general_purpose::STANDARD;
    let mut warnings = Vec::new();
    for cluster in config
        .clusters
        .iter_mut()
        .filter_map(|c| c.cluster.as_mut())
    {
        if cluster.certificate_authority_data.is_none()
            && let Some(path) = cluster.certificate_authority.clone()
            && let Some(bytes) = read_file(&path, &mut warnings)
        {
            cluster.certificate_authority_data = Some(engine.encode(bytes));
            cluster.certificate_authority = None;
        }
    }
    for user in config
        .auth_infos
        .iter_mut()
        .filter_map(|u| u.auth_info.as_mut())
    {
        if user.client_certificate_data.is_none()
            && let Some(path) = user.client_certificate.clone()
            && let Some(bytes) = read_file(&path, &mut warnings)
        {
            user.client_certificate_data = Some(engine.encode(bytes));
            user.client_certificate = None;
        }
        if user.client_key_data.is_none()
            && let Some(path) = user.client_key.clone()
            && let Some(bytes) = read_file(&path, &mut warnings).map(Zeroizing::new)
        {
            user.client_key_data = Some(SecretString::from(engine.encode(bytes.as_slice())));
            user.client_key = None;
        }
        if user.token.is_none()
            && let Some(path) = user.token_file.clone()
            && let Some(bytes) = read_file(&path, &mut warnings).map(Zeroizing::new)
        {
            let token = String::from_utf8_lossy(&bytes).trim().to_owned();
            user.token = Some(SecretString::from(token));
            user.token_file = None;
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const CONFIG: &str = r#"
apiVersion: v1
kind: Config
current-context: prod
clusters:
  - name: prod-cluster
    cluster: { server: "https://prod.example.com" }
users:
  - name: admin
    user: { token: s3cr3t }
  - name: eks
    user:
      exec:
        apiVersion: client.authentication.k8s.io/v1beta1
        command: aws
        args: [eks, get-token, --cluster-name, prod]
contexts:
  - name: prod
    context: { cluster: prod-cluster, user: admin }
  - name: prod-eks
    context: { cluster: prod-cluster, user: eks, namespace: web }
"#;

    fn names(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn plan_lists_what_is_added() {
        let plan = ImportPlan::from_yaml(CONFIG, &HashSet::new()).unwrap();
        assert_eq!(plan.context_names(), ["prod", "prod-eks"]);
        assert!(!plan.contexts.iter().any(PlannedContext::is_renamed));
        assert_eq!(
            plan.contexts[0].server.as_deref(),
            Some("https://prod.example.com")
        );
        assert_eq!(plan.clusters, ["prod-cluster"]);
        assert_eq!(plan.users, ["admin", "eks"]);
        assert_eq!(plan.review.programs.len(), 1);
        assert_eq!(
            plan.review.programs[0].command_line(),
            "aws eks get-token --cluster-name prod"
        );
        assert!(plan.warnings.is_empty());
        // The stored YAML keeps the secrets.
        assert!(plan.yaml().unwrap().contains("s3cr3t"));
    }

    #[test]
    fn clashing_context_names_are_renamed() {
        let plan = ImportPlan::from_yaml(CONFIG, &names(&["prod", "prod-imported"])).unwrap();
        assert_eq!(plan.context_names(), ["prod-imported-2", "prod-eks"]);
        assert!(plan.contexts[0].is_renamed());
        assert_eq!(plan.contexts[0].original, "prod");
        let stored = Kubeconfig::from_yaml(&plan.yaml().unwrap()).unwrap();
        assert_eq!(stored.current_context.as_deref(), Some("prod-imported-2"));
        assert_eq!(stored.contexts[0].name, "prod-imported-2");
    }

    #[test]
    fn manual_rename_updates_the_stored_config() {
        let mut plan = ImportPlan::from_yaml(CONFIG, &names(&["prod"])).unwrap();
        assert_eq!(plan.context_names(), ["prod-imported", "prod-eks"]);
        plan.rename(0, "  staging  ");
        assert_eq!(plan.context_names(), ["staging", "prod-eks"]);
        assert!(plan.contexts[0].is_renamed());
        assert_eq!(plan.contexts[0].original, "prod");
        assert!(plan.names_valid());
        let stored = Kubeconfig::from_yaml(&plan.yaml().unwrap()).unwrap();
        assert_eq!(stored.current_context.as_deref(), Some("staging"));
        assert_eq!(stored.contexts[0].name, "staging");
        // Renaming back to the original is not a rename.
        plan.rename(1, "prod-eks");
        assert!(!plan.contexts[1].is_renamed());
        // Out of range: ignored.
        plan.rename(5, "x");
        assert_eq!(plan.context_names(), ["staging", "prod-eks"]);
    }

    #[test]
    fn name_problems_are_detected() {
        let mut plan = ImportPlan::from_yaml(CONFIG, &names(&["dev"])).unwrap();
        assert!(plan.names_valid());
        assert_eq!(plan.problem(0), None);
        assert_eq!(plan.problem(9), None);

        plan.rename(0, "   ");
        assert_eq!(plan.problem(0), Some(NameProblem::Empty));
        assert!(!plan.names_valid());

        plan.rename(0, "dev");
        assert_eq!(plan.problem(0), Some(NameProblem::Taken));
        assert_eq!(plan.problem(1), None);

        plan.rename(0, "prod-eks");
        assert_eq!(plan.problem(0), Some(NameProblem::Duplicate));
        assert_eq!(plan.problem(1), Some(NameProblem::Duplicate));
        assert!(!plan.names_valid());

        plan.rename(1, "eks");
        assert!(plan.names_valid());
        assert_eq!(plan.context_names(), ["prod-eks", "eks"]);
    }

    #[test]
    fn renaming_back_to_a_taken_name_conflicts() {
        // The auto-rename picked a free name; taking the taken one back is
        // reported, not silently fixed.
        let mut plan = ImportPlan::from_yaml(CONFIG, &names(&["prod"])).unwrap();
        assert!(plan.names_valid());
        assert!(plan.is_taken("prod"));
        assert!(!plan.is_taken("prod-eks"));
        plan.rename(0, "prod");
        assert_eq!(plan.problem(0), Some(NameProblem::Taken));
    }

    #[test]
    fn invalid_yaml_is_reported() {
        assert!(matches!(
            ImportPlan::from_yaml("contexts: [oops: {", &HashSet::new()),
            Err(FileProblem::Invalid(_))
        ));
    }

    #[test]
    fn referenced_files_are_inlined() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ca.crt"), "CA").unwrap();
        std::fs::write(dir.path().join("token"), "tok\n").unwrap();
        let path = dir.path().join("config");
        std::fs::write(
            &path,
            r#"
apiVersion: v1
kind: Config
clusters:
  - name: c
    cluster: { server: "https://c", certificate-authority: ca.crt }
users:
  - name: u
    user: { tokenFile: token, client-key: missing.key }
contexts:
  - name: x
    context: { cluster: c, user: u }
"#,
        )
        .unwrap();
        let plan = ImportPlan::from_file(&path, &HashSet::new()).unwrap();
        let yaml = plan.yaml().unwrap();
        let stored = Kubeconfig::from_yaml(&yaml).unwrap();
        let cluster = stored.clusters[0].cluster.as_ref().unwrap();
        assert_eq!(cluster.certificate_authority, None);
        assert_eq!(cluster.certificate_authority_data.as_deref(), Some("Q0E="));
        let user = stored.auth_infos[0].auth_info.as_ref().unwrap();
        assert_eq!(user.token_file, None);
        assert!(yaml.contains("token: tok\n"));
        assert_eq!(plan.warnings.len(), 1);
        assert!(plan.warnings[0].english().contains("missing.key"));
    }

    #[test]
    fn plan_lists_auth_provider_commands_and_exec_env() {
        let yaml = CONFIG.replace(
            "user: { token: s3cr3t }",
            "user: { auth-provider: { name: gcp, config: { cmd-path: /usr/bin/true, \
             cmd-args: \"--flag value\" } } }",
        )
        .replace(
            "args: [eks, get-token, --cluster-name, prod]",
            "args: [eks, get-token, --cluster-name, prod]\n        \
             env: [{ name: AWS_PROFILE, value: p }, { name: NODE_OPTIONS, value: \"--require /x\" }]",
        );
        let plan = ImportPlan::from_yaml(&yaml, &HashSet::new()).unwrap();
        let programs = &plan.review.programs;
        assert_eq!(programs.len(), 2);
        assert_eq!(programs[0].user, "admin");
        assert_eq!(
            programs[0].kind,
            crate::kubeconfig::review::ProgramKind::AuthProvider("gcp".into())
        );
        assert_eq!(programs[0].command_line(), "/usr/bin/true --flag value");
        assert_eq!(programs[1].user, "eks");
        let env: Vec<_> = programs[1]
            .env
            .iter()
            .map(|v| (v.name.as_str(), v.value.as_str(), v.dangerous))
            .collect();
        assert_eq!(
            env,
            [
                ("AWS_PROFILE", "p", false),
                ("NODE_OPTIONS", "--require /x", true)
            ]
        );
        assert!(plan.review.has_dangerous_env());
    }

    #[test]
    fn pasted_yaml_reads_files_only_after_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("kubach-test-token");
        std::fs::write(&token, "local-secret\n").unwrap();
        let yaml = CONFIG.replace(
            "user: { token: s3cr3t }",
            // Single quotes: a Windows path's backslashes are no escapes.
            &format!("user: {{ tokenFile: '{}' }}", token.display()),
        );
        let mut plan = ImportPlan::from_yaml(&yaml, &HashSet::new()).unwrap();
        assert!(plan.unread_files());
        assert_eq!(plan.review.files.len(), 1);
        let file = &plan.review.files[0];
        assert_eq!(file.owner, "admin");
        assert_eq!(file.field.key(), "tokenFile");
        assert_eq!(file.path, token.display().to_string());
        // Pasted: no folder, so outside.
        assert!(file.outside);
        // Not read: neither stored nor used to test the connection.
        assert!(!plan.yaml().unwrap().contains("local-secret"));
        let tested = plan.to_contexts();
        assert!(tested.get("prod").is_some());
        assert!(!format!("{tested:?}").contains("kubach-test-token"));

        plan.read_files();
        assert!(!plan.unread_files());
        assert!(plan.yaml().unwrap().contains("local-secret"));
        assert!(plan.warnings.is_empty());
        // The list stays for the preview.
        assert_eq!(plan.review.files.len(), 1);
    }

    #[test]
    fn pasted_yaml_without_files_needs_no_confirmation() {
        let plan = ImportPlan::from_yaml(CONFIG, &HashSet::new()).unwrap();
        assert!(!plan.unread_files());
        assert!(plan.review.files.is_empty());
    }

    #[test]
    fn file_import_lists_inlined_files_and_flags_outside_ones() {
        let dir = tempfile::tempdir().unwrap();
        let kube = dir.path().join("kube");
        std::fs::create_dir_all(&kube).unwrap();
        std::fs::write(kube.join("ca.crt"), "CA").unwrap();
        let elsewhere = dir.path().join("kubach-test-npmrc");
        std::fs::write(&elsewhere, "npm-token").unwrap();
        let path = kube.join("config");
        std::fs::write(
            &path,
            format!(
                r#"
apiVersion: v1
kind: Config
clusters:
  - name: c
    cluster: {{ server: "https://c.invalid", certificate-authority: ca.crt }}
users:
  - name: u
    user: {{ tokenFile: '{}' }}
contexts:
  - name: x
    context: {{ cluster: c, user: u }}
"#,
                elsewhere.display()
            ),
        )
        .unwrap();
        let plan = ImportPlan::from_file(&path, &HashSet::new()).unwrap();
        assert!(!plan.unread_files());
        let files: Vec<_> = plan
            .review
            .files
            .iter()
            .map(|f| (f.owner.as_str(), f.field.key(), f.outside))
            .collect();
        assert_eq!(
            files,
            [
                ("c", "certificate-authority", false),
                ("u", "tokenFile", true)
            ]
        );
        assert!(plan.review.files[1].path.ends_with("kubach-test-npmrc"));
        assert!(plan.yaml().unwrap().contains("npm-token"));
    }

    #[test]
    fn isolation_breaks_references_to_other_files() {
        // The context names a user and cluster it doesn't define; earlier
        // files do. It must not borrow them.
        let mut config = Kubeconfig::from_yaml(
            r#"
apiVersion: v1
kind: Config
clusters:
  - name: evil
    cluster: { server: "https://attacker.invalid" }
contexts:
  - name: x
    context: { cluster: evil, user: kubernetes-admin }
  - name: y
    context: { cluster: kubernetes, user: kubernetes-admin }
  - name: z
    context: { cluster: evil, user: nobody }
"#,
        )
        .unwrap();
        isolate_names(
            &mut config,
            &names(&["kubernetes"]),
            &names(&["kubernetes-admin"]),
            "dropped",
        );
        let ctx = |ix: usize| config.contexts[ix].context.clone().unwrap();
        assert_eq!(ctx(0).cluster, "evil");
        assert_eq!(ctx(0).user.as_deref(), Some("kubernetes-admin-dropped"));
        assert_eq!(ctx(1).cluster, "kubernetes-dropped");
        assert_eq!(ctx(1).user.as_deref(), Some("kubernetes-admin-dropped"));
        // Unknown everywhere: left alone.
        assert_eq!(ctx(2).user.as_deref(), Some("nobody"));
        assert!(config.auth_infos.is_empty());
    }

    #[test]
    fn importing_a_file_that_is_not_a_kubeconfig_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.yaml");
        std::fs::write(
            &path,
            "apiVersion: v1\nkind: Service\nmetadata: { name: web }\n",
        )
        .unwrap();
        let error = ImportPlan::from_file(&path, &HashSet::new()).unwrap_err();
        assert!(matches!(&error, FileProblem::Invalid(reason) if reason.contains("Service")));
        assert_eq!(
            error.to_string(),
            "Not a valid kubeconfig: it is a Service, expected kind: Config"
        );
    }

    #[test]
    fn isolation_renames_taken_clusters_and_users() {
        let mut config = Kubeconfig::from_yaml(CONFIG).unwrap();
        isolate_names(
            &mut config,
            &names(&["prod-cluster"]),
            &names(&["admin"]),
            "team",
        );
        assert_eq!(config.clusters[0].name, "prod-cluster-team");
        assert_eq!(config.auth_infos[0].name, "admin-team");
        assert_eq!(config.auth_infos[1].name, "eks");
        let prod = config.contexts[0].context.as_ref().unwrap();
        assert_eq!(prod.cluster, "prod-cluster-team");
        assert_eq!(prod.user.as_deref(), Some("admin-team"));
        let eks = config.contexts[1].context.as_ref().unwrap();
        assert_eq!(eks.cluster, "prod-cluster-team");
        assert_eq!(eks.user.as_deref(), Some("eks"));
    }
}
