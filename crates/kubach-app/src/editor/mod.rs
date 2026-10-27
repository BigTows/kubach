//! YAML editor tabs: edit a live object or create new ones, with schema
//! completion, hovers and diagnostics, a server-side dry-run review and
//! server-side apply.
//!
//! Each editor tab is bound to the connection it was opened with: switching
//! contexts in the main window never redirects a review or an apply.

mod analysis;
mod assist;
mod configmap;
mod presets;
mod review;
mod secret;
mod tabs;
mod value;

pub use presets::new_button;
pub use review::diff_view_lines;
pub use secret::SecretTarget;
pub use tabs::EditorTabs;
pub(crate) use value::{binary_text, size_text};

use kubach_ui::ButtonLoading as _;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::base::input::{Diagnostic, DiagnosticSeverity};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    Editor, EditorState, InputEvent, InputState, Position, ShowCompletions,
};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::apply::{
    self, ApplyOptions, ChangeKind, FieldPath, ResolvedManifest, SchemaCache,
};
use kubach_core::{ApiCatalog, Connection, ConnectionState, ResourceKind, discovery, runtime};
use kubach_i18n::t;
use kubach_ui::tokens::TINT_OPACITY;
use kubach_ui::{DiffView, EmptyState, SegmentTab, SegmentTabs, Tone};
use kube::Client;
use kube::api::{DynamicObject, GroupVersionKind};
use serde_json::Value;

use crate::cluster::{CatalogState, Cluster};
use analysis::{Analysis, Debounce, Problem, Severity, analyze};
use assist::{SchemaAssist, SchemaSlot, SharedSchemas};
use review::{Failure, ReviewItem, review_item};

/// Quiet time after typing before the text is checked again.
const ANALYSIS_DEBOUNCE: Duration = Duration::from_millis(300);
const REVIEW_LIST_WIDTH: Pixels = px(380.);
const PROBLEMS_MAX_HEIGHT: Pixels = px(150.);
const KEY_CONTEXT: &str = "YamlEditor";

/// The example shown in an empty new-resources editor.
fn new_placeholder() -> String {
    format!(
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: example\ndata:\n  key: value\n---\n# {}",
        t!("editor-placeholder-more")
    )
}

/// Keeps the placeholder of a code editor in the current language, like
/// [`crate::i18n::localized_placeholder`] does for inputs.
fn localized_editor_placeholder<V: 'static>(
    editor: &Entity<EditorState>,
    text: fn() -> String,
    window: &mut Window,
    cx: &mut Context<V>,
) -> Option<Subscription> {
    editor.update(cx, |editor, cx| editor.set_placeholder(text(), window, cx));
    let language = crate::i18n::global(cx)?;
    let editor = editor.clone();
    Some(cx.observe_in(&language, window, move |_, _, window, cx| {
        editor.update(cx, |editor, cx| editor.set_placeholder(text(), window, cx));
    }))
}

actions!(yaml_editor, [ReviewChanges]);

/// Key bindings of the editor and its tabs, and the hub that routes open
/// requests to the main window.
pub fn init(cx: &mut App) {
    let hub = cx.new(|_| EditorHub);
    cx.set_global(GlobalHub(hub));
    tabs::init(cx);
    cx.bind_keys([
        KeyBinding::new("secondary-s", ReviewChanges, Some(KEY_CONTEXT)),
        KeyBinding::new(
            "ctrl-space",
            ShowCompletions,
            Some("Input && mode == editor"),
        ),
    ]);
}

/// What the editor works on.
#[derive(Clone)]
pub enum Target {
    /// A live object, loaded fresh from the cluster.
    Existing {
        kind: ResourceKind,
        name: String,
        namespace: Option<String>,
    },
    /// New objects from typed YAML (several documents allowed), starting
    /// from a template or preset, or empty.
    New {
        namespace: String,
        template: Option<presets::NewText>,
    },
}

impl Target {
    /// `ConfigMap web/app` style, for the toolbar.
    fn title(&self) -> String {
        match self {
            Target::Existing {
                kind,
                name,
                namespace: Some(namespace),
            } => format!("{} {namespace}/{name}", kind.api.kind),
            Target::Existing { kind, name, .. } => format!("{} {name}", kind.api.kind),
            Target::New { .. } => t!("editor-title-new"),
        }
    }

    /// `ConfigMap/app` style, for the tab.
    fn tab_title(&self) -> String {
        match self {
            Target::Existing { kind, name, .. } => format!("{}/{name}", kind.api.kind),
            Target::New {
                template:
                    Some(presets::NewText {
                        kind: Some(kind), ..
                    }),
                ..
            } => t!("editor-tab-new-kind", kind = kind),
            Target::New { .. } => t!("editor-tab-new"),
        }
    }

    /// Both edit the same live object.
    fn same_object(&self, other: &Target) -> bool {
        match (self, other) {
            (
                Target::Existing {
                    kind,
                    name,
                    namespace,
                },
                Target::Existing {
                    kind: other_kind,
                    name: other_name,
                    namespace: other_namespace,
                },
            ) => {
                kind.api.group == other_kind.api.group
                    && kind.api.kind == other_kind.api.kind
                    && name == other_name
                    && namespace == other_namespace
            }
            _ => false,
        }
    }

    fn default_namespace(&self) -> &str {
        match self {
            Target::Existing { namespace, .. } => namespace.as_deref().unwrap_or("default"),
            Target::New { namespace, .. } => namespace,
        }
    }

    /// Whether `manifest` is the edited object itself.
    fn is(&self, manifest: &ResolvedManifest) -> bool {
        match self {
            Target::Existing {
                kind,
                name,
                namespace,
            } => {
                manifest.kind.api.group == kind.api.group
                    && manifest.kind.api.kind == kind.api.kind
                    && &manifest.name == name
                    && &manifest.namespace == namespace
            }
            Target::New { .. } => false,
        }
    }
}

fn is_secret(kind: &ResourceKind) -> bool {
    kubach_core::secrets::mask::is_core_secret(&kind.api.group, &kind.api.kind)
}

/// Whether "Edit YAML" is offered for a kind. Secrets have their own
/// editor instead (see [`edit_button`]).
pub fn can_edit(kind: &ResourceKind) -> Result<(), String> {
    if is_secret(kind) {
        return Err(t!("editor-secret-not-yaml"));
    }
    if !kind.supports("patch") {
        return Err(t!("editor-kind-not-patchable"));
    }
    Ok(())
}

/// "Edit YAML" button for a resource details panel.
///
/// Secrets get "Edit values" instead, which opens the Secret editor: the
/// YAML editor would have to show their values in clear text, and masking
/// values with placeholders is not safe with server-side apply, since a
/// placeholder sent back would overwrite the real value.
pub fn edit_button(
    cluster: &Entity<Cluster>,
    kind: &ResourceKind,
    object: &DynamicObject,
) -> impl IntoElement + use<> {
    if is_secret(kind) {
        return edit_secret_button(cluster, object).into_any_element();
    }
    let allowed = can_edit(kind);
    let target = Target::Existing {
        kind: kind.clone(),
        name: object.metadata.name.clone().unwrap_or_default(),
        namespace: object.metadata.namespace.clone(),
    };
    let cluster = cluster.clone();
    let button = Button::new("edit-yaml")
        .outline()
        .small()
        .icon(IconName::FileText)
        .label(t!("editor-edit-yaml"))
        .disabled(allowed.is_err())
        .on_click(move |_, _, cx| open(&cluster, target.clone(), cx));
    match allowed {
        Err(reason) => button.tooltip(reason),
        Ok(()) => button,
    }
    .into_any_element()
}

/// "Edit values" for a Secret; disabled for immutable ones.
fn edit_secret_button(cluster: &Entity<Cluster>, object: &DynamicObject) -> Button {
    let immutable = object
        .data
        .get("immutable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let target = SecretTarget::Existing {
        namespace: object.metadata.namespace.clone().unwrap_or_default(),
        name: object.metadata.name.clone().unwrap_or_default(),
    };
    let cluster = cluster.clone();
    let button = Button::new("edit-secret")
        .outline()
        .small()
        .icon(IconName::FileText)
        .label(t!("editor-edit-values"))
        .disabled(immutable)
        .on_click(move |_, _, cx| open_secret(&cluster, target.clone(), cx));
    if immutable {
        button.tooltip(t!("editor-secret-immutable-tooltip"))
    } else {
        button.tooltip(t!("editor-edit-values-tooltip"))
    }
}

/// "New Secret" button: a form instead of YAML.
pub fn new_secret_button(
    cluster: &Entity<Cluster>,
    namespace: String,
    cx: &App,
) -> impl IntoElement + use<> {
    let connected = cluster.read(cx).connection().is_some();
    let cluster = cluster.clone();
    Button::new("new-secret")
        .outline()
        .small()
        .icon(IconName::Plus)
        .label(t!("editor-new-secret"))
        .tooltip(t!("editor-new-secret-tooltip"))
        .disabled(!connected)
        .on_click(move |_, _, cx| {
            open_secret(
                &cluster,
                SecretTarget::New {
                    namespace: namespace.clone(),
                },
                cx,
            )
        })
}

/// What an editor tab edits.
#[derive(Clone)]
enum Opened {
    Yaml(Target),
    Secret(SecretTarget),
}

/// A request to open an editor tab, bound to the connection that was
/// active when it was made.
#[derive(Clone)]
pub struct OpenRequest {
    connection: Connection,
    catalog: Option<ApiCatalog>,
    target: Opened,
}

/// Routes [`OpenRequest`]s from anywhere (buttons in screens) to the main
/// window's [`EditorTabs`].
pub struct EditorHub;

impl EventEmitter<OpenRequest> for EditorHub {}

struct GlobalHub(Entity<EditorHub>);

impl Global for GlobalHub {}

/// Opens an editor tab for `target` on the active connection of `cluster`.
pub fn open(cluster: &Entity<Cluster>, target: Target, cx: &mut App) {
    send(cluster, Opened::Yaml(target), cx);
}

/// Opens a Secret editor tab on the active connection of `cluster`.
pub fn open_secret(cluster: &Entity<Cluster>, target: SecretTarget, cx: &mut App) {
    send(cluster, Opened::Secret(target), cx);
}

fn send(cluster: &Entity<Cluster>, target: Opened, cx: &mut App) {
    let cluster = cluster.read(cx);
    let Some(connection) = cluster.connection().cloned() else {
        return;
    };
    let catalog = match cluster.catalog() {
        CatalogState::Ready(catalog) => Some(catalog.clone()),
        _ => None,
    };
    let request = OpenRequest {
        connection,
        catalog,
        target,
    };
    if let Some(GlobalHub(hub)) = cx.try_global::<GlobalHub>() {
        hub.clone().update(cx, |_, cx| cx.emit(request));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Busy {
    Loading,
    Reviewing,
    Applying,
}

struct Banner {
    tone: Tone,
    text: SharedString,
}

impl Banner {
    fn new(tone: Tone, text: impl Into<SharedString>) -> Self {
        Self {
            tone,
            text: text.into(),
        }
    }
}

/// One object of a review: what the server did or would do, or why not.
struct Entry {
    label: String,
    result: Result<ReviewItem, Failure>,
}

/// The result of a dry run (or of a failed real apply).
struct ReviewState {
    /// What was reviewed; "Apply" applies exactly this.
    manifests: Vec<ResolvedManifest>,
    entries: Vec<Entry>,
    selected: usize,
    force: bool,
    /// The entries come from a real apply (some failed).
    applied: bool,
}

impl ReviewState {
    fn conflicts(&self) -> bool {
        self.entries
            .iter()
            .any(|e| matches!(e.result, Err(Failure::Conflicts { .. })))
    }

    fn failed(&self) -> bool {
        self.entries.iter().any(|e| e.result.is_err())
    }
}

pub struct YamlEditor {
    target: Target,
    context: String,
    client: Client,
    catalog: Option<ApiCatalog>,
    schema_cache: Arc<SchemaCache>,
    schemas: SharedSchemas,
    editor: Entity<EditorState>,
    /// Edit view of the object as loaded, to tell removed fields apart.
    original: Option<Value>,
    analysis: Analysis,
    /// Problems the server reported for the current text.
    server_problems: Vec<Problem>,
    /// An edit is waiting for its (debounced) check.
    checking: bool,
    debounce: Debounce,
    review: Option<ReviewState>,
    busy: Option<Busy>,
    banner: Option<Banner>,
    /// Text as loaded (or empty for new resources), for unsaved changes.
    baseline: String,
    /// The main window's cluster, whose active context may differ.
    cluster: Entity<Cluster>,
    /// A ConfigMap is edited by keys (#275) rather than as YAML.
    keys_mode: bool,
    /// The keys view, while `keys_mode` is on and the text is a ConfigMap.
    keys: Option<configmap::KeysView>,
    /// Values that fail their check (invalid JSON), shown with the review.
    value_warnings: Vec<String>,
    /// A schema skeleton to complete once its schema is known (#320).
    skeleton: Option<presets::PendingSkeleton>,
    /// Name of the preset being saved, while "Save as preset" is open.
    preset_name: Option<Entity<InputState>>,
    preset_subscriptions: Vec<Subscription>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
    _analysis: Task<()>,
    _operation: Task<()>,
}

impl YamlEditor {
    fn new(
        connection: Connection,
        catalog: Option<ApiCatalog>,
        target: Target,
        cluster: Entity<Cluster>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let schemas = SharedSchemas::default();
        let assist = Rc::new(SchemaAssist {
            schemas: schemas.clone(),
            catalog: catalog.clone(),
        });
        let placeholder: fn() -> String = match target {
            Target::New { .. } => new_placeholder,
            Target::Existing { .. } => || t!("editor-loading"),
        };
        let editor = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language("yaml")
                .line_number(true);
            state.lsp_mut().completion_provider = Some(assist.clone());
            state.lsp_mut().hover_provider = Some(assist);
            state
        });
        let mut subscriptions = vec![
            cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.edited(cx);
                }
            }),
            // The "belongs to another context" banner follows switches.
            cx.observe(&cluster, |_, _, cx| cx.notify()),
        ];
        subscriptions.extend(localized_editor_placeholder(
            &editor,
            placeholder,
            window,
            cx,
        ));
        let keys_mode = configmap::is_configmap(&target);
        let mut this = Self {
            context: connection.context.clone(),
            schema_cache: Arc::new(SchemaCache::new(connection.client.clone())),
            client: connection.client,
            catalog,
            target,
            schemas,
            editor: editor.clone(),
            original: None,
            analysis: Analysis::default(),
            server_problems: Vec::new(),
            checking: false,
            debounce: Debounce::default(),
            review: None,
            busy: None,
            banner: None,
            baseline: String::new(),
            cluster,
            keys_mode,
            keys: None,
            value_warnings: Vec::new(),
            skeleton: None,
            preset_name: None,
            preset_subscriptions: Vec::new(),
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
            _analysis: Task::ready(()),
            _operation: Task::ready(()),
        };
        match &this.target {
            Target::Existing { .. } => this.load(window, cx),
            Target::New { template, .. } => {
                if let Some(template) = template.clone() {
                    // An untouched template is not an unsaved change.
                    this.baseline = template.text.clone();
                    this.skeleton =
                        template
                            .skeleton
                            .clone()
                            .map(|skeleton| presets::PendingSkeleton {
                                skeleton,
                                text: template.text.clone(),
                            });
                    this.set_text(&template, window, cx);
                }
                this.schedule_analysis(cx);
            }
        }
        editor.update(cx, |state, cx| state.focus(window, cx));
        this
    }

    fn text(&self, cx: &App) -> String {
        self.editor.read(cx).value().to_string()
    }

    /// The text differs from what was loaded (or applied).
    pub fn is_dirty(&self, cx: &App) -> bool {
        self.editor.read(cx).value().as_ref() != self.baseline
            || self.keys.as_ref().is_some_and(|keys| keys.is_dirty(cx))
    }

    pub fn context(&self) -> &str {
        &self.context
    }

    pub fn tab_title(&self) -> String {
        self.target.tab_title()
    }

    fn focus_editor(&self, window: &mut Window, cx: &mut App) {
        self.editor.update(cx, |state, cx| state.focus(window, cx));
    }

    /// The context of the main window, when it is not this tab's.
    fn foreign_context(&self, cx: &App) -> Option<String> {
        let active = self.cluster.read(cx).active().map(|c| c.name.clone());
        (active.as_deref() != Some(self.context.as_str())).then(|| self.context.clone())
    }

    /// The session of this tab's context was closed in the main window
    /// (disconnected, or evicted from the hotbar's kept sessions). Review
    /// and Apply wait until it is connected again.
    fn session_closed(&self, cx: &App) -> bool {
        !matches!(
            self.cluster.read(cx).state_of(&self.context),
            ConnectionState::Connected { .. }
        )
    }

    /// Refuses to talk to a context the user closed; `true` if refused.
    fn refuse_if_closed(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.session_closed(cx) {
            return false;
        }
        self.banner = Some(Banner::new(
            Tone::Warning,
            t!("editor-disconnected", context = &self.context),
        ));
        cx.notify();
        true
    }

    /// (Re)loads the edited object from the cluster, replacing the text.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Target::Existing {
            kind,
            name,
            namespace,
        } = &self.target
        else {
            return;
        };
        let api = discovery::api(&self.client, kind, namespace.as_deref());
        let name = name.clone();
        let task = runtime::spawn(async move { api.get(&name).await });
        self.busy = Some(Busy::Loading);
        self.banner = None;
        self.review = None;
        self._operation = cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(Ok(object)) => this.set_object(&object, window, cx),
                    Ok(Err(error)) => {
                        this.banner = Some(Banner::new(
                            Tone::Danger,
                            t!("editor-load-failed", error = error.to_string()),
                        ))
                    }
                    Err(error) => this.banner = Some(Banner::new(Tone::Danger, error.to_string())),
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    fn set_object(&mut self, object: &DynamicObject, window: &mut Window, cx: &mut Context<Self>) {
        let view = apply::edit_view(object);
        let text = apply::to_yaml(&view);
        self.original = Some(view);
        self.baseline = text.clone();
        self.server_problems.clear();
        self.editor
            .update(cx, |state, cx| state.set_value(text, window, cx));
        if self.keys_mode {
            self.keys = None;
            self.keys_mode = self.open_keys(window, cx);
        }
        self.schedule_analysis(cx);
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.server_problems.clear();
        if self.banner.as_ref().is_some_and(|b| b.tone != Tone::Danger) {
            self.banner = None;
        }
        self.schedule_analysis(cx);
    }

    fn schedule_analysis(&mut self, cx: &mut Context<Self>) {
        let generation = self.debounce.bump();
        self.checking = true;
        self._analysis = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ANALYSIS_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                if this.debounce.is_current(generation) {
                    this.analyze(cx);
                }
            })
            .ok();
        });
        cx.notify();
    }

    fn analyze(&mut self, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let analysis = {
            let store = self.schemas.borrow();
            analyze(
                &text,
                self.catalog.as_ref(),
                self.target.default_namespace(),
                &|gvk| store.lookup(gvk),
            )
        };
        self.fetch_schemas(&analysis.kinds, cx);
        self.analysis = analysis;
        self.checking = false;
        self.publish_diagnostics(cx);
        cx.notify();
    }

    /// Fetches schemas not yet known; each arrival rechecks the text.
    fn fetch_schemas(&mut self, kinds: &[GroupVersionKind], cx: &mut Context<Self>) {
        for gvk in kinds {
            if !self.schemas.borrow_mut().start(gvk) {
                continue;
            }
            let cache = self.schema_cache.clone();
            let wanted = gvk.clone();
            let task = runtime::spawn(async move { cache.document(&wanted).await });
            let gvk = gvk.clone();
            cx.spawn(async move |this, cx| {
                let slot = match task.await {
                    Ok(Ok(document)) => SchemaSlot::Ready(document),
                    Ok(Err(error)) => SchemaSlot::Failed(error.to_string()),
                    Err(error) => SchemaSlot::Failed(error.to_string()),
                };
                this.update(cx, |this, cx| {
                    this.schemas.borrow_mut().finish(&gvk, slot);
                    this.analyze(cx);
                })
                .ok();
            })
            .detach();
        }
    }

    fn publish_diagnostics(&mut self, cx: &mut Context<Self>) {
        let diagnostics: Vec<Diagnostic> = self
            .analysis
            .problems
            .iter()
            .chain(&self.server_problems)
            .map(to_diagnostic)
            .collect();
        self.editor.update(cx, |state, cx| {
            if let Some(set) = state.diagnostics_mut() {
                set.clear();
                set.extend(diagnostics);
            }
            cx.notify();
        });
    }

    fn jump_to(&mut self, line: usize, column: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |state, cx| {
            state.set_cursor_position(Position::new(line as u32, column as u32), window, cx)
        });
    }

    /// Parses and resolves the text, then runs a server-side dry run.
    fn review(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.refuse_if_closed(cx) {
            return;
        }
        // The keys view writes its edits into the text first: the text is
        // what is reviewed and applied.
        self.value_warnings.clear();
        if let Err(error) = self.sync_keys(window, cx) {
            self.banner = Some(Banner::new(Tone::Danger, error));
            cx.notify();
            return;
        }
        if let Some(keys) = &self.keys {
            self.value_warnings = keys.warnings(cx);
        }
        let text = self.text(cx);
        let Some(catalog) = self.catalog.clone() else {
            self.banner = Some(Banner::new(Tone::Warning, t!("editor-discovery-pending")));
            cx.notify();
            return;
        };
        let manifests = match apply::parse(&text) {
            Ok(manifests) if manifests.is_empty() => {
                self.banner = Some(Banner::new(Tone::Warning, t!("editor-nothing-to-apply")));
                cx.notify();
                return;
            }
            Ok(manifests) => manifests,
            Err(error) => {
                self.banner = Some(Banner::new(Tone::Danger, error.to_string()));
                cx.notify();
                return;
            }
        };
        let resolved =
            match apply::resolve_all(&manifests, &catalog, self.target.default_namespace()) {
                Ok(resolved) => resolved,
                Err(errors) => {
                    let messages: Vec<String> =
                        errors.iter().map(analysis::resolve_error).collect();
                    self.banner = Some(Banner::new(Tone::Danger, messages.join("\n")));
                    cx.notify();
                    return;
                }
            };
        self.run(true, resolved, text, force, window, cx);
    }

    /// Applies the reviewed manifests for real.
    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(review) = &self.review else {
            return;
        };
        if self.busy.is_some() || review.applied || review.failed() {
            return;
        }
        let (manifests, force) = (review.manifests.clone(), review.force);
        if self.refuse_if_closed(cx) {
            return;
        }
        let text = self.text(cx);
        self.run(false, manifests, text, force, window, cx);
    }

    fn run(
        &mut self,
        dry_run: bool,
        manifests: Vec<ResolvedManifest>,
        text: String,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let client = self.client.clone();
        let options = ApplyOptions {
            force,
            dry_run,
            ..ApplyOptions::default()
        };
        let batch = manifests.clone();
        let apply = async move { apply::apply_all(&client, &batch, &options).await };
        // A review (dry run) stops with the editor; a real apply is detached
        // so closing the tab does not leave a batch half applied (ADR 0011).
        let task = if dry_run {
            futures::future::Either::Left(runtime::spawn(apply))
        } else {
            futures::future::Either::Right(runtime::spawn_detached(apply))
        };
        self.busy = Some(if dry_run {
            Busy::Reviewing
        } else {
            Busy::Applying
        });
        self.banner = None;
        self._operation = cx.spawn_in(window, async move |this, cx| {
            let results = task.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match results {
                    Ok(results) => {
                        this.finish(dry_run, manifests, &text, force, results, window, cx)
                    }
                    Err(error) => this.banner = Some(Banner::new(Tone::Danger, error.to_string())),
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        &mut self,
        dry_run: bool,
        manifests: Vec<ResolvedManifest>,
        text: &str,
        force: bool,
        results: Vec<Result<apply::ApplyOutcome, apply::ApplyError>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut reload = None;
        let mut entries = Vec::with_capacity(results.len());
        for (manifest, result) in manifests.iter().zip(results) {
            let is_target = self.target.is(manifest);
            let original = self.original.as_ref().filter(|_| is_target);
            let result = result.map_err(Failure::from).map(|outcome| {
                if is_target && !dry_run {
                    reload = Some(outcome.after.clone());
                }
                review_item(manifest.label(), &outcome, original, &manifest.value)
            });
            entries.push(Entry {
                label: manifest.label(),
                result,
            });
        }

        // Point at what the server rejected, if the text is unchanged.
        if text == self.text(cx) {
            self.server_problems = server_problems(text, &manifests, &entries);
            self.publish_diagnostics(cx);
        }

        let failed = entries.iter().any(|e| e.result.is_err());
        if dry_run || failed {
            let selected = entries.iter().position(|e| e.result.is_err()).unwrap_or(0);
            self.review = Some(ReviewState {
                manifests,
                entries,
                selected,
                force,
                applied: !dry_run,
            });
            if !dry_run {
                self.banner = Some(Banner::new(Tone::Danger, t!("editor-some-not-applied")));
            }
            return;
        }

        let summary: Vec<String> = entries
            .iter()
            .filter_map(|e| e.result.as_ref().ok())
            .map(|item| {
                let label = item.label.as_str();
                if item.created {
                    t!("editor-applied-created", label = label)
                } else if item.changes.is_empty() {
                    t!("editor-applied-unchanged", label = label)
                } else {
                    t!("editor-applied-configured", label = label)
                }
            })
            .collect();
        self.review = None;
        self.baseline = text.to_string();
        if let Some(object) = reload {
            self.set_object(&object, window, cx);
        }
        self.banner = Some(Banner::new(
            Tone::Success,
            t!("editor-applied", objects = summary.join(", ")),
        ));
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_none() {
            self.review = None;
            cx.notify();
        }
    }

    fn on_review(&mut self, _: &ReviewChanges, window: &mut Window, cx: &mut Context<Self>) {
        if self.review.is_none() {
            self.review(false, window, cx);
        }
    }

    fn render_status(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let errors = self.analysis.errors();
        let warnings = self.analysis.problems.len() - errors;
        let schema_errors = self.schemas.borrow().errors();
        let (tone, text): (Tone, SharedString) = if self.busy == Some(Busy::Loading) {
            (Tone::Neutral, t!("editor-loading").into())
        } else if self.checking {
            (Tone::Neutral, t!("editor-checking").into())
        } else if errors > 0 {
            (
                Tone::Danger,
                t!(
                    "editor-errors-warnings",
                    errors = errors,
                    warnings = warnings
                )
                .into(),
            )
        } else if warnings > 0 {
            (
                Tone::Warning,
                t!("editor-warnings", count = warnings).into(),
            )
        } else if self.analysis.schema_pending {
            (Tone::Neutral, t!("editor-loading-schema").into())
        } else if let Some(error) = schema_errors.first() {
            (
                Tone::Warning,
                t!("editor-schema-unavailable", error = error.to_string()).into(),
            )
        } else if self.analysis.documents == 0 {
            (Tone::Neutral, t!("editor-empty").into())
        } else {
            (Tone::Success, t!("editor-no-problems").into())
        };
        div()
            .flex()
            .items_center()
            .gap_1p5()
            .min_w_0()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .size(px(6.))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(tone.color(cx)),
            )
            .child(div().truncate().child(text))
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let busy = self.busy.is_some() || self.session_closed(cx);
        let subtitle = match &self.target {
            Target::Existing { .. } => t!("editor-subtitle-edit", context = &self.context),
            Target::New { namespace, .. } => t!(
                "editor-subtitle-create",
                context = &self.context,
                namespace = namespace
            ),
        };
        let title = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(self.target.title()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(subtitle),
                    )
                    .child(self.render_status(cx)),
            );

        let actions = div().flex().items_center().gap_2();
        let actions = match &self.review {
            None => actions
                .when(configmap::is_configmap(&self.target), |row| {
                    row.child(
                        SegmentTabs::new(
                            "editor-mode",
                            vec![
                                SegmentTab::new(t!("editor-mode-keys"))
                                    .tooltip(t!("editor-mode-keys-tooltip")),
                                SegmentTab::new("YAML").tooltip(t!("editor-mode-yaml-tooltip")),
                            ],
                            if self.keys.is_some() { 0 } else { 1 },
                        )
                        .on_select(cx.listener(
                            |this, ix: &usize, window, cx| this.set_keys_mode(*ix == 0, window, cx),
                        )),
                    )
                })
                .when(matches!(self.target, Target::Existing { .. }), |row| {
                    row.child(
                        Button::new("reload")
                            .ghost()
                            .small()
                            .icon(IconName::RefreshCw)
                            .label(t!("editor-reload"))
                            .tooltip(t!("editor-reload-tooltip"))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.load(window, cx))),
                    )
                })
                .child(
                    Button::new("save-preset")
                        .ghost()
                        .small()
                        .icon(IconName::Star)
                        .label(t!("editor-save-preset"))
                        .tooltip(t!("editor-save-preset-tooltip"))
                        .disabled(self.busy == Some(Busy::Loading) || self.analysis.documents == 0)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.ask_preset_name(window, cx)),
                        ),
                )
                .child(
                    Button::new("review")
                        .primary()
                        .small()
                        .label(t!("editor-review"))
                        .tooltip(t!("editor-review-tooltip"))
                        .spinning(self.busy == Some(Busy::Reviewing))
                        .disabled(busy || self.analysis.documents == 0)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.review(false, window, cx)),
                        ),
                ),
            Some(review) => {
                let force = review.force;
                let apply_label = if force {
                    t!("editor-force-apply")
                } else {
                    t!("editor-apply")
                };
                let mut apply = Button::new("apply")
                    .small()
                    .label(apply_label)
                    .spinning(self.busy == Some(Busy::Applying))
                    .disabled(busy || review.applied || review.failed())
                    .on_click(cx.listener(|this, _, window, cx| this.apply(window, cx)));
                apply = if force {
                    apply.danger()
                } else {
                    apply.primary()
                };
                actions
                    .child(
                        Button::new("back")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .label(t!("editor-back"))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                    )
                    .when(review.conflicts() && !force, |row| {
                        row.child(
                            Button::new("force-review")
                                .warning()
                                .small()
                                .label(t!("editor-review-force"))
                                .tooltip(t!("editor-review-force-tooltip"))
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.review = None;
                                    this.review(true, window, cx);
                                })),
                        )
                    })
                    .child(apply)
            }
        };

        div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_4()
            .px_4()
            .py_2p5()
            .border_b_1()
            .border_color(theme.border)
            .child(title)
            .child(actions)
    }

    /// Shown while the main window is on another context: this tab still
    /// reviews and applies to its own.
    fn render_context_banner(&self, cx: &App) -> Option<impl IntoElement> {
        let (tone, text) = context_notice(
            self.foreign_context(cx).is_some(),
            self.session_closed(cx),
            &self.context,
        )?;
        let color = tone.color(cx);
        Some(
            div()
                .flex_shrink_0()
                .mx_4()
                .mt_3()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(color)
                .bg(color.opacity(TINT_OPACITY))
                .text_sm()
                .text_color(cx.theme().foreground)
                .child(text),
        )
    }

    fn render_banner(&self, cx: &App) -> Option<impl IntoElement> {
        let banner = self.banner.as_ref()?;
        let color = banner.tone.color(cx);
        Some(
            div()
                .flex_shrink_0()
                .mx_4()
                .mt_3()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(color)
                .bg(color.opacity(TINT_OPACITY))
                .text_sm()
                .text_color(cx.theme().foreground)
                .whitespace_normal()
                .child(banner.text.clone()),
        )
    }

    fn render_problems(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let problems: Vec<Problem> = self
            .analysis
            .problems
            .iter()
            .chain(&self.server_problems)
            .cloned()
            .collect();
        if problems.is_empty() {
            return None;
        }
        let theme = cx.theme();
        Some(
            div()
                .id("problems")
                .flex_shrink_0()
                .max_h(PROBLEMS_MAX_HEIGHT)
                .overflow_y_scroll()
                .border_t_1()
                .border_color(theme.border)
                .py_1()
                .text_xs()
                .children(problems.into_iter().enumerate().map(|(ix, problem)| {
                    let tone = match problem.severity {
                        Severity::Error => Tone::Danger,
                        Severity::Warning => Tone::Warning,
                    };
                    let (line, column) = (problem.line, problem.start);
                    div()
                        .id(("problem", ix))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .py_0p5()
                        .cursor_pointer()
                        .hover(|row| row.bg(theme.muted))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.jump_to(line, column, window, cx)
                        }))
                        .child(
                            Icon::new(match problem.severity {
                                Severity::Error => IconName::CircleX,
                                Severity::Warning => IconName::TriangleAlert,
                            })
                            .size_3()
                            .text_color(tone.color(cx)),
                        )
                        .child(
                            div()
                                .min_w(px(64.))
                                .flex_shrink_0()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(t!("editor-line", line = problem.line + 1)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.foreground)
                                .child(problem.message),
                        )
                })),
        )
    }

    fn render_edit(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div().flex_1().min_h_0().p_2().child(
                    Editor::new(&self.editor)
                        .h_full()
                        .bordered(false)
                        .disabled(self.busy == Some(Busy::Loading)),
                ),
            )
            .children(self.render_problems(cx))
            .into_any_element()
    }

    fn render_review(&self, review: &ReviewState, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let mut list = div()
            .id("review-list")
            .w(REVIEW_LIST_WIDTH)
            .flex_shrink_0()
            .h_full()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(theme.border)
            .p_3()
            .flex()
            .flex_col()
            .gap_2();
        for warning in &self.value_warnings {
            list = list.child(callout(
                Tone::Warning,
                t!("editor-value-saved-anyway", warning = warning),
                cx,
            ));
        }
        if review.force {
            list = list.child(callout(Tone::Danger, t!("editor-force-warning"), cx));
        }
        for (ix, entry) in review.entries.iter().enumerate() {
            list = list.child(self.render_entry(ix, entry, ix == review.selected, cx));
        }

        let detail = match review.entries.get(review.selected).map(|e| &e.result) {
            Some(Ok(item)) if item.changes.is_empty() && !item.created => {
                EmptyState::new(IconName::Check, t!("editor-no-changes"))
                    .description(t!("editor-no-changes-description"))
                    .into_any_element()
            }
            Some(Ok(item)) => DiffView::new(("review-diff", review.selected), item.lines.clone())
                .into_any_element(),
            Some(Err(failure)) => EmptyState::new(IconName::CircleX, t!("editor-not-applied"))
                .description(failure.message())
                .into_any_element(),
            None => div().into_any_element(),
        };

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .child(list)
            .child(div().flex_1().min_w_0().h_full().child(detail))
            .into_any_element()
    }

    fn render_entry(
        &self,
        ix: usize,
        entry: &Entry,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (tone, status) = match &entry.result {
            Ok(item) if item.created => (Tone::Success, t!("editor-entry-create")),
            Ok(item) if item.changes.is_empty() => (Tone::Neutral, t!("editor-entry-no-changes")),
            Ok(item) => (
                Tone::Info,
                t!("editor-entry-changes", count = item.changes.len()),
            ),
            Err(_) => (Tone::Danger, t!("editor-entry-failed")),
        };
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(entry.label.clone()),
            )
            .child(div().text_xs().text_color(tone.color(cx)).child(status));

        let mut body = div().flex().flex_col().gap_1().text_xs();
        match &entry.result {
            Ok(item) => {
                if !item.created {
                    for change in &item.changes {
                        body = body.child(change_row(change, cx));
                    }
                }
                if !item.retained.is_empty() {
                    body = body.child(callout(
                        Tone::Warning,
                        t!("editor-retained", fields = item.retained.join(", ")),
                        cx,
                    ));
                }
            }
            Err(failure) => {
                body = body.child(
                    div()
                        .whitespace_normal()
                        .text_color(theme.foreground)
                        .child(failure.message()),
                );
                match failure {
                    Failure::Conflicts { conflicts, .. } => {
                        for conflict in conflicts {
                            body = body.child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(conflict.field.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_color(theme.muted_foreground)
                                            .child(conflict.manager.clone()),
                                    ),
                            );
                        }
                    }
                    Failure::Invalid { fields, .. } => {
                        for field in fields {
                            body = body.child(
                                div()
                                    .whitespace_normal()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(format!("{}: {}", field.field, field.message)),
                            );
                        }
                    }
                    Failure::Stale { .. } if matches!(self.target, Target::Existing { .. }) => {
                        body = body.child(
                            Button::new(("reload-stale", ix))
                                .outline()
                                .xsmall()
                                .icon(IconName::RefreshCw)
                                .label(t!("editor-reload-stale"))
                                .on_click(cx.listener(|this, _, window, cx| this.load(window, cx))),
                        );
                    }
                    _ => {}
                }
            }
        }

        div()
            .id(("review-entry", ix))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(if selected { theme.ring } else { theme.border })
            .bg(if selected {
                theme.accent
            } else {
                theme.group_box
            })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(review) = &mut this.review {
                    review.selected = ix;
                    cx.notify();
                }
            }))
            .child(header)
            .child(body)
    }
}

/// A field change: path, then `before → after`.
fn change_row(change: &review::ChangeRow, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let (sign, tone) = match change.kind {
        ChangeKind::Added => ("+", Tone::Success),
        ChangeKind::Removed => ("−", Tone::Danger),
        ChangeKind::Modified => ("~", Tone::Info),
    };
    let values = match (&change.before, &change.after) {
        (Some(before), Some(after)) => format!("{before} → {after}"),
        (None, Some(after)) => after.clone(),
        (Some(before), None) => before.clone(),
        (None, None) => String::new(),
    };
    div()
        .flex()
        .flex_col()
        .font_family(theme.mono_font_family.clone())
        .child(
            div()
                .flex()
                .gap_1p5()
                .child(div().text_color(tone.color(cx)).child(sign))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.foreground)
                        .child(change.path.clone()),
                ),
        )
        .child(
            div()
                .pl_3()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(values),
        )
}

fn callout(tone: Tone, text: impl Into<SharedString>, cx: &App) -> Div {
    let color = tone.color(cx);
    div()
        .flex()
        .gap_2()
        .px_2p5()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(color)
        .bg(color.opacity(TINT_OPACITY))
        .text_xs()
        .text_color(cx.theme().foreground)
        .child(
            Icon::new(IconName::TriangleAlert)
                .size_3p5()
                .flex_shrink_0()
                .text_color(color),
        )
        .child(div().flex_1().whitespace_normal().child(text.into()))
}

/// The notice over a tab bound to `context`: whether the main window shows
/// another context, and whether `context`'s session is closed.
fn context_notice(foreign: bool, closed: bool, context: &str) -> Option<(Tone, String)> {
    match (foreign, closed) {
        (_, true) => Some((
            Tone::Warning,
            t!("editor-context-closed", context = context),
        )),
        (true, false) => Some((Tone::Info, t!("editor-context-foreign", context = context))),
        (false, false) => None,
    }
}

fn to_diagnostic(problem: &Problem) -> Diagnostic {
    let line = problem.line as u32;
    Diagnostic::new(
        Position::new(line, problem.start as u32)..Position::new(line, problem.end as u32),
        problem.message.clone(),
    )
    .with_severity(match problem.severity {
        Severity::Error => DiagnosticSeverity::Error,
        Severity::Warning => DiagnosticSeverity::Warning,
    })
    .with_source("kubach")
}

/// Conflicting and rejected fields of failed applies, positioned in `text`.
fn server_problems(text: &str, manifests: &[ResolvedManifest], entries: &[Entry]) -> Vec<Problem> {
    let mut problems = Vec::new();
    for (manifest, entry) in manifests.iter().zip(entries) {
        match &entry.result {
            Err(Failure::Conflicts { conflicts, .. }) => {
                for conflict in conflicts {
                    problems.push(Problem::at_path(
                        text,
                        manifest.line,
                        &FieldPath::parse(&conflict.field),
                        Severity::Warning,
                        t!(
                            "editor-problem-conflict",
                            field = &conflict.field,
                            manager = &conflict.manager
                        ),
                    ));
                }
            }
            Err(Failure::Invalid {
                fields, message, ..
            }) => {
                if fields.is_empty() {
                    problems.push(Problem::at_path(
                        text,
                        manifest.line,
                        &FieldPath::root(),
                        Severity::Error,
                        message.clone(),
                    ));
                }
                for field in fields {
                    problems.push(Problem::at_path(
                        text,
                        manifest.line,
                        &FieldPath::parse(&field.field),
                        Severity::Error,
                        format!("{}: {}", field.field, field.message),
                    ));
                }
            }
            _ => {}
        }
    }
    problems
}

impl Focusable for YamlEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for YamlEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_skeleton(window, cx);
        let body = match (&self.review, &self.keys) {
            (Some(review), _) => self.render_review(review, cx),
            (None, Some(keys)) => self.render_keys(keys, cx),
            (None, None) => self.render_edit(cx),
        };
        let theme = cx.theme();
        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_review))
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .text_color(theme.foreground)
            .child(self.render_toolbar(cx))
            .children(self.render_preset_bar(cx))
            .children(self.render_context_banner(cx))
            .children(self.render_banner(cx))
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::context_notice;
    use kubach_ui::Tone;

    #[test]
    fn notices_other_and_closed_contexts() {
        assert!(context_notice(false, false, "prod").is_none());
        let (tone, text) = context_notice(true, false, "prod").unwrap();
        assert_eq!(tone, Tone::Info);
        assert!(text.starts_with("This tab belongs to prod."));
        for foreign in [false, true] {
            let (tone, text) = context_notice(foreign, true, "prod").unwrap();
            assert_eq!(tone, Tone::Warning);
            assert!(text.starts_with("Disconnected from prod."));
        }
    }
}
