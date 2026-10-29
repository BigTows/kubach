//! Charts page of the Helm screen (ADR 0006), its own sidebar item: the
//! user's chart repositories, a search over their cached indexes, a chart
//! card (versions, README, default values) and the install panel.
//!
//! Repositories and indexes are read natively from the user's Helm files
//! (`kubach_core::helm::repo`), so browsing works without the binary and
//! offline. `helm repo add/update/remove`, `helm show` and `helm install`
//! run through the user's `helm`; the Helm screen confirms and runs the
//! changes ([`ChartsEvent`]) so they share its progress toast.
//!
//! Clicking a repository narrows the chart list to it (#272), together with
//! the search; a chip over the list shows the filter and clears it, and so
//! does a second click on the repository.
//!
//! Chart icons and README images come from the web through
//! [`RemoteImages`] (#167, #169), only while "Load chart icons" is on.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tab::{Tab as TabItem, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Selectable as _, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::helm::Sensitive;
use kubach_core::helm::cli::{HelmCommand, HelmTarget, RepoCredentials, ShowKind};
use kubach_core::helm::repo::{
    self, ChartVersion, HelmPaths, IndexState, LoadedRepository, SearchOptions,
    check_repository_name, check_repository_url, suggest_repository_name,
};
use kubach_i18n::t;
use kubach_ui::{
    ChartIcon, ConfirmDialog, EmptyState, FilterBar, KeyValueList, Panel, ResourceCell,
    ResourceColumn, ResourceRow, ResourceTable, StatusBadge, Tone,
};

use super::helm::HelmScreen;
use super::helm_install::{HelmInstall, InstallEvent, InstallInput, InstallSource, show};
use super::helm_release::validation_text;
use super::namespace_picker::Namespaces;
use super::table_sort;
use crate::images::RemoteImages;
use crate::settings::AppSettings;

/// Search rows shown at most; a narrower query shows the rest.
const MAX_ROWS: usize = 2000;

pub enum ChartsEvent {
    /// Run a repository command (`helm repo add/update/remove`); the
    /// repositories are read again afterwards.
    Run { title: String, command: HelmCommand },
    /// Confirm and run this install.
    Install(Box<InstallInput>),
}

/// Text of a chart card tab.
enum Text {
    Loading(#[allow(dead_code)] Task<()>),
    Ready(String),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CardTab {
    Readme,
    Values,
}

impl CardTab {
    const ALL: [Self; 2] = [Self::Readme, Self::Values];

    fn title(self) -> String {
        match self {
            Self::Readme => t!("helm-card-readme"),
            Self::Values => t!("helm-card-default-values"),
        }
    }

    fn kind(self) -> ShowKind {
        match self {
            Self::Readme => ShowKind::Readme,
            Self::Values => ShowKind::Values,
        }
    }
}

/// An open chart card.
struct Card {
    repository: String,
    chart: String,
    /// Picked version.
    version: String,
    tab: CardTab,
    texts: Vec<((CardTab, String), Text)>,
    viewer: Option<Entity<EditorState>>,
    viewer_key: Option<(CardTab, String)>,
    /// The README tab shows the Markdown source in the editor instead of
    /// rendering it.
    readme_source: bool,
}

impl Card {
    fn text(&self, tab: CardTab, version: &str) -> Option<&Text> {
        self.texts
            .iter()
            .find(|((t, v), _)| *t == tab && v == version)
            .map(|(_, text)| text)
    }
}

struct AddForm {
    name: Entity<InputState>,
    url: Entity<InputState>,
    /// Basic auth of a private repository; both empty without it.
    username: Entity<InputState>,
    password: Entity<InputState>,
    /// The name last filled in from the URL; while the name field still holds
    /// it (or is empty), a new URL replaces it.
    suggested_name: Option<String>,
}

/// A repository to add with `helm repo add`.
struct NewRepository {
    name: String,
    url: String,
    credentials: Option<RepoCredentials>,
}

pub struct HelmCharts {
    screen: WeakEntity<HelmScreen>,
    paths: Option<HelmPaths>,
    repos: Vec<LoadedRepository>,
    /// The repository list could not be read.
    error: Option<String>,
    /// Repositories were read at least once.
    loaded: bool,
    load_task: Option<Task<()>>,
    search: Entity<InputState>,
    all_versions: bool,
    /// Only charts of this repository are listed.
    repository: Option<String>,
    table: Entity<ResourceTable>,
    /// Matches of the current search and whether rows were cut off.
    matches: (usize, bool),
    add: Option<AddForm>,
    card: Option<Card>,
    install: Option<(Entity<HelmInstall>, Subscription)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ChartsEvent> for HelmCharts {}

impl HelmCharts {
    pub fn new(
        screen: WeakEntity<HelmScreen>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = FilterBar::search_state("", window, cx);
        let mut subscriptions = vec![cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.update_rows(cx);
            }
        })];
        subscriptions.extend(crate::i18n::localized_placeholder(
            &search,
            || t!("helm-search-charts"),
            window,
            cx,
        ));
        // Table headers are kept in the table: rebuild it in the new
        // language.
        if let Some(language) = crate::i18n::global(cx) {
            subscriptions.push(cx.observe_in(&language, window, |this, _, window, cx| {
                this.table = Self::new_table(window, cx);
                this.update_rows(cx);
            }));
        }
        // helm may be downloaded or its path set meanwhile.
        if let Some(settings) = AppSettings::try_global(cx) {
            subscriptions.push(cx.observe(&settings, |_, _, cx| cx.notify()));
        }
        if let Some(downloads) = crate::tools::global(cx) {
            subscriptions.push(cx.observe(&downloads, |_, _, cx| cx.notify()));
        }
        // An icon or README image arrived, or the setting changed.
        if let Some(images) = RemoteImages::global(cx) {
            subscriptions.push(cx.observe(&images, |this, _, cx| {
                this.table.update(cx, |_, cx| cx.notify());
                cx.notify();
            }));
        }
        let table = Self::new_table(window, cx);
        Self {
            screen,
            paths: None,
            repos: Vec::new(),
            error: None,
            loaded: false,
            load_task: None,
            search,
            all_versions: false,
            repository: None,
            table,
            matches: (0, false),
            add: None,
            card: None,
            install: None,
            _subscriptions: subscriptions,
        }
    }

    fn new_table(window: &mut Window, cx: &mut Context<Self>) -> Entity<ResourceTable> {
        let charts = cx.entity().downgrade();
        let table = cx.new(|cx| {
            ResourceTable::new(chart_columns(), window, cx).on_select(move |row, window, cx| {
                let key = row.key.to_string();
                charts
                    .update(cx, |this, cx| this.open_card_by_key(&key, window, cx))
                    .ok();
            })
        });
        table_sort::remember(&table, table_sort::screen("helm.charts"), cx);
        table
    }

    /// Repositories as last read.
    pub fn repositories(&self) -> &[LoadedRepository] {
        &self.repos
    }

    /// Reads the repository list and the cached indexes again (unchanged
    /// index files are reused). Called when the page is shown and after
    /// repository commands.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        // The environment may have changed (login shell loaded).
        let paths = HelmPaths::from_env();
        self.paths = paths.clone();
        let Some(paths) = paths else {
            self.error = Some(t!("helm-no-home"));
            self.loaded = true;
            cx.notify();
            return;
        };
        let previous = self.repos.clone();
        let load = cx.background_spawn(async move { repo::load(&paths, &previous) });
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |this, cx| {
                this.load_task = None;
                this.loaded = true;
                match result {
                    Ok(repos) => {
                        this.repos = repos;
                        this.error = None;
                        // The filtered repository was removed.
                        if let Some(name) = &this.repository
                            && !this.repos.iter().any(|r| r.repository.name == *name)
                        {
                            this.repository = None;
                        }
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                this.update_rows(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Back to the chart list with the focus in its search (`⌘F`).
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.install = None;
        if self.card.is_some() {
            self.close_card(cx);
        }
        self.search
            .update(cx, |search, cx| search.focus(window, cx));
    }

    pub fn close_install(&mut self, cx: &mut Context<Self>) {
        self.install = None;
        cx.notify();
    }

    /// The install started from the open panel failed: the panel shows it.
    pub fn install_failed(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some((panel, _)) = &self.install {
            panel.update(cx, |panel, cx| panel.install_failed(error, cx));
        }
    }

    fn options(&self) -> SearchOptions<'_> {
        SearchOptions {
            all_versions: self.all_versions,
            devel: self.all_versions,
            repository: self.repository.as_deref(),
        }
    }

    /// Lists only the charts of `name`; a second click lists all again.
    fn toggle_repository(&mut self, name: String, cx: &mut Context<Self>) {
        self.repository = if self.repository.as_deref() == Some(name.as_str()) {
            None
        } else {
            Some(name)
        };
        self.update_rows(cx);
    }

    fn clear_repository(&mut self, cx: &mut Context<Self>) {
        self.repository = None;
        self.update_rows(cx);
    }

    fn update_rows(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().to_string();
        let hits = repo::search(&self.repos, &query, self.options());
        self.matches = (hits.len(), hits.len() > MAX_ROWS);
        let rows = hits.iter().take(MAX_ROWS).map(chart_row).collect();
        self.table.update(cx, |table, cx| table.set_rows(rows, cx));
        cx.notify();
    }

    fn open_card_by_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((repository, chart, version)) = parse_row_key(key) {
            self.open_card(repository, chart, version, window, cx);
        }
    }

    fn open_card(
        &mut self,
        repository: String,
        chart: String,
        version: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let viewer = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("yaml")
                .line_number(true)
        });
        self.card = Some(Card {
            repository,
            chart,
            version,
            tab: CardTab::Readme,
            texts: Vec::new(),
            viewer: Some(viewer),
            viewer_key: None,
            readme_source: false,
        });
        self.ensure_card_text(cx);
        cx.notify();
    }

    fn close_card(&mut self, cx: &mut Context<Self>) {
        self.card = None;
        self.table.update(cx, |table, cx| table.clear_selection(cx));
        cx.notify();
    }

    /// Starts loading the text of the card's tab for its version.
    fn ensure_card_text(&mut self, cx: &mut Context<Self>) {
        if HelmScreen::helm_path(cx).is_none() {
            return;
        }
        let Some(card) = &self.card else {
            return;
        };
        let (tab, version) = (card.tab, card.version.clone());
        if card.text(tab, &version).is_some() {
            return;
        }
        let chart = repo::chart_reference(&card.repository, &card.chart);
        let call = show(cx, tab.kind(), chart, Some(version.clone()));
        let task_version = version.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = call.await;
            this.update(cx, |this, cx| {
                if let Some(card) = &mut this.card
                    && let Some((_, text)) = card
                        .texts
                        .iter_mut()
                        .find(|((t, v), _)| *t == tab && *v == task_version)
                {
                    *text = match result {
                        Ok(output) => Text::Ready(output),
                        Err(error) => Text::Failed(error),
                    };
                    cx.notify();
                }
            })
            .ok();
        });
        if let Some(card) = &mut self.card {
            card.texts.push(((tab, version), Text::Loading(task)));
        }
    }

    /// Loads the shown text into the card's viewer when it changed.
    fn ensure_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(card) = &mut self.card else {
            return;
        };
        let key = (card.tab, card.version.clone());
        if card.viewer_key.as_ref() == Some(&key) {
            return;
        }
        let Some(Text::Ready(text)) = card.text(key.0, &key.1) else {
            return;
        };
        let text = text.clone();
        if let Some(viewer) = &card.viewer {
            viewer.update(cx, |viewer, cx| viewer.set_value(text, window, cx));
        }
        card.viewer_key = Some(key);
    }

    fn install_target(&self, cx: &App) -> Option<(HelmTarget, String, Entity<Namespaces>)> {
        let screen = self.screen.upgrade()?;
        let screen = screen.read(cx);
        Some((
            screen.install_target(cx)?,
            screen.install_namespace(cx),
            screen.shared_namespaces(),
        ))
    }

    fn open_install(
        &mut self,
        source: InstallSource,
        version: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((target, namespace, namespaces)) = self.install_target(cx) else {
            return;
        };
        let panel = cx
            .new(|cx| HelmInstall::new(target, source, version, namespace, namespaces, window, cx));
        let subscription = cx.subscribe(&panel, |this, _, event: &InstallEvent, cx| match event {
            InstallEvent::Cancel => this.close_install(cx),
            InstallEvent::Apply(input) => cx.emit(ChartsEvent::Install(input.clone())),
        });
        let focus = panel.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        self.install = Some((panel, subscription));
        cx.notify();
    }

    fn open_card_install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(card) = &self.card else {
            return;
        };
        let versions = repo::find_chart(&self.repos, &card.repository, &card.chart)
            .map(|versions| versions.iter().map(|v| v.version.clone()).collect())
            .unwrap_or_default();
        let source = InstallSource::Repository {
            repository: card.repository.clone(),
            chart: card.chart.clone(),
            versions,
        };
        let version = Some(card.version.clone());
        self.open_install(source, version, window, cx);
    }

    fn toggle_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.add.is_some() {
            self.add = None;
        } else {
            let name = cx.new(|cx| InputState::new(window, cx));
            let url = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("https://prometheus-community.github.io/helm-charts")
            });
            let username = cx.new(|cx| InputState::new(window, cx));
            let password = cx.new(|cx| InputState::new(window, cx).masked(true));
            for (input, text) in [
                (
                    &name,
                    (|| t!("helm-repo-name-placeholder")) as fn() -> String,
                ),
                (&username, || t!("helm-repo-username-placeholder")),
                (&password, || t!("helm-repo-password-placeholder")),
            ] {
                self._subscriptions
                    .extend(crate::i18n::localized_placeholder(input, text, window, cx));
            }
            let submit_on_enter =
                |this: &mut Self, _, event: &InputEvent, cx: &mut Context<Self>| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.submit_add(cx);
                    }
                    cx.notify();
                };
            self._subscriptions.extend([
                cx.subscribe(&name, |_, _, _: &InputEvent, cx| cx.notify()),
                cx.subscribe(&url, submit_on_enter),
                cx.subscribe_in(&url, window, |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::Change = event {
                        this.suggest_name(window, cx);
                    }
                }),
                cx.subscribe(&username, submit_on_enter),
                cx.subscribe(&password, submit_on_enter),
            ]);
            // The name is filled from the URL, so the URL comes first.
            url.focus_handle(cx).focus(window, cx);
            self.add = Some(AddForm {
                name,
                url,
                username,
                password,
                suggested_name: None,
            });
        }
        cx.notify();
    }

    /// Fills the name from the URL unless the name was typed by hand.
    fn suggest_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.add else {
            return;
        };
        let current = form.name.read(cx).value().trim().to_owned();
        let untouched = current.is_empty() || form.suggested_name.as_deref() == Some(&current);
        if !untouched {
            return;
        }
        let suggestion = suggest_repository_name(&form.url.read(cx).value());
        let value = suggestion.clone().unwrap_or_default();
        if value != current {
            form.name
                .update(cx, |name, cx| name.set_value(value, window, cx));
        }
        form.suggested_name = suggestion;
    }

    /// The repository to add, or why it can't be added.
    fn add_input(&self, cx: &App) -> Option<Result<NewRepository, String>> {
        let form = self.add.as_ref()?;
        let name = form.name.read(cx).value().trim().to_owned();
        let url = form.url.read(cx).value().trim().to_owned();
        let username = form.username.read(cx).value().trim().to_owned();
        // Taken as typed: spaces may be part of a password.
        let password = form.password.read(cx).value().to_string();
        Some((|| {
            check_repository_name(&name).map_err(|error| validation_text(&error))?;
            if self.repos.iter().any(|r| r.repository.name == name) {
                return Err(t!("helm-repo-exists", name = name.clone()));
            }
            check_repository_url(&url).map_err(|error| validation_text(&error))?;
            let credentials = match (username.is_empty(), password.is_empty()) {
                (true, true) => None,
                (true, false) => return Err(t!("helm-repo-username-missing")),
                (false, _) => Some(RepoCredentials {
                    username,
                    password: Sensitive::new(password),
                }),
            };
            if credentials.is_some() {
                repo::check_repository_credentials(&url)
                    .map_err(|error| validation_text(&error))?;
            }
            Ok(NewRepository {
                name,
                url,
                credentials,
            })
        })())
    }

    fn submit_add(&mut self, cx: &mut Context<Self>) {
        let Some(Ok(repository)) = self.add_input(cx) else {
            return;
        };
        if self.busy(cx) {
            return;
        }
        // Dropping the form drops the typed password too.
        self.add = None;
        cx.emit(ChartsEvent::Run {
            title: t!("helm-repo-adding", name = repository.name.clone()),
            command: HelmCommand::repo_add(
                &repository.name,
                &repository.url,
                repository.credentials.as_ref(),
            ),
        });
        cx.notify();
    }

    fn update_repos(&mut self, names: &[&str], cx: &mut Context<Self>) {
        let title = match names {
            [] => t!("helm-repo-updating-all"),
            [name] => t!("helm-repo-updating", name = *name),
            names => t!("helm-repo-updating-count", count = names.len()),
        };
        cx.emit(ChartsEvent::Run {
            title,
            command: HelmCommand::repo_update(names),
        });
    }

    fn request_remove(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let command = HelmCommand::repo_remove(&name);
        let message = t!("helm-repo-remove-message", name = name.clone());
        let display = command.display();
        let charts = cx.entity().downgrade();
        ConfirmDialog::open(window, cx, move |_, _| {
            let (charts, command, name) = (charts.clone(), command.clone(), name.clone());
            ConfirmDialog::new(t!("helm-repo-remove-title", name = name.clone()))
                .message(message)
                .command(display.clone())
                .confirm_label(t!("helm-repo-remove-confirm"))
                .danger()
                .on_confirm(move |_, cx| {
                    let (command, name) = (command.clone(), name.clone());
                    charts
                        .update(cx, |_, cx| {
                            cx.emit(ChartsEvent::Run {
                                title: t!("helm-repo-removing", name = name.clone()),
                                command,
                            })
                        })
                        .ok();
                })
        });
    }

    /// No helm, or a helm call is running.
    fn busy(&self, cx: &App) -> bool {
        HelmScreen::helm_path(cx).is_none()
            || self
                .screen
                .upgrade()
                .is_some_and(|screen| screen.read(cx).action_running())
    }

    fn render_missing_helm(&self, cx: &App) -> Option<AnyElement> {
        if HelmScreen::helm_path(cx).is_some() {
            return None;
        }
        // Browsing works without helm; repository commands, chart details
        // and installs offer the same download as the release actions.
        Some(crate::tools::helm_missing_hint(cx))
    }

    fn render_repositories(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.busy(cx);
        let theme = cx.theme();
        let rows = self.repos.iter().enumerate().map(|(ix, loaded)| {
            let name = loaded.repository.name.clone();
            let (status, color) = match &loaded.index {
                IndexState::Ready(index) => {
                    let count = index.charts.len();
                    (
                        t!("helm-repo-charts", count = count),
                        theme.muted_foreground,
                    )
                }
                IndexState::Missing => (t!("helm-repo-index-missing"), theme.warning),
                IndexState::Invalid(error) => (
                    t!("helm-repo-index-invalid", error = error.to_string()),
                    theme.danger,
                ),
            };
            let update_name = name.clone();
            let remove_name = name.clone();
            let filter_name = name.clone();
            let picked = self.repository.as_deref() == Some(name.as_str());
            div()
                .id(("helm-repo", ix))
                .flex()
                .items_center()
                .gap_2()
                .py_1p5()
                .px_1p5()
                .rounded_md()
                .cursor_pointer()
                .when(picked, |el| el.bg(theme.list_active))
                .when(!picked, |el| el.hover(|el| el.bg(theme.list_hover)))
                .when(ix > 0, |el| el.border_t_1().border_color(theme.border))
                .tooltip(move |window, cx| {
                    let text = if picked {
                        t!("helm-repo-show-all")
                    } else {
                        t!("helm-repo-show-only")
                    };
                    gpui_kit::component::tooltip::Tooltip::new(text).build(window, cx)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_repository(filter_name.clone(), cx)
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .truncate()
                                .child(name),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(loaded.repository.url.clone()),
                        )
                        .child(div().text_xs().text_color(color).truncate().child(status)),
                )
                .child(
                    Button::new(("helm-repo-update", ix))
                        .ghost()
                        .xsmall()
                        .icon(IconName::RefreshCw)
                        .tooltip(t!("helm-repo-update-tooltip"))
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.update_repos(&[update_name.as_str()], cx)
                        })),
                )
                .child(
                    Button::new(("helm-repo-remove", ix))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Delete)
                        .tooltip(t!("helm-repo-remove-tooltip"))
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.request_remove(remove_name.clone(), window, cx)
                        })),
                )
        });
        let list = if self.repos.is_empty() {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child(match &self.error {
                    Some(error) => error.clone(),
                    None if !self.loaded => t!("helm-repo-reading"),
                    None => t!("helm-repo-none-yet"),
                })
                .into_any_element()
        } else {
            div()
                .id("helm-repo-list")
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .children(rows)
                .into_any_element()
        };
        let add_form = self.add.as_ref().map(|form| {
            let input = self.add_input(cx);
            let error = input
                .as_ref()
                .and_then(|input| input.as_ref().err().cloned());
            let filled = [&form.name, &form.url, &form.username, &form.password]
                .iter()
                .any(|input| !input.read(cx).value().is_empty());
            div()
                .flex()
                .flex_col()
                .gap_2()
                .pt_2()
                .border_t_1()
                .border_color(theme.border)
                .child(Input::new(&form.url).small())
                .child(Input::new(&form.name).small())
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .whitespace_normal()
                        .child(t!("helm-repo-private-note")),
                )
                .child(Input::new(&form.username).small())
                .child(Input::new(&form.password).small().mask_toggle())
                .when_some(error.filter(|_| filled), |el, error| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(theme.warning)
                            .whitespace_normal()
                            .child(error),
                    )
                })
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("helm-repo-add-cancel")
                                .ghost()
                                .xsmall()
                                .label(t!("helm-cancel"))
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.toggle_add(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("helm-repo-add-submit")
                                .primary()
                                .xsmall()
                                .label(t!("helm-repo-add"))
                                .disabled(busy || !matches!(input, Some(Ok(_))))
                                .on_click(cx.listener(|this, _, _, cx| this.submit_add(cx))),
                        ),
                )
        });
        let toolbar = div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(t!("helm-repositories")),
            )
            .child(
                Button::new("helm-repo-update-all")
                    .ghost()
                    .xsmall()
                    .icon(IconName::RefreshCw)
                    .tooltip(t!("helm-repo-update-all-tooltip"))
                    .disabled(busy || self.repos.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.update_repos(&[], cx))),
            )
            .child(
                Button::new("helm-repo-add")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip(t!("helm-repo-add-tooltip"))
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_add(window, cx))),
            );
        let paths_note = self.paths.as_ref().map(|paths| {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child(t!(
                    "helm-repo-config-from",
                    path = paths.repository_config.display().to_string()
                ))
        });
        div()
            .w(px(300.))
            .flex_shrink_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(toolbar)
            .child(div().flex_1().min_h_0().flex().flex_col().child(list))
            .children(add_form)
            .children(paths_note)
            .into_any_element()
    }

    fn render_results(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let ready = self.repos.iter().any(|r| r.charts().is_some());
        let body = if !self.loaded {
            EmptyState::new(IconName::LoaderCircle, t!("helm-charts-reading")).into_any_element()
        } else if self.repos.is_empty() {
            EmptyState::new(IconName::Inbox, t!("helm-charts-no-repositories"))
                .description(t!("helm-charts-no-repositories-description"))
                .into_any_element()
        } else if !ready {
            EmptyState::new(IconName::Inbox, t!("helm-charts-no-indexes"))
                .description(t!("helm-charts-no-indexes-description"))
                .into_any_element()
        } else if self.matches.0 == 0 {
            let description = match &self.repository {
                Some(name)
                    if !self
                        .repos
                        .iter()
                        .any(|r| r.repository.name == *name && r.charts().is_some()) =>
                {
                    t!("helm-charts-repo-no-index", name = name.clone())
                }
                Some(name) => t!("helm-charts-repo-no-match", name = name.clone()),
                None => t!("helm-charts-no-match"),
            };
            EmptyState::new(IconName::Search, t!("helm-charts-none-found"))
                .description(description)
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(self.table.clone())
                .into_any_element()
        };
        let (count, cut) = self.matches;
        let summary = (self.loaded && ready).then(|| {
            let what = if self.all_versions {
                "versions"
            } else {
                "charts"
            };
            let text = if cut {
                t!(
                    "helm-charts-summary-cut",
                    what = what,
                    count = count,
                    shown = MAX_ROWS
                )
            } else {
                t!("helm-charts-summary", what = what, count = count)
            };
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(text)
        });
        let chip = self.repository.as_ref().map(|name| {
            div()
                .flex()
                .items_center()
                .gap_0p5()
                .pl_2()
                .rounded_full()
                .border_1()
                .border_color(theme.border)
                .bg(theme.list_active)
                .text_xs()
                .text_color(theme.foreground)
                .child(t!("helm-charts-repo-chip", name = name.clone()))
                .child(
                    Button::new("helm-charts-repo-clear")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip(t!("helm-repo-show-all"))
                        .on_click(cx.listener(|this, _, _, cx| this.clear_repository(cx))),
                )
        });
        let summary = (summary.is_some() || chip.is_some()).then(|| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .children(chip)
                .children(summary)
        });
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .children(summary)
            .child(body)
            .into_any_element()
    }

    fn render_browse(&self, cx: &mut Context<Self>) -> AnyElement {
        let can_install = !self.busy(cx) && self.install_target(cx).is_some();
        let toolbar = FilterBar::new()
            .search(&self.search)
            .action(
                Checkbox::new("helm-charts-all-versions")
                    .label(t!("helm-charts-all-versions"))
                    .checked(self.all_versions)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.all_versions = *checked;
                        this.update_rows(cx);
                    })),
            )
            .action(
                Button::new("helm-install-oci")
                    .outline()
                    .small()
                    .icon(IconName::Plus)
                    .label(t!("helm-install-oci"))
                    .tooltip(t!("helm-install-oci-tooltip"))
                    .disabled(!can_install)
                    .on_click(cx.listener(|this, _, window, cx| {
                        // Pre-releases of an `oci://` chart like the search.
                        let source = InstallSource::Reference {
                            devel: this.all_versions,
                        };
                        this.open_install(source, None, window, cx)
                    })),
            )
            .action(
                Button::new("helm-charts-refresh")
                    .outline()
                    .small()
                    .icon(IconName::RefreshCw)
                    .tooltip(t!("helm-charts-refresh-tooltip"))
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
            );
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(toolbar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .gap_4()
                    .child(self.render_repositories(cx))
                    .child(self.render_results(cx)),
            )
            .into_any_element()
    }

    fn render_card(&self, card: &Card, cx: &mut Context<Self>) -> AnyElement {
        let versions = repo::find_chart(&self.repos, &card.repository, &card.chart);
        let can_install = !self.busy(cx) && self.install_target(cx).is_some();
        let theme = cx.theme();
        let back = Button::new("helm-chart-back")
            .ghost()
            .small()
            .icon(IconName::ArrowLeft)
            .label(t!("helm-back-charts"))
            .on_click(cx.listener(|this, _, _, cx| this.close_card(cx)));
        let icon_url = versions
            .and_then(|versions| {
                versions
                    .iter()
                    .find(|v| v.version == card.version)
                    .or(versions.first())
            })
            .and_then(|v| v.icon.clone());
        let icon = ChartIcon::new(card.chart.clone())
            .size(px(24.))
            .path(icon_url.and_then(|url| RemoteImages::path(&url, cx)));
        let title = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_base()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.foreground)
            .child(repo::chart_reference(&card.repository, &card.chart));
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .child(back)
            .child(icon)
            .child(title);
        let Some(versions) = versions else {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_3()
                .child(header)
                .child(
                    EmptyState::new(IconName::Inbox, t!("helm-chart-not-found"))
                        .description(t!("helm-chart-not-found-description")),
                )
                .into_any_element();
        };
        let current = versions
            .iter()
            .find(|v| v.version == card.version)
            .unwrap_or(&versions[0]);
        let picker = {
            let charts = cx.entity().downgrade();
            let picked = current.version.clone();
            let items: Vec<(String, Option<String>)> = versions
                .iter()
                .map(|v| (v.version.clone(), v.app_version.clone()))
                .collect();
            Button::new("helm-chart-version")
                .outline()
                .small()
                .label(t!("helm-chart-version", version = picked.clone()))
                .dropdown_menu(move |menu: PopupMenu, _, _| {
                    let menu = menu.scrollable(true).max_h(px(360.));
                    items.iter().fold(menu, |menu, (version, app)| {
                        let (charts, version) = (charts.clone(), version.clone());
                        let label = match app {
                            Some(app) => t!(
                                "helm-chart-version-app",
                                version = version.clone(),
                                app = app.clone()
                            ),
                            None => version.clone(),
                        };
                        menu.item(
                            PopupMenuItem::new(label)
                                .checked(version == picked)
                                .on_click(move |_, _, cx| {
                                    let version = version.clone();
                                    charts
                                        .update(cx, |this, cx| {
                                            if let Some(card) = &mut this.card {
                                                card.version = version;
                                            }
                                            this.ensure_card_text(cx);
                                            cx.notify();
                                        })
                                        .ok();
                                }),
                        )
                    })
                })
        };
        let install = Button::new("helm-chart-install")
            .primary()
            .small()
            .icon(IconName::Plus)
            .label(t!("helm-install-button"))
            .disabled(!can_install)
            .on_click(cx.listener(|this, _, window, cx| this.open_card_install(window, cx)));
        let header = header.child(picker).child(install);
        let repository_url = self
            .repos
            .iter()
            .find(|r| r.repository.name == card.repository)
            .map(|r| r.repository.url.clone())
            .unwrap_or_default();
        let summary = card_summary(current, &card.repository, &repository_url);
        let selected = CardTab::ALL
            .iter()
            .position(|tab| *tab == card.tab)
            .unwrap_or_default();
        let tabs = TabBar::new("helm-chart-tabs")
            .underline()
            .small()
            .selected_index(selected)
            .children(CardTab::ALL.map(|tab| TabItem::new().label(tab.title())))
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                if let Some(card) = &mut this.card {
                    card.tab = CardTab::ALL[*ix];
                }
                this.ensure_card_text(cx);
                cx.notify();
            }))
            .when(card.tab == CardTab::Readme, |tabs| {
                tabs.suffix(
                    Button::new("helm-chart-readme-source")
                        .ghost()
                        .xsmall()
                        .icon(IconName::FileText)
                        .label(t!("helm-readme-source"))
                        .tooltip(t!("helm-readme-source-tooltip"))
                        .selected(card.readme_source)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(card) = &mut this.card {
                                card.readme_source = !card.readme_source;
                            }
                            cx.notify();
                        })),
                )
            });
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(header)
            .child(Panel::new().child(summary))
            .child(tabs)
            .child(
                div()
                    .flex_1()
                    .min_h(px(240.))
                    .flex()
                    .flex_col()
                    .child(self.render_card_text(card, cx)),
            )
            .into_any_element()
    }

    fn render_card_text(&self, card: &Card, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        if HelmScreen::helm_path(cx).is_none() {
            return EmptyState::new(IconName::Info, t!("helm-card-needs-helm"))
                .description(t!("helm-card-needs-helm-description"))
                .into_any_element();
        }
        match card.text(card.tab, &card.version) {
            None | Some(Text::Loading(_)) => {
                EmptyState::new(IconName::LoaderCircle, t!("helm-loading")).into_any_element()
            }
            Some(Text::Failed(error)) => {
                EmptyState::new(IconName::CircleX, t!("helm-card-load-error"))
                    .description(error.clone())
                    .into_any_element()
            }
            Some(Text::Ready(text)) if text.trim().is_empty() => EmptyState::new(
                IconName::FileText,
                match card.tab {
                    CardTab::Readme => t!("helm-card-no-readme"),
                    CardTab::Values => t!("helm-card-no-values"),
                },
            )
            .into_any_element(),
            Some(Text::Ready(text)) if card.tab == CardTab::Readme && !card.readme_source => {
                let id = SharedString::from(format!(
                    "helm-readme-{}/{}@{}",
                    card.repository, card.chart, card.version
                ));
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .child(readme_view(id, text, cx))
                    .into_any_element()
            }
            Some(Text::Ready(_)) => match &card.viewer {
                Some(viewer) => div()
                    .flex_1()
                    .min_h_0()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .child(Editor::new(viewer).h_full().bordered(false).readonly(true))
                    .into_any_element(),
                None => div().into_any_element(),
            },
        }
    }

    fn render_install(&self, panel: &Entity<HelmInstall>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let back = Button::new("helm-install-back")
            .ghost()
            .small()
            .icon(IconName::ArrowLeft)
            .label(t!("helm-back"))
            .on_click(cx.listener(|this, _, _, cx| this.close_install(cx)));
        let title = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_base()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.foreground)
            .child(t!("helm-install-chart-title"));
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().flex().items_center().gap_2().child(back).child(title))
            .child(panel.clone())
            .into_any_element()
    }
}

impl Render for HelmCharts {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_views(window, cx);
        let body = if let Some((panel, _)) = &self.install {
            self.render_install(panel, cx)
        } else if let Some(card) = &self.card {
            self.render_card(card, cx)
        } else {
            self.render_browse(cx)
        };
        div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.render_missing_helm(cx))
            .child(div().flex_1().min_h_0().flex().child(body))
    }
}

fn chart_columns() -> Vec<ResourceColumn> {
    vec![
        ResourceColumn::new("chart", t!("helm-field-chart"))
            .width(px(260.))
            // Search results come sorted by chart.
            .sorted(),
        // Wide enough for `1.27.3-debian-12-r12`.
        ResourceColumn::new("version", t!("helm-field-version")).width(px(200.)),
        ResourceColumn::new("app-version", t!("helm-field-app-version")).width(px(160.)),
        ResourceColumn::new("description", t!("helm-field-description")).width(px(480.)),
    ]
}

/// Row identity of a search hit: `repo/chart@version`.
fn row_key(repository: &str, chart: &str, version: &str) -> String {
    format!("{repository}/{chart}@{version}")
}

/// `(repository, chart, version)` of a [`row_key`].
fn parse_row_key(key: &str) -> Option<(String, String, String)> {
    let (reference, version) = key.rsplit_once('@')?;
    let (repository, chart) = reference.split_once('/')?;
    Some((repository.to_owned(), chart.to_owned(), version.to_owned()))
}

fn chart_row(hit: &repo::ChartHit) -> ResourceRow {
    let version = hit.version;
    let description = version.description.clone().unwrap_or_default();
    let description = if version.deprecated {
        t!("helm-chart-deprecated", description = description)
    } else {
        description
    };
    let mut chart = chart_cell(hit.reference(), version.name.clone(), version.icon.clone());
    if version.deprecated {
        chart = chart.muted();
    }
    ResourceRow::new(
        row_key(hit.repository, &version.name, &version.version),
        [
            chart,
            ResourceCell::mono(version.version.clone()),
            ResourceCell::mono(version.app_version.clone().unwrap_or_else(|| "—".into())),
            ResourceCell::text(description).muted(),
        ],
    )
}

/// A text cell with the chart's icon before it, loaded when the row shows.
pub(super) fn chart_cell(text: String, name: String, icon: Option<String>) -> ResourceCell {
    ResourceCell::text(text).leading(move |_, cx| {
        let path = icon.as_deref().and_then(|url| RemoteImages::path(url, cx));
        ChartIcon::new(name.clone()).path(path).into_any_element()
    })
}

/// The rendered README: `http(s)` links open in the browser, any other scheme
/// is ignored (the text is untrusted); images come from
/// [`RemoteImages`] and are replaced by their alt text when it is off.
fn readme_view(id: SharedString, markdown: &str, cx: &App) -> impl IntoElement {
    let markdown = if RemoteImages::enabled(cx) {
        markdown.to_owned()
    } else {
        kubach_core::images::markdown_without_images(markdown)
    };
    gpui_kit::base::TextView::markdown(id, markdown)
        .image_source(RemoteImages::resolver(cx))
        .on_link_click(|url, _, _, cx| {
            if kubach_core::links::is_safe_external_url(url) {
                cx.open_url(url);
            }
        })
        .selectable(true)
        .scrollable(true)
        .flex_1()
        .min_h_0()
        .p_4()
}

fn card_summary(version: &ChartVersion, repository: &str, url: &str) -> KeyValueList {
    let or_dash = |text: Option<&str>| text.unwrap_or("—").to_owned();
    let mut summary = KeyValueList::new()
        .row(
            t!("helm-field-description"),
            div()
                .whitespace_normal()
                .child(or_dash(version.description.as_deref())),
        )
        .row(t!("helm-field-version"), version.version.clone())
        .row(
            t!("helm-field-app-version"),
            or_dash(version.app_version.as_deref()),
        )
        .row(t!("helm-field-repository"), format!("{repository} · {url}"));
    if version.deprecated {
        summary = summary.row(
            t!("helm-field-status"),
            StatusBadge::new(t!("helm-chart-status-deprecated"), Tone::Warning),
        );
    }
    if let Some(home) = &version.home {
        summary = summary.row(t!("helm-field-home"), home.clone());
    }
    if !version.sources.is_empty() {
        summary = summary.row(
            t!("helm-field-sources"),
            div().whitespace_normal().child(version.sources.join(", ")),
        );
    }
    if !version.keywords.is_empty() {
        summary = summary.row(
            t!("helm-field-keywords"),
            div().whitespace_normal().child(version.keywords.join(", ")),
        );
    }
    if !version.maintainers.is_empty() {
        let names: Vec<&str> = version
            .maintainers
            .iter()
            .map(|m| m.name.as_str())
            .filter(|name| !name.is_empty())
            .collect();
        summary = summary.row(
            t!("helm-field-maintainers"),
            div().whitespace_normal().child(names.join(", ")),
        );
    }
    if let Some(kube) = &version.kube_version {
        summary = summary.row("Kubernetes", kube.clone());
    }
    if let Some(created) = &version.created {
        summary = summary.row(t!("helm-field-created"), created.clone());
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::{parse_row_key, row_key};

    #[test]
    fn row_keys_round_trip() {
        let key = row_key("external-dns ", "external-dns", "1.15.0");
        assert_eq!(
            parse_row_key(&key),
            Some((
                "external-dns ".to_owned(),
                "external-dns".to_owned(),
                "1.15.0".to_owned()
            ))
        );
        assert_eq!(parse_row_key("no-version"), None);
    }
}
