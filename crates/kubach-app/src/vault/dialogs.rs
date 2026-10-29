//! Dialogs of the vault: choosing how to add a cluster, importing with a
//! preview, unlocking, changing the password, and managing entries.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::Contexts;
use kubach_core::vault::{ImportPlan, UnlockMethod, biometric};
use kubach_i18n::t;
use kubach_ui::{PathHandler, pick_path};
use zeroize::Zeroizing;

use super::{Support, VaultState, VaultStatus, global};
use crate::kubeconfig;
use crate::settings::AppSettings;

/// Runs after a successful unlock, e.g. connecting the clicked context.
pub type AfterUnlock = Rc<dyn Fn(&mut Window, &mut App)>;

/// Reopens the dialog the import preview was opened from ("Back").
pub(super) type Reopen = Rc<dyn Fn(&mut Window, &mut App)>;

pub(super) fn label(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

pub(super) fn error_line(error: &Option<SharedString>, cx: &App) -> Option<Div> {
    error
        .clone()
        .map(|error| div().text_xs().text_color(cx.theme().danger).child(error))
}

fn masked(placeholder: String, window: &mut Window, cx: &mut App) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .masked(true)
    })
}

pub(super) fn notify(window: &mut Window, cx: &mut App, note: Notification) {
    window.push_notification(note, cx);
}

/// One way to add a cluster in a chooser dialog: title, explanation and a
/// "Choose" button that closes the dialog and runs `action`.
fn add_option(
    id: &'static str,
    title: String,
    text: String,
    action: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    div()
        .flex()
        .items_center()
        .gap_3()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .child(
            // `min_w_0` lets the description wrap instead of pushing the
            // button out of the dialog.
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_sm().child(title))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(text),
                ),
        )
        .child(
            div().flex_shrink_0().child(
                Button::new(id)
                    .outline()
                    .small()
                    .label(t!("vault-choose"))
                    .on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        action(window, cx);
                    }),
            ),
        )
}

/// "+" on the cluster panel: link a file, import one, paste YAML, or enter
/// a cluster by hand.
pub fn add_cluster(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, cx| {
        dialog.title(t!("vault-add-title")).w(px(520.)).child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(add_option(
                    "add-link",
                    t!("vault-add-link"),
                    t!("vault-link-text"),
                    kubeconfig::link_file,
                    cx,
                ))
                .child(add_option(
                    "add-import",
                    t!("vault-add-import"),
                    t!("vault-import-text"),
                    import_file,
                    cx,
                ))
                .child(add_option(
                    "add-paste",
                    t!("vault-add-paste"),
                    t!("vault-add-paste-text"),
                    import_clipboard,
                    cx,
                ))
                .child(add_option(
                    "add-cloud",
                    t!("vault-add-cloud"),
                    t!("vault-add-cloud-text"),
                    super::cloud::open,
                    cx,
                ))
                .child(add_option(
                    "add-manual",
                    t!("vault-add-manual"),
                    t!("vault-add-manual-text"),
                    super::manual::open,
                    cx,
                )),
        )
    });
}

/// Makes `content` a place to drop kubeconfig files: highlighted while files
/// are dragged over it, asking whether to link or import them on drop.
/// Size the returned element like `content` would be.
pub fn drop_zone(id: impl Into<ElementId>, content: impl IntoElement, cx: &App) -> Div {
    let theme = cx.theme();
    let (background, border) = (theme.drop_target, theme.drag_border);
    div().relative().flex().child(content).child(
        // On top of the content so the highlight shows over its background;
        // its hitbox doesn't block clicks or hover below it.
        div()
            .id(id)
            .absolute()
            .inset_0()
            .rounded_lg()
            .drag_over::<ExternalPaths>(move |style, _, _, _| {
                style.bg(background).border_2().border_color(border)
            })
            .on_drop(|paths: &ExternalPaths, window, cx| {
                add_dropped(paths.paths(), window, cx);
            }),
    )
}

/// Files dragged onto the window: asks whether to link or import them,
/// then shows the usual preview (one for all linked files; imported files
/// one after another, each its own vault entry).
pub fn add_dropped(paths: &[PathBuf], window: &mut Window, cx: &mut App) {
    let paths = kubach_core::kubeconfig::dropped_paths(paths.iter().map(PathBuf::as_path));
    if paths.is_empty() {
        return;
    }
    let paths = Rc::new(paths);
    window.open_dialog(cx, move |dialog, _, cx| {
        let theme = cx.theme();
        let several = paths.len() > 1;
        let names = paths.iter().map(|path| {
            div()
                .w_full()
                .min_w_0()
                .truncate()
                .child(path.display().to_string())
        });
        let (link, import) = (paths.clone(), paths.clone());
        dialog
            .title(t!("vault-dropped-title", count = paths.len()))
            .w(px(520.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .id("dropped-files")
                            .max_h(px(120.))
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .children(names),
                    )
                    .child(add_option(
                        "dropped-link",
                        t!("vault-dropped-link", count = paths.len()),
                        t!("vault-link-text"),
                        move |window, cx| kubeconfig::link_files(link.as_ref().clone(), window, cx),
                        cx,
                    ))
                    .child(add_option(
                        "dropped-import",
                        t!("vault-dropped-import", count = paths.len()),
                        if several {
                            t!("vault-dropped-import-several")
                        } else {
                            t!("vault-import-text")
                        },
                        move |window, cx| import_files(import.as_ref().clone(), window, cx),
                        cx,
                    )),
            )
    });
}

/// Files imported one after another, each with its own preview.
#[derive(Clone, Debug, Default)]
struct Queue {
    /// Files still to preview after the current one.
    rest: Vec<PathBuf>,
    /// Number of the current file, from 1.
    current: usize,
    total: usize,
}

/// Opens the import preview of the first of `paths`; the others follow
/// once it is imported or skipped.
fn import_files(paths: Vec<PathBuf>, window: &mut Window, cx: &mut App) {
    let total = paths.len();
    import_next(
        Queue {
            rest: paths,
            current: 0,
            total,
        },
        window,
        cx,
    );
}

/// Previews the next file of `queue`, if any.
fn import_next(mut queue: Queue, window: &mut Window, cx: &mut App) {
    if queue.rest.is_empty() {
        return;
    }
    let path = queue.rest.remove(0);
    queue.current += 1;
    let plan =
        ImportPlan::from_file(&path, &taken_names(None, cx)).map_err(|e| e.message().to_string());
    let queue = (queue.total > 1).then_some(queue);
    open_preview(plan, Some(path), None, None, queue, window, cx);
}

/// Context names defined now, so imported ones that clash get renamed.
/// `except` is an entry being updated, whose own names don't count.
pub(super) fn taken_names(except: Option<&str>, cx: &App) -> HashSet<String> {
    let linked = AppSettings::try_global(cx)
        .map(|settings| settings.read(cx).get().clusters.resolved_kubeconfig_paths())
        .unwrap_or_default();
    let mut taken: HashSet<String> = Contexts::load_with_extra(&linked)
        .map(|contexts| contexts.list().iter().map(|c| c.name.clone()).collect())
        .unwrap_or_default();
    if let Some(vault) = global(cx) {
        for entry in vault.read(cx).entries() {
            if Some(entry.id.as_str()) != except {
                taken.extend(entry.contexts.iter().cloned());
            }
        }
    }
    taken
}

/// Asks for a kubeconfig file and opens the import preview.
pub fn import_file(window: &mut Window, cx: &mut App) {
    let on_pick: PathHandler = Rc::new(|path, window, cx| {
        let plan = ImportPlan::from_file(&path, &taken_names(None, cx));
        open_import(
            plan.map_err(|e| e.message().to_string()),
            Some(path),
            None,
            window,
            cx,
        );
    });
    pick_path(false, on_pick, window, cx);
}

/// Opens the import preview for kubeconfig YAML on the clipboard.
pub fn import_clipboard(window: &mut Window, cx: &mut App) {
    let text = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .map(Zeroizing::new);
    let Some(text) = text.filter(|text| !text.trim().is_empty()) else {
        notify(window, cx, Notification::error(t!("vault-clipboard-empty")));
        return;
    };
    let plan = ImportPlan::from_yaml(&text, &taken_names(None, cx));
    open_import(
        plan.map_err(|e| e.message().to_string()),
        None,
        None,
        window,
        cx,
    );
}

/// Imports the file `id` came from again, or asks for one.
pub fn update_from_file(id: String, window: &mut Window, cx: &mut App) {
    let origin =
        global(cx).and_then(|vault| vault.read(cx).vault()?.origin(&id).map(Path::to_path_buf));
    let open = move |path: PathBuf, window: &mut Window, cx: &mut App| {
        let plan = ImportPlan::from_file(&path, &taken_names(Some(&id), cx));
        open_import(
            plan.map_err(|e| e.message().to_string()),
            Some(path),
            Some(id.clone()),
            window,
            cx,
        );
    };
    match origin.filter(|path| path.exists()) {
        Some(path) => open(path, window, cx),
        None => {
            let on_pick: PathHandler = Rc::new(move |path, window, cx| open(path, window, cx));
            pick_path(false, on_pick, window, cx);
        }
    }
}

fn open_import(
    plan: Result<ImportPlan, String>,
    origin: Option<PathBuf>,
    replace: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    open_preview(plan, origin, replace, None, None, window, cx);
}

/// The import preview for a config entered by hand; "Back" runs `back`.
pub(super) fn open_manual_import(
    plan: ImportPlan,
    back: Reopen,
    window: &mut Window,
    cx: &mut App,
) {
    open_preview(Ok(plan), None, None, Some(back), None, window, cx);
}

fn open_preview(
    plan: Result<ImportPlan, String>,
    origin: Option<PathBuf>,
    replace: Option<String>,
    back: Option<Reopen>,
    queue: Option<Queue>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(vault) = global(cx) else {
        return;
    };
    let form = cx.new(|cx| {
        let mut form = ImportForm::new(vault, plan, origin, replace, back, window, cx);
        form.queue = queue;
        form
    });
    let title = if form.read(cx).replace.is_some() {
        t!("vault-update-title")
    } else {
        t!("vault-import-title")
    };
    let focus = form.read(cx).name.clone();
    focus.update(cx, |input, cx| input.focus(window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.title(title.clone()).w(px(560.)).child(form.clone())
    });
}

/// Preview of an import, with whatever the vault needs first (a new
/// password, or unlocking).
struct ImportForm {
    vault: Entity<VaultState>,
    plan: Result<ImportPlan, String>,
    origin: Option<PathBuf>,
    replace: Option<String>,
    /// Set when the config was entered by hand: returns to that form.
    back: Option<Reopen>,
    /// Set when several dropped files are imported in turn.
    queue: Option<Queue>,
    name: Entity<InputState>,
    /// Name of each planned context, editable before importing.
    contexts: Vec<Entity<InputState>>,
    password: Entity<InputState>,
    confirm: Entity<InputState>,
    remember: bool,
    /// The user agreed to read the local files of pasted YAML.
    read_files: bool,
    error: Option<SharedString>,
    busy: bool,
    _subscriptions: Vec<Subscription>,
    _task: Option<Task<()>>,
}

impl ImportForm {
    fn new(
        vault: Entity<VaultState>,
        plan: Result<ImportPlan, String>,
        origin: Option<PathBuf>,
        replace: Option<String>,
        back: Option<Reopen>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let default_name = match (&replace, &origin) {
            (Some(id), _) => vault
                .read(cx)
                .entries()
                .iter()
                .find(|entry| &entry.id == id)
                .map(|entry| entry.name.clone())
                .unwrap_or_default(),
            (None, Some(path)) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            (None, None) => plan
                .as_ref()
                .ok()
                .and_then(|plan| plan.contexts.first())
                .map(|context| context.name.clone())
                .unwrap_or_else(|| t!("vault-pasted-name")),
        };
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("vault-name"))
                .default_value(default_name)
        });
        let password = masked(t!("vault-password"), window, cx);
        let confirm = masked(t!("vault-password-repeat"), window, cx);
        let contexts: Vec<_> = plan
            .as_ref()
            .map(|plan| plan.contexts.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|context| {
                let name = context.name.clone();
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(t!("vault-context-name"))
                        .default_value(name)
                })
            })
            .collect();
        let mut subscriptions: Vec<Subscription> = [&name, &password, &confirm]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.submit(window, cx);
                    }
                })
            })
            .collect();
        for (ix, input) in contexts.iter().enumerate() {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                move |this, input, event, window, cx| match event {
                    InputEvent::Change => {
                        let name = input.read(cx).value().to_string();
                        if let Ok(plan) = &mut this.plan {
                            plan.rename(ix, &name);
                        }
                        this.error = None;
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.submit(window, cx),
                    _ => {}
                },
            ));
        }
        let observe = cx.observe(&vault, |_, _, cx| cx.notify());
        let mut this = Self {
            vault,
            plan,
            origin,
            replace,
            back,
            queue: None,
            name,
            contexts,
            password,
            confirm,
            remember: false,
            read_files: false,
            error: None,
            busy: false,
            _subscriptions: subscriptions,
            _task: None,
        };
        this._subscriptions.push(observe);
        this
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Ok(mut plan) = self.plan.clone() else {
            return;
        };
        if plan.unread_files() {
            if !self.read_files {
                self.error = Some(t!("vault-confirm-read-files").into());
                cx.notify();
                return;
            }
            plan.read_files();
        }
        if plan.is_empty() {
            self.error = Some(t!("vault-no-contexts").into());
            cx.notify();
            return;
        }
        if !plan.names_valid() {
            self.error = Some(t!("vault-fix-names").into());
            cx.notify();
            return;
        }
        let name = self.name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.error = Some(t!("vault-enter-name").into());
            cx.notify();
            return;
        }
        let password = Zeroizing::new(self.password.read(cx).value().to_string());
        let confirm = Zeroizing::new(self.confirm.read(cx).value().to_string());
        let remember = self.remember;
        let ready = match self.vault.read(cx).status() {
            VaultStatus::Missing => {
                if password.is_empty() && !remember {
                    self.error = Some(t!("vault-choose-password").into());
                    cx.notify();
                    return;
                }
                if *password != *confirm {
                    self.error = Some(t!("vault-passwords-differ").into());
                    cx.notify();
                    return;
                }
                let password = (!password.is_empty()).then_some(password);
                self.vault
                    .update(cx, |vault, cx| vault.create(password, remember, cx))
            }
            VaultStatus::Locked(_) => {
                let password = (!password.is_empty()).then_some(password);
                self.vault
                    .update(cx, |vault, cx| vault.unlock(password, cx))
            }
            VaultStatus::Unlocked(_) => Task::ready(Ok(())),
            VaultStatus::Broken(error) => Task::ready(Err(error.clone())),
            VaultStatus::Unavailable => Task::ready(Err(t!("vault-no-settings-folder").into())),
        };
        self.busy = true;
        self.error = None;
        cx.notify();
        let (origin, replace, vault, queue) = (
            self.origin.clone(),
            self.replace.clone(),
            self.vault.clone(),
            self.queue.clone(),
        );
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let ready = ready.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                let stored = ready.and_then(|()| {
                    vault.update(cx, |vault, cx| {
                        vault.edit(cx, |vault| match &replace {
                            Some(id) => vault.replace(id, &plan, origin.as_deref()),
                            None => vault.add(&name, &plan, origin.as_deref()).map(|_| ()),
                        })
                    })
                });
                match stored {
                    Ok(()) => {
                        window.close_dialog(cx);
                        let count = plan.contexts.len();
                        notify(
                            window,
                            cx,
                            Notification::success(t!("vault-imported", count = count)),
                        );
                        if let Some(queue) = queue.clone() {
                            import_next(queue, window, cx);
                        }
                    }
                    Err(error) => {
                        this.error = Some(error);
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }

    fn render_plan(&self, plan: &ImportPlan, cx: &App) -> Div {
        let theme = cx.theme();
        let (muted, warning, danger) = (theme.muted_foreground, theme.warning, theme.danger);
        let rows = plan.contexts.iter().enumerate().map(|(ix, context)| {
            let note = match plan.problem(ix) {
                Some(problem) => Some((danger, problem.message().to_string())),
                None if context.is_renamed() && plan.is_taken(&context.original) => Some((
                    warning,
                    t!("vault-renamed-taken", original = context.original.as_str()),
                )),
                None if context.is_renamed() => Some((
                    muted,
                    t!("vault-renamed", original = context.original.as_str()),
                )),
                None => None,
            };
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .py_1()
                .children(self.contexts.get(ix).map(|input| Input::new(input).small()))
                .children(note.map(|(color, text)| div().text_xs().text_color(color).child(text)))
                .child(
                    div().text_xs().text_color(muted).truncate().child(
                        context
                            .server
                            .clone()
                            .unwrap_or_else(|| t!("kubeconfig-no-server")),
                    ),
                )
        });
        let mut body = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_xs().text_color(muted).child(t!(
                "vault-adds",
                contexts = plan.contexts.len(),
                clusters = plan.clusters.len(),
                users = plan.users.len()
            )))
            .child(
                div()
                    .id("vault-import-contexts")
                    .max_h(px(260.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(rows),
            );
        let purpose = if plan.unread_files() {
            kubeconfig::ReviewFor::Pasted
        } else {
            kubeconfig::ReviewFor::Import
        };
        if let Some(review) =
            kubeconfig::review_section("vault-import-review", &plan.review, purpose, cx)
        {
            body = body.child(review);
        }
        for (ix, warning) in plan.warnings.iter().enumerate() {
            body = body
                .child(Alert::warning(("vault-import-warning", ix), warning.to_string()).small());
        }
        body
    }

    fn render_vault(&self, cx: &mut Context<Self>) -> Option<Div> {
        let state = self.vault.read(cx);
        let keychain = state.keychain().clone();
        let theme = cx.theme();
        let note = |text: String| {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        match state.status() {
            VaultStatus::Missing => {
                let available = keychain == Support::Available;
                let explain = match &keychain {
                    Support::Unavailable(reason) => Some(reason.clone()),
                    _ => None,
                };
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(note(t!("vault-new-note")))
                        .child(label(t!("vault-password"), cx))
                        .child(Input::new(&self.password).small())
                        .child(Input::new(&self.confirm).small())
                        .child(
                            Checkbox::new("vault-remember")
                                .label(t!("vault-remember"))
                                .checked(self.remember && available)
                                .disabled(!available)
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.remember = *checked;
                                    cx.notify();
                                })),
                        )
                        .children(explain.map(|reason| note(reason.to_string()))),
                )
            }
            VaultStatus::Locked(locked) => {
                let mode = locked.key_mode();
                let text = if mode.password && mode.biometric {
                    t!("vault-locked-biometric", method = biometric::METHOD_NAME)
                } else if mode.password && mode.keychain {
                    t!("vault-locked-keychain")
                } else if mode.password {
                    t!("vault-locked-password")
                } else {
                    t!("vault-locked-keychain-only")
                };
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(note(text))
                        .when(mode.password, |this| {
                            this.child(Input::new(&self.password).small())
                        }),
                )
            }
            VaultStatus::Broken(error) => Some(
                div().child(
                    Alert::error(
                        "vault-broken",
                        t!("vault-broken", error = error.to_string()),
                    )
                    .small(),
                ),
            ),
            VaultStatus::Unlocked(_) | VaultStatus::Unavailable => None,
        }
    }
}

impl Render for ImportForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let source = match (&self.origin, &self.back) {
            (Some(path), _) => path.display().to_string(),
            (None, Some(_)) => t!("vault-source-manual"),
            (None, None) => t!("vault-source-clipboard"),
        };
        let source = match &self.queue {
            Some(queue) => t!(
                "vault-source-queue",
                current = queue.current,
                total = queue.total,
                source = source
            ),
            None => source,
        };
        let header = div()
            .w_full()
            .min_w_0()
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .text_color(theme.muted_foreground)
            .truncate()
            .child(source);
        let plan = match &self.plan {
            Ok(plan) => self.render_plan(plan, cx),
            // Already says what is wrong ("Not a valid kubeconfig: …",
            // "File not found", …).
            Err(error) => div().child(Alert::error("vault-import-invalid", error.clone()).small()),
        };
        let unread = self.plan.as_ref().is_ok_and(ImportPlan::unread_files);
        let read_files = unread.then(|| {
            Checkbox::new("vault-import-read-files")
                .label(t!("vault-read-files"))
                .checked(self.read_files)
                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                    this.read_files = *checked;
                    this.error = None;
                    cx.notify();
                }))
        });
        let vault = self.render_vault(cx);
        let can_import = self
            .plan
            .as_ref()
            .is_ok_and(|plan| !plan.is_empty() && plan.names_valid())
            && (!unread || self.read_files)
            && !matches!(self.vault.read(cx).status(), VaultStatus::Broken(_));
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(header)
            .child(label(t!("vault-name"), cx))
            .child(Input::new(&self.name).small())
            .child(plan)
            .children(read_files)
            .children(vault)
            .children(error_line(&self.error, cx))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .pt_2()
                    .children(self.back.clone().map(|back| {
                        div().flex_1().child(
                            Button::new("vault-import-back")
                                .ghost()
                                .small()
                                .label(t!("vault-back"))
                                .disabled(self.busy)
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    back(window, cx);
                                }),
                        )
                    }))
                    .child(
                        Button::new("vault-import-cancel")
                            .ghost()
                            .small()
                            .label(t!("vault-cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .children(
                        self.queue
                            .clone()
                            .filter(|queue| !queue.rest.is_empty())
                            .map(|queue| {
                                Button::new("vault-import-skip")
                                    .ghost()
                                    .small()
                                    .label(t!("vault-skip"))
                                    .tooltip(t!("vault-skip-tooltip"))
                                    .disabled(self.busy)
                                    .on_click(move |_, window, cx| {
                                        window.close_dialog(cx);
                                        import_next(queue.clone(), window, cx);
                                    })
                            }),
                    )
                    .child(
                        Button::new("vault-import-ok")
                            .primary()
                            .small()
                            .label(match (self.busy, self.replace.is_some()) {
                                (true, _) => t!("vault-encrypting"),
                                (false, true) => t!("vault-update"),
                                (false, false) => t!("vault-import"),
                            })
                            .disabled(self.busy || !can_import)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

/// Unlocks the vault, then runs `then`. Without typing when possible:
/// Touch ID / Windows Hello opens the unlock dialog waiting for the system
/// prompt (with "Use password"); the system keychain unlocks silently. The
/// password form takes over when they fail or are cancelled.
pub fn prompt_unlock(then: Option<AfterUnlock>, window: &mut Window, cx: &mut App) {
    let Some(vault) = global(cx) else {
        return;
    };
    let state = vault.read(cx);
    let first = state.first_unlock();
    match state.status() {
        VaultStatus::Unlocked(_) => {
            if let Some(then) = then {
                then(window, cx);
            }
        }
        VaultStatus::Locked(_) if first == UnlockMethod::Biometric => {
            open_unlock(then, None, true, window, cx)
        }
        VaultStatus::Locked(_) if first == UnlockMethod::Keychain => {
            let task = vault.update(cx, |vault, cx| vault.unlock(None, cx));
            window
                .spawn(cx, async move |cx| {
                    let result = task.await;
                    cx.update(|window, cx| match result {
                        Ok(()) => {
                            if let Some(then) = then {
                                then(window, cx);
                            }
                        }
                        Err(error) => open_unlock(then, Some(error), false, window, cx),
                    })
                    .ok();
                })
                .detach();
        }
        VaultStatus::Locked(_) => open_unlock(then, None, false, window, cx),
        VaultStatus::Broken(error) => {
            let error = error.clone();
            notify(window, cx, Notification::error(error));
        }
        VaultStatus::Missing | VaultStatus::Unavailable => {}
    }
}

/// The unlock dialog; with `automatic` it first waits for Touch ID /
/// Windows Hello.
fn open_unlock(
    then: Option<AfterUnlock>,
    error: Option<SharedString>,
    automatic: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(vault) = global(cx) else {
        return;
    };
    let form = cx.new(|cx| UnlockForm::new(vault, then, error, window, cx));
    if automatic {
        form.update(cx, |form, cx| form.start_automatic(window, cx));
    } else {
        let focus = form.read(cx).password.clone();
        focus.update(cx, |input, cx| input.focus(window, cx));
    }
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(t!("vault-unlock-title"))
            .w(px(420.))
            .child(form.clone())
    });
}

struct UnlockForm {
    vault: Entity<VaultState>,
    then: Option<AfterUnlock>,
    password: Entity<InputState>,
    error: Option<SharedString>,
    busy: bool,
    /// Waiting for Touch ID / Windows Hello; the password field is hidden
    /// until it fails or "Use password" is clicked.
    waiting: bool,
    _subscriptions: Vec<Subscription>,
    _task: Option<Task<()>>,
    /// The biometric attempt; separate so the password can be typed while
    /// the system prompt is still up.
    _automatic: Option<Task<()>>,
}

impl UnlockForm {
    fn new(
        vault: Entity<VaultState>,
        then: Option<AfterUnlock>,
        error: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let password = masked(t!("vault-password"), window, cx);
        let subscriptions = vec![
            cx.subscribe_in(&password, window, |this, _, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit(false, window, cx);
                }
            }),
            cx.observe(&vault, |_, _, cx| cx.notify()),
        ];
        Self {
            vault,
            then,
            password,
            error,
            busy: false,
            waiting: false,
            _subscriptions: subscriptions,
            _task: None,
            _automatic: None,
        }
    }

    /// Asks the OS for Touch ID / Windows Hello in the background.
    fn start_automatic(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.vault.update(cx, |vault, cx| vault.unlock(None, cx));
        self.waiting = true;
        self.error = None;
        cx.notify();
        self._automatic = Some(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => this.done(window, cx),
                Err(error) => {
                    // Cancelled, fingerprints changed, unavailable: the
                    // password, with the reason (unless already typing).
                    if this.waiting || this.error.is_none() {
                        this.error = Some(error);
                    }
                    this.use_password(window, cx);
                }
            })
            .ok();
        }));
    }

    fn use_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.waiting = false;
        self.password
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn done(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        if let Some(then) = self.then.clone() {
            then(window, cx);
        }
    }

    fn submit(&mut self, keychain: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let password = (!keychain)
            .then(|| Zeroizing::new(self.password.read(cx).value().to_string()))
            .filter(|password| !password.is_empty());
        if password.is_none() && !keychain {
            self.error = Some(t!("vault-enter-password").into());
            cx.notify();
            return;
        }
        let task = self
            .vault
            .update(cx, |vault, cx| vault.unlock(password, cx));
        self.busy = true;
        self.error = None;
        cx.notify();
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => this.done(window, cx),
                    Err(error) => {
                        this.error = Some(error);
                        this.password
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }
}

impl Render for UnlockForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.vault.read(cx);
        let mode = state.key_mode();
        let keychain = mode.is_some_and(|mode| mode.keychain)
            && !matches!(state.keychain(), Support::Unavailable(_));
        let biometric = mode.is_some_and(|mode| mode.biometric)
            && !matches!(state.biometric(), Support::Unavailable(_));
        let has_password = mode.is_none_or(|mode| mode.password);
        let waiting = self.waiting;
        let theme = cx.theme();
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!("vault-unlock-text")),
            )
            .when(waiting, |this| {
                this.child(
                    div()
                        .text_sm()
                        .child(t!("vault-unlock-waiting", method = biometric::METHOD_NAME)),
                )
            })
            .when(has_password && !waiting, |this| {
                this.child(Input::new(&self.password).small())
            })
            .children(error_line(&self.error, cx))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .pt_2()
                    .child(
                        Button::new("vault-unlock-cancel")
                            .ghost()
                            .small()
                            .label(t!("vault-cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .when(waiting && has_password, |this| {
                        this.child(
                            Button::new("vault-unlock-use-password")
                                .primary()
                                .small()
                                .label(t!("vault-use-password"))
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.use_password(window, cx)
                                    }),
                                ),
                        )
                    })
                    .when(biometric && !waiting, |this| {
                        this.child(
                            Button::new("vault-unlock-biometric")
                                .outline()
                                .small()
                                .label(t!("vault-use-biometric", method = biometric::METHOD_NAME))
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.start_automatic(window, cx)
                                })),
                        )
                    })
                    .when(keychain && !biometric && !waiting, |this| {
                        this.child(
                            Button::new("vault-unlock-keychain")
                                .outline()
                                .small()
                                .label(t!("vault-use-keychain"))
                                .disabled(self.busy)
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.submit(true, window, cx)
                                    }),
                                ),
                        )
                    })
                    .when(has_password && !waiting, |this| {
                        this.child(
                            Button::new("vault-unlock-ok")
                                .primary()
                                .small()
                                .label(if self.busy {
                                    t!("vault-unlocking")
                                } else {
                                    t!("vault-unlock")
                                })
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.submit(false, window, cx)
                                })),
                        )
                    }),
            )
    }
}

/// Asks for a new password (twice) and re-encrypts the vault with it.
pub fn change_password(window: &mut Window, cx: &mut App) {
    let Some(vault) = global(cx) else {
        return;
    };
    if !vault.read(cx).is_unlocked() {
        let then: AfterUnlock = Rc::new(change_password);
        prompt_unlock(Some(then), window, cx);
        return;
    }
    let form = cx.new(|cx| PasswordForm::new(vault, window, cx));
    let focus = form.read(cx).password.clone();
    focus.update(cx, |input, cx| input.focus(window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(t!("vault-password-title"))
            .w(px(420.))
            .child(form.clone())
    });
}

struct PasswordForm {
    vault: Entity<VaultState>,
    password: Entity<InputState>,
    confirm: Entity<InputState>,
    error: Option<SharedString>,
    busy: bool,
    _subscriptions: Vec<Subscription>,
    _task: Option<Task<()>>,
}

impl PasswordForm {
    fn new(vault: Entity<VaultState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let password = masked(t!("vault-new-password"), window, cx);
        let confirm = masked(t!("vault-new-password-repeat"), window, cx);
        let subscriptions = [&password, &confirm]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.submit(window, cx);
                    }
                })
            })
            .collect();
        Self {
            vault,
            password,
            confirm,
            error: None,
            busy: false,
            _subscriptions: subscriptions,
            _task: None,
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let password = Zeroizing::new(self.password.read(cx).value().to_string());
        let confirm = Zeroizing::new(self.confirm.read(cx).value().to_string());
        if password.is_empty() {
            self.error = Some(t!("vault-enter-new-password").into());
        } else if *password != *confirm {
            self.error = Some(t!("vault-passwords-differ").into());
        } else {
            self.error = None;
        }
        if self.error.is_some() {
            cx.notify();
            return;
        }
        let task = self
            .vault
            .update(cx, |vault, cx| vault.change_password(password, cx));
        self.busy = true;
        cx.notify();
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        window.close_dialog(cx);
                        notify(
                            window,
                            cx,
                            Notification::success(t!("vault-password-changed")),
                        );
                    }
                    Err(error) => {
                        this.error = Some(error);
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }
}

impl Render for PasswordForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("vault-password-text")),
            )
            .child(Input::new(&self.password).small())
            .child(Input::new(&self.confirm).small())
            .children(error_line(&self.error, cx))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .pt_2()
                    .child(
                        Button::new("vault-password-cancel")
                            .ghost()
                            .small()
                            .label(t!("vault-cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("vault-password-ok")
                            .primary()
                            .small()
                            .label(if self.busy {
                                t!("vault-encrypting")
                            } else {
                                t!("vault-change")
                            })
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

/// Deletes the vault after an explicit warning.
pub fn confirm_reset(window: &mut Window, cx: &mut App) {
    let count = global(cx)
        .map(|vault| vault.read(cx).entries().len())
        .unwrap_or_default();
    window.open_alert_dialog(cx, move |dialog, _, _| {
        dialog
            .confirm()
            .title(t!("vault-reset-title"))
            .width(px(440.))
            .description(t!("vault-reset-description", count = count))
            .ok_text(t!("vault-reset-ok"))
            .ok_variant(ButtonVariant::Danger)
            .on_ok(|_, window, cx| {
                // The keychain entry is deleted in the background; the
                // settings button shows "Deleting…" meanwhile.
                if let Some(vault) = global(cx) {
                    let task = vault.update(cx, |vault, cx| vault.reset(cx));
                    window
                        .spawn(cx, async move |cx| {
                            let notification = match task.await {
                                Ok(None) => return,
                                Ok(Some(warning)) => {
                                    Notification::warning(warning.to_string()).autohide(false)
                                }
                                Err(error) => Notification::error(error),
                            };
                            cx.update(|window, cx| notify(window, cx, notification))
                                .ok();
                        })
                        .detach();
                }
                true
            })
    });
}

/// Removes the imported entry `id` after confirmation.
pub fn confirm_remove(id: String, window: &mut Window, cx: &mut App) {
    let Some(entry) = global(cx).and_then(|vault| {
        vault
            .read(cx)
            .entries()
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
    }) else {
        return;
    };
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let id = id.clone();
        dialog
            .confirm()
            .title(t!("vault-remove-title", name = entry.name.as_str()))
            .width(px(440.))
            .description(t!(
                "vault-remove-description",
                contexts = entry.contexts.join(", ")
            ))
            .ok_text(t!("vault-remove"))
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                let id = id.clone();
                if let Some(vault) = global(cx)
                    && let Err(error) =
                        vault.update(cx, |vault, cx| vault.edit(cx, |vault| vault.remove(&id)))
                {
                    notify(window, cx, Notification::error(error));
                }
                true
            })
    });
}

/// Asks for a new display name of entry `id`.
pub fn rename(id: String, window: &mut Window, cx: &mut App) {
    let Some(current) = global(cx).and_then(|vault| {
        vault
            .read(cx)
            .entries()
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.name.clone())
    }) else {
        return;
    };
    let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
    let dialog_input = input.clone();
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let (input, id) = (dialog_input.clone(), id.clone());
        dialog
            .confirm()
            .title(t!("vault-rename-title"))
            .child(Input::new(&input))
            .on_ok(move |_, window, cx| {
                let name = input.read(cx).value().trim().to_owned();
                if name.is_empty() {
                    return false;
                }
                let id = id.clone();
                if let Some(vault) = global(cx)
                    && let Err(error) = vault.update(cx, |vault, cx| {
                        vault.edit(cx, |vault| vault.rename(&id, &name))
                    })
                {
                    notify(window, cx, Notification::error(error));
                }
                true
            })
    });
    input.update(cx, |input, cx| input.focus(window, cx));
}

/// Writes entry `id` as a plain kubeconfig file, after a warning.
pub fn export(id: String, window: &mut Window, cx: &mut App) {
    let Some(name) = global(cx).and_then(|vault| {
        let vault = vault.read(cx).vault()?;
        vault.entry(&id).map(|entry| entry.name.clone())
    }) else {
        return;
    };
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let (id, name) = (id.clone(), name.clone());
        dialog
            .confirm()
            .title(t!("vault-export-title"))
            .width(px(460.))
            .description(t!("vault-export-description"))
            .ok_text(t!("vault-export-ok"))
            .ok_variant(ButtonVariant::Warning)
            .on_ok(move |_, window, cx| {
                save_export(id.clone(), &name, window, cx);
                true
            })
    });
}

fn save_export(id: String, name: &str, window: &mut Window, cx: &mut App) {
    let dir = dirs_home().unwrap_or_else(|| PathBuf::from("."));
    let suggested = if name.ends_with(".yaml") || name.ends_with(".yml") {
        name.to_owned()
    } else {
        format!("{name}.yaml")
    };
    let target = cx.prompt_for_new_path(&dir, Some(&suggested));
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(path))) = target.await else {
                return;
            };
            cx.update(|window, cx| {
                let yaml = global(cx).and_then(|vault| {
                    vault
                        .read(cx)
                        .vault()?
                        .kubeconfig_yaml(&id)
                        .map(|yaml| Zeroizing::new(yaml.to_owned()))
                });
                let Some(yaml) = yaml else {
                    notify(window, cx, Notification::error(t!("vault-export-locked")));
                    return;
                };
                let note = match write_private(&path, yaml.as_bytes()) {
                    Ok(()) => Notification::success(t!(
                        "vault-exported",
                        path = path.display().to_string()
                    )),
                    Err(error) => Notification::error(t!(
                        "vault-export-failed",
                        path = path.display().to_string(),
                        error = error.to_string()
                    )),
                };
                notify(window, cx, note);
            })
            .ok();
        })
        .detach();
}

fn dirs_home() -> Option<PathBuf> {
    kubach_core::env::home_dir()
}

/// Creates or replaces `path` readable by the owner only (`0600` on Unix,
/// owner-only ACL on Windows).
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    kubach_core::kubeconfig::restrict_permissions(path)?;
    file.write_all(bytes)
}
