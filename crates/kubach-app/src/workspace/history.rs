//! "← Back" to where a jump came from (#395).
//!
//! A jump is a transition a screen makes on its own: a [`Reveal`] link
//! (Topology, Access, GitOps, the details' owners, "Managed by",
//! Connections) or the charts opening the releases after an install. Each
//! one records the place it left; "back" (the titlebar button, `⌘[` /
//! `Alt+←`, the mouse's back button) returns there.
//!
//! The sidebar, the status bar and the hotbar are not recorded: the user
//! picks those places himself and sees where he is, so recording each click
//! would turn "back" into a replay of the sidebar, and one jump would sink
//! under the clicks after it. Sidebar clicks after a jump leave the history
//! as it is, so "back" still returns to where the jump came from.
//!
//! The history belongs to one context: a cluster switch clears it.
//!
//! [`Reveal`]: crate::screens::Reveal

use gpui_kit::SharedString;
use kubach_i18n::t;
use kubach_ui::MultiSelection;

use crate::screens::BrowserPlace;

/// How many places "back" remembers; the oldest go first.
pub(super) const HISTORY_MAX: usize = 50;

/// A place of the workspace, as much of it as can be shown again cheaply.
/// Screens other than the resource browser keep their own state (filters,
/// selection, the topology's graph and opened node) while hidden, so their
/// sidebar item is enough to show them again as they were.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Location {
    /// The sidebar item: a screen or a resource kind.
    pub nav_id: SharedString,
    /// "Topology", "Deployments", "Deployment web": the button's label.
    pub label: String,
    /// The browser's selection and search, on a resource kind.
    pub browser: Option<BrowserPlace>,
    /// The namespaces shown then (a jump may add one).
    pub namespaces: Option<MultiSelection>,
}

impl Location {
    /// Whether both are the same place, whatever namespaces they show.
    pub fn same_place(&self, other: &Location) -> bool {
        self.nav_id == other.nav_id && self.browser == other.browser
    }
}

/// How the workspace gets to another place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Transition {
    /// A sidebar item (or `⌘1…9`, a context switch restoring the sidebar).
    Sidebar,
    /// A status bar item ("2 port forwards").
    StatusBar,
    /// A link of a screen or the details: [`Reveal`](crate::screens::Reveal).
    Reveal,
    /// The charts open the releases after an install.
    ReleaseInstalled,
}

impl Transition {
    /// Whether "back" should return to the place it leaves: only what a
    /// screen does on its own, see the module docs.
    pub fn is_jump(self) -> bool {
        match self {
            Transition::Reveal | Transition::ReleaseInstalled => true,
            Transition::Sidebar | Transition::StatusBar => false,
        }
    }
}

/// The namespaces "back" puts back: those of `target` when the jump (or the
/// user since) changed them.
pub(super) fn namespaces_to_restore(
    current: &Location,
    target: &Location,
) -> Option<MultiSelection> {
    target
        .namespaces
        .clone()
        .filter(|selection| Some(selection) != current.namespaces.as_ref())
}

/// Places jumps came from, last on top.
#[derive(Default)]
pub(super) struct History {
    entries: Vec<Location>,
}

impl History {
    /// Records `from`, the place a jump leaves. The same place twice in a
    /// row is kept once.
    pub fn record(&mut self, from: Location) {
        if let Some(top) = self.entries.last_mut()
            && top.same_place(&from)
        {
            *top = from;
            return;
        }
        self.entries.push(from);
        if self.entries.len() > HISTORY_MAX {
            let extra = self.entries.len() - HISTORY_MAX;
            self.entries.drain(..extra);
        }
    }

    /// Where "back" goes from `current`: the last place that isn't it.
    pub fn target(&self, current: &Location) -> Option<&Location> {
        self.entries
            .iter()
            .rev()
            .find(|entry| !entry.same_place(current))
    }

    /// Takes the place to go back to from `current`, dropping it and the
    /// places on top of it that are `current` itself (the user went back
    /// there through the sidebar).
    pub fn back(&mut self, current: &Location) -> Option<Location> {
        while let Some(top) = self.entries.pop() {
            if !top.same_place(current) {
                return Some(top);
            }
        }
        None
    }

    /// Forgets everything (another context).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// "Back to Topology", the button's tooltip, with the shortcut of the
/// platform.
pub(super) fn back_tooltip(label: &str) -> String {
    t!(
        "workspace-back-tooltip",
        place = label,
        shortcut = BACK_SHORTCUT
    )
}

#[cfg(target_os = "macos")]
const BACK_SHORTCUT: &str = "⌘[";
#[cfg(not(target_os = "macos"))]
const BACK_SHORTCUT: &str = "Alt+←";

#[cfg(test)]
mod tests {
    use kubach_ui::MultiSelection;

    use super::{HISTORY_MAX, History, Location, Transition, namespaces_to_restore};
    use crate::screens::BrowserPlace;

    fn screen(nav_id: &str) -> Location {
        Location {
            nav_id: nav_id.to_owned().into(),
            label: nav_id.to_owned(),
            browser: None,
            namespaces: None,
        }
    }

    fn kind(nav_id: &str, selected: Option<&str>, query: &str) -> Location {
        Location {
            nav_id: nav_id.to_owned().into(),
            label: nav_id.to_owned(),
            browser: Some(BrowserPlace {
                selected: selected.map(ToOwned::to_owned),
                query: query.to_owned(),
            }),
            namespaces: None,
        }
    }

    #[test]
    fn back_returns_places_in_reverse_order() {
        let mut history = History::default();
        let pods = kind("kind:/Pod", Some("web/api-1"), "");
        history.record(screen("topology"));
        history.record(kind("kind:apps/Deployment", Some("web/api"), "ap"));
        assert_eq!(
            history.target(&pods).map(|l| l.nav_id.as_ref()),
            Some("kind:apps/Deployment")
        );
        let back = history.back(&pods).unwrap();
        // The browser's selection and search come back with the kind.
        assert_eq!(back, kind("kind:apps/Deployment", Some("web/api"), "ap"));
        assert_eq!(history.back(&back).unwrap(), screen("topology"));
        assert_eq!(history.back(&screen("topology")), None);
        assert_eq!(history.target(&screen("topology")), None);
    }

    #[test]
    fn keeps_the_last_places_only() {
        let mut history = History::default();
        for ix in 0..HISTORY_MAX + 10 {
            history.record(kind("kind:/Pod", Some(&format!("web/p{ix}")), ""));
        }
        assert_eq!(history.len(), HISTORY_MAX);
        let mut current = screen("topology");
        let mut last = None;
        while let Some(place) = history.back(&current) {
            last = Some(place.clone());
            current = place;
        }
        // The first ten were dropped.
        assert_eq!(last.unwrap(), kind("kind:/Pod", Some("web/p10"), ""));
    }

    #[test]
    fn the_same_place_twice_is_kept_once() {
        let mut history = History::default();
        history.record(screen("topology"));
        let mut again = screen("topology");
        again.namespaces = Some(MultiSelection::All);
        history.record(again.clone());
        assert_eq!(history.len(), 1);
        // The newer namespaces win.
        assert_eq!(history.back(&screen("events")), Some(again));
    }

    #[test]
    fn sidebar_back_to_the_target_skips_it() {
        // Topology → (jump) Pods; the user clicks Topology in the sidebar,
        // then Events: "back" from Events goes to Topology; from Topology
        // itself there is nothing left.
        let mut history = History::default();
        history.record(screen("permissions"));
        history.record(screen("topology"));
        assert_eq!(history.target(&screen("events")), Some(&screen("topology")));
        let mut on_topology = screen("topology");
        on_topology.namespaces = Some(MultiSelection::All);
        // Namespaces don't make it another place.
        assert_eq!(history.target(&on_topology), Some(&screen("permissions")));
        assert_eq!(history.back(&on_topology), Some(screen("permissions")));
        assert_eq!(history.len(), 0);
    }

    #[test]
    fn only_jumps_are_recorded() {
        assert!(Transition::Reveal.is_jump());
        assert!(Transition::ReleaseInstalled.is_jump());
        assert!(!Transition::Sidebar.is_jump());
        assert!(!Transition::StatusBar.is_jump());
    }

    #[test]
    fn back_puts_back_the_namespaces_a_jump_added() {
        let web = MultiSelection::Items(vec!["web".into()]);
        let both = MultiSelection::Items(vec!["web".into(), "db".into()]);
        let mut topology = screen("topology");
        topology.namespaces = Some(web.clone());
        let mut pods = kind("kind:/Pod", Some("db/postgres-0"), "");
        pods.namespaces = Some(both);
        assert_eq!(namespaces_to_restore(&pods, &topology), Some(web.clone()));
        // Unchanged: nothing to restore.
        pods.namespaces = Some(web);
        assert_eq!(namespaces_to_restore(&pods, &topology), None);
        // Not known then: left as they are.
        assert_eq!(namespaces_to_restore(&pods, &screen("events")), None);
    }

    #[test]
    fn clear_forgets_everything() {
        let mut history = History::default();
        history.record(screen("topology"));
        history.clear();
        assert_eq!(history.back(&screen("events")), None);
    }
}
