//! The application state remembered between runs (`state.json`, see
//! [`kubach_core::state`]): loaded once at startup, saved shortly after a
//! change and on quit.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::*;
use kubach_core::state::{self, AppState};

/// Changes made within this delay are saved together.
const SAVE_DELAY: Duration = Duration::from_millis(500);

pub struct StateStore {
    /// `None` when the OS has no config directory: state stays in memory.
    path: Option<PathBuf>,
    state: AppState,
    /// Changed since the last save.
    dirty: bool,
    save: Option<Task<()>>,
}

struct GlobalState(Entity<StateStore>);

impl Global for GlobalState {}

impl StateStore {
    pub fn try_global(cx: &App) -> Option<Entity<StateStore>> {
        cx.try_global::<GlobalState>()
            .map(|global| global.0.clone())
    }

    fn new(path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let state = path.as_deref().map(state::load).unwrap_or_default();
        cx.on_app_quit(|this, _| {
            this.flush();
            async {}
        })
        .detach();
        Self {
            path,
            state,
            dirty: false,
            save: None,
        }
    }

    pub fn get(&self) -> &AppState {
        &self.state
    }

    /// Changes the state; `edit` returns whether it changed anything, and
    /// only then a save is scheduled.
    pub fn edit(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut AppState) -> bool) {
        if !edit(&mut self.state) {
            return;
        }
        self.dirty = true;
        if self.save.is_none() {
            self.save = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(SAVE_DELAY).await;
                this.update(cx, |this, _| {
                    this.save = None;
                    this.flush();
                })
                .ok();
            }));
        }
    }

    /// Writes pending changes now.
    fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        if let Some(path) = &self.path {
            // The state is disposable: a failed save only loses what it
            // remembers, so it is not reported.
            state::save(path, &self.state).ok();
        }
    }
}

/// Loads the state and makes it global.
pub fn init(cx: &mut App) {
    let entity = cx.new(|cx| StateStore::new(state::default_path(), cx));
    cx.set_global(GlobalState(entity));
}
