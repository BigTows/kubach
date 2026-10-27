//! Which Secrets a pod spec uses and how (#227): env, envFrom, volumes,
//! projected volumes and imagePullSecrets. Names and keys only.

use std::collections::BTreeSet;

use k8s_openapi::api::core::v1::{Container, PodSpec};
use serde::Serialize;

/// How a pod spec reaches a Secret.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Via {
    /// `env[].valueFrom.secretKeyRef`.
    Env,
    /// `envFrom[].secretRef`.
    EnvFrom,
    /// `volumes[].secret`.
    Volume,
    /// `volumes[].projected.sources[].secret`.
    Projected,
    ImagePullSecret,
}

/// One use of a Secret.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretRef {
    pub secret: String,
    pub via: Via,
    /// The key it needs: a `secretKeyRef` key or a volume item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The pod starts without it (`optional: true`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    /// Container or volume name, for the agent to find it.
    pub at: String,
}

fn containers(spec: &PodSpec) -> impl Iterator<Item = &Container> {
    spec.init_containers
        .iter()
        .flatten()
        .chain(spec.containers.iter())
}

/// Every Secret use in `spec`, sorted and without duplicates.
pub fn secret_refs(spec: &PodSpec) -> Vec<SecretRef> {
    let mut refs = BTreeSet::new();
    for container in containers(spec) {
        for env in container.env.iter().flatten() {
            if let Some(selector) = env
                .value_from
                .as_ref()
                .and_then(|from| from.secret_key_ref.as_ref())
            {
                refs.insert(SecretRef {
                    secret: selector.name.clone(),
                    via: Via::Env,
                    key: Some(selector.key.clone()),
                    optional: selector.optional.unwrap_or(false),
                    at: format!("container {} env {}", container.name, env.name),
                });
            }
        }
        for from in container.env_from.iter().flatten() {
            if let Some(source) = &from.secret_ref {
                refs.insert(SecretRef {
                    secret: source.name.clone(),
                    via: Via::EnvFrom,
                    key: None,
                    optional: source.optional.unwrap_or(false),
                    at: format!("container {}", container.name),
                });
            }
        }
    }
    for volume in spec.volumes.iter().flatten() {
        if let Some(secret) = &volume.secret {
            let name = secret.secret_name.clone().unwrap_or_default();
            let optional = secret.optional.unwrap_or(false);
            let at = format!("volume {}", volume.name);
            let items: Vec<_> = secret.items.iter().flatten().collect();
            if items.is_empty() {
                refs.insert(SecretRef {
                    secret: name.clone(),
                    via: Via::Volume,
                    key: None,
                    optional,
                    at: at.clone(),
                });
            }
            for item in items {
                refs.insert(SecretRef {
                    secret: name.clone(),
                    via: Via::Volume,
                    key: Some(item.key.clone()),
                    optional,
                    at: at.clone(),
                });
            }
        }
        for source in volume
            .projected
            .iter()
            .flat_map(|projected| projected.sources.iter().flatten())
        {
            let Some(secret) = &source.secret else {
                continue;
            };
            let optional = secret.optional.unwrap_or(false);
            let at = format!("volume {}", volume.name);
            let items: Vec<_> = secret.items.iter().flatten().collect();
            if items.is_empty() {
                refs.insert(SecretRef {
                    secret: secret.name.clone(),
                    via: Via::Projected,
                    key: None,
                    optional,
                    at: at.clone(),
                });
            }
            for item in items {
                refs.insert(SecretRef {
                    secret: secret.name.clone(),
                    via: Via::Projected,
                    key: Some(item.key.clone()),
                    optional,
                    at: at.clone(),
                });
            }
        }
    }
    for pull in spec.image_pull_secrets.iter().flatten() {
        refs.insert(SecretRef {
            secret: pull.name.clone(),
            via: Via::ImagePullSecret,
            key: None,
            optional: false,
            at: "imagePullSecrets".into(),
        });
    }
    refs.into_iter().filter(|r| !r.secret.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_every_kind_of_use() {
        let spec: PodSpec = serde_json::from_value(serde_json::json!({
            "initContainers": [{"name": "migrate", "envFrom": [{"secretRef": {"name": "db"}}]}],
            "containers": [{
                "name": "app",
                "env": [
                    {"name": "PASSWORD", "valueFrom": {"secretKeyRef": {"name": "db", "key": "password"}}},
                    {"name": "TOKEN", "valueFrom": {"secretKeyRef": {"name": "api", "key": "token", "optional": true}}},
                    {"name": "PLAIN", "value": "x"}
                ]
            }],
            "volumes": [
                {"name": "tls", "secret": {"secretName": "web-tls", "items": [{"key": "tls.crt", "path": "c"}]}},
                {"name": "all", "secret": {"secretName": "bundle"}},
                {"name": "proj", "projected": {"sources": [{"secret": {"name": "extra"}}]}}
            ],
            "imagePullSecrets": [{"name": "registry"}]
        }))
        .unwrap();
        let refs = secret_refs(&spec);
        let find = |secret: &str| refs.iter().filter(|r| r.secret == secret).count();
        assert_eq!(find("db"), 2);
        assert_eq!(find("web-tls"), 1);
        assert_eq!(find("bundle"), 1);
        assert_eq!(find("extra"), 1);
        assert_eq!(find("registry"), 1);
        let token = refs.iter().find(|r| r.secret == "api").unwrap();
        assert!(token.optional);
        assert_eq!(token.key.as_deref(), Some("token"));
        assert_eq!(token.via, Via::Env);
        let tls = refs.iter().find(|r| r.secret == "web-tls").unwrap();
        assert_eq!(tls.key.as_deref(), Some("tls.crt"));
    }
}
