//! Small squares, one per container (or pod), colored by state: a dense way
//! to show many statuses in a row, with the details in tooltips.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::tokens::Tone;
use kubach_i18n::t;

const CUBE: Pixels = px(10.);
/// Opacity of init container cubes: they matter less once the pod runs.
const DIMMED: f32 = 0.5;

/// One square of [`StatusCubes`].
#[derive(Clone, Debug, PartialEq)]
pub struct StatusCube {
    pub tone: Tone,
    /// e.g. `app: Running`.
    pub tooltip: SharedString,
    /// Drawn as an outline: not running yet, or unknown.
    pub hollow: bool,
}

impl StatusCube {
    /// A filled square; a [`Tone::Neutral`] one is hollow, so "unknown"
    /// does not read as a state.
    pub fn new(tone: Tone, tooltip: impl Into<SharedString>) -> Self {
        Self {
            tone,
            tooltip: tooltip.into(),
            hollow: tone == Tone::Neutral,
        }
    }

    /// Draws the square as an outline (`true`) or filled.
    pub fn hollow(mut self, hollow: bool) -> Self {
        self.hollow = hollow;
        self
    }

    /// The square of a container in `state`, the same in every view.
    pub fn container(state: ContainerCubeState, tooltip: impl Into<SharedString>) -> Self {
        let (tone, hollow) = match state {
            ContainerCubeState::Ready => (Tone::Success, false),
            ContainerCubeState::NotReady => (Tone::Warning, false),
            ContainerCubeState::Starting => (Tone::Warning, true),
            ContainerCubeState::Failing | ContainerCubeState::Failed => (Tone::Danger, false),
            ContainerCubeState::Completed => (Tone::Neutral, false),
            ContainerCubeState::Unknown => (Tone::Neutral, true),
        };
        Self::new(tone, tooltip).hollow(hollow)
    }
}

/// State of a container, as [`StatusCube::container`] draws it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerCubeState {
    /// Running and ready: filled success.
    Ready,
    /// Running, not ready: filled warning.
    NotReady,
    /// Waiting on its way up (`ContainerCreating`): warning outline.
    Starting,
    /// Waiting because it keeps failing (`CrashLoopBackOff`): filled danger.
    Failing,
    /// Exited with code 0: filled gray.
    Completed,
    /// Exited with an error: filled danger.
    Failed,
    /// No status yet: gray outline.
    Unknown,
}

/// Tooltip text of a container cube: name (and role), state, restarts.
///
/// ```text
/// migrate · init
/// Waiting: CrashLoopBackOff
/// Restarts: 7
/// ```
pub fn container_tooltip(name: &str, role: Option<&str>, state: &str, restarts: i32) -> String {
    let mut text = name.to_owned();
    if let Some(role) = role {
        text.push_str(" · ");
        text.push_str(role);
    }
    text.push('\n');
    text.push_str(state);
    text.push('\n');
    text.push_str(&t!("ui-cubes-restarts", count = restarts));
    text
}

/// A row of status squares. Neutral ones are drawn hollow, so "unknown"
/// does not read as a state.
///
/// [`init`](Self::init) squares come first, dimmed and set apart by a thin
/// divider, like init containers before the app containers.
#[derive(IntoElement)]
pub struct StatusCubes {
    id: SharedString,
    init: Vec<StatusCube>,
    cubes: Vec<StatusCube>,
    single_line: bool,
}

impl StatusCubes {
    pub fn new(id: impl Into<SharedString>, cubes: impl IntoIterator<Item = StatusCube>) -> Self {
        Self {
            id: id.into(),
            init: Vec::new(),
            cubes: cubes.into_iter().collect(),
            single_line: false,
        }
    }

    /// Squares drawn before the others, dimmed: init containers.
    pub fn init(mut self, cubes: impl IntoIterator<Item = StatusCube>) -> Self {
        self.init = cubes.into_iter().collect();
        self
    }

    /// Keeps the squares on one line and clips what does not fit, for
    /// table cells of a fixed height.
    pub fn single_line(mut self) -> Self {
        self.single_line = true;
        self
    }
}

fn cube(id: &SharedString, ix: usize, cube: StatusCube, dimmed: bool, cx: &App) -> Stateful<Div> {
    let mut color = cube.tone.color(cx);
    if dimmed {
        color = color.opacity(DIMMED);
    }
    let tooltip = cube.tooltip;
    div()
        .id((id.clone(), ix))
        .size(CUBE)
        .flex_none()
        .rounded_sm()
        .map(|square| {
            if cube.hollow {
                square.border_1().border_color(color)
            } else {
                square.bg(color)
            }
        })
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
}

impl RenderOnce for StatusCubes {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = self.id;
        let has_init = !self.init.is_empty();
        let init_count = self.init.len();
        let init = self
            .init
            .into_iter()
            .enumerate()
            .map(|(ix, c)| cube(&id, ix, c, true, cx))
            .collect::<Vec<_>>();
        let cubes = self
            .cubes
            .into_iter()
            .enumerate()
            .map(|(ix, c)| cube(&id, init_count + ix, c, false, cx))
            .collect::<Vec<_>>();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(3.))
            .map(|row| {
                if self.single_line {
                    row.flex_nowrap().overflow_hidden()
                } else {
                    row.flex_wrap()
                }
            })
            .children(init)
            .when(has_init && !cubes.is_empty(), |row| {
                row.child(
                    div()
                        .flex_none()
                        .w(px(1.))
                        .h(CUBE)
                        .mx(px(1.))
                        .bg(cx.theme().border),
                )
            })
            .children(cubes)
    }
}

#[cfg(test)]
mod tests {
    use super::{ContainerCubeState, StatusCube, container_tooltip};
    use crate::tokens::Tone;

    #[test]
    fn container_states_have_distinct_looks() {
        use ContainerCubeState::*;
        let look = |state| {
            let cube = StatusCube::container(state, "");
            (cube.tone, cube.hollow)
        };
        assert_eq!(look(Ready), (Tone::Success, false));
        assert_eq!(look(NotReady), (Tone::Warning, false));
        assert_eq!(look(Starting), (Tone::Warning, true));
        assert_eq!(look(Failing), (Tone::Danger, false));
        assert_eq!(look(Failed), (Tone::Danger, false));
        assert_eq!(look(Completed), (Tone::Neutral, false));
        assert_eq!(look(Unknown), (Tone::Neutral, true));
        assert!(StatusCube::new(Tone::Neutral, "").hollow);
        assert!(!StatusCube::new(Tone::Success, "").hollow);
    }

    #[test]
    fn tooltip_lists_name_state_and_restarts() {
        assert_eq!(
            container_tooltip("migrate", Some("init"), "Waiting: CrashLoopBackOff", 7),
            "migrate · init\nWaiting: CrashLoopBackOff\nRestarts: 7"
        );
        assert_eq!(
            container_tooltip("app", None, "Running", 0),
            "app\nRunning\nRestarts: 0"
        );
    }
}
