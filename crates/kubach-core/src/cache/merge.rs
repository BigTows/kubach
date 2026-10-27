//! Several subscriptions read as one list, e.g. a kind in a few namespaces.

use std::sync::Arc;

use futures::future::select_all;

use super::{Changes, Objects, Snapshot, Subscription, WatchError, WatchStatus};

/// Subscriptions whose objects are shown together. Keeps every watch alive
/// while it lives.
#[derive(Clone)]
pub struct SubscriptionSet {
    parts: Vec<Subscription>,
}

impl SubscriptionSet {
    pub fn new(parts: Vec<Subscription>) -> Self {
        Self { parts }
    }

    pub fn subscriptions(&self) -> &[Subscription] {
        &self.parts
    }

    /// Objects of every watch, merged and sorted by namespace and name.
    pub fn snapshot(&self) -> MergedSnapshot {
        merge(
            self.parts
                .iter()
                .map(|part| (part.key().namespace.clone(), part.snapshot())),
        )
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

/// Merges snapshots of watches keyed by namespace.
pub fn merge(parts: impl IntoIterator<Item = (Option<String>, Snapshot)>) -> MergedSnapshot {
    let parts: Vec<_> = parts.into_iter().collect();
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

    let objects = match parts.as_slice() {
        [] => Objects::from([]),
        [(_, only)] => Arc::clone(&only.objects),
        _ => {
            let mut all: Vec<_> = parts
                .iter()
                .flat_map(|(_, snapshot)| snapshot.objects.iter().cloned())
                .collect();
            let key = |object: &Arc<kube::api::DynamicObject>| {
                (
                    object.metadata.namespace.clone(),
                    object.metadata.name.clone(),
                )
            };
            all.sort_by_key(key);
            all.dedup_by_key(|object| key(object));
            all.into()
        }
    };
    MergedSnapshot {
        objects,
        status,
        errors,
    }
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
    use serde_json::json;

    use super::*;

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
}
