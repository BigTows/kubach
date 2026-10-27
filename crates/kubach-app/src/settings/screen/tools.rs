//! The "External tools" page: the helm binary, the copy Kubach downloads
//! (#131), its release line, latest version and signature (#403) and chart
//! icons.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::*;
use kubach_core::settings::{HelmBinary, HelmSource, Settings, Tools};
use kubach_core::tools::Verification;
use kubach_core::tools::helm::{HelmLine, unix_now};
use kubach_i18n::t;
use kubach_ui::{ConfirmDialog, PathRow, SelectRow, SettingRow, ToggleRow, format_age};

use super::{Entry, Group, SettingsScreen, searchable, setter};
use crate::tools::{self, HelmDownload, ToolDownloads};

impl SettingsScreen {
    pub(super) fn tools(&self, settings: &Settings, cx: &mut Context<Self>) -> Vec<Group> {
        // What an empty path picks: PATH, else the downloaded copy.
        let auto = Tools::default().locate_helm();
        let placeholder = match &auto {
            Some(HelmBinary {
                path,
                source: HelmSource::Downloaded,
            }) => t!(
                "helm-tools-auto-downloaded",
                path = path.display().to_string()
            ),
            Some(helm) => t!("helm-tools-auto", path = helm.path.display().to_string()),
            None => t!("helm-tools-auto-not-found"),
        };
        let (path_title, path_description) = (
            t!("helm-tools-helm-binary"),
            t!("helm-tools-helm-binary-description"),
        );
        let pick = setter(&self.settings, |s, path: PathBuf| {
            s.tools.helm_path = Some(path)
        });
        let clear = setter(&self.settings, |s, ()| s.tools.helm_path = None);
        let path_row = PathRow::new("helm-path", path_title.clone())
            .description(path_description.clone())
            .path(
                settings
                    .tools
                    .helm_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
            )
            .placeholder(placeholder)
            .on_pick(pick)
            .on_clear(move |window, cx| clear((), window, cx));
        let mut group = Group::new("Helm").entry(Entry::new(
            searchable(
                "helm binary path executable",
                &[&path_title, &path_description],
            ),
            path_row,
        ));
        if let Some(downloads) = tools::global(cx) {
            // At most once a day, and only while this page is shown.
            downloads.update(cx, |d, cx| d.ensure_latest(cx));
            let line = settings.tools.helm_line;
            group = group
                .entry(Entry::new(
                    searchable(
                        "helm in use version source path downloaded",
                        &[&t!("helm-tools-helm-in-use")],
                    ),
                    helm_in_use_row(settings.tools.locate_helm(), &downloads, cx),
                ))
                .entry(Entry::new(
                    searchable(
                        "helm line version 4 3 release download",
                        &[
                            &t!("helm-tools-helm-line"),
                            &t!("helm-tools-helm-line-description"),
                        ],
                    ),
                    self.helm_line_row(line),
                ))
                .entry(Entry::new(
                    searchable(
                        "download helm update remove get.helm.sh downloaded available latest \
                         check for updates signature signed gpg",
                        &[
                            &t!("helm-tools-downloaded"),
                            &t!("helm-tools-check-updates"),
                        ],
                    ),
                    helm_download_row(line, &downloads, cx),
                ));
        }
        let (icons_title, icons_description) = (
            t!("helm-tools-chart-icons"),
            t!("helm-tools-chart-icons-description"),
        );
        group = group.entry(Entry::new(
            searchable(
                "load chart icons images readme network internet download",
                &[&icons_title, &icons_description],
            ),
            ToggleRow::new("helm-chart-icons", icons_title.clone())
                .description(icons_description.clone())
                .checked(settings.helm.load_chart_icons)
                .default_value(true)
                .on_change(setter(&self.settings, |s, on: bool| {
                    s.helm.load_chart_icons = on
                })),
        ));
        vec![group]
    }

    /// Which Helm release line Kubach downloads.
    fn helm_line_row(&self, line: HelmLine) -> SelectRow {
        let selected = HelmLine::ALL.iter().position(|l| *l == line).unwrap_or(0);
        let default = HelmLine::ALL
            .iter()
            .position(|l| *l == HelmLine::default())
            .unwrap_or(0);
        SelectRow::new(
            "helm-line",
            t!("helm-tools-helm-line"),
            HelmLine::ALL.map(HelmLine::title),
        )
        .description(t!("helm-tools-helm-line-description"))
        .selected(selected)
        .default_index(default)
        .on_change(setter(&self.settings, |s, ix: usize| {
            s.tools.helm_line = HelmLine::ALL.get(ix).copied().unwrap_or_default();
        }))
    }
}

/// Which `helm` Helm actions run, where it comes from and its version.
fn helm_in_use_row(
    helm: Option<HelmBinary>,
    downloads: &Entity<ToolDownloads>,
    cx: &mut App,
) -> SettingRow {
    let row = SettingRow::new("helm-in-use", t!("helm-tools-helm-in-use"));
    let Some(helm) = helm else {
        return row.description(t!("helm-tools-helm-in-use-missing"));
    };
    let source = match helm.source {
        HelmSource::Settings => "settings",
        HelmSource::Path => "path",
        HelmSource::Downloaded => "downloaded",
    };
    let version = downloads.update(cx, |d, cx| d.helm_version(&helm.path, cx));
    let version = version.map_or_else(|| t!("helm-tools-version-unknown"), |v| v.to_string());
    row.description(t!(
        "helm-tools-helm-in-use-description",
        path = helm.path.display().to_string(),
        source = source,
        version = version
    ))
}

/// How the downloaded copy was checked, as a sentence.
fn verification_text(verification: Option<&Verification>) -> String {
    match verification {
        Some(Verification {
            signer: Some(signer),
            ..
        }) => t!("helm-tools-signed-by", signer = signer.to_string()),
        Some(Verification {
            pinned: true,
            signature_note,
            ..
        }) => match signature_note {
            Some(note) => t!("helm-tools-checked-pinned-unsigned", note = note.clone()),
            None => t!("helm-tools-checked-pinned"),
        },
        // Unsigned versions are never installed; this is a copy from an
        // older Kubach, checked by the pinned SHA-256 then.
        _ => t!("helm-tools-checked-sha"),
    }
}

/// When the latest versions were checked, or why that failed.
fn check_text(state: &ToolDownloads, line: HelmLine) -> String {
    if state.checking() {
        return t!("helm-tools-checking");
    }
    if let Some(error) = state.check_error() {
        return t!("helm-tools-check-failed", error = error.to_string());
    }
    match state.latest() {
        Some(latest) if latest.get(line).is_some() => t!(
            "helm-tools-checked-ago",
            age = format_age(unix_now().saturating_sub(latest.checked_at))
        ),
        Some(_) => t!("helm-tools-latest-unknown", line = line.title()),
        None => t!("helm-tools-not-checked"),
    }
}

/// Download, update or remove the copy of Helm Kubach keeps (#131, #403).
fn helm_download_row(line: HelmLine, downloads: &Entity<ToolDownloads>, cx: &App) -> SettingRow {
    let state = downloads.read(cx);
    let running = state.helm_running();
    let error = state.helm_error();
    let notice = state.helm_notice();
    let available = state.helm_available(line);
    let checked = check_text(state, line);
    let row = SettingRow::new("helm-download", t!("helm-tools-downloaded"));
    let download_button = |label: String| {
        let downloads = downloads.clone();
        Button::new("helm-download-start")
            .outline()
            .small()
            .label(label)
            .disabled(running.is_some())
            .on_click(move |_, _, cx| downloads.update(cx, |d, cx| d.start_helm(line, cx)))
    };
    let check_button = {
        let downloads = downloads.clone();
        Button::new("helm-check-updates")
            .ghost()
            .small()
            .label(t!("helm-tools-check-updates"))
            .disabled(state.checking() || running.is_some())
            .on_click(move |_, _, cx| downloads.update(cx, |d, cx| d.check_latest(cx)))
    };
    let remove_button = {
        let downloads = downloads.clone();
        Button::new("helm-download-remove")
            .ghost()
            .small()
            .label(t!("helm-tools-remove"))
            .disabled(running.is_some())
            .on_click(move |_, window, cx| {
                let downloads = downloads.clone();
                ConfirmDialog::open(window, cx, move |_, _| {
                    let downloads = downloads.clone();
                    ConfirmDialog::new(t!("helm-tools-remove-title"))
                        .message(t!("helm-tools-remove-message"))
                        .confirm_label(t!("helm-tools-remove"))
                        .danger()
                        .on_confirm(move |_, cx| {
                            downloads.update(cx, |d, cx| d.remove_helm(cx));
                        })
                });
            })
    };
    let row = match tools::helm_download(line, &available) {
        HelmDownload::Unsupported => row.description(t!("helm-tools-unsupported")),
        HelmDownload::NoDataDir => row.description(t!("helm-tools-no-data-folder")),
        HelmDownload::Absent => row
            .description(t!(
                "helm-tools-absent",
                available = available.clone(),
                checked = checked
            ))
            .control(
                div()
                    .flex()
                    .gap_1()
                    .child(check_button)
                    .child(download_button(t!(
                        "helm-tools-download-helm",
                        version = available.clone()
                    ))),
            ),
        HelmDownload::Current(installed) => row
            .description(t!(
                "helm-tools-current",
                version = installed.version.clone(),
                path = installed.path.display().to_string(),
                verification = verification_text(installed.verification().as_ref()),
                checked = checked
            ))
            .control(
                div()
                    .flex()
                    .gap_1()
                    .child(check_button)
                    .child(remove_button),
            ),
        HelmDownload::Outdated(installed) => row
            .description(t!(
                "helm-tools-outdated",
                version = installed.version.clone(),
                path = installed.path.display().to_string(),
                verification = verification_text(installed.verification().as_ref()),
                available = available.clone(),
                checked = checked
            ))
            .control(
                div()
                    .flex()
                    .gap_1()
                    .child(check_button)
                    .child(download_button(t!(
                        "helm-tools-update-to",
                        version = available.clone()
                    )))
                    .child(remove_button),
            ),
    };
    if let Some(progress) = running {
        row.below(tools::helm_progress(
            "helm-download-progress",
            downloads,
            progress,
            cx,
        ))
    } else if let Some(error) = error {
        row.below(div().text_xs().text_color(cx.theme().danger).child(error))
    } else if let Some(notice) = notice {
        row.below(div().text_xs().text_color(cx.theme().warning).child(notice))
    } else {
        row
    }
}

#[cfg(test)]
mod tests {
    use kubach_core::tools::Verification;
    use kubach_core::tools::signature::Signer;

    use super::verification_text;

    #[test]
    fn says_how_the_copy_was_checked() {
        let signer = Signer {
            user_id: "Scott Rigby <scott@r6by.com>".into(),
            fingerprint: "208DD36ED5BB3745A16743A4C7C6FBB5B91C1155".into(),
        };
        let signed = Verification {
            signer: Some(signer),
            ..Default::default()
        };
        assert_eq!(
            verification_text(Some(&signed)),
            "Signed by Scott Rigby <scott@r6by.com> \
             (208D D36E D5BB 3745 A167 43A4 C7C6 FBB5 B91C 1155)."
        );
        let pinned = Verification {
            pinned: true,
            signature_note: Some("can't reach github.com: timeout".into()),
            ..Default::default()
        };
        assert!(verification_text(Some(&pinned)).starts_with("Checked by the SHA-256 pinned"));
        assert!(verification_text(Some(&pinned)).contains("can't reach github.com"));
        assert_eq!(verification_text(None), "Checked by SHA-256.");
    }
}
