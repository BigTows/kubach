//! Install panel of the Charts tab (ADR 0006): a repository chart (or an
//! `oci://…` reference), release name, namespace (an existing one or a new
//! one) and values that start from the chart defaults, an optional
//! server-side `helm install --dry-run` preview of the manifest, then the
//! real install, which the Helm screen confirms and runs on
//! [`InstallEvent::Apply`].
//!
//! The panel re-renders on every keystroke (and every frame of a spinner),
//! so render reads no editor text: the values check and whether the editor
//! still holds the defaults are updated on edits, the preview's diff rows
//! are folded once and drawn virtualized, and staleness is a revision
//! counter instead of comparing the whole input.
//!
//! A failed install keeps the panel open with the failure in place of the
//! preview, so closing the progress toast loses nothing (#271); a failed
//! preview or install over custom resources without CRDs is explained
//! ([`missing_crds_explanation`], #261), with helm's text under a toggle.
//!
//! A typed `oci://` chart gets its versions listed from the registry
//! ([`OciVersions`], #301) with the newest stable one entered, and a release
//! name from the chart name, both until the user changes them.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::helm::cli::{
    self, HelmCommand, HelmTarget, InstallRequest, ShowKind, UpgradeOutcome,
};
use kubach_core::helm::manifest::ChangeKind;
use kubach_core::helm::oci;
use kubach_core::helm::repo::{chart_reference, check_namespace_name, check_release_name};
use kubach_core::helm::target_namespace::{self, Decision, Listing, Lookup, NamespaceStatus};
use kubach_core::helm::{ManifestDiff, Release, Sensitive, check_values_yaml, diff_manifests};
use kubach_core::runtime;
use kubach_i18n::t;
use kubach_ui::{DiffRow, DiffView, EmptyState, StatusBadge, Tone, fold_rows};

use super::helm::HelmScreen;
use super::helm_release::{missing_crds_text, validation_text};
use super::namespace_picker::{NamespaceListing, Namespaces};
use super::oci_versions::{OciVersions, version_label};

/// Pause after typing a namespace before it is looked up (when the
/// namespaces can't be listed).
const LOOKUP_DELAY: Duration = Duration::from_millis(300);

/// `GET namespace/<name>` of the namespace entered, when the namespaces
/// can't be listed.
struct NamespaceLookup {
    namespace: String,
    /// `None` while it runs.
    result: Option<Lookup>,
    /// Asked the API server (not the fallback without a connection).
    asked: bool,
    _task: Task<()>,
}

/// An install to confirm and run.
#[derive(Clone)]
pub struct InstallInput {
    pub target: HelmTarget,
    pub request: InstallRequest,
    /// A preview of this very input succeeded.
    pub previewed: bool,
}

pub enum InstallEvent {
    /// Confirm and run this install.
    Apply(Box<InstallInput>),
    Cancel,
}

/// The chart being installed.
#[derive(Clone)]
pub enum InstallSource {
    /// `repo/chart` with the versions of the repository index, newest first.
    Repository {
        repository: String,
        chart: String,
        versions: Vec<String>,
    },
    /// A chart typed in: `oci://…`, a path or a URL. `devel` lists
    /// pre-releases of an `oci://` chart.
    Reference { devel: bool },
}

/// Runs `helm show <kind>` on the I/O runtime; errors as text.
pub(super) fn show(
    cx: &App,
    kind: ShowKind,
    chart: String,
    version: Option<String>,
) -> impl Future<Output = Result<String, String>> + 'static {
    let helm = HelmScreen::helm_path(cx);
    let handle = runtime::spawn(async move {
        let command = HelmCommand::show(kind, &chart, version.as_deref());
        cli::run(helm.as_deref(), &command, cli::DEFAULT_TIMEOUT)
            .await
            .map_err(|error| error.to_string())
    });
    async move {
        handle
            .await
            .unwrap_or_else(|error| Err(t!("helm-call-interrupted", error = error.to_string())))
    }
}

/// Chart and version the defaults were loaded for.
type DefaultsKey = (String, Option<String>);

/// Chart defaults for the editor.
enum Defaults {
    None,
    Loading { key: DefaultsKey, _task: Task<()> },
    Loaded(DefaultsKey),
    Failed { key: DefaultsKey, error: String },
}

enum PreviewState {
    Running,
    Ready {
        release: Box<Release>,
        diff: ManifestDiff,
        /// Folded once; drawn virtualized.
        rows: Arc<[DiffRow]>,
    },
    Failed(Failure),
}

/// A failed `helm` call: what it means, when known, and helm's text.
struct Failure {
    explanation: Option<String>,
    output: String,
}

impl Failure {
    fn new(output: String) -> Self {
        Self {
            explanation: missing_crds_text(&output),
            output,
        }
    }
}

struct Preview {
    /// [`HelmInstall::revision`] it was made for; edits after it make it
    /// stale.
    revision: u64,
    namespace: String,
    state: PreviewState,
    _task: Task<()>,
}

pub struct HelmInstall {
    /// Context, kubeconfig and driver; the namespace comes from the form.
    target: HelmTarget,
    source: InstallSource,
    /// Picked version of a repository chart.
    version: String,
    /// Chart and version of a reference install.
    chart_input: Entity<InputState>,
    version_input: Entity<InputState>,
    /// Versions of a typed `oci://` chart.
    oci: Entity<OciVersions>,
    /// Release name last filled in from the typed chart: while the field
    /// holds it (or nothing), a new chart replaces it.
    auto_release: String,
    /// Version last filled in from the registry list, likewise.
    auto_version: String,
    release: Entity<InputState>,
    namespace: Entity<InputState>,
    /// The `--create-namespace` checkbox, `None` until clicked; a namespace
    /// the cluster does not have is created anyway ([`target_namespace`]).
    create_asked: Option<bool>,
    /// Namespaces of the cluster: the picker, and telling a new namespace
    /// from an existing one.
    namespaces: Entity<Namespaces>,
    /// Existence check of the namespace entered when they can't be listed.
    lookup: Option<NamespaceLookup>,
    values: Entity<EditorState>,
    /// Why the values are not valid YAML, checked on edits.
    values_error: Option<String>,
    /// The editor is empty or still holds the loaded defaults.
    values_pristine: bool,
    defaults: Defaults,
    /// Text last put into the editor from the defaults: while the editor
    /// still holds it, newly loaded defaults replace it.
    applied: String,
    /// Bumped on every change of the input.
    revision: u64,
    preview: Option<Preview>,
    /// The last install from this panel failed; shown instead of the
    /// preview until the next preview or install.
    install_failure: Option<Failure>,
    /// helm's text of a failure is shown (it is folded when explained).
    output_open: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<InstallEvent> for HelmInstall {}

impl HelmInstall {
    /// `version` is the one picked on the chart card (repository charts).
    pub fn new(
        target: HelmTarget,
        source: InstallSource,
        version: Option<String>,
        namespace: String,
        namespaces: Entity<Namespaces>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (release_name, version) = match &source {
            InstallSource::Repository {
                chart, versions, ..
            } => (
                chart.clone(),
                version
                    .or_else(|| versions.first().cloned())
                    .unwrap_or_default(),
            ),
            InstallSource::Reference { .. } => (String::new(), String::new()),
        };
        let devel = matches!(source, InstallSource::Reference { devel: true });
        let oci = cx.new(|_| OciVersions::new(devel));
        let values = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("yaml")
                .line_number(true)
        });
        let chart_input = cx.new(|cx| InputState::new(window, cx));
        let version_input = cx.new(|cx| InputState::new(window, cx));
        let release = cx.new(|cx| InputState::new(window, cx).default_value(release_name));
        let namespace = cx.new(|cx| InputState::new(window, cx).default_value(namespace));
        let changed = |this: &mut Self, _, event: &InputEvent, cx: &mut Context<Self>| {
            if let InputEvent::Change = event {
                this.changed(cx);
            }
        };
        let mut subscriptions = vec![
            cx.subscribe_in(
                &chart_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::Change = event {
                        this.chart_changed(window, cx);
                    }
                },
            ),
            cx.observe_in(&oci, window, |this, _, window, cx| {
                this.versions_listed(window, cx)
            }),
            cx.subscribe(&version_input, changed),
            cx.subscribe(&release, changed),
            cx.subscribe(&namespace, |this, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    this.changed(cx);
                    this.sync_lookup(cx);
                }
            }),
            cx.subscribe(&values, |this, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    this.check_values(cx);
                    this.changed(cx);
                }
            }),
            cx.observe(&namespaces, |this, _, cx| {
                this.sync_lookup(cx);
                cx.notify();
            }),
        ];
        for (input, text) in [
            (
                &chart_input,
                (|| t!("helm-install-chart-placeholder")) as fn() -> String,
            ),
            (&version_input, || t!("helm-version-latest")),
            (&release, || t!("helm-install-release-placeholder")),
            (&namespace, || t!("helm-install-namespace-placeholder")),
        ] {
            subscriptions.extend(crate::i18n::localized_placeholder(input, text, window, cx));
        }
        let is_repository = matches!(source, InstallSource::Repository { .. });
        let mut this = Self {
            target,
            source,
            version,
            chart_input,
            version_input,
            oci,
            auto_release: String::new(),
            auto_version: String::new(),
            release,
            namespace,
            create_asked: None,
            namespaces,
            lookup: None,
            values,
            values_error: None,
            values_pristine: true,
            defaults: Defaults::None,
            applied: String::new(),
            revision: 0,
            preview: None,
            install_failure: None,
            output_open: false,
            _subscriptions: subscriptions,
        };
        if is_repository {
            this.load_defaults(false, window, cx);
        }
        this.sync_lookup(cx);
        this
    }

    /// Focus of the first field to fill in.
    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        let input = match self.source {
            InstallSource::Repository { .. } => &self.release,
            InstallSource::Reference { .. } => &self.chart_input,
        };
        input.focus_handle(cx)
    }

    /// The input changed: a preview made before is stale.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.revision += 1;
        cx.notify();
    }

    /// The typed chart changed: the release name follows it (until edited),
    /// a version filled in for the previous chart goes, and the versions of
    /// an `oci://` chart are listed.
    fn chart_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chart = self.chart_input.read(cx).value().trim().to_owned();
        let release = self.release.read(cx).value().trim().to_owned();
        if release.is_empty() || release == self.auto_release {
            let name = oci::release_name(&chart).unwrap_or_default();
            if name != release {
                let value = name.clone();
                self.release
                    .update(cx, |input, cx| input.set_value(value, window, cx));
            }
            self.auto_release = name;
        }
        let version = self.version_input.read(cx).value().trim().to_owned();
        if !self.auto_version.is_empty() && version == self.auto_version {
            self.version_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.auto_version.clear();
        }
        self.oci.update(cx, |oci, cx| oci.set_chart(&chart, cx));
        self.changed(cx);
    }

    /// Versions of the `oci://` chart were listed: the newest stable one is
    /// entered unless the user typed a version, and its defaults load.
    fn versions_listed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(newest) = oci::newest_stable(self.oci.read(cx).versions()).map(str::to_owned)
        else {
            return;
        };
        let version = self.version_input.read(cx).value().trim().to_owned();
        let untouched = version.is_empty() || version == self.auto_version;
        if !untouched || (version == newest && self.auto_version == newest) {
            return;
        }
        self.set_reference_version(newest, window, cx);
    }

    /// Enters `version` of the `oci://` chart (from the registry list) and
    /// loads its defaults.
    fn set_reference_version(
        &mut self,
        version: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = version.clone();
        self.version_input
            .update(cx, |input, cx| input.set_value(value, window, cx));
        self.auto_version = version;
        self.changed(cx);
        self.load_defaults(false, window, cx);
    }

    /// Checks the values YAML once per edit (not on every render).
    fn check_values(&mut self, cx: &App) {
        let text = self.values.read(cx).value().to_string();
        self.values_error = check_values_yaml(&text)
            .err()
            .map(|error| t!("helm-values-error", error = validation_text(&error)));
        self.values_pristine = text.trim().is_empty() || text == self.applied;
    }

    /// Chart and version as entered.
    fn chart(&self, cx: &App) -> DefaultsKey {
        match &self.source {
            InstallSource::Repository {
                repository, chart, ..
            } => (
                chart_reference(repository, chart),
                (!self.version.is_empty()).then(|| self.version.clone()),
            ),
            InstallSource::Reference { .. } => {
                let version = self.version_input.read(cx).value().trim().to_owned();
                (
                    self.chart_input.read(cx).value().trim().to_owned(),
                    (!version.is_empty()).then_some(version),
                )
            }
        }
    }

    /// Loads the chart defaults (`helm show values`) of the chart as
    /// entered. They go into the editor when it still holds the previous
    /// defaults (or nothing), or always with `force`.
    fn load_defaults(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.chart(cx);
        if key.0.is_empty() {
            return;
        }
        let call = show(cx, ShowKind::Values, key.0.clone(), key.1.clone());
        let task_key = key.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            this.update_in(cx, |this, window, cx| {
                // A newer request replaced this one.
                if !matches!(&this.defaults, Defaults::Loading { key, .. } if *key == task_key) {
                    return;
                }
                this.defaults = match result {
                    Ok(text) => {
                        if force || this.values_pristine {
                            this.applied = text.clone();
                            this.values
                                .update(cx, |editor, cx| editor.set_value(text, window, cx));
                            this.check_values(cx);
                            this.changed(cx);
                        }
                        Defaults::Loaded(task_key)
                    }
                    Err(error) => Defaults::Failed {
                        key: task_key,
                        error,
                    },
                };
                cx.notify();
            })
            .ok();
        });
        self.defaults = Defaults::Loading { key, _task: task };
        cx.notify();
    }

    fn pick_version(&mut self, version: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.version != version {
            self.version = version;
            self.changed(cx);
            self.load_defaults(false, window, cx);
        }
    }

    fn pick_namespace(&mut self, namespace: String, window: &mut Window, cx: &mut Context<Self>) {
        self.namespace
            .update(cx, |input, cx| input.set_value(namespace, window, cx));
        self.changed(cx);
    }

    fn entered_namespace(&self, cx: &App) -> String {
        self.namespace.read(cx).value().trim().to_owned()
    }

    /// Whether the namespace entered exists and whether the install gets
    /// `--create-namespace` (or waits until that is known).
    fn decision(&self, cx: &App) -> Decision {
        let namespace = self.entered_namespace(cx);
        match self.namespaces.read(cx).listing(cx) {
            NamespaceListing::Loading => target_namespace::decide(
                &namespace,
                Listing::<SharedString>::Loading,
                self.create_asked,
            ),
            NamespaceListing::Listed(names) => {
                target_namespace::decide(&namespace, Listing::Listed(&names), self.create_asked)
            }
            NamespaceListing::Unavailable => {
                let result = self
                    .lookup
                    .as_ref()
                    .filter(|lookup| lookup.namespace == namespace)
                    .and_then(|lookup| lookup.result);
                target_namespace::decide(
                    &namespace,
                    Listing::<SharedString>::Unlisted(result),
                    self.create_asked,
                )
            }
        }
    }

    /// When the namespaces can't be listed, looks up the namespace entered
    /// (after a pause in typing); otherwise drops the lookup.
    fn sync_lookup(&mut self, cx: &mut Context<Self>) {
        let namespaces = self.namespaces.read(cx);
        let unavailable = matches!(namespaces.listing(cx), NamespaceListing::Unavailable);
        let client = namespaces.client(cx);
        let namespace = self.entered_namespace(cx);
        if !unavailable || check_namespace_name(&namespace).is_err() {
            self.lookup = None;
            return;
        }
        if self.lookup.as_ref().is_some_and(|lookup| {
            // A fallback is redone once connected.
            lookup.namespace == namespace && (lookup.asked || client.is_none())
        }) {
            return;
        }
        let Some(client) = client else {
            // Not connected: nothing to ask, so the check is inconclusive
            // rather than pending forever.
            self.lookup = Some(NamespaceLookup {
                namespace,
                result: Some(Lookup::Unknown { forbidden: false }),
                asked: false,
                _task: Task::ready(()),
            });
            return;
        };
        let name = namespace.clone();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LOOKUP_DELAY).await;
            let lookup_name = name.clone();
            let handle =
                runtime::spawn(
                    async move { target_namespace::lookup(&client, &lookup_name).await },
                );
            let result = handle.await.unwrap_or(Lookup::Unknown { forbidden: false });
            this.update(cx, |this, cx| {
                if let Some(lookup) = &mut this.lookup
                    && lookup.namespace == name
                {
                    lookup.result = Some(result);
                }
                cx.notify();
            })
            .ok();
        });
        self.lookup = Some(NamespaceLookup {
            namespace,
            result: None,
            asked: true,
            _task: task,
        });
    }

    /// The install as entered, without the values text (cheap enough for
    /// every render), or why it can't run.
    fn check(&self, cx: &App) -> Result<(HelmTarget, InstallRequest), String> {
        let (chart, version) = self.chart(cx);
        if chart.is_empty() {
            return Err(t!("helm-install-enter-chart"));
        }
        let release = self.release.read(cx).value().trim().to_owned();
        check_release_name(&release).map_err(|error| validation_text(&error))?;
        let namespace = self.namespace.read(cx).value().trim().to_owned();
        check_namespace_name(&namespace).map_err(|error| validation_text(&error))?;
        if self.values_error.is_some() {
            return Err(t!("helm-install-fix-values"));
        }
        let mut target = self.target.clone();
        target.namespace = namespace;
        Ok((
            target,
            InstallRequest {
                release,
                chart,
                version,
                values: Sensitive::new(String::new()),
                // Helm does not create a missing namespace by itself: the
                // install would fail (its preview would not). Until the
                // namespace is checked the panel does not run anything.
                create_namespace: self.decision(cx).create.unwrap_or(false),
            },
        ))
    }

    /// The install as entered, with the values.
    fn input(&self, cx: &App) -> Result<(HelmTarget, InstallRequest), String> {
        let (target, mut request) = self.check(cx)?;
        request.values = Sensitive::new(self.values.read(cx).value().to_string());
        Ok((target, request))
    }

    /// A successful preview of the current input.
    fn previewed(&self) -> bool {
        self.preview.as_ref().is_some_and(|preview| {
            preview.revision == self.revision && matches!(preview.state, PreviewState::Ready { .. })
        })
    }

    fn preview_running(&self) -> bool {
        self.preview
            .as_ref()
            .is_some_and(|p| matches!(p.state, PreviewState::Running))
    }

    /// `--create-namespace` is not decided yet: Preview and Install wait.
    fn waiting(&self, cx: &App) -> bool {
        self.decision(cx).create.is_none()
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        if self.waiting(cx) {
            return;
        }
        let Ok((target, request)) = self.input(cx) else {
            return;
        };
        let previewed = self.previewed();
        self.install_failure = None;
        cx.notify();
        cx.emit(InstallEvent::Apply(Box::new(InstallInput {
            target,
            request,
            previewed,
        })));
    }

    /// The install started from this panel failed with `error`.
    pub fn install_failed(&mut self, error: String, cx: &mut Context<Self>) {
        self.install_failure = Some(Failure::new(error));
        self.output_open = false;
        cx.notify();
    }

    fn run_preview(&mut self, cx: &mut Context<Self>) {
        if self.waiting(cx) {
            return;
        }
        let Ok((target, request)) = self.input(cx) else {
            return;
        };
        self.install_failure = None;
        self.output_open = false;
        let helm = HelmScreen::helm_path(cx);
        let namespace = target.namespace.clone();
        let handle = runtime::spawn(async move {
            let outcome = cli::install(
                helm.as_deref(),
                &target,
                &request,
                true,
                cli::DEFAULT_TIMEOUT,
            )
            .await;
            match outcome {
                Ok(UpgradeOutcome::Preview(release)) => {
                    let manifest = release.manifest.expose();
                    let diff = diff_manifests("", manifest);
                    let rows = fold_rows(&crate::editor::diff_view_lines("", manifest));
                    PreviewState::Ready {
                        release,
                        diff,
                        rows,
                    }
                }
                Ok(UpgradeOutcome::Applied(_)) => {
                    PreviewState::Failed(Failure::new(t!("helm-no-preview")))
                }
                Err(error) => PreviewState::Failed(Failure::new(error.to_string())),
            }
        });
        let task = cx.spawn(async move |this, cx| {
            let state = handle.await.unwrap_or_else(|error| {
                PreviewState::Failed(Failure::new(t!(
                    "helm-preview-interrupted",
                    error = error.to_string()
                )))
            });
            this.update(cx, |this, cx| {
                if let Some(preview) = &mut this.preview {
                    preview.state = state;
                }
                cx.notify();
            })
            .ok();
        });
        self.preview = Some(Preview {
            revision: self.revision,
            namespace,
            state: PreviewState::Running,
            _task: task,
        });
        cx.notify();
    }

    fn render_chart_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(t!("helm-field-chart"));
        match &self.source {
            InstallSource::Repository {
                repository,
                chart,
                versions,
            } => {
                let current = self.version.clone();
                let versions = versions.clone();
                let panel = cx.entity().downgrade();
                let picker = Button::new("helm-install-version")
                    .outline()
                    .small()
                    .label(if current.is_empty() {
                        t!("helm-version-latest")
                    } else {
                        current.clone()
                    })
                    .dropdown_menu(move |menu: PopupMenu, _, _| {
                        let menu = menu.scrollable(true).max_h(px(360.));
                        versions.iter().fold(menu, |menu, version| {
                            let (panel, version) = (panel.clone(), version.clone());
                            menu.item(
                                PopupMenuItem::new(version.clone())
                                    .checked(version == current)
                                    .on_click(move |_, window, cx| {
                                        let version = version.clone();
                                        panel
                                            .update(cx, |this, cx| {
                                                this.pick_version(version, window, cx)
                                            })
                                            .ok();
                                    }),
                            )
                        })
                    });
                row = row
                    .child(
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.foreground)
                            .child(chart_reference(repository, chart)),
                    )
                    .child(t!("helm-label-version"))
                    .child(picker);
            }
            InstallSource::Reference { .. } => {
                let panel = cx.entity().downgrade();
                let current = self.version_input.read(cx).value().trim().to_owned();
                let picker = self.oci.read(cx).picker(
                    "helm-install-oci-versions",
                    current,
                    version_label,
                    move |version, window, cx| {
                        panel
                            .update(cx, |this, cx| {
                                this.set_reference_version(version, window, cx)
                            })
                            .ok();
                    },
                );
                row = row
                    .child(
                        div()
                            .w(px(340.))
                            .child(Input::new(&self.chart_input).small()),
                    )
                    .child(t!("helm-label-version"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                // Wide enough for `1.27.3-debian-12-r12`.
                                div()
                                    .w(px(220.))
                                    .child(Input::new(&self.version_input).small()),
                            )
                            .children(picker),
                    );
            }
        }
        let note = self.oci.read(cx).note(cx);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(row)
            .children(note)
            .into_any_element()
    }

    /// Namespaces of the cluster to pick from; a name can be typed too.
    fn render_namespace_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let namespaces = self.namespaces.read(cx);
        let names = namespaces.names();
        let note = namespaces.note();
        let current = self.namespace.read(cx).value().trim().to_owned();
        let panel = cx.entity().downgrade();
        Button::new("helm-install-namespaces")
            .outline()
            .small()
            .icon(IconName::ChevronDown)
            .tooltip(
                note.map(|note| note.to_string())
                    .unwrap_or_else(|| t!("helm-install-pick-namespace")),
            )
            .disabled(names.is_empty())
            .dropdown_menu(move |menu: PopupMenu, _, _| {
                let menu = menu.scrollable(true).max_h(px(360.));
                names.iter().fold(menu, |menu, name| {
                    let (panel, name) = (panel.clone(), name.to_string());
                    menu.item(
                        PopupMenuItem::new(name.clone())
                            .checked(name == current)
                            .on_click(move |_, window, cx| {
                                let name = name.clone();
                                panel
                                    .update(cx, |this, cx| this.pick_namespace(name, window, cx))
                                    .ok();
                            }),
                    )
                })
            })
            .into_any_element()
    }

    fn render_target_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let decision = self.decision(cx);
        let namespace = self.entered_namespace(cx);
        let listing_loading = matches!(
            self.namespaces.read(cx).listing(cx),
            NamespaceListing::Loading
        );
        let picker = self.render_namespace_picker(cx);
        let theme = cx.theme();
        let row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(t!("helm-label-release"))
            .child(div().w(px(220.)).child(Input::new(&self.release).small()))
            .child(t!("helm-label-namespace"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().w(px(200.)).child(Input::new(&self.namespace).small()))
                    .child(picker),
            )
            .child(
                Checkbox::new("helm-install-create-namespace")
                    .label(t!("helm-install-create-namespace"))
                    .checked(decision.create.unwrap_or(false))
                    // A new namespace is always created.
                    .disabled(!decision.editable())
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.create_asked = Some(*checked);
                        this.changed(cx);
                    })),
            );
        // `helm install --dry-run=server` neither creates the namespace nor
        // sends the objects to the API server, so a preview in a new
        // namespace renders like in an existing one.
        let note = match decision.status {
            NamespaceStatus::New => Some((
                t!("helm-install-namespace-new", namespace = namespace.clone()),
                theme.muted_foreground,
            )),
            NamespaceStatus::Checking if decision.create.is_none() => Some((
                if listing_loading {
                    t!(
                        "helm-install-namespaces-loading",
                        namespace = namespace.clone()
                    )
                } else {
                    t!(
                        "helm-install-namespace-checking",
                        namespace = namespace.clone()
                    )
                },
                theme.muted_foreground,
            )),
            NamespaceStatus::Unverified { forbidden } => Some((
                if forbidden {
                    t!(
                        "helm-install-namespace-unverified-forbidden",
                        namespace = namespace.clone()
                    )
                } else {
                    t!(
                        "helm-install-namespace-unverified",
                        namespace = namespace.clone()
                    )
                },
                theme.warning,
            )),
            _ => None,
        }
        .map(|(text, color)| {
            div()
                .text_xs()
                .text_color(color)
                .whitespace_normal()
                .child(text)
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(row)
            .children(note)
            .into_any_element()
    }

    fn render_values(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let key = self.chart(cx);
        let loading = matches!(self.defaults, Defaults::Loading { .. });
        let (note, color) = match &self.defaults {
            Defaults::None => (t!("helm-values-user"), theme.muted_foreground),
            Defaults::Loading { key, .. } => (
                t!("helm-values-defaults-loading", chart = key_label(key)),
                theme.muted_foreground,
            ),
            Defaults::Loaded(loaded) if *loaded == key => {
                (t!("helm-values-from-defaults"), theme.muted_foreground)
            }
            Defaults::Loaded(loaded) => (
                t!("helm-values-edited", chart = key_label(loaded)),
                theme.warning,
            ),
            Defaults::Failed { key, error } => (
                t!(
                    "helm-values-defaults-failed",
                    chart = key_label(key),
                    error = error.clone()
                ),
                theme.danger,
            ),
        };
        let reset = Button::new("helm-install-defaults")
            .ghost()
            .xsmall()
            .icon(IconName::RefreshCw)
            .label(t!("helm-values-defaults-button"))
            .tooltip(t!("helm-values-defaults-tooltip"))
            .disabled(loading || key.0.is_empty())
            .on_click(cx.listener(|this, _, window, cx| this.load_defaults(true, window, cx)));
        // While the defaults load into an untouched editor, show that
        // instead of an empty editor (edited values stay visible).
        let body = if loading && self.values_pristine {
            EmptyState::new(
                IconName::LoaderCircle,
                t!("helm-values-defaults-loading-title"),
            )
            .description("helm show values")
            .into_any_element()
        } else {
            div()
                .size_full()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(Editor::new(&self.values).h_full().bordered(false))
                .into_any_element()
        };
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(color)
                            .whitespace_normal()
                            .child(note),
                    )
                    .child(reset),
            )
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .when_some(self.values_error.clone(), |el, error| {
                el.child(
                    div()
                        .flex()
                        .items_start()
                        .gap_1()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(Icon::new(IconName::CircleX).xsmall().mt_0p5())
                        .child(div().flex_1().min_w_0().whitespace_normal().child(error)),
                )
            })
            .into_any_element()
    }

    /// A failed preview or install: the explanation, then helm's text
    /// (folded when explained), wrapped.
    fn render_failure(
        &self,
        title: String,
        note: String,
        failure: &Failure,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.output_open || failure.explanation.is_none();
        let toggle = cx.listener(|this, _, _, cx| {
            this.output_open = !this.output_open;
            cx.notify();
        });
        let output = helm_output(
            "helm-install-output",
            &failure.output,
            open,
            failure.explanation.is_some().then_some(toggle),
            cx,
        );
        let theme = cx.theme();
        div()
            .id("helm-install-failure")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.danger)
                    .child(Icon::new(IconName::CircleX).small())
                    .child(title),
            )
            .children(failure.explanation.clone().map(|text| {
                div()
                    .text_sm()
                    .text_color(theme.foreground)
                    .whitespace_normal()
                    .child(text)
            }))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .whitespace_normal()
                    .child(note),
            )
            .child(output)
            .into_any_element()
    }

    fn render_preview(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(failure) = &self.install_failure {
            return self.render_failure(
                t!("helm-install-failed"),
                t!("helm-install-failed-note"),
                failure,
                cx,
            );
        }
        let theme = cx.theme();
        let Some(preview) = &self.preview else {
            return EmptyState::new(IconName::Eye, t!("helm-install-preview-title"))
                .description(t!("helm-install-preview-description"))
                .into_any_element();
        };
        let (release, diff, rows) = match &preview.state {
            PreviewState::Running => {
                return EmptyState::new(IconName::LoaderCircle, t!("helm-install-rendering"))
                    .into_any_element();
            }
            PreviewState::Failed(failure) => {
                return self.render_failure(
                    t!("helm-preview-failed"),
                    t!("helm-install-preview-failed-note"),
                    failure,
                    cx,
                );
            }
            PreviewState::Ready {
                release,
                diff,
                rows,
            } => (release, diff, rows),
        };
        let stale_note = (preview.revision != self.revision).then(|| {
            div()
                .text_xs()
                .text_color(theme.warning)
                .child(t!("helm-install-preview-stale"))
        });
        let header = div().text_xs().text_color(theme.muted_foreground).child(t!(
            "helm-install-preview-summary",
            name = release.name.clone(),
            namespace = preview.namespace.clone(),
            revision = release.revision,
            chart = release.chart.reference(),
            count = diff.changes.len()
        ));
        let changes = div()
            .flex()
            .flex_wrap()
            .gap_1()
            .children(diff.changes.iter().map(|change| {
                let (sign, tone) = match change.kind {
                    ChangeKind::Added => ("+", Tone::Success),
                    ChangeKind::Changed => ("~", Tone::Warning),
                    ChangeKind::Removed => ("−", Tone::Danger),
                };
                StatusBadge::new(format!("{sign} {}", change.resource), tone)
            }));
        let body = if rows.is_empty() {
            EmptyState::new(IconName::Inbox, t!("helm-install-no-objects"))
                .description(t!("helm-install-no-objects-description"))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(DiffView::from_rows("helm-install-diff", rows.clone()))
                .into_any_element()
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .children(stale_note)
            .child(header)
            .when(!diff.changes.is_empty(), |el| el.child(changes))
            .child(body)
            .into_any_element()
    }
}

/// helm's text of a call: wrapped, left-aligned, in a monospace block;
/// with `toggle` it folds under a "helm output" switch.
pub(super) fn helm_output(
    id: &'static str,
    text: &str,
    open: bool,
    toggle: Option<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let switch = toggle.map(|toggle| {
        Button::new(id)
            .ghost()
            .xsmall()
            .icon(if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .label(if open {
                t!("helm-output-hide")
            } else {
                t!("helm-output-show")
            })
            .on_click(toggle)
    });
    let body = open.then(|| {
        div()
            .w_full()
            .p_2()
            .rounded_md()
            .bg(theme.muted)
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .text_color(theme.foreground)
            .whitespace_normal()
            .child(text.trim().to_owned())
    });
    div()
        .flex()
        .flex_col()
        .items_start()
        .gap_1()
        .children(switch)
        .children(body)
        .into_any_element()
}

fn key_label((chart, version): &DefaultsKey) -> String {
    match version {
        Some(version) => format!("{chart} {version}"),
        None => chart.clone(),
    }
}

impl Render for HelmInstall {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let input = self.check(cx);
        let running = self.preview_running();
        let waiting = self.waiting(cx);
        let namespace = self.entered_namespace(cx);
        let chart_row = self.render_chart_row(cx);
        let target_row = self.render_target_row(cx);
        let values = self.render_values(cx);
        let preview = self.render_preview(cx);
        let theme = cx.theme();
        let (status, status_color) = match &input {
            // `--create-namespace` is not known yet.
            Ok(_) if waiting => (
                t!(
                    "helm-install-namespace-checking",
                    namespace = namespace.clone()
                ),
                theme.muted_foreground,
            ),
            Ok((target, request)) => (
                HelmCommand::install(target, request, false).display(),
                theme.muted_foreground,
            ),
            Err(error) => (error.clone(), theme.warning),
        };
        let footer = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(status_color)
                    .child(status),
            )
            .child(
                Button::new("helm-install-cancel")
                    .ghost()
                    .small()
                    .label(t!("helm-cancel"))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(InstallEvent::Cancel))),
            )
            .child(
                Button::new("helm-install-preview")
                    .outline()
                    .small()
                    .icon(IconName::Eye)
                    .label(t!("helm-preview"))
                    .loading(waiting && input.is_ok())
                    .disabled(input.is_err() || running || waiting)
                    .on_click(cx.listener(|this, _, _, cx| this.run_preview(cx))),
            )
            .child(
                Button::new("helm-install-apply")
                    .primary()
                    .small()
                    .icon(IconName::Plus)
                    .label(t!("helm-install-button"))
                    .loading(waiting && input.is_ok())
                    .disabled(input.is_err() || running || waiting)
                    .on_click(cx.listener(|this, _, _, cx| this.apply(cx))),
            );
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(chart_row)
            .child(target_row)
            .child(
                div()
                    .flex_1()
                    .min_h(px(240.))
                    .flex()
                    .gap_3()
                    .child(values)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(preview),
                    ),
            )
            .child(footer)
    }
}
