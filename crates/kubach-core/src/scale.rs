//! Relative scaling through the `/scale` subresource (#347).
//!
//! The replica stepper changes the count by a step from what the cluster
//! has now, not from what the UI last drew: the current `Scale` is read,
//! the new count computed from its `spec.replicas` and written back with
//! its `resourceVersion`, so a change made meanwhile (kubectl, an HPA) is
//! not overwritten. A conflict is retried from a fresh read a few times:
//! status updates of a rolling workload bump its resourceVersion as well.

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::autoscaling::v1::{Scale, ScaleSpec};
use kube::Api;
use kube::api::{DynamicObject, PostParams};

use crate::plugin::PluginError;

/// How many times the read-and-replace is tried before giving up on
/// concurrent changes.
const ATTEMPTS: usize = 4;

/// Pause before a retry, times the attempt number.
const RETRY_PAUSE: Duration = Duration::from_millis(50);

/// Why a step was not taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StepError {
    /// The count would go below zero (or past `i32::MAX`).
    #[error("{name} has {current} replicas; can't scale by {delta}")]
    OutOfRange {
        name: String,
        current: i32,
        delta: i32,
    },
    /// The step turns the workload off, which the user did not confirm
    /// (the UI showed more replicas than the cluster has now).
    #[error("{name} now has {current} replica(s); scaling it to 0 needs confirmation")]
    ZeroNotConfirmed { name: String, current: i32 },
}

/// The replica count after `delta` from `current`. Going from one or more
/// to zero needs `allow_zero`.
pub fn stepped(name: &str, current: i32, delta: i32, allow_zero: bool) -> Result<i32, StepError> {
    let next = current
        .checked_add(delta)
        .filter(|n| *n >= 0)
        .ok_or_else(|| StepError::OutOfRange {
            name: name.to_owned(),
            current,
            delta,
        })?;
    if next == 0 && current > 0 && !allow_zero {
        return Err(StepError::ZeroNotConfirmed {
            name: name.to_owned(),
            current,
        });
    }
    Ok(next)
}

/// Whether a step from `shown` replicas by `delta` turns the workload off,
/// so the UI asks first.
pub fn needs_confirmation(shown: i64, delta: i32) -> bool {
    shown > 0 && shown + i64::from(delta) <= 0
}

fn replicas_of(scale: &Scale) -> i32 {
    scale.spec.as_ref().and_then(|s| s.replicas).unwrap_or(0)
}

/// Changes the replicas of `name` by `delta` from the current `Scale`,
/// with optimistic locking; returns the new count.
pub async fn scale_by(
    api: &Api<DynamicObject>,
    name: &str,
    delta: i32,
    allow_zero: bool,
) -> Result<i32, PluginError> {
    for attempt in 1..=ATTEMPTS {
        let mut scale = api.get_scale(name).await?;
        let next = stepped(name, replicas_of(&scale), delta, allow_zero)
            .map_err(|error| PluginError::Refused(error.to_string()))?;
        // The read's resourceVersion stays in `metadata`: the server
        // refuses the write if the object changed since.
        scale.spec = Some(ScaleSpec {
            replicas: Some(next),
        });
        scale.status = None;
        match api
            .replace_scale(name, &PostParams::default(), &scale)
            .await
        {
            Ok(_) => return Ok(next),
            Err(kube::Error::Api(status)) if status.is_conflict() && attempt < ATTEMPTS => {
                tokio::time::sleep(RETRY_PAUSE * attempt as u32).await;
            }
            Err(kube::Error::Api(status)) if status.is_conflict() => {
                return Err(PluginError::Refused(format!(
                    "{name} keeps changing (an autoscaler or another client); try again"
                )));
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("the last attempt returns")
}

/// The name of an HPA among `hpas` (of the object's namespace) whose
/// scale target is the `kind` object `name` of API `group`.
pub fn hpa_for(hpas: &[Arc<DynamicObject>], group: &str, kind: &str, name: &str) -> Option<String> {
    hpas.iter().find_map(|hpa| {
        let target = hpa.data.pointer("/spec/scaleTargetRef")?;
        let field = |key: &str| target.get(key).and_then(serde_json::Value::as_str);
        let target_group = field("apiVersion")
            .map(|v| v.rsplit_once('/').map_or("", |(group, _)| group))
            .unwrap_or("apps");
        (field("kind") == Some(kind) && field("name") == Some(name) && target_group == group)
            .then(|| hpa.metadata.name.clone())
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use http::{Request, Response};
    use http_body_util::BodyExt;
    use k8s_openapi::api::apps::v1::Deployment;
    use kube::Client;
    use kube::api::ApiResource;
    use kube::client::Body;
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn steps_from_the_current_count() {
        assert_eq!(stepped("web", 5, -1, false), Ok(4));
        assert_eq!(stepped("web", 5, 1, false), Ok(6));
        assert_eq!(stepped("web", 0, 1, false), Ok(1));
        assert_eq!(stepped("web", 1, -1, true), Ok(0));
    }

    #[test]
    fn never_negative_nor_unconfirmed_zero() {
        assert!(matches!(
            stepped("web", 0, -1, true),
            Err(StepError::OutOfRange { current: 0, .. })
        ));
        assert!(matches!(
            stepped("web", i32::MAX, 1, true),
            Err(StepError::OutOfRange { .. })
        ));
        assert_eq!(
            stepped("web", 1, -1, false),
            Err(StepError::ZeroNotConfirmed {
                name: "web".into(),
                current: 1
            })
        );
    }

    #[test]
    fn zero_is_confirmed_from_what_is_shown() {
        assert!(needs_confirmation(1, -1));
        assert!(!needs_confirmation(2, -1));
        assert!(!needs_confirmation(0, 1));
        assert!(!needs_confirmation(0, -1));
        assert!(!needs_confirmation(1, 1));
    }

    #[test]
    fn finds_the_hpa_of_a_workload() {
        let hpa = |name: &str, api_version: &str, kind: &str, target: &str| {
            Arc::new(
                serde_json::from_value::<DynamicObject>(json!({
                    "apiVersion": "autoscaling/v2", "kind": "HorizontalPodAutoscaler",
                    "metadata": {"name": name, "namespace": "ns"},
                    "spec": {"scaleTargetRef": {
                        "apiVersion": api_version, "kind": kind, "name": target
                    }}
                }))
                .unwrap(),
            )
        };
        let hpas = [
            hpa("other", "apps/v1", "Deployment", "api"),
            hpa("sts", "apps/v1", "StatefulSet", "web"),
            hpa("web-hpa", "apps/v1", "Deployment", "web"),
        ];
        assert_eq!(
            hpa_for(&hpas, "apps", "Deployment", "web").as_deref(),
            Some("web-hpa")
        );
        assert_eq!(
            hpa_for(&hpas, "apps", "StatefulSet", "web").as_deref(),
            Some("sts")
        );
        assert_eq!(hpa_for(&hpas, "apps", "ReplicaSet", "web"), None);
        assert_eq!(hpa_for(&hpas, "example.com", "Deployment", "web"), None);
    }

    /// A fake API server: `GET …/scale` answers the next of `replicas`
    /// (with resourceVersion = its index), `PUT` answers the next of
    /// `puts` as a status code. Returns the client and the PUT bodies.
    fn server(replicas: Vec<i32>, puts: Vec<u16>) -> (Api<DynamicObject>, Arc<Mutex<Vec<Value>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(Mutex::new((replicas, puts, 0usize)));
        let log = sent.clone();
        let service = tower::service_fn(move |req: Request<Body>| {
            let (log, state) = (log.clone(), state.clone());
            async move {
                assert!(req.uri().path().ends_with("/deployments/web/scale"));
                let method = req.method().clone();
                let bytes = req.into_body().collect().await?.to_bytes();
                let (status, reply) = {
                    let mut state = state.lock().unwrap();
                    if method == http::Method::GET {
                        let ix = state.2;
                        state.2 += 1;
                        let replicas = state.0[ix];
                        (
                            200,
                            json!({"apiVersion": "autoscaling/v1", "kind": "Scale",
                                   "metadata": {"name": "web", "namespace": "ns",
                                                "resourceVersion": ix.to_string()},
                                   "spec": {"replicas": replicas},
                                   "status": {"replicas": replicas}}),
                        )
                    } else {
                        assert_eq!(method, http::Method::PUT);
                        let body: Value = serde_json::from_slice(&bytes).unwrap();
                        log.lock().unwrap().push(body.clone());
                        let code = state.1.remove(0);
                        let reply = if code == 200 {
                            body
                        } else {
                            json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
                                   "reason": "Conflict", "code": code, "message": "conflict"})
                        };
                        (code, reply)
                    }
                };
                Ok::<_, kube::Error>(
                    Response::builder()
                        .status(status)
                        .header(http::header::CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::to_vec(&reply).unwrap()))
                        .unwrap(),
                )
            }
        });
        let client = Client::new(service, "default");
        let resource = ApiResource::erase::<Deployment>(&());
        (Api::namespaced_with(client, "ns", &resource), sent)
    }

    #[tokio::test]
    async fn replaces_from_the_read_with_its_resource_version() {
        let (api, sent) = server(vec![5], vec![200]);
        assert_eq!(scale_by(&api, "web", -1, false).await.unwrap(), 4);
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["spec"]["replicas"], 4);
        assert_eq!(sent[0]["metadata"]["resourceVersion"], "0");
    }

    #[tokio::test]
    async fn retries_a_conflict_from_a_fresh_read() {
        let (api, sent) = server(vec![5, 7], vec![409, 200]);
        assert_eq!(scale_by(&api, "web", -1, false).await.unwrap(), 6);
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1]["spec"]["replicas"], 6);
        assert_eq!(sent[1]["metadata"]["resourceVersion"], "1");
    }

    #[tokio::test]
    async fn gives_up_after_repeated_conflicts() {
        let (api, sent) = server(vec![5; ATTEMPTS], vec![409; ATTEMPTS]);
        let error = scale_by(&api, "web", 1, false).await.unwrap_err();
        assert!(
            matches!(&error, PluginError::Refused(m) if m.contains("keeps changing")),
            "{error}"
        );
        assert_eq!(sent.lock().unwrap().len(), ATTEMPTS);
    }

    #[tokio::test]
    async fn refuses_without_writing() {
        // The cluster went down to 1 meanwhile: no unconfirmed zero.
        let (api, sent) = server(vec![1], vec![]);
        let error = scale_by(&api, "web", -1, false).await.unwrap_err();
        assert!(matches!(error, PluginError::Refused(_)));
        // Already at zero: no negative count.
        let (api, _) = server(vec![0], vec![]);
        assert!(scale_by(&api, "web", -1, true).await.is_err());
        assert!(sent.lock().unwrap().is_empty());
    }
}
