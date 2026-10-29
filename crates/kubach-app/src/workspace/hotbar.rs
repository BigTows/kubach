//! Hotbar of clusters: tiles, tooltips, tile menus and the hotbar arrangement.

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{IconName, WindowExt as _};
use gpui_kit::*;
use kubach_core::kubeconfig::{FileProblem, FileState, Source};
use kubach_core::settings::HotbarPrefs;
use kubach_core::{ConnectionState, ContextInfo};
use kubach_i18n::t;
use kubach_ui::tokens::TITLEBAR_HEIGHT;
use kubach_ui::{Hotbar, HotbarTile, TileColor, Tone};

use crate::hotbar::{
    ALIAS_MAX, file_tile_id, file_tile_label, file_tile_path, initials, is_local, problem_rows,
    source_row, tile_color, tile_label,
};
use crate::kubeconfig;
use crate::settings::{AppSettings, SettingsSection, SettingsTarget, open_settings};
use crate::vault;

use super::Workspace;
use super::status::state_tone;

/// Hotbar arrangement from settings (defaults without them).
pub(super) fn hotbar_prefs(cx: &App) -> HotbarPrefs {
    crate::hotbar::prefs(cx)
}

/// Action of a hotbar tile menu item on the tile's context.
pub(super) type TileAction = fn(&mut Workspace, String, &mut Window, &mut Context<Workspace>);

pub(super) fn state_label(state: &ConnectionState) -> String {
    match state {
        ConnectionState::Disconnected => t!("hotbar-state-disconnected"),
        ConnectionState::Connecting => t!("hotbar-state-connecting"),
        ConnectionState::Connected { server_version } => {
            t!("hotbar-state-connected", version = server_version)
        }
        ConnectionState::Error(error) => t!("hotbar-state-error", error = error),
    }
}

/// Tooltip rows of the hotbar tile at `ix` for `context`.
pub(super) fn tooltip_rows(
    context: &ContextInfo,
    source: Option<&Source>,
    state: &ConnectionState,
    ix: usize,
) -> Vec<(SharedString, SharedString)> {
    let or_dash = |value: &Option<String>| value.clone().unwrap_or_else(|| "—".into());
    let mut rows: Vec<(SharedString, SharedString)> = vec![
        (
            t!("hotbar-row-cluster").into(),
            context.cluster.clone().into(),
        ),
        (
            t!("hotbar-row-server").into(),
            or_dash(&context.server).into(),
        ),
        (t!("hotbar-row-user").into(), or_dash(&context.user).into()),
        (
            t!("hotbar-row-namespace").into(),
            context
                .namespace
                .clone()
                .unwrap_or_else(|| "default".into())
                .into(),
        ),
        (t!("hotbar-row-status").into(), state_label(state).into()),
    ];
    rows.extend(source.map(source_row));
    if ix < 9 {
        let key = if cfg!(target_os = "macos") {
            "⌘"
        } else {
            "Ctrl+"
        };
        rows.push((
            t!("hotbar-row-shortcut").into(),
            format!("{key}{}", ix + 1).into(),
        ));
    }
    rows
}

impl Workspace {
    /// Names of all kubeconfig contexts, in kubeconfig order.
    pub(super) fn context_names(&self, cx: &App) -> Vec<String> {
        self.cluster
            .read(cx)
            .contexts()
            .map(|contexts| contexts.list().iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default()
    }

    /// Switches to the `ix`-th visible hotbar tile.
    pub(super) fn select_nth(&mut self, ix: usize, cx: &mut Context<Self>) {
        let names = self.context_names(cx);
        if let Some(name) = hotbar_prefs(cx).visible(&names).into_iter().nth(ix) {
            self.switch_to(name, cx);
        }
    }

    pub(super) fn switch_to(&mut self, name: String, cx: &mut Context<Self>) {
        self.cluster
            .update(cx, |cluster, cx| cluster.connect(name, cx));
    }

    /// Changes the hotbar arrangement in settings (saved to the file).
    pub(super) fn update_prefs(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut HotbarPrefs),
    ) {
        if let Some(settings) = AppSettings::try_global(cx) {
            settings.update(cx, |settings, cx| {
                settings.edit(cx, |settings| f(&mut settings.clusters.hotbar))
            });
        }
    }

    /// Asks for the alias of `name` in a dialog.
    pub(super) fn rename(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let current = hotbar_prefs(cx).alias(&name).unwrap_or_default().to_owned();
        let placeholder = initials(&name);
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(current)
        });
        let this = cx.entity().downgrade();
        let dialog_input = input.clone();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let (input, this, name) = (dialog_input.clone(), this.clone(), name.clone());
            dialog
                .confirm()
                .title(t!("hotbar-rename-title", context = name.as_str()))
                .description(t!("hotbar-rename-description", max = ALIAS_MAX))
                .child(Input::new(&input))
                .on_ok(move |_, _, cx| {
                    let alias = input.read(cx).value();
                    this.update(cx, |this, cx| {
                        this.update_prefs(cx, |prefs| prefs.set_alias(&name, &alias));
                    })
                    .ok();
                    true
                })
        });
        input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Right-click menu of the tile of `name`.
    pub(super) fn tile_menu(
        this: &WeakEntity<Self>,
        name: &str,
        menu: PopupMenu,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let Some(workspace) = this.upgrade() else {
            return menu;
        };
        let workspace = workspace.read(cx);
        if let Some(file) = workspace.unreadable_file(name, cx) {
            if file.problem == Some(FileProblem::Locked) {
                return Self::locked_menu(this, name, menu);
            }
            return Self::unreadable_menu(this, name, file, menu);
        }
        // The vault entry an imported context comes from.
        let imported = workspace
            .cluster
            .read(cx)
            .contexts()
            .ok()
            .and_then(|contexts| contexts.source(name))
            .and_then(|source| match source {
                Source::Imported { id, .. } => Some(id.clone()),
                _ => None,
            });
        let connected = !matches!(
            workspace.cluster.read(cx).state_of(name),
            ConnectionState::Disconnected
        );
        let item = |label: String, action: TileAction| {
            let (this, name) = (this.clone(), name.to_owned());
            PopupMenuItem::new(label).on_click(move |_, window, cx| {
                this.update(cx, |workspace, cx| {
                    action(workspace, name.clone(), window, cx)
                })
                .ok();
            })
        };

        let menu = if connected {
            menu.item(item(t!("hotbar-menu-disconnect"), |this, name, _, cx| {
                this.cluster
                    .update(cx, |cluster, cx| cluster.disconnect(&name, cx));
            }))
        } else {
            menu.item(item(t!("hotbar-menu-connect"), |this, name, _, cx| {
                this.cluster
                    .update(cx, |cluster, cx| cluster.connect_in_background(name, cx));
            }))
        };
        // Colors are picked in settings: a submenu here closes when the
        // pointer crosses a neighbouring item on its way (#173).
        let menu = menu
            .item(item(t!("hotbar-menu-rename"), |this, name, window, cx| {
                this.rename(name, window, cx)
            }))
            .item(item(t!("hotbar-menu-set-color"), |_, name, _, cx| {
                open_settings(SettingsTarget::tile_color(name), cx)
            }))
            .item(item(t!("hotbar-menu-hide"), |this, name, _, cx| {
                this.update_prefs(cx, |prefs| prefs.set_hidden(&name, true));
            }))
            .separator()
            .item(item(t!("hotbar-menu-security"), |_, name, _, cx| {
                open_settings(SettingsTarget::security(name), cx)
            }));
        if let Some(id) = imported {
            menu.item(
                PopupMenuItem::new(t!("hotbar-menu-lock-imported"))
                    .on_click(|_, _, cx| vault::lock(cx)),
            )
            .item(
                PopupMenuItem::new(t!("hotbar-menu-remove"))
                    .on_click(move |_, window, cx| vault::confirm_remove(id.clone(), window, cx)),
            )
        } else {
            menu
        }
    }

    /// Right-click menu of a context whose vault is locked.
    pub(super) fn locked_menu(this: &WeakEntity<Self>, name: &str, menu: PopupMenu) -> PopupMenu {
        let (unlock, hide) = (
            (this.clone(), name.to_owned()),
            (this.clone(), name.to_owned()),
        );
        menu.item(
            PopupMenuItem::new(t!("hotbar-menu-unlock")).on_click(move |_, window, cx| {
                Self::unlock_and_switch(unlock.0.clone(), unlock.1.clone(), window, cx)
            }),
        )
        .item(
            PopupMenuItem::new(t!("hotbar-menu-kubeconfig-settings")).on_click(|_, _, cx| {
                open_settings(SettingsTarget::section(SettingsSection::Clusters), cx)
            }),
        )
        .separator()
        .item(
            PopupMenuItem::new(t!("hotbar-menu-hide")).on_click(move |_, _, cx| {
                let name = hide.1.clone();
                hide.0
                    .update(cx, |this, cx| {
                        this.update_prefs(cx, |prefs| prefs.set_hidden(&name, true));
                    })
                    .ok();
            }),
        )
    }

    /// Asks to unlock the vault, then switches to `name`.
    pub(super) fn unlock_and_switch(
        this: WeakEntity<Self>,
        name: String,
        window: &mut Window,
        cx: &mut App,
    ) {
        let then: vault::AfterUnlock = Rc::new(move |_, cx| {
            let name = name.clone();
            this.update(cx, |this, cx| this.switch_to(name, cx)).ok();
        });
        vault::prompt_unlock(Some(then), window, cx);
    }

    /// The unreadable kubeconfig file behind the tile `id`, if any.
    pub(super) fn unreadable_file(&self, id: &str, cx: &App) -> Option<FileState> {
        let cluster = self.cluster.read(cx);
        match file_tile_path(id) {
            Some(path) => cluster
                .unavailable_files()
                .into_iter()
                .find(|file| file.path() == path),
            None => cluster
                .unavailable()
                .into_iter()
                .find(|unavailable| unavailable.context.name == id)
                .map(|unavailable| unavailable.file),
        }
    }

    /// Right-click menu of a tile whose kubeconfig file can't be read.
    pub(super) fn unreadable_menu(
        this: &WeakEntity<Self>,
        id: &str,
        file: FileState,
        mut menu: PopupMenu,
    ) -> PopupMenu {
        if file.source.is_linked() {
            let (find, remove) = (file.path().to_owned(), file.path().to_owned());
            menu = menu
                .item(PopupMenuItem::new(t!("hotbar-menu-find-again")).on_click(
                    move |_, window, cx| kubeconfig::find_again(find.clone(), window, cx),
                ))
                .item(
                    PopupMenuItem::new(t!("hotbar-menu-remove-file"))
                        .on_click(move |_, _, cx| kubeconfig::unlink(&remove, cx)),
                );
        }
        menu = menu.item(
            PopupMenuItem::new(t!("hotbar-menu-kubeconfig-settings")).on_click(|_, _, cx| {
                open_settings(SettingsTarget::section(SettingsSection::Clusters), cx)
            }),
        );
        if file_tile_path(id).is_none() {
            let (this, name) = (this.clone(), id.to_owned());
            menu = menu
                .separator()
                .item(
                    PopupMenuItem::new(t!("hotbar-menu-hide")).on_click(move |_, _, cx| {
                        this.update(cx, |this, cx| {
                            this.update_prefs(cx, |prefs| prefs.set_hidden(&name, true));
                        })
                        .ok();
                    }),
                );
        }
        menu
    }

    pub(super) fn render_hotbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let cluster = self.cluster.read(cx);
        let active = cluster.active().map(|context| context.name.clone());
        let contexts: Vec<ContextInfo> = cluster
            .contexts()
            .map(|contexts| contexts.list().to_vec())
            .unwrap_or_default();
        let sources = cluster.contexts().ok();
        // Contexts of unreadable files keep their place, shown as unavailable.
        let unavailable = cluster.unavailable();
        let names: Vec<String> = contexts
            .iter()
            .map(|c| c.name.clone())
            .chain(unavailable.iter().map(|u| u.context.name.clone()))
            .collect();
        let settings = AppSettings::try_global(cx).map(|settings| settings.read(cx).get().clone());
        let prefs = settings
            .as_ref()
            .map(|settings| settings.clusters.hotbar.clone())
            .unwrap_or_default();
        let tiles: Vec<HotbarTile> = prefs
            .visible(&names)
            .into_iter()
            .enumerate()
            .filter_map(|(ix, name)| {
                let Some(context) = contexts.iter().find(|c| c.name == name) else {
                    let missing = unavailable.iter().find(|u| u.context.name == name)?;
                    if missing.file.problem == Some(FileProblem::Locked) {
                        return Some(
                            HotbarTile::new(name.clone(), tile_label(&prefs, &name))
                                .color(tile_color(&prefs, &name))
                                .icon(Some(Lucide::Lock))
                                .tooltip(
                                    name.clone(),
                                    vec![
                                        source_row(&missing.file.source),
                                        (
                                            t!("hotbar-row-vault").into(),
                                            t!("hotbar-vault-locked").into(),
                                        ),
                                    ],
                                ),
                        );
                    }
                    return Some(
                        HotbarTile::new(name.clone(), tile_label(&prefs, &name))
                            .color(tile_color(&prefs, &name))
                            .status(Tone::Danger)
                            .unavailable(true)
                            .tooltip(name.clone(), problem_rows(&missing.file)),
                    );
                };
                let source = sources.and_then(|sources| sources.source(&name));
                let state = cluster.state_of(&name);
                let rows = tooltip_rows(context, source, state, ix);
                Some(
                    HotbarTile::new(name.clone(), tile_label(&prefs, &name))
                        .color(tile_color(&prefs, &name))
                        .status(state_tone(state))
                        .active(active.as_ref() == Some(&name))
                        .icon(
                            is_local(context)
                                .then_some(Lucide::from(IconName::HardDrive))
                                .or(source
                                    .is_some_and(Source::is_imported)
                                    .then_some(Lucide::Lock)),
                        )
                        .tooltip(name.clone(), rows),
                )
            })
            .chain(cluster.unavailable_files().into_iter().map(|file| {
                let title = file
                    .path()
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                HotbarTile::new(file_tile_id(file.path()), file_tile_label(file.path()))
                    .color(TileColor::Gray)
                    .status(Tone::Danger)
                    .unavailable(true)
                    .tooltip(title, problem_rows(&file))
            }))
            .collect();
        let unreadable: Vec<SharedString> = tiles
            .iter()
            .filter(|tile| tile.unavailable)
            .map(|tile| tile.id.clone())
            .collect();
        let locked: Vec<SharedString> = unavailable
            .iter()
            .filter(|u| u.file.problem == Some(FileProblem::Locked))
            .map(|u| u.context.name.clone().into())
            .collect();
        let hidden = prefs.hidden_of(&names);

        let this = cx.entity().downgrade();
        let (on_select, on_move, menu, more) = (this.clone(), this.clone(), this.clone(), this);
        let mut hotbar = Hotbar::new("hotbar", tiles)
            .top_inset(TITLEBAR_HEIGHT)
            .on_select(move |name, window, cx| {
                // The vault is locked: unlock, then connect.
                if locked.contains(name) {
                    Self::unlock_and_switch(on_select.clone(), name.to_string(), window, cx);
                    return;
                }
                // Nothing to connect to: show where to fix the file.
                if unreadable.contains(name) {
                    open_settings(SettingsTarget::section(SettingsSection::Clusters), cx);
                    return;
                }
                on_select
                    .update(cx, |this, cx| this.switch_to(name.to_string(), cx))
                    .ok();
            })
            .on_move(move |dragged, target, _, cx| {
                on_move
                    .update(cx, |this, cx| {
                        this.update_prefs(cx, |prefs| prefs.move_to(&names, dragged, target));
                    })
                    .ok();
            })
            .tile_menu(move |name, popup, _, cx| Self::tile_menu(&menu, name, popup, cx))
            .on_add(vault::add_cluster);
        if !hidden.is_empty() {
            hotbar = hotbar.more_menu(move |popup, _, _| {
                hidden
                    .iter()
                    .fold(popup.label(t!("hotbar-hidden")), |popup, name| {
                        let (this, name) = (more.clone(), name.clone());
                        popup.item(PopupMenuItem::new(name.clone()).on_click(move |_, _, cx| {
                            this.update(cx, |this, cx| {
                                this.update_prefs(cx, |prefs| prefs.set_hidden(&name, false));
                                this.switch_to(name.clone(), cx);
                            })
                            .ok();
                        }))
                    })
            });
        }
        hotbar
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::SharedString;
    use kubach_core::kubeconfig::Source;
    use kubach_core::{ConnectionState, ContextInfo};

    use super::tooltip_rows;

    #[test]
    fn tooltip_lists_context_details_and_shortcut() {
        let context = ContextInfo {
            name: "prod".into(),
            cluster: "prod-cluster".into(),
            user: Some("alice".into()),
            namespace: None,
            server: Some("https://prod.example.com".into()),
        };
        let value = |rows: &[(SharedString, SharedString)], key: &str| {
            rows.iter()
                .find(|(k, _)| k.as_ref() == key)
                .map(|(_, v)| v.to_string())
        };
        let source = Source::Linked("/x/eks.yaml".into());
        let rows = tooltip_rows(&context, Some(&source), &ConnectionState::Connecting, 0);
        assert_eq!(value(&rows, "Cluster").as_deref(), Some("prod-cluster"));
        assert_eq!(
            value(&rows, "Server").as_deref(),
            Some("https://prod.example.com")
        );
        assert_eq!(value(&rows, "User").as_deref(), Some("alice"));
        assert_eq!(value(&rows, "Namespace").as_deref(), Some("default"));
        assert!(value(&rows, "Shortcut").is_some_and(|key| key.ends_with('1')));
        assert_eq!(
            value(&rows, "Source").as_deref(),
            Some("Linked file: /x/eks.yaml")
        );

        // Only the first nine tiles have a shortcut.
        let rows = tooltip_rows(&context, None, &ConnectionState::Disconnected, 9);
        assert_eq!(value(&rows, "Shortcut"), None);
        assert_eq!(value(&rows, "Source"), None);
    }
}
