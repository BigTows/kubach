//! Kustomize diff against the cluster and GitOps "managed by" lookups on a
//! real cluster. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-88 \
//!     cargo test -p kubach-core --test kustomize_gitops_cluster -- --ignored
//! ```
//!
//! The namespace must exist and `kubectl` (or `kustomize`) be on `PATH`.
//! With `KUBACH_TEST_GITOPS_CRDS=1` the Argo CD `Application` and Flux
//! `Kustomization` CRDs are expected to be installed (only the CRDs: no
//! controllers), with an Application `guestbook` and a Kustomization `apps`
//! in the namespace; without it the kinds are expected to be absent.

use std::collections::BTreeMap;
use std::fs;

use k8s_openapi::api::core::v1::ConfigMap;
use kubach_core::discovery::ApiCatalog;
use kubach_core::gitops::{self, LinkState, ManagerKind};
use kubach_core::kustomize::{self, BuildOptions, DiffCounts};
use kube::api::{Api, DeleteParams, ObjectMeta, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};

async fn client() -> (Client, String) {
    let context = std::env::var("KUBACH_TEST_CONTEXT").expect("KUBACH_TEST_CONTEXT");
    let namespace = std::env::var("KUBACH_TEST_NAMESPACE").expect("KUBACH_TEST_NAMESPACE");
    let kubeconfig = Kubeconfig::read().unwrap();
    let options = KubeConfigOptions {
        context: Some(context),
        ..Default::default()
    };
    let config = Config::from_custom_kubeconfig(kubeconfig, &options)
        .await
        .unwrap();
    (Client::try_from(config).unwrap(), namespace)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster"]
async fn kustomize_diff_with_cluster() {
    let (client, ns) = client().await;
    let catalog = ApiCatalog::discover(&client).await.unwrap();
    let configmaps: Api<ConfigMap> = Api::namespaced(client.clone(), &ns);
    configmaps
        .delete("k88-settings", &DeleteParams::default())
        .await
        .ok();
    let live = ConfigMap {
        metadata: ObjectMeta {
            name: Some("k88-settings".into()),
            ..ObjectMeta::default()
        },
        data: Some(BTreeMap::from([("mode".into(), "green".into())])),
        ..ConfigMap::default()
    };
    configmaps
        .create(&PostParams::default(), &live)
        .await
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("kustomization.yaml"),
        format!("namespace: {ns}\nnamePrefix: k88-\nresources: [cm.yaml]\n"),
    )
    .unwrap();
    fs::write(
        dir.path().join("cm.yaml"),
        "apiVersion: v1\nkind: ConfigMap\nmetadata: {name: settings}\ndata: {mode: blue}\n---\n\
         apiVersion: v1\nkind: ConfigMap\nmetadata: {name: extra}\ndata: {a: b}\n",
    )
    .unwrap();
    let build = kustomize::build_with_path_tool(dir.path(), &BuildOptions::default())
        .await
        .unwrap();
    let results = kustomize::diff_each(&client, &catalog, &build.manifests, &ns).await;
    configmaps
        .delete("k88-settings", &DeleteParams::default())
        .await
        .unwrap();

    assert_eq!(results.len(), 2);
    // kustomize sorts the output: find the objects by name.
    let by_name = |name: &str| {
        results
            .iter()
            .map(|r| r.as_ref().unwrap())
            .find(|d| d.manifest.name == name)
            .unwrap()
    };
    let changed = by_name("k88-settings");
    assert_eq!(changed.status(), "Changed");
    assert!(changed.live_yaml.contains("green"), "{}", changed.live_yaml);
    assert!(changed.desired_yaml.contains("blue"));
    assert!(by_name("k88-extra").is_new());
    assert_eq!(DiffCounts::of(&results).describe(), "1 new · 1 changed");

    // A broken kustomization reports kustomize's own message.
    fs::write(
        dir.path().join("kustomization.yaml"),
        "resources: [gone.yaml]\n",
    )
    .unwrap();
    let error = kustomize::build_with_path_tool(dir.path(), &BuildOptions::default())
        .await
        .unwrap_err();
    assert_eq!(error.title(), "Build failed");
    assert!(error.details().contains("gone.yaml"), "{}", error.details());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster"]
async fn managed_by_lookup() {
    let (client, ns) = client().await;
    let catalog = ApiCatalog::discover(&client).await.unwrap();
    let with_crds = std::env::var("KUBACH_TEST_GITOPS_CRDS").is_ok_and(|v| v == "1");
    let meta = ObjectMeta {
        labels: Some(BTreeMap::from([
            ("kustomize.toolkit.fluxcd.io/name".into(), "apps".into()),
            ("kustomize.toolkit.fluxcd.io/namespace".into(), ns.clone()),
            ("app.kubernetes.io/instance".into(), "guestbook".into()),
            ("argocd.argoproj.io/instance".into(), "missing-app".into()),
        ])),
        ..ObjectMeta::default()
    };
    let links = gitops::resolve(&client, &catalog, gitops::managers(&meta)).await;
    let state = |kind: ManagerKind, name: &str| {
        links
            .iter()
            .find(|l| l.manager.kind == kind && l.manager.name == name)
            .map(|l| l.state.clone())
    };
    if with_crds {
        assert_eq!(
            state(ManagerKind::FluxKustomization, "apps"),
            Some(LinkState::Found)
        );
        // The weak instance label leads to an existing Application, found
        // across namespaces.
        let guestbook = links
            .iter()
            .find(|l| l.manager.name == "guestbook")
            .expect("guestbook link");
        assert!(guestbook.is_found());
        assert_eq!(guestbook.manager.namespace.as_deref(), Some(ns.as_str()));
        assert_eq!(
            state(ManagerKind::ArgoApplication, "missing-app"),
            Some(LinkState::Missing)
        );
    } else {
        assert_eq!(
            state(ManagerKind::FluxKustomization, "apps"),
            Some(LinkState::NotServed)
        );
        // Uncertain and not found: dropped.
        assert_eq!(state(ManagerKind::ArgoApplication, "guestbook"), None);
    }
}
