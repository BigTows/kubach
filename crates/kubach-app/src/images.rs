//! Chart icons and README images from the web (#167, #169, ADR 0006).
//!
//! One app-wide [`RemoteImages`] knows which URLs are cached, loading or
//! failed and downloads the missing ones through
//! [`kubach_core::images::Fetcher`] on the I/O runtime. Views ask for an
//! image while they draw it ([`RemoteImages::path`], [`RemoteImages::resolver`]),
//! so only what is on screen is loaded, and observe the entity to redraw
//! when it arrives. With "Load chart icons" off nothing is asked for, the
//! running downloads are aborted and views get no images.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::*;
use kubach_core::images::{self, Fetcher};
use kubach_core::runtime;

use crate::settings::AppSettings;

enum Entry {
    Loading(tokio::task::AbortHandle),
    Ready(PathBuf),
    Failed,
}

/// State shared with the image resolver of a text view, which runs off the
/// entity (it has no `cx`).
#[derive(Default)]
struct Shared {
    entries: Mutex<HashMap<String, Entry>>,
    enabled: AtomicBool,
}

impl Shared {
    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// The cached file of `url`; `None` while unknown, loading or failed.
    fn ready(&self, url: &str) -> Option<PathBuf> {
        match self.entries.lock().ok()?.get(url) {
            Some(Entry::Ready(path)) => Some(path.clone()),
            _ => None,
        }
    }

    fn known(&self, url: &str) -> bool {
        self.entries
            .lock()
            .map(|entries| entries.contains_key(url))
            .unwrap_or(true)
    }
}

/// Downloaded and downloading remote images.
pub struct RemoteImages {
    shared: Arc<Shared>,
    fetcher: Option<Fetcher>,
    requests: mpsc::UnboundedSender<String>,
    _listener: Task<()>,
    _settings: Option<Subscription>,
}

struct GlobalImages(Entity<RemoteImages>);

impl Global for GlobalImages {}

/// Creates the global [`RemoteImages`]; after `settings::init`.
pub fn init(cx: &mut App) {
    let entity = cx.new(RemoteImages::new);
    cx.set_global(GlobalImages(entity));
}

/// Whether "Load chart icons" is on.
fn setting(cx: &App) -> bool {
    AppSettings::try_global(cx).is_none_or(|s| s.read(cx).get().helm.load_chart_icons)
}

impl RemoteImages {
    fn new(cx: &mut Context<Self>) -> Self {
        let shared = Arc::new(Shared::default());
        shared.enabled.store(setting(cx), Ordering::Relaxed);
        let (requests, mut rx) = mpsc::unbounded::<String>();
        let listener = cx.spawn(async move |this, cx| {
            while let Some(url) = rx.next().await {
                if this.update(cx, |this, cx| this.start(url, cx)).is_err() {
                    break;
                }
            }
        });
        let settings = AppSettings::try_global(cx)
            .map(|settings| cx.observe(&settings, |this, _, cx| this.sync_setting(cx)));
        Self {
            shared,
            fetcher: images::cache_dir().map(|dir| Fetcher::new(dir, images::MAX_PARALLEL)),
            requests,
            _listener: listener,
            _settings: settings,
        }
    }

    pub fn global(cx: &App) -> Option<Entity<RemoteImages>> {
        cx.try_global::<GlobalImages>().map(|g| g.0.clone())
    }

    /// Whether images are loaded at all ("Load chart icons").
    pub fn enabled(cx: &App) -> bool {
        Self::global(cx).is_some_and(|images| images.read(cx).shared.enabled())
    }

    /// The cached image of `url`, asking for it when unknown. `None` while
    /// it loads, when it failed, when `url` is not http(s) and when images
    /// are off. Cheap: call it while drawing.
    pub fn path(url: &str, cx: &App) -> Option<PathBuf> {
        let images = Self::global(cx)?;
        images.read(cx).lookup(url)
    }

    fn lookup(&self, url: &str) -> Option<PathBuf> {
        request(&self.shared, &self.requests, url)
    }

    /// Image sources for a text view's document images: the cached file,
    /// else nothing (an empty image) while it loads, fails or images are
    /// off. Never lets the text view fetch a URL by itself.
    pub fn resolver(cx: &App) -> impl Fn(&SharedUri) -> ImageSource + Send + Sync + 'static {
        let state = Self::global(cx).map(|images| {
            let images = images.read(cx);
            (images.shared.clone(), images.requests.clone())
        });
        move |uri: &SharedUri| {
            let path = state
                .as_ref()
                .and_then(|(shared, requests)| request(shared, requests, uri.as_ref()));
            match path {
                Some(path) => ImageSource::from(path),
                None => ImageSource::Custom(Arc::new(|_, _| None)),
            }
        }
    }

    /// Forgets every image after the disk cache was cleared: running
    /// downloads stop, and images are fetched again when drawn.
    pub fn forget_all(&mut self, cx: &mut Context<Self>) {
        if let Ok(mut entries) = self.shared.entries.lock() {
            for entry in entries.values() {
                if let Entry::Loading(abort) = entry {
                    abort.abort();
                }
            }
            entries.clear();
        }
        cx.notify();
    }

    fn sync_setting(&mut self, cx: &mut Context<Self>) {
        let enabled = setting(cx);
        if enabled == self.shared.enabled() {
            return;
        }
        self.shared.enabled.store(enabled, Ordering::Relaxed);
        if let Ok(mut entries) = self.shared.entries.lock() {
            if !enabled {
                for entry in entries.values() {
                    if let Entry::Loading(abort) = entry {
                        abort.abort();
                    }
                }
            }
            // Turned back on: failed URLs get another try.
            entries.retain(|_, entry| matches!(entry, Entry::Ready(_)));
        }
        cx.notify();
    }

    /// Starts downloading `url` unless known or images are off.
    fn start(&mut self, url: String, cx: &mut Context<Self>) {
        if !self.shared.enabled() || self.shared.known(&url) {
            return;
        }
        let Some(fetcher) = self.fetcher.clone() else {
            self.finish(url, None, cx);
            return;
        };
        let task_url = url.clone();
        let handle = runtime::spawn(async move { fetcher.get(&task_url).await });
        if let Ok(mut entries) = self.shared.entries.lock() {
            entries.insert(url.clone(), Entry::Loading(handle.abort_handle()));
        }
        cx.spawn(async move |this, cx| {
            let path = match handle.await {
                Ok(Ok(path)) => Some(path),
                // Aborted: the setting was turned off; its entry is gone.
                Err(error) if error.is_cancelled() => return,
                _ => None,
            };
            this.update(cx, |this, cx| this.finish(url, path, cx)).ok();
        })
        .detach();
    }

    fn finish(&mut self, url: String, path: Option<PathBuf>, cx: &mut Context<Self>) {
        if !self.shared.enabled() {
            return;
        }
        if let Ok(mut entries) = self.shared.entries.lock() {
            let entry = match path {
                Some(path) => Entry::Ready(path),
                None => Entry::Failed,
            };
            entries.insert(url, entry);
        }
        cx.notify();
    }
}

/// The cached file of `url`, or queues a request for it when images are on
/// and it is unknown.
fn request(
    shared: &Shared,
    requests: &mpsc::UnboundedSender<String>,
    url: &str,
) -> Option<PathBuf> {
    if !shared.enabled() || !images::is_remote(url) {
        return None;
    }
    if let Some(path) = shared.ready(url) {
        return Some(path);
    }
    if !shared.known(url) {
        requests.unbounded_send(url.to_owned()).ok();
    }
    None
}
