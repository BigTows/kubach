//! Kustomize builds through the user's `kustomize` or `kubectl kustomize`
//! (ADR 0007), with a diff of the result against the live cluster.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use kube::Client;
use serde_json::{Map, Value};

use crate::apply::diff::{Change, DiffMode, semantic_diff, unified_diff};
use crate::apply::edit::strip_for_edit;
use crate::apply::resolve::{ResolveError, ResolvedManifest, resolve, resolve_all};
use crate::apply::yaml::{Manifest, YamlError, parse, to_yaml};
use crate::discovery::{self, ApiCatalog};
use crate::process::{self, ProcessError};
use crate::secrets::mask::{is_core_secret, masked_copy};
use crate::which::which;

/// File names kustomize accepts as the kustomization of a directory.
pub const KUSTOMIZATION_FILES: [&str; 3] =
    ["kustomization.yaml", "kustomization.yml", "Kustomization"];

/// The binary that runs the build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Standalone `kustomize build`.
    Kustomize(PathBuf),
    /// `kubectl kustomize` (bundled kustomize, usually older).
    Kubectl(PathBuf),
}

impl Tool {
    /// Looks up `kustomize`, then `kubectl`, in `PATH`.
    pub fn find() -> Option<Self> {
        Self::find_in(&crate::env::var_os("PATH")?)
    }

    /// Like [`find`](Self::find), with an explicit `PATH` value.
    pub fn find_in(path: &OsStr) -> Option<Self> {
        which("kustomize", path)
            .map(Self::Kustomize)
            .or_else(|| which("kubectl", path).map(Self::Kubectl))
    }

    /// A configured binary; `kubectl*` names run `kubectl kustomize`.
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let is_kubectl = path
            .file_stem()
            .and_then(OsStr::to_str)
            .is_some_and(|stem| stem.starts_with("kubectl"));
        if is_kubectl {
            Self::Kubectl(path)
        } else {
            Self::Kustomize(path)
        }
    }

    pub fn program(&self) -> &Path {
        match self {
            Self::Kustomize(path) | Self::Kubectl(path) => path,
        }
    }

    /// Command-line arguments for building `dir`.
    pub fn args(&self, dir: &Path, options: &BuildOptions) -> Vec<OsString> {
        let mut args: Vec<OsString> = match self {
            Self::Kustomize(_) => vec!["build".into()],
            Self::Kubectl(_) => vec!["kustomize".into()],
        };
        args.push(dir.as_os_str().to_owned());
        if options.enable_helm {
            args.push("--enable-helm".into());
        }
        args
    }
}

#[derive(Clone, Debug)]
pub struct BuildOptions {
    /// Inflate `helmCharts` (needs `helm` in `PATH`).
    pub enable_helm: bool,
    pub timeout: Duration,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            enable_helm: false,
            timeout: Duration::from_secs(60),
        }
    }
}

/// Result of a successful build.
#[derive(Clone, Debug)]
pub struct Build {
    pub tool: Tool,
    /// Raw YAML output.
    pub yaml: String,
    pub manifests: Vec<Manifest>,
    /// Warnings printed on stderr (deprecations and the like).
    pub warnings: String,
}

#[derive(Debug, thiserror::Error)]
pub enum KustomizeError {
    #[error("neither `kustomize` nor `kubectl` was found in PATH")]
    NoTool,
    #[error("{} has no kustomization.yaml", .0.display())]
    NotKustomization(PathBuf),
    #[error("failed to run {}: {source}", program.display())]
    Spawn {
        program: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("kustomize build did not finish within {} s", .0.as_secs())]
    Timeout(Duration),
    #[error("kustomize build failed ({status}): {stderr}")]
    Failed { status: String, stderr: String },
    #[error("kustomize output is not UTF-8")]
    NotUtf8,
    #[error("invalid kustomize output: {0}")]
    Yaml(#[from] YamlError),
}

impl KustomizeError {
    /// Short headline for the UI.
    pub fn title(&self) -> &'static str {
        match self {
            Self::NoTool => "kustomize is not available",
            Self::NotKustomization(_) => "Not a kustomization folder",
            Self::Spawn { .. } => "Can't run kustomize",
            Self::Timeout(_) => "Build timed out",
            Self::Failed { .. } => "Build failed",
            Self::NotUtf8 | Self::Yaml(_) => "Unreadable build output",
        }
    }

    /// What the user can do about it.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::NoTool => Some(
                "Install kustomize or kubectl and make sure it is on PATH \
                 (Kubach reads PATH from your login shell).",
            ),
            Self::NotKustomization(_) => Some(
                "Pick a folder that contains kustomization.yaml, kustomization.yml \
                 or Kustomization.",
            ),
            Self::Spawn { .. } => Some("Check that the binary exists and is executable."),
            Self::Timeout(_) => {
                Some("Remote bases and helmCharts need network access; check the URLs.")
            }
            Self::Failed { .. } | Self::NotUtf8 | Self::Yaml(_) => None,
        }
    }

    /// The details: the tool's own message for a failed build (without the
    /// `Error: ` prefix and usage text), the error text otherwise.
    pub fn details(&self) -> String {
        match self {
            Self::Failed { status, stderr } => {
                let message = clean_stderr(stderr);
                if message.is_empty() {
                    format!("kustomize exited with {status}")
                } else {
                    message
                }
            }
            other => other.to_string(),
        }
    }
}

/// The meaningful part of kustomize's stderr: `Error: ` prefixes are
/// dropped and so is the usage text cobra prints after an error.
fn clean_stderr(stderr: &str) -> String {
    let mut lines = Vec::new();
    for line in stderr.lines() {
        if line.starts_with("Usage:") {
            break;
        }
        let line = line.strip_prefix("Error: ").unwrap_or(line);
        let line = line.strip_prefix("error: ").unwrap_or(line);
        lines.push(line);
    }
    lines.join("\n").trim().to_owned()
}

/// How many folders [`remember_dir`] keeps by default.
pub const RECENT_DIRS: usize = 8;

/// Moves `dir` to the front of `recent`, keeping at most `max` entries.
pub fn remember_dir(recent: &mut Vec<PathBuf>, dir: &Path, max: usize) {
    recent.retain(|known| known != dir);
    recent.insert(0, dir.to_owned());
    recent.truncate(max);
}

/// Identity of a built object as written in the output, before it is bound
/// to a cluster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectSummary {
    pub api_version: String,
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
}

impl ObjectSummary {
    pub fn of(manifest: &Manifest) -> Self {
        let str_at = |pointer: &str| {
            manifest
                .value
                .pointer(pointer)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let namespace = str_at("/metadata/namespace");
        Self {
            api_version: str_at("/apiVersion"),
            kind: str_at("/kind"),
            name: str_at("/metadata/name"),
            namespace: (!namespace.is_empty()).then_some(namespace),
        }
    }
}

impl ObjectSummary {
    /// Whether this is a core `Secret`, whose values the UI hides until asked.
    pub fn is_secret(&self) -> bool {
        let group = self.api_version.split_once('/').map_or("", |(g, _)| g);
        is_core_secret(group, &self.kind)
    }
}

/// Replaces the values of a Secret with a fingerprint, see
/// [`crate::secrets::mask::mask_for_diff`]. Other objects are returned as is.
pub fn mask_secret(value: &Value) -> Value {
    masked_copy(value)
}

/// Counts of a cluster comparison, for the summary line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffCounts {
    pub new: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub failed: usize,
}

impl DiffCounts {
    pub fn of<'a>(results: impl IntoIterator<Item = &'a Result<ObjectDiff, String>>) -> Self {
        let mut counts = Self::default();
        for result in results {
            match result {
                Ok(diff) if diff.is_new() => counts.new += 1,
                Ok(diff) if diff.is_unchanged() => counts.unchanged += 1,
                Ok(_) => counts.changed += 1,
                Err(_) => counts.failed += 1,
            }
        }
        counts
    }

    /// E.g. `2 new · 1 changed · 5 unchanged`; empty parts are left out.
    pub fn describe(&self) -> String {
        let parts: Vec<String> = [
            (self.new, "new"),
            (self.changed, "changed"),
            (self.unchanged, "unchanged"),
            (self.failed, "not compared"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, label)| format!("{count} {label}"))
        .collect();
        if parts.is_empty() {
            "nothing to compare".to_owned()
        } else {
            parts.join(" · ")
        }
    }
}

/// Whether `dir` contains a kustomization file.
pub fn is_kustomization_dir(dir: &Path) -> bool {
    KUSTOMIZATION_FILES
        .iter()
        .any(|name| dir.join(name).is_file())
}

/// Builds `dir` with the first tool found in `PATH`.
pub async fn build_with_path_tool(
    dir: &Path,
    options: &BuildOptions,
) -> Result<Build, KustomizeError> {
    let tool = Tool::find().ok_or(KustomizeError::NoTool)?;
    build(&tool, dir, options).await
}

/// Runs `kustomize build` (or `kubectl kustomize`) on `dir` and parses the output.
/// The build is local: no cluster access, nothing is applied.
pub async fn build(
    tool: &Tool,
    dir: &Path,
    options: &BuildOptions,
) -> Result<Build, KustomizeError> {
    if !is_kustomization_dir(dir) {
        return Err(KustomizeError::NotKustomization(dir.to_owned()));
    }
    let program = tool.program();
    let mut command = tokio::process::Command::new(program);
    command.args(tool.args(dir, options));
    let output = process::run(command, options.timeout)
        .await
        .map_err(|error| match error {
            ProcessError::NotFound { .. } => KustomizeError::Spawn {
                program: program.to_owned(),
                source: std::io::ErrorKind::NotFound.into(),
            },
            ProcessError::Spawn { source, .. } => KustomizeError::Spawn {
                program: program.to_owned(),
                source,
            },
            // Not cancellable here: no `Watch` is passed.
            ProcessError::Timeout(timeout) => KustomizeError::Timeout(timeout),
            ProcessError::Cancelled { .. } => KustomizeError::Timeout(options.timeout),
            ProcessError::Failed { status, stderr, .. } => KustomizeError::Failed {
                status: status.to_string(),
                stderr: process::stderr_warnings(&stderr),
            },
        })?;
    let stderr = output.warnings();
    let yaml = String::from_utf8(output.stdout).map_err(|_| KustomizeError::NotUtf8)?;
    Ok(Build {
        tool: tool.clone(),
        manifests: parse(&yaml)?,
        yaml,
        warnings: stderr,
    })
}

/// A built object compared with its live counterpart.
#[derive(Clone, Debug)]
pub struct ObjectDiff {
    pub manifest: ResolvedManifest,
    /// The live object; `None` if it does not exist yet.
    pub live: Option<Value>,
    /// Changes the manifest makes; fields it does not set are ignored.
    pub changes: Vec<Change>,
    /// Unified YAML diff of the live object (limited to the manifest's
    /// fields) against the manifest.
    pub diff: String,
    /// The live object limited to the manifest's fields, as YAML; empty for
    /// a new object.
    pub live_yaml: String,
    /// The manifest as YAML.
    pub desired_yaml: String,
}

impl ObjectDiff {
    pub fn is_new(&self) -> bool {
        self.live.is_none()
    }

    pub fn is_unchanged(&self) -> bool {
        self.live.is_some() && self.changes.is_empty()
    }

    /// `(live, desired)` YAML with Secret values replaced by fingerprints
    /// (see [`mask_secret`]); the same texts as the fields for other kinds.
    pub fn masked_texts(&self) -> (String, String) {
        let desired = mask_secret(&self.manifest.value);
        let live = self
            .live
            .as_ref()
            .map(|live| render(&mask_secret(&project(live, &self.manifest.value))))
            .unwrap_or_default();
        (live, render(&desired))
    }

    /// `New`, `Changed` or `Unchanged`.
    pub fn status(&self) -> &'static str {
        if self.is_new() {
            "New"
        } else if self.is_unchanged() {
            "Unchanged"
        } else {
            "Changed"
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DiffError {
    #[error("{} manifest(s) cannot be resolved: {}", .0.len(), .0.first().map(ToString::to_string).unwrap_or_default())]
    Resolve(Vec<ResolveError>),
    #[error("failed to get {label}: {source}")]
    Get {
        label: String,
        #[source]
        source: Box<kube::Error>,
    },
}

/// Diffs built manifests against the cluster. Namespaced objects without a
/// namespace go to `default_namespace`.
pub async fn diff_live(
    client: &Client,
    catalog: &ApiCatalog,
    manifests: &[Manifest],
    default_namespace: &str,
) -> Result<Vec<ObjectDiff>, DiffError> {
    let resolved =
        resolve_all(manifests, catalog, default_namespace).map_err(DiffError::Resolve)?;
    let mut out = Vec::with_capacity(resolved.len());
    for manifest in resolved {
        let api = discovery::api(client, &manifest.kind, manifest.namespace.as_deref());
        let live = api
            .get_opt(&manifest.name)
            .await
            .map_err(|source| DiffError::Get {
                label: manifest.label(),
                source: Box::new(source),
            })?
            .map(|obj| serde_json::to_value(obj).unwrap_or(Value::Null));
        out.push(diff_object(manifest, live));
    }
    Ok(out)
}

/// Like [`diff_live`], but an object that cannot be resolved or read gets
/// its own error instead of failing the whole diff. Results are in manifest
/// order.
pub async fn diff_each(
    client: &Client,
    catalog: &ApiCatalog,
    manifests: &[Manifest],
    default_namespace: &str,
) -> Vec<Result<ObjectDiff, String>> {
    let mut out = Vec::with_capacity(manifests.len());
    for manifest in manifests {
        let manifest = match resolve(manifest, catalog, default_namespace) {
            Ok(manifest) => manifest,
            Err(error) => {
                out.push(Err(error.kind.to_string()));
                continue;
            }
        };
        let api = discovery::api(client, &manifest.kind, manifest.namespace.as_deref());
        out.push(match api.get_opt(&manifest.name).await {
            Ok(live) => Ok(diff_object(
                manifest,
                live.map(|obj| serde_json::to_value(obj).unwrap_or(Value::Null)),
            )),
            Err(kube::Error::Api(status)) if status.code == 403 => {
                Err(format!("no permission to read {}", manifest.label()))
            }
            Err(error) => Err(format!("failed to get {}: {error}", manifest.label())),
        });
    }
    out
}

/// Compares one resolved manifest with the live object, if any.
pub fn diff_object(manifest: ResolvedManifest, live: Option<Value>) -> ObjectDiff {
    let empty = Value::Object(Map::new());
    let changes = semantic_diff(
        live.as_ref().unwrap_or(&empty),
        &manifest.value,
        DiffMode::Apply,
    );
    let before = live
        .as_ref()
        .map(|live| render(&project(live, &manifest.value)))
        .unwrap_or_default();
    let after = render(&manifest.value);
    let diff = unified_diff(&before, &after, "live", "kustomize");
    ObjectDiff {
        manifest,
        live,
        changes,
        diff,
        live_yaml: before,
        desired_yaml: after,
    }
}

/// YAML of an object as compared: without server-managed fields.
fn render(value: &Value) -> String {
    let mut value = value.clone();
    strip_for_edit(&mut value);
    to_yaml(&value)
}

/// The parts of `live` at paths `desired` sets, in `desired`'s key order, so
/// server defaults and status do not show up as removals.
fn project(live: &Value, desired: &Value) -> Value {
    match (live, desired) {
        (Value::Object(l), Value::Object(d)) => Value::Object(
            d.iter()
                .filter_map(|(k, dv)| l.get(k).map(|lv| (k.clone(), project(lv, dv))))
                .collect(),
        ),
        (Value::Array(l), Value::Array(d)) => {
            let name = |v: &Value| v.get("name").and_then(Value::as_str).map(str::to_owned);
            let named = !l.is_empty() && l.iter().chain(d).all(|v| name(v).is_some());
            let mut out: Vec<Value> = d
                .iter()
                .enumerate()
                .filter_map(|(i, dv)| {
                    let lv = if named {
                        l.iter().find(|lv| name(lv) == name(dv))
                    } else {
                        l.get(i)
                    };
                    lv.map(|lv| project(lv, dv))
                })
                .collect();
            // Extra live items stay visible: applying would remove them.
            if !named {
                out.extend(l.iter().skip(d.len()).cloned());
            }
            Value::Array(out)
        }
        _ => live.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use kube::api::{ApiResource, GroupVersionKind};
    use serde_json::json;

    use super::*;
    use crate::apply::ChangeKind;
    use crate::discovery::ResourceKind;

    /// A fresh directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "kubach-kustomize-{tag}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture() -> TempDir {
        let dir = TempDir::new("build");
        fs::write(
            dir.0.join("kustomization.yaml"),
            "\
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
namespace: shop
namePrefix: dev-
labels:
  - pairs:
      app: web
resources:
  - deployment.yaml
configMapGenerator:
  - name: settings
    literals:
      - MODE=dev
",
        )
        .unwrap();
        fs::write(
            dir.0.join("deployment.yaml"),
            "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
spec:
  replicas: 2
  selector:
    matchLabels: {app: web}
  template:
    metadata:
      labels: {app: web}
    spec:
      containers:
        - name: web
          image: nginx:1.27
",
        )
        .unwrap();
        dir
    }

    #[tokio::test]
    async fn builds_fixture_with_path_tool() {
        let Some(tool) = Tool::find() else {
            eprintln!("skipped: neither kustomize nor kubectl in PATH");
            return;
        };
        let dir = fixture();
        let build = build(&tool, &dir.0, &BuildOptions::default())
            .await
            .unwrap();
        let kinds: Vec<_> = build
            .manifests
            .iter()
            .map(|m| m.value["kind"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(kinds, ["ConfigMap", "Deployment"], "{}", build.yaml);
        let cm = &build.manifests[0].value;
        assert!(
            cm["metadata"]["name"]
                .as_str()
                .unwrap()
                .starts_with("dev-settings-"),
            "{cm}"
        );
        assert_eq!(cm["metadata"]["namespace"], "shop");
        assert_eq!(cm["data"]["MODE"], "dev");
        let deploy = &build.manifests[1].value;
        assert_eq!(deploy["metadata"]["name"], "dev-web");
        assert_eq!(deploy["metadata"]["labels"]["app"], "web");
    }

    #[tokio::test]
    async fn build_errors() {
        let empty = TempDir::new("empty");
        let tool = Tool::Kustomize(PathBuf::from("/nonexistent/kustomize"));
        let err = build(&tool, &empty.0, &BuildOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, KustomizeError::NotKustomization(_)), "{err}");

        let dir = fixture();
        let err = build(&tool, &dir.0, &BuildOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, KustomizeError::Spawn { .. }), "{err}");

        // A broken kustomization reports the tool's stderr.
        let Some(tool) = Tool::find() else {
            return;
        };
        fs::write(
            dir.0.join("kustomization.yaml"),
            "resources:\n  - missing.yaml\n",
        )
        .unwrap();
        let err = build(&tool, &dir.0, &BuildOptions::default())
            .await
            .unwrap_err();
        let KustomizeError::Failed { stderr, .. } = &err else {
            panic!("{err}");
        };
        assert!(stderr.contains("missing.yaml"), "{stderr}");
    }

    #[test]
    fn error_texts() {
        let failed = KustomizeError::Failed {
            status: "exit status: 1".into(),
            stderr: "Error: accumulating resources: missing.yaml: no such file\n\
                     Usage:\n  kustomize build DIR [flags]\n"
                .into(),
        };
        assert_eq!(failed.title(), "Build failed");
        assert_eq!(
            failed.details(),
            "accumulating resources: missing.yaml: no such file"
        );
        assert_eq!(failed.hint(), None);
        let silent = KustomizeError::Failed {
            status: "exit status: 2".into(),
            stderr: String::new(),
        };
        assert_eq!(silent.details(), "kustomize exited with exit status: 2");
        assert!(KustomizeError::NoTool.hint().unwrap().contains("PATH"));
        let not_dir = KustomizeError::NotKustomization(PathBuf::from("/k"));
        assert_eq!(not_dir.details(), "/k has no kustomization.yaml");
    }

    #[test]
    fn recent_dirs() {
        let mut recent = Vec::new();
        for dir in ["/a", "/b", "/c", "/b"] {
            remember_dir(&mut recent, Path::new(dir), 2);
        }
        assert_eq!(recent, [PathBuf::from("/b"), PathBuf::from("/c")]);
    }

    #[test]
    fn object_summary() {
        let manifests = parse(
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: a\n  namespace: shop\n---\n\
             apiVersion: rbac.authorization.k8s.io/v1\nkind: ClusterRole\nmetadata:\n  name: r\n",
        )
        .unwrap();
        assert_eq!(
            ObjectSummary::of(&manifests[0]),
            ObjectSummary {
                api_version: "v1".into(),
                kind: "ConfigMap".into(),
                name: "a".into(),
                namespace: Some("shop".into()),
            }
        );
        assert_eq!(ObjectSummary::of(&manifests[1]).namespace, None);
        assert!(!ObjectSummary::of(&manifests[0]).is_secret());
    }

    #[test]
    fn secret_masking() {
        let secret = json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": "db"},
            "data": {"password": "aHVudGVyMg==", "user": "YWRtaW4="},
            "stringData": {"token": "s3cr3t-tok"},
        });
        let masked = mask_secret(&secret);
        let shown = to_yaml(&masked);
        assert!(!shown.contains("aHVudGVyMg=="), "{shown}");
        assert!(!shown.contains("s3cr3t-tok"), "{shown}");
        assert!(shown.contains("password: hidden-"), "{shown}");
        assert_eq!(masked["metadata"], secret["metadata"]);
        // Same value, same fingerprint; another value, another one.
        assert_eq!(mask_secret(&secret), masked);
        let mut changed = secret.clone();
        changed["data"]["password"] = json!("b3RoZXI=");
        assert_ne!(
            mask_secret(&changed)["data"]["password"],
            masked["data"]["password"]
        );
        assert_eq!(
            mask_secret(&changed)["data"]["user"],
            masked["data"]["user"]
        );
        // Not a Secret: untouched.
        let config = json!({"kind": "ConfigMap", "data": {"a": "b"}});
        assert_eq!(mask_secret(&config)["data"], config["data"]);
        let summary = ObjectSummary::of(&Manifest {
            index: 0,
            line: 1,
            value: secret,
        });
        assert!(summary.is_secret());
    }

    #[test]
    fn diff_counts() {
        let new = diff_object(resolved(desired()), None);
        let mut same = desired();
        same["spec"]["replicas"] = json!(3);
        let unchanged = diff_object(resolved(desired()), Some(same));
        let results = vec![
            Ok(new.clone()),
            Ok(new),
            Ok(unchanged),
            Err("forbidden".into()),
        ];
        let counts = DiffCounts::of(&results);
        assert_eq!(
            counts,
            DiffCounts {
                new: 2,
                changed: 0,
                unchanged: 1,
                failed: 1
            }
        );
        assert_eq!(counts.describe(), "2 new · 1 unchanged · 1 not compared");
        assert_eq!(DiffCounts::default().describe(), "nothing to compare");

        let desired_secret = json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": "db", "namespace": "shop"},
            "data": {"password": "bmV3"},
        });
        let mut live_secret = desired_secret.clone();
        live_secret["data"]["password"] = json!("b2xk");
        let mut manifest = resolved(desired_secret);
        manifest.value["kind"] = json!("Secret");
        let d = diff_object(manifest, Some(live_secret));
        let (live, desired) = d.masked_texts();
        assert!(!live.contains("b2xk") && !desired.contains("bmV3"));
        assert_ne!(live, desired);
        assert!(d.live_yaml.contains("b2xk"));
    }

    #[test]
    fn tool_lookup_and_args() {
        assert_eq!(Tool::find_in(OsStr::new("")), None);
        let empty = TempDir::new("path");
        assert_eq!(Tool::find_in(empty.0.as_os_str()), None);

        let opts = BuildOptions {
            enable_helm: true,
            ..BuildOptions::default()
        };
        let dir = Path::new("/k");
        assert_eq!(
            Tool::from_path("/usr/bin/kubectl").args(dir, &opts),
            ["kustomize", "/k", "--enable-helm"].map(OsString::from)
        );
        assert_eq!(
            Tool::from_path("/opt/kustomize").args(dir, &BuildOptions::default()),
            ["build", "/k"].map(OsString::from)
        );
    }

    #[cfg(unix)]
    #[test]
    fn prefers_standalone_kustomize() {
        use std::os::unix::fs::PermissionsExt;
        let bin = TempDir::new("bin");
        for name in ["kubectl", "kustomize"] {
            let path = bin.0.join(name);
            fs::write(&path, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(
            Tool::find_in(bin.0.as_os_str()),
            Some(Tool::Kustomize(bin.0.join("kustomize")))
        );
        fs::remove_file(bin.0.join("kustomize")).unwrap();
        assert_eq!(
            Tool::find_in(bin.0.as_os_str()),
            Some(Tool::Kubectl(bin.0.join("kubectl")))
        );
    }

    fn resolved(value: Value) -> ResolvedManifest {
        let gvk = GroupVersionKind::gvk("apps", "v1", "Deployment");
        ResolvedManifest {
            index: 0,
            line: 1,
            kind: ResourceKind {
                api: ApiResource::from_gvk_with_plural(&gvk, "deployments"),
                namespaced: true,
                verbs: Vec::new(),
            },
            name: "web".into(),
            namespace: Some("shop".into()),
            value,
        }
    }

    fn desired() -> Value {
        json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {"name": "web", "namespace": "shop"},
            "spec": {"replicas": 3, "template": {"spec": {"containers": [
                {"name": "web", "image": "nginx:1.27"}
            ]}}}
        })
    }

    #[test]
    fn diff_against_live() {
        let live = json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {"name": "web", "namespace": "shop", "uid": "u1", "resourceVersion": "7"},
            "spec": {
                "replicas": 2, "revisionHistoryLimit": 10,
                "template": {"spec": {"containers": [
                    {"name": "web", "image": "nginx:1.27", "imagePullPolicy": "IfNotPresent"}
                ]}}
            },
            "status": {"replicas": 2}
        });
        let d = diff_object(resolved(desired()), Some(live.clone()));
        assert!(!d.is_new() && !d.is_unchanged());
        assert_eq!(d.changes.len(), 1, "{:?}", d.changes);
        assert_eq!(d.changes[0].kind, ChangeKind::Modified);
        assert_eq!(d.changes[0].after, Some(json!(3)));
        // Server defaults and status are not shown as removals.
        assert!(d.diff.contains("-  replicas: 2"), "{}", d.diff);
        assert!(d.diff.contains("+  replicas: 3"), "{}", d.diff);
        assert!(!d.diff.contains("revisionHistoryLimit"), "{}", d.diff);
        assert!(!d.diff.contains("imagePullPolicy"), "{}", d.diff);

        let mut same = live;
        same["spec"]["replicas"] = json!(3);
        assert!(diff_object(resolved(desired()), Some(same)).is_unchanged());

        assert_eq!(d.status(), "Changed");
        assert!(d.live_yaml.contains("replicas: 2"), "{}", d.live_yaml);
        assert!(d.desired_yaml.contains("replicas: 3"), "{}", d.desired_yaml);

        let new = diff_object(resolved(desired()), None);
        assert!(new.is_new());
        assert_eq!(new.status(), "New");
        assert!(new.live_yaml.is_empty());
        assert!(!new.changes.is_empty());
        assert!(new.diff.contains("+kind: Deployment"), "{}", new.diff);
    }
}
