//! Column order of resource tables, remembered per kind in the application
//! state (`state.json`, [`kubach_core::state::AppState::column_orders`]).

use gpui_kit::*;
use kubach_core::state;
use kubach_ui::{ColumnOrderEvent, ResourceTable};

use crate::state::StateStore;

/// Shows the columns of a [`movable`](ResourceTable::movable_columns) `table`
/// in the remembered order of `kind` ([`state::kind_key`]) and remembers the order the user drags
/// them into. `default` holds the keys of every column the kind can show
/// (hidden ones too), in plugin order.
pub fn remember<T: 'static>(
    table: &Entity<ResourceTable>,
    kind: String,
    default: Vec<SharedString>,
    cx: &mut Context<T>,
) {
    let saved = StateStore::try_global(cx)
        .and_then(|store| store.read(cx).get().column_order(&kind).map(<[_]>::to_vec));
    let order = state::column_order(&default, saved.as_deref());
    table.update(cx, |table, cx| table.set_column_order(&order, cx));
    cx.subscribe(table, move |_, _, event: &ColumnOrderEvent, cx| {
        let Some(store) = StateStore::try_global(cx) else {
            return;
        };
        store.update(cx, |store, cx| {
            store.edit(cx, |state| match event {
                ColumnOrderEvent::Moved(shown) => {
                    state.remember_column_order(&kind, shown, &default)
                }
                ColumnOrderEvent::Reset => state.forget_column_order(&kind),
            })
        });
    })
    .detach();
}
