//! Linked kubeconfig files in the UI: linking one after a preview of its
//! contexts, finding a missing one again, unlinking, tightening permissions
//! and watching the files for changes. Used by the hotbar and the settings
//! screen; the file logic itself is in [`kubach_core::kubeconfig`].
//!
//! Also the block both the link and the import previews show about what a
//! kubeconfig runs and reads on connect ([`review_section`]).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use futures::channel::mpsc;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::kubeconfig::review::{ConfigReview, ProgramKind};
use kubach_core::kubeconfig::{self, FileProblem, FilesWatcher, Preview, format_mode};
use kubach_core::settings::{Settings, expand_home};
use kubach_core::{ContextInfo, Contexts};
use kubach_i18n::t;
use kubach_ui::{PathHandler, pick_path};

use crate::settings::AppSettings;

/// Tools rewrite kubeconfig files in several steps; reload once settled.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(300);

/// Linked files from settings, `~/` expanded.
fn linked_paths(cx: &App) -> Vec<PathBuf> {
    AppSettings::try_global(cx)
        .map(|settings| settings.read(cx).get().clusters.resolved_kubeconfig_paths())
        .unwrap_or_default()
}

fn edit_settings(cx: &mut App, edit: impl FnOnce(&mut Settings)) {
    if let Some(settings) = AppSettings::try_global(cx) {
        settings.update(cx, |settings, cx| settings.edit(cx, edit));
    }
}

/// Asks for a kubeconfig file, previews its contexts and links it on
/// confirmation.
pub fn link_file(window: &mut Window, cx: &mut App) {
    pick(None, window, cx);
}

/// Asks where the linked file `missing` (expanded path) went and links the
/// chosen file in its place.
pub fn find_again(missing: PathBuf, window: &mut Window, cx: &mut App) {
    pick(Some(missing), window, cx);
}

/// Unlinks `path` (expanded); its contexts disappear from Kubach.
pub fn unlink(path: &Path, cx: &mut App) {
    let path = path.to_owned();
    edit_settings(cx, move |settings| {
        settings
            .clusters
            .kubeconfig_paths
            .retain(|stored| expand_home(stored) != path);
    });
}

/// Asks before unlinking `path` (expanded): every context of the file
/// (`contexts`) disappears from Kubach; the file itself stays as it is.
pub fn confirm_unlink(path: PathBuf, contexts: Vec<String>, window: &mut Window, cx: &mut App) {
    use gpui_kit::component::button::ButtonVariant;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let path = path.clone();
        dialog
            .confirm()
            .title(t!("kubeconfig-unlink-title", name = name.as_str()))
            .width(px(440.))
            .description(t!(
                "kubeconfig-unlink-description",
                count = contexts.len(),
                path = path.display().to_string(),
                contexts = contexts.join(", ")
            ))
            .ok_text(t!("kubeconfig-unlink"))
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, _, cx| {
                unlink(&path, cx);
                true
            })
    });
}

/// Restricts `path` to its owner (`0600`).
pub fn restrict(path: &Path, window: &mut Window, cx: &mut App) {
    use gpui_kit::component::notification::Notification;
    if let Err(error) = kubeconfig::restrict_permissions(path) {
        window.push_notification(
            Notification::error(t!(
                "kubeconfig-restrict-failed",
                path = path.display().to_string(),
                error = error.to_string()
            )),
            cx,
        );
    }
}

/// Watches `paths`; the receiver gets a message after each settled change.
pub fn watch(paths: &[PathBuf]) -> Option<(FilesWatcher, mpsc::UnboundedReceiver<()>)> {
    let (tx, rx) = mpsc::unbounded();
    let watcher = kubeconfig::watch(paths, WATCH_DEBOUNCE, move || {
        tx.unbounded_send(()).ok();
    })
    .ok()?;
    Some((watcher, rx))
}

/// Previews the contexts of `paths` (e.g. dragged onto the window) and
/// links the readable ones on confirmation.
pub fn link_files(paths: Vec<PathBuf>, window: &mut Window, cx: &mut App) {
    if !paths.is_empty() {
        open_preview(paths, None, window, cx);
    }
}

fn pick(replace: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    let on_pick: PathHandler =
        Rc::new(move |path, window, cx| open_preview(vec![path], replace.clone(), window, cx));
    pick_path(false, on_pick, window, cx);
}

/// Links `path`, in place of `replace` when given.
fn link(path: PathBuf, replace: Option<PathBuf>, cx: &mut App) {
    edit_settings(cx, move |settings| {
        let paths = &mut settings.clusters.kubeconfig_paths;
        let at = |target: &Path| {
            paths
                .iter()
                .position(|stored| expand_home(stored) == target)
        };
        match replace.as_deref().and_then(at) {
            Some(ix) => {
                paths[ix] = path.clone();
                // The new location may also be linked already.
                let mut seen = HashSet::new();
                paths.retain(|stored| seen.insert(expand_home(stored)));
            }
            None if at(&path).is_none() => paths.push(path),
            None => {}
        }
    });
}

/// One file in the link preview.
struct Candidate {
    path: PathBuf,
    preview: Result<Preview, FileProblem>,
    /// Linked already (and not the file being replaced).
    already: bool,
    /// Context names defined before this file: in other files, or in the
    /// files listed above it.
    existing: HashSet<String>,
}

impl Candidate {
    fn can_link(&self) -> bool {
        self.preview.is_ok() && !self.already
    }
}

/// Dialog with the contexts of `paths`, linking them on confirmation;
/// `replace` (with a single path) is the missing file it takes the place of.
fn open_preview(paths: Vec<PathBuf>, replace: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    let linked = linked_paths(cx);
    let others: Vec<PathBuf> = linked
        .iter()
        .filter(|linked| Some(*linked) != replace.as_ref())
        .cloned()
        .collect();
    // Names defined elsewhere win over the ones in these files.
    let mut existing: HashSet<String> = Contexts::load_with_extra(&others)
        .map(|contexts| contexts.list().iter().map(|c| c.name.clone()).collect())
        .unwrap_or_default();
    let candidates: Rc<Vec<Candidate>> = Rc::new(
        paths
            .into_iter()
            .map(|path| {
                let already = linked.contains(&path) && replace.as_ref() != Some(&path);
                let preview = kubeconfig::preview(&path);
                let candidate = Candidate {
                    existing: existing.clone(),
                    path,
                    preview,
                    already,
                };
                if let (true, Ok(preview)) = (candidate.can_link(), &candidate.preview) {
                    existing.extend(preview.contexts.list().iter().map(|c| c.name.clone()));
                }
                candidate
            })
            .collect(),
    );
    let linkable = candidates.iter().filter(|c| c.can_link()).count();
    let several = candidates.len() > 1;
    window.open_alert_dialog(cx, move |dialog, _, cx| {
        let title: SharedString = match (&replace, several) {
            (Some(_), _) => t!("kubeconfig-find-again-title").into(),
            (None, _) => t!("kubeconfig-link-title", count = candidates.len()).into(),
        };
        let body = if several {
            div()
                .id("kubeconfig-preview-files")
                .max_h(px(420.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_4()
                .children(
                    candidates
                        .iter()
                        .map(|c| preview_body(&c.path, &c.preview, &c.existing, c.already, cx)),
                )
                .into_any_element()
        } else {
            let c = &candidates[0];
            preview_body(&c.path, &c.preview, &c.existing, c.already, cx).into_any_element()
        };
        let dialog = dialog
            .title(title)
            .width(px(520.))
            .description(t!("kubeconfig-link-description"))
            .child(body);
        if linkable == 0 {
            return dialog.ok_text(t!("kubeconfig-close"));
        }
        let (candidates, replace) = (candidates.clone(), replace.clone());
        let ok_text = match (&replace, several) {
            (Some(_), _) => t!("kubeconfig-use-file"),
            (None, false) => t!("kubeconfig-link-file"),
            (None, true) => t!("kubeconfig-link-files", count = linkable),
        };
        dialog.confirm().ok_text(ok_text).on_ok(move |_, _, cx| {
            for candidate in candidates.iter().filter(|c| c.can_link()) {
                link(candidate.path.clone(), replace.clone(), cx);
            }
            true
        })
    });
}

/// Warning about a file other users can read.
pub fn permissions_warning(path: &Path, mode: u32) -> String {
    t!(
        "kubeconfig-permissions-warning",
        mode = format_mode(mode),
        path = path.display().to_string()
    )
}

fn preview_body(
    path: &Path,
    preview: &Result<Preview, FileProblem>,
    existing: &HashSet<String>,
    already: bool,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    let mut body = div().flex().flex_col().gap_2().child(
        div()
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .text_color(theme.muted_foreground)
            .child(path.display().to_string()),
    );
    if already {
        body =
            body.child(Alert::info("kubeconfig-linked", t!("kubeconfig-already-linked")).small());
    }
    let preview = match preview {
        Ok(preview) => preview,
        Err(problem) => {
            return body
                .child(Alert::error("kubeconfig-problem", problem.message().to_string()).small());
        }
    };
    if let Some(mode) = preview.loose_mode {
        body =
            body.child(Alert::warning("kubeconfig-mode", permissions_warning(path, mode)).small());
    }
    if let Some(review) = review_section("kubeconfig-review", &preview.review, ReviewFor::Link, cx)
    {
        body = body.child(review);
    }
    let contexts: Vec<ContextInfo> = preview.contexts.list().to_vec();
    if contexts.is_empty() {
        return body.child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t!("kubeconfig-no-contexts")),
        );
    }
    let (muted, warning) = (theme.muted_foreground, theme.warning);
    let rows = contexts.into_iter().map(|context| {
        let shadowed = existing.contains(&context.name);
        div()
            .flex()
            .flex_col()
            .py_1()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .text_sm()
                    .child(context.name.clone())
                    .when(shadowed, |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(warning)
                                .child(t!("kubeconfig-context-shadowed")),
                        )
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .truncate()
                    .child(context.server.unwrap_or_else(|| t!("kubeconfig-no-server"))),
            )
    });
    body.child(div().text_xs().text_color(muted).child(t!(
        "kubeconfig-contexts",
        count = preview.contexts.list().len()
    )))
    .child(
        div()
            .id("kubeconfig-preview-contexts")
            .max_h(px(240.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .children(rows),
    )
}

/// Which preview shows a [`review_section`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewFor {
    /// Linking a file: read in place on every connect.
    Link,
    /// Importing a file: its files are already copied into the config.
    Import,
    /// Importing pasted YAML: its files are read only once confirmed.
    Pasted,
}

/// Programs a kubeconfig runs on connect (exec plugins, auth-provider
/// `cmd-path`) with their environment, and local files it reads, for the
/// link and import previews. Variables that change how programs run and
/// files outside the kubeconfig's folder are highlighted. `None` when there
/// is nothing to show.
pub fn review_section(
    id: &'static str,
    review: &ConfigReview,
    purpose: ReviewFor,
    cx: &App,
) -> Option<Div> {
    if review.is_empty() {
        return None;
    }
    let theme = cx.theme();
    let (muted, danger) = (theme.muted_foreground, theme.danger);
    let mono = theme.mono_font_family.clone();
    let line = || {
        div()
            .text_xs()
            .font_family(mono.clone())
            .whitespace_normal()
    };
    let mut section = div().flex().flex_col().gap_2();
    if !review.programs.is_empty() {
        let purpose = match purpose {
            ReviewFor::Link => "link",
            ReviewFor::Import | ReviewFor::Pasted => "import",
        };
        let message = if review.has_dangerous_env() {
            t!("kubeconfig-review-programs-dangerous", purpose = purpose)
        } else {
            t!("kubeconfig-review-programs", purpose = purpose)
        };
        let programs = review.programs.iter().map(|program| {
            let source = match &program.kind {
                ProgramKind::Exec => t!("kubeconfig-review-exec", user = program.user.as_str()),
                ProgramKind::AuthProvider(name) => t!(
                    "kubeconfig-review-auth-provider",
                    user = program.user.as_str(),
                    name = name.as_str()
                ),
            };
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_xs().text_color(muted).child(source))
                .child(line().child(program.command_line()))
                .children(program.env.iter().map(|var| {
                    let row = line().pl_3().child(format!("{}={}", var.name, var.value));
                    if var.dangerous {
                        row.text_color(danger)
                    } else {
                        row.text_color(muted)
                    }
                }))
        });
        section = section
            .child(Alert::warning((id, 0usize), message).small())
            .child(div().flex().flex_col().gap_2().pl_1().children(programs));
    }
    if !review.files.is_empty() {
        let message = match purpose {
            ReviewFor::Link => t!("kubeconfig-review-files-link"),
            ReviewFor::Import => t!("kubeconfig-review-files-import"),
            ReviewFor::Pasted => t!("kubeconfig-review-files-pasted"),
        };
        let files = review.files.iter().map(|file| {
            let outside = file.outside && purpose != ReviewFor::Pasted;
            let (owner, key) = (file.owner.as_str(), file.field.key());
            let owner = match (file.field.is_cluster(), outside) {
                (true, false) => t!("kubeconfig-review-file-cluster", owner = owner, key = key),
                (true, true) => {
                    t!(
                        "kubeconfig-review-file-cluster-outside",
                        owner = owner,
                        key = key
                    )
                }
                (false, false) => t!("kubeconfig-review-file-user", owner = owner, key = key),
                (false, true) => {
                    t!(
                        "kubeconfig-review-file-user-outside",
                        owner = owner,
                        key = key
                    )
                }
            };
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_xs()
                        .text_color(if file.outside { danger } else { muted })
                        .child(owner),
                )
                .child(
                    line()
                        .when(file.outside, |line| line.text_color(danger))
                        .child(file.path.clone()),
                )
        });
        section = section
            .child(Alert::warning((id, 1usize), message).small())
            .child(div().flex().flex_col().gap_2().pl_1().children(files));
    }
    Some(section)
}
