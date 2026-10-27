//! Kubach with the `helm` on `PATH`, Helm 3 or Helm 4 (#403): repositories
//! (`repo add`/`update`/`remove` and the native index search), `show
//! values`/`readme`, install with a server dry run and with progress,
//! upgrade from an OCI registry, rollback, the native list and history,
//! the ConfigMap driver and uninstall. Ignored by default; run once per
//! Helm line with
//!
//! ```sh
//! KUBACH_TEST_CONTEXT=minikube KUBACH_TEST_NAMESPACE=kubach-test-403 \
//!     HELM_CONFIG_HOME=$tmp/config HELM_CACHE_HOME=$tmp/cache HELM_DATA_HOME=$tmp/data \
//!     PATH=$dir_with_helm:$PATH \
//!     cargo test -p kubach-core --test helm_compat_cluster -- --ignored
//! ```
//!
//! The Helm homes must be set (the test adds and removes a repository).
//! Needs the namespace and network access to stefanprodan.github.io and
//! ghcr.io (the podinfo chart, no CRDs). Installs and removes its own
//! releases `kubach-compat` and `kubach-compat-cm`.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kubach_core::helm::cli::{
    self, HelmCommand, HelmTarget, HelmWatch, InstallRequest, ShowKind, UpgradeChart,
    UpgradeOutcome, UpgradeRequest,
};
use kubach_core::helm::repo::{self, HelmPaths, SearchOptions};
use kubach_core::helm::{Driver, HelmError, ReleaseStatus, Releases, Sensitive};
use kubach_core::settings::find_on_path;
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};

const RELEASE: &str = "kubach-compat";
const CM_RELEASE: &str = "kubach-compat-cm";
const REPO: &str = "kubach-compat-podinfo";
const REPO_URL: &str = "https://stefanprodan.github.io/podinfo";
const OCI_CHART: &str = "oci://ghcr.io/stefanprodan/charts/podinfo";
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

/// A chart with one ConfigMap.
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

fn watch(phases: &Arc<Mutex<Vec<String>>>) -> HelmWatch {
    let phases = phases.clone();
    HelmWatch {
        on_phase: Some(Arc::new(move |phase| phases.lock().unwrap().push(phase))),
        cancel: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a cluster, helm, Helm homes and network access"]
async fn helm_line_compatibility() {
    for var in ["HELM_CONFIG_HOME", "HELM_CACHE_HOME", "HELM_DATA_HOME"] {
        assert!(
            std::env::var_os(var).is_some(),
            "{var} must point to a scratch directory"
        );
    }
    let (client, context, namespace) = client().await;
    let helm_path = find_on_path("helm").expect("helm on PATH");
    let version = kubach_core::tools::helm::binary_version(&helm_path)
        .await
        .expect("helm version");
    let helm = Some(helm_path.as_path());
    let target = |driver| HelmTarget {
        context: context.clone(),
        kubeconfig: cli::kubeconfig_files(&[]),
        namespace: namespace.clone(),
        driver,
        secret_env: Default::default(),
    };
    let releases = Releases::new(client.clone());
    for (release, driver) in [(RELEASE, Driver::Secret), (CM_RELEASE, Driver::ConfigMap)] {
        cli::run(
            helm,
            &HelmCommand::uninstall(&target(driver), release, false),
            TIMEOUT,
        )
        .await
        .ok();
    }

    // Repositories: helm writes, Kubach reads the index natively.
    cli::run(helm, &HelmCommand::repo_add(REPO, REPO_URL, None), TIMEOUT)
        .await
        .unwrap();
    cli::run(helm, &HelmCommand::repo_update(&[REPO]), TIMEOUT)
        .await
        .unwrap();
    let paths = HelmPaths::from_env().unwrap();
    let loaded = repo::load(&paths, &[]).unwrap();
    let hits = repo::search(&loaded, "podinfo", SearchOptions::default());
    let hit = hits
        .iter()
        .find(|hit| hit.repository == REPO)
        .unwrap_or_else(|| panic!("{version}: podinfo not found in {loaded:?}"));
    let chart = hit.reference();
    assert_eq!(chart, format!("{REPO}/podinfo"));
    let chart_version = hit.version.version.clone();
    assert!(repo::find_chart(&loaded, REPO, "podinfo").is_some_and(|v| !v.is_empty()));

    // Chart details.
    let values = cli::run(
        helm,
        &HelmCommand::show(ShowKind::Values, &chart, Some(&chart_version)),
        TIMEOUT,
    )
    .await
    .unwrap();
    assert!(values.contains("replicaCount"), "{version}: {values}");
    let readme = cli::run(
        helm,
        &HelmCommand::show(ShowKind::Readme, &chart, Some(&chart_version)),
        TIMEOUT,
    )
    .await
    .unwrap();
    assert!(readme.to_lowercase().contains("podinfo"), "{version}");

    // Install: the server dry run writes nothing.
    let request = InstallRequest {
        release: RELEASE.into(),
        chart: chart.clone(),
        version: Some(chart_version.clone()),
        values: Sensitive::new("replicaCount: 1\n".into()),
        create_namespace: false,
    };
    let secret = target(Driver::Secret);
    let UpgradeOutcome::Preview(preview) = cli::install(helm, &secret, &request, true, TIMEOUT)
        .await
        .unwrap()
    else {
        panic!("{version}: expected a preview");
    };
    assert_eq!(preview.revision, 1);
    assert!(preview.manifest.expose().contains("kind: Deployment"));
    assert!(matches!(
        releases.history(&namespace, RELEASE).await,
        Err(HelmError::NotFound { .. })
    ));

    // The real install, with phases from the `--debug` log (#382).
    let phases = Arc::new(Mutex::new(Vec::new()));
    let outcome = cli::install_watched(
        helm,
        &secret,
        &request,
        false,
        TIMEOUT,
        Some(watch(&phases)),
    )
    .await
    .unwrap();
    let UpgradeOutcome::Applied(output) = outcome else {
        panic!("{version}: expected an install");
    };
    assert!(output.contains("STATUS: deployed"), "{version}: {output}");
    assert!(!output.contains("COMPUTED VALUES"), "{version}: {output}");
    assert!(!phases.lock().unwrap().is_empty(), "{version}: no phases");
    let installed = releases.get(&namespace, RELEASE, None).await.unwrap();
    assert_eq!(installed.revision, 1);
    assert_eq!(installed.status, ReleaseStatus::Deployed);
    assert_eq!(installed.chart.name, "podinfo");
    assert_eq!(
        installed.user_values.expose(),
        &serde_json::json!({"replicaCount": 1})
    );

    // Upgrade from the OCI registry: preview, then apply with progress.
    let upgrade = UpgradeRequest {
        release: RELEASE.into(),
        chart: UpgradeChart::Reference {
            chart: OCI_CHART.into(),
            version: Some(chart_version.clone()),
        },
        values: Sensitive::new("replicaCount: 1\nui:\n  message: compat\n".into()),
    };
    let UpgradeOutcome::Preview(preview) = cli::upgrade(helm, &secret, &upgrade, true, TIMEOUT)
        .await
        .unwrap()
    else {
        panic!("{version}: expected a preview");
    };
    assert_eq!(preview.revision, 2);
    assert!(preview.manifest.expose().contains("compat"));
    phases.lock().unwrap().clear();
    cli::upgrade_watched(
        helm,
        &secret,
        &upgrade,
        false,
        TIMEOUT,
        Some(watch(&phases)),
    )
    .await
    .unwrap();
    assert!(!phases.lock().unwrap().is_empty(), "{version}: no phases");
    let upgraded = releases.get(&namespace, RELEASE, None).await.unwrap();
    assert_eq!(upgraded.revision, 2);
    assert_eq!(upgraded.status, ReleaseStatus::Deployed);

    // Rollback to the first revision writes a third.
    cli::run(helm, &HelmCommand::rollback(&secret, RELEASE, 1), TIMEOUT)
        .await
        .unwrap();
    let history = releases.history(&namespace, RELEASE).await.unwrap();
    let revisions: Vec<(u32, ReleaseStatus)> = history
        .iter()
        .map(|r| (r.revision, r.status.clone()))
        .collect();
    assert!(
        revisions.contains(&(3, ReleaseStatus::Deployed))
            && revisions.contains(&(2, ReleaseStatus::Superseded))
            && revisions.contains(&(1, ReleaseStatus::Superseded)),
        "{version}: {revisions:?}"
    );
    let rolled_back = releases.get(&namespace, RELEASE, None).await.unwrap();
    assert_eq!(
        rolled_back.user_values.expose(),
        &serde_json::json!({"replicaCount": 1})
    );
    let listed = releases.list(Some(&namespace)).await.unwrap();
    assert!(
        listed
            .iter()
            .any(|r| r.name == RELEASE && r.revision == 3 && r.chart.name == "podinfo"),
        "{version}: {listed:?}"
    );

    // The ConfigMap storage driver.
    let charts = tempfile::tempdir().unwrap();
    write_chart(charts.path());
    let configmap = target(Driver::ConfigMap);
    let local = InstallRequest {
        release: CM_RELEASE.into(),
        chart: charts.path().to_string_lossy().into_owned(),
        version: None,
        values: Sensitive::new("greeting: hi\n".into()),
        create_namespace: false,
    };
    cli::install(helm, &configmap, &local, false, TIMEOUT)
        .await
        .unwrap();
    let in_configmap = Releases::with_driver(client.clone(), Driver::ConfigMap)
        .get(&namespace, CM_RELEASE, None)
        .await
        .unwrap();
    assert_eq!(in_configmap.status, ReleaseStatus::Deployed);
    assert!(matches!(
        releases.history(&namespace, CM_RELEASE).await,
        Err(HelmError::NotFound { .. })
    ));

    // Uninstall both; the repository goes too.
    for (release, driver) in [(RELEASE, Driver::Secret), (CM_RELEASE, Driver::ConfigMap)] {
        cli::run(
            helm,
            &HelmCommand::uninstall(&target(driver), release, false),
            TIMEOUT,
        )
        .await
        .unwrap();
    }
    assert!(matches!(
        releases.history(&namespace, RELEASE).await,
        Err(HelmError::NotFound { .. })
    ));
    cli::run(helm, &HelmCommand::repo_remove(REPO), TIMEOUT)
        .await
        .unwrap();
    let loaded = repo::load(&paths, &[]).unwrap();
    assert!(loaded.iter().all(|r| r.repository.name != REPO));
}
