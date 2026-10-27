//! Debug overlay (#291): frame rate and frame time of the main window, CPU,
//! memory and threads of the process, and Kubach's own counters (clusters,
//! watches, cached objects, port forwards, runtime tasks) in a corner of the
//! window. Debug builds only: the module is not compiled into release.
//!
//! `cmd-shift-d` / `ctrl-shift-d` or "Debug Overlay" in the app menu toggles
//! it; whether it is shown is remembered in `state.json`.
//!
//! Frames are counted when the window really draws: [`frame_started`] runs
//! at the top of the workspace render and the overlay's canvas, painted last
//! (deferred), finishes the frame. Nothing requests extra frames, so an
//! unchanged window draws nothing and shows "idle". The overlay's own
//! refresh once a second is not counted. Process metrics and counters are
//! read on the background executor.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::*;
use kubach_core::debug_stats::{CpuMeter, FrameSummary, FrameWindow, ProcessSample};
use kubach_core::{debug_stats, runtime};
use kubach_i18n::t;
use kubach_ui::tokens::{STATUS_BAR_HEIGHT, Tone};
use kubach_ui::{DebugHud, format_bytes};

use crate::cluster::Cluster;
use crate::dock::Forwards;
use crate::state::StateStore;

actions!(
    debug,
    [
        /// Shows or hides the debug overlay (`cmd-shift-d`).
        ToggleDebugOverlay
    ]
);

/// How often the metrics refresh.
const REFRESH: Duration = Duration::from_secs(1);

/// A frame slower than this misses 60 Hz.
const SLOW_FRAME: Duration = Duration::from_micros(16_667);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "secondary-shift-d",
        ToggleDebugOverlay,
        None,
    )]);
}

/// "Debug Overlay" for the app menu.
pub fn menu_item() -> MenuItem {
    MenuItem::action(t!("debug-menu-overlay"), ToggleDebugOverlay)
}

/// Frames of the main window as they are drawn.
#[derive(Default)]
struct FrameProbe {
    /// The workspace render of the frame being drawn began.
    started: Cell<Option<Instant>>,
    /// The next frame is the overlay's own refresh: not counted.
    skip_next: Cell<bool>,
    frames: RefCell<FrameWindow>,
}

impl FrameProbe {
    fn finish(&self) {
        let Some(started) = self.started.take() else {
            return;
        };
        if self.skip_next.replace(false) {
            return;
        }
        let now = Instant::now();
        self.frames
            .borrow_mut()
            .record(now, now.saturating_duration_since(started));
    }
}

/// What the overlay shows.
#[derive(Default)]
struct Metrics {
    frames: FrameSummary,
    cpu: Option<f32>,
    process: Option<ProcessSample>,
    clusters: usize,
    watches: usize,
    objects: usize,
    forwards: usize,
    tasks: usize,
}

/// Read off the UI thread.
struct Sample {
    at: Instant,
    process: Option<ProcessSample>,
    watches: usize,
    objects: usize,
    tasks: usize,
}

pub struct DebugOverlay {
    shown: bool,
    cluster: Entity<Cluster>,
    forwards: Entity<Forwards>,
    probe: Rc<FrameProbe>,
    cpu: CpuMeter,
    metrics: Metrics,
    /// Refreshes the metrics while shown.
    _poll: Option<Task<()>>,
}

impl DebugOverlay {
    pub fn new(cluster: Entity<Cluster>, forwards: Entity<Forwards>, cx: &mut App) -> Entity<Self> {
        let shown =
            StateStore::try_global(cx).is_some_and(|state| state.read(cx).get().debug_overlay);
        cx.new(|cx| {
            let mut this = Self {
                shown: false,
                cluster,
                forwards,
                probe: Rc::default(),
                cpu: CpuMeter::default(),
                metrics: Metrics::default(),
                _poll: None,
            };
            this.set_shown(shown, cx);
            this
        })
    }

    pub fn is_shown(&self) -> bool {
        self.shown
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        let shown = !self.shown;
        self.set_shown(shown, cx);
        if let Some(state) = StateStore::try_global(cx) {
            state.update(cx, |state, cx| {
                state.edit(cx, |state| {
                    let changed = state.debug_overlay != shown;
                    state.debug_overlay = shown;
                    changed
                })
            });
        }
    }

    fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.shown = shown;
        self.probe = Rc::default();
        self.cpu = CpuMeter::default();
        self.metrics = Metrics::default();
        self._poll = shown.then(|| Self::poll(cx));
        cx.notify();
    }

    fn poll(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                let Ok((caches, clusters, forwards)) = this.update(cx, |this, cx| {
                    let (clusters, caches) = this.cluster.read(cx).debug_sessions();
                    (caches, clusters, this.forwards.read(cx).list().len())
                }) else {
                    break;
                };
                let sample = cx
                    .background_executor()
                    .spawn(async move {
                        Sample {
                            process: debug_stats::sample_process(),
                            at: Instant::now(),
                            watches: caches.iter().map(|cache| cache.active_watches()).sum(),
                            objects: caches.iter().map(|cache| cache.cached_objects()).sum(),
                            tasks: runtime::alive_tasks(),
                        }
                    })
                    .await;
                let applied = this.update(cx, |this, cx| {
                    this.apply(sample, clusters, forwards);
                    // The redraw this causes is not a frame of the app.
                    this.probe.skip_next.set(true);
                    cx.notify();
                });
                if applied.is_err() {
                    break;
                }
                cx.background_executor().timer(REFRESH).await;
            }
        })
    }

    fn apply(&mut self, sample: Sample, clusters: usize, forwards: usize) {
        let cpu = sample
            .process
            .and_then(|process| self.cpu.sample(sample.at, process.cpu_time));
        self.metrics = Metrics {
            frames: self.probe.frames.borrow_mut().summary(Instant::now()),
            // Keep the last figure while a reading is skipped.
            cpu: cpu.or(self.metrics.cpu),
            process: sample.process,
            clusters,
            watches: sample.watches,
            objects: sample.objects,
            forwards,
            tasks: sample.tasks,
        };
    }

    fn hud(&self) -> DebugHud {
        let metrics = &self.metrics;
        let frames = metrics.frames;
        let mut hud = DebugHud::new().section(t!("debug-section-frames"));
        hud = if frames.is_idle() {
            hud.toned_row(t!("debug-fps"), t!("debug-idle"), Tone::Neutral)
        } else {
            hud.row(t!("debug-fps"), frames.fps.to_string())
        };
        hud = hud.row(t!("debug-frame"), ms(frames.last));
        let avg_max = format!("{} / {}", ms_value(frames.avg), ms(frames.max));
        let avg_max_label = t!("debug-avg-max");
        hud = match frames.max {
            Some(max) if max > SLOW_FRAME * 2 => {
                hud.toned_row(avg_max_label, avg_max, Tone::Danger)
            }
            Some(max) if max > SLOW_FRAME => hud.toned_row(avg_max_label, avg_max, Tone::Warning),
            _ => hud.row(avg_max_label, avg_max),
        };
        hud = hud.section(t!("debug-section-process"));
        let cpu = metrics.cpu.map_or("—".into(), |cpu| format!("{cpu:.1} %"));
        hud = match metrics.cpu {
            Some(usage) if usage >= 100. => hud.toned_row(t!("debug-cpu"), cpu, Tone::Danger),
            Some(usage) if usage >= 30. => hud.toned_row(t!("debug-cpu"), cpu, Tone::Warning),
            _ => hud.row(t!("debug-cpu"), cpu),
        };
        let (memory, threads) = match metrics.process {
            Some(process) => (format_bytes(process.rss), process.threads.to_string()),
            None => ("—".into(), "—".into()),
        };
        let shortcut = if cfg!(target_os = "macos") {
            "⌘⇧D"
        } else {
            "Ctrl+Shift+D"
        };
        hud.row(t!("debug-memory"), memory)
            .row(t!("debug-threads"), threads)
            .section("Kubach")
            .row(t!("debug-clusters"), metrics.clusters.to_string())
            .row(t!("debug-watches"), metrics.watches.to_string())
            .row(t!("debug-cached-objects"), metrics.objects.to_string())
            .row(t!("debug-port-forwards"), metrics.forwards.to_string())
            .row(t!("debug-runtime-tasks"), metrics.tasks.to_string())
            .footer(t!("debug-hide", shortcut = shortcut))
    }
}

/// `4.2` for a duration, `—` for none.
fn ms_value(duration: Option<Duration>) -> String {
    duration.map_or("—".into(), |duration| {
        format!("{:.1}", duration.as_secs_f64() * 1000.)
    })
}

fn ms(duration: Option<Duration>) -> String {
    match duration {
        Some(_) => t!("debug-ms", value = ms_value(duration)),
        None => "—".into(),
    }
}

impl Render for DebugOverlay {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let probe = self.probe.clone();
        // Painted after everything else: on top, and the end of the frame.
        let card = div().relative().child(self.hud()).child(
            canvas(|_, _, _| {}, move |_, _, _, _| probe.finish())
                .absolute()
                .size_full(),
        );
        div()
            .absolute()
            .right_3()
            .bottom(STATUS_BAR_HEIGHT + px(12.))
            .child(deferred(card).with_priority(1))
    }
}

/// Call at the top of the workspace render: a frame begins.
pub fn frame_started(overlay: &Entity<DebugOverlay>, cx: &App) {
    let overlay = overlay.read(cx);
    if overlay.shown {
        overlay.probe.started.set(Some(Instant::now()));
    }
}

/// The workspace root with the toggle action and, while shown, the overlay.
pub fn attach(root: Div, overlay: &Entity<DebugOverlay>, cx: &App) -> Div {
    let toggle = overlay.clone();
    let root = root
        .relative()
        .on_action(move |_: &ToggleDebugOverlay, _, cx| {
            toggle.update(cx, |overlay, cx| overlay.toggle(cx));
        });
    if overlay.read(cx).is_shown() {
        root.child(overlay.clone())
    } else {
        root
    }
}
