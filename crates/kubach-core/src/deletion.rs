//! Deleting a known object safely (#345).
//!
//! - The `DELETE` carries `preconditions.uid` of the object the user saw:
//!   an object deleted and recreated under the same name meanwhile is a
//!   different one and is not deleted. The API server answers such a
//!   delete with HTTP 409 Conflict, which [`finish`] turns into
//!   [`DeleteError::Recreated`].
//! - HTTP 404 means the object is already gone, which counts as deleted:
//!   someone else deleted it, or the first try of a `DELETE` retried after
//!   a dropped connection ([`crate::transport::RetryOnce`]) went through.

use kube::api::{DeleteParams, Preconditions};
use kube::core::Status;

/// Why deleting a known object failed.
#[derive(Debug, thiserror::Error)]
pub enum DeleteError {
    /// An object of that name exists, but with another UID: it was
    /// recreated after it was loaded and was left alone.
    #[error(
        "The object was recreated after it was loaded (its UID changed), so it was not deleted"
    )]
    Recreated,
    /// Any other error.
    #[error(transparent)]
    Kube(#[from] kube::Error),
}

/// `params` with the precondition that the object still has `uid`; no
/// precondition without a UID.
pub fn with_uid(params: DeleteParams, uid: Option<&str>) -> DeleteParams {
    match uid.filter(|uid| !uid.is_empty()) {
        Some(uid) => DeleteParams {
            preconditions: Some(Preconditions {
                uid: Some(uid.to_owned()),
                resource_version: None,
            }),
            ..params
        },
        None => params,
    }
}

/// The result of a `DELETE`: deleted or already gone is `Ok`, a UID
/// mismatch is [`DeleteError::Recreated`].
pub fn finish<T>(result: Result<T, kube::Error>) -> Result<(), DeleteError> {
    match result {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(status)) if status.is_not_found() => Ok(()),
        Err(kube::Error::Api(status)) if is_uid_conflict(&status) => Err(DeleteError::Recreated),
        Err(error) => Err(DeleteError::Kube(error)),
    }
}

/// Whether `status` is the API server's refusal of a `DELETE` whose UID
/// precondition does not match: `the UID in the precondition (…) does not
/// match the UID in record (…)`, or `Precondition failed: UID in
/// precondition: …, UID in object meta: …` from older servers.
pub fn is_uid_conflict(status: &Status) -> bool {
    status.code == 409
        && (status.message.contains("UID in the precondition")
            || status.message.contains("UID in precondition"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use http::{Request, Response};
    use http_body_util::BodyExt;
    use k8s_openapi::api::core::v1::ConfigMap;
    use kube::client::Body;
    use kube::{Api, Client};
    use serde_json::{Value, json};

    use super::*;

    fn api_error(value: Value) -> kube::Error {
        kube::Error::Api(Box::new(serde_json::from_value(value).unwrap()))
    }

    fn conflict() -> Value {
        json!({
            "kind": "Status", "apiVersion": "v1", "status": "Failure", "code": 409,
            "reason": "Conflict",
            "message": "Operation cannot be fulfilled on configmaps \"web\": Precondition \
                        failed: UID in precondition: 1111, UID in object meta: 2222",
            "details": {"name": "web", "kind": "configmaps"}
        })
    }

    #[test]
    fn uid_becomes_a_precondition() {
        let params = with_uid(DeleteParams::background(), Some("1111"));
        let preconditions = params.preconditions.as_ref().unwrap();
        assert_eq!(preconditions.uid.as_deref(), Some("1111"));
        assert_eq!(preconditions.resource_version, None);
        // The rest of the options stay.
        assert_eq!(
            params.propagation_policy,
            DeleteParams::background().propagation_policy
        );
        assert!(
            with_uid(DeleteParams::default(), None)
                .preconditions
                .is_none()
        );
        assert!(
            with_uid(DeleteParams::default(), Some(""))
                .preconditions
                .is_none()
        );
    }

    #[test]
    fn gone_is_done_and_a_uid_conflict_is_a_recreated_object() {
        let gone = api_error(
            json!({"status": "Failure", "code": 404, "reason": "NotFound",
                                    "message": "configmaps \"web\" not found"}),
        );
        assert!(finish::<()>(Err(gone)).is_ok());
        assert!(finish(Ok(())).is_ok());

        let error = finish::<()>(Err(api_error(conflict()))).unwrap_err();
        assert!(matches!(error, DeleteError::Recreated));
        assert!(error.to_string().contains("recreated"), "{error}");
        // As current API servers word it.
        let current = api_error(json!({
            "status": "Failure", "code": 409, "reason": "Conflict",
            "message": "Operation cannot be fulfilled on ConfigMap \"web\": the UID in the \
                        precondition (1111) does not match the UID in record (2222). The \
                        object might have been deleted and then recreated"
        }));
        assert!(matches!(
            finish::<()>(Err(current)),
            Err(DeleteError::Recreated)
        ));

        // Another conflict is not a recreated object.
        let other = api_error(
            json!({"status": "Failure", "code": 409, "reason": "Conflict",
                                     "message": "the object has been modified"}),
        );
        assert!(matches!(
            finish::<()>(Err(other)),
            Err(DeleteError::Kube(kube::Error::Api(_)))
        ));
        let forbidden = api_error(json!({"status": "Failure", "code": 403,
                                         "reason": "Forbidden", "message": "no"}));
        assert!(matches!(
            finish::<()>(Err(forbidden)),
            Err(DeleteError::Kube(_))
        ));
    }

    #[tokio::test]
    async fn the_delete_sends_the_uid_precondition() {
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let log = bodies.clone();
        let service = tower::service_fn(move |req: Request<Body>| {
            let log = log.clone();
            async move {
                let (parts, payload) = req.into_parts();
                let bytes = payload.collect().await?.to_bytes();
                let body: Value = serde_json::from_slice(&bytes).unwrap_or_default();
                log.lock().unwrap().push((parts.method.to_string(), body));
                Ok::<_, kube::Error>(
                    Response::builder()
                        .status(409)
                        .header(http::header::CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::to_vec(&conflict()).unwrap()))
                        .unwrap(),
                )
            }
        });
        let api: Api<ConfigMap> = Api::namespaced(Client::new(service, "default"), "ns1");
        let params = with_uid(DeleteParams::background(), Some("1111"));
        let result = finish(api.delete("web", &params).await);
        assert!(matches!(result, Err(DeleteError::Recreated)));
        let sent = bodies.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "DELETE");
        assert_eq!(sent[0].1["preconditions"], json!({"uid": "1111"}));
        assert_eq!(sent[0].1["propagationPolicy"], "Background");
    }
}
