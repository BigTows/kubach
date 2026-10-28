//! Sort of resource tables, remembered per table in the application state
//! (`state.json`, [`kubach_core::state::AppState::table_sorts`], #33): per
//! resource kind in the browser, per screen elsewhere. Shared by all
//! contexts; saved with the state's debounce.

use gpui_kit::*;
use kubach_core::state::{self, SortOrder, TableSort};
use kubach_ui::{ColumnSort, ResourceTable, SortDirection, TableSortEvent};

use crate::state::StateStore;

/// Sorts `table` the way the table `key` was last sorted and remembers the
/// sorts the user picks by a header click. `key` is a [`state::kind_key`]
/// or a [`screen`] key. A remembered column the table does not have (another
/// version, hidden now) leaves the default order and stays remembered.
pub fn remember<T: 'static>(table: &Entity<ResourceTable>, key: String, cx: &mut Context<T>) {
    let saved =
        StateStore::try_global(cx).and_then(|store| store.read(cx).get().table_sort(&key).cloned());
    if let Some(saved) = saved {
        table.update(cx, |table, cx| {
            table.set_sort(Some((&saved.column, direction(saved.order))), cx)
        });
    }
    cx.subscribe(table, move |_, _, event: &TableSortEvent, cx| {
        let Some(store) = StateStore::try_global(cx) else {
            return;
        };
        let TableSortEvent::Changed(sort) = event;
        let sort = sort.as_ref().map(stored);
        store.update(cx, |store, cx| {
            store.edit(cx, |state| state.remember_table_sort(&key, sort))
        });
    })
    .detach();
}

/// Key of the table of a screen: `screen:gitops`.
pub fn screen(id: &str) -> String {
    state::screen_table_key(id)
}

fn direction(order: SortOrder) -> SortDirection {
    match order {
        SortOrder::Ascending => SortDirection::Ascending,
        SortOrder::Descending => SortDirection::Descending,
    }
}

fn stored(sort: &ColumnSort) -> TableSort {
    let order = match sort.direction {
        SortDirection::Ascending => SortOrder::Ascending,
        SortDirection::Descending => SortOrder::Descending,
    };
    TableSort::new(sort.column.as_ref(), order)
}
