//! Kustomize (ADR 0007): builds a kustomization folder with the user's
//! `kustomize` (or `kubectl kustomize`), lists the resulting objects with
//! their YAML and compares them with the connected cluster.
//!
//! Everything here is read-only: the build is local and the comparison only
//! reads live objects. Secret values stay hidden until revealed, and can't
//! be revealed when the active context's security settings hide them (#96).

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tab::{Tab as TabItem, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Selectable as _, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::apply::yaml::{to_yaml, to_yaml_multi};
use kubach_core::cache::ResourceCache;
use kubach_core::kustomize::{
    self, Build, BuildOptions, DiffCounts, ObjectDiff, ObjectSummary, RECENT_DIRS, Tool,
};
use kubach_core::runtime;
use kubach_core::settings::SecretAction;
use kubach_core::{ConnectionState, kustomize::KustomizeError};
use kubach_i18n::{Message, msg, t};
use kubach_ui::tokens::TINT_OPACITY;
use kubach_ui::{
    DiffView, EmptyState, PageHeader, PathHandler, ResourceCell, ResourceColumn, ResourceRow,
    ResourceTable, Tone, pick_path,
};

use super::activity::ScreenActivity;
use super::table_sort;
use crate::cluster::{CatalogState, Cluster};
use crate::settings::AppSettings;

/// A failed build, as shown to the user.
#[derive(Clone)]
struct Failure {
    title: Message,
    hint: Option<Message>,
    details: String,
}

impl From<&KustomizeError> for Failure {
    fn from(error: &KustomizeError) -> Self {
        match error {
            KustomizeError::NoTool => Self {
                title: msg!("kustomize-error-no-tool"),
                hint: Some(msg!("kustomize-error-no-tool-hint")),
                details: t!("kustomize-error-no-tool-details"),
            },
            KustomizeError::NotKustomization(dir) => Self {
                title: msg!("kustomize-error-not-kustomization"),
                hint: Some(msg!("kustomize-error-not-kustomization-hint")),
                details: t!(
                    "kustomize-error-not-kustomization-details",
                    path = dir.display().to_string()
                ),
            },
            KustomizeError::Spawn { program, source } => Self {
                title: msg!("kustomize-error-spawn"),
                hint: Some(msg!("kustomize-error-spawn-hint")),
                details: t!(
                    "kustomize-error-spawn-details",
                    program = program.display().to_string(),
                    error = source.to_string()
                ),
            },
            KustomizeError::Timeout(after) => Self {
                title: msg!("kustomize-error-timeout"),
                hint: Some(msg!("kustomize-error-timeout-hint")),
                details: t!("kustomize-error-timeout-details", seconds = after.as_secs()),
            },
            // The tool's own message.
            KustomizeError::Failed { .. } => Self {
                title: msg!("kustomize-error-failed"),
                hint: None,
                details: error.details(),
            },
            KustomizeError::NotUtf8 => Self {
                title: msg!("kustomize-error-unreadable"),
                hint: None,
                details: t!("kustomize-error-not-utf8"),
            },
            KustomizeError::Yaml(yaml) => Self {
                title: msg!("kustomize-error-unreadable"),
                hint: None,
                details: t!("kustomize-error-yaml", error = yaml.to_string()),
            },
        }
    }
}

/// `2 new · 1 changed · 5 unchanged`; empty parts are left out.
fn describe_counts(counts: &DiffCounts) -> String {
    let parts: Vec<String> = [
        (counts.new, msg!("kustomize-count-new", count = counts.new)),
        (
            counts.changed,
            msg!("kustomize-count-changed", count = counts.changed),
        ),
        (
            counts.unchanged,
            msg!("kustomize-count-unchanged", count = counts.unchanged),
        ),
        (
            counts.failed,
            msg!("kustomize-count-failed", count = counts.failed),
        ),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(_, text)| text.to_string())
    .collect();
    if parts.is_empty() {
        t!("kustomize-count-nothing")
    } else {
        parts.join(" · ")
    }
}

enum BuildState {
    /// No folder picked yet.
    Idle,
    Building,
    Ready(Arc<Built>),
    Failed(Failure),
}

/// A build with the identity of each object.
struct Built {
    build: Build,
    objects: Vec<ObjectSummary>,
    has_secrets: bool,
}

enum Compare {
    /// Not connected, or the build is not ready.
    Off,
    Running,
    /// Per object, in build order.
    Ready(Vec<Result<ObjectDiff, String>>),
}

#[derive(Clone, Copy, PartialEq)]
enum ListTab {
    Resources,
    Output,
}

#[derive(Clone, Copy, PartialEq)]
enum DetailTab {
    Yaml,
    Diff,
}

/// What the read-only viewer holds.
#[derive(Clone, PartialEq)]
enum ViewerKey {
    Output { revealed: bool },
    Object { index: usize, revealed: bool },
}

pub struct KustomizeScreen {
    cluster: Entity<Cluster>,
    /// On screen: watches run only then.
    active: bool,
    dir: Option<PathBuf>,
    enable_helm: bool,
    state: BuildState,
    /// The running build; dropping it aborts the I/O task and kills the
    /// `kustomize` process (ADR 0011).
    build_task: Option<Task<()>>,
    compare: Compare,
    /// Connection the comparison was made with.
    compared_with: Option<ResourceCache>,
    compare_task: Option<Task<()>>,
    list_tab: ListTab,
    /// Object index whose details are open.
    selected: Option<usize>,
    detail_tab: DetailTab,
    /// Secret values are shown.
    revealed: bool,
    table: Option<Entity<ResourceTable>>,
    viewer: Option<Entity<EditorState>>,
    viewer_key: Option<ViewerKey>,
    _subscriptions: Vec<Subscription>,
}

impl KustomizeScreen {
    pub fn new(cluster: Entity<Cluster>, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = vec![cx.observe(&cluster, |this, _, cx| this.sync(cx))];
        // Table headers are kept in the table: rebuild it in the new
        // language.
        if let Some(language) = crate::i18n::global(cx) {
            subscriptions.push(cx.observe(&language, |this, _, cx| {
                this.table = None;
                cx.notify();
            }));
        }
        // Recent folders come from the settings; the screen is embedded
        // cached (#363).
        if let Some(settings) = AppSettings::try_global(cx) {
            subscriptions.push(cx.observe(&settings, |_, _, cx| cx.notify()));
        }
        Self {
            cluster,
            active: false,
            dir: None,
            enable_helm: false,
            state: BuildState::Idle,
            build_task: None,
            compare: Compare::Off,
            compared_with: None,
            compare_task: None,
            list_tab: ListTab::Resources,
            selected: None,
            detail_tab: DetailTab::Yaml,
            revealed: false,
            table: None,
            viewer: None,
            viewer_key: None,
            _subscriptions: subscriptions,
        }
    }

    fn recent_dirs(cx: &App) -> Vec<PathBuf> {
        AppSettings::try_global(cx)
            .map(|settings| {
                settings
                    .read(cx)
                    .get()
                    .general
                    .recent_kustomize_dirs
                    .clone()
            })
            .unwrap_or_default()
    }

    /// Re-runs the comparison when the connection changed under a ready build.
    fn sync(&mut self, cx: &mut Context<Self>) {
        if !self.active {
            return;
        }
        let cache = self.cluster.read(cx).cache().cloned();
        let same = match (&self.compared_with, &cache) {
            (Some(old), Some(new)) => old.same_as(new),
            (None, None) => true,
            _ => false,
        };
        let catalog_ready = matches!(self.cluster.read(cx).catalog(), CatalogState::Ready(_));
        let stale = !same || (matches!(self.compare, Compare::Off) && cache.is_some());
        if stale && catalog_ready && matches!(self.state, BuildState::Ready(_)) {
            self.start_compare(cx);
        } else if cache.is_none() {
            self.compare = Compare::Off;
            self.compared_with = None;
            self.compare_task = None;
            self.update_rows(cx);
        }
        cx.notify();
    }

    fn pick_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let screen = cx.entity().downgrade();
        let on_pick: PathHandler = Rc::new(move |path, _, cx| {
            screen.update(cx, |this, cx| this.open_dir(path, cx)).ok();
        });
        pick_path(true, on_pick, window, cx);
    }

    fn open_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if let Some(settings) = AppSettings::try_global(cx) {
            let remembered = dir.clone();
            settings.update(cx, |settings, cx| {
                settings.edit(cx, |s| {
                    kustomize::remember_dir(
                        &mut s.general.recent_kustomize_dirs,
                        &remembered,
                        RECENT_DIRS,
                    )
                })
            });
        }
        if self.dir.as_ref() != Some(&dir) {
            self.selected = None;
            self.revealed = false;
            self.list_tab = ListTab::Resources;
        }
        self.dir = Some(dir);
        self.rebuild(cx);
    }

    /// Builds the folder again in the background; a running build is
    /// cancelled.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.dir.clone() else {
            return;
        };
        let options = BuildOptions {
            enable_helm: self.enable_helm,
            ..BuildOptions::default()
        };
        self.state = BuildState::Building;
        self.compare = Compare::Off;
        self.compare_task = None;
        self.viewer_key = None;
        let build = runtime::spawn(async move {
            let tool = Tool::find().ok_or(KustomizeError::NoTool)?;
            kustomize::build(&tool, &dir, &options).await
        });
        let ui = cx.spawn(async move |this, cx| {
            let result = build.await;
            this.update(cx, |this, cx| {
                this.build_task = None;
                this.state = match result {
                    Ok(Ok(build)) => {
                        let objects: Vec<ObjectSummary> =
                            build.manifests.iter().map(ObjectSummary::of).collect();
                        let has_secrets = objects.iter().any(ObjectSummary::is_secret);
                        BuildState::Ready(Arc::new(Built {
                            build,
                            objects,
                            has_secrets,
                        }))
                    }
                    Ok(Err(error)) => BuildState::Failed(Failure::from(&error)),
                    Err(_) => BuildState::Failed(Failure {
                        title: msg!("kustomize-error-interrupted"),
                        hint: None,
                        details: String::new(),
                    }),
                };
                if this.selected.is_some_and(|ix| ix >= this.built_len()) {
                    this.selected = None;
                }
                this.viewer_key = None;
                this.update_rows(cx);
                this.compared_with = None;
                this.compare = Compare::Off;
                this.sync(cx);
                cx.notify();
            })
            .ok();
        });
        self.build_task = Some(ui);
        self.update_rows(cx);
        cx.notify();
    }

    fn built(&self) -> Option<&Arc<Built>> {
        match &self.state {
            BuildState::Ready(built) => Some(built),
            _ => None,
        }
    }

    fn built_len(&self) -> usize {
        self.built().map_or(0, |built| built.objects.len())
    }

    /// Compares the build with the connected cluster.
    fn start_compare(&mut self, cx: &mut Context<Self>) {
        let Some(built) = self.built().cloned() else {
            return;
        };
        let cluster = self.cluster.read(cx);
        let (Some(connection), CatalogState::Ready(catalog), Some(cache)) =
            (cluster.connection(), cluster.catalog(), cluster.cache())
        else {
            return;
        };
        let namespace = cluster
            .active()
            .and_then(|context| context.namespace.clone())
            .unwrap_or_else(|| "default".into());
        let (client, catalog) = (connection.client.clone(), catalog.clone());
        self.compared_with = Some(cache.clone());
        self.compare = Compare::Running;
        let diff = runtime::spawn(async move {
            kustomize::diff_each(&client, &catalog, &built.build.manifests, &namespace).await
        });
        let ui = cx.spawn(async move |this, cx| {
            let result = diff.await;
            this.update(cx, |this, cx| {
                this.compare_task = None;
                this.compare = match result {
                    Ok(results) => Compare::Ready(results),
                    Err(_) => Compare::Off,
                };
                this.update_rows(cx);
                cx.notify();
            })
            .ok();
        });
        self.compare_task = Some(ui);
        self.update_rows(cx);
    }

    fn compare_result(&self, index: usize) -> Option<&Result<ObjectDiff, String>> {
        match &self.compare {
            Compare::Ready(results) => results.get(index),
            _ => None,
        }
    }

    fn columns() -> Vec<ResourceColumn> {
        [
            ResourceColumn::new("kind", t!("kustomize-column-kind")).width(px(180.)),
            ResourceColumn::new("name", t!("kustomize-column-name")).width(px(260.)),
            ResourceColumn::new("namespace", t!("kustomize-column-namespace")).width(px(160.)),
            ResourceColumn::new("api-version", t!("kustomize-column-api-version")).width(px(200.)),
            ResourceColumn::new("cluster", t!("kustomize-column-cluster")).width(px(140.)),
        ]
        .into_iter()
        .map(ResourceColumn::sortable)
        .collect()
    }

    fn cluster_cell(&self, index: usize) -> ResourceCell {
        match (&self.compare, self.compare_result(index)) {
            (_, Some(Ok(diff))) if diff.is_new() => {
                ResourceCell::status(t!("kustomize-status-new"), Tone::Info)
            }
            (_, Some(Ok(diff))) if diff.is_unchanged() => {
                ResourceCell::status(t!("kustomize-status-unchanged"), Tone::Success)
            }
            (_, Some(Ok(_))) => ResourceCell::status(t!("kustomize-status-changed"), Tone::Warning),
            (_, Some(Err(_))) => ResourceCell::status(t!("kustomize-status-error"), Tone::Danger),
            (Compare::Running, _) => ResourceCell::text("…").muted(),
            _ => ResourceCell::text("—").muted(),
        }
    }

    fn update_rows(&mut self, cx: &mut Context<Self>) {
        let Some(table) = self.table.clone() else {
            return;
        };
        let rows = match self.built() {
            Some(built) => built
                .objects
                .iter()
                .enumerate()
                .map(|(ix, object)| {
                    let namespace = match &object.namespace {
                        Some(namespace) => ResourceCell::text(namespace.clone()),
                        None => ResourceCell::text("—").muted(),
                    };
                    ResourceRow::new(
                        ix.to_string(),
                        [
                            ResourceCell::text(object.kind.clone()),
                            ResourceCell::text(object.name.clone()),
                            namespace,
                            ResourceCell::text(object.api_version.clone()).muted(),
                            self.cluster_cell(ix),
                        ],
                    )
                })
                .collect(),
            None => Vec::new(),
        };
        table.update(cx, |table, cx| table.set_rows(rows, cx));
    }

    fn ensure_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.table.is_none() {
            let screen = cx.entity().downgrade();
            let table = cx.new(|cx| {
                ResourceTable::new(Self::columns(), window, cx).on_select(move |row, _, cx| {
                    let index = row.key.parse().ok();
                    screen
                        .update(cx, |this, cx| {
                            this.selected = index;
                            this.viewer_key = None;
                            cx.notify();
                        })
                        .ok();
                })
            });
            table_sort::remember(&table, table_sort::screen("kustomize"), cx);
            self.table = Some(table);
            self.update_rows(cx);
        }
        let viewer = self
            .viewer
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language("yaml")
                        .line_number(true)
                })
            })
            .clone();
        let Some(built) = self.built().cloned() else {
            return;
        };
        let revealed = self.showing_secrets(cx);
        let wanted = match self.selected {
            Some(index) => ViewerKey::Object { index, revealed },
            None => ViewerKey::Output { revealed },
        };
        if self.viewer_key.as_ref() == Some(&wanted) {
            return;
        }
        let text = match &wanted {
            ViewerKey::Output { revealed: true } => built.build.yaml.clone(),
            ViewerKey::Output { revealed: false } if !built.has_secrets => built.build.yaml.clone(),
            ViewerKey::Output { revealed: false } => to_yaml_multi(
                built
                    .build
                    .manifests
                    .iter()
                    .map(|m| kustomize::mask_secret(&m.value))
                    .collect::<Vec<_>>()
                    .iter(),
            ),
            ViewerKey::Object { index, revealed } => match built.build.manifests.get(*index) {
                Some(manifest) if *revealed => to_yaml(&manifest.value),
                Some(manifest) => to_yaml(&kustomize::mask_secret(&manifest.value)),
                None => String::new(),
            },
        };
        self.viewer_key = Some(wanted);
        viewer.update(cx, |viewer, cx| viewer.set_value(text, window, cx));
    }

    fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.viewer_key = None;
        if let Some(table) = &self.table {
            table.update(cx, |table, cx| table.clear_selection(cx));
        }
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> PageHeader {
        let mut header = PageHeader::new("Kustomize").description(match &self.dir {
            Some(dir) => dir.display().to_string(),
            None => t!("kustomize-description"),
        });
        let recent = Self::recent_dirs(cx);
        if !recent.is_empty() {
            let screen = cx.entity().downgrade();
            header = header.action(
                Button::new("kustomize-recent")
                    .outline()
                    .small()
                    .icon(IconName::ChevronDown)
                    .label(t!("kustomize-recent"))
                    .tooltip(t!("kustomize-recent-tooltip"))
                    .dropdown_menu(move |menu: PopupMenu, _, _| {
                        recent.iter().fold(menu, |menu, dir| {
                            let (screen, dir) = (screen.clone(), dir.clone());
                            menu.item(PopupMenuItem::new(dir.display().to_string()).on_click(
                                move |_, _, cx| {
                                    let dir = dir.clone();
                                    screen.update(cx, |this, cx| this.open_dir(dir, cx)).ok();
                                },
                            ))
                        })
                    }),
            );
        }
        let building = matches!(self.state, BuildState::Building);
        header
            .action(
                Button::new("kustomize-helm")
                    .ghost()
                    .small()
                    .label("--enable-helm")
                    .selected(self.enable_helm)
                    .tooltip(t!("kustomize-enable-helm-tooltip"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.enable_helm = !this.enable_helm;
                        this.rebuild(cx);
                        cx.notify();
                    })),
            )
            .action(
                Button::new("kustomize-rebuild")
                    .outline()
                    .small()
                    .icon(IconName::RefreshCw)
                    .tooltip(t!("kustomize-rebuild-tooltip"))
                    .disabled(self.dir.is_none() || building)
                    .on_click(cx.listener(|this, _, _, cx| this.rebuild(cx))),
            )
            .action(
                Button::new("kustomize-open")
                    .primary()
                    .small()
                    .icon(IconName::FolderOpen)
                    .label(t!("kustomize-open-folder"))
                    .on_click(cx.listener(|this, _, window, cx| this.pick_folder(window, cx))),
            )
    }

    fn render_failure(&self, failure: &Failure, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let color = theme.danger;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .py_3()
            .rounded_lg()
            .border_1()
            .border_color(color)
            .bg(color.opacity(TINT_OPACITY))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color)
                    .child(failure.title.to_string()),
            )
            .when(!failure.details.is_empty(), |el| {
                el.child(
                    div()
                        .id("kustomize-error")
                        .max_h(px(320.))
                        .overflow_y_scroll()
                        .text_xs()
                        .whitespace_normal()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.foreground)
                        .child(failure.details.clone()),
                )
            })
            .children(failure.hint.as_ref().map(|hint| {
                div()
                    .text_xs()
                    .whitespace_normal()
                    .text_color(theme.muted_foreground)
                    .child(hint.to_string())
            }))
            .into_any_element()
    }

    fn render_warnings(&self, built: &Built, cx: &mut Context<Self>) -> Option<AnyElement> {
        let warnings = built.build.warnings.trim();
        if warnings.is_empty() {
            return None;
        }
        let theme = cx.theme();
        Some(
            div()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(theme.warning)
                .text_xs()
                .whitespace_normal()
                .font_family(theme.mono_font_family.clone())
                .text_color(theme.warning)
                .child(warnings.to_owned())
                .into_any_element(),
        )
    }

    /// Whether the active context's security settings allow revealing
    /// Secret values (the build may hold real ones, the diff live ones).
    fn reveal_allowed(&self, cx: &App) -> bool {
        self.cluster
            .read(cx)
            .active_allows_secret(SecretAction::Reveal)
    }

    /// Secret values are shown: revealed and allowed.
    fn showing_secrets(&self, cx: &App) -> bool {
        self.revealed && self.reveal_allowed(cx)
    }

    fn render_reveal(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self.reveal_allowed(cx) {
            return Button::new("kustomize-reveal")
                .ghost()
                .xsmall()
                .icon(IconName::EyeOff)
                .label(t!("kustomize-secrets-hidden"))
                .tooltip(t!("kustomize-secrets-hidden-tooltip"))
                .disabled(true)
                .into_any_element();
        }
        let revealed = self.revealed;
        Button::new("kustomize-reveal")
            .ghost()
            .xsmall()
            .icon(if revealed {
                IconName::EyeOff
            } else {
                IconName::Eye
            })
            .label(if revealed {
                t!("kustomize-secrets-hide")
            } else {
                t!("kustomize-secrets-reveal")
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.revealed = !this.revealed;
                cx.notify();
            }))
            .into_any_element()
    }

    fn framed(content: impl IntoElement, cx: &App) -> AnyElement {
        div()
            .flex_1()
            .min_h_0()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .child(content)
            .into_any_element()
    }

    fn render_viewer(&self, cx: &App) -> AnyElement {
        match &self.viewer {
            Some(viewer) => Self::framed(
                Editor::new(viewer).h_full().bordered(false).readonly(true),
                cx,
            ),
            None => div().into_any_element(),
        }
    }

    fn render_list(&self, built: &Built, cx: &mut Context<Self>) -> AnyElement {
        let count = built.objects.len();
        let compare = match &self.compare {
            Compare::Off => match self.cluster.read(cx).state() {
                ConnectionState::Connected { .. } => None,
                _ => Some(t!("kustomize-compare-connect")),
            },
            Compare::Running => Some(t!("kustomize-compare-running")),
            Compare::Ready(results) => Some(t!(
                "kustomize-compare-result",
                counts = describe_counts(&DiffCounts::of(results))
            )),
        };
        let tool = match &built.build.tool {
            Tool::Kustomize(path) | Tool::Kubectl(path) => path.display().to_string(),
        };
        let line: Vec<String> = [Some(t!("kustomize-objects", count = count))]
            .into_iter()
            .chain([compare, Some(t!("kustomize-built-with", tool = tool))])
            .flatten()
            .collect();
        let selected = match self.list_tab {
            ListTab::Resources => 0,
            ListTab::Output => 1,
        };
        let tabs = TabBar::new("kustomize-list-tabs")
            .underline()
            .small()
            .selected_index(selected)
            .child(TabItem::new().label(t!("kustomize-tab-resources")))
            .child(TabItem::new().label(t!("kustomize-tab-output")))
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.list_tab = if *ix == 0 {
                    ListTab::Resources
                } else {
                    ListTab::Output
                };
                cx.notify();
            }));
        let theme = cx.theme();
        let summary = div()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().flex_1().min_w_0().truncate().child(line.join(" · ")))
            .when(
                built.has_secrets && self.list_tab == ListTab::Output,
                |el| el.child(self.render_reveal(cx)),
            );
        let body = match self.list_tab {
            ListTab::Resources if count == 0 => {
                EmptyState::new(IconName::Inbox, t!("kustomize-empty-build"))
                    .description(t!("kustomize-empty-build-description"))
                    .into_any_element()
            }
            ListTab::Resources => match &self.table {
                Some(table) => Self::framed(table.clone(), cx),
                None => div().into_any_element(),
            },
            ListTab::Output => self.render_viewer(cx),
        };
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .children(self.render_warnings(built, cx))
            .child(tabs)
            .child(summary)
            .child(body)
            .into_any_element()
    }

    fn render_detail(&self, built: &Built, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(object) = built.objects.get(index) else {
            return div().into_any_element();
        };
        let back = Button::new("kustomize-back")
            .ghost()
            .small()
            .icon(IconName::ArrowLeft)
            .label(t!("kustomize-tab-resources"))
            .on_click(cx.listener(|this, _, _, cx| this.close_detail(cx)));
        let name = match &object.namespace {
            Some(namespace) => format!("{} {namespace}/{}", object.kind, object.name),
            None => format!("{} {}", object.kind, object.name),
        };
        let theme = cx.theme();
        let title = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_base()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.foreground)
            .child(name);
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .child(back)
            .child(title)
            .when(object.is_secret(), |el| el.child(self.render_reveal(cx)));
        let selected = match self.detail_tab {
            DetailTab::Yaml => 0,
            DetailTab::Diff => 1,
        };
        let tabs = TabBar::new("kustomize-detail-tabs")
            .underline()
            .small()
            .selected_index(selected)
            .child(TabItem::new().label("YAML"))
            .child(TabItem::new().label(t!("kustomize-tab-diff")))
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.detail_tab = if *ix == 0 {
                    DetailTab::Yaml
                } else {
                    DetailTab::Diff
                };
                cx.notify();
            }));
        let body = match self.detail_tab {
            DetailTab::Yaml => self.render_viewer(cx),
            DetailTab::Diff => self.render_diff(object, index, cx),
        };
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(header)
            .child(tabs)
            .child(div().flex_1().min_h(px(240.)).flex().flex_col().child(body))
            .into_any_element()
    }

    fn render_diff(
        &self,
        object: &ObjectSummary,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let diff = match (&self.compare, self.compare_result(index)) {
            (_, Some(Ok(diff))) => diff,
            (_, Some(Err(error))) => {
                return EmptyState::new(IconName::CircleX, t!("kustomize-compare-error"))
                    .description(error.clone())
                    .into_any_element();
            }
            (Compare::Running, _) => {
                return EmptyState::new(IconName::LoaderCircle, t!("kustomize-comparing"))
                    .into_any_element();
            }
            _ => {
                return EmptyState::new(IconName::Globe, t!("kustomize-no-cluster"))
                    .description(t!("kustomize-no-cluster-description"))
                    .into_any_element();
            }
        };
        if diff.is_unchanged() {
            return EmptyState::new(IconName::Check, t!("kustomize-no-differences"))
                .description(t!("kustomize-no-differences-description"))
                .into_any_element();
        }
        let (before, after) = if object.is_secret() && !self.showing_secrets(cx) {
            diff.masked_texts()
        } else {
            (diff.live_yaml.clone(), diff.desired_yaml.clone())
        };
        let theme = cx.theme();
        let note = if diff.is_new() {
            t!("kustomize-diff-new")
        } else {
            t!("kustomize-diff-fields", count = diff.changes.len())
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(note),
            )
            .child(Self::framed(
                DiffView::new(
                    "kustomize-diff",
                    crate::editor::diff_view_lines(&before, &after),
                ),
                cx,
            ))
            .into_any_element()
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.state {
            BuildState::Idle => EmptyState::new(IconName::FolderOpen, t!("kustomize-no-folder"))
                .description(t!("kustomize-no-folder-description"))
                .action(
                    Button::new("kustomize-open-empty")
                        .outline()
                        .small()
                        .icon(IconName::FolderOpen)
                        .label(t!("kustomize-open-folder"))
                        .on_click(cx.listener(|this, _, window, cx| this.pick_folder(window, cx))),
                )
                .into_any_element(),
            BuildState::Building => {
                EmptyState::new(IconName::LoaderCircle, t!("kustomize-building"))
                    .description(self.dir.as_deref().map(display).unwrap_or_default())
                    .into_any_element()
            }
            BuildState::Failed(failure) => self.render_failure(failure, cx),
            BuildState::Ready(built) => {
                let built = built.clone();
                match self.selected {
                    Some(index) => self.render_detail(&built, index, cx),
                    None => self.render_list(&built, cx),
                }
            }
        }
    }
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

impl ScreenActivity for KustomizeScreen {
    /// Shown, the screen starts its watches; hidden, it stops them.
    fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            self.sync(cx);
        }
    }
}

impl Render for KustomizeScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_views(window, cx);
        let header = self.render_header(cx);
        div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_4()
            .p_8()
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(self.render_body(cx)),
            )
    }
}
