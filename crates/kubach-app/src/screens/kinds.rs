//! Which resource kinds each browser section shows.

use kubach_core::{ApiCatalog, ResourceKind};

/// A sidebar section that lists resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BrowseSection {
    Cluster,
    Workloads,
    Network,
    Storage,
    Config,
    AccessControl,
    /// Gateway API (CRDs): shown only when the cluster serves them.
    GatewayApi,
    Custom,
}

/// A built-in kind a section offers when the cluster serves it.
struct Wanted {
    group: &'static str,
    kind: &'static str,
    title: &'static str,
}

const fn wanted(group: &'static str, kind: &'static str, title: &'static str) -> Wanted {
    Wanted { group, kind, title }
}

const CLUSTER: &[Wanted] = &[
    wanted("", "Node", "Nodes"),
    wanted("", "Namespace", "Namespaces"),
    wanted("scheduling.k8s.io", "PriorityClass", "PriorityClasses"),
    wanted("node.k8s.io", "RuntimeClass", "RuntimeClasses"),
    wanted(
        "apiextensions.k8s.io",
        "CustomResourceDefinition",
        "CustomResourceDefinitions",
    ),
    wanted(
        "admissionregistration.k8s.io",
        "MutatingWebhookConfiguration",
        "MutatingWebhooks",
    ),
    wanted(
        "admissionregistration.k8s.io",
        "ValidatingWebhookConfiguration",
        "ValidatingWebhooks",
    ),
];

const WORKLOADS: &[Wanted] = &[
    wanted("", "Pod", "Pods"),
    wanted("apps", "Deployment", "Deployments"),
    wanted("apps", "StatefulSet", "StatefulSets"),
    wanted("apps", "DaemonSet", "DaemonSets"),
    wanted("apps", "ReplicaSet", "ReplicaSets"),
    wanted("batch", "Job", "Jobs"),
    wanted("batch", "CronJob", "CronJobs"),
    wanted(
        "autoscaling",
        "HorizontalPodAutoscaler",
        "HorizontalPodAutoscalers",
    ),
    wanted("policy", "PodDisruptionBudget", "PodDisruptionBudgets"),
    // A CRD (#294): listed only when the VPA is installed.
    wanted(
        "autoscaling.k8s.io",
        "VerticalPodAutoscaler",
        "VerticalPodAutoscalers",
    ),
];

const NETWORK: &[Wanted] = &[
    wanted("", "Service", "Services"),
    wanted("networking.k8s.io", "Ingress", "Ingresses"),
    wanted("networking.k8s.io", "IngressClass", "IngressClasses"),
    wanted("networking.k8s.io", "NetworkPolicy", "NetworkPolicies"),
    wanted("discovery.k8s.io", "EndpointSlice", "EndpointSlices"),
];

const STORAGE: &[Wanted] = &[
    wanted("", "PersistentVolumeClaim", "PersistentVolumeClaims"),
    wanted("", "PersistentVolume", "PersistentVolumes"),
    wanted("storage.k8s.io", "StorageClass", "StorageClasses"),
    wanted("storage.k8s.io", "VolumeAttachment", "VolumeAttachments"),
    wanted("storage.k8s.io", "CSIDriver", "CSIDrivers"),
];

const CONFIG: &[Wanted] = &[
    wanted("", "ConfigMap", "ConfigMaps"),
    wanted("", "Secret", "Secrets"),
    wanted("", "ResourceQuota", "ResourceQuotas"),
    wanted("", "LimitRange", "LimitRanges"),
    wanted("coordination.k8s.io", "Lease", "Leases"),
];

/// Gateway API kinds; CRDs, so each shows up only when installed.
const GATEWAY_API: &[Wanted] = &[
    wanted(GATEWAY, "GatewayClass", "GatewayClasses"),
    wanted(GATEWAY, "Gateway", "Gateways"),
    wanted(GATEWAY, "HTTPRoute", "HTTPRoutes"),
    wanted(GATEWAY, "GRPCRoute", "GRPCRoutes"),
    wanted(GATEWAY, "TLSRoute", "TLSRoutes"),
    wanted(GATEWAY, "TCPRoute", "TCPRoutes"),
    wanted(GATEWAY, "UDPRoute", "UDPRoutes"),
    wanted(GATEWAY, "ReferenceGrant", "ReferenceGrants"),
    wanted(GATEWAY, "BackendTLSPolicy", "BackendTLSPolicies"),
];

const GATEWAY: &str = kubach_core::plugin::builtin::gateway::GROUP;

const ACCESS_CONTROL: &[Wanted] = &[
    wanted("", "ServiceAccount", "ServiceAccounts"),
    wanted("rbac.authorization.k8s.io", "Role", "Roles"),
    wanted("rbac.authorization.k8s.io", "RoleBinding", "RoleBindings"),
    wanted("rbac.authorization.k8s.io", "ClusterRole", "ClusterRoles"),
    wanted(
        "rbac.authorization.k8s.io",
        "ClusterRoleBinding",
        "ClusterRoleBindings",
    ),
];

/// A kind as offered in a section: the served kind plus a tab title.
#[derive(Clone, Debug)]
pub struct KindEntry {
    pub kind: ResourceKind,
    pub title: String,
}

impl KindEntry {
    /// `(group, kind)`, stable across API versions.
    pub fn id(&self) -> (String, String) {
        (self.kind.api.group.clone(), self.kind.api.kind.clone())
    }
}

impl BrowseSection {
    /// Sections with built-in kinds, in sidebar order.
    pub const BUILTIN: [BrowseSection; 7] = [
        BrowseSection::Cluster,
        BrowseSection::Workloads,
        BrowseSection::Network,
        BrowseSection::GatewayApi,
        BrowseSection::Storage,
        BrowseSection::Config,
        BrowseSection::AccessControl,
    ];

    fn wanted(self) -> &'static [Wanted] {
        match self {
            BrowseSection::Cluster => CLUSTER,
            BrowseSection::Workloads => WORKLOADS,
            BrowseSection::Network => NETWORK,
            BrowseSection::Storage => STORAGE,
            BrowseSection::Config => CONFIG,
            BrowseSection::AccessControl => ACCESS_CONTROL,
            BrowseSection::GatewayApi => GATEWAY_API,
            BrowseSection::Custom => &[],
        }
    }

    /// Kinds of this section that `catalog` serves, in display order.
    /// Custom resources are sorted by group, then kind.
    pub fn kinds(self, catalog: &ApiCatalog) -> Vec<KindEntry> {
        if self == BrowseSection::Custom {
            return catalog
                .custom_kinds()
                .map(|kind| KindEntry {
                    title: kind.api.kind.clone(),
                    kind: kind.clone(),
                })
                .collect();
        }
        self.wanted()
            .iter()
            .filter_map(|wanted| {
                catalog
                    .find(wanted.group, wanted.kind)
                    .map(|kind| KindEntry {
                        kind: kind.clone(),
                        title: wanted.title.to_owned(),
                    })
            })
            .collect()
    }
}

/// The section and entry of a served kind shown in the sidebar.
pub fn find(catalog: &ApiCatalog, group: &str, kind: &str) -> Option<(BrowseSection, KindEntry)> {
    let served = catalog.find(group, kind)?;
    for section in BrowseSection::BUILTIN {
        if let Some(wanted) = section
            .wanted()
            .iter()
            .find(|wanted| wanted.group == group && wanted.kind == kind)
        {
            let entry = KindEntry {
                kind: served.clone(),
                title: wanted.title.to_owned(),
            };
            return Some((section, entry));
        }
    }
    (!served.is_builtin()).then(|| {
        let entry = KindEntry {
            kind: served.clone(),
            title: served.api.kind.clone(),
        };
        (BrowseSection::Custom, entry)
    })
}

/// Title of `kind` as the sidebar shows it: the built-in plural, else the
/// kind's own name (custom resources).
pub fn title_of(kind: &ResourceKind) -> String {
    BrowseSection::BUILTIN
        .iter()
        .flat_map(|section| section.wanted())
        .find(|wanted| wanted.group == kind.api.group && wanted.kind == kind.api.kind)
        .map_or_else(|| kind.api.kind.clone(), |wanted| wanted.title.to_owned())
}

/// Custom resource kinds grouped by API group, in catalog order.
pub fn custom_groups(catalog: &ApiCatalog) -> Vec<(String, Vec<KindEntry>)> {
    let mut groups: Vec<(String, Vec<KindEntry>)> = Vec::new();
    for entry in BrowseSection::Custom.kinds(catalog) {
        let group = entry.kind.group_label().to_owned();
        match groups.last_mut() {
            Some((last, entries)) if *last == group => entries.push(entry),
            _ => groups.push((group, vec![entry])),
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use kube::api::{ApiResource, GroupVersionKind};
    use kube::discovery::{ApiCapabilities, Scope, verbs};

    use super::*;

    fn catalog(kinds: &[(&str, &str)]) -> ApiCatalog {
        ApiCatalog::from_resources(kinds.iter().map(|(group, kind)| {
            let gvk = GroupVersionKind::gvk(group, "v1", kind);
            (
                ApiResource::from_gvk(&gvk),
                ApiCapabilities {
                    scope: Scope::Namespaced,
                    subresources: Vec::new(),
                    operations: vec![verbs::LIST.into(), verbs::WATCH.into()],
                },
            )
        }))
    }

    fn titles(entries: &[KindEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.title.as_str()).collect()
    }

    #[test]
    fn shows_only_served_kinds_in_section_order() {
        let catalog = catalog(&[
            ("batch", "Job"),
            ("apps", "Deployment"),
            ("", "Pod"),
            ("", "Secret"),
            ("", "ConfigMap"),
        ]);
        assert_eq!(
            titles(&BrowseSection::Workloads.kinds(&catalog)),
            ["Pods", "Deployments", "Jobs"]
        );
        assert_eq!(
            titles(&BrowseSection::Config.kinds(&catalog)),
            ["ConfigMaps", "Secrets"]
        );
        assert!(BrowseSection::Network.kinds(&catalog).is_empty());
    }

    #[test]
    fn access_control_lists_rbac_kinds() {
        let rbac = "rbac.authorization.k8s.io";
        let catalog = catalog(&[
            (rbac, "ClusterRole"),
            (rbac, "RoleBinding"),
            (rbac, "Role"),
            ("", "ServiceAccount"),
        ]);
        assert_eq!(
            titles(&BrowseSection::AccessControl.kinds(&catalog)),
            ["ServiceAccounts", "Roles", "RoleBindings", "ClusterRoles"]
        );
        let (section, _) = find(&catalog, rbac, "Role").unwrap();
        assert_eq!(section, BrowseSection::AccessControl);
    }

    #[test]
    fn coverage_kinds_have_sections() {
        let gateway = "gateway.networking.k8s.io";
        let catalog = catalog(&[
            ("autoscaling", "HorizontalPodAutoscaler"),
            ("policy", "PodDisruptionBudget"),
            ("", "ResourceQuota"),
            ("", "LimitRange"),
            ("scheduling.k8s.io", "PriorityClass"),
            ("networking.k8s.io", "IngressClass"),
            (gateway, "HTTPRoute"),
            (gateway, "Gateway"),
            (gateway, "GatewayClass"),
        ]);
        for (group, kind, section) in [
            (
                "autoscaling",
                "HorizontalPodAutoscaler",
                BrowseSection::Workloads,
            ),
            ("policy", "PodDisruptionBudget", BrowseSection::Workloads),
            ("", "ResourceQuota", BrowseSection::Config),
            ("", "LimitRange", BrowseSection::Config),
            ("scheduling.k8s.io", "PriorityClass", BrowseSection::Cluster),
            ("networking.k8s.io", "IngressClass", BrowseSection::Network),
            (gateway, "HTTPRoute", BrowseSection::GatewayApi),
        ] {
            assert_eq!(find(&catalog, group, kind).unwrap().0, section, "{kind}");
        }
        assert_eq!(
            titles(&BrowseSection::GatewayApi.kinds(&catalog)),
            ["GatewayClasses", "Gateways", "HTTPRoutes"]
        );
        // Gateway API kinds are not custom resources: their group ends in .k8s.io.
        assert!(BrowseSection::Custom.kinds(&catalog).is_empty());
        // Without the CRDs the section is empty (and hidden).
        let plain = self::catalog(&[("", "Pod")]);
        assert!(BrowseSection::GatewayApi.kinds(&plain).is_empty());
    }

    #[test]
    fn vpa_is_a_workload_when_installed() {
        let catalog = catalog(&[
            ("autoscaling.k8s.io", "VerticalPodAutoscaler"),
            ("apiextensions.k8s.io", "CustomResourceDefinition"),
        ]);
        let (section, entry) =
            find(&catalog, "autoscaling.k8s.io", "VerticalPodAutoscaler").unwrap();
        assert_eq!(section, BrowseSection::Workloads);
        assert_eq!(entry.title, "VerticalPodAutoscalers");
        assert_eq!(
            find(&catalog, "apiextensions.k8s.io", "CustomResourceDefinition")
                .unwrap()
                .0,
            BrowseSection::Cluster
        );
        // Not a custom resource: the group ends in .k8s.io.
        assert!(BrowseSection::Custom.kinds(&catalog).is_empty());
        let plain = self::catalog(&[("", "Pod")]);
        assert!(find(&plain, "autoscaling.k8s.io", "VerticalPodAutoscaler").is_none());
    }

    #[test]
    fn custom_section_lists_crd_kinds() {
        let catalog = catalog(&[
            ("", "Pod"),
            ("cert-manager.io", "Certificate"),
            ("acme.cert-manager.io", "Order"),
            ("networking.k8s.io", "Ingress"),
        ]);
        let custom = BrowseSection::Custom.kinds(&catalog);
        assert_eq!(titles(&custom), ["Order", "Certificate"]);
        assert_eq!(
            custom[1].id(),
            ("cert-manager.io".to_owned(), "Certificate".to_owned())
        );
    }

    #[test]
    fn finds_sections_and_groups_crds() {
        let catalog = catalog(&[
            ("", "Pod"),
            ("", "Namespace"),
            ("cert-manager.io", "Certificate"),
            ("cert-manager.io", "Issuer"),
            ("acme.cert-manager.io", "Order"),
        ]);
        let (section, entry) = find(&catalog, "", "Pod").unwrap();
        assert_eq!(
            (section, entry.title.as_str()),
            (BrowseSection::Workloads, "Pods")
        );
        let (section, _) = find(&catalog, "cert-manager.io", "Issuer").unwrap();
        assert_eq!(section, BrowseSection::Custom);
        // Namespaces are listed under Cluster (#277).
        let (section, entry) = find(&catalog, "", "Namespace").unwrap();
        assert_eq!(
            (section, entry.title.as_str()),
            (BrowseSection::Cluster, "Namespaces")
        );
        assert_eq!(
            titles(&BrowseSection::Cluster.kinds(&catalog)),
            ["Namespaces"]
        );
        // Not served at all.
        assert!(find(&catalog, "apps", "Deployment").is_none());

        let groups: Vec<_> = custom_groups(&catalog)
            .into_iter()
            .map(|(group, entries)| (group, titles(&entries).join(",")))
            .collect();
        assert_eq!(
            groups,
            [
                ("acme.cert-manager.io".to_owned(), "Order".to_owned()),
                (
                    "cert-manager.io".to_owned(),
                    "Certificate,Issuer".to_owned()
                ),
            ]
        );
    }
}
