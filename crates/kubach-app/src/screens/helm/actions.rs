//! Changes through the user's `helm` (ADR 0006): rollback, uninstall,
//! upgrade and install with their confirmations, one call at a time with a
//! progress toast, and the action buttons of an open release.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _};
use gpui_kit::*;
use kubach_core::helm::cli::{
    self, HelmCommand, HelmTarget, HelmWatch, InstallRequest, UpgradeRequest,
};
use kubach_core::helm::{ReleaseStatus, claims, manifest_resources, updates};
use kubach_core::settings::find_on_path;
use kubach_core::status_bar;
use kubach_core::{Connection, runtime};
use kubach_i18n::t;
use kubach_ui::{ActionToast, BulkResult, ConfirmDialog, Operations, Tone};

use super::super::helm_install::helm_output;
use super::super::helm_release::{claims_notice, uninstall_message};
use super::super::helm_upgrade::{HelmUpgrade, UpgradeEvent};
use super::{
    ActionRun, AfterSuccess, Detail, HelmPage, HelmScreen, InstalledNotice, ShowHelmPage,
    UninstallLookup,
};
use crate::action_context;
use crate::settings::AppSettings;

/// Pause between updates of the toast with helm's phases.
const PHASE_BATCH: Duration = Duration::from_millis(200);

/// A change to a release, announced in the status bar when it starts and
/// when it ends (#459).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HelmChange {
    Install,
    Upgrade,
    Rollback,
    Uninstall,
}

/// Status bar notices of a `change` to release `name`.
pub(super) struct HelmNotice {
    change: HelmChange,
    name: String,
}

impl HelmNotice {
    pub(super) fn new(change: HelmChange, name: impl Into<String>) -> Self {
        Self {
            change,
            name: name.into(),
        }
    }

    fn started(&self) -> String {
        let name = self.name.clone();
        match self.change {
            HelmChange::Install => t!("helm-notice-install-started", name = name),
            HelmChange::Upgrade => t!("helm-notice-upgrade-started", name = name),
            HelmChange::Rollback => t!("helm-notice-rollback-started", name = name),
            HelmChange::Uninstall => t!("helm-notice-uninstall-started", name = name),
        }
    }

    /// The outcome: done, or failed with the first line of `error`.
    fn finished(&self, outcome: &Result<String, String>) -> (String, Tone) {
        let name = self.name.clone();
        let Err(error) = outcome else {
            let text = match self.change {
                HelmChange::Install => t!("helm-notice-install-done", name = name),
                HelmChange::Upgrade => t!("helm-notice-upgrade-done", name = name),
                HelmChange::Rollback => t!("helm-notice-rollback-done", name = name),
                HelmChange::Uninstall => t!("helm-notice-uninstall-done", name = name),
            };
            return (text, Tone::Success);
        };
        let error = status_bar::short_error(error);
        let text = match self.change {
            HelmChange::Install => t!("helm-notice-install-failed", name = name, error = error),
            HelmChange::Upgrade => t!("helm-notice-upgrade-failed", name = name, error = error),
            HelmChange::Rollback => {
                t!("helm-notice-rollback-failed", name = name, error = error)
            }
            HelmChange::Uninstall => {
                t!("helm-notice-uninstall-failed", name = name, error = error)
            }
        };
        (text, Tone::Danger)
    }
}

/// Title of a change's toast and the command it shows.
struct ActionText {
    title: String,
    command: String,
}

impl ActionText {
    fn new(title: String, command: String) -> Self {
        Self { title, command }
    }
}

/// What the Stop confirmation of a running install or upgrade says.
struct StopHelm {
    title: String,
    message: String,
}

impl StopHelm {
    /// Stopping an install; a killed helm leaves the release
    /// `pending-install`.
    fn install(release: &str) -> Self {
        Self {
            title: t!("helm-stop-install-title", release = release),
            message: Self::message("pending-install"),
        }
    }

    /// Stopping an upgrade; a killed helm leaves the release
    /// `pending-upgrade`.
    fn upgrade(release: &str) -> Self {
        Self {
            title: t!("helm-stop-upgrade-title", release = release),
            message: Self::message("pending-upgrade"),
        }
    }

    fn message(pending: &str) -> String {
        t!(
            "helm-stop-message",
            seconds = kubach_core::process::CANCEL_GRACE.as_secs(),
            status = pending
        )
    }
}

impl HelmScreen {
    /// `helm` from settings, else from `PATH`, else downloaded by Kubach.
    pub(in crate::screens) fn helm_path(cx: &App) -> Option<PathBuf> {
        match AppSettings::try_global(cx) {
            Some(settings) => settings.read(cx).get().tools.helm(),
            None => find_on_path("helm"),
        }
    }

    pub(super) fn helm_target(&self, namespace: &str, cx: &App) -> Option<HelmTarget> {
        let cluster = self.cluster.read(cx);
        Some(HelmTarget {
            context: cluster.active()?.name.clone(),
            kubeconfig: cli::kubeconfig_files(cluster.kubeconfig_paths()),
            namespace: namespace.to_owned(),
            driver: self.releases.read(cx).driver().driver,
            secret_env: Default::default(),
        })
    }

    /// The active connection, when `target` was made for its context; a
    /// confirmation captures it and runs only while it stays active (#344).
    fn target_connection(&self, target: &HelmTarget, cx: &App) -> Option<Connection> {
        self.cluster
            .read(cx)
            .connection()
            .filter(|connection| connection.context == target.context)
            .cloned()
    }

    /// Runs `run` on the screen when the user confirms, unless the context
    /// changed since `connection` was captured.
    fn guarded(
        screen: &WeakEntity<Self>,
        connection: &Connection,
        cx: &mut App,
        run: impl FnOnce(&mut Self, &mut Context<Self>),
    ) {
        screen
            .update(cx, |this, cx| {
                if action_context::still_active(&this.cluster, connection, cx) {
                    run(this, cx);
                }
            })
            .ok();
    }

    /// Asks to confirm, then rolls the open release back to the picked
    /// revision with `helm rollback`.
    fn request_rollback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else {
            return;
        };
        let (Some(revision), Some(latest)) = (detail.revision, detail.latest()) else {
            return;
        };
        let Some(target) = self.helm_target(&detail.namespace, cx) else {
            return;
        };
        let Some(connection) = self.target_connection(&target, cx) else {
            return;
        };
        let name = detail.name.clone();
        let command = HelmCommand::rollback(&target, &name, revision);
        let next = latest.revision + 1;
        let message = t!(
            "helm-rollback-message",
            next = next,
            name = name.clone(),
            namespace = target.namespace.clone(),
            revision = revision
        );
        let display = command.display();
        let screen = cx.entity().downgrade();
        let captured = connection.clone();
        let handle = ConfirmDialog::open(window, cx, move |_, _| {
            let (screen, command, name) = (screen.clone(), command.clone(), name.clone());
            let connection = captured.clone();
            ConfirmDialog::new(t!(
                "helm-rollback-title",
                name = name.clone(),
                revision = revision
            ))
            .context(connection.context.clone())
            .message(message)
            .command(display.clone())
            .confirm_label(t!("helm-rollback-confirm"))
            .danger()
            .on_confirm(move |_, cx| {
                let (command, name) = (command.clone(), name.clone());
                Self::guarded(&screen, &connection, cx, |this, cx| {
                    let title = t!(
                        "helm-rollback-toast",
                        name = name.clone(),
                        revision = revision
                    );
                    let notice = HelmNotice::new(HelmChange::Rollback, name);
                    this.run_helm(title, command, Some(notice), AfterSuccess::Reload, cx)
                });
            })
        });
        self.confirms.track(handle, connection);
    }

    /// Looks up the claims the open release would leave behind (#273),
    /// then asks to confirm the uninstall.
    fn request_uninstall(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else {
            return;
        };
        let Some(latest) = detail.latest() else {
            return;
        };
        // The release is shown for the active connection; the lookup, the
        // uninstall and the claim delete all stay in it (#344).
        let Some(connection) = self.cluster.read(cx).connection().cloned() else {
            return;
        };
        let client = connection.client.clone();
        let (namespace, name) = (detail.namespace.clone(), detail.name.clone());
        let manifest = latest.manifest.clone();
        let (task_namespace, task_name) = (namespace.clone(), name.clone());
        let handle = runtime::spawn(async move {
            claims::list_claims(&client, &task_namespace)
                .await
                .map(|found| {
                    claims::leftover_claims(&task_name, &task_namespace, manifest.expose(), &found)
                })
                .map_err(|error| error.to_string())
        });
        let (lookup_namespace, lookup_name) = (namespace.clone(), name.clone());
        let task = cx.spawn_in(window, async move |this, cx| {
            let leftover = handle.await.unwrap_or_else(|error| {
                Err(t!("helm-lookup-interrupted", error = error.to_string()))
            });
            this.update_in(cx, |this, window, cx| {
                // Another release opened (or the context changed, which
                // drops the lookup) since: the result is not for it.
                let current = this
                    .uninstall_lookup
                    .as_ref()
                    .is_some_and(|lookup| lookup.is_for(&lookup_namespace, &lookup_name));
                if !current {
                    return;
                }
                this.uninstall_lookup = None;
                let shown = this
                    .detail
                    .as_ref()
                    .is_some_and(|d| d.namespace == lookup_namespace && d.name == lookup_name);
                if shown && action_context::still_active(&this.cluster, &connection, cx) {
                    this.confirm_uninstall(leftover, connection, window, cx);
                }
                cx.notify();
            })
            .ok();
        });
        self.uninstall_lookup = Some(UninstallLookup {
            namespace,
            name,
            _task: task,
        });
        cx.notify();
    }

    /// Asks to confirm, listing what Helm deletes and the claims that stay,
    /// then uninstalls the open release with `helm uninstall` (optionally
    /// `--keep-history`) and deletes those claims when asked.
    fn confirm_uninstall(
        &mut self,
        leftover: Result<Vec<claims::LeftoverClaim>, String>,
        connection: Connection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(detail) = &self.detail else {
            return;
        };
        let Some(latest) = detail.latest() else {
            return;
        };
        let Some(target) = self.helm_target(&detail.namespace, cx) else {
            return;
        };
        let name = detail.name.clone();
        let names = leftover
            .as_ref()
            .map(|claims| claims.iter().map(|claim| claim.name.clone()).collect())
            .map_err(Clone::clone);
        let notice = claims_notice(&names);
        let names = names.unwrap_or_default();
        let claims = leftover.unwrap_or_default();
        let resources: Vec<String> = manifest_resources(latest.manifest.expose())
            .iter()
            .map(ToString::to_string)
            .collect();
        let hooks = latest
            .hooks
            .iter()
            .filter(|hook| hook.events.iter().any(|event| event.ends_with("-delete")))
            .count();
        let message = uninstall_message(&name, &target.namespace, resources.len(), hooks);
        if target.context != connection.context {
            return;
        }
        let display = HelmCommand::uninstall(&target, &name, false).display();
        let screen = cx.entity().downgrade();
        let captured = connection.clone();
        let handle = ConfirmDialog::open(window, cx, move |window, cx| {
            let (screen, target, name) = (screen.clone(), target.clone(), name.clone());
            let connection = captured.clone();
            let typed = name.clone();
            let claims = claims.clone();
            let mut dialog = ConfirmDialog::new(t!("helm-uninstall-title", name = name.clone()))
                .context(connection.context.clone())
                .message(message.clone())
                .command(display.clone())
                .items(resources.clone());
            if let Some(notice) = &notice {
                dialog = dialog.notice(notice.clone(), names.clone());
            }
            dialog = dialog.option(t!("helm-uninstall-keep-history"), false);
            if !claims.is_empty() {
                let label = t!("helm-uninstall-delete-claims", count = claims.len());
                dialog = dialog.option(label, false);
            }
            dialog
                .confirm_label(t!("helm-uninstall-confirm"))
                .danger()
                .on_confirm_with_options(move |checked, _, cx| {
                    let keep_history = checked.first().copied().unwrap_or(false);
                    let delete_claims = checked.get(1).copied().unwrap_or(false);
                    let command = HelmCommand::uninstall(&target, &name, keep_history);
                    let after = if delete_claims {
                        AfterSuccess::DeleteClaims {
                            namespace: target.namespace.clone(),
                            claims: claims.clone(),
                            close: !keep_history,
                            connection: connection.clone(),
                        }
                    } else if keep_history {
                        AfterSuccess::Reload
                    } else {
                        AfterSuccess::CloseRelease
                    };
                    let title = t!("helm-uninstall-toast", name = name.clone());
                    let notice = HelmNotice::new(HelmChange::Uninstall, name.clone());
                    Self::guarded(&screen, &connection, cx, |this, cx| {
                        this.run_helm(title, command, Some(notice), after, cx)
                    });
                })
                .require_typed(typed, window, cx)
        });
        self.confirms.track(handle, connection);
    }

    /// Deletes the claims an uninstall left behind with the claim plugin's
    /// Delete (#274), through `connection` (where the uninstall was
    /// confirmed) and only while it is still the active one (#344); the
    /// toast tells how it went.
    fn delete_claims(
        &mut self,
        namespace: String,
        claims: Vec<claims::LeftoverClaim>,
        connection: Connection,
        cx: &mut Context<Self>,
    ) {
        let names: Vec<String> = claims.iter().map(|claim| claim.name.clone()).collect();
        let count = names.len();
        let title = if count == 1 {
            t!("helm-delete-claim-toast", name = names[0].clone())
        } else {
            t!("helm-delete-claims-toast", count = count)
        };
        let detail = t!(
            "helm-delete-claims-detail",
            namespace = namespace.clone(),
            names = names.join(", ")
        );
        if let Err(changed) = action_context::check(self.cluster.read(cx), &connection) {
            let toast = ActionToast::show_in_active_window(title, count, cx);
            toast.set_detail(detail, cx);
            toast.finish(
                BulkResult {
                    succeeded: 0,
                    failures: vec![(
                        SharedString::default(),
                        t!("helm-claims-not-deleted", reason = changed.to_string()).into(),
                    )],
                    hint: None,
                },
                cx,
            );
            return;
        }
        let client = connection.client;
        // Detached like `helm` calls: deletes sent go through (ADR 0011).
        let handle = runtime::spawn_detached(async move {
            claims::delete_claims(&client, &namespace, &claims).await
        });
        let task = cx.spawn(async move |this, cx| {
            let results = handle.await;
            this.update(cx, |this, cx| {
                let result = match results {
                    Ok(results) => {
                        let failures: Vec<(SharedString, SharedString)> = results
                            .iter()
                            .filter_map(|(name, result)| {
                                let error = result.as_ref().err()?;
                                Some((name.clone().into(), error.clone().into()))
                            })
                            .collect();
                        BulkResult {
                            succeeded: results.len() - failures.len(),
                            failures,
                            hint: None,
                        }
                    }
                    Err(error) => BulkResult {
                        succeeded: 0,
                        failures: vec![(
                            SharedString::default(),
                            t!("helm-delete-interrupted", error = error.to_string()).into(),
                        )],
                        hint: None,
                    },
                };
                if let Some(run) = &mut this.action {
                    let outcome = if result.failures.is_empty() {
                        Ok(String::new())
                    } else {
                        Err(t!(
                            "helm-claims-not-deleted-count",
                            count = result.failures.len()
                        ))
                    };
                    run.toast.finish(result, cx);
                    run.outcome = Some(outcome);
                }
                cx.notify();
            })
            .ok();
        });
        let toast = ActionToast::show_in_active_window(title, count, cx);
        toast.set_detail(detail, cx);
        self.action = Some(ActionRun {
            toast,
            outcome: None,
            _task: task,
        });
        cx.notify();
    }

    /// Opens the upgrade panel for the latest revision of the open release;
    /// `version` picks that version of the repository chart (from the
    /// newer-version notice).
    pub(super) fn open_upgrade(
        &mut self,
        version: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let client = self
            .cluster
            .read(cx)
            .connection()
            .map(|connection| connection.client.clone());
        let Some(detail) = &self.detail else {
            return;
        };
        let (Some(client), Some(latest), Some(target)) = (
            client,
            detail.latest().cloned(),
            self.helm_target(&detail.namespace, cx),
        ) else {
            return;
        };
        let candidates: Vec<(String, Vec<kubach_core::helm::repo::ChartVersion>)> =
            updates::locate_chart(&self.repos, &latest.chart)
                .candidates()
                .into_iter()
                .map(|found| (found.repository.to_owned(), found.versions.to_vec()))
                .collect();
        let panel =
            cx.new(|cx| HelmUpgrade::new(client, target, &latest, candidates, version, window, cx));
        let subscription = cx.subscribe_in(
            &panel,
            window,
            |this, panel, event: &UpgradeEvent, window, cx| match event {
                UpgradeEvent::Cancel => this.close_upgrade(cx),
                UpgradeEvent::Apply(request) => {
                    let base = panel.read(cx).base_revision();
                    this.confirm_upgrade((**request).clone(), base, window, cx)
                }
            },
        );
        if let Some(detail) = &mut self.detail {
            detail.upgrade = Some((panel, subscription));
        }
        cx.notify();
    }

    fn close_upgrade(&mut self, cx: &mut Context<Self>) {
        if let Some(detail) = &mut self.detail {
            detail.upgrade = None;
            cx.notify();
        }
    }

    /// Asks to confirm a previewed upgrade, then runs it.
    fn confirm_upgrade(
        &mut self,
        request: UpgradeRequest,
        base: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(detail) = &self.detail else {
            return;
        };
        let Some(target) = self.helm_target(&detail.namespace, cx) else {
            return;
        };
        let name = request.release.clone();
        // The release moved on since the panel opened: the preview no longer
        // describes what the upgrade would do.
        let latest = detail.latest().map(|r| r.revision);
        let message = if latest != Some(base) {
            t!(
                "helm-upgrade-confirm-message-moved",
                revision = base,
                name = name.clone(),
                namespace = target.namespace.clone()
            )
        } else {
            t!(
                "helm-upgrade-confirm-message",
                name = name.clone(),
                namespace = target.namespace.clone()
            )
        };
        let Some(connection) = self.target_connection(&target, cx) else {
            return;
        };
        let display = HelmCommand::upgrade(&target, &request, None, false).display();
        let screen = cx.entity().downgrade();
        let captured = connection.clone();
        let handle = ConfirmDialog::open(window, cx, move |_, _| {
            let (screen, target, request) = (screen.clone(), target.clone(), request.clone());
            let connection = captured.clone();
            ConfirmDialog::new(t!("helm-upgrade-title", name = name.clone()))
                .context(connection.context.clone())
                .message(message.clone())
                .command(display.clone())
                .confirm_label(t!("helm-upgrade-confirm"))
                .on_confirm(move |_, cx| {
                    let (target, request) = (target.clone(), request.clone());
                    Self::guarded(&screen, &connection, cx, |this, cx| {
                        this.run_upgrade(target, request, cx)
                    });
                })
        });
        self.confirms.track(handle, connection);
    }

    fn run_upgrade(&mut self, target: HelmTarget, request: UpgradeRequest, cx: &mut Context<Self>) {
        let helm = Self::helm_path(cx);
        let title = t!("helm-upgrade-toast", name = request.release.clone());
        let display = HelmCommand::upgrade(&target, &request, None, false).display();
        let stop = StopHelm::upgrade(&request.release);
        let notice = HelmNotice::new(HelmChange::Upgrade, request.release.clone());
        let call = move |watch: HelmWatch| async move {
            let outcome = cli::upgrade_watched(
                helm.as_deref(),
                &target,
                &request,
                false,
                cli::DEFAULT_TIMEOUT,
                Some(watch),
            )
            .await;
            match outcome {
                Ok(cli::UpgradeOutcome::Applied(output)) => Ok(output),
                Ok(cli::UpgradeOutcome::Preview(_)) => Ok(String::new()),
                Err(error) => Err(error.to_string()),
            }
        };
        self.start_action(
            ActionText::new(title, display),
            call,
            Some(stop),
            Some(notice),
            AfterSuccess::CloseUpgrade,
            cx,
        );
    }

    /// Asks to confirm an install (like an upgrade), then runs it. The
    /// preview is optional (ADR 0006); the dialog says when it was skipped.
    pub(super) fn confirm_install(
        &mut self,
        target: HelmTarget,
        request: InstallRequest,
        previewed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = request.release.clone();
        let chart = match request.version.as_deref() {
            Some(version) => format!("{} {version}", request.chart),
            None => request.chart.clone(),
        };
        let yes_no = |yes: bool| if yes { "yes" } else { "no" };
        let message = t!(
            "helm-install-confirm-message",
            chart = chart,
            release = name.clone(),
            namespace = target.namespace.clone(),
            create = yes_no(request.create_namespace),
            previewed = yes_no(previewed)
        );
        // The panel was opened in `target.context`; it is closed when the
        // context changes, so a mismatch here is a race lost to a switch.
        let Some(connection) = self.target_connection(&target, cx) else {
            action_context::refuse(
                &action_context::ContextChanged {
                    requested: target.context.clone(),
                    active: self.cluster.read(cx).active_name().map(str::to_owned),
                },
                cx,
            );
            return;
        };
        let display = HelmCommand::install(&target, &request, false).display();
        let screen = cx.entity().downgrade();
        let captured = connection.clone();
        let handle = ConfirmDialog::open(window, cx, move |_, _| {
            let (screen, target, request) = (screen.clone(), target.clone(), request.clone());
            let connection = captured.clone();
            ConfirmDialog::new(t!("helm-install-title", name = name.clone()))
                .context(connection.context.clone())
                .message(message.clone())
                .command(display.clone())
                .confirm_label(t!("helm-install-confirm"))
                .on_confirm(move |_, cx| {
                    let (target, request) = (target.clone(), request.clone());
                    Self::guarded(&screen, &connection, cx, |this, cx| {
                        this.run_install(target, request, cx)
                    });
                })
        });
        self.confirms.track(handle, connection);
    }

    fn run_install(&mut self, target: HelmTarget, request: InstallRequest, cx: &mut Context<Self>) {
        let helm = Self::helm_path(cx);
        let title = t!("helm-install-toast", name = request.release.clone());
        let display = HelmCommand::install(&target, &request, false).display();
        let after = AfterSuccess::Installed {
            namespace: target.namespace.clone(),
            name: request.release.clone(),
        };
        let stop = StopHelm::install(&request.release);
        let notice = HelmNotice::new(HelmChange::Install, request.release.clone());
        let call = move |watch: HelmWatch| async move {
            match cli::install_watched(
                helm.as_deref(),
                &target,
                &request,
                false,
                cli::DEFAULT_TIMEOUT,
                Some(watch),
            )
            .await
            {
                Ok(cli::UpgradeOutcome::Applied(output)) => Ok(output),
                Ok(cli::UpgradeOutcome::Preview(_)) => Ok(String::new()),
                Err(error) => Err(error.to_string()),
            }
        };
        self.start_action(
            ActionText::new(title, display),
            call,
            Some(stop),
            Some(notice),
            after,
            cx,
        );
    }

    /// Runs `helm` on the I/O runtime. Changes to a release (not to the
    /// repositories) run with `--debug` for their phases (#382).
    pub(super) fn run_helm(
        &mut self,
        title: String,
        command: HelmCommand,
        notice: Option<HelmNotice>,
        after: AfterSuccess,
        cx: &mut Context<Self>,
    ) {
        let helm = Self::helm_path(cx);
        let command = if matches!(after, AfterSuccess::ReloadCharts) {
            command
        } else {
            command.with_progress()
        };
        let display = command.display();
        let call = move |watch: HelmWatch| async move {
            cli::run_watched(helm.as_deref(), &command, cli::DEFAULT_TIMEOUT, watch)
                .await
                .map_err(|error| error.to_string())
        };
        self.start_action(
            ActionText::new(title, display),
            call,
            None,
            notice,
            after,
            cx,
        );
    }

    /// Runs one change at a time on the I/O runtime; reloads the list and
    /// the open release when it is done. helm's phases go to the toast as
    /// they come; with `stop` the toast offers to stop it (#382).
    fn start_action<F>(
        &mut self,
        text: ActionText,
        call: impl FnOnce(HelmWatch) -> F,
        stop: Option<StopHelm>,
        notice: Option<HelmNotice>,
        after: AfterSuccess,
        cx: &mut Context<Self>,
    ) where
        F: Future<Output = Result<String, String>> + Send + 'static,
    {
        if self.action_running() {
            return;
        }
        if let Some(notice) = &notice {
            Operations::announce(notice.started(), Tone::Info, cx);
        }
        let toast = ActionToast::show_in_active_window(text.title, 1, cx);
        toast.set_detail(text.command, cx);
        let (phases, mut phase_rx) = mpsc::unbounded::<String>();
        let cancel = stop.as_ref().map(|_| cli::Cancel::new());
        let watch = HelmWatch {
            on_phase: Some(Arc::new(move |phase| {
                phases.unbounded_send(phase).ok();
            })),
            cancel: cancel.clone(),
        };
        if let (Some(stop), Some(cancel)) = (stop, cancel) {
            toast.set_cancel_confirmed(stop.title, stop.message, move |_| cancel.cancel(), cx);
        }
        // Detached: a change already handed to `helm` runs to the end even
        // if the screen goes away; killing it halfway would leave the
        // release `pending-*` (ADR 0011). Only Stop ends it early.
        let handle = runtime::spawn_detached(call(watch));
        let phase_toast = toast.clone();
        let task = cx.spawn(async move |this, cx| {
            // The sender goes away with the call, ending the loop.
            while let Some(phase) = phase_rx.next().await {
                let mut lines = vec![SharedString::from(phase)];
                while let Ok(more) = phase_rx.try_recv() {
                    lines.push(more.into());
                }
                cx.update(|cx| phase_toast.push_output(lines, cx));
                // At most a few redraws a second; lines wait in the channel.
                cx.background_executor().timer(PHASE_BATCH).await;
            }
            let outcome = handle.await.unwrap_or_else(|error| {
                Err(t!("helm-call-interrupted", error = error.to_string()))
            });
            // Announced even when the screen has gone away meanwhile.
            if let Some(notice) = &notice {
                let (text, tone) = notice.finished(&outcome);
                cx.update(|cx| Operations::announce(text, tone, cx));
            }
            this.update(cx, |this, cx| {
                let succeeded = outcome.is_ok();
                if let Some(run) = &mut this.action {
                    match &outcome {
                        Ok(output) => {
                            run.toast.set_detail(output.trim().to_owned(), cx);
                            run.toast.finish(
                                BulkResult {
                                    succeeded: 1,
                                    ..Default::default()
                                },
                                cx,
                            );
                        }
                        Err(error) => run.toast.finish(
                            BulkResult {
                                succeeded: 0,
                                failures: vec![(SharedString::default(), error.clone().into())],
                                hint: None,
                            },
                            cx,
                        ),
                    }
                    run.outcome = Some(outcome.clone());
                }
                match after {
                    AfterSuccess::ReloadCharts => {
                        this.reload_charts(cx);
                        this.reload_repos(cx);
                        cx.notify();
                        return;
                    }
                    AfterSuccess::Installed { namespace, name } => {
                        let output = match outcome {
                            Ok(output) => output,
                            Err(error) => {
                                // The panel stays open with the failure.
                                if let Some((charts, _)) = &this.charts {
                                    charts
                                        .update(cx, |charts, cx| charts.install_failed(error, cx));
                                }
                                cx.notify();
                                return;
                            }
                        };
                        if let Some((charts, _)) = &this.charts {
                            charts.update(cx, |charts, cx| charts.close_install(cx));
                        }
                        this.installed = Some(InstalledNotice {
                            namespace: namespace.clone(),
                            name: name.clone(),
                            output,
                            open: false,
                        });
                        // Through the sidebar, so its selection follows.
                        cx.emit(ShowHelmPage(HelmPage::Releases));
                        this.open_release(namespace, name, cx);
                        cx.notify();
                        return;
                    }
                    AfterSuccess::CloseUpgrade if succeeded => this.close_upgrade(cx),
                    AfterSuccess::CloseRelease if succeeded => this.close_detail(cx),
                    AfterSuccess::DeleteClaims {
                        namespace,
                        claims,
                        close,
                        connection,
                    } if succeeded => {
                        if close {
                            this.close_detail(cx);
                        }
                        this.delete_claims(namespace, claims, connection, cx);
                        if close {
                            return;
                        }
                    }
                    _ => {}
                }
                // The storage watch brings the list and the open release up
                // to date; reloading the release also covers a failed call
                // that still wrote a revision.
                if let Some(detail) = &this.detail {
                    let (namespace, name) = (detail.namespace.clone(), detail.name.clone());
                    this.open_release(namespace, name, cx);
                }
                cx.notify();
            })
            .ok();
        });
        self.action = Some(ActionRun {
            toast,
            outcome: None,
            _task: task,
        });
        cx.notify();
    }

    /// The open release was just installed from the Charts page: says so,
    /// with helm's output under a toggle, until dismissed.
    pub(super) fn render_installed(
        &self,
        detail: &Detail,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let notice = self
            .installed
            .as_ref()
            .filter(|n| n.namespace == detail.namespace && n.name == detail.name)?;
        let toggle = cx.listener(|this, _, _, cx| {
            if let Some(notice) = &mut this.installed {
                notice.open = !notice.open;
            }
            cx.notify();
        });
        let output = (!notice.output.trim().is_empty()).then(|| {
            helm_output(
                "helm-installed-output",
                &notice.output,
                notice.open,
                Some(toggle),
                cx,
            )
        });
        let dismiss = Button::new("helm-installed-dismiss")
            .ghost()
            .xsmall()
            .icon(IconName::Close)
            .tooltip(t!("helm-dismiss"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.installed = None;
                cx.notify();
            }));
        let theme = cx.theme();
        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.group_box)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .text_color(theme.success)
                        .child(Icon::new(IconName::CircleCheck).small())
                        .child(div().flex_1().min_w_0().child(t!(
                            "helm-installed",
                            name = notice.name.clone(),
                            namespace = notice.namespace.clone()
                        )))
                        .child(dismiss),
                )
                .children(output)
                .into_any_element(),
        )
    }

    pub(in crate::screens) fn action_running(&self) -> bool {
        self.action
            .as_ref()
            .is_some_and(|run| run.outcome.is_none())
    }

    /// Rollback (for a picked older revision), upgrade and uninstall, or
    /// why they are not offered.
    pub(super) fn render_actions(
        &self,
        detail: &Detail,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let latest = detail.latest()?;
        if Self::helm_path(cx).is_none() {
            return Some(crate::tools::helm_missing_hint(cx));
        }
        if detail.upgrade.is_some() {
            return None;
        }
        let blocked =
            self.action_running() || self.uninstall_lookup.is_some() || latest.status.is_pending();
        let uninstalled = latest.status == ReleaseStatus::Uninstalled;
        let rollback = detail.revision.map(|revision| {
            Button::new("helm-rollback")
                .outline()
                .small()
                .icon(IconName::Undo2)
                .label(t!("helm-rollback-button", revision = revision))
                .disabled(blocked)
                .on_click(cx.listener(|this, _, window, cx| this.request_rollback(window, cx)))
        });
        let upgrade = Button::new("helm-upgrade")
            .outline()
            .small()
            .icon(IconName::ArrowUp)
            .label(t!("helm-upgrade-button"))
            .tooltip(t!("helm-upgrade-tooltip"))
            .disabled(blocked || uninstalled)
            .on_click(cx.listener(|this, _, window, cx| this.open_upgrade(None, window, cx)));
        let uninstall = Button::new("helm-uninstall")
            .outline()
            .small()
            .icon(IconName::Delete)
            .label(t!("helm-uninstall-button"))
            .disabled(blocked || uninstalled)
            .on_click(cx.listener(|this, _, window, cx| this.request_uninstall(window, cx)));
        Some(
            div()
                .flex()
                .items_center()
                .gap_2()
                .children(rollback)
                .child(upgrade)
                .child(uninstall)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{HelmChange, HelmNotice};
    use kubach_ui::Tone;

    #[test]
    fn notices_tell_start_and_outcome() {
        let notice = HelmNotice::new(HelmChange::Install, "web");
        assert_eq!(notice.started(), "Installing web started");
        assert_eq!(
            notice.finished(&Ok(String::new())),
            ("web installed".to_owned(), Tone::Success)
        );
        let error = Err("Error: INSTALLATION FAILED: timed out\nmore lines".to_owned());
        assert_eq!(
            notice.finished(&error),
            (
                "Install of web failed: Error: INSTALLATION FAILED: timed out".to_owned(),
                Tone::Danger
            )
        );
        let notice = HelmNotice::new(HelmChange::Uninstall, "web");
        assert_eq!(notice.started(), "Uninstalling web started");
    }
}
