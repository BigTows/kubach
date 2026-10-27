//! Managing namespaces: which ones are protected from deletion, checking a
//! new one's name and labels, creating it and counting what is inside one
//! before it is deleted.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::Namespace;
use kubach_i18n::{Message, msg};
use kube::api::{Api, ListParams, ObjectMeta, PostParams};
use kube::{Client, Resource as _};

use crate::discovery::{self, ResourceKind};
use crate::names::validate_dns_label;

/// Namespaces the API server refuses to delete (the `NamespaceLifecycle`
/// admission plugin): Delete is not offered for them at all.
pub const IMMORTAL: [&str; 3] = ["default", "kube-system", "kube-public"];

/// Namespaces the cluster itself relies on: [`IMMORTAL`] plus
/// `kube-node-lease` (node heartbeats). Deleting one asks to type its name.
pub const SYSTEM: [&str; 4] = ["default", "kube-system", "kube-public", "kube-node-lease"];

/// Label every namespace gets from the API server (`= its name`); it says
/// nothing, so short label lists leave it out.
pub const NAME_LABEL: &str = "kubernetes.io/metadata.name";

/// Whether the API server never deletes `name`.
pub fn is_immortal(name: &str) -> bool {
    IMMORTAL.contains(&name)
}

/// Whether `name` is a namespace of the cluster itself.
pub fn is_system(name: &str) -> bool {
    SYSTEM.contains(&name)
}

/// Kinds counted inside a namespace before it is deleted, with their plural
/// titles, in the order the confirmation lists them.
pub const CONTENT_KINDS: [(&str, &str, &str); 11] = [
    ("", "Pod", "Pods"),
    ("apps", "Deployment", "Deployments"),
    ("apps", "StatefulSet", "StatefulSets"),
    ("apps", "DaemonSet", "DaemonSets"),
    ("batch", "Job", "Jobs"),
    ("batch", "CronJob", "CronJobs"),
    ("", "Service", "Services"),
    ("networking.k8s.io", "Ingress", "Ingresses"),
    ("", "ConfigMap", "ConfigMaps"),
    ("", "Secret", "Secrets"),
    ("", "PersistentVolumeClaim", "PersistentVolumeClaims"),
];

/// Checks the name of a new namespace (a DNS-1123 label).
pub fn validate_name(name: &str) -> Result<(), String> {
    validate_dns_label(name).map_err(|error| format!("Invalid name: {error}"))
}

/// Name part of a label key or a label value: at most 63 alphanumerics,
/// `-`, `_` and `.`, starting and ending with an alphanumeric.
fn is_label_name(s: &str) -> bool {
    let edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    s.len() <= 63
        && edge(s.chars().next())
        && edge(s.chars().last())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Checks a label key: an optional DNS subdomain prefix and `/`, then a
/// name.
fn check_label_key(key: &str) -> Result<(), String> {
    let (prefix, name) = match key.rsplit_once('/') {
        Some((prefix, name)) => (Some(prefix), name),
        None => (None, key),
    };
    if let Some(prefix) = prefix
        && crate::names::validate_dns_subdomain(prefix).is_err()
    {
        return Err(format!(
            "Label key {key:?}: the prefix must be a DNS subdomain"
        ));
    }
    if !is_label_name(name) {
        return Err(format!(
            "Label key {key:?}: up to 63 letters, digits, '-', '_' or '.', \
             starting and ending with a letter or digit"
        ));
    }
    Ok(())
}

/// Parses labels typed as `key=value`, separated by commas or new lines.
/// Empty input means no labels.
pub fn parse_labels(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut labels = BTreeMap::new();
    for part in text
        .split([',', '\n'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        let Some((key, value)) = part.split_once('=') else {
            return Err(format!("Label {part:?}: write it as key=value"));
        };
        let (key, value) = (key.trim(), value.trim());
        check_label_key(key)?;
        if !value.is_empty() && !is_label_name(value) {
            return Err(format!(
                "Label value {value:?}: up to 63 letters, digits, '-', '_' or '.', \
                 starting and ending with a letter or digit"
            ));
        }
        if labels.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(format!("Label {key:?} is given twice"));
        }
    }
    Ok(labels)
}

/// The namespace object to create.
pub fn new_namespace(name: &str, labels: BTreeMap<String, String>) -> Namespace {
    Namespace {
        metadata: ObjectMeta {
            name: Some(name.to_owned()),
            labels: (!labels.is_empty()).then_some(labels),
            ..ObjectMeta::default()
        },
        ..Namespace::default()
    }
}

/// Creates namespace `name` with `labels`.
pub async fn create(
    client: &Client,
    name: &str,
    labels: BTreeMap<String, String>,
) -> Result<(), kube::Error> {
    let api: Api<Namespace> = Api::all(client.clone());
    api.create(&PostParams::default(), &new_namespace(name, labels))
        .await?;
    Ok(())
}

/// Number of `kind` objects in `namespace`: one list request for a single
/// item, whose `remainingItemCount` gives the rest. Lists everything (only
/// metadata) when the server does not report the count.
pub async fn count(
    client: &Client,
    kind: &ResourceKind,
    namespace: &str,
) -> Result<usize, kube::Error> {
    let api = discovery::api(client, kind, Some(namespace));
    let page = api.list_metadata(&ListParams::default().limit(1)).await?;
    let more = page
        .metadata
        .continue_
        .as_deref()
        .is_some_and(|c| !c.is_empty());
    match (more, page.metadata.remaining_item_count) {
        (false, _) => Ok(page.items.len()),
        (true, Some(rest)) => Ok(page.items.len() + rest.max(0) as usize),
        (true, None) => Ok(api.list_metadata(&ListParams::default()).await?.items.len()),
    }
}

/// `3 Pods, 1 Deployment`: the non-empty counts, singular for one.
pub fn describe_counts(counts: &[(String, usize)]) -> Vec<Message> {
    counts
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(title, n)| {
            msg!(
                "action-namespace-count",
                count = *n,
                kind = singular(title),
                title = title
            )
        })
        .collect()
}

fn singular(title: &str) -> &str {
    match title.strip_suffix("sses") {
        Some(_) => &title[..title.len() - 2],
        None => title.strip_suffix('s').unwrap_or(title),
    }
}

/// What a delete confirmation of namespace `name` says about its content:
/// counts when known (`None` while unknown).
pub fn delete_note(name: &str, counts: Option<&[(String, usize)]>) -> Message {
    match counts.map(describe_counts) {
        Some(parts) if parts.is_empty() => msg!("action-namespace-empty", namespace = name),
        Some(parts) => msg!("action-namespace-holds", namespace = name, contents = parts),
        None => msg!("action-namespace-unknown", namespace = name),
    }
}

/// The kind of a namespace object, for checks on plain objects.
pub fn is_namespace_kind(group: &str, kind: &str) -> bool {
    group.is_empty() && kind == Namespace::kind(&())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_namespaces() {
        for name in IMMORTAL {
            assert!(is_immortal(name) && is_system(name), "{name}");
        }
        assert!(is_system("kube-node-lease"));
        assert!(!is_immortal("kube-node-lease"));
        assert!(!is_system("kube-flannel"));
        assert!(!is_immortal("kubach-test-277"));
    }

    #[test]
    fn names_are_dns_labels() {
        assert!(validate_name("kubach-test-277").is_ok());
        for bad in ["", "Dev", "a.b", "-a", "a-", &"a".repeat(64)] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parses_labels() {
        assert!(parse_labels("  ").unwrap().is_empty());
        let labels =
            parse_labels("team=web, env = prod\napp.kubernetes.io/part-of=shop,empty=").unwrap();
        assert_eq!(labels.len(), 4);
        assert_eq!(labels["env"], "prod");
        assert_eq!(labels["app.kubernetes.io/part-of"], "shop");
        assert_eq!(labels["empty"], "");
        assert!(parse_labels("team").is_err());
        assert!(parse_labels("team=a,team=b").is_err());
        assert!(parse_labels("-x=1").is_err());
        assert!(parse_labels("Bad_Prefix/x=1").is_err());
        assert!(parse_labels("x=bad value").is_err());
    }

    #[test]
    fn new_namespace_object() {
        let ns = new_namespace("dev", BTreeMap::new());
        assert_eq!(ns.metadata.name.as_deref(), Some("dev"));
        assert_eq!(ns.metadata.labels, None);
        let ns = new_namespace("dev", parse_labels("team=web").unwrap());
        assert_eq!(ns.metadata.labels.unwrap()["team"], "web");
    }

    #[test]
    fn describes_contents() {
        let counts = vec![
            ("Pods".to_owned(), 3),
            ("Deployments".to_owned(), 1),
            ("Ingresses".to_owned(), 1),
            ("Services".to_owned(), 0),
            ("ConfigMaps".to_owned(), 2),
        ];
        assert_eq!(
            describe_counts(&counts)
                .iter()
                .map(Message::english)
                .collect::<Vec<_>>()
                .join(", "),
            "3 Pods, 1 Deployment, 1 Ingress, 2 ConfigMaps"
        );
        let note = delete_note("shop", Some(&counts)).english();
        assert!(note.starts_with("shop holds 3 Pods"), "{note}");
        assert!(note.contains("deletes every object inside"));
        assert!(
            delete_note("shop", Some(&[]))
                .english()
                .contains("none of the main kinds")
        );
        assert!(
            delete_note("shop", None)
                .english()
                .starts_with("Everything in shop")
        );
        assert!(is_namespace_kind("", "Namespace"));
        assert!(!is_namespace_kind("example.com", "Namespace"));
    }
}
