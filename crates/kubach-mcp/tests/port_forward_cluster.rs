//! The agent's port-forward against a real cluster (#17): a pod with a
//! web server, a session at the debug level, a forward the user approves,
//! a request through `127.0.0.1`, and the forward gone with the session.
//! Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-17 \
//!     cargo test -p kubach-mcp --test port_forward_cluster -- --ignored --test-threads=1
//! ```
//!
//! A missing namespace is created and deleted at the end with the pod; the
//! session's objects go with the session. The test of a silent agent (#104)
//! waits out the real grace of two minutes.

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::core::v1::{Namespace, Pod, Service};
use k8s_openapi::api::rbac::v1::Role;
use kubach_mcp::transport::{HttpEndpoint, HttpOptions, MCP_PATH, serve_shared_http};
use kubach_mcp::{
    Access, McpServer, Profile, RequestKind, ScriptedConfirmer, SecurityPolicy, Session,
    SessionSpec, SharedServer,
};
use kube::api::{Api, DeleteParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const POD: &str = "kubach-test-forward";

async fn config() -> (Config, String, String) {
    let context = std::env::var("KUBACH_TEST_CONTEXT").expect("KUBACH_TEST_CONTEXT");
    let namespace = std::env::var("KUBACH_TEST_NAMESPACE").expect("KUBACH_TEST_NAMESPACE");
    let options = KubeConfigOptions {
        context: Some(context.clone()),
        ..Default::default()
    };
    let config = Config::from_custom_kubeconfig(Kubeconfig::read().unwrap(), &options)
        .await
        .unwrap();
    (config, context, namespace)
}

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("not an object"),
    }
}

/// Waits up to two minutes for `check`.
async fn eventually(mut check: impl AsyncFnMut() -> bool) -> bool {
    for _ in 0..240 {
        if check().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

async fn http_get(address: &str) -> Option<String> {
    let mut stream = TcpStream::connect(address).await.ok()?;
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .await
        .ok()?;
    let mut reply = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut reply))
        .await
        .ok()?
        .ok()?;
    Some(String::from_utf8_lossy(&reply).into_owned())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn the_agent_forwards_to_a_pod_until_the_session_ends() {
    let (base, context, namespace) = config().await;
    let user = Client::try_from(base.clone()).unwrap();
    let namespaces: Api<Namespace> = Api::all(user.clone());
    let created_namespace = namespaces.get_opt(&namespace).await.unwrap().is_none();
    if created_namespace {
        let object: Namespace =
            serde_json::from_value(json!({"metadata": {"name": namespace}})).unwrap();
        namespaces
            .create(&PostParams::default(), &object)
            .await
            .unwrap();
    }
    let pods: Api<Pod> = Api::namespaced(user.clone(), &namespace);
    let pod: Pod = serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": POD},
        "spec": {"containers": [{"name": "web", "image": "nginx:alpine",
                                 "ports": [{"name": "http", "containerPort": 80}]}]},
    }))
    .unwrap();
    let _ = pods
        .delete(POD, &DeleteParams::default().grace_period(0))
        .await;
    let _ = eventually(async || pods.get_opt(POD).await.unwrap().is_none()).await;
    pods.create(&PostParams::default(), &pod).await.unwrap();
    let ready = eventually(async || {
        pods.get_opt(POD).await.ok().flatten().is_some_and(|pod| {
            pod.status
                .and_then(|status| status.conditions)
                .unwrap_or_default()
                .iter()
                .any(|c| c.type_ == "Ready" && c.status == "True")
        })
    })
    .await;
    assert!(ready, "pod {POD} did not become ready");

    let policy = SecurityPolicy::with_minimum(Access::profile(Profile::Debug));
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: Vec::new(),
        profile: Profile::Debug,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);
    let outcome = server
        .call(
            "test",
            "port_forward_start",
            args(json!({"target": format!("pod/{POD}"), "port": "http", "namespace": namespace})),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(
        confirmer
            .requests()
            .iter()
            .any(|request| matches!(request.kind, RequestKind::PortForward { .. }))
    );
    let json_start = outcome.text.find('{').unwrap();
    let result: Value = serde_json::from_str(&outcome.text[json_start..]).unwrap();
    let address = result["address"].as_str().unwrap().to_owned();
    assert!(address.starts_with("127.0.0.1:"), "{address}");

    // A temporary ServiceAccount's Role has port-forward while it runs.
    let roles: Api<Role> = Api::namespaced(user.clone(), &namespace);
    let role = format!("kubach-mcp-{}", session.id());
    if let Some(role) = roles.get_opt(&role).await.unwrap() {
        let resources: Vec<String> = role
            .rules
            .unwrap_or_default()
            .into_iter()
            .flat_map(|rule| rule.resources.unwrap_or_default())
            .collect();
        assert!(
            resources.iter().any(|r| r == "pods/portforward"),
            "{resources:?}"
        );
    }

    let reply = http_get(&address).await.unwrap_or_default();
    assert!(reply.starts_with("HTTP/1."), "{reply}");

    session.close().await.unwrap();
    assert!(session.forwards().manager().list().is_empty());
    assert!(TcpStream::connect(&address).await.is_err() || http_get(&address).await.is_none());
    let _ = pods
        .delete(POD, &DeleteParams::default().grace_period(0))
        .await;
    if created_namespace {
        let _ = namespaces
            .delete(&namespace, &DeleteParams::default())
            .await;
    }
}

const SERVICE: &str = "kubach-test-silent";
const TOKEN: &str = "tttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttt";

/// A pod `name` behind [`SERVICE`]. Its nginx waits for a request's headers
/// longer than an agent's silence takes, the default is a minute.
fn web_pod(name: &str) -> Pod {
    let start = "sed -i '/^http {/a client_header_timeout 600s;' /etc/nginx/nginx.conf \
                 && exec nginx -g 'daemon off;'";
    serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": name, "labels": {"app": SERVICE}},
        "spec": {"containers": [{"name": "web", "image": "nginx:alpine",
                                 "command": ["sh", "-c", start],
                                 "ports": [{"name": "http", "containerPort": 80}]}]},
    }))
    .unwrap()
}

async fn wait_ready(pods: &Api<Pod>, name: &str) {
    let ready = eventually(async || {
        pods.get_opt(name).await.ok().flatten().is_some_and(|pod| {
            pod.status
                .and_then(|status| status.conditions)
                .unwrap_or_default()
                .iter()
                .any(|c| c.type_ == "Ready" && c.status == "True")
        })
    })
    .await;
    assert!(ready, "pod {name} did not become ready");
}

/// Sends one MCP request to `endpoint`; returns the response head.
async fn post(endpoint: &HttpEndpoint, session: Option<&str>, body: &Value) -> String {
    let port = endpoint.addr.port();
    let session = session
        .map(|id| format!("mcp-session-id: {id}\r\nmcp-protocol-version: 2025-06-18\r\n"))
        .unwrap_or_default();
    let body = body.to_string();
    let head = format!(
        "POST {MCP_PATH} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nauthorization: Bearer {TOKEN}\r\n\
         content-type: application/json\r\naccept: application/json, text/event-stream\r\n\
         {session}content-length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(endpoint.addr).await.unwrap();
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        if line == "\r\n" || line.is_empty() {
            return response;
        }
        response.push_str(&line);
    }
}

/// An agent connects as one that holds no `GET` stream.
async fn connect_agent(endpoint: &HttpEndpoint) {
    let initialize = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "test", "version": "1"}},
    });
    let head = post(endpoint, None, &initialize).await;
    let id = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("mcp-session-id")
                .then(|| value.trim().to_owned())
        })
        .unwrap_or_else(|| panic!("no session id: {head}"));
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    post(endpoint, Some(&id), &initialized).await;
}

async fn start(server: &McpServer, namespace: &str) -> (u64, String) {
    let outcome = server
        .call(
            "test",
            "port_forward_start",
            args(
                json!({"target": format!("service/{SERVICE}"), "port": 80, "namespace": namespace}),
            ),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let json_start = outcome.text.find('{').unwrap();
    let result: Value = serde_json::from_str(&outcome.text[json_start..]).unwrap();
    (
        result["id"].as_u64().unwrap(),
        result["address"].as_str().unwrap().to_owned(),
    )
}

/// The entry of forward `id` in `port_forward_list`.
async fn entry(server: &McpServer, id: u64) -> Value {
    let outcome = server.call("test", "port_forward_list", Map::new()).await;
    assert!(!outcome.is_error, "{}", outcome.text);
    let list: Vec<Value> =
        serde_json::from_str(&outcome.text).unwrap_or_else(|_| panic!("{}", outcome.text));
    list.into_iter()
        .find(|forward| forward["id"] == id)
        .unwrap_or_else(|| panic!("forward {id} is not listed: {}", outcome.text))
}

/// BUG-2 of #101: a pod going away keeps the forward; two minutes of an
/// agent's silence close it, it stays listed with why, a forward in use
/// outlives the silence, and a new forward works in the same session.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn a_silent_agents_forward_closes_and_stays_listed_with_why() {
    let (base, context, namespace) = config().await;
    let user = Client::try_from(base.clone()).unwrap();
    let namespaces: Api<Namespace> = Api::all(user.clone());
    let created_namespace = namespaces.get_opt(&namespace).await.unwrap().is_none();
    if created_namespace {
        let object: Namespace =
            serde_json::from_value(json!({"metadata": {"name": namespace}})).unwrap();
        namespaces
            .create(&PostParams::default(), &object)
            .await
            .unwrap();
    }
    let pods: Api<Pod> = Api::namespaced(user.clone(), &namespace);
    let services: Api<Service> = Api::namespaced(user.clone(), &namespace);
    let (first, second) = (format!("{SERVICE}-a"), format!("{SERVICE}-b"));
    for name in [&first, &second] {
        let _ = pods
            .delete(name, &DeleteParams::default().grace_period(0))
            .await;
        let _ = eventually(async || pods.get_opt(name).await.unwrap().is_none()).await;
    }
    let _ = services.delete(SERVICE, &DeleteParams::default()).await;
    let service: Service = serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Service",
        "metadata": {"name": SERVICE},
        "spec": {"selector": {"app": SERVICE}, "ports": [{"port": 80, "targetPort": "http"}]},
    }))
    .unwrap();
    services
        .create(&PostParams::default(), &service)
        .await
        .unwrap();
    pods.create(&PostParams::default(), &web_pod(&first))
        .await
        .unwrap();
    wait_ready(&pods, &first).await;

    let policy = SecurityPolicy::with_minimum(Access::profile(Profile::Debug));
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: Vec::new(),
        profile: Profile::Debug,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), confirmer, None);
    let shared = SharedServer::new();
    shared.set(Some(server.clone()));
    let endpoint = serve_shared_http(
        shared,
        HttpOptions {
            port: 0,
            token: TOKEN.into(),
            bridge: None,
        },
    )
    .await
    .unwrap();

    let (idle, idle_address) = start(&server, &namespace).await;
    let (used, used_address) = start(&server, &namespace).await;
    assert!(http_get(&idle_address).await.is_some());

    // The pod goes away: the local port stays, only connections fail.
    pods.delete(&first, &DeleteParams::default().grace_period(0))
        .await
        .unwrap();
    let _ = eventually(async || pods.get_opt(&first).await.unwrap().is_none()).await;
    let failed = eventually(async || {
        let _ = http_get(&idle_address).await;
        entry(&server, idle).await["lastError"].is_string()
    })
    .await;
    let forward = entry(&server, idle).await;
    assert!(failed, "{forward}");
    assert_ne!(forward["state"], "closed", "{forward}");
    assert!(TcpStream::connect(&idle_address).await.is_ok());

    // A new pod behind the Service takes over on the same port.
    pods.create(&PostParams::default(), &web_pod(&second))
        .await
        .unwrap();
    wait_ready(&pods, &second).await;
    let moved = eventually(async || {
        http_get(&idle_address)
            .await
            .is_some_and(|reply| reply.starts_with("HTTP/1."))
    })
    .await;
    assert!(moved, "the forward did not reach the new pod");
    assert_eq!(entry(&server, idle).await["pod"], second.as_str());

    // The agent connects and falls silent; a connection stays open through
    // `used`, in the middle of a request.
    connect_agent(&endpoint).await;
    let mut connection = TcpStream::connect(&used_address).await.unwrap();
    connection
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(135)).await;

    assert!(session.is_active());
    assert!(TcpStream::connect(&idle_address).await.is_err());
    let forward = entry(&server, idle).await;
    assert_eq!(forward["state"], "closed", "{forward}");
    assert_eq!(forward["reason"], "disconnected", "{forward}");
    assert!(forward["closedAt"].is_string(), "{forward}");
    assert!(forward["lastError"].is_string(), "{forward}");

    let forward = entry(&server, used).await;
    assert_eq!(forward["activeConnections"], 1, "{forward}");
    connection.write_all(b"\r\n").await.unwrap();
    let mut reply = [0u8; 8];
    connection.read_exact(&mut reply).await.unwrap();
    assert!(reply.starts_with(b"HTTP/1."));

    // Its connection over, the forward follows.
    drop(connection);
    let closed = eventually(async || entry(&server, used).await["state"] == "closed").await;
    assert!(closed, "{}", entry(&server, used).await);

    // The same session goes on.
    connect_agent(&endpoint).await;
    let (_, address) = start(&server, &namespace).await;
    let reply = http_get(&address).await.unwrap_or_default();
    assert!(reply.starts_with("HTTP/1."), "{reply}");

    endpoint.shutdown().await;
    session.close().await.unwrap();
    let _ = pods
        .delete(&second, &DeleteParams::default().grace_period(0))
        .await;
    let _ = services.delete(SERVICE, &DeleteParams::default()).await;
    if created_namespace {
        let _ = namespaces
            .delete(&namespace, &DeleteParams::default())
            .await;
    }
}

const NODE_NETWORK: &str = "kubach-test-node-network";
const NODE_NETWORK_PORT: u16 = 18179;

/// A pod behind Service [`NODE_NETWORK`] that answers on
/// [`NODE_NETWORK_PORT`], in its own network or in its node's.
fn network_pod(name: &str, host_network: bool) -> Pod {
    let start = format!(
        "printf 'server {{ listen {NODE_NETWORK_PORT}; location / {{ return 200 ok; }} }}' \
         > /etc/nginx/conf.d/default.conf && exec nginx -g 'daemon off;'"
    );
    serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": name, "labels": {"app": NODE_NETWORK}},
        "spec": {"hostNetwork": host_network,
                 "containers": [{"name": "web", "image": "nginx:alpine",
                                 "command": ["sh", "-c", start],
                                 "ports": [{"name": "http",
                                            "containerPort": NODE_NETWORK_PORT}]}]},
    }))
    .unwrap()
}

/// #179: a forward approved for a pod with its own network does not move
/// to a pod in the node's network; a new forward to that pod shows the
/// user what it reaches, and works once approved.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE)"]
async fn a_pod_in_the_nodes_network_is_shown_and_not_reached_unasked() {
    let (base, context, namespace) = config().await;
    let user = Client::try_from(base.clone()).unwrap();
    let namespaces: Api<Namespace> = Api::all(user.clone());
    let created_namespace = namespaces.get_opt(&namespace).await.unwrap().is_none();
    if created_namespace {
        let object: Namespace =
            serde_json::from_value(json!({"metadata": {"name": namespace}})).unwrap();
        namespaces
            .create(&PostParams::default(), &object)
            .await
            .unwrap();
    }
    let pods: Api<Pod> = Api::namespaced(user.clone(), &namespace);
    let services: Api<Service> = Api::namespaced(user.clone(), &namespace);
    let own = format!("{NODE_NETWORK}-own");
    let node = format!("{NODE_NETWORK}-node");
    for name in [&own, &node] {
        let _ = pods
            .delete(name, &DeleteParams::default().grace_period(0))
            .await;
        let _ = eventually(async || pods.get_opt(name).await.unwrap().is_none()).await;
    }
    let _ = services
        .delete(NODE_NETWORK, &DeleteParams::default())
        .await;
    let service: Service = serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Service",
        "metadata": {"name": NODE_NETWORK},
        "spec": {"selector": {"app": NODE_NETWORK},
                 "ports": [{"port": 80, "targetPort": NODE_NETWORK_PORT}]},
    }))
    .unwrap();
    services
        .create(&PostParams::default(), &service)
        .await
        .unwrap();
    pods.create(&PostParams::default(), &network_pod(&own, false))
        .await
        .unwrap();
    wait_ready(&pods, &own).await;

    let policy = SecurityPolicy::with_minimum(Access::profile(Profile::Debug));
    let spec = SessionSpec {
        context,
        server_url: base.cluster_url.to_string(),
        namespaces: Vec::new(),
        profile: Profile::Debug,
        ttl: Some(Duration::from_secs(600)),
        hints: Vec::new(),
    };
    let session = Session::start(&base, &spec, &policy).await.unwrap();
    let confirmer = Arc::new(ScriptedConfirmer::approving());
    let server = McpServer::new(session.clone(), confirmer.clone(), None);
    let start = async || {
        let outcome = server
            .call(
                "test",
                "port_forward_start",
                args(
                    json!({"target": format!("service/{NODE_NETWORK}"), "port": 80,
                            "namespace": namespace}),
                ),
            )
            .await;
        assert!(!outcome.is_error, "{}", outcome.text);
        let json_start = outcome.text.find('{').unwrap();
        let result: Value = serde_json::from_str(&outcome.text[json_start..]).unwrap();
        (
            result["id"].as_u64().unwrap(),
            result["address"].as_str().unwrap().to_owned(),
            result["hostNetwork"].as_bool().unwrap(),
        )
    };
    let questions = || -> Vec<_> {
        confirmer
            .requests()
            .into_iter()
            .filter(|request| matches!(request.kind, RequestKind::PortForward { .. }))
            .collect()
    };
    let answers = async |address: &str| {
        eventually(async || {
            http_get(address)
                .await
                .is_some_and(|reply| reply.starts_with("HTTP/1.") && reply.ends_with("ok"))
        })
        .await
    };

    // A pod with its own network: nothing to tick, and it answers.
    let (first, first_address, host_network) = start().await;
    assert!(!host_network);
    assert!(questions()[0].dangerous.is_empty());
    assert!(answers(&first_address).await, "the pod did not answer");

    // The Service's only pod is now one in the node's network.
    pods.delete(&own, &DeleteParams::default().grace_period(0))
        .await
        .unwrap();
    let _ = eventually(async || pods.get_opt(&own).await.unwrap().is_none()).await;
    pods.create(&PostParams::default(), &network_pod(&node, true))
        .await
        .unwrap();
    wait_ready(&pods, &node).await;
    let refused = eventually(async || {
        let _ = http_get(&first_address).await;
        entry(&server, first).await["lastError"]
            .as_str()
            .is_some_and(|error| error.contains("hostNetwork"))
    })
    .await;
    let last = entry(&server, first).await;
    assert!(refused, "{last}");
    assert_eq!(last["pod"], own.as_str(), "{last}");
    let reply = http_get(&first_address).await.unwrap_or_default();
    assert!(!reply.contains("200"), "{reply}");

    // A new forward shows what it reaches and works once approved.
    let (_, second_address, host_network) = start().await;
    assert!(host_network);
    let asked = questions();
    let finding = &asked[1].dangerous;
    assert_eq!(finding.len(), 1, "{finding:?}");
    assert_eq!(finding[0].path, format!("pod/{node} .spec.hostNetwork"));
    assert!(!asked[1].allow_remember);
    assert!(
        answers(&second_address).await,
        "the node's pod did not answer"
    );

    session.close().await.unwrap();
    for name in [&own, &node] {
        let _ = pods
            .delete(name, &DeleteParams::default().grace_period(0))
            .await;
    }
    let _ = services
        .delete(NODE_NETWORK, &DeleteParams::default())
        .await;
    if created_namespace {
        let _ = namespaces
            .delete(&namespace, &DeleteParams::default())
            .await;
    }
}
