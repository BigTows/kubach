//! Downloading external tools on the user's request (#131, ADR 0006).
//!
//! One app-wide [`ToolDownloads`] runs and tracks downloads, so the Helm
//! screen and Settings → Tools show the same progress. It also knows the
//! latest Helm releases (#403): read from disk at start, asked for at most
//! once a day while Settings → Tools is on screen, on a download and on
//! request.
//! The fetching, checking and unpacking is in [`kubach_core::tools`].

use std::path::{Path, PathBuf};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::runtime;
use kubach_core::tools::helm::{self as helm_tool, HelmLine, LatestVersions};
use kubach_core::tools::{self, Installed, Platform, Progress};
use kubach_i18n::t;
use kubach_ui::DownloadProgress;

use crate::settings::{AppSettings, SettingsSection, SettingsTarget, open_settings};

/// Progress is reported to the UI at most once per this many bytes.
const PROGRESS_STEP: u64 = 256 * 1024;
/// After a failed check, the automatic one waits this long (seconds).
const RECHECK_AFTER_FAILURE: u64 = 60 * 60;

/// State of a tool download.
enum Job {
    Idle,
    Running {
        /// The version being downloaded, for the progress label.
        version: String,
        progress: Progress,
        abort: tokio::task::AbortHandle,
        _tasks: [Task<()>; 2],
    },
    Failed(SharedString),
}

/// State of the check for the latest Helm releases.
enum Check {
    Idle,
    Running { _task: Task<()> },
    Failed { error: SharedString, at: u64 },
}

/// Downloads of external tools and what is known about the binaries.
pub struct ToolDownloads {
    helm: Job,
    /// Why the last download installed the pinned version instead of the
    /// latest one.
    helm_notice: Option<SharedString>,
    /// `helm version --short` per binary path; `None` while asking or when
    /// it did not answer.
    helm_version: Option<(PathBuf, Option<SharedString>)>,
    _version_task: Option<Task<()>>,
    /// The latest Helm releases, as last checked.
    latest: Option<LatestVersions>,
    check: Check,
}

struct GlobalTools(Entity<ToolDownloads>);

impl Global for GlobalTools {}

pub fn init(cx: &mut App) {
    let latest = tools::tools_dir().and_then(|root| LatestVersions::load(&root));
    let entity = cx.new(|_| ToolDownloads {
        helm: Job::Idle,
        helm_notice: None,
        helm_version: None,
        _version_task: None,
        latest,
        check: Check::Idle,
    });
    cx.set_global(GlobalTools(entity));
}

/// The download state, if [`init`] ran.
pub fn global(cx: &App) -> Option<Entity<ToolDownloads>> {
    cx.try_global::<GlobalTools>().map(|g| g.0.clone())
}

/// The Helm release line chosen in Settings → Tools.
pub fn helm_line(cx: &App) -> HelmLine {
    AppSettings::try_global(cx)
        .map(|settings| settings.read(cx).get().tools.helm_line)
        .unwrap_or_default()
}

/// What can be done about the downloaded Helm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelmDownload {
    /// No release for this OS and CPU.
    Unsupported,
    /// No data directory to keep it in.
    NoDataDir,
    /// Nothing downloaded yet.
    Absent,
    /// The available version of the chosen line is downloaded.
    Current(Installed),
    /// An older version, or one of the other line, is downloaded.
    Outdated(Installed),
}

/// The state of the downloaded Helm on disk, against `available` of
/// `line`.
pub fn helm_download(line: HelmLine, available: &str) -> HelmDownload {
    if Platform::current()
        .and_then(|p| line.pinned().artifact(p))
        .is_none()
    {
        return HelmDownload::Unsupported;
    }
    if tools::tools_dir().is_none() {
        return HelmDownload::NoDataDir;
    }
    match helm_tool::downloaded() {
        None => HelmDownload::Absent,
        Some(installed) => classify(installed, line, available),
    }
}

/// Current when the copy is of `line` and not older than `available`.
fn classify(installed: Installed, line: HelmLine, available: &str) -> HelmDownload {
    let key = tools::download::version_key;
    if HelmLine::of_version(&installed.version) == Some(line)
        && key(&installed.version) >= key(available)
    {
        HelmDownload::Current(installed)
    } else {
        HelmDownload::Outdated(installed)
    }
}

impl ToolDownloads {
    pub fn helm_running(&self) -> Option<Progress> {
        match &self.helm {
            Job::Running { progress, .. } => Some(*progress),
            _ => None,
        }
    }

    pub fn helm_error(&self) -> Option<SharedString> {
        match &self.helm {
            Job::Failed(error) => Some(error.clone()),
            _ => None,
        }
    }

    /// Why the last download fell back to the pinned version, if it did.
    pub fn helm_notice(&self) -> Option<SharedString> {
        self.helm_notice.clone()
    }

    /// The latest releases as last checked, if ever.
    pub fn latest(&self) -> Option<&LatestVersions> {
        self.latest.as_ref()
    }

    /// The version of `line` a download would get now.
    pub fn helm_available(&self, line: HelmLine) -> String {
        helm_tool::available(line, self.latest.as_ref())
    }

    pub fn checking(&self) -> bool {
        matches!(self.check, Check::Running { .. })
    }

    /// Why the last check for updates failed, if it did.
    pub fn check_error(&self) -> Option<SharedString> {
        match &self.check {
            Check::Failed { error, .. } => Some(error.clone()),
            _ => None,
        }
    }

    /// Checks for new Helm releases unless the last check is less than a
    /// day old (or failed within the hour). Called while Settings → Tools
    /// is shown, so Kubach asks get.helm.sh only then (and on a download).
    pub fn ensure_latest(&mut self, cx: &mut Context<Self>) {
        let now = helm_tool::unix_now();
        let recent_failure = matches!(
            self.check,
            Check::Failed { at, .. } if now.saturating_sub(at) < RECHECK_AFTER_FAILURE
        );
        let fresh = self.latest.as_ref().is_some_and(|l| l.is_fresh(now));
        if !self.checking() && !recent_failure && !fresh && tools::tools_dir().is_some() {
            self.check_latest(cx);
        }
    }

    /// Asks get.helm.sh for the latest Helm releases now ("Check for
    /// updates") and saves the answer in the data folder.
    pub fn check_latest(&mut self, cx: &mut Context<Self>) {
        if self.checking() {
            return;
        }
        let root = tools::tools_dir();
        let handle = runtime::spawn(async move {
            let latest = helm_tool::check_latest().await?;
            if let Some(root) = root {
                latest.save(&root).ok();
            }
            Ok::<_, tools::DownloadError>(latest)
        });
        let task = cx.spawn(async move |this, cx| {
            let outcome = handle.await;
            this.update(cx, |this, cx| {
                this.check = match outcome {
                    Ok(Ok(latest)) => {
                        this.latest = Some(latest);
                        Check::Idle
                    }
                    Ok(Err(error)) => Check::Failed {
                        error: error.to_string().into(),
                        at: helm_tool::unix_now(),
                    },
                    Err(error) => Check::Failed {
                        error: t!("helm-tools-check-stopped", error = error.to_string()).into(),
                        at: helm_tool::unix_now(),
                    },
                };
                cx.notify();
            })
            .ok();
        });
        self.check = Check::Running { _task: task };
        cx.notify();
    }

    /// Downloads the latest Helm of `line` into the data directory (the
    /// pinned one when that fails); on success the other downloaded
    /// versions are removed.
    pub fn start_helm(&mut self, line: HelmLine, cx: &mut Context<Self>) {
        if self.helm_running().is_some() {
            return;
        }
        let Some(root) = tools::tools_dir() else {
            self.helm = Job::Failed(t!("helm-tools-no-data-dir").into());
            cx.notify();
            return;
        };
        let (tx, mut rx) = mpsc::unbounded::<Progress>();
        let known = self.latest.clone();
        let handle = runtime::spawn(async move {
            let mut reported = 0u64;
            let report = move |progress: Progress| {
                let done = progress.total == Some(progress.downloaded);
                if progress.downloaded == 0
                    || done
                    || progress.downloaded.saturating_sub(reported) >= PROGRESS_STEP
                {
                    reported = progress.downloaded;
                    tx.unbounded_send(progress).ok();
                }
            };
            let result = helm_tool::download_line(line, &root, known, report).await?;
            // Older versions go only once the new one is in place.
            tools::remove_other_versions(&helm_tool::HELM4, &root, &result.installed.version).ok();
            Ok::<_, tools::DownloadError>(result)
        });
        let abort = handle.abort_handle();
        let progress_task = cx.spawn(async move |this, cx| {
            while let Some(progress) = rx.next().await {
                let alive = this.update(cx, |this, cx| {
                    if let Job::Running { progress: p, .. } = &mut this.helm {
                        *p = progress;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let result_task = cx.spawn(async move |this, cx| {
            let outcome = handle.await;
            this.update(cx, |this, cx| {
                this.helm = match outcome {
                    Ok(Ok(result)) => {
                        this.helm_notice = result.fallback.map(Into::into);
                        if let Some(latest) = result.latest {
                            this.latest = Some(latest);
                            this.check = Check::Idle;
                        }
                        Job::Idle
                    }
                    Ok(Err(error)) => Job::Failed(error.to_string().into()),
                    // Cancelled: the state was already reset.
                    Err(error) if error.is_cancelled() => return,
                    Err(error) => Job::Failed(
                        t!("helm-tools-download-stopped", error = error.to_string()).into(),
                    ),
                };
                this.helm_version = None;
                cx.notify();
            })
            .ok();
        });
        self.helm = Job::Running {
            version: self.helm_available(line),
            progress: Progress::default(),
            abort,
            _tasks: [progress_task, result_task],
        };
        self.helm_notice = None;
        cx.notify();
    }

    /// Stops a running download; its temporary files are removed.
    pub fn cancel_helm(&mut self, cx: &mut Context<Self>) {
        if let Job::Running { abort, .. } = &self.helm {
            abort.abort();
            self.helm = Job::Idle;
            cx.notify();
        }
    }

    /// Deletes every downloaded Helm.
    pub fn remove_helm(&mut self, cx: &mut Context<Self>) {
        if self.helm_running().is_some() {
            return;
        }
        let Some(root) = tools::tools_dir() else {
            return;
        };
        self.helm = match tools::remove_all(&helm_tool::HELM4, &root) {
            Ok(()) => Job::Idle,
            Err(error) => {
                Job::Failed(t!("helm-tools-remove-failed", error = error.to_string()).into())
            }
        };
        self.helm_notice = None;
        self.helm_version = None;
        cx.notify();
    }

    /// `helm version --short` of `path`, asked once per path in the
    /// background; `None` until it answers.
    pub fn helm_version(&mut self, path: &Path, cx: &mut Context<Self>) -> Option<SharedString> {
        if let Some((known, version)) = &self.helm_version
            && known == path
        {
            return version.clone();
        }
        self.helm_version = Some((path.to_owned(), None));
        let task_path = path.to_owned();
        let handle = runtime::spawn(async move { helm_tool::binary_version(&task_path).await });
        let path = path.to_owned();
        self._version_task = Some(cx.spawn(async move |this, cx| {
            let version = handle.await.ok().flatten();
            this.update(cx, |this, cx| {
                if this.helm_version.as_ref().is_some_and(|(p, _)| *p == path) {
                    this.helm_version = Some((path, version.map(Into::into)));
                    cx.notify();
                }
            })
            .ok();
        }));
        None
    }
}

/// Progress bar of a running Helm download, with cancel.
pub fn helm_progress(
    id: impl Into<ElementId>,
    downloads: &Entity<ToolDownloads>,
    progress: Progress,
    cx: &App,
) -> DownloadProgress {
    let version = match &downloads.read(cx).helm {
        Job::Running { version, .. } => version.clone(),
        _ => String::new(),
    };
    let downloads = downloads.clone();
    DownloadProgress::new(id, t!("helm-tools-downloading-helm", version = version))
        .bytes(progress.downloaded, progress.total)
        .on_cancel(move |_, _, cx| downloads.update(cx, |d, cx| d.cancel_helm(cx)))
}

/// Shown instead of Helm actions when no `helm` is found: offers to
/// download it (with progress) or to set its path in Settings → Tools.
pub fn helm_missing_hint(cx: &App) -> AnyElement {
    let downloads = global(cx);
    let line = helm_line(cx);
    let theme = cx.theme();
    let state = downloads.as_ref().map(|d| d.read(cx));
    let running = state.and_then(ToolDownloads::helm_running);
    let error = state.and_then(ToolDownloads::helm_error);
    let available = state.map_or_else(
        || line.pinned().version.to_owned(),
        |d| d.helm_available(line),
    );
    // The download asks for the latest version itself; the number is
    // shown only when the last check is fresh.
    let fresh = state
        .and_then(ToolDownloads::latest)
        .is_some_and(|latest| latest.is_fresh(helm_tool::unix_now()));
    let label = if fresh {
        t!("helm-tools-download-helm", version = available.clone())
    } else {
        t!("helm-tools-download-line", line = line.title())
    };
    let can_download = downloads.is_some()
        && matches!(
            helm_download(line, &available),
            HelmDownload::Absent | HelmDownload::Outdated(_)
        );
    let settings = Button::new("helm-missing-settings")
        .ghost()
        .xsmall()
        .label(t!("helm-tools-settings"))
        .on_click(|_, _, cx| open_settings(SettingsTarget::section(SettingsSection::Tools), cx));
    let mut row = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .text_xs()
        .text_color(theme.muted_foreground);
    match (running, &downloads) {
        (Some(progress), Some(downloads)) => {
            row = row.child(helm_progress(
                "helm-missing-progress",
                downloads,
                progress,
                cx,
            ));
        }
        _ => {
            row = row
                .child(t!("helm-tools-helm-needed"))
                .when_some(error, |row, error| {
                    row.child(
                        div()
                            .max_w(px(420.))
                            .truncate()
                            .text_color(theme.danger)
                            .child(error),
                    )
                })
                .when(can_download, |row| {
                    let downloads = downloads.clone();
                    row.child(
                        Button::new("helm-missing-download")
                            .outline()
                            .xsmall()
                            .label(label)
                            .tooltip(t!("helm-tools-download-tooltip"))
                            .on_click(move |_, _, cx| {
                                if let Some(downloads) = &downloads {
                                    let line = helm_line(cx);
                                    downloads.update(cx, |d, cx| d.start_helm(line, cx));
                                }
                            }),
                    )
                });
        }
    }
    row.child(settings).into_any_element()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kubach_core::tools::Installed;
    use kubach_core::tools::helm::HelmLine;

    use super::{HelmDownload, classify};

    fn copy(version: &str) -> Installed {
        Installed {
            version: version.to_owned(),
            path: PathBuf::from(format!("/data/tools/helm/{version}/helm")),
        }
    }

    #[test]
    fn downloaded_copy_against_the_chosen_line() {
        assert_eq!(
            classify(copy("4.3.1"), HelmLine::Helm4, "4.3.1"),
            HelmDownload::Current(copy("4.3.1"))
        );
        assert_eq!(
            classify(copy("4.3.1"), HelmLine::Helm4, "4.4.0"),
            HelmDownload::Outdated(copy("4.3.1"))
        );
        // Newer than the last check (downloaded after it): still current.
        assert_eq!(
            classify(copy("4.10.0"), HelmLine::Helm4, "4.9.0"),
            HelmDownload::Current(copy("4.10.0"))
        );
        // Switching the line offers the other one.
        assert_eq!(
            classify(copy("4.3.1"), HelmLine::Helm3, "3.22.1"),
            HelmDownload::Outdated(copy("4.3.1"))
        );
        assert_eq!(
            classify(copy("3.22.1"), HelmLine::Helm4, "4.3.1"),
            HelmDownload::Outdated(copy("3.22.1"))
        );
    }
}
