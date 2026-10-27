//! What is inside the selected namespace: counts of the main kinds, shown
//! in its details and in the confirmation of its delete. Taken from running
//! watches when there are any, else one small list request per kind, only
//! while a namespace is selected.

use futures::future::join_all;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use kubach_core::namespace::{self, CONTENT_KINDS};
use kubach_core::runtime;
use kubach_i18n::t;
use kubach_ui::{KeyValueList, Panel};

use super::details::ResourceDetails;
use crate::cluster::CatalogState;

/// Counts of the main kinds in one namespace.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Counts {
    /// `(plural title, count)` of the kinds counted, in [`CONTENT_KINDS`]
    /// order.
    pub counted: Vec<(String, usize)>,
    /// Kinds that could not be counted, with why.
    pub failed: Vec<(String, String)>,
}

/// Contents of the namespace shown in the details.
pub(super) struct Contents {
    name: String,
    /// `None` while counting.
    counts: Option<Counts>,
    _task: Option<Task<()>>,
}

/// One kind to count: from the cache, or with a request.
enum Source {
    Cached(usize),
    List(kubach_core::ResourceKind),
}

impl ResourceDetails {
    /// Name of the shown object when it is a namespace.
    fn selected_namespace(&self) -> Option<String> {
        let target = self.target.as_ref()?;
        let api = &target.kind.api;
        if !namespace::is_namespace_kind(&api.group, &api.kind) {
            return None;
        }
        target.object.metadata.name.clone()
    }

    /// Starts counting when another namespace is selected; forgets the
    /// counts when none is.
    pub(super) fn sync_namespace_contents(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.selected_namespace() else {
            self.contents = None;
            return;
        };
        if self.contents.as_ref().is_some_and(|c| c.name == name) {
            return;
        }
        self.count_namespace(name, cx);
    }

    fn count_namespace(&mut self, name: String, cx: &mut Context<Self>) {
        let cluster = self.cluster.read(cx);
        let (Some(cache), CatalogState::Ready(catalog), Some(connection)) =
            (cluster.cache(), cluster.catalog(), cluster.connection())
        else {
            self.contents = None;
            return;
        };
        let client = connection.client.clone();
        let sources: Vec<(String, Source)> = CONTENT_KINDS
            .iter()
            .filter_map(|(group, kind, title)| {
                let kind = catalog.find(group, kind)?;
                let source = match cache.loaded_count_in(kind, &name) {
                    Some(count) => Source::Cached(count),
                    None => Source::List(kind.clone()),
                };
                Some(((*title).to_owned(), source))
            })
            .collect();
        let namespace = name.clone();
        let task = runtime::spawn(async move {
            let counts = sources.into_iter().map(|(title, source)| {
                let client = client.clone();
                let namespace = namespace.clone();
                async move {
                    let count = match source {
                        Source::Cached(count) => Ok(count),
                        Source::List(kind) => namespace::count(&client, &kind, &namespace)
                            .await
                            .map_err(|error| match error {
                                kube::Error::Api(status) if status.is_forbidden() => {
                                    t!("namespace-no-permission")
                                }
                                other => other.to_string(),
                            }),
                    };
                    (title, count)
                }
            });
            let mut result = Counts::default();
            for (title, count) in join_all(counts).await {
                match count {
                    Ok(count) => result.counted.push((title, count)),
                    Err(error) => result.failed.push((title, error)),
                }
            }
            result
        });
        let listener = cx.spawn(async move |this, cx| {
            let counts = task.await.unwrap_or_else(|error| Counts {
                counted: Vec::new(),
                failed: vec![(t!("namespace-contents"), error.to_string())],
            });
            this.update(cx, |this, cx| {
                if let Some(contents) = &mut this.contents {
                    contents.counts = Some(counts);
                    cx.notify();
                }
            })
            .ok();
        });
        self.contents = Some(Contents {
            name,
            counts: None,
            _task: Some(listener),
        });
    }

    /// Counts of namespace `name` if it is the shown one and they are in.
    fn namespace_counts(&self, name: &str) -> Option<&Counts> {
        self.contents
            .as_ref()
            .filter(|contents| contents.name == name)?
            .counts
            .as_ref()
    }

    /// Delete confirmation text for namespaces `names`.
    pub(super) fn namespaces_delete_note(&self, names: &[String]) -> String {
        names
            .iter()
            .map(|name| {
                let counts = self
                    .namespace_counts(name)
                    .filter(|counts| counts.failed.is_empty())
                    .map(|counts| counts.counted.as_slice());
                namespace::delete_note(name, counts).to_string()
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The "Contents" panel of the selected namespace.
    pub(super) fn render_namespace_contents(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let contents = self.contents.as_ref()?;
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let body: AnyElement = match &contents.counts {
            None => div()
                .text_sm()
                .text_color(muted)
                .child(t!("namespace-counting"))
                .into_any_element(),
            Some(counts) => {
                let mut list = KeyValueList::new();
                let present: Vec<&(String, usize)> =
                    counts.counted.iter().filter(|(_, n)| *n > 0).collect();
                for (title, count) in &present {
                    list = list.row(title.clone(), count.to_string());
                }
                for (title, error) in &counts.failed {
                    list = list.row(
                        title.clone(),
                        div()
                            .whitespace_normal()
                            .text_color(muted)
                            .child(t!("namespace-not-counted", error = error.as_str())),
                    );
                }
                let empty = present.is_empty() && counts.failed.is_empty();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(empty, |column| {
                        column.child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child(t!("namespace-contents-empty")),
                        )
                    })
                    .when(!empty, |column| column.child(list))
                    .into_any_element()
            }
        };
        let name = contents.name.clone();
        let total: usize = contents
            .counts
            .as_ref()
            .map(|c| c.counted.iter().map(|(_, n)| n).sum())
            .unwrap_or_default();
        let refresh = Button::new("namespace-contents-refresh")
            .link()
            .xsmall()
            .label(t!("namespace-count-again"))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.count_namespace(name.clone(), cx);
                cx.notify();
            }));
        Some(
            Panel::new()
                .title(t!("namespace-contents"))
                .when(contents.counts.is_some(), |panel| {
                    panel.note(total.to_string())
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(body)
                        .child(div().child(refresh)),
                )
                .into_any_element(),
        )
    }
}
