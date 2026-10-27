//! The HTTP stack of kube clients (#316): the same layers as
//! `kube::client::ClientBuilder::try_from`, plus
//!
//! - **TCP keepalive** on API server connections, so a connection silently
//!   dropped by a load balancer (common for remote clusters behind one) is
//!   noticed instead of hanging the next request or watch;
//! - a shorter **pool idle timeout**, so idle connections are closed by us
//!   before a load balancer's idle timeout closes them under a request;
//! - **one retry** of an idempotent request (`GET`, `HEAD`, `DELETE`, …)
//!   that failed on the connection itself, i.e. before any HTTP answer.
//!   Answers of the API server (4xx, 5xx) are never retried here;
//! - kube's **server retry** (HTTP 429, 503, 504 with backoff and
//!   `Retry-After`) for **safe** requests only (`GET`, `HEAD`, `OPTIONS`,
//!   [`SafeServerRetry`]): a `POST`, `PUT`, `PATCH` or `DELETE` answered so
//!   fails at once (#345). A refused eviction (429 from a
//!   PodDisruptionBudget) is not repeated for minutes, and a `POST` that
//!   timed out at a gateway (504) after the write went through does not
//!   create a second object.
//!
//! Neither retry repeats a WebSocket upgrade (exec, attach, port-forward):
//! a repeated exec could run the command twice.
//!
//! kube-rs negotiates HTTP/1.1 only (no ALPN `h2`), so HTTP/2 keep-alive
//! pings do not apply. Clusters reached through a proxy (`proxy-url`) keep
//! kube's own stack, without these additions.

use std::error::Error as StdError;
use std::future::{Ready, ready};
use std::io;
use std::time::Duration;

use http::{Method, Request};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use kube::client::retry::RetryPolicy;
use kube::client::{Body, ClientBuilder, ConfigExt as _, DynBody};
use tower::retry::{Policy, RetryLayer};
use tower::{BoxError, ServiceBuilder};

/// Idle time before the first TCP keepalive probe.
pub const KEEPALIVE: Duration = Duration::from_secs(30);

/// Time between TCP keepalive probes.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// Pooled connections idle longer than this are closed. Below the common
/// 60 s idle timeout of load balancers.
pub const POOL_IDLE: Duration = Duration::from_secs(50);

/// The service type of kube's default client builder.
pub type GenericService =
    tower::util::BoxService<Request<Body>, http::Response<Box<DynBody>>, BoxError>;

/// Client builder for `config` with the stack described in the module docs.
pub fn builder(config: kube::Config) -> Result<ClientBuilder<GenericService>, kube::Error> {
    if config.proxy_url.is_some() {
        return ClientBuilder::try_from(config);
    }
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    http.set_nodelay(true);
    http.set_keepalive(Some(KEEPALIVE));
    http.set_keepalive_interval(Some(KEEPALIVE_INTERVAL));
    let https = config.rustls_https_connector_with_connector(http)?;
    let mut connector = hyper_timeout::TimeoutConnector::new(https);
    connector.set_connect_timeout(config.connect_timeout);
    connector.set_read_timeout(config.read_timeout);
    connector.set_write_timeout(config.write_timeout);
    let client: hyper_util::client::legacy::Client<_, Body> =
        hyper_util::client::legacy::Builder::new(TokioExecutor::new())
            .pool_idle_timeout(POOL_IDLE)
            .build(connector);

    let service = ServiceBuilder::new()
        .layer(config.base_uri_layer())
        .option_layer(
            config
                .default_retry
                .then(|| RetryLayer::new(SafeServerRetry::new(RetryPolicy::server_retry()))),
        )
        .option_layer(config.auth_layer()?)
        .layer(config.extra_headers_layer()?)
        .layer(RetryLayer::new(RetryOnce::default()))
        .map_err(BoxError::from)
        .service(client);
    let service = tower::ServiceExt::map_response(service, |response: http::Response<_>| {
        response.map(|body| {
            Box::new(http_body_util::BodyExt::map_err(body, BoxError::from)) as Box<DynBody>
        })
    });
    Ok(ClientBuilder::new(
        tower::util::BoxService::new(service),
        config.default_namespace,
    ))
}

/// kube's [`RetryPolicy`] limited to safe requests: anything else, and any
/// WebSocket upgrade, is neither retried nor cloned for a retry.
#[derive(Clone)]
pub struct SafeServerRetry(RetryPolicy);

impl SafeServerRetry {
    pub fn new(policy: RetryPolicy) -> Self {
        Self(policy)
    }
}

impl<Res> Policy<Request<Body>, http::Response<Res>, BoxError> for SafeServerRetry {
    type Future = tokio::time::Sleep;

    fn retry(
        &mut self,
        request: &mut Request<Body>,
        result: &mut Result<http::Response<Res>, BoxError>,
    ) -> Option<Self::Future> {
        if !is_safe(request.method()) || is_upgrade(request) {
            return None;
        }
        self.0.retry(request, result)
    }

    fn clone_request(&mut self, request: &Request<Body>) -> Option<Request<Body>> {
        if !is_safe(request.method()) || is_upgrade(request) {
            return None;
        }
        <RetryPolicy as Policy<Request<Body>, http::Response<Res>, BoxError>>::clone_request(
            &mut self.0,
            request,
        )
    }
}

/// Retries an idempotent request once after a connection error.
#[derive(Clone, Debug, Default)]
pub struct RetryOnce {
    retried: bool,
}

impl<Res> Policy<Request<Body>, Res, BoxError> for RetryOnce {
    type Future = Ready<()>;

    fn retry(
        &mut self,
        request: &mut Request<Body>,
        result: &mut Result<Res, BoxError>,
    ) -> Option<Self::Future> {
        let Err(error) = result else {
            return None;
        };
        if self.retried
            || !is_idempotent(request.method())
            || is_upgrade(request)
            || !is_connection_error(&**error)
        {
            return None;
        }
        self.retried = true;
        Some(ready(()))
    }

    fn clone_request(&mut self, request: &Request<Body>) -> Option<Request<Body>> {
        if self.retried || !is_idempotent(request.method()) || is_upgrade(request) {
            return None;
        }
        let body = request.body().try_clone()?;
        let mut clone = Request::new(body);
        *clone.method_mut() = request.method().clone();
        *clone.uri_mut() = request.uri().clone();
        *clone.version_mut() = request.version();
        *clone.headers_mut() = request.headers().clone();
        *clone.extensions_mut() = request.extensions().clone();
        Some(clone)
    }
}

/// Whether a request only reads: repeating it after any answer is safe.
pub fn is_safe(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Whether the request asks to switch protocols (the WebSocket of exec,
/// attach and port-forward): a `GET` that starts something.
pub fn is_upgrade<B>(request: &Request<B>) -> bool {
    request.headers().contains_key(http::header::UPGRADE)
}

/// Whether repeating a request has no effect beyond the first one.
pub fn is_idempotent(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::DELETE
    )
}

/// Whether `error` (or a source of it) means the connection failed or was
/// dropped before an answer: worth one more try on a fresh connection.
/// Timeouts are not: they already took long.
pub fn is_connection_error(error: &(dyn StdError + 'static)) -> bool {
    let mut connection = false;
    let mut source = Some(error);
    while let Some(current) = source {
        if let Some(io) = current.downcast_ref::<io::Error>() {
            match io.kind() {
                io::ErrorKind::TimedOut => return false,
                io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::ConnectionRefused
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::NotConnected
                | io::ErrorKind::UnexpectedEof => connection = true,
                _ => {}
            }
        }
        if current.is::<hyper_util::client::legacy::Error>() {
            connection = true;
        }
        source = current.source();
    }
    connection
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tower::{Service as _, ServiceExt as _};

    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("request failed")]
    struct Failed(#[source] io::Error);

    /// A service failing with `kind` the first `failures` calls, then
    /// answering 200; counts the calls.
    fn flaky(
        failures: usize,
        kind: io::ErrorKind,
    ) -> (
        impl tower::Service<
            Request<Body>,
            Response = http::Response<()>,
            Error = BoxError,
            Future: Send,
        > + Clone,
        Arc<AtomicUsize>,
    ) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let service = tower::service_fn(move |_request: Request<Body>| {
            let call = counter.fetch_add(1, Ordering::SeqCst);
            async move {
                if call < failures {
                    Err(BoxError::from(Failed(io::Error::from(kind))))
                } else {
                    Ok(http::Response::new(()))
                }
            }
        });
        (service, calls)
    }

    fn request(method: Method) -> Request<Body> {
        let mut request = Request::new(Body::empty());
        *request.method_mut() = method;
        request
    }

    async fn send(failures: usize, kind: io::ErrorKind, method: Method) -> (bool, usize) {
        let (service, calls) = flaky(failures, kind);
        let mut service = ServiceBuilder::new()
            .layer(RetryLayer::new(RetryOnce::default()))
            .service(service);
        let ok = service
            .ready()
            .await
            .unwrap()
            .call(request(method))
            .await
            .is_ok();
        (ok, calls.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn idempotent_requests_are_retried_once_after_a_dropped_connection() {
        let reset = io::ErrorKind::ConnectionReset;
        assert_eq!(send(1, reset, Method::DELETE).await, (true, 2));
        assert_eq!(send(1, reset, Method::GET).await, (true, 2));
        // Only once.
        assert_eq!(send(2, reset, Method::GET).await, (false, 2));
        // Not a POST or PATCH: the first one may have gone through.
        assert_eq!(send(1, reset, Method::POST).await, (false, 1));
        assert_eq!(send(1, reset, Method::PATCH).await, (false, 1));
        // Not a timeout, nor an error that is not the connection's.
        assert_eq!(
            send(1, io::ErrorKind::TimedOut, Method::GET).await,
            (false, 1)
        );
        assert_eq!(
            send(1, io::ErrorKind::NotFound, Method::GET).await,
            (false, 1)
        );
    }

    #[tokio::test]
    async fn upgrades_are_not_retried_after_a_dropped_connection() {
        let (service, calls) = flaky(1, io::ErrorKind::ConnectionReset);
        let mut service = ServiceBuilder::new()
            .layer(RetryLayer::new(RetryOnce::default()))
            .service(service);
        let mut exec = request(Method::GET);
        exec.headers_mut()
            .insert(http::header::UPGRADE, "websocket".parse().unwrap());
        let result = service.ready().await.unwrap().call(exec).await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A service answering `status` (with `Retry-After: 0`) the first
    /// `refusals` calls, then 200; counts the calls.
    fn refusing(
        refusals: usize,
        status: u16,
    ) -> (
        impl tower::Service<
            Request<Body>,
            Response = http::Response<()>,
            Error = BoxError,
            Future: Send,
        > + Clone,
        Arc<AtomicUsize>,
    ) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let service = tower::service_fn(move |_request: Request<Body>| {
            let call = counter.fetch_add(1, Ordering::SeqCst);
            async move {
                let status = if call < refusals { status } else { 200 };
                Ok::<_, BoxError>(
                    http::Response::builder()
                        .status(status)
                        .header("Retry-After", "0")
                        .body(())
                        .unwrap(),
                )
            }
        });
        (service, calls)
    }

    /// Sends `request` through [`SafeServerRetry`] (3 quick retries) to a
    /// service refusing `refusals` times with `status`: the final status
    /// and the number of calls.
    async fn send_refused(refusals: usize, status: u16, request: Request<Body>) -> (u16, usize) {
        let (service, calls) = refusing(refusals, status);
        let policy =
            RetryPolicy::new(Duration::from_millis(1), Duration::from_millis(2), 3, true).unwrap();
        let mut service = ServiceBuilder::new()
            .layer(RetryLayer::new(SafeServerRetry::new(policy)))
            .service(service);
        let response = service.ready().await.unwrap().call(request).await.unwrap();
        (response.status().as_u16(), calls.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn server_refusals_are_retried_for_safe_requests_only() {
        for status in [429, 503, 504] {
            // Reads are retried, up to the policy's limit.
            assert_eq!(
                send_refused(1, status, request(Method::GET)).await,
                (200, 2)
            );
            assert_eq!(
                send_refused(1, status, request(Method::HEAD)).await,
                (200, 2)
            );
            assert_eq!(
                send_refused(9, status, request(Method::GET)).await,
                (status, 4)
            );
            // Writes are not: the first one may have gone through, or an
            // eviction refused by a budget must be reported at once.
            for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
                assert_eq!(send_refused(1, status, request(method)).await, (status, 1));
            }
        }
        // Other answers are not retried.
        assert_eq!(send_refused(1, 500, request(Method::GET)).await, (500, 1));
        // Nor an upgrade (exec, attach, port-forward).
        let mut exec = request(Method::GET);
        exec.headers_mut()
            .insert(http::header::UPGRADE, "websocket".parse().unwrap());
        assert_eq!(send_refused(1, 503, exec).await, (503, 1));
    }

    #[test]
    fn connection_errors_are_found_among_the_sources() {
        let reset = Failed(io::Error::from(io::ErrorKind::ConnectionReset));
        assert!(is_connection_error(&reset));
        let timed_out = Failed(io::Error::from(io::ErrorKind::TimedOut));
        assert!(!is_connection_error(&timed_out));
        let other = io::Error::other("bad certificate");
        assert!(!is_connection_error(&other));
    }

    #[test]
    fn builds_a_client_without_contacting_the_server() {
        let config = kube::Config::new("https://127.0.0.1:9".parse().unwrap());
        assert!(builder(config).is_ok());
    }
}
