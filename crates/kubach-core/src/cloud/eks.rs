//! Amazon EKS through `aws`: profiles, regions, `eks list-clusters` per
//! region and `eks describe-cluster` per cluster.

use std::path::Path;

use serde_json::Value;

use super::{
    CliCall, CloudCluster, CloudError, Discovery, Endpoint, EndpointKind, Provider, Scope, Target,
    parse_json, run, run_all, text,
};

/// The profile used when none is named.
pub const DEFAULT_PROFILE: &str = "default";

/// Regions with EKS (commercial partitions), offered to pick from.
pub const REGIONS: &[&str] = &[
    "us-east-1",
    "us-east-2",
    "us-west-1",
    "us-west-2",
    "ca-central-1",
    "ca-west-1",
    "mx-central-1",
    "sa-east-1",
    "eu-west-1",
    "eu-west-2",
    "eu-west-3",
    "eu-central-1",
    "eu-central-2",
    "eu-north-1",
    "eu-south-1",
    "eu-south-2",
    "il-central-1",
    "me-south-1",
    "me-central-1",
    "af-south-1",
    "ap-east-1",
    "ap-south-1",
    "ap-south-2",
    "ap-northeast-1",
    "ap-northeast-2",
    "ap-northeast-3",
    "ap-southeast-1",
    "ap-southeast-2",
    "ap-southeast-3",
    "ap-southeast-4",
    "ap-southeast-5",
    "ap-southeast-7",
];

fn with_profile(mut args: Vec<String>, profile: Option<&str>) -> CliCall {
    if let Some(profile) = profile.filter(|p| !p.is_empty() && *p != DEFAULT_PROFILE) {
        args.extend(["--profile".to_owned(), profile.to_owned()]);
    }
    CliCall { args }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_owned()).collect()
}

/// `aws configure list-profiles` (one name per line).
pub fn profiles_call() -> CliCall {
    CliCall::new(["configure", "list-profiles"])
}

/// `aws configure get region [--profile p]`: the profile's region.
pub fn region_call(profile: Option<&str>) -> CliCall {
    with_profile(strings(&["configure", "get", "region"]), profile)
}

/// `aws eks list-clusters --region r --output json [--profile p]`.
pub fn list_call(region: &str, profile: Option<&str>) -> CliCall {
    with_profile(
        strings(&["eks", "list-clusters", "--region", region, "--output", "json"]),
        profile,
    )
}

/// `aws eks describe-cluster --name n --region r --output json`.
pub fn describe_call(name: &str, region: &str, profile: Option<&str>) -> CliCall {
    with_profile(
        strings(&[
            "eks",
            "describe-cluster",
            "--name",
            name,
            "--region",
            region,
            "--output",
            "json",
        ]),
        profile,
    )
}

/// Profile names from `aws configure list-profiles`.
pub fn parse_profiles(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The region scopes, with `extra` (a profile's region not in the list)
/// added.
pub fn region_scopes(extra: Option<&str>) -> Vec<Scope> {
    let mut regions: Vec<String> = REGIONS.iter().map(|r| (*r).to_owned()).collect();
    if let Some(extra) = extra.filter(|r| !REGIONS.contains(r)) {
        regions.insert(0, extra.to_owned());
    }
    regions
        .into_iter()
        .map(|region| Scope {
            label: region.clone(),
            id: region,
        })
        .collect()
}

/// Cluster names in `eks list-clusters` output.
pub fn parse_list(json: &str, region: &str) -> Result<Vec<String>, CloudError> {
    let value = parse_json(&list_call(region, None), json)?;
    Ok(value
        .get("clusters")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default())
}

/// The cluster in `eks describe-cluster` output.
pub fn parse_describe(
    json: &str,
    region: &str,
    profile: Option<&str>,
) -> Result<CloudCluster, CloudError> {
    let call = describe_call("", region, profile);
    let value = parse_json(&call, json)?;
    let c = value.get("cluster").ok_or_else(|| CloudError::Parse {
        command: call.args.join(" "),
        detail: "no cluster".into(),
    })?;
    let name = text(c, "/name").unwrap_or_default();
    let arn = text(c, "/arn").unwrap_or_else(|| name.clone());
    let public = c
        .pointer("/resourcesVpcConfig/endpointPublicAccess")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    // One address either way: inside the VPC it resolves to private IPs.
    let endpoints = text(c, "/endpoint")
        .map(|server| Endpoint {
            kind: if public {
                EndpointKind::External
            } else {
                EndpointKind::Internal
            },
            server,
        })
        .into_iter()
        .collect();
    Ok(CloudCluster {
        name,
        location: region.to_owned(),
        scope: region.to_owned(),
        status: text(c, "/status").unwrap_or_default(),
        version: text(c, "/version"),
        endpoints,
        ca_data: text(c, "/certificateAuthority/data"),
        target: Target::Eks {
            arn,
            region: region.to_owned(),
            profile: profile.map(str::to_owned),
        },
    })
}

pub(super) async fn discover(
    program: &Path,
    profile: Option<&str>,
) -> Result<Discovery, CloudError> {
    let mut profiles = parse_profiles(&run(Provider::Eks, program, &profiles_call()).await?);
    if profiles.is_empty() {
        profiles.push(DEFAULT_PROFILE.to_owned());
    }
    let profile = profile
        .map(str::to_owned)
        .or_else(|| crate::env::var("AWS_PROFILE"))
        .filter(|p| profiles.contains(p))
        .unwrap_or_else(|| {
            if profiles.iter().any(|p| p == DEFAULT_PROFILE) {
                DEFAULT_PROFILE.to_owned()
            } else {
                profiles[0].clone()
            }
        });
    // No region set is not an error: the user picks one.
    let region = run(Provider::Eks, program, &region_call(Some(&profile)))
        .await
        .ok()
        .map(|out| out.trim().to_owned())
        .filter(|r| !r.is_empty());
    Ok(Discovery {
        profiles,
        profile: Some(profile),
        scopes: region_scopes(region.as_deref()),
        preselected: region.into_iter().collect(),
        account: None,
    })
}

pub(super) async fn clusters(
    program: &Path,
    profile: Option<&str>,
    region: &Scope,
) -> Result<Vec<CloudCluster>, CloudError> {
    let output = run(Provider::Eks, program, &list_call(&region.id, profile)).await?;
    let names = parse_list(&output, &region.id)?;
    let calls = names
        .iter()
        .map(|name| describe_call(name, &region.id, profile))
        .collect();
    let mut clusters = Vec::new();
    for (_, result) in run_all(Provider::Eks, program, calls).await {
        clusters.push(parse_describe(&result?, &region.id, profile)?);
    }
    Ok(clusters)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = include_str!("fixtures/aws-eks-list-clusters.json");
    const DESCRIBE: &str = include_str!("fixtures/aws-eks-describe-cluster.json");
    const DESCRIBE_PRIVATE: &str = include_str!("fixtures/aws-eks-describe-cluster-private.json");

    #[test]
    fn calls() {
        assert_eq!(
            list_call("eu-west-1", Some("work")).args,
            [
                "eks",
                "list-clusters",
                "--region",
                "eu-west-1",
                "--output",
                "json",
                "--profile",
                "work"
            ]
        );
        assert_eq!(
            describe_call("main", "eu-west-1", Some("default")).args,
            [
                "eks",
                "describe-cluster",
                "--name",
                "main",
                "--region",
                "eu-west-1",
                "--output",
                "json"
            ]
        );
        assert_eq!(
            region_call(Some("work")).args,
            ["configure", "get", "region", "--profile", "work"]
        );
        assert_eq!(profiles_call().args, ["configure", "list-profiles"]);
    }

    #[test]
    fn profiles_and_regions() {
        assert_eq!(parse_profiles("default\nwork\n\n"), ["default", "work"]);
        let scopes = region_scopes(Some("eu-west-1"));
        assert_eq!(scopes.len(), REGIONS.len());
        let scopes = region_scopes(Some("us-gov-west-1"));
        assert_eq!(scopes[0].id, "us-gov-west-1");
        assert_eq!(scopes.len(), REGIONS.len() + 1);
    }

    #[test]
    fn list_and_describe() {
        assert_eq!(parse_list(LIST, "eu-west-1").unwrap(), ["main", "internal"]);
        assert_eq!(
            parse_list(r#"{"clusters": []}"#, "eu-west-1").unwrap(),
            Vec::<String>::new()
        );
        let main = parse_describe(DESCRIBE, "eu-west-1", Some("work")).unwrap();
        assert_eq!(main.name, "main");
        assert_eq!(main.status, "ACTIVE");
        assert_eq!(main.version.as_deref(), Some("1.31"));
        assert_eq!(main.ca_data.as_deref(), Some("LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0tCk1JSURCVENDQWUyZ0F3SUJBZ0lJ"));
        assert_eq!(
            main.endpoints,
            [Endpoint {
                kind: EndpointKind::External,
                server: "https://0123456789ABCDEF0123456789ABCDEF.gr7.eu-west-1.eks.amazonaws.com"
                    .into()
            }]
        );
        assert_eq!(
            main.context_name(),
            "arn:aws:eks:eu-west-1:123456789012:cluster/main"
        );
        let private = parse_describe(DESCRIBE_PRIVATE, "eu-west-1", None).unwrap();
        assert_eq!(private.endpoints[0].kind, EndpointKind::Internal);
        assert!(matches!(
            parse_describe("{}", "eu-west-1", None),
            Err(CloudError::Parse { .. })
        ));
    }
}
