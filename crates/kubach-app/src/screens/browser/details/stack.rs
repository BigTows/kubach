//! Objects the details show in place (#393): a click on a pod in the
//! "Pods" block of a Deployment shows the pod's details in the same panel,
//! the browser's list and selection stay as they are. Each such object is
//! a frame on a stack over the browser's selected object; "← Deployment
//! web" at the top (or Esc, or the mouse's back button) goes back one
//! frame. Selecting another row of the list drops the stack.
//!
//! Any block can push a frame with [`ShowObject`]; a frame follows its
//! object live from a watch of its namespace (joining the one the block
//! already holds).
//!
//! Another screen can show the details over a view of its own (#394): the
//! topology opens a node's object with [`ResourceDetails::open`]. The
//! object is then the bottom frame, there is no selection under it, and
//! "← Deployment web" on it goes back to that screen's view
//! ([`DetailsEvent::Back`](super::DetailsEvent::Back)).

use std::sync::Arc;

use kubach_core::ResourceKind;
use kubach_core::cache::{Subscription as Watch, WatchStatus};
use kube::api::DynamicObject;
use kube::discovery::verbs;

use super::super::super::activity::watch_changes;
use super::{DetailsEvent, DetailsTarget, ResourceDetails};
use crate::cluster::CatalogState;
use gpui_kit::{Context, Task};

/// Asks the details to show `object` (of `group`/`kind`) in place, with
/// a way back to what they show now.
pub(in super::super) struct ShowObject {
    pub group: String,
    pub kind: String,
    pub object: Arc<DynamicObject>,
}

/// An object to open by reference (a node of the topology): shown once
/// a watch of its kind in its namespace lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::screens) struct OpenObject {
    pub group: String,
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
}

/// An object opened by reference that its watch does not list yet.
pub(super) struct Pending {
    kind: ResourceKind,
    namespace: Option<String>,
    name: String,
    watch: Watch,
    _listener: Task<()>,
}

impl Pending {
    /// How its watch is doing: loaded without the object, failing, or
    /// still loading.
    pub(super) fn status(&self) -> WatchStatus {
        self.watch.snapshot().status
    }
}

/// What "back" does on the shown object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Back {
    /// Pops the top frame.
    Pop,
    /// Leaves the details for the view of the screen that opened them.
    Home,
    /// Nothing to go back to.
    Nothing,
}

/// "Back" over `depth` frames, on a selection (`base`) or not, with a view
/// to return to (`home`) or not.
pub(super) fn back_step(base: bool, depth: usize, home: bool) -> Back {
    let floor = if base { 0 } else { 1 };
    if depth > floor {
        Back::Pop
    } else if home {
        Back::Home
    } else {
        Back::Nothing
    }
}

/// The label of "back": the object under the top frame, or the view the
/// details were opened from.
pub(super) fn back_label(
    base: Option<&DetailsTarget>,
    stack: &[Frame],
    home: Option<&str>,
) -> Option<String> {
    match back_step(base.is_some(), stack.len(), home.is_some()) {
        Back::Pop => back_of(base, stack).map(|back| {
            format!(
                "{} {}",
                back.kind.api.kind,
                back.object.metadata.name.as_deref().unwrap_or_default()
            )
        }),
        Back::Home => home.map(str::to_owned),
        Back::Nothing => None,
    }
}

/// An object shown over the browser's selection.
pub(super) struct Frame {
    pub target: DetailsTarget,
    /// Keeps the object current; `None` when the kind cannot be watched.
    watch: Option<Watch>,
    _listener: Option<Task<()>>,
}

/// Whether `a` and `b` are the same object (any version).
pub(super) fn same_object(a: &DetailsTarget, b: &DetailsTarget) -> bool {
    let (a_meta, b_meta) = (&a.object.metadata, &b.object.metadata);
    a.kind.api == b.kind.api
        && a_meta.uid == b_meta.uid
        && a_meta.name == b_meta.name
        && a_meta.namespace == b_meta.namespace
}

/// What the details show: the top frame, or the browser's selection.
pub(super) fn shown(base: Option<&DetailsTarget>, stack: &[Frame]) -> Option<DetailsTarget> {
    stack
        .last()
        .map(|frame| frame.target.clone())
        .or_else(|| base.cloned())
}

/// Where "back" leads: the object under the top frame.
pub(super) fn back_of<'a>(
    base: Option<&'a DetailsTarget>,
    stack: &'a [Frame],
) -> Option<&'a DetailsTarget> {
    match stack.len() {
        0 => None,
        1 => base,
        n => Some(&stack[n - 2].target),
    }
}

impl ResourceDetails {
    /// Shows `show.object` in place, over what is shown now.
    pub(super) fn push(&mut self, show: &ShowObject, cx: &mut Context<Self>) {
        let cluster = self.cluster.read(cx);
        let (Some(cache), CatalogState::Ready(catalog)) = (cluster.cache(), cluster.catalog())
        else {
            return;
        };
        let Some(kind) = catalog.find(&show.group, &show.kind).cloned() else {
            return;
        };
        let cache = cache.clone();
        let watch = kind
            .supports(verbs::WATCH)
            .then(|| cache.subscribe(&kind, show.object.metadata.namespace.as_deref()));
        self.push_frame(kind, show.object.clone(), watch, cx);
    }

    /// Puts `object` on top, followed live by `watch`.
    fn push_frame(
        &mut self,
        kind: ResourceKind,
        object: Arc<DynamicObject>,
        watch: Option<Watch>,
        cx: &mut Context<Self>,
    ) {
        let object = kubach_core::discovery::typed(object, &kind.api);
        let listener = watch
            .as_ref()
            .map(|watch| watch_changes(watch.changes(), cx, |this: &mut Self, cx| this.follow(cx)));
        let target = DetailsTarget {
            plugin: self.registry.plugin_for(&kind.api.group, &kind.api.kind),
            kind,
            object,
            provisional: false,
            from_disk: false,
            reconnecting: false,
            running: self
                .base
                .as_ref()
                .map_or(self.screen_running, |base| base.running),
        };
        self.stack.push(Frame {
            target,
            watch,
            _listener: listener,
        });
        self.focus_pending = true;
        self.show_top(cx);
    }

    /// Opens `object` over a view of the screen (#394): any shown object
    /// and frames go; "back" on it emits [`DetailsEvent::Back`], labeled
    /// `home`. Shown once a watch of its kind in its namespace lists it,
    /// then followed live. `false` when the cluster cannot list the kind.
    pub(in crate::screens) fn open(
        &mut self,
        object: &OpenObject,
        home: String,
        running: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        self.clear(cx);
        self.base = None;
        self.show_top(cx);
        let cluster = self.cluster.read(cx);
        let (Some(cache), CatalogState::Ready(catalog)) = (cluster.cache(), cluster.catalog())
        else {
            return false;
        };
        let Some(kind) = catalog
            .find(&object.group, &object.kind)
            .filter(|kind| kind.supports(verbs::WATCH))
            .cloned()
        else {
            return false;
        };
        let watch = cache.subscribe(&kind, object.namespace.as_deref());
        let listener = watch_changes(watch.changes(), cx, |this: &mut Self, cx| this.resolve(cx));
        self.home = Some(home);
        self.screen_running = running;
        self.pending = Some(Pending {
            kind,
            namespace: object.namespace.clone(),
            name: object.name.clone(),
            watch,
            _listener: listener,
        });
        self.resolve(cx);
        cx.notify();
        true
    }

    /// Shows the object opened by reference once its watch lists it.
    fn resolve(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = &self.pending else {
            return;
        };
        let snapshot = pending.watch.snapshot();
        let found = snapshot
            .objects
            .iter()
            .find(|object| {
                object.metadata.namespace == pending.namespace
                    && object.metadata.name.as_deref() == Some(pending.name.as_str())
            })
            .cloned();
        let Some(found) = found else {
            // "Loading…" becomes "Not found" when the watch loads.
            cx.notify();
            return;
        };
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.push_frame(pending.kind, found, Some(pending.watch), cx);
    }

    /// Whether an action of the screen runs: the action buttons are off.
    pub(in crate::screens) fn set_running(&mut self, running: bool, cx: &mut Context<Self>) {
        self.screen_running = running;
        if let Some(base) = &mut self.base {
            base.running = running;
        }
        for frame in &mut self.stack {
            frame.target.running = running;
        }
        self.show_top(cx);
    }

    /// Whether "back" leads anywhere from what is shown.
    pub(super) fn can_go_back(&self) -> bool {
        self.back_step() != Back::Nothing
    }

    fn back_step(&self) -> Back {
        back_step(self.base.is_some(), self.stack.len(), self.home.is_some())
    }

    /// Back to the object under the top frame, or to the view the
    /// details were opened from.
    pub(super) fn back(&mut self, cx: &mut Context<Self>) {
        match self.back_step() {
            Back::Pop => {
                self.stack.pop();
                self.show_top(cx);
            }
            Back::Home => cx.emit(DetailsEvent::Back),
            Back::Nothing => {}
        }
    }

    /// Shows the top frame (or the selection); redraws when that changed.
    pub(super) fn show_top(&mut self, cx: &mut Context<Self>) {
        let shown = shown(self.base.as_ref(), &self.stack);
        let same = match (&self.target, &shown) {
            (Some(old), Some(new)) => old.same_as(new),
            (None, None) => true,
            _ => false,
        };
        self.target = shown;
        if !same {
            cx.notify();
        }
    }

    /// Takes the frames' objects as their watches have them now. A pod
    /// deleted meanwhile stays shown as it was last.
    fn follow(&mut self, cx: &mut Context<Self>) {
        for frame in &mut self.stack {
            let Some(watch) = &frame.watch else {
                continue;
            };
            let snapshot = watch.snapshot();
            let meta = &frame.target.object.metadata;
            let found = snapshot.objects.iter().find(|object| {
                object.metadata.namespace == meta.namespace
                    && object.metadata.name == meta.name
                    && object.metadata.uid == meta.uid
            });
            if let Some(found) = found {
                frame.target.object =
                    kubach_core::discovery::typed(found.clone(), &frame.target.kind.api);
            }
            frame.target.reconnecting = watch.is_reconnecting();
        }
        self.show_top(cx);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kubach_core::ResourceKind;
    use kubach_core::plugin::PluginRegistry;
    use kube::api::{ApiResource, DynamicObject, GroupVersionKind};

    use super::{Back, DetailsTarget, Frame, back_label, back_of, back_step, same_object, shown};

    fn target(group: &str, kind: &str, name: &str, version: &str) -> DetailsTarget {
        let api = ApiResource::from_gvk(&GroupVersionKind::gvk(group, "v1", kind));
        let mut object = DynamicObject::new(name, &api).within("shop");
        object.metadata.uid = Some(format!("uid-{name}"));
        object.metadata.resource_version = Some(version.into());
        DetailsTarget {
            plugin: PluginRegistry::with_builtins().plugin_for_resource(&api),
            kind: ResourceKind {
                api,
                namespaced: true,
                verbs: Vec::new(),
            },
            object: Arc::new(object),
            provisional: false,
            from_disk: false,
            reconnecting: false,
            running: false,
        }
    }

    fn frame(target: DetailsTarget) -> Frame {
        Frame {
            target,
            watch: None,
            _listener: None,
        }
    }

    fn name(target: Option<&DetailsTarget>) -> Option<&str> {
        target.and_then(|t| t.object.metadata.name.as_deref())
    }

    #[test]
    fn the_top_frame_is_shown_and_back_leads_under_it() {
        let deployment = target("apps", "Deployment", "web", "1");
        assert_eq!(name(shown(Some(&deployment), &[]).as_ref()), Some("web"));
        assert!(back_of(Some(&deployment), &[]).is_none());

        let stack = vec![frame(target("", "Pod", "web-1", "5"))];
        assert_eq!(
            name(shown(Some(&deployment), &stack).as_ref()),
            Some("web-1")
        );
        assert_eq!(name(back_of(Some(&deployment), &stack)), Some("web"));

        let stack = vec![
            frame(target("", "Pod", "web-1", "5")),
            frame(target("", "Service", "web", "7")),
        ];
        assert_eq!(name(shown(Some(&deployment), &stack).as_ref()), Some("web"));
        assert_eq!(
            back_of(Some(&deployment), &stack).map(|t| t.kind.api.kind.as_str()),
            Some("Pod")
        );
    }

    #[test]
    fn a_new_version_is_the_same_object_another_row_is_not() {
        let web = target("apps", "Deployment", "web", "1");
        // An update of the selected object keeps the stack.
        assert!(same_object(&web, &target("apps", "Deployment", "web", "2")));
        // Another row, or another kind of the same name, drops it.
        assert!(!same_object(
            &web,
            &target("apps", "Deployment", "api", "1")
        ));
        assert!(!same_object(
            &web,
            &target("apps", "StatefulSet", "web", "1")
        ));
    }

    /// The topology (#394): node summary → details of the Deployment →
    /// one of its pods → back → back to the summary.
    #[test]
    fn details_opened_over_a_screen_go_back_to_it() {
        let home = Some("Deployment web");
        // Opening: the watch has not listed the Deployment yet.
        assert_eq!(back_step(false, 0, true), Back::Home);
        assert_eq!(back_label(None, &[], home).as_deref(), home);

        // The Deployment shown: back leads to the node summary.
        let mut stack = vec![frame(target("apps", "Deployment", "web", "1"))];
        assert_eq!(name(shown(None, &stack).as_ref()), Some("web"));
        assert_eq!(back_step(false, stack.len(), true), Back::Home);
        assert_eq!(back_label(None, &stack, home).as_deref(), home);

        // A pod of it, from the Pods block: back leads to the Deployment.
        stack.push(frame(target("", "Pod", "web-1", "5")));
        assert_eq!(name(shown(None, &stack).as_ref()), Some("web-1"));
        assert_eq!(back_step(false, stack.len(), true), Back::Pop);
        assert_eq!(
            back_label(None, &stack, home).as_deref(),
            Some("Deployment web")
        );

        // Back: the Deployment again; back once more: the summary.
        stack.pop();
        assert_eq!(name(shown(None, &stack).as_ref()), Some("web"));
        assert_eq!(back_step(false, stack.len(), true), Back::Home);
    }

    /// The browser: back pops frames down to the selection, no further.
    #[test]
    fn details_of_a_selection_stop_at_it() {
        let deployment = target("apps", "Deployment", "web", "1");
        assert_eq!(back_step(true, 0, false), Back::Nothing);
        assert_eq!(back_label(Some(&deployment), &[], None), None);
        let stack = vec![frame(target("", "Pod", "web-1", "5"))];
        assert_eq!(back_step(true, 1, false), Back::Pop);
        assert_eq!(
            back_label(Some(&deployment), &stack, None).as_deref(),
            Some("Deployment web")
        );
        // Without a selection or a screen under it, nothing to go back to.
        assert_eq!(back_step(false, 1, false), Back::Nothing);
    }
}
