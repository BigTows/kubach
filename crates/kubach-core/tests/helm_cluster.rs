//! Helm upgrade and uninstall through the user's `helm` against a real
//! cluster. Ignored by default; run with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-99 \
//!     cargo test -p kubach-core --test helm_cluster -- --ignored
//! ```
//!
//! Needs `helm` (3.13+) on `PATH`. The namespace must exist; the test installs
//! and removes its own release `kubach-helm-test`.

use std::path::Path;
use std::time::Duration;

use kubach_core::helm::cli::{
    self, HelmCommand, HelmTarget, UpgradeChart, UpgradeOutcome, UpgradeRequest,
};
use kubach_core::helm::manifest::ChangeKind;
use kubach_core::helm::{
    Driver, HelmError, ReleaseStatus, Releases, Sensitive, diff_manifests, manifest_resources,
};
use kubach_core::settings::find_on_path;
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};

const RELEASE: &str = "kubach-helm-test";
const TIMEOUT: Duration = Duration::from_secs(120);

async fn client() -> (Client, String, String) {
    let context = std::env::var("KUBACH_TEST_CONTEXT").expect("KUBACH_TEST_CONTEXT");
    let namespace = std::env::var("KUBACH_TEST_NAMESPACE").expect("KUBACH_TEST_NAMESPACE");
    let kubeconfig = Kubeconfig::read().unwrap();
    let options = KubeConfigOptions {
        context: Some(context.clone()),
        ..Default::default()
    };
    let config = Config::from_custom_kubeconfig(kubeconfig, &options)
        .await
        .unwrap();
    (Client::try_from(config).unwrap(), context, namespace)
}

/// A chart with one ConfigMap built from `greeting`.
fn write_chart(dir: &Path) {
    std::fs::create_dir_all(dir.join("templates")).unwrap();
    std::fs::write(
        dir.join("Chart.yaml"),
        "apiVersion: v2\nname: greeter\nversion: 0.1.0\n",
    )
    .unwrap();
    std::fs::write(dir.join("values.yaml"), "greeting: hello\n").unwrap();
    std::fs::write(
        dir.join("templates/greeting.yaml"),
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {{ .Release.Name }}-greeting\n\
         data:\n  text: {{ .Values.greeting | quote }}\n",
    )
    .unwrap();
}

fn helm_command(args: &[&str], target: &HelmTarget) -> HelmCommand {
    let mut command = HelmCommand::uninstall(target, RELEASE, false);
    // `uninstall <pinning flags> -- <release>`: keep the pinning flags.
    let pinned = command.args[1..command.args.len() - 2].to_vec();
    command.args = args
        .iter()
        .map(std::ffi::OsString::from)
        .chain(pinned)
        .collect();
    command
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE) and helm"]
async fn upgrade_and_uninstall() {
    let (client, context, namespace) = client().await;
    let helm = find_on_path("helm").expect("helm on PATH");
    let helm = Some(helm.as_path());
    let target = HelmTarget {
        context,
        kubeconfig: cli::kubeconfig_files(&[]),
        namespace: namespace.clone(),
        driver: Driver::Secret,
        secret_env: Default::default(),
    };
    let releases = Releases::new(client);
    let charts = tempfile::tempdir().unwrap();
    write_chart(charts.path());
    let chart_path = charts.path().to_string_lossy().into_owned();

    // A leftover of an interrupted run.
    cli::run(
        helm,
        &HelmCommand::uninstall(&target, RELEASE, false),
        TIMEOUT,
    )
    .await
    .ok();
    cli::run(
        helm,
        &helm_command(&["install", RELEASE, &chart_path], &target),
        TIMEOUT,
    )
    .await
    .unwrap();

    let installed = releases.get(&namespace, RELEASE, None).await.unwrap();
    let resources: Vec<String> = manifest_resources(installed.manifest.expose())
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(resources, [format!("ConfigMap {RELEASE}-greeting")]);

    // Preview with the chart kept in the release: nothing is written.
    let chart = releases
        .stored_chart(&namespace, RELEASE, installed.revision)
        .await
        .unwrap();
    let request = UpgradeRequest {
        release: RELEASE.into(),
        chart: UpgradeChart::Stored(chart),
        values: Sensitive::new("greeting: hi\n".into()),
    };
    let UpgradeOutcome::Preview(preview) = cli::upgrade(helm, &target, &request, true, TIMEOUT)
        .await
        .unwrap()
    else {
        panic!("expected a preview");
    };
    assert_eq!(preview.revision, installed.revision + 1);
    let diff = diff_manifests(installed.manifest.expose(), preview.manifest.expose());
    assert_eq!(diff.changes.len(), 1);
    assert_eq!(diff.changes[0].kind, ChangeKind::Changed);
    assert!(
        diff.text.expose().contains("+  text: \"hi\""),
        "{:?}",
        diff.text.expose()
    );
    let unchanged = releases.get(&namespace, RELEASE, None).await.unwrap();
    assert_eq!(unchanged.revision, installed.revision);

    // The real upgrade writes revision 2 with the new values only.
    cli::upgrade(helm, &target, &request, false, TIMEOUT)
        .await
        .unwrap();
    let upgraded = releases.get(&namespace, RELEASE, None).await.unwrap();
    assert_eq!(upgraded.revision, installed.revision + 1);
    assert_eq!(upgraded.status, ReleaseStatus::Deployed);
    assert_eq!(
        upgraded.user_values.expose(),
        &serde_json::json!({"greeting": "hi"})
    );

    // A chart path works the same way.
    let by_path = UpgradeRequest {
        chart: UpgradeChart::Reference {
            chart: chart_path.clone(),
            version: None,
        },
        values: Sensitive::new(String::new()),
        ..request
    };
    let UpgradeOutcome::Preview(preview) = cli::upgrade(helm, &target, &by_path, true, TIMEOUT)
        .await
        .unwrap()
    else {
        panic!("expected a preview");
    };
    assert!(preview.manifest.expose().contains("\"hello\""));

    // Uninstall keeping history, then for good.
    cli::run(
        helm,
        &HelmCommand::uninstall(&target, RELEASE, true),
        TIMEOUT,
    )
    .await
    .unwrap();
    let kept = releases.get(&namespace, RELEASE, None).await.unwrap();
    assert_eq!(kept.status, ReleaseStatus::Uninstalled);
    cli::run(
        helm,
        &HelmCommand::uninstall(&target, RELEASE, false),
        TIMEOUT,
    )
    .await
    .unwrap();
    assert!(matches!(
        releases.history(&namespace, RELEASE).await,
        Err(HelmError::NotFound { .. })
    ));
}

/// #292: with the namespaces unlisted, a `GET` decides `--create-namespace`;
/// an install into a new namespace creates it, and the flag left on for a
/// namespace that already exists (an unverified one) does not break helm.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster (KUBACH_TEST_CONTEXT, KUBACH_TEST_NAMESPACE) and helm"]
async fn install_into_a_new_namespace_when_unlisted() {
    use k8s_openapi::api::core::v1::Namespace;
    use kubach_core::helm::cli::InstallRequest;
    use kubach_core::helm::target_namespace::{self, Listing, Lookup, NamespaceStatus};
    use kube::api::{Api, DeleteParams};

    let (client, context, namespace) = client().await;
    let helm = find_on_path("helm").expect("helm on PATH");
    let helm = Some(helm.as_path());
    let fresh = format!("{namespace}-new");
    let charts = tempfile::tempdir().unwrap();
    write_chart(charts.path());
    let request = |release: &str, create_namespace: bool| InstallRequest {
        release: release.into(),
        chart: charts.path().to_string_lossy().into_owned(),
        version: None,
        values: Sensitive::new(String::new()),
        create_namespace,
    };
    let target = |namespace: &str| HelmTarget {
        context: context.clone(),
        kubeconfig: cli::kubeconfig_files(&[]),
        namespace: namespace.into(),
        driver: Driver::Secret,
        secret_env: Default::default(),
    };
    let namespaces: Api<Namespace> = Api::all(client.clone());
    namespaces
        .delete(&fresh, &DeleteParams::default())
        .await
        .ok();

    // The existing namespace is found; the new one is not.
    let found = target_namespace::lookup(&client, &namespace).await;
    assert_eq!(found, Lookup::Found);
    let existing =
        target_namespace::decide(&namespace, Listing::<&str>::Unlisted(Some(found)), None);
    assert_eq!(existing.create, Some(false));
    let missing = target_namespace::lookup(&client, &fresh).await;
    assert_eq!(missing, Lookup::NotFound);
    let new = target_namespace::decide(&fresh, Listing::<&str>::Unlisted(Some(missing)), None);
    assert_eq!(new.status, NamespaceStatus::New);
    assert_eq!(new.create, Some(true));

    // Install into the new namespace with the decided flag.
    let outcome = cli::install(
        helm,
        &target(&fresh),
        &request(RELEASE, new.create.unwrap()),
        false,
        TIMEOUT,
    )
    .await
    .unwrap();
    assert!(matches!(outcome, UpgradeOutcome::Applied(_)));
    assert_eq!(
        target_namespace::lookup(&client, &fresh).await,
        Lookup::Found
    );

    // An unverified namespace gets the flag by default; it exists here.
    let unverified = target_namespace::decide(
        &namespace,
        Listing::<&str>::Unlisted(Some(Lookup::Unknown { forbidden: true })),
        None,
    );
    assert_eq!(unverified.create, Some(true));
    let existing_release = "kubach-helm-test-292";
    cli::run(
        helm,
        &HelmCommand::uninstall(&target(&namespace), existing_release, false),
        TIMEOUT,
    )
    .await
    .ok();
    cli::install(
        helm,
        &target(&namespace),
        &request(existing_release, unverified.create.unwrap()),
        false,
        TIMEOUT,
    )
    .await
    .unwrap();

    for (namespace, release) in [(&fresh, RELEASE), (&namespace, existing_release)] {
        cli::run(
            helm,
            &HelmCommand::uninstall(&target(namespace), release, false),
            TIMEOUT,
        )
        .await
        .unwrap();
    }
    namespaces
        .delete(&fresh, &DeleteParams::default())
        .await
        .unwrap();
}
