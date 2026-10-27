//! "Add manually…": a cluster entered as a form (API server, CA, token,
//! client certificate or exec plugin) or as kubeconfig YAML, tested before
//! saving, then stored through the same import preview (and conflict
//! renaming) as an imported file.
//!
//! Building the kubeconfig, parsing YAML back into the form and explaining
//! connection failures live in `kubach_core` (`vault::manual`, `probe`).

use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    Editor, EditorState, Input, InputEvent, InputState, Textarea, TextareaState,
};
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::*;
use kubach_core::runtime;
use kubach_core::vault::ImportPlan;
use kubach_core::vault::manual::{
    AKS_SERVER_ID, Auth, CaMode, DEFAULT_EXEC_API_VERSION, ExecSpec, ExecTemplate, Field,
    FormProblem, ManualCluster, OidcSpec, TemplateParams, join_args, join_env, missing_commands,
    parse_env, split_args, split_scopes,
};
use kubach_i18n::{Message, msg, t};
use kubach_ui::{CheckResult, CheckStatus, PathHandler, pick_path};
use zeroize::Zeroizing;

use super::dialogs::{Reopen, error_line, label, open_manual_import, taken_names};

/// Largest certificate or key file read into the form.
const MAX_PEM_FILE: u64 = 256 * 1024;
const DIALOG_WIDTH: Pixels = px(640.);
const BODY_MAX_HEIGHT: Pixels = px(500.);
const YAML_HEIGHT: Pixels = px(300.);
const YAML_PLACEHOLDER: &str = "apiVersion: v1\nkind: Config\ncurrent-context: dev\nclusters:\n  - name: dev\n    cluster:\n      server: https://10.0.0.1:6443\n      certificate-authority-data: LS0t…\nusers:\n  - name: dev\n    user:\n      token: …\ncontexts:\n  - name: dev\n    context: { cluster: dev, user: dev }";

/// Opens the form in the main window.
pub fn open(window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| ManualForm::new(window, cx));
    show(form, window, cx);
}

fn show(form: Entity<ManualForm>, window: &mut Window, cx: &mut App) {
    let focus = form.read(cx).context.clone();
    focus.update(cx, |input, cx| input.focus(window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(t!("manual-title"))
            .w(DIALOG_WIDTH)
            .child(form.clone())
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Form,
    Yaml,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CaChoice {
    Custom,
    System,
    Insecure,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthChoice {
    Token,
    ClientCert,
    Basic,
    Oidc,
    Exec,
}

impl AuthChoice {
    const ALL: [Self; 5] = [
        Self::Token,
        Self::ClientCert,
        Self::Basic,
        Self::Oidc,
        Self::Exec,
    ];

    fn label(self) -> String {
        match self {
            Self::Token => t!("manual-auth-token"),
            Self::ClientCert => t!("manual-auth-cert"),
            Self::Basic => t!("manual-auth-basic"),
            Self::Oidc => t!("manual-auth-oidc"),
            Self::Exec => t!("manual-auth-exec"),
        }
    }
}

struct ManualForm {
    mode: Mode,
    context: Entity<InputState>,
    server: Entity<InputState>,
    namespace: Entity<InputState>,
    ca_choice: CaChoice,
    ca: Entity<TextareaState>,
    auth_choice: AuthChoice,
    token: Entity<InputState>,
    cert: Entity<TextareaState>,
    key: Entity<TextareaState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    issuer: Entity<InputState>,
    client_id: Entity<InputState>,
    client_secret: Entity<InputState>,
    scopes: Entity<InputState>,
    /// The exec plugin comes from a template's fields instead of the
    /// command, arguments and environment fields.
    template: Option<ExecTemplate>,
    eks_cluster: Entity<InputState>,
    eks_region: Entity<InputState>,
    eks_profile: Entity<InputState>,
    aks_server_id: Entity<InputState>,
    command: Entity<InputState>,
    args: Entity<InputState>,
    env: Entity<InputState>,
    api_version: Entity<InputState>,
    yaml: Entity<EditorState>,
    /// The YAML last generated from the form; while the editor still holds
    /// it, switching tabs regenerates it instead of keeping stale text.
    generated: Option<Zeroizing<String>>,
    problem: Option<FormProblem>,
    /// Programs of the chosen sign-in that are not in `PATH`.
    missing: Vec<String>,
    error: Option<SharedString>,
    /// Why the YAML could not be shown in the form.
    note: Option<SharedString>,
    check: Option<CheckStatus>,
    /// Bumped on every edit and test, so a late result of an old test is
    /// dropped.
    generation: u64,
    _subscriptions: Vec<Subscription>,
    _check: Option<Task<()>>,
}

impl ManualForm {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |placeholder: &str, window: &mut Window, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_owned();
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let textarea = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(3, 8)
                    .placeholder(placeholder)
            })
        };
        let context = input(&t!("manual-context-placeholder"), window, cx);
        let server = input("https://api.example.com:6443", window, cx);
        let namespace = input("default", window, cx);
        let ca = textarea(
            "-----BEGIN CERTIFICATE-----\n…\n-----END CERTIFICATE-----",
            window,
            cx,
        );
        let token = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("manual-token-placeholder"))
                .masked(true)
        });
        let cert = textarea(
            "-----BEGIN CERTIFICATE-----\n…\n-----END CERTIFICATE-----",
            window,
            cx,
        );
        let key = textarea(
            "-----BEGIN PRIVATE KEY-----\n…\n-----END PRIVATE KEY-----",
            window,
            cx,
        );
        let masked = |placeholder: String, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .masked(true)
            })
        };
        let username = input("admin", window, cx);
        let password = masked(t!("manual-password-placeholder"), window, cx);
        let issuer = input("https://dex.example.com", window, cx);
        let client_id = input("kubernetes", window, cx);
        let client_secret = masked(t!("manual-client-secret-placeholder"), window, cx);
        let scopes = input("email groups", window, cx);
        let eks_cluster = input("prod", window, cx);
        let eks_region = input(&t!("manual-region-placeholder"), window, cx);
        let eks_profile = input("default", window, cx);
        let aks_server_id = input(AKS_SERVER_ID, window, cx);
        aks_server_id.update(cx, |input, cx| input.set_value(AKS_SERVER_ID, window, cx));
        let command = input("aws, kubelogin, gke-gcloud-auth-plugin…", window, cx);
        let args = input("eks get-token --cluster-name prod", window, cx);
        let env = input("AWS_PROFILE=prod AWS_REGION=eu-west-1", window, cx);
        let api_version = input(DEFAULT_EXEC_API_VERSION, window, cx);
        let yaml = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("yaml")
                .line_number(true)
                .placeholder(YAML_PLACEHOLDER)
        });
        let mut subscriptions = Vec::new();
        for input in [
            &context,
            &server,
            &namespace,
            &token,
            &username,
            &password,
            &issuer,
            &client_id,
            &client_secret,
            &scopes,
            &eks_cluster,
            &eks_region,
            &eks_profile,
            &aks_server_id,
            &command,
            &args,
            &env,
            &api_version,
        ] {
            subscriptions.push(cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.edited(cx);
                }
            }));
        }
        for input in [&ca, &cert, &key] {
            subscriptions.push(cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.edited(cx);
                }
            }));
        }
        subscriptions.push(cx.subscribe(&yaml, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.edited(cx);
            }
        }));
        Self {
            mode: Mode::Form,
            context,
            server,
            namespace,
            ca_choice: CaChoice::Custom,
            ca,
            auth_choice: AuthChoice::Token,
            token,
            cert,
            key,
            username,
            password,
            issuer,
            client_id,
            client_secret,
            scopes,
            template: None,
            eks_cluster,
            eks_region,
            eks_profile,
            aks_server_id,
            command,
            args,
            env,
            api_version,
            yaml,
            generated: None,
            problem: None,
            missing: Vec::new(),
            error: None,
            note: None,
            check: None,
            generation: 0,
            _subscriptions: subscriptions,
            _check: None,
        }
    }

    /// Anything changed: the last test result and problems no longer apply.
    fn edited(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.check = None;
        self._check = None;
        self.problem = None;
        self.error = None;
        self.refresh_missing(cx);
        cx.notify();
    }

    /// Looks the chosen plugin up in `PATH` (the login shell's, loaded at
    /// startup).
    fn refresh_missing(&mut self, cx: &App) {
        let auth = match (self.mode, self.auth_choice) {
            (Mode::Form, AuthChoice::Oidc) => Auth::Oidc(OidcSpec::default()),
            (Mode::Form, AuthChoice::Exec) => Auth::Exec(match self.template {
                Some(template) => ExecSpec {
                    command: template.command().into(),
                    ..ExecSpec::default()
                },
                None => ExecSpec {
                    command: Self::text(&self.command, cx),
                    env: parse_env(&Self::text(&self.env, cx)).unwrap_or_default(),
                    ..ExecSpec::default()
                },
            }),
            _ => {
                self.missing.clear();
                return;
            }
        };
        let path = kubach_core::env::path();
        self.missing = missing_commands(&auth, &path);
    }

    fn template_params(&self, cx: &App) -> TemplateParams {
        TemplateParams {
            cluster_name: Self::text(&self.eks_cluster, cx),
            region: Self::text(&self.eks_region, cx),
            profile: Self::text(&self.eks_profile, cx),
            server_id: Self::text(&self.aks_server_id, cx),
        }
    }

    /// Picks a template, or (`None`) goes back to the plain fields, which
    /// then hold what the template made.
    fn set_template(
        &mut self,
        template: Option<ExecTemplate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let (Some(previous), None) = (self.template, template) {
            let spec = previous
                .exec(&self.template_params(cx))
                .unwrap_or_else(|_| ExecSpec {
                    command: previous.command().into(),
                    api_version: DEFAULT_EXEC_API_VERSION.into(),
                    ..ExecSpec::default()
                });
            self.fill_exec(&spec, window, cx);
        }
        self.template = template;
        self.edited(cx);
    }

    fn fill_exec(&self, exec: &ExecSpec, window: &mut Window, cx: &mut App) {
        let set = |input: &Entity<InputState>, text: String, window: &mut Window, cx: &mut App| {
            input.update(cx, |input, cx| input.set_value(text, window, cx));
        };
        set(&self.command, exec.command.clone(), window, cx);
        set(&self.args, join_args(&exec.args), window, cx);
        set(&self.env, join_env(&exec.env), window, cx);
        set(&self.api_version, exec.api_version.clone(), window, cx);
    }

    fn text(input: &Entity<InputState>, cx: &App) -> String {
        input.read(cx).value().to_string()
    }

    fn area(input: &Entity<TextareaState>, cx: &App) -> String {
        input.read(cx).value().to_string()
    }

    /// The form's fields as entered.
    fn read_form(&self, cx: &App) -> Result<ManualCluster, FormProblem> {
        let ca = match self.ca_choice {
            CaChoice::Custom => CaMode::Pem(Self::area(&self.ca, cx)),
            CaChoice::System => CaMode::System,
            CaChoice::Insecure => CaMode::InsecureSkipVerify,
        };
        let auth = match self.auth_choice {
            AuthChoice::Token => Auth::Token(Zeroizing::new(Self::text(&self.token, cx))),
            AuthChoice::ClientCert => Auth::ClientCert {
                cert: Self::area(&self.cert, cx),
                key: Zeroizing::new(Self::area(&self.key, cx)),
            },
            AuthChoice::Basic => Auth::Basic {
                username: Self::text(&self.username, cx),
                password: Zeroizing::new(Self::text(&self.password, cx)),
            },
            AuthChoice::Oidc => Auth::Oidc(OidcSpec {
                issuer_url: Self::text(&self.issuer, cx),
                client_id: Self::text(&self.client_id, cx),
                client_secret: Zeroizing::new(Self::text(&self.client_secret, cx)),
                extra_scopes: split_scopes(&Self::text(&self.scopes, cx)),
            }),
            AuthChoice::Exec => Auth::Exec(match self.template {
                Some(template) => template.exec(&self.template_params(cx))?,
                None => ExecSpec {
                    command: Self::text(&self.command, cx),
                    args: split_args(&Self::text(&self.args, cx))?,
                    env: parse_env(&Self::text(&self.env, cx))?,
                    api_version: Self::text(&self.api_version, cx),
                },
            }),
        };
        Ok(ManualCluster {
            context: Self::text(&self.context, cx),
            server: Self::text(&self.server, cx),
            namespace: Self::text(&self.namespace, cx),
            ca,
            auth,
        })
    }

    fn form_yaml(&self, cx: &App) -> Result<Zeroizing<String>, FormProblem> {
        self.read_form(cx)?.to_yaml()
    }

    /// Shows `cluster` in the form's fields.
    fn fill_form(&mut self, cluster: &ManualCluster, window: &mut Window, cx: &mut Context<Self>) {
        let set = |input: &Entity<InputState>, text: String, window: &mut Window, cx: &mut App| {
            input.update(cx, |input, cx| input.set_value(text, window, cx));
        };
        let set_area =
            |input: &Entity<TextareaState>, text: String, window: &mut Window, cx: &mut App| {
                input.update(cx, |input, cx| input.set_value(text, window, cx));
            };
        set(&self.context, cluster.context.clone(), window, cx);
        set(&self.server, cluster.server.clone(), window, cx);
        set(&self.namespace, cluster.namespace.clone(), window, cx);
        self.ca_choice = match &cluster.ca {
            CaMode::Pem(pem) => {
                set_area(&self.ca, pem.clone(), window, cx);
                CaChoice::Custom
            }
            CaMode::System => CaChoice::System,
            CaMode::InsecureSkipVerify => CaChoice::Insecure,
        };
        self.auth_choice = match &cluster.auth {
            Auth::Token(token) => {
                set(&self.token, token.to_string(), window, cx);
                AuthChoice::Token
            }
            Auth::ClientCert { cert, key } => {
                set_area(&self.cert, cert.clone(), window, cx);
                set_area(&self.key, key.to_string(), window, cx);
                AuthChoice::ClientCert
            }
            Auth::Basic { username, password } => {
                set(&self.username, username.clone(), window, cx);
                set(&self.password, password.to_string(), window, cx);
                AuthChoice::Basic
            }
            Auth::Oidc(oidc) => {
                set(&self.issuer, oidc.issuer_url.clone(), window, cx);
                set(&self.client_id, oidc.client_id.clone(), window, cx);
                set(
                    &self.client_secret,
                    oidc.client_secret.to_string(),
                    window,
                    cx,
                );
                set(&self.scopes, oidc.extra_scopes.join(" "), window, cx);
                AuthChoice::Oidc
            }
            Auth::Exec(exec) => {
                self.fill_exec(exec, window, cx);
                self.template = ExecTemplate::detect(exec).map(|(template, params)| {
                    set(&self.eks_cluster, params.cluster_name, window, cx);
                    set(&self.eks_region, params.region, window, cx);
                    set(&self.eks_profile, params.profile, window, cx);
                    if !params.server_id.is_empty() {
                        set(&self.aks_server_id, params.server_id, window, cx);
                    }
                    template
                });
                AuthChoice::Exec
            }
        };
    }

    fn set_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if mode == self.mode {
            return;
        }
        let text = Zeroizing::new(self.yaml.read(cx).value().to_string());
        let untouched = text.trim().is_empty()
            || self
                .generated
                .as_ref()
                .is_some_and(|generated| **generated == *text);
        self.note = None;
        match mode {
            Mode::Yaml if untouched => {
                // Carry the form over; an incomplete form empties the editor
                // (its placeholder shows the shape) rather than leaving YAML
                // of an earlier state of the form.
                let yaml = self.form_yaml(cx).unwrap_or_default();
                self.yaml
                    .update(cx, |editor, cx| editor.set_value(yaml.as_str(), window, cx));
                self.generated = (!yaml.is_empty()).then_some(yaml);
            }
            Mode::Form if !untouched => match ManualCluster::from_yaml(&text) {
                Ok(cluster) => {
                    self.fill_form(&cluster, window, cx);
                    // The form now matches the YAML, so it may follow the
                    // form again.
                    self.generated = Some(text);
                }
                Err(why) => {
                    self.note = Some(t!("manual-yaml-not-shown", why = why.message()).into());
                }
            },
            Mode::Yaml | Mode::Form => {}
        }
        self.mode = mode;
        self.edited(cx);
    }

    /// The config of the open tab as an import plan; problems are shown.
    fn plan(&mut self, taken: &HashSet<String>, cx: &mut Context<Self>) -> Option<ImportPlan> {
        let yaml = match self.mode {
            Mode::Form => match self.form_yaml(cx) {
                Ok(yaml) => yaml,
                Err(problem) => {
                    self.problem = Some(problem);
                    cx.notify();
                    return None;
                }
            },
            Mode::Yaml => Zeroizing::new(self.yaml.read(cx).value().to_string()),
        };
        if yaml.trim().is_empty() {
            self.error = Some(t!("manual-write-yaml").into());
            cx.notify();
            return None;
        }
        match ImportPlan::from_yaml(&yaml, taken) {
            Ok(plan) if plan.is_empty() => {
                self.error = Some(t!("vault-no-contexts").into());
                cx.notify();
                None
            }
            Ok(plan) => Some(plan),
            Err(error) => {
                self.error = Some(error.message().to_string().into());
                cx.notify();
                None
            }
        }
    }

    /// Connects to the config's (current) context and reports the server
    /// version or what went wrong.
    fn test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Names clashing with existing contexts don't matter for a test.
        let Some(plan) = self.plan(&HashSet::new(), cx) else {
            return;
        };
        let contexts = plan.to_contexts();
        let Some(context) = contexts.default_context().cloned() else {
            return;
        };
        let server = context.server.clone().unwrap_or_default();
        self.generation += 1;
        let generation = self.generation;
        self.check = Some(CheckStatus::Running(
            t!("manual-connecting", server = server.as_str()).into(),
        ));
        cx.notify();
        let several = contexts.list().len() > 1;
        let name = context.name.clone();
        let task = runtime::spawn(async move { contexts.check(&name).await });
        self._check = Some(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let target = if several {
                    format!("{} · {server}", context.name)
                } else {
                    server
                };
                this.check = Some(match result {
                    Ok(Ok(version)) => CheckStatus::Passed {
                        title: t!("manual-connected", version = version.to_string()).into(),
                        detail: Some(target.into()),
                    },
                    Ok(Err(failure)) => CheckStatus::Failed {
                        title: failure.title().to_string().into(),
                        detail: Some(failure.detail.clone().into()),
                        hint: failure.hint().map(|hint| hint.to_string().into()),
                    },
                    // The runtime task panicked or was cancelled.
                    Err(error) => CheckStatus::Failed {
                        title: t!("manual-test-stopped").into(),
                        detail: Some(error.to_string().into()),
                        hint: None,
                    },
                });
                cx.notify();
            })
            .ok();
        }));
    }

    /// Opens the import preview; "Back" there returns to this form.
    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plan) = self.plan(&taken_names(None, cx), cx) else {
            return;
        };
        let this = cx.entity();
        let back: Reopen = Rc::new(move |window, cx| show(this.clone(), window, cx));
        window.close_dialog(cx);
        open_manual_import(plan, back, window, cx);
    }

    /// Asks for a PEM file and puts its text into `target`.
    fn load_file(
        &self,
        target: Entity<TextareaState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let form = cx.entity().downgrade();
        let on_pick: PathHandler = Rc::new(move |path, window, cx| {
            let read = read_pem_file(&path);
            form.update(cx, |this, cx| match read {
                Ok(text) => {
                    target.update(cx, |input, cx| input.set_value(text.as_str(), window, cx));
                    this.edited(cx);
                }
                Err(error) => {
                    this.error = Some(error.into());
                    cx.notify();
                }
            })
            .ok();
        });
        pick_path(false, on_pick, window, cx);
    }

    fn field_problem(&self, field: Field, cx: &App) -> Option<Div> {
        let problem = self.problem.as_ref().filter(|p| p.field() == field)?;
        error_line(&Some(problem.message().to_string().into()), cx)
    }

    /// A labelled field with its problem underneath.
    fn field(&self, name: String, field: Option<Field>, body: impl IntoElement, cx: &App) -> Div {
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .child(label(name, cx))
            .child(body)
            .children(field.and_then(|field| self.field_problem(field, cx)))
    }

    fn pem_field(
        &self,
        name: String,
        field: Field,
        id: &'static str,
        input: &Entity<TextareaState>,
        cx: &mut Context<Self>,
    ) -> Div {
        let target = input.clone();
        let body = div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .child(Textarea::new(input).small())
            .child(
                div().child(
                    Button::new(id)
                        .ghost()
                        .xsmall()
                        .label(t!("manual-load-file"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.load_file(target.clone(), window, cx)
                        })),
                ),
            );
        self.field(name, Some(field), body, cx)
    }

    fn render_form(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let note = |text: String| div().text_xs().text_color(muted).child(text);
        let ca_choice = RadioGroup::horizontal("manual-ca")
            .selected_index(Some(match self.ca_choice {
                CaChoice::Custom => 0,
                CaChoice::System => 1,
                CaChoice::Insecure => 2,
            }))
            .child(Radio::new("manual-ca-custom").label(t!("manual-ca-custom")))
            .child(Radio::new("manual-ca-system").label(t!("manual-ca-system")))
            .child(Radio::new("manual-ca-insecure").label(t!("manual-ca-insecure")))
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.ca_choice = match ix {
                    0 => CaChoice::Custom,
                    1 => CaChoice::System,
                    _ => CaChoice::Insecure,
                };
                this.edited(cx);
            }));
        let ca_body = match self.ca_choice {
            CaChoice::Custom => self.pem_field(
                t!("manual-ca-pem"),
                Field::Ca,
                "manual-ca-file",
                &self.ca,
                cx,
            ),
            CaChoice::System => div().child(note(t!("manual-ca-system-note"))),
            CaChoice::Insecure => div()
                .child(Alert::warning("manual-insecure", t!("manual-insecure-warning")).small()),
        };
        let auth_choice = TabBar::new("manual-auth")
            .segmented()
            .small()
            .selected_index(
                AuthChoice::ALL
                    .iter()
                    .position(|choice| *choice == self.auth_choice)
                    .unwrap_or_default(),
            )
            .children(
                AuthChoice::ALL
                    .iter()
                    .map(|choice| Tab::new().label(choice.label())),
            )
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.auth_choice = AuthChoice::ALL
                    .get(*ix)
                    .copied()
                    .unwrap_or(AuthChoice::Token);
                this.edited(cx);
            }));
        let auth_body = match self.auth_choice {
            AuthChoice::Token => div().child(self.field(
                t!("manual-token"),
                Some(Field::Token),
                Input::new(&self.token).small(),
                cx,
            )),
            AuthChoice::ClientCert => div()
                .flex()
                .flex_col()
                .gap_3()
                .child(self.pem_field(
                    t!("manual-cert-pem"),
                    Field::Cert,
                    "manual-cert-file",
                    &self.cert,
                    cx,
                ))
                .child(self.pem_field(
                    t!("manual-key-pem"),
                    Field::Key,
                    "manual-key-file",
                    &self.key,
                    cx,
                )),
            AuthChoice::Basic => div()
                .flex()
                .flex_col()
                .gap_3()
                .child(self.field(
                    t!("manual-username"),
                    Some(Field::Username),
                    Input::new(&self.username).small(),
                    cx,
                ))
                .child(self.field(
                    t!("manual-password"),
                    Some(Field::Password),
                    Input::new(&self.password).small(),
                    cx,
                ))
                .child(note(t!("manual-basic-note"))),
            AuthChoice::Oidc => div()
                .flex()
                .flex_col()
                .gap_3()
                .child(self.field(
                    t!("manual-issuer"),
                    Some(Field::Issuer),
                    Input::new(&self.issuer).small(),
                    cx,
                ))
                .child(self.field(
                    t!("manual-client-id"),
                    Some(Field::ClientId),
                    Input::new(&self.client_id).small(),
                    cx,
                ))
                .child(self.field(
                    t!("manual-client-secret"),
                    None,
                    Input::new(&self.client_secret).small(),
                    cx,
                ))
                .child(self.field(
                    t!("manual-scopes"),
                    None,
                    Input::new(&self.scopes).small(),
                    cx,
                ))
                .children(self.missing_warning(msg!("manual-oidc-install"), cx))
                .child(note(t!("manual-oidc-note"))),
            AuthChoice::Exec => self.render_exec(cx),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.field(
                t!("manual-context"),
                Some(Field::Context),
                Input::new(&self.context).small(),
                cx,
            ))
            .child(self.field(
                t!("manual-server"),
                Some(Field::Server),
                Input::new(&self.server).small(),
                cx,
            ))
            .child(self.field(
                t!("manual-namespace"),
                None,
                Input::new(&self.namespace).small(),
                cx,
            ))
            .child(self.field(t!("manual-server-certificate"), None, ca_choice, cx))
            .child(ca_body)
            .child(self.field(t!("manual-sign-in"), None, auth_choice, cx))
            .child(auth_body)
    }

    /// A warning that the sign-in's programs are not in `PATH`.
    fn missing_warning(&self, hint: Message, cx: &App) -> Option<Alert> {
        if self.missing.is_empty() {
            return None;
        }
        let (names, count) = (self.missing.join(", "), self.missing.len());
        let text = match crate::shell_env::error(cx) {
            Some(error) => t!(
                "manual-missing-shell",
                count = count,
                names = names,
                hint = hint,
                error = error.to_string()
            ),
            None => t!("manual-missing", count = count, names = names, hint = hint),
        };
        Some(Alert::warning("manual-missing", text).small())
    }

    fn render_exec(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let mono = theme.mono_font_family.clone();
        let note = |text: String| div().text_xs().text_color(muted).child(text);
        let templates = TabBar::new("manual-exec-template")
            .segmented()
            .small()
            .selected_index(match self.template {
                None => 0,
                Some(template) => {
                    1 + ExecTemplate::ALL
                        .iter()
                        .position(|t| *t == template)
                        .unwrap_or_default()
                }
            })
            .child(Tab::new().label(t!("manual-template-custom")))
            .children(
                ExecTemplate::ALL
                    .iter()
                    .map(|template| Tab::new().label(template.label())),
            )
            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                let template = ix
                    .checked_sub(1)
                    .and_then(|ix| ExecTemplate::ALL.get(ix).copied());
                this.set_template(template, window, cx);
            }));
        let input = |name: String, field: Option<Field>, state: &Entity<InputState>| {
            self.field(name, field, Input::new(state).small(), cx)
        };
        let body = match self.template {
            None => div()
                .flex()
                .flex_col()
                .gap_3()
                .child(input(
                    t!("manual-command"),
                    Some(Field::Command),
                    &self.command,
                ))
                .child(input(t!("manual-args"), Some(Field::Args), &self.args))
                .child(input(t!("manual-env"), Some(Field::Env), &self.env))
                .child(input(t!("manual-api-version"), None, &self.api_version)),
            Some(ExecTemplate::Eks) => div()
                .flex()
                .flex_col()
                .gap_3()
                .child(input(
                    t!("manual-eks-cluster"),
                    Some(Field::ClusterName),
                    &self.eks_cluster,
                ))
                .child(input(t!("manual-region"), None, &self.eks_region))
                .child(input(t!("manual-profile"), None, &self.eks_profile)),
            Some(ExecTemplate::Gke) => div().child(note(t!("manual-gke-note"))),
            Some(ExecTemplate::Aks) => div()
                .flex()
                .flex_col()
                .gap_3()
                .child(input(
                    t!("manual-server-id"),
                    Some(Field::ServerId),
                    &self.aks_server_id,
                ))
                .child(note(t!("manual-aks-note"))),
        };
        let preview = self
            .template
            .and_then(|template| template.exec(&self.template_params(cx)).ok())
            .map(|spec| {
                let env = join_env(&spec.env);
                let line = [env, spec.command.clone(), join_args(&spec.args)]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                div()
                    .text_xs()
                    .text_color(muted)
                    .font_family(mono)
                    .child(t!("manual-runs", line = line))
            });
        let hint = self
            .template
            .map_or_else(|| msg!("manual-install-custom"), ExecTemplate::install_hint);
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.field(t!("manual-template"), None, templates, cx))
            .child(body)
            .children(preview)
            .children(self.missing_warning(hint, cx))
            .child(note(t!("manual-exec-note")))
    }

    fn render_yaml(&self, cx: &App) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .h(YAML_HEIGHT)
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .overflow_hidden()
                    .child(Editor::new(&self.yaml).h_full().bordered(false)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("manual-yaml-note")),
            )
    }
}

impl Render for ManualForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = TabBar::new("manual-mode")
            .underline()
            .small()
            .selected_index(match self.mode {
                Mode::Form => 0,
                Mode::Yaml => 1,
            })
            .child(Tab::new().label(t!("manual-tab-form")))
            .child(Tab::new().label("YAML"))
            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                let mode = if *ix == 0 { Mode::Form } else { Mode::Yaml };
                this.set_mode(mode, window, cx);
            }));
        let body = match self.mode {
            Mode::Form => self.render_form(cx),
            Mode::Yaml => self.render_yaml(cx),
        };
        let testing = matches!(self.check, Some(CheckStatus::Running(_)));
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(tabs)
            .children(
                self.note
                    .clone()
                    .map(|note| Alert::info("manual-note", note).small()),
            )
            .child(
                div()
                    .id("manual-body")
                    .max_h(BODY_MAX_HEIGHT)
                    .overflow_y_scroll()
                    .min_w_0()
                    .pr_1()
                    .child(body),
            )
            .children(
                self.check
                    .clone()
                    .map(|status| CheckResult::new("manual-check", status)),
            )
            .children(error_line(&self.error, cx))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .pt_2()
                    .child(
                        div().flex_1().child(
                            Button::new("manual-test")
                                .outline()
                                .small()
                                .label(if testing {
                                    t!("manual-testing")
                                } else {
                                    t!("manual-test")
                                })
                                .disabled(testing)
                                .on_click(cx.listener(|this, _, window, cx| this.test(window, cx))),
                        ),
                    )
                    .child(
                        Button::new("manual-cancel")
                            .ghost()
                            .small()
                            .label(t!("vault-cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("manual-next")
                            .primary()
                            .small()
                            .label(t!("manual-next"))
                            .on_click(cx.listener(|this, _, window, cx| this.next(window, cx))),
                    ),
            )
    }
}

/// Text of a certificate or key file, refusing big or binary (DER) files.
fn read_pem_file(path: &Path) -> Result<Zeroizing<String>, String> {
    let name = path.display().to_string();
    let unreadable = |error: std::io::Error| {
        t!(
            "manual-file-unreadable",
            name = name.as_str(),
            error = error.to_string()
        )
    };
    let size = std::fs::metadata(path).map_err(unreadable)?.len();
    if size > MAX_PEM_FILE {
        return Err(t!("manual-file-too-large", name = name.as_str()));
    }
    let bytes = Zeroizing::new(std::fs::read(path).map_err(unreadable)?);
    std::str::from_utf8(&bytes)
        .map(|text| Zeroizing::new(text.to_owned()))
        .map_err(|_| t!("manual-file-not-pem", name = name.as_str()))
}
