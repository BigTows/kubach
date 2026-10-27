//! Upgrade panel of a Helm release (ADR 0006): the same chart with edited
//! values, a server-side `helm upgrade --dry-run` preview of the manifest
//! diff, then the real upgrade, which the Helm screen runs on
//! [`UpgradeEvent::Apply`].
//!
//! The chart is the one kept in the release unless it has subcharts (Helm
//! does not keep those) or the user picks another version. When the chart
//! is in the user's repositories (cached `index.yaml`, #273) the version is
//! picked from its list; a chart typed by reference (`oci://…`, a path or a
//! URL) is offered only when no repository has it; the versions of an
//! `oci://` chart are listed from the registry ([`OciVersions`], #301).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::helm::cli::{
    self, HelmCommand, HelmTarget, UpgradeChart, UpgradeOutcome, UpgradeRequest,
};
use kubach_core::helm::manifest::ChangeKind;
use kubach_core::helm::oci;
use kubach_core::helm::repo::{ChartVersion, chart_reference};
use kubach_core::helm::{
    ManifestDiff, Release, Releases, Sensitive, StoredChart, check_values_yaml, diff_manifests,
    values_to_yaml,
};
use kubach_core::runtime;
use kubach_i18n::t;
use std::sync::Arc;

use kubach_ui::{DiffRow, DiffView, EmptyState, StatusBadge, Tone, fold_rows};
use kube::Client;

use super::helm::HelmScreen;
use super::helm_release::validation_text;
use super::oci_versions::OciVersions;

pub enum UpgradeEvent {
    /// Run this upgrade (it was previewed with the same input).
    Apply(Box<UpgradeRequest>),
    Cancel,
}

/// Where the chart comes from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Stored,
    /// A version of the chart from a repository that has it.
    Repository,
    /// Typed by the user; only when no repository has the chart.
    Reference,
}

/// Label of a version in the version list: `1.2.3 · app 4.5 · current`.
pub fn version_label(version: &ChartVersion, installed: &str) -> String {
    let mut parts = vec![version.version.clone()];
    if let Some(app) = &version.app_version {
        parts.push(t!("helm-version-app", app = app.clone()));
    }
    if version.is_prerelease() {
        parts.push(t!("helm-version-prerelease"));
    }
    if version.version == installed {
        parts.push(t!("helm-version-current"));
    }
    parts.join(" · ")
}

/// Version an upgrade starts with: `wanted` when listed, else the installed
/// one when listed, else the newest stable (or newest) version.
pub fn initial_version(versions: &[ChartVersion], installed: &str, wanted: Option<&str>) -> String {
    let listed = |v: &str| versions.iter().any(|version| version.version == v);
    if let Some(wanted) = wanted.filter(|w| listed(w)) {
        return wanted.to_owned();
    }
    if listed(installed) {
        return installed.to_owned();
    }
    versions
        .iter()
        .find(|version| !version.is_prerelease())
        .or(versions.first())
        .map(|version| version.version.clone())
        .unwrap_or_default()
}

enum Stored {
    Loading,
    Ready(StoredChart),
    Unusable(String),
}

enum PreviewState {
    Running,
    Ready {
        release: Box<Release>,
        diff: ManifestDiff,
        /// Folded once: the panel re-renders on every keystroke and frame.
        rows: Arc<[DiffRow]>,
    },
    Failed(String),
}

struct Preview {
    /// The input it was made for; edits after it make it stale.
    request: UpgradeRequest,
    state: PreviewState,
    _task: Task<()>,
}

pub struct HelmUpgrade {
    target: HelmTarget,
    release: String,
    /// Revision the values and the diff start from.
    base_revision: u32,
    base_manifest: Sensitive<String>,
    /// `name-version` of the deployed chart.
    deployed_chart: String,
    chart_name: String,
    installed_version: String,
    /// Repositories that have the chart, with its versions (newest first).
    repo_charts: Vec<(String, Vec<ChartVersion>)>,
    repo_ix: usize,
    /// Version picked from the repository chart.
    picked: String,
    stored: Stored,
    source: Source,
    chart: Entity<InputState>,
    version: Entity<InputState>,
    /// Versions of a typed `oci://` chart.
    oci: Entity<OciVersions>,
    values: Entity<EditorState>,
    preview: Option<Preview>,
    _load: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<UpgradeEvent> for HelmUpgrade {}

impl HelmUpgrade {
    /// Starts from `release` (the latest revision) and its user values.
    /// `repo_charts` are the repositories that have its chart; with any, the
    /// version is picked from their list, starting with `version` if given.
    pub fn new(
        client: Client,
        target: HelmTarget,
        release: &Release,
        repo_charts: Vec<(String, Vec<ChartVersion>)>,
        version: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let picked = repo_charts
            .first()
            .map(|(_, versions)| {
                initial_version(versions, &release.chart.version, version.as_deref())
            })
            .unwrap_or_default();
        // The repository chart by default: the version list is what the
        // user picks from; asked for a version, it is the only way there.
        let source = if repo_charts.is_empty() {
            Source::Stored
        } else {
            Source::Repository
        };
        let values_text = match release.user_values.expose() {
            serde_json::Value::Object(map) if map.is_empty() => String::new(),
            values => values_to_yaml(values),
        };
        let values = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("yaml")
                .line_number(true)
                .default_value(values_text)
        });
        let chart =
            cx.new(|cx| InputState::new(window, cx).default_value(release.chart.name.clone()));
        let version =
            cx.new(|cx| InputState::new(window, cx).default_value(release.chart.version.clone()));
        // Pre-releases are listed (labelled), as for repository charts.
        let oci = cx.new(|_| OciVersions::new(true));
        // Re-render on edits: the preview may go stale.
        let mut subscriptions = vec![
            cx.subscribe(&chart, |this, chart, _: &InputEvent, cx| {
                let text = chart.read(cx).value().to_string();
                this.oci.update(cx, |oci, cx| oci.set_chart(&text, cx));
                cx.notify();
            }),
            // An emptied version field gets the newest stable version.
            cx.observe_in(&oci, window, |this, oci, window, cx| {
                let newest = oci::newest_stable(oci.read(cx).versions()).map(str::to_owned);
                if let Some(newest) = newest
                    && this.version.read(cx).value().trim().is_empty()
                {
                    this.version
                        .update(cx, |input, cx| input.set_value(newest, window, cx));
                }
                cx.notify();
            }),
            cx.subscribe(&version, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&values, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        for (input, text) in [
            (
                &chart,
                (|| t!("helm-upgrade-chart-placeholder")) as fn() -> String,
            ),
            (&version, || t!("helm-version-latest")),
        ] {
            subscriptions.extend(crate::i18n::localized_placeholder(input, text, window, cx));
        }

        let (namespace, name, revision) = (
            release.namespace.clone(),
            release.name.clone(),
            release.revision,
        );
        let handle = runtime::spawn(async move {
            Releases::with_driver(client, target.driver)
                .stored_chart(&namespace, &name, revision)
                .await
        });
        let load = cx.spawn(async move |this, cx| {
            let result = handle.await;
            this.update(cx, |this, cx| {
                this.stored = match result {
                    Ok(Ok(chart)) => match chart.usable() {
                        Ok(()) => Stored::Ready(chart),
                        Err(reason) => Stored::Unusable(validation_text(&reason)),
                    },
                    Ok(Err(error)) => Stored::Unusable(error.to_string()),
                    Err(error) => {
                        Stored::Unusable(t!("helm-loading-interrupted", error = error.to_string()))
                    }
                };
                if matches!(this.stored, Stored::Unusable(_)) && this.source == Source::Stored {
                    this.source = if this.repo_charts.is_empty() {
                        Source::Reference
                    } else {
                        Source::Repository
                    };
                }
                cx.notify();
            })
            .ok();
        });

        Self {
            target,
            release: release.name.clone(),
            base_revision: release.revision,
            base_manifest: release.manifest.clone(),
            deployed_chart: release.chart.reference(),
            chart_name: release.chart.name.clone(),
            installed_version: release.chart.version.clone(),
            repo_charts,
            repo_ix: 0,
            picked,
            stored: Stored::Loading,
            source,
            chart,
            version,
            oci,
            values,
            preview: None,
            _load: load,
            _subscriptions: subscriptions,
        }
    }

    pub fn base_revision(&self) -> u32 {
        self.base_revision
    }

    /// The upgrade as currently entered, or why it can't run.
    fn request(&self, cx: &App) -> Result<UpgradeRequest, String> {
        let chart = match self.source {
            Source::Stored => match &self.stored {
                Stored::Ready(chart) => UpgradeChart::Stored(chart.clone()),
                Stored::Loading => return Err(t!("helm-upgrade-stored-loading")),
                Stored::Unusable(reason) => {
                    return Err(t!("helm-upgrade-stored-error", reason = reason.clone()));
                }
            },
            Source::Repository => {
                let Some((repository, _)) = self.repo_charts.get(self.repo_ix) else {
                    return Err(t!("helm-upgrade-no-repository"));
                };
                if self.picked.is_empty() {
                    return Err(t!("helm-upgrade-pick-version"));
                }
                UpgradeChart::Reference {
                    chart: chart_reference(repository, &self.chart_name),
                    version: Some(self.picked.clone()),
                }
            }
            Source::Reference => {
                let chart = self.chart.read(cx).value().trim().to_owned();
                if chart.is_empty() {
                    return Err(t!("helm-upgrade-enter-chart"));
                }
                let version = self.version.read(cx).value().trim().to_owned();
                UpgradeChart::Reference {
                    chart,
                    version: (!version.is_empty()).then_some(version),
                }
            }
        };
        let values = self.values.read(cx).value().to_string();
        check_values_yaml(&values)
            .map_err(|error| t!("helm-values-error", error = validation_text(&error)))?;
        Ok(UpgradeRequest {
            release: self.release.clone(),
            chart,
            values: Sensitive::new(values),
        })
    }

    /// The previewed request when the input has not changed since.
    fn previewed(&self, cx: &App) -> Option<UpgradeRequest> {
        let preview = self.preview.as_ref()?;
        let current = self.request(cx).ok()?;
        (matches!(preview.state, PreviewState::Ready { .. }) && preview.request == current)
            .then_some(current)
    }

    fn run_preview(&mut self, cx: &mut Context<Self>) {
        let Ok(request) = self.request(cx) else {
            return;
        };
        let helm = HelmScreen::helm_path(cx);
        let target = self.target.clone();
        let task_request = request.clone();
        let base = self.base_manifest.clone();
        let handle = runtime::spawn(async move {
            let outcome = cli::upgrade(
                helm.as_deref(),
                &target,
                &task_request,
                true,
                cli::DEFAULT_TIMEOUT,
            )
            .await;
            match outcome {
                Ok(UpgradeOutcome::Preview(release)) => {
                    let diff = diff_manifests(base.expose(), release.manifest.expose());
                    let rows = fold_rows(&crate::editor::diff_view_lines(
                        base.expose(),
                        release.manifest.expose(),
                    ));
                    PreviewState::Ready {
                        release,
                        diff,
                        rows,
                    }
                }
                Ok(UpgradeOutcome::Applied(_)) => PreviewState::Failed(t!("helm-no-preview")),
                Err(error) => PreviewState::Failed(error.to_string()),
            }
        });
        let task = cx.spawn(async move |this, cx| {
            let state = handle.await.unwrap_or_else(|error| {
                PreviewState::Failed(t!("helm-preview-interrupted", error = error.to_string()))
            });
            this.update(cx, |this, cx| {
                if let Some(preview) = &mut this.preview {
                    preview.state = state;
                }
                cx.notify();
            })
            .ok();
        });
        self.preview = Some(Preview {
            request,
            state: PreviewState::Running,
            _task: task,
        });
        cx.notify();
    }

    fn command_line(&self, request: &UpgradeRequest) -> String {
        HelmCommand::upgrade(&self.target, request, None, false).display()
    }

    fn pick_repository(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some((_, versions)) = self.repo_charts.get(ix) {
            self.picked = initial_version(versions, &self.installed_version, Some(&self.picked));
            self.repo_ix = ix;
            cx.notify();
        }
    }

    fn render_source(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let stored_label = match &self.stored {
            Stored::Loading => t!("helm-upgrade-source-stored-loading"),
            Stored::Ready(chart) => t!("helm-upgrade-source-stored", chart = chart.reference()),
            Stored::Unusable(_) => t!("helm-upgrade-source-stored-unavailable"),
        };
        let repository_label = match self.repo_charts.as_slice() {
            [(repository, _)] => t!(
                "helm-upgrade-source-repository",
                repository = repository.clone()
            ),
            _ => t!("helm-upgrade-source-any-repository"),
        };
        let reference_label = t!("helm-upgrade-source-reference");
        let current = self.source;
        let label = match current {
            Source::Stored => stored_label.clone(),
            Source::Repository => repository_label.clone(),
            Source::Reference => reference_label.clone(),
        };
        let screen = cx.entity().downgrade();
        let stored_usable = !matches!(self.stored, Stored::Unusable(_));
        let in_repository = !self.repo_charts.is_empty();
        let picker = Button::new("helm-upgrade-source")
            .outline()
            .small()
            .label(label)
            .dropdown_menu(move |menu: PopupMenu, _, _| {
                let pick = |source: Source, title: String, enabled: bool| {
                    let screen = screen.clone();
                    PopupMenuItem::new(title)
                        .checked(source == current)
                        .disabled(!enabled)
                        .on_click(move |_, _, cx| {
                            screen
                                .update(cx, |this, cx| {
                                    this.source = source;
                                    cx.notify();
                                })
                                .ok();
                        })
                };
                let menu = menu.item(pick(Source::Stored, stored_label.clone(), stored_usable));
                // Typing a chart is for charts no repository has.
                if in_repository {
                    menu.item(pick(Source::Repository, repository_label.clone(), true))
                } else {
                    menu.item(pick(Source::Reference, reference_label.clone(), true))
                }
            });
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(t!("helm-field-chart"))
            .child(picker);
        match current {
            Source::Stored => {}
            Source::Repository => {
                if let Some((repository, versions)) = self.repo_charts.get(self.repo_ix) {
                    if self.repo_charts.len() > 1 {
                        let screen = cx.entity().downgrade();
                        let names: Vec<String> = self
                            .repo_charts
                            .iter()
                            .map(|(name, _)| name.clone())
                            .collect();
                        let picked_ix = self.repo_ix;
                        row = row.child(
                            Button::new("helm-upgrade-repository")
                                .outline()
                                .small()
                                .label(chart_reference(repository, &self.chart_name))
                                .dropdown_menu(move |menu: PopupMenu, _, _| {
                                    names.iter().enumerate().fold(menu, |menu, (ix, name)| {
                                        let screen = screen.clone();
                                        menu.item(
                                            PopupMenuItem::new(name.clone())
                                                .checked(ix == picked_ix)
                                                .on_click(move |_, _, cx| {
                                                    screen
                                                        .update(cx, |this, cx| {
                                                            this.pick_repository(ix, cx)
                                                        })
                                                        .ok();
                                                }),
                                        )
                                    })
                                }),
                        );
                    } else {
                        row = row.child(
                            div()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.foreground)
                                .child(chart_reference(repository, &self.chart_name)),
                        );
                    }
                    let screen = cx.entity().downgrade();
                    let items: Vec<(String, String)> = versions
                        .iter()
                        .map(|v| (v.version.clone(), version_label(v, &self.installed_version)))
                        .collect();
                    let picked = self.picked.clone();
                    let shown = versions
                        .iter()
                        .find(|v| v.version == picked)
                        .map(|v| version_label(v, &self.installed_version))
                        .unwrap_or_else(|| picked.clone());
                    row = row.child(t!("helm-label-version")).child(
                        Button::new("helm-upgrade-version")
                            .outline()
                            .small()
                            .label(shown)
                            .dropdown_menu(move |menu: PopupMenu, _, _| {
                                let menu = menu.scrollable(true).max_h(px(360.));
                                items.iter().fold(menu, |menu, (version, label)| {
                                    let (screen, version) = (screen.clone(), version.clone());
                                    menu.item(
                                        PopupMenuItem::new(label.clone())
                                            .checked(version == picked)
                                            .on_click(move |_, _, cx| {
                                                let version = version.clone();
                                                screen
                                                    .update(cx, |this, cx| {
                                                        this.picked = version;
                                                        cx.notify();
                                                    })
                                                    .ok();
                                            }),
                                    )
                                })
                            }),
                    );
                }
            }
            Source::Reference => {
                let screen = cx.entity().downgrade();
                let installed = self.installed_version.clone();
                let picker = self.oci.read(cx).picker(
                    "helm-upgrade-oci-versions",
                    self.version.read(cx).value().trim().to_owned(),
                    move |version| {
                        let label = super::oci_versions::version_label(version);
                        if version == installed {
                            format!("{label} · {}", t!("helm-version-current"))
                        } else {
                            label
                        }
                    },
                    move |version, window, cx| {
                        screen
                            .update(cx, |this, cx| {
                                this.version
                                    .update(cx, |input, cx| input.set_value(version, window, cx));
                                cx.notify();
                            })
                            .ok();
                    },
                );
                row = row
                    .child(div().w(px(280.)).child(Input::new(&self.chart).small()))
                    .child(t!("helm-label-version"))
                    .child(div().w(px(120.)).child(Input::new(&self.version).small()))
                    .children(picker);
            }
        }
        let note = match (&self.stored, current) {
            (Stored::Unusable(reason), _) => {
                Some(t!("helm-upgrade-stored-unusable", reason = reason.clone()))
            }
            (_, Source::Repository) => Some(t!(
                "helm-upgrade-note-repository",
                chart = self.deployed_chart.clone()
            )),
            (_, Source::Reference) => Some(t!(
                "helm-upgrade-note-reference",
                chart = self.deployed_chart.clone()
            )),
            _ => None,
        };
        let oci_note = (current == Source::Reference)
            .then(|| self.oci.read(cx).note(cx))
            .flatten();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(row)
            .children(note.map(|note| {
                div()
                    .text_xs()
                    .whitespace_normal()
                    .text_color(theme.muted_foreground)
                    .child(note)
            }))
            .children(oci_note)
            .into_any_element()
    }

    fn render_preview(&self, stale: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(preview) = &self.preview else {
            return EmptyState::new(IconName::Eye, t!("helm-upgrade-preview-title"))
                .description(t!("helm-upgrade-preview-description"))
                .into_any_element();
        };
        let (release, diff, rows) = match &preview.state {
            PreviewState::Running => {
                return EmptyState::new(IconName::LoaderCircle, t!("helm-upgrade-rendering"))
                    .into_any_element();
            }
            PreviewState::Failed(error) => {
                return EmptyState::new(IconName::CircleX, t!("helm-preview-failed"))
                    .description(error.clone())
                    .into_any_element();
            }
            PreviewState::Ready {
                release,
                diff,
                rows,
            } => (release, diff, rows),
        };
        let stale_note = stale.then(|| {
            div()
                .text_xs()
                .text_color(theme.warning)
                .child(t!("helm-upgrade-preview-stale"))
        });
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(t!(
                "helm-upgrade-preview-summary",
                from = self.base_revision,
                to = release.revision,
                chart = release.chart.reference()
            ));
        let changes = div()
            .flex()
            .flex_wrap()
            .gap_1()
            .children(diff.changes.iter().map(|change| {
                let (sign, tone) = match change.kind {
                    ChangeKind::Added => ("+", Tone::Success),
                    ChangeKind::Changed => ("~", Tone::Warning),
                    ChangeKind::Removed => ("−", Tone::Danger),
                };
                StatusBadge::new(format!("{sign} {}", change.resource), tone)
            }));
        let body = if diff.text.expose().is_empty() {
            EmptyState::new(IconName::Check, t!("helm-upgrade-no-change"))
                .description(t!("helm-upgrade-no-change-description"))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(DiffView::from_rows("helm-upgrade-diff", rows.clone()))
                .into_any_element()
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .children(stale_note)
            .child(header)
            .when(!diff.changes.is_empty(), |el| el.child(changes))
            .child(body)
            .into_any_element()
    }
}

impl Render for HelmUpgrade {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let request = self.request(cx);
        let previewed = self.previewed(cx);
        let running = self
            .preview
            .as_ref()
            .is_some_and(|p| matches!(p.state, PreviewState::Running));
        let stale = self.preview.is_some() && !running && previewed.is_none();
        let source = self.render_source(cx);
        let preview = self.render_preview(stale, cx);
        let theme = cx.theme();

        let values = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .whitespace_normal()
                    .child(t!("helm-upgrade-values-note")),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .child(Editor::new(&self.values).h_full().bordered(false)),
            );
        let (status, status_color) = match &request {
            Ok(request) => (self.command_line(request), theme.muted_foreground),
            Err(error) => (error.clone(), theme.warning),
        };
        let footer = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(status_color)
                    .child(status),
            )
            .child(
                Button::new("helm-upgrade-cancel")
                    .ghost()
                    .small()
                    .label(t!("helm-cancel"))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(UpgradeEvent::Cancel))),
            )
            .child(
                Button::new("helm-upgrade-preview")
                    .outline()
                    .small()
                    .icon(IconName::Eye)
                    .label(t!("helm-upgrade-preview-button"))
                    .disabled(request.is_err() || running)
                    .on_click(cx.listener(|this, _, _, cx| this.run_preview(cx))),
            )
            .child(
                Button::new("helm-upgrade-apply")
                    .primary()
                    .small()
                    .icon(IconName::ArrowUp)
                    .label(t!("helm-upgrade-button"))
                    .disabled(previewed.is_none())
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(request) = this.previewed(cx) {
                            cx.emit(UpgradeEvent::Apply(Box::new(request)));
                        }
                    })),
            );
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(source)
            .child(
                div()
                    .flex_1()
                    .min_h(px(240.))
                    .flex()
                    .gap_3()
                    .child(values)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(preview),
                    ),
            )
            .child(footer)
    }
}

#[cfg(test)]
mod tests {
    use kubach_core::helm::repo::ChartVersion;

    use super::{initial_version, version_label};

    fn version(v: &str, app: Option<&str>) -> ChartVersion {
        ChartVersion {
            version: v.into(),
            app_version: app.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn versions_are_labelled_and_picked() {
        let versions = [
            version("2.0.0-rc.1", None),
            version("1.2.0", Some("4.5")),
            version("1.1.0", None),
        ];
        assert_eq!(version_label(&versions[1], "1.1.0"), "1.2.0 · app 4.5");
        assert_eq!(version_label(&versions[2], "1.1.0"), "1.1.0 · current");
        assert_eq!(
            version_label(&versions[0], "1.1.0"),
            "2.0.0-rc.1 · pre-release"
        );
        assert_eq!(initial_version(&versions, "1.1.0", Some("1.2.0")), "1.2.0");
        assert_eq!(initial_version(&versions, "1.1.0", Some("9.9.9")), "1.1.0");
        assert_eq!(initial_version(&versions, "1.1.0", None), "1.1.0");
        // The installed version is no longer listed: the newest stable one.
        assert_eq!(initial_version(&versions, "0.9.0", None), "1.2.0");
        assert_eq!(initial_version(&[], "1.0.0", None), "");
    }
}
