//! Which GitOps object manages a resource: the Argo CD `Application`, Flux
//! `Kustomization` or Flux `HelmRelease` named by its tracking labels and
//! annotations.
//!
//! [`managers`] reads the metadata only; [`resolve`] looks the candidates up
//! in the cluster, so a generic label like `app.kubernetes.io/instance` (also
//! set by plain Helm charts) only leads somewhere when such an Application
//! exists.
//!
//! [`overview`] turns the GitOps objects of a cluster into the rows and
//! details of the GitOps screen.

pub mod overview;

use std::collections::BTreeMap;

use kube::Client;
use kube::api::{ListParams, ObjectMeta};

use crate::discovery::{self, ApiCatalog};

/// Annotation-based Argo CD tracking: `<app>:<group>/<kind>:<namespace>/<name>`.
pub const ARGO_TRACKING_ID: &str = "argocd.argoproj.io/tracking-id";
/// Label-based Argo CD tracking with the recommended custom label key.
pub const ARGO_INSTANCE: &str = "argocd.argoproj.io/instance";
/// Argo CD's default tracking label; Helm charts set it to the release name too.
pub const APP_INSTANCE: &str = "app.kubernetes.io/instance";
pub const FLUX_KUSTOMIZATION_NAME: &str = "kustomize.toolkit.fluxcd.io/name";
pub const FLUX_KUSTOMIZATION_NAMESPACE: &str = "kustomize.toolkit.fluxcd.io/namespace";
pub const FLUX_HELM_NAME: &str = "helm.toolkit.fluxcd.io/name";
pub const FLUX_HELM_NAMESPACE: &str = "helm.toolkit.fluxcd.io/namespace";

/// Kind of a managing GitOps object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ManagerKind {
    ArgoApplication,
    FluxKustomization,
    FluxHelmRelease,
}

impl ManagerKind {
    pub fn group(self) -> &'static str {
        match self {
            Self::ArgoApplication => "argoproj.io",
            Self::FluxKustomization => "kustomize.toolkit.fluxcd.io",
            Self::FluxHelmRelease => "helm.toolkit.fluxcd.io",
        }
    }

    pub fn kind(self) -> &'static str {
        match self {
            Self::ArgoApplication => "Application",
            Self::FluxKustomization => "Kustomization",
            Self::FluxHelmRelease => "HelmRelease",
        }
    }

    /// Human name, e.g. "Argo CD Application".
    pub fn title(self) -> &'static str {
        match self {
            Self::ArgoApplication => "Argo CD Application",
            Self::FluxKustomization => "Flux Kustomization",
            Self::FluxHelmRelease => "Flux HelmRelease",
        }
    }
}

/// A managing object named by the metadata of a resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manager {
    pub kind: ManagerKind,
    pub name: String,
    /// `None` when the metadata does not say (Argo CD apps in the control
    /// plane namespace): [`resolve`] searches all namespaces.
    pub namespace: Option<String>,
    /// Label or annotation the reference comes from.
    pub via: &'static str,
    /// `false` for `app.kubernetes.io/instance`, which plain Helm charts set
    /// too: shown only when such an Application exists.
    pub certain: bool,
}

impl Manager {
    fn same_target(&self, other: &Manager) -> bool {
        self.kind == other.kind
            && self.name == other.name
            && (self.namespace == other.namespace
                || self.namespace.is_none()
                || other.namespace.is_none())
    }
}

/// Argo CD application reference: `name` or `namespace_name` (apps in any
/// namespace). Object names never contain `_`, so the split is unambiguous.
fn argo_app(value: &str) -> Option<(String, Option<String>)> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(match value.split_once('_') {
        Some((ns, name)) if !ns.is_empty() && !name.is_empty() => {
            (name.to_owned(), Some(ns.to_owned()))
        }
        _ => (value.to_owned(), None),
    })
}

/// Managers named by the labels and annotations of a resource, the most
/// specific first, without duplicates.
pub fn managers(meta: &ObjectMeta) -> Vec<Manager> {
    let empty = BTreeMap::new();
    let labels = meta.labels.as_ref().unwrap_or(&empty);
    let annotations = meta.annotations.as_ref().unwrap_or(&empty);
    let label = |key: &str| {
        labels
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    };

    let mut out: Vec<Manager> = Vec::new();
    let mut push = |manager: Manager| {
        if let Some(existing) = out.iter_mut().find(|m| m.same_target(&manager)) {
            // Keep the first source, but learn a namespace or certainty.
            if existing.namespace.is_none() {
                existing.namespace = manager.namespace;
            }
            existing.certain |= manager.certain;
        } else {
            out.push(manager);
        }
    };

    for (key, value) in [
        (
            FLUX_KUSTOMIZATION_NAME,
            label(FLUX_KUSTOMIZATION_NAME).map(|name| (name, label(FLUX_KUSTOMIZATION_NAMESPACE))),
        ),
        (
            FLUX_HELM_NAME,
            label(FLUX_HELM_NAME).map(|name| (name, label(FLUX_HELM_NAMESPACE))),
        ),
    ] {
        if let Some((name, namespace)) = value {
            push(Manager {
                kind: if key == FLUX_HELM_NAME {
                    ManagerKind::FluxHelmRelease
                } else {
                    ManagerKind::FluxKustomization
                },
                name: name.to_owned(),
                namespace: namespace.map(str::to_owned),
                via: key,
                certain: true,
            });
        }
    }

    let tracking = annotations
        .get(ARGO_TRACKING_ID)
        .and_then(|id| id.split_once(':'))
        .and_then(|(app, _)| argo_app(app));
    let argo = [
        (ARGO_TRACKING_ID, tracking, true),
        (ARGO_INSTANCE, label(ARGO_INSTANCE).and_then(argo_app), true),
        (APP_INSTANCE, label(APP_INSTANCE).and_then(argo_app), false),
    ];
    for (via, app, certain) in argo {
        if let Some((name, namespace)) = app {
            push(Manager {
                kind: ManagerKind::ArgoApplication,
                name,
                namespace,
                via,
                certain,
            });
        }
    }
    out
}

/// What the cluster says about a [`Manager`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkState {
    /// Exists in `namespace` (`None` for cluster-scoped).
    Found,
    /// Not in this cluster (Argo CD may run elsewhere, or it was deleted).
    Missing,
    /// The cluster does not serve the kind (CRD not installed).
    NotServed,
    /// The lookup failed, e.g. no permission to list the kind.
    Failed(String),
}

/// A manager with the result of looking it up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagerLink {
    pub manager: Manager,
    pub state: LinkState,
}

impl ManagerLink {
    pub fn is_found(&self) -> bool {
        self.state == LinkState::Found
    }
}

/// Turns the lookup of one manager into links: one per namespace found
/// when the namespace was unknown. Uncertain managers are dropped unless
/// found.
pub fn links_of(manager: Manager, lookup: Result<Vec<String>, LinkState>) -> Vec<ManagerLink> {
    let links = match lookup {
        Ok(namespaces) if namespaces.is_empty() => vec![ManagerLink {
            manager,
            state: LinkState::Missing,
        }],
        Ok(namespaces) => namespaces
            .into_iter()
            .map(|namespace| ManagerLink {
                manager: Manager {
                    namespace: Some(namespace),
                    ..manager.clone()
                },
                state: LinkState::Found,
            })
            .collect(),
        Err(state) => vec![ManagerLink { manager, state }],
    };
    links
        .into_iter()
        .filter(|link| link.manager.certain || link.is_found())
        .collect()
}

/// Looks the managers up in the cluster: by name in their namespace, or
/// across namespaces when it is unknown.
pub async fn resolve(
    client: &Client,
    catalog: &ApiCatalog,
    managers: Vec<Manager>,
) -> Vec<ManagerLink> {
    let mut out = Vec::new();
    for manager in managers {
        let lookup = match catalog.find(manager.kind.group(), manager.kind.kind()) {
            None => Err(LinkState::NotServed),
            Some(kind) => match &manager.namespace {
                Some(namespace) => discovery::api(client, kind, Some(namespace))
                    .get_metadata_opt(&manager.name)
                    .await
                    .map(|found| found.map(|_| namespace.clone()).into_iter().collect())
                    .map_err(|error| LinkState::Failed(error.to_string())),
                None => {
                    let params =
                        ListParams::default().fields(&format!("metadata.name={}", manager.name));
                    discovery::api(client, kind, None)
                        .list_metadata(&params)
                        .await
                        .map(|list| {
                            list.items
                                .into_iter()
                                .filter_map(|item| item.metadata.namespace)
                                .collect()
                        })
                        .map_err(|error| LinkState::Failed(error.to_string()))
                }
            },
        };
        out.extend(links_of(manager, lookup));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(labels: &[(&str, &str)], annotations: &[(&str, &str)]) -> ObjectMeta {
        let map = |pairs: &[(&str, &str)]| {
            (!pairs.is_empty()).then(|| {
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            })
        };
        ObjectMeta {
            name: Some("web".into()),
            namespace: Some("shop".into()),
            labels: map(labels),
            annotations: map(annotations),
            ..ObjectMeta::default()
        }
    }

    fn manager(
        kind: ManagerKind,
        name: &str,
        namespace: Option<&str>,
        via: &'static str,
        certain: bool,
    ) -> Manager {
        Manager {
            kind,
            name: name.into(),
            namespace: namespace.map(Into::into),
            via,
            certain,
        }
    }

    #[test]
    fn nothing_without_tracking_metadata() {
        assert!(managers(&ObjectMeta::default()).is_empty());
        assert!(managers(&meta(&[("app", "web")], &[("note", "x")])).is_empty());
    }

    #[test]
    fn flux_labels() {
        let m = managers(&meta(
            &[
                (FLUX_KUSTOMIZATION_NAME, "apps"),
                (FLUX_KUSTOMIZATION_NAMESPACE, "flux-system"),
            ],
            &[],
        ));
        assert_eq!(
            m,
            [manager(
                ManagerKind::FluxKustomization,
                "apps",
                Some("flux-system"),
                FLUX_KUSTOMIZATION_NAME,
                true
            )]
        );

        // A HelmRelease's objects; its chart sets app.kubernetes.io/instance
        // to the release name, which is not an Argo CD app.
        let m = managers(&meta(
            &[
                (FLUX_HELM_NAME, "podinfo"),
                (FLUX_HELM_NAMESPACE, "apps"),
                (APP_INSTANCE, "podinfo"),
            ],
            &[],
        ));
        assert_eq!(m.len(), 2);
        assert_eq!(
            m[0],
            manager(
                ManagerKind::FluxHelmRelease,
                "podinfo",
                Some("apps"),
                FLUX_HELM_NAME,
                true
            )
        );
        assert_eq!(m[1].kind, ManagerKind::ArgoApplication);
        assert!(!m[1].certain);

        // Name without namespace: the namespace is searched for.
        let m = managers(&meta(&[(FLUX_KUSTOMIZATION_NAME, "apps")], &[]));
        assert_eq!(m[0].namespace, None);
    }

    #[test]
    fn argo_tracking() {
        // Annotation tracking, apps in any namespace (`ns_name`).
        let m = managers(&meta(
            &[(APP_INSTANCE, "team-a_guestbook")],
            &[(
                ARGO_TRACKING_ID,
                "team-a_guestbook:apps/Deployment:shop/web",
            )],
        ));
        assert_eq!(
            m,
            [manager(
                ManagerKind::ArgoApplication,
                "guestbook",
                Some("team-a"),
                ARGO_TRACKING_ID,
                true
            )]
        );

        // The custom instance label is certain; the default one is not.
        let m = managers(&meta(&[(ARGO_INSTANCE, "guestbook")], &[]));
        assert_eq!(
            m,
            [manager(
                ManagerKind::ArgoApplication,
                "guestbook",
                None,
                ARGO_INSTANCE,
                true
            )]
        );
        let m = managers(&meta(&[(APP_INSTANCE, "guestbook")], &[]));
        assert_eq!(
            m,
            [manager(
                ManagerKind::ArgoApplication,
                "guestbook",
                None,
                APP_INSTANCE,
                false
            )]
        );

        // Same app by a less specific source learns certainty and namespace.
        let m = managers(&meta(
            &[
                (ARGO_INSTANCE, "guestbook"),
                (APP_INSTANCE, "argocd_guestbook"),
            ],
            &[],
        ));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].namespace.as_deref(), Some("argocd"));
        assert!(m[0].certain);
        assert_eq!(m[0].via, ARGO_INSTANCE);

        // Malformed values are ignored.
        assert!(managers(&meta(&[(ARGO_INSTANCE, " ")], &[(ARGO_TRACKING_ID, "x")])).is_empty());
    }

    #[test]
    fn argo_app_values() {
        assert_eq!(argo_app("app"), Some(("app".into(), None)));
        assert_eq!(argo_app("ns_app"), Some(("app".into(), Some("ns".into()))));
        assert_eq!(argo_app("_app"), Some(("_app".into(), None)));
        assert_eq!(argo_app(""), None);
    }

    #[test]
    fn lookup_results_to_links() {
        let certain = manager(
            ManagerKind::ArgoApplication,
            "guestbook",
            None,
            ARGO_INSTANCE,
            true,
        );
        let weak = Manager {
            certain: false,
            via: APP_INSTANCE,
            ..certain.clone()
        };

        let found = links_of(certain.clone(), Ok(vec!["argocd".into(), "team-a".into()]));
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(ManagerLink::is_found));
        assert_eq!(found[1].manager.namespace.as_deref(), Some("team-a"));

        let missing = links_of(certain.clone(), Ok(Vec::new()));
        assert_eq!(missing[0].state, LinkState::Missing);
        let failed = links_of(certain, Err(LinkState::Failed("forbidden".into())));
        assert_eq!(failed[0].state, LinkState::Failed("forbidden".into()));

        // A plain Helm release's instance label leads nowhere: dropped.
        assert!(links_of(weak.clone(), Ok(Vec::new())).is_empty());
        assert!(links_of(weak.clone(), Err(LinkState::NotServed)).is_empty());
        assert_eq!(links_of(weak, Ok(vec!["argocd".into()])).len(), 1);
    }
}
