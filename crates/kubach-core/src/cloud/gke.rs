//! Google GKE through `gcloud`: the signed-in account, projects, and
//! clusters of each project (`gcloud container clusters list`).

use std::path::Path;

use serde_json::Value;

use super::{
    CliCall, CloudCluster, CloudError, Discovery, Endpoint, EndpointKind, Provider, Scope, Target,
    dedup_endpoints, https, parse_json, run, text,
};

/// `gcloud auth list --format=json`.
pub fn accounts_call() -> CliCall {
    CliCall::new(["auth", "list", "--format=json"])
}

/// `gcloud projects list --format=json`.
pub fn projects_call() -> CliCall {
    CliCall::new(["projects", "list", "--format=json", "--sort-by=projectId"])
}

/// `gcloud container clusters list --project <id> --format=json`.
pub fn clusters_call(project: &str) -> CliCall {
    CliCall::new([
        "container",
        "clusters",
        "list",
        "--project",
        project,
        "--format=json",
    ])
}

/// The active account in `gcloud auth list` output; `Ok(None)` when no
/// account is signed in.
pub fn parse_accounts(json: &str) -> Result<Option<String>, CloudError> {
    let value = parse_json(&accounts_call(), json)?;
    let accounts = value.as_array().cloned().unwrap_or_default();
    Ok(accounts
        .iter()
        .find(|a| a.get("status").and_then(Value::as_str) == Some("ACTIVE"))
        .and_then(|a| text(a, "/account")))
}

/// Active projects in `gcloud projects list` output.
pub fn parse_projects(json: &str) -> Result<Vec<Scope>, CloudError> {
    let value = parse_json(&projects_call(), json)?;
    let projects = value.as_array().cloned().unwrap_or_default();
    Ok(projects
        .iter()
        .filter(|p| text(p, "/lifecycleState").is_none_or(|state| state == "ACTIVE"))
        .filter_map(|p| {
            let id = text(p, "/projectId")?;
            let label = match text(p, "/name") {
                Some(name) if name != id => format!("{name} ({id})"),
                _ => id.clone(),
            };
            Some(Scope { id, label })
        })
        .collect())
}

/// Clusters in `gcloud container clusters list` output for `project`.
pub fn parse_clusters(json: &str, project: &Scope) -> Result<Vec<CloudCluster>, CloudError> {
    let value = parse_json(&clusters_call(&project.id), json)?;
    let clusters = value.as_array().cloned().unwrap_or_default();
    Ok(clusters
        .iter()
        .filter_map(|c| cluster(c, project))
        .collect())
}

fn cluster(c: &Value, project: &Scope) -> Option<CloudCluster> {
    let name = text(c, "/name")?;
    let location = text(c, "/location").or_else(|| text(c, "/zone"))?;
    let private_only = c
        .pointer("/privateClusterConfig/enablePrivateEndpoint")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let public_disabled = c
        .pointer("/controlPlaneEndpointsConfig/ipEndpointsConfig/enablePublicEndpoint")
        .and_then(Value::as_bool)
        == Some(false);
    let mut endpoints = Vec::new();
    let public = text(
        c,
        "/controlPlaneEndpointsConfig/ipEndpointsConfig/publicEndpoint",
    )
    .or_else(|| text(c, "/privateClusterConfig/publicEndpoint"))
    .or_else(|| text(c, "/endpoint"));
    if let Some(public) = public.filter(|_| !private_only && !public_disabled) {
        endpoints.push(Endpoint {
            kind: EndpointKind::External,
            server: https(&public),
        });
    }
    if let Some(private) = text(c, "/privateClusterConfig/privateEndpoint").or_else(|| {
        text(
            c,
            "/controlPlaneEndpointsConfig/ipEndpointsConfig/privateEndpoint",
        )
    }) {
        endpoints.push(Endpoint {
            kind: EndpointKind::Internal,
            server: https(&private),
        });
    }
    if let Some(dns) = text(c, "/controlPlaneEndpointsConfig/dnsEndpointConfig/endpoint") {
        endpoints.push(Endpoint {
            kind: EndpointKind::Dns,
            server: https(&dns),
        });
    }
    Some(CloudCluster {
        status: text(c, "/status").unwrap_or_default(),
        version: text(c, "/currentMasterVersion"),
        endpoints: dedup_endpoints(endpoints),
        ca_data: text(c, "/masterAuth/clusterCaCertificate"),
        target: Target::Gke {
            project: project.id.clone(),
            location: location.clone(),
        },
        scope: project.label.clone(),
        location,
        name,
    })
}

pub(super) async fn discover(program: &Path) -> Result<Discovery, CloudError> {
    let account = parse_accounts(&run(Provider::Gke, program, &accounts_call()).await?)?;
    let Some(account) = account else {
        return Err(CloudError::NotLoggedIn {
            message: "gcloud has no active account".into(),
        });
    };
    let scopes = parse_projects(&run(Provider::Gke, program, &projects_call()).await?)?;
    Ok(Discovery {
        account: Some(account),
        scopes,
        ..Discovery::default()
    })
}

pub(super) async fn clusters(
    program: &Path,
    project: &Scope,
) -> Result<Vec<CloudCluster>, CloudError> {
    let output = run(Provider::Gke, program, &clusters_call(&project.id)).await?;
    parse_clusters(&output, project)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROJECTS: &str = include_str!("fixtures/gcloud-projects.json");
    const CLUSTERS: &str = include_str!("fixtures/gcloud-clusters.json");

    fn project() -> Scope {
        Scope {
            id: "acme-prod".into(),
            label: "Acme Prod (acme-prod)".into(),
        }
    }

    #[test]
    fn calls() {
        assert_eq!(accounts_call().args, ["auth", "list", "--format=json"]);
        assert_eq!(
            clusters_call("acme-prod").args,
            [
                "container",
                "clusters",
                "list",
                "--project",
                "acme-prod",
                "--format=json"
            ]
        );
        assert_eq!(
            projects_call().args[..3],
            ["projects", "list", "--format=json"]
        );
    }

    #[test]
    fn accounts() {
        let json = r#"[{"account":"old@example.com","status":""},
                       {"account":"me@example.com","status":"ACTIVE"}]"#;
        assert_eq!(
            parse_accounts(json).unwrap().as_deref(),
            Some("me@example.com")
        );
        assert_eq!(parse_accounts("[]").unwrap(), None);
        assert!(matches!(
            parse_accounts("Listed 0 items."),
            Err(CloudError::Parse { .. })
        ));
    }

    #[test]
    fn projects() {
        let scopes = parse_projects(PROJECTS).unwrap();
        assert_eq!(
            scopes,
            [
                Scope {
                    id: "acme-prod".into(),
                    label: "Acme Prod (acme-prod)".into()
                },
                Scope {
                    id: "acme-sandbox".into(),
                    label: "acme-sandbox".into()
                },
            ]
        );
    }

    #[test]
    fn clusters_and_their_endpoints() {
        let clusters = parse_clusters(CLUSTERS, &project()).unwrap();
        assert_eq!(clusters.len(), 3);

        let public = &clusters[0];
        assert_eq!(public.name, "web");
        assert_eq!(public.location, "europe-west1");
        assert_eq!(public.status, "RUNNING");
        assert_eq!(public.version.as_deref(), Some("1.31.1-gke.1678000"));
        assert_eq!(
            public.ca_data.as_deref(),
            Some("LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0tCk1JSQ==")
        );
        assert_eq!(
            public.endpoints,
            [
                Endpoint {
                    kind: EndpointKind::External,
                    server: "https://34.76.10.20".into()
                },
                Endpoint {
                    kind: EndpointKind::Dns,
                    server: "https://gke-0123456789abcdef-123456789012.europe-west1.gke.goog"
                        .into()
                },
            ]
        );
        assert_eq!(public.context_name(), "gke_acme-prod_europe-west1_web");

        // Private nodes with a public endpoint: both addresses.
        let private_nodes = &clusters[1];
        assert_eq!(private_nodes.location, "europe-west1-b");
        let kinds: Vec<EndpointKind> = private_nodes.endpoints.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, [EndpointKind::External, EndpointKind::Internal]);
        assert_eq!(
            private_nodes
                .endpoint(EndpointKind::Internal)
                .unwrap()
                .server,
            "https://172.16.0.2"
        );

        // Private endpoint only: no external address.
        let private = &clusters[2];
        assert_eq!(
            private.endpoints,
            [Endpoint {
                kind: EndpointKind::Internal,
                server: "https://10.10.0.2".into()
            }]
        );
    }
}
