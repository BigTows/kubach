//! "Managed by" panel of the resource details: the Argo CD Application or
//! Flux Kustomization / HelmRelease named by the object's tracking labels
//! (`kubach_core::gitops`), with links that open it.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::*;
use kubach_core::gitops::{self, LinkState, ManagerLink};
use kubach_core::{ApiCatalog, runtime};
use kubach_i18n::t;
use kubach_ui::{KeyValueList, Panel};
use kube::Client;
use kube::api::DynamicObject;

use super::access::Reveal;

pub struct ManagedBy {
    /// Identity of the object, to keep the panel across updates.
    id: String,
    /// `None` while the managers are looked up.
    links: Option<Vec<ManagerLink>>,
    _task: Option<Task<()>>,
}

impl EventEmitter<Reveal> for ManagedBy {}

impl ManagedBy {
    pub fn new(
        id: String,
        client: Client,
        catalog: ApiCatalog,
        object: &DynamicObject,
        cx: &mut Context<Self>,
    ) -> Self {
        let managers = gitops::managers(&object.metadata);
        if managers.is_empty() {
            return Self {
                id,
                links: Some(Vec::new()),
                _task: None,
            };
        }
        let handle =
            runtime::spawn(async move { gitops::resolve(&client, &catalog, managers).await });
        let task = cx.spawn(async move |this, cx| {
            let links = handle.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.links = Some(links);
                cx.notify();
            })
            .ok();
        });
        Self {
            id,
            links: None,
            _task: Some(task),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Managers found in the cluster: kind title and how to open them, for
    /// the main block of the details.
    pub fn found(&self) -> Vec<(&'static str, Reveal)> {
        self.links
            .iter()
            .flatten()
            .filter(|link| matches!(link.state, LinkState::Found))
            .map(|link| {
                let manager = &link.manager;
                (
                    manager.kind.title(),
                    Reveal {
                        group: manager.kind.group().to_owned(),
                        kind: manager.kind.kind().to_owned(),
                        namespace: manager.namespace.clone(),
                        name: manager.name.clone(),
                    },
                )
            })
            .collect()
    }
}

impl Render for ManagedBy {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let links = self.links.clone().unwrap_or_default();
        let muted = cx.theme().muted_foreground;
        let list = links
            .into_iter()
            .enumerate()
            .fold(KeyValueList::new(), |list, (ix, link)| {
                let manager = &link.manager;
                let label = match &manager.namespace {
                    Some(namespace) => format!("{namespace}/{}", manager.name),
                    None => manager.name.clone(),
                };
                let note = match &link.state {
                    LinkState::Found => None,
                    LinkState::Missing => Some(t!("managed-by-missing")),
                    LinkState::NotServed => Some(t!("managed-by-not-served")),
                    LinkState::Failed(error) => {
                        Some(t!("managed-by-failed", error = error.as_str()))
                    }
                };
                let value = match note {
                    None => {
                        let reveal = Reveal {
                            group: manager.kind.group().to_owned(),
                            kind: manager.kind.kind().to_owned(),
                            namespace: manager.namespace.clone(),
                            name: manager.name.clone(),
                        };
                        Button::new(("managed-by", ix))
                            .link()
                            .small()
                            .label(label)
                            .tooltip(t!("managed-by-open", via = manager.via))
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(reveal.clone())))
                            .into_any_element()
                    }
                    Some(note) => div()
                        .whitespace_normal()
                        .child(label)
                        .child(div().text_xs().text_color(muted).child(t!(
                            "managed-by-note",
                            note = note,
                            via = manager.via
                        )))
                        .into_any_element(),
                };
                list.row(manager.kind.title(), value)
            });
        Panel::new().title(t!("details-managed-by")).child(list)
    }
}
