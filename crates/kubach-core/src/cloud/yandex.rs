//! Yandex Cloud Managed Kubernetes through `yc`: profiles, clouds and
//! their folders, and clusters of each folder.

use std::path::Path;

use serde_json::Value;

use super::{
    CliCall, CloudCluster, CloudError, Discovery, Endpoint, EndpointKind, Provider, Scope, Target,
    dedup_endpoints, parse_json, pem_to_data, run, text,
};

/// `yc config profile list` (plain text: `name [ACTIVE]` per line).
pub fn profiles_call() -> CliCall {
    CliCall::new(["config", "profile", "list"])
}

fn with_profile(mut args: Vec<String>, profile: Option<&str>) -> CliCall {
    if let Some(profile) = profile.filter(|p| !p.is_empty()) {
        args.extend(["--profile".to_owned(), profile.to_owned()]);
    }
    CliCall { args }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_owned()).collect()
}

/// `yc resource-manager cloud list --format json [--profile p]`.
pub fn clouds_call(profile: Option<&str>) -> CliCall {
    with_profile(
        strings(&["resource-manager", "cloud", "list", "--format", "json"]),
        profile,
    )
}

/// `yc resource-manager folder list --cloud-id <id> --format json`.
pub fn folders_call(cloud: &str, profile: Option<&str>) -> CliCall {
    with_profile(
        strings(&[
            "resource-manager",
            "folder",
            "list",
            "--cloud-id",
            cloud,
            "--format",
            "json",
        ]),
        profile,
    )
}

/// `yc managed-kubernetes cluster list --folder-id <id> --format json`.
pub fn clusters_call(folder: &str, profile: Option<&str>) -> CliCall {
    with_profile(
        strings(&[
            "managed-kubernetes",
            "cluster",
            "list",
            "--folder-id",
            folder,
            "--format",
            "json",
        ]),
        profile,
    )
}

/// Profile names and the active one.
pub fn parse_profiles(output: &str) -> (Vec<String>, Option<String>) {
    let mut names = Vec::new();
    let mut active = None;
    for line in output.lines() {
        let mut words = line.split_whitespace();
        let Some(name) = words.next() else {
            continue;
        };
        if words.next() == Some("ACTIVE") {
            active = Some(name.to_owned());
        }
        names.push(name.to_owned());
    }
    (names, active)
}

/// `(id, name)` of each cloud.
pub fn parse_clouds(json: &str) -> Result<Vec<(String, String)>, CloudError> {
    let value = parse_json(&clouds_call(None), json)?;
    Ok(value
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|c| {
            let id = text(c, "/id")?;
            let name = text(c, "/name").unwrap_or_else(|| id.clone());
            Some((id, name))
        })
        .collect())
}

/// Active folders of the cloud named `cloud_name`, as scopes.
pub fn parse_folders(json: &str, cloud_name: &str) -> Result<Vec<Scope>, CloudError> {
    let value = parse_json(&folders_call("", None), json)?;
    Ok(value
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|f| text(f, "/status").is_none_or(|s| s == "ACTIVE"))
        .filter_map(|f| {
            let id = text(f, "/id")?;
            let name = text(f, "/name").unwrap_or_else(|| id.clone());
            Some(Scope {
                id,
                label: format!("{cloud_name} / {name}"),
            })
        })
        .collect())
}

/// Clusters in `yc managed-kubernetes cluster list` output.
pub fn parse_clusters(
    json: &str,
    folder: &Scope,
    profile: Option<&str>,
) -> Result<Vec<CloudCluster>, CloudError> {
    let value = parse_json(&clusters_call(&folder.id, profile), json)?;
    Ok(value
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|c| cluster(c, folder, profile))
        .collect())
}

fn cluster(c: &Value, folder: &Scope, profile: Option<&str>) -> Option<CloudCluster> {
    let id = text(c, "/id")?;
    let name = text(c, "/name").unwrap_or_else(|| id.clone());
    let location = text(c, "/master/zonal_master/zone_id")
        .or_else(|| text(c, "/master/regional_master/region_id"))
        .or_else(|| text(c, "/master/locations/0/zone_id"))
        .unwrap_or_default();
    let mut endpoints = Vec::new();
    if let Some(server) = text(c, "/master/endpoints/external_v4_endpoint") {
        endpoints.push(Endpoint {
            kind: EndpointKind::External,
            server,
        });
    }
    if let Some(server) = text(c, "/master/endpoints/internal_v4_endpoint") {
        endpoints.push(Endpoint {
            kind: EndpointKind::Internal,
            server,
        });
    }
    Some(CloudCluster {
        name,
        location,
        scope: folder.label.clone(),
        status: text(c, "/status").unwrap_or_default(),
        version: text(c, "/master/version"),
        endpoints: dedup_endpoints(endpoints),
        ca_data: text(c, "/master/master_auth/cluster_ca_certificate")
            .and_then(|pem| pem_to_data(&pem)),
        target: Target::Yandex {
            id,
            profile: profile.map(str::to_owned),
        },
    })
}

pub(super) async fn discover(
    program: &Path,
    profile: Option<&str>,
) -> Result<Discovery, CloudError> {
    let (profiles, active) = parse_profiles(&run(Provider::Yandex, program, &profiles_call()).await?);
    if profiles.is_empty() {
        return Err(CloudError::NotLoggedIn {
            message: "yc has no profiles".into(),
        });
    }
    let profile = profile.map(str::to_owned).or(active);
    let clouds = parse_clouds(&run(Provider::Yandex, program, &clouds_call(profile.as_deref())).await?)?;
    let mut scopes = Vec::new();
    for (id, name) in clouds {
        let output = run(
            Provider::Yandex,
            program,
            &folders_call(&id, profile.as_deref()),
        )
        .await?;
        scopes.extend(parse_folders(&output, &name)?);
    }
    Ok(Discovery {
        profiles,
        profile,
        scopes,
        ..Discovery::default()
    })
}

pub(super) async fn clusters(
    program: &Path,
    profile: Option<&str>,
    folder: &Scope,
) -> Result<Vec<CloudCluster>, CloudError> {
    let output = run(Provider::Yandex, program, &clusters_call(&folder.id, profile)).await?;
    parse_clusters(&output, folder, profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLOUDS: &str = include_str!("fixtures/yc-clouds.json");
    const FOLDERS: &str = include_str!("fixtures/yc-folders.json");
    const CLUSTERS: &str = include_str!("fixtures/yc-clusters.json");

    #[test]
    fn calls() {
        assert_eq!(
            clusters_call("b1gfolder", Some("prod")).args,
            [
                "managed-kubernetes",
                "cluster",
                "list",
                "--folder-id",
                "b1gfolder",
                "--format",
                "json",
                "--profile",
                "prod"
            ]
        );
        assert_eq!(
            clouds_call(None).args,
            ["resource-manager", "cloud", "list", "--format", "json"]
        );
        assert_eq!(
            folders_call("b1gcloud", None).args,
            [
                "resource-manager",
                "folder",
                "list",
                "--cloud-id",
                "b1gcloud",
                "--format",
                "json"
            ]
        );
    }

    #[test]
    fn profiles() {
        let (names, active) = parse_profiles("default\nprod ACTIVE\n\n");
        assert_eq!(names, ["default", "prod"]);
        assert_eq!(active.as_deref(), Some("prod"));
        assert_eq!(parse_profiles(""), (Vec::new(), None));
    }

    #[test]
    fn clouds_and_folders() {
        assert_eq!(
            parse_clouds(CLOUDS).unwrap(),
            [("b1gcloud0000000000".to_owned(), "acme".to_owned())]
        );
        let folders = parse_folders(FOLDERS, "acme").unwrap();
        assert_eq!(
            folders,
            [
                Scope {
                    id: "b1gfolder000000000a".into(),
                    label: "acme / default".into()
                },
                Scope {
                    id: "b1gfolder000000000b".into(),
                    label: "acme / prod".into()
                },
            ]
        );
    }

    #[test]
    fn clusters_and_their_endpoints() {
        let folder = Scope {
            id: "b1gfolder000000000a".into(),
            label: "acme / default".into(),
        };
        let clusters = parse_clusters(CLUSTERS, &folder, Some("default")).unwrap();
        assert_eq!(clusters.len(), 2);
        let zonal = &clusters[0];
        assert_eq!(zonal.name, "dev");
        assert_eq!(zonal.location, "ru-central1-a");
        assert_eq!(zonal.status, "RUNNING");
        assert_eq!(zonal.version.as_deref(), Some("1.30"));
        assert_eq!(
            zonal.endpoints,
            [
                Endpoint {
                    kind: EndpointKind::External,
                    server: "https://158.160.10.20".into()
                },
                Endpoint {
                    kind: EndpointKind::Internal,
                    server: "https://10.128.0.27".into()
                },
            ]
        );
        // The PEM is stored as base64, like get-credentials does.
        let data = zonal.ca_data.as_deref().unwrap();
        assert!(data.starts_with("LS0tLS1CRUdJTi"));
        assert_eq!(zonal.context_name(), "yc-dev");
        assert_eq!(
            zonal.target,
            Target::Yandex {
                id: "cat0000000000000dev".into(),
                profile: Some("default".into())
            }
        );

        let regional = &clusters[1];
        assert_eq!(regional.location, "ru-central1");
        let kinds: Vec<EndpointKind> = regional.endpoints.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, [EndpointKind::Internal]);
    }
}
