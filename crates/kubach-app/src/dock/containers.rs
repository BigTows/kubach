//! The container menu shared by the details panel (split buttons of Logs /
//! Shell / Debug) and the container pickers in logs and terminal tabs.

use std::rc::Rc;

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::*;
use kubach_i18n::t;

use super::target::{ContainerChoice, ContainerMenu};

/// Called with the chosen container; `None` is "All containers".
pub type OnPick = Rc<dyn Fn(Option<String>, &mut Window, &mut App)>;

/// Fills `popup` with `menu`: "All containers" when offered, the app
/// containers, then the init containers in their own group. `current`
/// checks the active choice; `None` checks nothing (nothing is open yet).
pub fn fill_menu(
    mut popup: PopupMenu,
    menu: &ContainerMenu,
    current: Option<Option<&str>>,
    on_pick: OnPick,
) -> PopupMenu {
    let item = |label: String, value: Option<String>| {
        let on_pick = on_pick.clone();
        let checked = current == Some(value.as_deref());
        PopupMenuItem::new(label)
            .checked(checked)
            .on_click(move |_, window, cx| on_pick(value.clone(), window, cx))
    };
    let choice = |choice: &ContainerChoice| item(choice.label(), Some(choice.name.clone()));
    if menu.all {
        popup = popup
            .item(item(t!("dock-all-containers"), None))
            .separator();
    }
    let grouped = !menu.init_containers.is_empty();
    if grouped {
        popup = popup.label(t!("dock-containers"));
    }
    for container in &menu.containers {
        popup = popup.item(choice(container));
    }
    if grouped {
        popup = popup.separator().label(t!("dock-init-containers"));
        for container in &menu.init_containers {
            popup = popup.item(choice(container));
        }
    }
    popup
}
