//! Several subscriptions read as one list, e.g. a kind in a few namespaces.

use std::sync::{Arc, Mutex};

use futures::future::select_all;
use kube::api::DynamicObject;

use super::{Changes, Objects, Snapshot, Subscription, WatchError, WatchStatus};
use crate::sync::lock;

/// Subscriptions whose objects are shown together. Keeps every watch alive
/// while it lives.
#[derive(Clone)]
pub struct SubscriptionSet {
    parts: Vec<Subscription>,
    /// The last merged list with the lists it was merged from, shared by
    /// the clones of the set.
    merged: Arc<Mutex<Option<Merged>>>,
}

/// A merged list and the lists of the watches it came from. A watch keeps
/// its list (the same `Arc`) until its objects change, so the merged one is
/// good while every part is still the same allocation.
struct Merged {
    parts: Vec<Objects>,
    objects: Objects,
}

impl SubscriptionSet {
    pub fn new(parts: Vec<Subscription>) -> Self {
        Self {
            parts,
            merged: Arc::default(),
        }
    }

    pub fn subscriptions(&self) -> &[Subscription] {
        &self.parts
    }

    /// Objects of every watch, merged and sorted by namespace and name.
    /// While no watch has changed, the list is the one returned before (the
    /// same `Arc`), not merged again.
    pub fn snapshot(&self) -> MergedSnapshot {
        let parts: Vec<_> = self
            .parts
            .iter()
            .map(|part| (part.key().namespace.clone(), part.snapshot()))
            .collect();
        let (status, errors) = status_of(&parts);
        let objects = match parts.as_slice() {
            [] => Objects::from([]),
            [(_, only)] => Arc::clone(&only.objects),
            _ => {
                let mut merged = lock(&self.merged);
                match &*merged {
                    Some(last) if last.is_of(&parts) => Arc::clone(&last.objects),
                    _ => {
                        let lists: Vec<&Objects> =
                            parts.iter().map(|(_, part)| &part.objects).collect();
                        let objects = merge_objects(&lists);
                        *merged = Some(Merged {
                            parts: lists.into_iter().cloned().collect(),
                            objects: Arc::clone(&objects),
                        });
                        objects
                    }
                }
            }
        };
        MergedSnapshot {
            objects,
            status,
            errors,
        }
    }

    /// Fires when any of the watches changes. Like [`Subscription::changes`],
    /// the first [`MergedChanges::next`] resolves right away.
    pub fn changes(&self) -> MergedChanges {
        MergedChanges(self.parts.iter().map(Subscription::changes).collect())
    }
}

/// What a [`SubscriptionSet`] currently holds.
#[derive(Clone, Debug)]
pub struct MergedSnapshot {
    /// Sorted by namespace, then name; an object seen by two watches once.
    pub objects: Objects,
    /// `Loading` while any watch is loading, `Error` only when every watch
    /// fails, `Ready` otherwise (failing watches are listed in `errors`).
    pub status: WatchStatus,
    /// Failing watches by namespace (`None`: all namespaces or cluster-scoped).
    pub errors: Vec<(Option<String>, WatchError)>,
}

impl Merged {
    fn is_of(&self, parts: &[(Option<String>, Snapshot)]) -> bool {
        self.parts.len() == parts.len()
            && self
                .parts
                .iter()
                .zip(parts)
                .all(|(merged, (_, part))| Arc::ptr_eq(merged, &part.objects))
    }
}

/// Merges snapshots of watches keyed by namespace.
pub fn merge(parts: impl IntoIterator<Item = (Option<String>, Snapshot)>) -> MergedSnapshot {
    let parts: Vec<_> = parts.into_iter().collect();
    let (status, errors) = status_of(&parts);
    let objects = match parts.as_slice() {
        [] => Objects::from([]),
        [(_, only)] => Arc::clone(&only.objects),
        _ => {
            let lists: Vec<&Objects> = parts.iter().map(|(_, part)| &part.objects).collect();
            merge_objects(&lists)
        }
    };
    MergedSnapshot {
        objects,
        status,
        errors,
    }
}

/// The status of the whole and the failing watches by namespace.
fn status_of(
    parts: &[(Option<String>, Snapshot)],
) -> (WatchStatus, Vec<(Option<String>, WatchError)>) {
    let errors: Vec<_> = parts
        .iter()
        .filter_map(|(namespace, snapshot)| match &snapshot.status {
            WatchStatus::Error(error) => Some((namespace.clone(), error.clone())),
            _ => None,
        })
        .collect();
    let status = if parts
        .iter()
        .any(|(_, snapshot)| snapshot.status == WatchStatus::Loading)
    {
        WatchStatus::Loading
    } else if !parts.is_empty() && errors.len() == parts.len() {
        WatchStatus::Error(errors[0].1.clone())
    } else {
        WatchStatus::Ready
    };
    (status, errors)
}

/// What the merged list is sorted by, borrowed from the object.
fn sort_key(object: &DynamicObject) -> (Option<&str>, Option<&str>) {
    (
        object.metadata.namespace.as_deref(),
        object.metadata.name.as_deref(),
    )
}

/// One list of the objects of `parts`, sorted by namespace and name; of
/// objects with the same namespace and name, the one of the earliest part.
///
/// The lists of watches are sorted and, being of different namespaces, do
/// not overlap: put in the order of their first objects they are the result
/// as they stand, which costs one comparison per object to make sure of.
/// Anything else (a namespace watched twice with different lists, a watch of
/// all namespaces next to one of a namespace, unsorted input) is sorted.
pub(crate) fn merge_objects(parts: &[&Objects]) -> Objects {
    // A namespace listed twice is the same watch, hence the same list.
    let mut distinct: Vec<&Objects> = Vec::with_capacity(parts.len());
    for part in parts {
        if !part.is_empty() && !distinct.iter().any(|seen| Arc::ptr_eq(seen, part)) {
            distinct.push(part);
        }
    }
    let mut ordered = distinct.clone();
    ordered.sort_by(|a, b| sort_key(&a[0]).cmp(&sort_key(&b[0])));
    let ascending = |part: &Objects| {
        part.windows(2)
            .all(|pair| sort_key(&pair[0]) < sort_key(&pair[1]))
    };
    let apart = ordered.windows(2).all(|pair| {
        let last = &pair[0][pair[0].len() - 1];
        sort_key(last) < sort_key(&pair[1][0])
    });
    if apart && ordered.iter().all(|part| ascending(part)) {
        return match ordered.as_slice() {
            [] => Objects::from([]),
            [only] => Arc::clone(only),
            _ => {
                let mut all = Vec::with_capacity(ordered.iter().map(|part| part.len()).sum());
                for part in ordered {
                    all.extend_from_slice(part);
                }
                all.into()
            }
        };
    }
    let mut all: Vec<_> = distinct
        .iter()
        .flat_map(|part| part.iter().cloned())
        .collect();
    // Stable, so the object of the earliest part stays first of its equals.
    all.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    all.dedup_by(|b, a| sort_key(a) == sort_key(b));
    all.into()
}

/// Change notifications of a [`SubscriptionSet`].
pub struct MergedChanges(Vec<Changes>);

impl MergedChanges {
    /// Waits until any watch changes. Returns `false` once every watch has
    /// stopped.
    pub async fn next(&mut self) -> bool {
        loop {
            if self.0.is_empty() {
                return false;
            }
            let (changed, ix) = {
                let (changed, ix, _rest) =
                    select_all(self.0.iter_mut().map(|changes| Box::pin(changes.next()))).await;
                (changed, ix)
            };
            if changed {
                return true;
            }
            self.0.swap_remove(ix);
        }
    }
}

#[cfg(test)]
mod tests {
    use kube::api::DynamicObject;
    use rand_chacha::ChaCha8Rng;
    use rand_chacha::rand_core::{RngCore, SeedableRng as _};
    use serde_json::json;

    use super::*;

    /// The merge as it was first written (sort and dedup of everything by
    /// owned keys): what [`merge_objects`] must keep returning.
    fn reference(parts: &[&Objects]) -> Vec<Arc<DynamicObject>> {
        let mut all: Vec<_> = parts.iter().flat_map(|part| part.iter().cloned()).collect();
        let key = |object: &Arc<DynamicObject>| {
            (
                object.metadata.namespace.clone(),
                object.metadata.name.clone(),
            )
        };
        all.sort_by_key(key);
        all.dedup_by_key(|object| key(object));
        all
    }

    fn assert_same_objects(merged: &Objects, expected: &[Arc<DynamicObject>], case: &str) {
        assert_eq!(merged.len(), expected.len(), "{case}");
        for (ix, (got, want)) in merged.iter().zip(expected).enumerate() {
            assert!(Arc::ptr_eq(got, want), "{case}: object {ix} differs");
        }
    }

    fn object(namespace: &str, name: &str) -> Arc<DynamicObject> {
        Arc::new(
            serde_json::from_value(json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": {"name": name, "namespace": namespace}
            }))
            .unwrap(),
        )
    }

    fn snapshot(status: WatchStatus, objects: &[(&str, &str)]) -> Snapshot {
        Snapshot {
            objects: objects.iter().map(|(ns, name)| object(ns, name)).collect(),
            status,
            generation: 1,
        }
    }

    fn forbidden(namespace: &str) -> WatchStatus {
        WatchStatus::Error(WatchError {
            forbidden: true,
            connection: false,
            message: format!("forbidden in {namespace}"),
        })
    }

    fn names(merged: &MergedSnapshot) -> Vec<String> {
        merged
            .objects
            .iter()
            .map(|o| {
                format!(
                    "{}/{}",
                    o.metadata.namespace.as_deref().unwrap_or(""),
                    o.metadata.name.as_deref().unwrap_or("")
                )
            })
            .collect()
    }

    #[test]
    fn merges_sorted_and_deduplicated() {
        let merged = merge([
            (
                Some("web".into()),
                snapshot(WatchStatus::Ready, &[("web", "a"), ("web", "c")]),
            ),
            (
                Some("api".into()),
                snapshot(WatchStatus::Ready, &[("api", "z"), ("web", "a")]),
            ),
        ]);
        assert_eq!(names(&merged), ["api/z", "web/a", "web/c"]);
        assert_eq!(merged.status, WatchStatus::Ready);
        assert!(merged.errors.is_empty());
    }

    #[test]
    fn a_single_watch_is_shared_not_copied() {
        let part = snapshot(WatchStatus::Ready, &[("web", "a")]);
        let merged = merge([(Some("web".into()), part.clone())]);
        assert!(Arc::ptr_eq(&merged.objects, &part.objects));
    }

    #[test]
    fn errors_in_some_namespaces_keep_the_others() {
        let merged = merge([
            (
                Some("web".into()),
                snapshot(WatchStatus::Ready, &[("web", "a")]),
            ),
            (Some("secret".into()), snapshot(forbidden("secret"), &[])),
        ]);
        assert_eq!(merged.status, WatchStatus::Ready);
        assert_eq!(names(&merged), ["web/a"]);
        assert_eq!(merged.errors.len(), 1);
        assert_eq!(merged.errors[0].0.as_deref(), Some("secret"));
        assert!(merged.errors[0].1.forbidden);
    }

    #[test]
    fn status_is_loading_until_all_load_and_error_only_if_all_fail() {
        let loading = merge([
            (Some("a".into()), snapshot(WatchStatus::Ready, &[])),
            (Some("b".into()), snapshot(WatchStatus::Loading, &[])),
        ]);
        assert_eq!(loading.status, WatchStatus::Loading);

        let failed = merge([
            (Some("a".into()), snapshot(forbidden("a"), &[])),
            (Some("b".into()), snapshot(forbidden("b"), &[])),
        ]);
        assert!(matches!(failed.status, WatchStatus::Error(e) if e.message == "forbidden in a"));
        assert_eq!(failed.errors.len(), 2);

        let empty = merge([]);
        assert_eq!(empty.status, WatchStatus::Ready);
        assert!(empty.objects.is_empty());
    }

    /// An object with any of namespace and name missing or empty.
    fn loose_object(namespace: Option<&str>, name: Option<&str>) -> Arc<DynamicObject> {
        let mut object: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {}
        }))
        .unwrap();
        object.metadata.namespace = namespace.map(str::to_owned);
        object.metadata.name = name.map(str::to_owned);
        Arc::new(object)
    }

    #[test]
    fn merges_like_the_reference_on_random_parts() {
        const NAMESPACES: [Option<&str>; 6] = [
            None,
            Some(""),
            Some("a"),
            Some("a-b"),
            Some("ab"),
            Some("b"),
        ];
        const NAMES: [Option<&str>; 7] = [
            None,
            Some(""),
            Some("a"),
            Some("a-1"),
            Some("a1"),
            Some("b"),
            Some("c"),
        ];
        let mut rng = ChaCha8Rng::seed_from_u64(170);
        let mut pick = move |below: usize| rng.next_u32() as usize % below;
        let mut concatenated = 0;
        for case in 0..4000 {
            // 0: what watches give (a sorted list per namespace); 1: sorted
            // lists that may overlap; 2: anything.
            let shape = case % 3;
            let mut parts: Vec<Objects> = Vec::new();
            for part_ix in 0..pick(6) {
                let mut part: Vec<Arc<DynamicObject>> = (0..pick(8))
                    .map(|_| {
                        let namespace = match shape {
                            0 => NAMESPACES[2 + part_ix % 4],
                            _ => NAMESPACES[pick(NAMESPACES.len())],
                        };
                        loose_object(namespace, NAMES[pick(NAMES.len())])
                    })
                    .collect();
                if shape < 2 {
                    part.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
                    part.dedup_by(|b, a| sort_key(a) == sort_key(b));
                }
                parts.push(part.into());
            }
            // A namespace listed twice: the same list again.
            if !parts.is_empty() && pick(4) == 0 {
                let again = Arc::clone(&parts[pick(parts.len())]);
                parts.insert(pick(parts.len() + 1), again);
            }
            let lists: Vec<&Objects> = parts.iter().collect();
            let expected = reference(&lists);
            let merged = merge_objects(&lists);
            assert_same_objects(&merged, &expected, &format!("case {case}"));
            let total: usize = parts.iter().map(|part| part.len()).sum();
            concatenated += usize::from(shape == 0 && total > 0);
        }
        assert!(concatenated > 500);
    }

    #[test]
    fn parts_in_any_order_are_put_together_sorted() {
        let merged = merge([
            (
                Some("web".into()),
                snapshot(WatchStatus::Ready, &[("web", "a"), ("web", "c")]),
            ),
            (Some("db".into()), snapshot(WatchStatus::Ready, &[])),
            (
                Some("api".into()),
                snapshot(WatchStatus::Ready, &[("api", "y"), ("api", "z")]),
            ),
        ]);
        assert_eq!(names(&merged), ["api/y", "api/z", "web/a", "web/c"]);
    }

    #[test]
    fn the_same_list_twice_or_with_empty_ones_is_shared_not_copied() {
        let part = snapshot(WatchStatus::Ready, &[("web", "a"), ("web", "b")]);
        let merged = merge([
            (Some("web".into()), part.clone()),
            (Some("api".into()), snapshot(WatchStatus::Ready, &[])),
            (Some("web".into()), part.clone()),
        ]);
        assert!(Arc::ptr_eq(&merged.objects, &part.objects));
    }

    #[test]
    fn of_equal_objects_the_one_of_the_first_part_is_kept() {
        let all = snapshot(WatchStatus::Ready, &[("api", "a"), ("web", "a")]);
        let web = snapshot(WatchStatus::Ready, &[("web", "a"), ("web", "b")]);
        let merged = merge([(None, all.clone()), (Some("web".into()), web.clone())]);
        assert_eq!(names(&merged), ["api/a", "web/a", "web/b"]);
        assert!(Arc::ptr_eq(&merged.objects[1], &all.objects[1]));
    }

    #[test]
    fn unsorted_parts_are_sorted() {
        let merged = merge([
            (
                Some("web".into()),
                snapshot(WatchStatus::Ready, &[("web", "c"), ("web", "a")]),
            ),
            (Some("api".into()), snapshot(WatchStatus::Ready, &[])),
        ]);
        assert_eq!(names(&merged), ["web/a", "web/c"]);
    }
}
