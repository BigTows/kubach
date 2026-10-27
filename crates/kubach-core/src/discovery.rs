//! API discovery and dynamic access to any resource kind, including CRDs.

use std::collections::BTreeMap;

use futures::{Stream, StreamExt as _};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::api::{ApiResource, DynamicObject, GroupVersionKind, ListParams};
use kube::core::{PartialObjectMeta, Request, Resource as _};
use kube::discovery::{ApiCapabilities, Discovery, Scope, verbs};
use kube::runtime::watcher;
use kube::{Api, Client};
use serde::{Deserialize, Serialize};

pub mod cache;

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("API discovery failed: {0}")]
    Discovery(#[source] Box<kube::Error>),
    #[error("failed to list {kind}: {source}")]
    List {
        kind: String,
        #[source]
        source: Box<kube::Error>,
    },
    #[error("failed to build request: {0}")]
    Request(String),
}

/// A resource kind served by the cluster.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceKind {
    pub api: ApiResource,
    pub namespaced: bool,
    pub verbs: Vec<String>,
}

impl ResourceKind {
    pub fn new(api: ApiResource, capabilities: &ApiCapabilities) -> Self {
        Self {
            api,
            namespaced: capabilities.scope == Scope::Namespaced,
            verbs: capabilities.operations.clone(),
        }
    }

    pub fn gvk(&self) -> GroupVersionKind {
        GroupVersionKind::gvk(&self.api.group, &self.api.version, &self.api.kind)
    }

    pub fn supports(&self, verb: &str) -> bool {
        self.verbs.iter().any(|v| v == verb)
    }

    /// Served by Kubernetes itself rather than a CRD or an aggregated API
    /// from a third party. Built-in groups are either dotless (`apps`,
    /// `batch`) or end in `.k8s.io`.
    pub fn is_builtin(&self) -> bool {
        let group = self.api.group.as_str();
        !group.contains('.') || group.ends_with(".k8s.io")
    }

    /// The group as shown in the UI; the core group has an empty name.
    pub fn group_label(&self) -> &str {
        if self.api.group.is_empty() {
            "core"
        } else {
            &self.api.group
        }
    }
}

/// Sets `apiVersion` / `kind` of `object` from `api` when it has none.
///
/// The API server leaves them out of list items (`kubectl get --raw` shows
/// items without `kind`), so objects from the initial list of a watch come
/// untyped while watch events carry their type. Code that reads the kind
/// from the object (port-forward, delete through discovery, topology)
/// then misses it (#372).
pub fn fill_types(object: &mut DynamicObject, api: &ApiResource) {
    if object.types.is_none() {
        object.types = Some(kube::api::TypeMeta {
            api_version: api.api_version.clone(),
            kind: api.kind.clone(),
        });
    }
}

/// `object` with its type set from `api` (see [`fill_types`]); the same
/// `Arc` when it already has one.
pub fn typed(
    object: std::sync::Arc<DynamicObject>,
    api: &ApiResource,
) -> std::sync::Arc<DynamicObject> {
    if object.types.is_some() {
        return object;
    }
    let mut object = (*object).clone();
    fill_types(&mut object, api);
    std::sync::Arc::new(object)
}

/// Every listable kind the cluster serves, at its preferred version.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ApiCatalog {
    kinds: Vec<ResourceKind>,
}

impl ApiCatalog {
    /// Runs discovery against the cluster. Uses aggregated discovery (one
    /// request, Kubernetes 1.30+) and falls back to per-group discovery.
    pub async fn discover(client: &Client) -> Result<Self, DiscoveryError> {
        let discovery = match Discovery::new(client.clone()).run_aggregated().await {
            Ok(discovery) => discovery,
            Err(_) => Discovery::new(client.clone())
                .run()
                .await
                .map_err(|error| DiscoveryError::Discovery(Box::new(error)))?,
        };
        let resources = discovery
            .groups()
            .flat_map(|group| group.recommended_resources());
        Ok(Self::from_resources(resources))
    }

    /// Builds a catalog from discovered resources, keeping only kinds that
    /// can be listed, sorted by group and kind.
    pub fn from_resources(
        resources: impl IntoIterator<Item = (ApiResource, ApiCapabilities)>,
    ) -> Self {
        let mut kinds: Vec<_> = resources
            .into_iter()
            .map(|(api, capabilities)| ResourceKind::new(api, &capabilities))
            .filter(|kind| kind.supports(verbs::LIST))
            .collect();
        kinds.sort_by(|a, b| {
            (a.api.group.as_str(), a.api.kind.as_str())
                .cmp(&(b.api.group.as_str(), b.api.kind.as_str()))
        });
        Self { kinds }
    }

    pub fn kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }

    pub fn find(&self, group: &str, kind: &str) -> Option<&ResourceKind> {
        self.kinds
            .iter()
            .find(|k| k.api.group == group && k.api.kind == kind)
    }

    /// Kinds grouped by API group, groups in alphabetical order.
    pub fn by_group(&self) -> BTreeMap<&str, Vec<&ResourceKind>> {
        let mut groups: BTreeMap<&str, Vec<&ResourceKind>> = BTreeMap::new();
        for kind in &self.kinds {
            groups.entry(kind.group_label()).or_default().push(kind);
        }
        groups
    }

    pub fn custom_kinds(&self) -> impl Iterator<Item = &ResourceKind> {
        self.kinds.iter().filter(|kind| !kind.is_builtin())
    }
}

/// `Api` handle for a kind: namespaced when `namespace` is given and the
/// kind is namespaced, cluster-wide otherwise.
pub fn api(client: &Client, kind: &ResourceKind, namespace: Option<&str>) -> Api<DynamicObject> {
    match namespace {
        Some(namespace) if kind.namespaced => {
            Api::namespaced_with(client.clone(), namespace, &kind.api)
        }
        _ => Api::all_with(client.clone(), &kind.api),
    }
}

/// Lists objects of any kind.
pub async fn list(
    client: &Client,
    kind: &ResourceKind,
    namespace: Option<&str>,
) -> Result<Vec<DynamicObject>, DiscoveryError> {
    let list = api(client, kind, namespace)
        .list(&ListParams::default())
        .await
        .map_err(|source| DiscoveryError::List {
            kind: kind.api.kind.clone(),
            source: Box::new(source),
        })?;
    Ok(list.items)
}

/// Server-side table (`kubectl get` columns) for any kind.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Table {
    pub column_definitions: Vec<TableColumn>,
    #[serde(default)]
    pub rows: Vec<TableRow>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TableColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub description: String,
    /// 0 for columns `kubectl get` shows by default, higher for `-o wide`.
    #[serde(default)]
    pub priority: i32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TableRow {
    pub cells: Vec<serde_json::Value>,
}

const TABLE_ACCEPT: &str = "application/json;as=Table;v=v1;g=meta.k8s.io";

/// Lists objects as the server renders them for `kubectl get`.
pub async fn list_table(
    client: &Client,
    kind: &ResourceKind,
    namespace: Option<&str>,
) -> Result<Table, DiscoveryError> {
    let namespace = namespace.filter(|_| kind.namespaced);
    let url = DynamicObject::url_path(&kind.api, namespace);
    let mut request = Request::new(url)
        .list(&ListParams::default())
        .map_err(|error| DiscoveryError::Request(error.to_string()))?;
    request.headers_mut().insert(
        http::header::ACCEPT,
        http::HeaderValue::from_static(TABLE_ACCEPT),
    );
    client
        .request(request)
        .await
        .map_err(|source| DiscoveryError::List {
            kind: kind.api.kind.clone(),
            source: Box::new(source),
        })
}

/// Yields once every time a CRD is added, changed or removed, so discovery
/// can be refreshed. The initial listing does not count as a change.
pub fn crd_changes(client: &Client) -> impl Stream<Item = ()> + Send + use<> {
    // Metadata only: full CRDs carry large OpenAPI schemas.
    let api = Api::<PartialObjectMeta<CustomResourceDefinition>>::all(client.clone());
    watcher::watcher(api, watcher::Config::default()).filter_map(|event| async move {
        match event {
            Ok(watcher::Event::Apply(_) | watcher::Event::Delete(_)) => Some(()),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn fills_missing_types_from_the_kind() {
        let api = kube::api::ApiResource::from_gvk(&kube::api::GroupVersionKind::gvk(
            "apps",
            "v1",
            "Deployment",
        ));
        let untyped: kube::api::DynamicObject =
            serde_json::from_value(serde_json::json!({"metadata": {"name": "web"}})).unwrap();
        assert!(untyped.types.is_none());
        let filled = super::typed(std::sync::Arc::new(untyped), &api);
        let types = filled.types.as_ref().unwrap();
        assert_eq!(
            (types.api_version.as_str(), types.kind.as_str()),
            ("apps/v1", "Deployment")
        );

        // A typed object is kept as is.
        let same = super::typed(filled.clone(), &api);
        assert!(std::sync::Arc::ptr_eq(&same, &filled));
    }

    use super::*;

    fn resource(
        group: &str,
        kind: &str,
        plural: &str,
        scope: Scope,
        ops: &[&str],
    ) -> (ApiResource, ApiCapabilities) {
        let gvk = GroupVersionKind::gvk(group, "v1", kind);
        (
            ApiResource::from_gvk_with_plural(&gvk, plural),
            ApiCapabilities {
                scope,
                subresources: Vec::new(),
                operations: ops.iter().map(|op| op.to_string()).collect(),
            },
        )
    }

    fn catalog() -> ApiCatalog {
        let list = &[verbs::LIST, verbs::WATCH, verbs::GET];
        ApiCatalog::from_resources([
            resource("apps", "Deployment", "deployments", Scope::Namespaced, list),
            resource("", "Pod", "pods", Scope::Namespaced, list),
            resource("", "Node", "nodes", Scope::Cluster, list),
            resource(
                "cert-manager.io",
                "Certificate",
                "certificates",
                Scope::Namespaced,
                list,
            ),
            resource(
                "networking.k8s.io",
                "Ingress",
                "ingresses",
                Scope::Namespaced,
                list,
            ),
            resource(
                "authorization.k8s.io",
                "SelfSubjectReview",
                "selfsubjectreviews",
                Scope::Cluster,
                &[verbs::CREATE],
            ),
        ])
    }

    #[test]
    fn keeps_only_listable_kinds_sorted() {
        let kinds: Vec<_> = catalog()
            .kinds()
            .iter()
            .map(|k| k.api.kind.clone())
            .collect();
        assert_eq!(
            kinds,
            ["Node", "Pod", "Deployment", "Certificate", "Ingress"]
        );
    }

    #[test]
    fn groups_core_under_its_label() {
        let catalog = catalog();
        let groups = catalog.by_group();
        let names: Vec<_> = groups.keys().copied().collect();
        assert_eq!(
            names,
            ["apps", "cert-manager.io", "core", "networking.k8s.io"]
        );
        assert_eq!(groups["core"].len(), 2);
    }

    #[test]
    fn detects_custom_kinds() {
        let catalog = catalog();
        let custom: Vec<_> = catalog
            .custom_kinds()
            .map(|k| k.api.kind.as_str())
            .collect();
        assert_eq!(custom, ["Certificate"]);
    }

    #[test]
    fn finds_kind_and_scope() {
        let catalog = catalog();
        assert!(catalog.find("", "Pod").unwrap().namespaced);
        assert!(!catalog.find("", "Node").unwrap().namespaced);
        assert_eq!(
            catalog.find("apps", "Deployment").unwrap().gvk().version,
            "v1"
        );
        assert!(catalog.find("apps", "StatefulSet").is_none());
    }

    #[test]
    fn parses_server_side_table() {
        let json = r#"{
            "kind": "Table",
            "apiVersion": "meta.k8s.io/v1",
            "columnDefinitions": [
                {"name": "Name", "type": "string", "format": "name", "description": "Name", "priority": 0},
                {"name": "Ready", "type": "string", "format": "", "description": "Ready", "priority": 0},
                {"name": "IP", "type": "string", "format": "", "description": "Pod IP", "priority": 1}
            ],
            "rows": [
                {"cells": ["nginx-1", "1/1", "10.0.0.1"], "object": {"kind": "PartialObjectMetadata"}}
            ]
        }"#;
        let table: Table = serde_json::from_str(json).unwrap();
        assert_eq!(table.column_definitions.len(), 3);
        assert_eq!(table.column_definitions[2].priority, 1);
        assert_eq!(table.rows[0].cells[1], "1/1");
    }

    #[test]
    fn table_url_respects_scope() {
        let catalog = catalog();
        let node = catalog.find("", "Node").unwrap();
        let pod = catalog.find("", "Pod").unwrap();
        let ns = node.namespaced.then_some("web");
        assert_eq!(DynamicObject::url_path(&node.api, ns), "/api/v1/nodes");
        assert_eq!(
            DynamicObject::url_path(&pod.api, Some("web")),
            "/api/v1/namespaces/web/pods"
        );
    }
}
