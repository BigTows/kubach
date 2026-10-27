//! What screens share about watching: following cache changes, and running
//! watches and polls only while on screen.

use std::future::Future;

use gpui_kit::{App, Context, Entity, Task};
use kubach_core::cache::{Changes, MergedChanges, SubscriptionSet};

/// A screen that keeps watches and polls only while it is active (on
/// screen). The workspace activates the shown screen and deactivates the
/// others; screens with several pages pick the page separately.
pub trait ScreenActivity: Sized + 'static {
    fn set_active(&mut self, active: bool, cx: &mut Context<Self>);
}

/// [`ScreenActivity::set_active`] on an entity.
pub fn set_active<S: ScreenActivity>(screen: &Entity<S>, active: bool, cx: &mut App) {
    screen.update(cx, |screen, cx| screen.set_active(active, cx));
}

/// Change notifications of the cache: of one watch or of a set.
pub trait Notifications: 'static {
    /// Waits for the next change; `false` once no more will come.
    fn changed(&mut self) -> impl Future<Output = bool>;
}

impl Notifications for Changes {
    fn changed(&mut self) -> impl Future<Output = bool> {
        self.next()
    }
}

impl Notifications for MergedChanges {
    fn changed(&mut self) -> impl Future<Output = bool> {
        self.next()
    }
}

/// Calls `on_change` for each (coalesced) change of `set`, the first time
/// right away, until the set's watches or the entity are gone. Dropping the
/// task stops it.
pub fn watch_set<T: 'static>(
    set: &SubscriptionSet,
    cx: &mut Context<T>,
    on_change: impl Fn(&mut T, &mut Context<T>) + 'static,
) -> Task<()> {
    watch_changes(set.changes(), cx, on_change)
}

/// [`watch_set`] for any [`Notifications`].
pub fn watch_changes<T: 'static>(
    mut changes: impl Notifications,
    cx: &mut Context<T>,
    on_change: impl Fn(&mut T, &mut Context<T>) + 'static,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        while changes.changed().await {
            if this.update(cx, |this, cx| on_change(this, cx)).is_err() {
                break;
            }
        }
    })
}
