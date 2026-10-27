use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::components::StatusDot;
use crate::measure::{TEXT_XS, label_column_width};
use crate::tokens::{HOTBAR_WIDTH, Tone};
use kubach_i18n::t;

/// Side of a hotbar tile.
const TILE_SIZE: Pixels = px(36.);

type IdHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;
type MoveHandler = Rc<dyn Fn(&SharedString, &SharedString, &mut Window, &mut App)>;
type ClickHandler = Rc<dyn Fn(&mut Window, &mut App)>;
type TileMenu =
    Rc<dyn Fn(&SharedString, PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;
type Menu = Rc<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;

/// Accent of a hotbar tile: a preset taken from the theme's chart palette,
/// or a custom color chosen by the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TileColor {
    Blue,
    Teal,
    Orange,
    Purple,
    Pink,
    Gray,
    /// `0xRRGGBB` picked by the user. It is user data, not a design token,
    /// so it is drawn as given in both themes.
    Custom(u32),
}

impl TileColor {
    pub const ALL: [TileColor; 6] = [
        TileColor::Blue,
        TileColor::Teal,
        TileColor::Orange,
        TileColor::Purple,
        TileColor::Pink,
        TileColor::Gray,
    ];

    /// Stable lowercase name of a preset, e.g. for saving; `"custom"` for
    /// [`TileColor::Custom`].
    pub fn name(self) -> &'static str {
        match self {
            TileColor::Blue => "blue",
            TileColor::Teal => "teal",
            TileColor::Orange => "orange",
            TileColor::Purple => "purple",
            TileColor::Pink => "pink",
            TileColor::Gray => "gray",
            TileColor::Custom(_) => "custom",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|color| color.name() == name)
    }

    /// Display label, e.g. for a color menu.
    pub fn label(self) -> String {
        match self {
            TileColor::Blue => t!("ui-tile-blue"),
            TileColor::Teal => t!("ui-tile-teal"),
            TileColor::Orange => t!("ui-tile-orange"),
            TileColor::Purple => t!("ui-tile-purple"),
            TileColor::Pink => t!("ui-tile-pink"),
            TileColor::Gray => t!("ui-tile-gray"),
            TileColor::Custom(_) => t!("ui-tile-custom"),
        }
    }

    pub fn color(self, cx: &App) -> Hsla {
        let theme = cx.theme();
        match self {
            TileColor::Blue => theme.chart_1,
            TileColor::Teal => theme.chart_2,
            TileColor::Orange => theme.chart_3,
            TileColor::Purple => theme.chart_4,
            TileColor::Pink => theme.chart_5,
            TileColor::Gray => theme.muted_foreground,
            TileColor::Custom(hex) => rgb(hex).into(),
        }
    }
}

/// A hotbar tile face on its own, e.g. as a preview in settings.
#[derive(IntoElement)]
pub struct TilePreview {
    label: SharedString,
    color: TileColor,
    active: bool,
}

impl TilePreview {
    pub fn new(label: impl Into<SharedString>, color: TileColor) -> Self {
        Self {
            label: label.into(),
            color,
            active: false,
        }
    }

    /// Drawn as the selected tile.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }
}

impl RenderOnce for TilePreview {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        tile_face(&self.label, self.color, self.active, cx).flex_shrink_0()
    }
}

/// One entry of a [`Hotbar`]: a short label on a colored tile.
#[derive(Clone)]
pub struct HotbarTile {
    pub id: SharedString,
    /// Two or three characters: initials or a short alias.
    pub label: SharedString,
    pub color: TileColor,
    /// Status dot in the bottom-right corner.
    pub status: Tone,
    pub active: bool,
    /// Small icon in the top-left corner (e.g. "local cluster", "imported
    /// into the encrypted vault").
    pub icon: Option<Lucide>,
    /// Warning icon in the top-right corner (e.g. "security relaxed").
    pub badge: Option<Lucide>,
    /// Title and `(key, value)` rows shown on hover.
    pub tooltip: Option<(SharedString, Vec<(SharedString, SharedString)>)>,
    /// Can't be used right now (e.g. its kubeconfig file is missing): the
    /// face is dimmed and struck through.
    pub unavailable: bool,
}

impl HotbarTile {
    pub fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            color: TileColor::Blue,
            status: Tone::Neutral,
            active: false,
            icon: None,
            badge: None,
            tooltip: None,
            unavailable: false,
        }
    }

    pub fn unavailable(mut self, unavailable: bool) -> Self {
        self.unavailable = unavailable;
        self
    }

    pub fn color(mut self, color: TileColor) -> Self {
        self.color = color;
        self
    }

    pub fn status(mut self, status: Tone) -> Self {
        self.status = status;
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Any icon of the full Lucide set, e.g. `IconName::HardDrive` or
    /// `Lucide::Lock`.
    pub fn icon(mut self, icon: Option<impl Into<Lucide>>) -> Self {
        self.icon = icon.map(Into::into);
        self
    }

    pub fn badge(mut self, badge: Option<impl Into<Lucide>>) -> Self {
        self.badge = badge.map(Into::into);
        self
    }

    pub fn tooltip(
        mut self,
        title: impl Into<SharedString>,
        rows: Vec<(SharedString, SharedString)>,
    ) -> Self {
        self.tooltip = Some((title.into(), rows));
        self
    }
}

/// Payload of a tile being dragged; also its drag preview.
#[derive(Clone)]
struct DraggedTile {
    id: SharedString,
    label: SharedString,
    color: TileColor,
}

impl Render for DraggedTile {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        tile_face(&self.label, self.color, false, cx).opacity(0.8)
    }
}

/// Square with the label, tinted by `color`.
fn tile_face(label: &SharedString, color: TileColor, active: bool, cx: &App) -> Div {
    let theme = cx.theme();
    let accent = color.color(cx);
    div()
        .size(TILE_SIZE)
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .border_1()
        .border_color(if active { accent } else { theme.transparent })
        .bg(accent.opacity(if active { 0.3 } else { 0.16 }))
        .text_color(if active { theme.foreground } else { accent })
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .child(label.clone())
}

/// Narrow vertical strip of tiles (clusters, workspaces), Slack / Lens style.
///
/// Stateless: the owner keeps the list and reacts to clicks, drops (reorder)
/// and the per-tile context menu.
#[derive(IntoElement)]
pub struct Hotbar {
    id: ElementId,
    tiles: Vec<HotbarTile>,
    /// Empty space above the tiles, e.g. for the macOS traffic lights.
    top_inset: Pixels,
    on_select: Option<IdHandler>,
    on_move: Option<MoveHandler>,
    on_add: Option<ClickHandler>,
    tile_menu: Option<TileMenu>,
    more_menu: Option<Menu>,
}

impl Hotbar {
    pub fn new(id: impl Into<ElementId>, tiles: Vec<HotbarTile>) -> Self {
        Self {
            id: id.into(),
            tiles,
            top_inset: px(0.),
            on_select: None,
            on_move: None,
            on_add: None,
            tile_menu: None,
            more_menu: None,
        }
    }

    pub fn top_inset(mut self, inset: Pixels) -> Self {
        self.top_inset = inset;
        self
    }

    /// Tile clicked.
    pub fn on_select(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    /// Tile `dragged` dropped onto tile `target`: move it to that position.
    pub fn on_move(
        mut self,
        handler: impl Fn(&SharedString, &SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_move = Some(Rc::new(handler));
        self
    }

    /// Shows a "+" button at the bottom.
    pub fn on_add(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_add = Some(Rc::new(handler));
        self
    }

    /// Right-click menu of a tile.
    pub fn tile_menu(
        mut self,
        menu: impl Fn(&SharedString, PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu
        + 'static,
    ) -> Self {
        self.tile_menu = Some(Rc::new(menu));
        self
    }

    /// Shows a "…" button above "+" with this menu (e.g. hidden entries).
    pub fn more_menu(
        mut self,
        menu: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> Self {
        self.more_menu = Some(Rc::new(menu));
        self
    }

    fn render_tile(&self, ix: usize, tile: HotbarTile, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let ring = theme.sidebar;
        let indicator = theme.foreground;
        let drop_target = theme.drop_target;
        let muted = theme.muted_foreground;
        let warning = theme.warning;

        let face = tile_face(&tile.label, tile.color, tile.active, cx)
            .relative()
            .when(tile.unavailable, |face| {
                face.border_dashed()
                    .border_color(muted)
                    .bg(theme.transparent)
                    .text_color(muted)
                    .line_through()
            })
            .when(!tile.active, |face| {
                let hover = tile.color.color(cx).opacity(0.24);
                face.hover(move |face| face.bg(hover))
            })
            .child(
                div()
                    .absolute()
                    .bottom(px(-3.))
                    .right(px(-3.))
                    .p(px(2.))
                    .rounded_full()
                    .bg(ring)
                    .child(StatusDot::new(tile.status)),
            )
            .when_some(tile.icon, |face, icon| {
                // Inside the corner: outside, the hotbar's scroll area
                // clipped the first tile's top.
                face.child(
                    div()
                        .absolute()
                        .top(px(3.))
                        .left(px(3.))
                        .child(Icon::new(icon).size(px(10.)).text_color(muted)),
                )
            })
            .when_some(tile.badge, |face, badge| {
                face.child(
                    div()
                        .absolute()
                        .top(px(3.))
                        .right(px(3.))
                        .child(Icon::new(badge).size(px(10.)).text_color(warning)),
                )
            });

        let dragged = DraggedTile {
            id: tile.id.clone(),
            label: tile.label.clone(),
            color: tile.color,
        };
        let id = tile.id.clone();
        let target = tile.id.clone();
        let on_select = self.on_select.clone();
        let on_move = self.on_move.clone();
        let row = div()
            .id(ElementId::NamedInteger("hotbar-tile".into(), ix as u64))
            .relative()
            .w_full()
            .py_0p5()
            .flex()
            .justify_center()
            .cursor_pointer()
            .when(tile.active, |row| {
                row.child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(10.))
                        .h(px(20.))
                        .w(px(3.))
                        .rounded_r_full()
                        .bg(indicator),
                )
            })
            .child(face)
            .when_some(on_select, |row, handler| {
                row.on_click(move |_, window, cx| handler(&id, window, cx))
            })
            .when_some(tile.tooltip, |row, (title, rows)| {
                row.tooltip(move |window, cx| {
                    let (title, rows) = (title.clone(), rows.clone());
                    Tooltip::element(move |window, cx| tooltip_body(&title, &rows, window, cx))
                        .build(window, cx)
                })
            })
            .when_some(on_move, |row, handler| {
                row.on_drag(dragged, |tile, _, _, cx| cx.new(|_| tile.clone()))
                    .drag_over::<DraggedTile>(move |style, _, _, _| style.bg(drop_target))
                    .on_drop(move |dragged: &DraggedTile, window, cx| {
                        if dragged.id != target {
                            handler(&dragged.id, &target, window, cx);
                        }
                    })
            });

        match self.tile_menu.clone() {
            Some(menu) => {
                let id = tile.id.clone();
                row.context_menu(move |popup, window, cx| menu(&id, popup, window, cx))
                    .into_any_element()
            }
            None => row.into_any_element(),
        }
    }
}

fn tooltip_body(
    title: &SharedString,
    rows: &[(SharedString, SharedString)],
    window: &Window,
    cx: &App,
) -> Div {
    // Keys line up in a column as wide as the widest (#444).
    let key_width = label_column_width(
        rows.iter().map(|(key, _)| key.as_ref()),
        TEXT_XS,
        px(72.),
        px(160.),
        window,
        cx,
    );
    let muted = cx.theme().muted_foreground;
    div()
        .flex()
        .flex_col()
        .gap_1()
        .text_xs()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.clone()),
        )
        .children(rows.iter().map(|(key, value)| {
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .w(key_width)
                        .flex_shrink_0()
                        .text_color(muted)
                        .child(key.clone()),
                )
                .child(value.clone())
        }))
}

/// Round icon button of the hotbar footer.
fn footer_button(id: &'static str, icon: IconName, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let hover = theme.list_hover;
    div()
        .id(id)
        .size(TILE_SIZE)
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .border_1()
        .border_dashed()
        .border_color(theme.border)
        .text_color(theme.muted_foreground)
        .cursor_pointer()
        .hover(move |button| button.bg(hover))
        .child(Icon::new(icon).size_4())
}

impl RenderOnce for Hotbar {
    fn render(mut self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, border) = (theme.sidebar, theme.sidebar_border);
        let tiles = std::mem::take(&mut self.tiles);
        let rendered: Vec<AnyElement> = tiles
            .into_iter()
            .enumerate()
            .map(|(ix, tile)| self.render_tile(ix, tile, cx))
            .collect();

        let more = self.more_menu.clone().map(|menu| {
            Button::new("hotbar-more")
                .ghost()
                .icon(IconName::Ellipsis)
                .dropdown_menu(move |popup, window, cx| menu(popup, window, cx))
        });
        let add = self.on_add.clone().map(|handler| {
            footer_button("hotbar-add", IconName::Plus, cx)
                .on_click(move |_, window, cx| handler(window, cx))
        });

        // The inset belongs to the titlebar: the traffic lights are wider
        // than the hotbar, so it carries the titlebar's bottom line and no
        // background or right border of its own.
        let inset = (self.top_inset > px(0.)).then(|| {
            div()
                .h(self.top_inset)
                .flex_shrink_0()
                .border_b_1()
                .border_color(theme.title_bar_border)
        });

        div()
            .id(self.id)
            .w(HOTBAR_WIDTH)
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .children(inset)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .bg(bg)
                    .border_r_1()
                    .border_color(border)
                    .child(
                        div()
                            .id("hotbar-tiles")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap_1p5()
                            .pt_2()
                            .pb_2()
                            .children(rendered),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_2()
                            .pb_3()
                            .children(more)
                            .children(add),
                    ),
            )
    }
}
