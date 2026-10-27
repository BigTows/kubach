//! Application settings in the UI: the shared model (loaded from and saved
//! to the settings file, reloaded on external edits), live application of
//! the theme, and the settings screen of the main window.

mod screen;

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::component::ThemeMode;
use gpui_kit::*;
use kubach_core::settings::{self, Settings, SettingsWatcher, ThemePreference};

use crate::cluster::Cluster;

pub use screen::{
    Anchor as SettingsAnchor, CloseRequested, Section as SettingsSection, SettingsScreen,
};

actions!(
    kubach,
    [
        /// Opens the settings screen (`cmd-,` / `ctrl-,`).
        OpenSettings,
        /// Leaves the settings screen (`escape`).
        CloseSettings
    ]
);

/// Where to open the settings screen. `None` fields keep what it shows now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsTarget {
    pub section: Option<SettingsSection>,
    /// Context to select on the per-cluster page.
    pub context: Option<String>,
    /// Block of that context to scroll to.
    pub anchor: Option<SettingsAnchor>,
}

impl SettingsTarget {
    pub fn section(section: SettingsSection) -> Self {
        Self {
            section: Some(section),
            ..Self::default()
        }
    }

    /// Per-cluster page with `context` picked, at its `anchor`.
    fn cluster(context: impl Into<String>, anchor: SettingsAnchor) -> Self {
        Self {
            section: Some(SettingsSection::Cluster),
            context: Some(context.into()),
            anchor: Some(anchor),
        }
    }

    /// Security settings of `context` on the per-cluster page.
    pub fn security(context: impl Into<String>) -> Self {
        Self::cluster(context, SettingsAnchor::Security)
    }

    /// Per-cluster page, where the Prometheus of `context` is chosen.
    pub fn prometheus(context: impl Into<String>) -> Self {
        Self::cluster(context, SettingsAnchor::Prometheus)
    }

    /// Tile color of `context` on the per-cluster page.
    pub fn tile_color(context: impl Into<String>) -> Self {
        Self::cluster(context, SettingsAnchor::TileColor)
    }
}

/// Emitted by [`AppSettings`] when something asks for the settings screen;
/// the main window shows it.
pub struct OpenRequested(pub SettingsTarget);

/// Opens the settings screen in the main window at `target`. For example
/// `open_settings(SettingsTarget::security("minikube"), cx)`.
pub fn open_settings(target: SettingsTarget, cx: &mut App) {
    if let Some(settings) = AppSettings::try_global(cx) {
        settings.update(cx, |_, cx| cx.emit(OpenRequested(target)));
    }
}

/// External edits often come as several writes; reload once they settle.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(250);

/// The settings everyone reads. Observe it to react to changes.
pub struct AppSettings {
    /// `None` when the OS has no config directory: settings stay in memory.
    path: Option<PathBuf>,
    settings: Settings,
    /// Load and save errors to show in the settings window.
    problems: Vec<SharedString>,
    /// Saving is disabled (file from a newer Kubach or unreadable).
    read_only: bool,
    _watcher: Option<SettingsWatcher>,
    _listener: Option<Task<()>>,
}

impl EventEmitter<OpenRequested> for AppSettings {}

struct GlobalSettings(Entity<AppSettings>);

impl Global for GlobalSettings {}

impl AppSettings {
    /// The shared settings; [`init`] must have run.
    pub fn global(cx: &App) -> Entity<AppSettings> {
        cx.global::<GlobalSettings>().0.clone()
    }

    pub fn try_global(cx: &App) -> Option<Entity<AppSettings>> {
        cx.try_global::<GlobalSettings>()
            .map(|global| global.0.clone())
    }

    fn new(path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            path,
            settings: Settings::default(),
            problems: Vec::new(),
            read_only: false,
            _watcher: None,
            _listener: None,
        };
        this.reload(cx);
        this.watch(cx);
        this
    }

    pub fn get(&self) -> &Settings {
        &self.settings
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether changes reach the file (not read-only, a config directory).
    pub fn can_save(&self) -> bool {
        self.path.is_some() && !self.read_only
    }

    pub fn problems(&self) -> &[SharedString] {
        &self.problems
    }

    /// Changes settings, saves them and notifies observers. Nothing happens
    /// when `edit` leaves them unchanged.
    pub fn edit(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Settings)) {
        let mut next = self.settings.clone();
        edit(&mut next);
        if next == self.settings {
            return;
        }
        self.settings = next;
        self.save();
        cx.notify();
    }

    fn save(&mut self) {
        let Some(path) = &self.path else { return };
        if self.read_only {
            return;
        }
        let error = format!("cannot save {}", path.display());
        self.problems.retain(|problem| !problem.starts_with(&error));
        if let Err(io) = settings::save(path, &self.settings) {
            self.problems.push(format!("{error}: {io}").into());
        }
    }

    /// Reads the file again; observers are notified only on a change.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(path) = &self.path else { return };
        let loaded = settings::load(path);
        let mut problems: Vec<SharedString> = loaded
            .problems
            .into_iter()
            .map(SharedString::from)
            .collect();
        if let Some(backup) = loaded.backup {
            problems.push(format!("the original file was copied to {}", backup.display()).into());
        }
        let changed = loaded.settings != self.settings || problems != self.problems;
        self.settings = loaded.settings;
        self.problems = problems;
        self.read_only = loaded.read_only;
        if changed {
            cx.notify();
        }
    }

    fn watch(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let (tx, mut rx) = mpsc::unbounded();
        match settings::watch(&path, WATCH_DEBOUNCE, move || {
            tx.unbounded_send(()).ok();
        }) {
            Ok(watcher) => self._watcher = Some(watcher),
            Err(error) => {
                eprintln!("warning: settings file is not watched: {error}");
                return;
            }
        }
        self._listener = Some(cx.spawn(async move |this, cx| {
            while rx.next().await.is_some() {
                // Our own saves come back here too; `reload` ignores them
                // because the content is unchanged.
                if this.update(cx, |this, cx| this.reload(cx)).is_err() {
                    break;
                }
            }
        }));
    }
}

/// Loads the settings, makes them global and applies the theme now and on
/// every change. Call after `kubach_ui::theme::init`.
pub fn init(cx: &mut App) {
    let entity = cx.new(|cx| AppSettings::new(settings::default_path(), cx));
    cx.set_global(GlobalSettings(entity.clone()));

    apply_theme(entity.read(cx).get().general.theme, cx);
    cx.observe(&entity, |entity, cx| {
        apply_theme(entity.read(cx).get().general.theme, cx);
    })
    .detach();

    cx.on_action(|_: &OpenSettings, cx| open_settings(SettingsTarget::default(), cx));
    cx.bind_keys([
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
        KeyBinding::new("escape", CloseSettings, Some(screen::KEY_CONTEXT)),
    ]);
}

pub fn theme_mode(preference: ThemePreference, cx: &App) -> ThemeMode {
    match preference {
        ThemePreference::Dark => ThemeMode::Dark,
        ThemePreference::Light => ThemeMode::Light,
        ThemePreference::System => kubach_ui::theme::system_mode(cx),
    }
}

fn apply_theme(preference: ThemePreference, cx: &mut App) {
    let mode = theme_mode(preference, cx);
    kubach_ui::theme::set_mode(mode, cx);
}

/// Hosts the settings screen in a view (the main window's workspace):
/// creates it on [`open_settings`] and drops it when the user closes it.
pub struct SettingsHost<V> {
    screen: Option<Entity<SettingsScreen>>,
    request: Option<SettingsTarget>,
    /// Contexts the screen shows.
    cluster: Entity<Cluster>,
    field: fn(&mut V) -> &mut SettingsHost<V>,
    _subscriptions: Vec<Subscription>,
}

impl<V: 'static> SettingsHost<V> {
    /// `field` returns this host inside the view; the screen shows the
    /// contexts of `cluster`.
    pub fn new(
        field: fn(&mut V) -> &mut SettingsHost<V>,
        cluster: Entity<Cluster>,
        cx: &mut Context<V>,
    ) -> Self {
        let subscriptions = AppSettings::try_global(cx)
            .map(|settings| {
                cx.subscribe(&settings, move |view, _, OpenRequested(target), cx| {
                    field(view).request = Some(target.clone());
                    cx.notify();
                })
            })
            .into_iter()
            .collect();
        Self {
            screen: None,
            request: None,
            cluster,
            field,
            _subscriptions: subscriptions,
        }
    }

    /// The screen to show in place of the content, if open. Call from
    /// `render`: a pending request is applied here, where a window exists.
    pub fn screen(&mut self, window: &mut Window, cx: &mut Context<V>) -> Option<AnyView> {
        if let Some(target) = self.request.take() {
            match &self.screen {
                Some(screen) => screen.update(cx, |screen, cx| screen.show(target, window, cx)),
                None => {
                    let cluster = self.cluster.clone();
                    let screen = cx.new(|cx| SettingsScreen::new(target, cluster, window, cx));
                    let field = self.field;
                    self._subscriptions.push(cx.subscribe(
                        &screen,
                        move |view, _, _: &CloseRequested, cx| {
                            field(view).screen = None;
                            cx.notify();
                        },
                    ));
                    self.screen = Some(screen);
                }
            }
        }
        self.screen.clone().map(Into::into)
    }
}

/// Follows OS light/dark switches in `window` while the theme is "system".
pub fn follow_system_appearance(window: &Window) -> Subscription {
    window.observe_window_appearance(|window, cx| {
        let Some(settings) = AppSettings::try_global(cx) else {
            return;
        };
        if settings.read(cx).get().general.theme == ThemePreference::System {
            kubach_ui::theme::set_mode(window.appearance().into(), cx);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{SettingsAnchor, SettingsSection, SettingsTarget};

    #[test]
    fn context_targets_open_the_clusters_page_at_their_block() {
        for (target, anchor) in [
            (SettingsTarget::security("kind"), SettingsAnchor::Security),
            (
                SettingsTarget::prometheus("kind"),
                SettingsAnchor::Prometheus,
            ),
            (
                SettingsTarget::tile_color("kind"),
                SettingsAnchor::TileColor,
            ),
        ] {
            assert_eq!(target.section, Some(SettingsSection::Cluster));
            assert_eq!(target.context.as_deref(), Some("kind"));
            assert_eq!(target.anchor, Some(anchor));
        }
        let tools = SettingsTarget::section(SettingsSection::Tools);
        assert_eq!(tools.context, None);
        assert_eq!(tools.anchor, None);
    }
}
