//! Pod eviction through the Eviction API (`POST pods/{name}/eviction` with a
//! `policy/v1` `Eviction`), which honours PodDisruptionBudgets, unlike a
//! plain delete.
//!
//! One call evicts one pod and classifies the answer: evicted, already gone,
//! refused by a PodDisruptionBudget (HTTP 429), or another API error. The
//! Pod plugin's Evict action and node drain both go through [`evict_pod`].

use k8s_openapi::api::policy::v1::Eviction;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::Client;
use kube::core::Status;

/// How an eviction that did not fail ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evicted {
    /// The API server accepted the eviction: the pod is being deleted.
    Evicted,
    /// The pod no longer exists (HTTP 404): nothing left to evict, which
    /// counts as done.
    AlreadyGone,
}

/// Why an eviction failed.
#[derive(Debug, thiserror::Error)]
pub enum EvictError {
    /// A PodDisruptionBudget does not allow the disruption now (HTTP 429).
    /// Retrying later may succeed, once enough pods are healthy.
    #[error("{}", disruption_message(.budget.as_deref(), .detail.as_deref()))]
    DisruptionBudget {
        /// Name of the budget, when the API server names it.
        budget: Option<String>,
        /// The API server's explanation (`needs 2 healthy pods and has 2
        /// currently`), without the budget's name.
        detail: Option<String>,
    },
    /// Any other error, RBAC refusals (403) included.
    #[error(transparent)]
    Kube(#[from] kube::Error),
}

impl EvictError {
    /// Whether a PodDisruptionBudget refused the eviction.
    pub fn is_disruption_budget(&self) -> bool {
        matches!(self, Self::DisruptionBudget { .. })
    }
}

fn disruption_message(budget: Option<&str>, detail: Option<&str>) -> String {
    let who = match budget {
        Some(budget) => format!("PodDisruptionBudget {budget}"),
        None => "A PodDisruptionBudget".to_owned(),
    };
    match detail {
        Some(detail) => format!("{who} does not allow evicting the pod now ({detail})"),
        None => format!("{who} does not allow evicting the pod now"),
    }
}

/// The `policy/v1` Eviction sent for `namespace/name`.
pub fn eviction(namespace: &str, name: &str) -> Eviction {
    Eviction {
        metadata: ObjectMeta {
            name: Some(name.to_owned()),
            namespace: Some(namespace.to_owned()),
            ..ObjectMeta::default()
        },
        delete_options: None,
    }
}

/// The request that evicts `namespace/name`.
pub fn eviction_request(namespace: &str, name: &str) -> http::Request<Vec<u8>> {
    let mut body = serde_json::to_value(eviction(namespace, name)).unwrap_or_default();
    // `Eviction` serializes without its type; the API server wants it.
    body["apiVersion"] = "policy/v1".into();
    body["kind"] = "Eviction".into();
    let path = format!("/api/v1/namespaces/{namespace}/pods/{name}/eviction");
    http::Request::post(path)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_vec(&body).unwrap_or_default())
        .expect("a valid eviction request")
}

/// Evicts one pod. A PodDisruptionBudget refusal comes back as
/// [`EvictError::DisruptionBudget`], a missing pod as
/// [`Evicted::AlreadyGone`].
pub async fn evict_pod(
    client: &Client,
    namespace: &str,
    name: &str,
) -> Result<Evicted, EvictError> {
    match client.request_text(eviction_request(namespace, name)).await {
        Ok(_) => Ok(Evicted::Evicted),
        Err(error) => classify(error),
    }
}

/// Classifies an error of an eviction request.
pub fn classify(error: kube::Error) -> Result<Evicted, EvictError> {
    match &error {
        kube::Error::Api(status) if status.is_not_found() => Ok(Evicted::AlreadyGone),
        kube::Error::Api(status) if is_disruption_refusal(status) => {
            let (budget, detail) = budget_of(status);
            Err(EvictError::DisruptionBudget { budget, detail })
        }
        _ => Err(EvictError::Kube(error)),
    }
}

/// A 429 from the eviction subresource is a budget refusal; one from API
/// priority and fairness (throttling) is not and names no budget.
fn is_disruption_refusal(status: &Status) -> bool {
    status.code == 429
        && (causes(status).any(|(reason, _)| reason == "DisruptionBudget")
            || status.message.contains("disruption budget"))
}

fn causes(status: &Status) -> impl Iterator<Item = (&str, &str)> {
    status
        .details
        .iter()
        .flat_map(|d| d.causes.iter())
        .map(|c| (c.reason.as_str(), c.message.as_str()))
}

/// The budget's name and the rest of the explanation from a cause like
/// `The disruption budget web needs 2 healthy pods and has 2 currently`.
fn budget_of(status: &Status) -> (Option<String>, Option<String>) {
    let Some((_, message)) = causes(status).find(|(reason, _)| *reason == "DisruptionBudget")
    else {
        return (None, None);
    };
    match message
        .strip_prefix("The disruption budget ")
        .and_then(|rest| rest.split_once(' '))
    {
        Some((budget, detail)) => (Some(budget.to_owned()), Some(detail.to_owned())),
        None => (None, (!message.is_empty()).then(|| message.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use http::{Request, Response};
    use http_body_util::BodyExt;
    use kube::client::Body;
    use serde_json::{Value, json};

    use super::*;

    fn status(value: Value) -> kube::Error {
        kube::Error::Api(Box::new(serde_json::from_value(value).unwrap()))
    }

    fn pdb_refusal() -> Value {
        json!({
            "kind": "Status", "apiVersion": "v1", "status": "Failure", "code": 429,
            "reason": "TooManyRequests",
            "message": "Cannot evict pod as it would violate the pod's disruption budget.",
            "details": {"causes": [{
                "reason": "DisruptionBudget",
                "message": "The disruption budget web-pdb needs 2 healthy pods and has 2 currently"
            }]}
        })
    }

    #[test]
    fn request_posts_a_policy_v1_eviction() {
        let req = eviction_request("ns1", "web-1");
        assert_eq!(req.method(), http::Method::POST);
        assert_eq!(req.uri(), "/api/v1/namespaces/ns1/pods/web-1/eviction");
        assert_eq!(
            req.headers()[http::header::CONTENT_TYPE],
            "application/json"
        );
        let body: Value = serde_json::from_slice(req.body()).unwrap();
        assert_eq!(
            body,
            json!({
                "apiVersion": "policy/v1", "kind": "Eviction",
                "metadata": {"name": "web-1", "namespace": "ns1"}
            })
        );
    }

    #[test]
    fn budget_refusal_names_the_budget() {
        let error = classify(status(pdb_refusal())).unwrap_err();
        assert!(error.is_disruption_budget());
        assert_eq!(
            error.to_string(),
            "PodDisruptionBudget web-pdb does not allow evicting the pod now \
             (needs 2 healthy pods and has 2 currently)"
        );

        // Without causes: still a budget refusal, unnamed.
        let mut bare = pdb_refusal();
        bare.as_object_mut().unwrap().remove("details");
        let error = classify(status(bare)).unwrap_err();
        assert!(error.is_disruption_budget());
        assert_eq!(
            error.to_string(),
            "A PodDisruptionBudget does not allow evicting the pod now"
        );
    }

    #[test]
    fn missing_pod_is_done_and_other_errors_pass_through() {
        let gone = status(
            json!({"status": "Failure", "code": 404, "reason": "NotFound",
                                 "message": "pods \"web-1\" not found"}),
        );
        assert_eq!(classify(gone).unwrap(), Evicted::AlreadyGone);

        let forbidden = status(
            json!({"status": "Failure", "code": 403, "reason": "Forbidden",
                                      "message": "pods \"web-1\" is forbidden"}),
        );
        let error = classify(forbidden).unwrap_err();
        assert!(matches!(&error, EvictError::Kube(kube::Error::Api(s)) if s.is_forbidden()));

        // Throttling by API priority and fairness is not a budget refusal.
        let throttled = status(json!({"status": "Failure", "code": 429,
                                      "reason": "TooManyRequests",
                                      "message": "Too many requests, please try again later."}));
        assert!(!classify(throttled).unwrap_err().is_disruption_budget());
    }

    /// A client answering every request with `code` and `body`, recording
    /// method and URI.
    fn answering(code: u16, body: Value) -> (Client, Arc<Mutex<Vec<String>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        let service = tower::service_fn(move |req: Request<Body>| {
            let log = log.clone();
            let body = body.clone();
            async move {
                let (parts, payload) = req.into_parts();
                payload.collect().await?;
                log.lock()
                    .unwrap()
                    .push(format!("{} {}", parts.method, parts.uri));
                Ok::<_, kube::Error>(
                    Response::builder()
                        .status(code)
                        .header(http::header::CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
            }
        });
        (Client::new(service, "default"), sent)
    }

    #[tokio::test]
    async fn evict_pod_sends_one_request_and_classifies_the_answer() {
        let (client, sent) = answering(201, json!({"kind": "Status", "status": "Success"}));
        assert_eq!(
            evict_pod(&client, "ns1", "web-1").await.unwrap(),
            Evicted::Evicted
        );
        assert_eq!(
            *sent.lock().unwrap(),
            ["POST /api/v1/namespaces/ns1/pods/web-1/eviction"]
        );

        let (client, _) = answering(429, pdb_refusal());
        let error = evict_pod(&client, "ns1", "web-1").await.unwrap_err();
        assert!(error.is_disruption_budget());

        let (client, _) = answering(
            404,
            json!({"kind": "Status", "status": "Failure", "code": 404, "reason": "NotFound"}),
        );
        assert_eq!(
            evict_pod(&client, "ns1", "web-1").await.unwrap(),
            Evicted::AlreadyGone
        );
    }
}
