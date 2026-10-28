//! The settings screen, shown in the main window's content area: sections
//! on the left, settings on the right, search over all sections. Every
//! change is saved and applied immediately.

mod agent;
mod clusters;
mod general;
mod hotbar;
mod presets;
mod security;
mod tools;
mod vault;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::Contexts;
use kubach_core::settings::Settings;
use kubach_i18n::{Message, msg, t};
use kubach_ui::{NavItem, SettingGroup};

use super::{AppSettings, CloseSettings, SettingsTarget};
use crate::cluster::Cluster;
use crate::screens::PrometheusRegistry;

/// Emitted when the user leaves the settings screen (Esc or the close button).
pub struct CloseRequested;

/// Key context of the screen, for the Esc binding.
pub(super) const KEY_CONTEXT: &str = "Settings";

/// A page of the settings screen. `Clusters` and `Cluster` are the two
/// sub-pages under "Clusters" in the sidebar (#402).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    General,
    /// What applies to every cluster: kubeconfig files, the vault of
    /// imported configs, the startup context and background connections.
    Clusters,
    /// One context at a time, picked from a list: tile, default namespace,
    /// Prometheus, Helm, security and removal.
    Cluster,
    Agent,
    Tools,
    Presets,
}

/// A block of the selected context on the per-cluster page that a
/// [`SettingsTarget`] can scroll to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    TileColor,
    Prometheus,
    Security,
}

impl Section {
    const ALL: [Section; 6] = [
        Section::General,
        Section::Clusters,
        Section::Cluster,
        Section::Agent,
        Section::Tools,
        Section::Presets,
    ];

    fn label(self) -> String {
        self.label_message().to_string()
    }

    fn label_message(self) -> Message {
        match self {
            Section::General => msg!("settings-section-general"),
            Section::Clusters => msg!("settings-section-clusters"),
            Section::Cluster => msg!("settings-section-cluster"),
            Section::Agent => msg!("settings-section-agent"),
            Section::Tools => msg!("settings-section-tools"),
            Section::Presets => msg!("settings-section-presets"),
        }
    }

    /// Heading of the page and of its results while searching.
    fn title(self) -> String {
        self.title_message().to_string()
    }

    fn title_message(self) -> Message {
        match self {
            Section::Clusters => msg!("settings-title-clusters"),
            Section::Cluster => msg!("settings-title-cluster"),
            _ => self.label_message(),
        }
    }

    /// Words a search matches besides the group, such as the parent of a
    /// sub-page: in the interface language and in English.
    fn search_text(self) -> String {
        let title = self.title_message();
        let mut text = format!("{} {}", title, title.english());
        if let Some(parent) = self.parent() {
            text = format!("{parent} {} {text}", parent.english());
        }
        text
    }

    /// Sidebar item the page is nested under.
    fn parent(self) -> Option<Message> {
        matches!(self, Section::Clusters | Section::Cluster).then(|| msg!("settings-nav-clusters"))
    }

    /// Id of the page's elements, unique among the pages.
    fn key(self) -> &'static str {
        match self {
            Section::General => "general",
            Section::Clusters => "clusters-common",
            Section::Cluster => "clusters-each",
            Section::Agent => "agent",
            Section::Tools => "tools",
            Section::Presets => "presets",
        }
    }

    fn description(self) -> String {
        match self {
            Section::General => t!("settings-description-general"),
            Section::Clusters => t!("settings-description-clusters"),
            Section::Cluster => t!("settings-description-cluster"),
            Section::Agent => t!("settings-description-agent"),
            Section::Tools => t!("settings-description-tools"),
            Section::Presets => t!("settings-description-presets"),
        }
    }

    fn icon(self) -> Lucide {
        match self {
            Section::General => Lucide::SlidersHorizontal,
            Section::Clusters => Lucide::SlidersHorizontal,
            Section::Cluster => Lucide::Layers,
            Section::Agent => Lucide::Bot,
            Section::Tools => Lucide::Wrench,
            Section::Presets => Lucide::FileText,
        }
    }
}

/// One searchable setting.
struct Entry {
    /// Lowercase title, description and keywords.
    text: String,
    element: AnyElement,
}

impl Entry {
    fn new(text: impl AsRef<str>, element: impl IntoElement) -> Self {
        Self {
            text: text.as_ref().to_lowercase(),
            element: element.into_any_element(),
        }
    }
}

/// A titled block of entries, with notices shown above it.
struct Group {
    title: SharedString,
    description: Option<SharedString>,
    /// Lowercase words searched like the title but not shown, such as the
    /// context the group belongs to.
    keywords: String,
    /// Block a [`SettingsTarget`] scrolls to.
    anchor: Option<Anchor>,
    notices: Vec<AnyElement>,
    entries: Vec<Entry>,
}

impl Group {
    fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            description: None,
            keywords: String::new(),
            anchor: None,
            notices: Vec::new(),
            entries: Vec::new(),
        }
    }

    fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    fn keywords(mut self, keywords: impl AsRef<str>) -> Self {
        self.keywords = keywords.as_ref().to_lowercase();
        self
    }

    fn anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = Some(anchor);
        self
    }

    fn notice(mut self, notice: impl IntoElement) -> Self {
        self.notices.push(notice.into_any_element());
        self
    }

    fn entry(mut self, entry: Entry) -> Self {
        self.entries.push(entry);
        self
    }

    /// Keeps entries matching every word of `query` (matching the group
    /// title keeps all of them); `None` when nothing is left.
    fn filter(mut self, words: &[String], section: Section) -> Option<Self> {
        if words.is_empty() {
            return Some(self);
        }
        let header =
            format!("{} {} {}", section.search_text(), self.title, self.keywords).to_lowercase();
        if !words.iter().all(|word| header.contains(word.as_str())) {
            self.entries.retain(|entry| {
                words.iter().all(|word| {
                    entry.text.contains(word.as_str()) || header.contains(word.as_str())
                })
            });
        }
        (!self.entries.is_empty()).then_some(self)
    }
}

fn index_of<T: PartialEq + Copy, L>(options: &[(T, L)], value: T) -> usize {
    options.iter().position(|(v, _)| *v == value).unwrap_or(0)
}

fn labels<T>(options: &[(T, &'static str)]) -> Vec<&'static str> {
    options.iter().map(|(_, label)| *label).collect()
}

/// A choice of a select with its label in the interface language.
type Choice<T> = (T, fn() -> String);

/// Labels of `options` in the interface language.
fn translated<T>(options: &[Choice<T>]) -> Vec<String> {
    options.iter().map(|(_, label)| label()).collect()
}

/// Search text of an entry: English keywords plus its title and
/// description in the interface language, so either language finds it.
fn searchable(keywords: &str, shown: &[&str]) -> String {
    let mut text = keywords.to_owned();
    for part in shown {
        text.push(' ');
        text.push_str(part);
    }
    text
}

/// A handler that edits the settings with the value it receives.
fn setter<T: 'static>(
    settings: &Entity<AppSettings>,
    apply: impl Fn(&mut Settings, T) + 'static,
) -> impl Fn(T, &mut Window, &mut App) + 'static {
    let settings = settings.clone();
    move |value, _, cx| {
        settings.update(cx, |settings, cx| {
            settings.edit(cx, |settings| apply(settings, value))
        });
    }
}

/// Settings screen; the host shows it until [`CloseRequested`].
pub struct SettingsScreen {
    focus: FocusHandle,
    settings: Entity<AppSettings>,
    section: Section,
    search: Entity<InputState>,
    query: String,
    /// Loads and watches the kubeconfig files and the vault; the screen
    /// shows what it has loaded.
    cluster: Entity<Cluster>,
    /// The contexts of `cluster` as of `revision`: kubeconfig, the extra
    /// files in settings and the vault's imported configs.
    contexts: Result<Contexts, SharedString>,
    /// [`Cluster::revision`] `contexts` was copied at.
    revision: Option<u64>,
    /// Context whose settings the per-cluster page shows; `None` or a gone
    /// context shows the first.
    cluster_context: Option<String>,
    /// Filter of the context list on the per-cluster page.
    context_filter: Entity<InputState>,
    /// Default namespace and Prometheus inputs of the clusters page.
    clusters: clusters::ClustersForm,
    agent: agent::AgentForm,
    /// Node shell image and namespace (#318).
    node_shell: general::NodeShellForm,
    /// Color picker of the selected context's tile.
    tile_color: hotbar::TileColorForm,
    /// Name inputs of the resource presets.
    presets: presets::PresetsForm,
    /// Scroll of the page; its direct children are the groups.
    scroll: ScrollHandle,
    /// Block to scroll to once the page is laid out.
    pending_anchor: Option<Anchor>,
    /// Bytes in Kubach's disk cache; `None` while counting.
    cache_size: Option<u64>,
    /// Why the last "Clear cache" failed.
    cache_error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CloseRequested> for SettingsScreen {}

impl SettingsScreen {
    pub fn new(
        target: SettingsTarget,
        cluster: Entity<Cluster>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings = AppSettings::global(cx);
        let search = cx.new(|cx| InputState::new(window, cx));
        let context_filter = cx.new(|cx| InputState::new(window, cx));
        let placeholders = [
            crate::i18n::localized_placeholder(
                &search,
                || t!("settings-search-placeholder"),
                window,
                cx,
            ),
            crate::i18n::localized_placeholder(
                &context_filter,
                || t!("settings-filter-contexts-placeholder"),
                window,
                cx,
            ),
        ];
        let mut subscriptions = vec![
            cx.subscribe(&context_filter, |_, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&search, window, |this, search, event, _, cx| {
                if let InputEvent::Change = event {
                    this.query = search.read(cx).value().to_lowercase();
                    cx.notify();
                }
            }),
            cx.observe_in(&settings, window, |this, _, window, cx| {
                this.sync(window, cx);
                cx.notify();
            }),
            // Contexts reloaded after a file, the linked files or the vault
            // changed; connection changes only redraw.
            cx.observe_in(&cluster, window, |this, _, window, cx| {
                this.sync_contexts(window, cx);
                cx.notify();
            }),
        ];
        subscriptions.extend(placeholders.into_iter().flatten());
        if let Some(vault) = crate::vault::global(cx) {
            // Lock state, busy actions and the imported list.
            subscriptions.push(cx.observe(&vault, |_, _, cx| cx.notify()));
        }
        if let Some(state) = crate::agent::global(cx) {
            subscriptions.push(cx.observe(&state, |_, _, cx| cx.notify()));
        }
        if let Some(registry) = PrometheusRegistry::try_global(cx) {
            // Searches show their progress and result.
            subscriptions.push(cx.observe(&registry, |_, _, cx| cx.notify()));
        }
        if let Some(downloads) = crate::tools::global(cx) {
            subscriptions.push(cx.observe(&downloads, |_, _, cx| cx.notify()));
        }
        let agent = agent::AgentForm::new(window, cx);
        let current = settings.read(cx).get().clone();
        let node_shell = general::NodeShellForm::new(&current, window, cx);
        let tile_color = hotbar::TileColorForm::new(window, cx);
        let mut this = Self {
            focus: cx.focus_handle(),
            settings,
            section: Section::General,
            search,
            query: String::new(),
            cluster,
            contexts: Err(SharedString::default()),
            revision: None,
            cluster_context: None,
            context_filter,
            clusters: clusters::ClustersForm::default(),
            agent,
            node_shell,
            tile_color,
            presets: presets::PresetsForm::default(),
            scroll: ScrollHandle::new(),
            pending_anchor: None,
            cache_size: None,
            cache_error: None,
            _subscriptions: subscriptions,
        };
        this.count_cache(cx);
        this.sync(window, cx);
        this.show(target, window, cx);
        this
    }

    /// Switches to `target` (keeping the current page when it names none)
    /// and focuses the screen so Esc closes it.
    pub fn show(&mut self, target: SettingsTarget, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(section) = target.section {
            self.section = section;
        }
        if let Some(context) = target.context {
            self.cluster_context = Some(context);
            // The context must show in the list.
            if !self.context_filter.read(cx).value().is_empty() {
                self.context_filter
                    .update(cx, |filter, cx| filter.set_value("", window, cx));
            }
        }
        self.pending_anchor = target.anchor;
        self.sync_tile_color(window, cx);
        if !self.query.is_empty() {
            self.query.clear();
            self.search
                .update(cx, |search, cx| search.set_value("", window, cx));
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Brings derived state in line with the settings: picks up reloaded
    /// contexts and updates namespace inputs. A changed kubeconfig list is
    /// reloaded by [`Cluster`], which the screen observes.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_contexts(window, cx);
        let settings = self.settings.read(cx).get().clone();
        for (context, input) in &self.clusters.namespace_inputs {
            let stored = settings.clusters.default_namespace(context).unwrap_or("");
            if input.read(cx).value() != stored {
                input.update(cx, |input, cx| {
                    input.set_value(stored.to_owned(), window, cx)
                });
            }
        }
        for (context, input) in &self.clusters.prometheus_inputs {
            let stored = settings.clusters.prometheus(context).unwrap_or("");
            // Trimmed on save: leave spaces being typed alone.
            if input.read(cx).value().trim() != stored {
                input.update(cx, |input, cx| {
                    input.set_value(stored.to_owned(), window, cx)
                });
            }
        }
        self.node_shell.sync(&settings, window, cx);
        self.sync_tile_color(window, cx);
        self.sync_presets(window, cx);
    }

    /// Whether the kubeconfig is still being read for the first time.
    fn contexts_loading(&self, cx: &App) -> bool {
        self.cluster.read(cx).is_loading()
    }

    /// Context names, in kubeconfig order.
    fn context_names(&self) -> Vec<String> {
        self.contexts
            .as_ref()
            .map(|contexts| contexts.list().iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default()
    }

    /// Context the per-cluster page shows: the picked one while it exists,
    /// else the first.
    fn selected_context(&self) -> Option<String> {
        shown_context(self.context_names(), self.cluster_context.clone())
    }

    /// Shows the settings of `context` on the per-cluster page.
    fn select_context(
        &mut self,
        context: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cluster_context = context;
        self.sync_tile_color(window, cx);
        cx.notify();
    }

    /// Copies the contexts of [`Cluster`] when it has loaded new ones and
    /// rebuilds the per-context inputs when the contexts themselves changed.
    fn sync_contexts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cluster = self.cluster.read(cx);
        if cluster.is_loading() || self.revision == Some(cluster.revision()) {
            return;
        }
        self.revision = Some(cluster.revision());
        let contexts = cluster.contexts().cloned().map_err(Clone::clone);
        let changed = context_list(&contexts) != context_list(&self.contexts);
        self.contexts = contexts;
        if changed {
            let settings = self.settings.read(cx).get().clone();
            self.rebuild_context_inputs(&settings, window, cx);
        }
    }

    fn groups(&self, section: Section, cx: &mut Context<Self>) -> Vec<Group> {
        let settings = self.settings.read(cx).get().clone();
        match section {
            Section::General => self.general(&settings, cx),
            Section::Clusters => self.clusters(&settings, cx),
            Section::Cluster => self.cluster_page(&settings, cx),
            Section::Agent => self.agent(&settings, cx),
            Section::Tools => self.tools(&settings, cx),
            Section::Presets => self.presets_page(&settings),
        }
    }

    fn render_sidebar(&self, counts: &[usize], cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let searching = !self.query.trim().is_empty();
        div()
            .w(px(220.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_0p5()
            .p_2()
            .border_r_1()
            .border_color(theme.border)
            // Always in sight, also while searching.
            .child(
                Button::new("close-settings")
                    .outline()
                    .small()
                    .w_full()
                    .icon(IconName::ArrowLeft)
                    .label(t!("settings-back"))
                    .tooltip(t!("settings-back-tooltip"))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseRequested))),
            )
            .child(
                div()
                    .mt_2()
                    .mb_2()
                    .child(Input::new(&self.search).small().cleanable(true)),
            )
            .children(
                Section::ALL
                    .into_iter()
                    .zip(counts)
                    .flat_map(|(section, count)| {
                        // The parent of the sub-pages opens the first of them.
                        let parent = (section == Section::Clusters).then(|| {
                            NavItem::new(
                                "settings-nav-clusters",
                                Lucide::Server,
                                t!("settings-nav-clusters"),
                            )
                            .on_click(Self::open_section(section, cx))
                            .into_any_element()
                        });
                        let item = NavItem::new(
                            SharedString::from(format!("settings-nav-{}", section.key())),
                            section.icon(),
                            section.label(),
                        )
                        .nested(section.parent().is_some())
                        .selected(!searching && section == self.section)
                        .on_click(Self::open_section(section, cx));
                        let item = if searching { item.count(*count) } else { item };
                        parent.into_iter().chain([item.into_any_element()])
                    }),
            )
    }

    /// Click handler that shows `section` and clears the search.
    fn open_section(
        section: Section,
        cx: &mut Context<Self>,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, _, window, cx| {
            this.section = section;
            this.query.clear();
            this.search
                .update(cx, |search, cx| search.set_value("", window, cx));
            cx.notify();
        })
    }

    fn render_groups(section: Section, groups: Vec<Group>) -> Vec<AnyElement> {
        groups
            .into_iter()
            .enumerate()
            .map(|(ix, group)| {
                let id = match group.anchor {
                    Some(anchor) => SharedString::from(format!("{}-{anchor:?}", section.key())),
                    None => SharedString::from(format!("{}-{ix}", section.key())),
                };
                let mut block = SettingGroup::new().title(group.title.clone());
                if let Some(description) = group.description {
                    block = block.description(description);
                }
                let has_entries = !group.entries.is_empty();
                div()
                    .id(id)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(group.notices)
                    .when(has_entries, |this| {
                        this.child(
                            block.children(group.entries.into_iter().map(|entry| entry.element)),
                        )
                    })
                    .into_any_element()
            })
            .collect()
    }
}

/// The context the per-cluster page shows: `picked` (as a
/// [`SettingsTarget`] names it) while it is among `names`, else the first.
fn shown_context(names: Vec<String>, picked: Option<String>) -> Option<String> {
    picked
        .filter(|name| names.contains(name))
        .or_else(|| names.into_iter().next())
}

/// Name and kubeconfig namespace of each context: what the per-context
/// inputs are built from.
fn context_list(contexts: &Result<Contexts, SharedString>) -> Vec<(&str, Option<&str>)> {
    contexts
        .as_ref()
        .map(|contexts| {
            contexts
                .list()
                .iter()
                .map(|context| (context.name.as_str(), context.namespace.as_deref()))
                .collect()
        })
        .unwrap_or_default()
}

impl Render for SettingsScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let words: Vec<String> = self.query.split_whitespace().map(str::to_owned).collect();
        let searching = !words.is_empty();

        // While searching every section is filtered; otherwise only the
        // selected one is shown.
        let mut counts = Vec::new();
        let mut visible: Vec<(Section, Vec<Group>)> = Vec::new();
        for section in Section::ALL {
            if !searching && section != self.section {
                counts.push(0);
                continue;
            }
            let groups: Vec<Group> = self
                .groups(section, cx)
                .into_iter()
                .filter_map(|group| group.filter(&words, section))
                .collect();
            counts.push(groups.iter().map(|group| group.entries.len()).sum());
            if !groups.is_empty() {
                visible.push((section, groups));
            }
        }

        let state = self.settings.read(cx);
        let problems: Vec<SharedString> = state.problems().to_vec();
        let path = state.path().map(|path| path.display().to_string());

        let theme = cx.theme().clone();
        // Direct children of the scrolled element, so a group can be
        // scrolled to by its index.
        let mut content: Vec<AnyElement> = problems
            .into_iter()
            .enumerate()
            .map(|(ix, problem)| {
                Alert::error(("settings-problem", ix), problem)
                    .title(t!("settings-file-problem"))
                    .small()
                    .into_any_element()
            })
            .collect();
        let mut anchor_ix = None;
        if searching {
            if visible.is_empty() {
                content.push(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(t!("settings-no-match", query = self.query.trim()))
                        .into_any_element(),
                );
            }
            for (section, groups) in visible {
                content.push(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .child(section.title().to_uppercase())
                        .into_any_element(),
                );
                content.extend(Self::render_groups(section, groups));
            }
        } else {
            content.push(
                kubach_ui::PageHeader::new(self.section.title())
                    .description(self.section.description())
                    .into_any_element(),
            );
            for (section, groups) in visible {
                if let Some(anchor) = self.pending_anchor {
                    anchor_ix = groups
                        .iter()
                        .position(|group| group.anchor == Some(anchor))
                        .map(|ix| content.len() + ix);
                }
                content.extend(Self::render_groups(section, groups));
            }
        }
        content.push(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(match path {
                    Some(path) => t!("settings-saved-to", path = path),
                    None => t!("settings-not-saved"),
                })
                .into_any_element(),
        );
        if self.pending_anchor.is_some() {
            if searching || self.section != Section::Cluster {
                self.pending_anchor = None;
            } else if let Some(ix) = anchor_ix {
                self.scroll.scroll_to_top_of_item(ix);
                // Until the page has been laid out once, the viewport is
                // unknown: scroll again on the next frame.
                if self.scroll.bounds().size.height > px(0.) {
                    self.pending_anchor = None;
                } else {
                    cx.on_next_frame(window, |_, _, cx| cx.notify());
                }
            }
        }

        div()
            .id("settings-screen")
            .track_focus(&self.focus)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|_, _: &CloseSettings, _, cx| cx.emit(CloseRequested)))
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .text_color(theme.foreground)
            .child(self.render_sidebar(&counts, cx))
            .child({
                let content = div()
                    .id("settings-content")
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .min_w_0()
                    .overflow_y_scroll()
                    .p_8()
                    .flex()
                    .flex_col()
                    .gap_6()
                    .children(
                        content
                            .into_iter()
                            .map(|child| div().flex_shrink_0().child(child)),
                    );
                // Kubeconfig files dropped on the common page are added; the
                // per-cluster page lists the contexts beside its settings.
                if self.section == Section::Cluster && !searching {
                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .flex()
                        .child(self.render_context_list(cx))
                        .child(content)
                        .into_any_element()
                } else if self.section == Section::Clusters && !searching {
                    crate::vault::drop_zone("settings-clusters-drop", content, cx)
                        .flex_1()
                        .min_w_0()
                        .into_any_element()
                } else {
                    content.into_any_element()
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::div;

    use super::{Entry, Group, Section, searchable, shown_context};
    use crate::settings::SettingsTarget;

    fn words(query: &str) -> Vec<String> {
        query.split_whitespace().map(str::to_owned).collect()
    }

    fn group() -> Group {
        Group::new("Session")
            .keywords("security kind https://127.0.0.1:6443")
            .entry(Entry::new("session ttl time limit duration", div()))
            .entry(Entry::new("namespaces session several one", div()))
    }

    #[test]
    fn keywords_are_searched_like_the_title() {
        let kept = |query: &str| {
            group()
                .filter(&words(query), Section::Clusters)
                .map_or(0, |group| group.entries.len())
        };
        // The page, the hidden keywords and the title keep every entry.
        assert_eq!(kept("security"), 2);
        assert_eq!(kept("kind"), 2);
        assert_eq!(kept("clusters session"), 2);
        // A keyword narrows with an entry word.
        assert_eq!(kept("security ttl"), 1);
        assert_eq!(kept("hotbar"), 0);
    }

    #[test]
    fn entries_are_found_in_english_and_in_the_interface_language() {
        let text = searchable("theme dark light", &["Тема"]);
        assert!(text.contains("theme"));
        assert!(text.contains("Тема"));
    }

    #[test]
    fn a_target_opens_the_tab_of_its_context() {
        let names = || vec!["minikube".to_owned(), "kind".to_owned(), "prod".to_owned()];
        for target in [
            SettingsTarget::tile_color("kind"),
            SettingsTarget::prometheus("kind"),
            SettingsTarget::security("kind"),
        ] {
            assert_eq!(target.section, Some(Section::Cluster));
            assert_eq!(
                shown_context(names(), target.context).as_deref(),
                Some("kind")
            );
        }
        // A gone context falls back to the first; no contexts, no tab.
        assert_eq!(
            shown_context(names(), Some("gone".into())).as_deref(),
            Some("minikube")
        );
        assert_eq!(shown_context(Vec::new(), Some("kind".into())), None);
        // Kubeconfig and vault links go to the common page.
        assert_eq!(
            SettingsTarget::section(Section::Clusters).section,
            Some(Section::Clusters)
        );
    }

    #[test]
    fn sub_pages_are_found_by_their_parent() {
        assert!(
            Section::Cluster
                .search_text()
                .to_lowercase()
                .contains("clusters")
        );
        assert!(
            Section::Clusters
                .search_text()
                .to_lowercase()
                .contains("common")
        );
        let keys: std::collections::HashSet<_> = Section::ALL.iter().map(|s| s.key()).collect();
        assert_eq!(keys.len(), Section::ALL.len());
    }
}
