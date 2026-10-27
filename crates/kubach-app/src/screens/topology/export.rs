//! "Export" in the toolbar of the Topology screen (#327): the graph as
//! shown (filters, detail level, network policy layer) to the clipboard as
//! Mermaid or Graphviz DOT, or to a file as SVG or PNG drawn from the
//! canvas layout in the current theme's colors.

use std::path::PathBuf;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::Button;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::*;
use kubach_core::topology::export;
use kubach_i18n::t;
use kubach_ui::{SvgPalette, svg_to_png};

use super::TopologyScreen;

/// What the menu exports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Mermaid,
    Dot,
    Svg,
    Png,
}

impl Format {
    fn extension(self) -> &'static str {
        match self {
            Format::Mermaid => "mmd",
            Format::Dot => "dot",
            Format::Svg => "svg",
            Format::Png => "png",
        }
    }
}

/// Pixel density of a PNG export.
const PNG_SCALE: f32 = 2.;

/// The toolbar button with the export menu; disabled without a graph.
pub(super) fn button(
    screen: &TopologyScreen,
    cx: &mut Context<TopologyScreen>,
) -> impl IntoElement {
    let empty = screen
        .snapshot
        .as_ref()
        .is_none_or(|snapshot| snapshot.graph.nodes().is_empty());
    let this = cx.entity().downgrade();
    Button::new("topology-export")
        .small()
        .outline()
        .icon(Lucide::Download)
        .label(t!("topology-export"))
        .tooltip(t!("topology-export-tooltip"))
        .disabled(empty)
        .dropdown_menu(move |menu: PopupMenu, _, _| {
            [
                (t!("topology-export-mermaid"), Format::Mermaid),
                (t!("topology-export-dot"), Format::Dot),
                (t!("topology-export-svg"), Format::Svg),
                (t!("topology-export-png"), Format::Png),
            ]
            .into_iter()
            .fold(menu, |menu, (label, format)| {
                let this = this.clone();
                menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                    this.update(cx, |this, cx| this.export(format, window, cx))
                        .ok();
                }))
            })
        })
}

impl TopologyScreen {
    fn export(&mut self, format: Format, window: &mut Window, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        match format {
            Format::Mermaid | Format::Dot => {
                let text = if format == Format::Mermaid {
                    export::to_mermaid(&snapshot.graph)
                } else {
                    export::to_dot(&snapshot.graph)
                };
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                let what = if format == Format::Mermaid {
                    "Mermaid"
                } else {
                    "Graphviz DOT"
                };
                window.push_notification(
                    Notification::success(t!("topology-export-copied", format = what)),
                    cx,
                );
            }
            Format::Svg | Format::Png => {
                let palette = SvgPalette::from_app(cx);
                let svg = self.graph.read(cx).to_svg(&palette);
                self.save(format, svg, window, cx);
            }
        }
    }

    /// Asks where to save, then writes the SVG (or its PNG) off the UI
    /// thread.
    fn save(&mut self, format: Format, svg: String, window: &mut Window, cx: &mut Context<Self>) {
        let directory = kubach_core::env::home_dir()
            .map(|home| home.join("Downloads"))
            .filter(|dir| dir.is_dir())
            .or_else(kubach_core::env::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let name = format!("topology.{}", format.extension());
        let path = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return;
            };
            let target = path.clone();
            let written = cx
                .background_spawn(async move {
                    let bytes = match format {
                        Format::Png => svg_to_png(&svg, PNG_SCALE)?,
                        _ => svg.into_bytes(),
                    };
                    std::fs::write(&target, bytes).map_err(|error| error.to_string())
                })
                .await;
            this.update_in(cx, |_, window, cx| {
                let notification = match written {
                    Ok(()) => Notification::success(t!(
                        "topology-export-saved",
                        path = path.display().to_string()
                    )),
                    Err(error) => Notification::error(t!(
                        "topology-export-save-failed",
                        path = path.display().to_string(),
                        error = error
                    )),
                };
                window.push_notification(notification, cx);
            })
            .ok();
        })
        .detach();
    }
}
