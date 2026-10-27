//! Remote images: chart icons and README pictures (#167, #169, ADR 0006).
//!
//! The only outgoing requests Kubach makes on its own that go neither to a
//! cluster nor through `helm`. They are off with the "Load chart icons"
//! setting; the caller checks it before asking for anything here.
//!
//! [`Fetcher::get`] returns a file in the disk cache
//! (`<config dir>/cache/images/<sha256 of the URL>`), downloading it first
//! when missing: plain `GET` over http(s) without cookies or credentials,
//! through the proxy from the environment, with a bounded number of parallel
//! downloads, timeouts, a size limit and a check that the body is a PNG,
//! JPEG, GIF, WebP or SVG image of a sane pixel size ([`dimensions`]), an
//! SVG without references to files outside it ([`svg_refs`]). Only
//! public addresses are connected to, every host is resolved and checked
//! before the connection ([`address`]), and a redirect never goes from
//! `https` to `http`. The UI draws the cached file.

pub mod address;
pub mod dimensions;
pub mod svg_refs;

use std::fs;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use http_body_util::BodyExt as _;
use sha2::{Digest as _, Sha256};
use tokio::sync::Semaphore;

/// Responses larger than this are refused.
pub const MAX_IMAGE_BYTES: u64 = 4 * 1024 * 1024;
/// Downloads running at once.
pub const MAX_PARALLEL: usize = 4;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest wait for the next chunk of a response.
const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest time for a whole download, redirects included.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 5;

/// `cache/images` in the config directory (`KUBACH_CONFIG_DIR` overrides it).
pub fn cache_dir() -> Option<PathBuf> {
    crate::disk_cache::dir().map(|dir| dir.join("images"))
}

/// Where the image of `url` is cached in `dir`.
pub fn cache_path(dir: &Path, url: &str) -> PathBuf {
    let hash: String = Sha256::digest(url.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    dir.join(hash)
}

/// Whether `url` is one this module downloads: absolute `http` or `https`.
pub fn is_remote(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && url
            .parse::<http::Uri>()
            .is_ok_and(|uri| uri.host().is_some())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Gif,
    WebP,
    Svg,
}

/// The image format of `bytes` by their content, `None` for anything else
/// (an HTML error page, say).
pub fn sniff(bytes: &[u8]) -> Option<ImageKind> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(ImageKind::Png);
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(ImageKind::Jpeg);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(ImageKind::Gif);
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(ImageKind::WebP);
    }
    let head = &bytes[..bytes.len().min(4096)];
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    let lower = text.to_ascii_lowercase();
    (text.starts_with('<') && lower.contains("<svg")).then_some(ImageKind::Svg)
}

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("not an http(s) URL: {0}")]
    InvalidUrl(String),
    #[error("can't reach {host}: {message}")]
    Network { host: String, message: String },
    #[error("{url} answered {status}")]
    Http { url: String, status: u16 },
    #[error("too many redirects")]
    TooManyRedirects,
    #[error("the image is larger than {limit} bytes")]
    TooLarge { limit: u64 },
    #[error("timed out")]
    Timeout,
    #[error("not a PNG, JPEG, GIF, WebP or SVG image")]
    NotAnImage,
    #[error(
        "the image is {width}×{height} pixels, more than {max}×{max}",
        max = dimensions::MAX_SIDE
    )]
    TooManyPixels { width: u32, height: u32 },
    #[error("the SVG image refers to an external resource: {0}")]
    ExternalReference(String),
    #[error("{host} is a local or private address")]
    Forbidden { host: String },
    #[error("redirect from https to http: {0}")]
    InsecureRedirect(String),
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// Downloads images into a disk cache, a few at a time. Cheap to clone.
#[derive(Clone)]
pub struct Fetcher {
    dir: PathBuf,
    permits: Arc<Semaphore>,
}

impl Fetcher {
    pub fn new(dir: PathBuf, parallel: usize) -> Self {
        Self {
            dir,
            permits: Arc::new(Semaphore::new(parallel.max(1))),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The cached file of `url`, if downloaded before.
    pub fn cached(&self, url: &str) -> Option<PathBuf> {
        let path = cache_path(&self.dir, url);
        path.is_file().then_some(path)
    }

    /// The cached file of `url`, downloading it first when missing. Must
    /// run on a tokio runtime.
    pub async fn get(&self, url: &str) -> Result<PathBuf, ImageError> {
        if !is_remote(url) {
            return Err(ImageError::InvalidUrl(url.to_owned()));
        }
        if let Some(path) = self.checked_cache(url).await {
            return Ok(path);
        }
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| io::Error::other("image fetcher closed"))?;
        // Another request for the same URL may have finished meanwhile.
        if let Some(path) = self.checked_cache(url).await {
            return Ok(path);
        }
        let bytes = tokio::time::timeout(TOTAL_TIMEOUT, download(url, MAX_IMAGE_BYTES))
            .await
            .map_err(|_| ImageError::Timeout)??;
        let bytes = tokio::task::spawn_blocking(move || validate(&bytes).map(|()| bytes))
            .await
            .map_err(io::Error::other)??;
        store(&self.dir, url, &bytes)
    }

    /// The cached file of `url` if it passes [`validate`]; one that does
    /// not (cached by an older version) is deleted and downloaded again.
    async fn checked_cache(&self, url: &str) -> Option<PathBuf> {
        let path = self.cached(url)?;
        let check = path.clone();
        let valid = tokio::task::spawn_blocking(move || {
            fs::read(&check).is_ok_and(|bytes| validate(&bytes).is_ok())
        })
        .await
        .unwrap_or(false);
        if valid {
            return Some(path);
        }
        fs::remove_file(&path).ok();
        None
    }
}

/// Fails unless `bytes` are a PNG, JPEG, GIF, WebP or SVG image within the
/// [`dimensions`] limits, an SVG without external references
/// ([`svg_refs`]). Parses SVG: run it off the async threads.
pub fn validate(bytes: &[u8]) -> Result<(), ImageError> {
    let kind = sniff(bytes).ok_or(ImageError::NotAnImage)?;
    if kind == ImageKind::Svg {
        svg_refs::check(bytes)?;
    }
    dimensions::check(kind, bytes)
}

/// Writes `bytes` as the cached image of `url`: to a temporary file first,
/// renamed into place, so a reader never sees half a file.
fn store(dir: &Path, url: &str, bytes: &[u8]) -> Result<PathBuf, ImageError> {
    fs::create_dir_all(dir)?;
    let path = cache_path(dir, url);
    let mut temp = tempfile::Builder::new().prefix(".part-").tempfile_in(dir)?;
    io::Write::write_all(&mut temp, bytes)?;
    temp.persist(&path).map_err(|e| e.error)?;
    Ok(path)
}

/// Where a redirect from `base` to `location` goes.
fn resolve(base: &http::Uri, location: &str) -> Option<String> {
    let location = location.trim();
    if is_remote(location) {
        return Some(location.to_owned());
    }
    let scheme = base.scheme_str()?;
    let authority = base.authority()?;
    if let Some(rest) = location.strip_prefix("//") {
        return Some(format!("{scheme}://{rest}"));
    }
    if location.starts_with('/') {
        return Some(format!("{scheme}://{authority}{location}"));
    }
    let path = base.path();
    let dir = &path[..path.rfind('/').map_or(0, |ix| ix + 1)];
    let dir = if dir.is_empty() { "/" } else { dir };
    Some(format!("{scheme}://{authority}{dir}{location}"))
}

/// Where a redirect from `url` to `location` goes: an http(s) URL, and
/// never from `https` down to `http`.
fn redirect_target(url: &str, location: &str) -> Result<String, ImageError> {
    let base: http::Uri = url
        .parse()
        .map_err(|_| ImageError::InvalidUrl(url.to_owned()))?;
    let next = resolve(&base, location)
        .filter(|next| is_remote(next))
        .ok_or_else(|| ImageError::InvalidUrl(location.to_owned()))?;
    let secure = |url: &str| {
        url.trim_start()
            .to_ascii_lowercase()
            .starts_with("https://")
    };
    if secure(url) && !secure(&next) {
        return Err(ImageError::InsecureRedirect(next));
    }
    Ok(next)
}

/// GETs `url`, following redirects, into memory: at most `limit` bytes.
async fn download(url: &str, limit: u64) -> Result<Vec<u8>, ImageError> {
    let mut url = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        match get_once(&url, limit).await? {
            Response::Body(bytes) => return Ok(bytes),
            Response::Redirect(location) => url = redirect_target(&url, &location)?,
        }
    }
    Err(ImageError::TooManyRedirects)
}

/// The addresses of `host` to connect to, all checked with
/// [`address::is_forbidden`]: one forbidden address refuses the host, so a
/// name that resolves to both a public and a private address is not used.
/// The caller connects to these addresses, not to the name, so a second
/// lookup can't return something else (DNS rebinding). Through a `proxy`
/// the proxy resolves the name itself: a name that doesn't resolve here is
/// left to it, and the list is empty.
async fn resolve_host(host: &str, port: u16, proxy: bool) -> Result<Vec<SocketAddr>, ImageError> {
    let forbidden = || ImageError::Forbidden {
        host: host.to_owned(),
    };
    if let Ok(ip) = host.parse::<IpAddr>() {
        return if address::is_forbidden(ip) {
            Err(forbidden())
        } else {
            Ok(vec![SocketAddr::new(ip, port)])
        };
    }
    let addrs: Vec<SocketAddr> = match tokio::net::lookup_host((host, port)).await {
        Ok(addrs) => addrs.collect(),
        Err(_) if proxy => return Ok(Vec::new()),
        Err(e) => {
            return Err(ImageError::Network {
                host: host.to_owned(),
                message: e.to_string(),
            });
        }
    };
    if addrs.iter().any(|addr| address::is_forbidden(addr.ip())) {
        return Err(forbidden());
    }
    if addrs.is_empty() && !proxy {
        return Err(ImageError::Network {
            host: host.to_owned(),
            message: "no addresses".to_owned(),
        });
    }
    Ok(addrs)
}

enum Response {
    Body(Vec<u8>),
    Redirect(String),
}

async fn get_once(url: &str, limit: u64) -> Result<Response, ImageError> {
    let invalid = || ImageError::InvalidUrl(url.to_owned());
    if !is_remote(url) {
        return Err(invalid());
    }
    let uri: http::Uri = url.parse().map_err(|_| invalid())?;
    let (Some(scheme), Some(authority)) = (uri.scheme(), uri.authority()) else {
        return Err(invalid());
    };
    let https = *scheme == http::uri::Scheme::HTTPS;
    // `[::1]` in a URL, `::1` as an address.
    let host = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = authority.port_u16().unwrap_or(if https { 443 } else { 80 });
    let network = |message: String| ImageError::Network {
        host: host.clone(),
        message,
    };
    let proxy = crate::tools::proxy::from_env(&uri);
    let addrs = resolve_host(&host, port, proxy.is_some()).await?;
    // The name and port of the URL, without user info.
    let host_header = match authority.port() {
        Some(port) => format!("{}:{port}", authority.host()),
        None => authority.host().to_owned(),
    };
    // Straight to each checked address in turn; through a proxy, the name
    // goes to the proxy.
    let targets: Vec<String> = if proxy.is_some() {
        vec![format!("{scheme}://{host_header}")]
    } else {
        addrs
            .iter()
            .map(|addr| format!("{scheme}://{addr}"))
            .collect()
    };
    let path = uri.path_and_query().map_or("/", |p| p.as_str());
    let mut response = None;
    let mut last_error = String::new();
    for base in targets {
        let base: http::Uri = base.parse().map_err(|_| invalid())?;
        // kube's client is hyper + rustls with the system roots, already in
        // the tree; without kubeconfig auth it sends no credentials.
        let mut config = kube::Config::new(base);
        config.connect_timeout = Some(CONNECT_TIMEOUT);
        config.read_timeout = Some(READ_TIMEOUT);
        config.proxy_url = proxy.clone();
        if https {
            // The certificate is checked against the name, not the address.
            config.tls_server_name = Some(host.clone());
        }
        let client = match kube::Client::try_from(config) {
            Ok(client) => client,
            Err(e) => {
                last_error = e.to_string();
                continue;
            }
        };
        let request = http::Request::get(path)
            .header(http::header::HOST, &host_header)
            .header(http::header::ACCEPT, "image/*")
            // Some hosts (Wikimedia) refuse requests without one.
            .header(
                http::header::USER_AGENT,
                format!(
                    "Kubach/{} (+{})",
                    crate::VERSION,
                    env!("CARGO_PKG_REPOSITORY")
                ),
            )
            .body(kube::client::Body::empty())
            .map_err(|_| invalid())?;
        match client.send(request).await {
            Ok(ok) => {
                response = Some(ok);
                break;
            }
            Err(e) => last_error = e.to_string(),
        }
    }
    let response = response.ok_or_else(|| network(last_error))?;
    let status = response.status();
    if status.is_redirection() {
        let location = response
            .headers()
            .get(http::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ImageError::Http {
                url: url.to_owned(),
                status: status.as_u16(),
            })?;
        return Ok(Response::Redirect(location.to_owned()));
    }
    if !status.is_success() {
        return Err(ImageError::Http {
            url: url.to_owned(),
            status: status.as_u16(),
        });
    }
    let total = response
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
    if total.is_some_and(|total| total > limit) {
        return Err(ImageError::TooLarge { limit });
    }
    let mut bytes = Vec::new();
    let mut body = response.into_body().into_data_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| network(e.to_string()))?;
        if (bytes.len() + chunk.len()) as u64 > limit {
            return Err(ImageError::TooLarge { limit });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Response::Body(bytes))
}

/// `markdown` with every image replaced by its alt text, for when remote
/// images are off: Markdown images (`![alt](url)`, a linked badge keeps
/// its link) and HTML `<img>` tags (by their `alt` attribute). Fenced code
/// blocks and code spans are left alone.
pub fn markdown_without_images(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut fence: Option<String> = None;
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some(marker) = &fence {
            if trimmed.starts_with(marker.as_str()) {
                fence = None;
            }
            out.push_str(line);
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = Some(trimmed[..3].to_owned());
            out.push_str(line);
            continue;
        }
        out.push_str(&line_without_images(line));
    }
    out
}

fn line_without_images(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(ix) = rest.find(['!', '<', '`']) {
        out.push_str(&rest[..ix]);
        let tail = &rest[ix..];
        if tail.starts_with('`') {
            // A code span: copied as is up to its closing backtick.
            let ticks = tail.len() - tail.trim_start_matches('`').len();
            let after = &tail[ticks..];
            match after.find(&tail[..ticks]) {
                Some(end) => {
                    let len = ticks + end + ticks;
                    out.push_str(&tail[..len]);
                    rest = &tail[len..];
                }
                None => {
                    out.push_str(&tail[..ticks]);
                    rest = after;
                }
            }
            continue;
        }
        if let Some((alt, len)) = markdown_image(tail).or_else(|| html_image(tail)) {
            out.push_str(&alt);
            rest = &tail[len..];
            continue;
        }
        out.push_str(&tail[..1]);
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

/// `![alt](url "title")` at the start of `text`: its alt text and length.
fn markdown_image(text: &str) -> Option<(String, usize)> {
    let inner = text.strip_prefix("![")?;
    let mut depth = 0usize;
    let mut alt_end = None;
    for (ix, c) in inner.char_indices() {
        match c {
            '[' => depth += 1,
            ']' if depth == 0 => {
                alt_end = Some(ix);
                break;
            }
            ']' => depth -= 1,
            '\n' => return None,
            _ => {}
        }
    }
    let alt_end = alt_end?;
    let after = inner[alt_end + 1..].strip_prefix('(')?;
    let mut depth = 0usize;
    let close = after.char_indices().find_map(|(ix, c)| match c {
        '(' => {
            depth += 1;
            None
        }
        ')' if depth == 0 => Some(ix),
        ')' => {
            depth -= 1;
            None
        }
        _ => None,
    })?;
    let len = 2 + alt_end + 1 + 1 + close + 1;
    Some((inner[..alt_end].to_owned(), len))
}

/// An HTML `<img …>` tag at the start of `text`: its `alt` and length.
fn html_image(text: &str) -> Option<(String, usize)> {
    let lower = text.get(..4)?.to_ascii_lowercase();
    if lower != "<img"
        || !text[4..].starts_with(|c: char| c.is_whitespace() || c == '/' || c == '>')
    {
        return None;
    }
    let end = text.find('>')?;
    let tag = &text[..end];
    let alt = attribute(tag, "alt").unwrap_or_default();
    Some((alt, end + 1))
}

/// The value of `name="…"` (or single-quoted) in an HTML tag.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(ix) = lower[from..].find(name) {
        let start = from + ix;
        from = start + name.len();
        let before_ok = lower[..start].ends_with(char::is_whitespace);
        let rest = lower[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let value_at = tag.len() - rest.len() + 1;
        let value = tag[value_at..].trim_start();
        let quote = value.chars().next()?;
        if quote == '"' || quote == '\'' {
            let body = &value[1..];
            return body.find(quote).map(|end| body[..end].to_owned());
        }
        let end = value.find(char::is_whitespace).unwrap_or(value.len());
        return Some(value[..end].trim_end_matches('/').to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_urls_only() {
        assert!(is_remote("https://example.com/icon.svg"));
        assert!(is_remote("HTTP://example.com/a.png"));
        assert!(!is_remote("file:///etc/passwd"));
        assert!(!is_remote("docs/img.png"));
        assert!(!is_remote("data:image/png;base64,AAAA"));
        assert!(!is_remote("https://"));
    }

    #[test]
    fn cache_path_is_a_hash_of_the_url() {
        let dir = Path::new("/cache");
        let a = cache_path(dir, "https://a/icon.png");
        assert_eq!(a, cache_path(dir, "https://a/icon.png"));
        assert_ne!(a, cache_path(dir, "https://b/icon.png"));
        assert_eq!(a.file_name().unwrap().len(), 64);
        assert!(a.starts_with(dir));
    }

    #[test]
    fn sniffs_image_formats() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nrest"), Some(ImageKind::Png));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(ImageKind::Jpeg));
        assert_eq!(sniff(b"GIF89a..."), Some(ImageKind::Gif));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(ImageKind::WebP));
        assert_eq!(
            sniff(b"<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
            Some(ImageKind::Svg)
        );
        assert_eq!(
            sniff("\u{feff}  <SVG></SVG>".as_bytes()),
            Some(ImageKind::Svg)
        );
        assert_eq!(sniff(b"<!doctype html><html>404</html>"), None);
        assert_eq!(sniff(b"{\"error\": 1}"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn resolves_redirects() {
        let base: http::Uri = "https://a.com/x/y/icon.png".parse().unwrap();
        assert_eq!(
            resolve(&base, "https://b.com/i.png").as_deref(),
            Some("https://b.com/i.png")
        );
        assert_eq!(
            resolve(&base, "//c.com/i.png").as_deref(),
            Some("https://c.com/i.png")
        );
        assert_eq!(
            resolve(&base, "/root.png").as_deref(),
            Some("https://a.com/root.png")
        );
        assert_eq!(
            resolve(&base, "other.png").as_deref(),
            Some("https://a.com/x/y/other.png")
        );
    }

    #[test]
    fn redirects_never_downgrade_to_http() {
        assert!(matches!(
            redirect_target("https://a.com/i.png", "http://b.com/i.png"),
            Err(ImageError::InsecureRedirect(_))
        ));
        assert!(matches!(
            redirect_target("https://a.com/i.png", "HTTP://a.com/i.png"),
            Err(ImageError::InsecureRedirect(_))
        ));
        assert_eq!(
            redirect_target("https://a.com/x/i.png", "/y.png").unwrap(),
            "https://a.com/y.png"
        );
        assert_eq!(
            redirect_target("http://a.com/i.png", "https://b.com/i.png").unwrap(),
            "https://b.com/i.png"
        );
        assert_eq!(
            redirect_target("http://a.com/i.png", "http://b.com/i.png").unwrap(),
            "http://b.com/i.png"
        );
        // Scheme-relative keeps https.
        assert_eq!(
            redirect_target("https://a.com/i.png", "//b.com/i.png").unwrap(),
            "https://b.com/i.png"
        );
    }

    #[tokio::test]
    async fn local_hosts_are_refused_before_connecting() {
        for (host, proxy) in [
            ("127.0.0.1", false),
            ("::1", false),
            ("169.254.169.254", true),
            ("::ffff:10.0.0.1", false),
            ("localhost", false),
            ("localhost", true),
        ] {
            assert!(
                matches!(
                    resolve_host(host, 80, proxy).await,
                    Err(ImageError::Forbidden { .. })
                ),
                "{host}"
            );
        }
        assert_eq!(
            resolve_host("1.1.1.1", 443, false).await.unwrap(),
            vec!["1.1.1.1:443".parse::<SocketAddr>().unwrap()]
        );
        let dir = tempfile::tempdir().unwrap();
        let fetcher = Fetcher::new(dir.path().to_owned(), 1);
        for url in [
            "http://127.0.0.1:1/icon.svg",
            "http://[::1]/icon.svg",
            "https://user@localhost/icon.svg",
            "http://169.254.169.254/latest/meta-data/",
        ] {
            assert!(
                matches!(fetcher.get(url).await, Err(ImageError::Forbidden { .. })),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn oversized_cached_images_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = Fetcher::new(dir.path().to_owned(), 1);
        // Cached before the size check existed; refetching it from a
        // loopback URL is refused, so nothing comes back.
        let url = "http://127.0.0.1:1/bomb.svg";
        let bomb = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"20000\" height=\"20000\"/>";
        let path = store(dir.path(), url, bomb).unwrap();
        assert!(fetcher.get(url).await.is_err());
        assert!(!path.exists());
        let fine = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"64\" height=\"64\"/>";
        let path = store(dir.path(), url, fine).unwrap();
        assert_eq!(fetcher.get(url).await.unwrap(), path);
    }

    #[tokio::test]
    async fn cached_svg_with_external_references_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = Fetcher::new(dir.path().to_owned(), 1);
        // Cached before the check existed; refetching from loopback is refused.
        let url = "http://127.0.0.1:1/leak.svg";
        let leak = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\">\
                     <image width=\"16\" height=\"16\" href=\"/etc/passwd\"/></svg>";
        let path = store(dir.path(), url, leak).unwrap();
        assert!(fetcher.get(url).await.is_err());
        assert!(!path.exists());
    }

    #[test]
    fn validate_refuses_external_svg_references() {
        assert!(matches!(
            validate(
                b"<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
                  width=\"16\" height=\"16\"><image xlink:href=\"file:///etc/passwd\"/></svg>"
            ),
            Err(ImageError::ExternalReference(_))
        ));
        assert!(
            validate(
                b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\">\
                  <defs><path id=\"p\" d=\"M0 0h1v1z\"/></defs><use href=\"#p\"/>\
                  <a href=\"https://example.com\"><use href=\"#p\"/></a></svg>"
            )
            .is_ok()
        );
    }

    #[test]
    fn validates_kind_and_size() {
        assert!(matches!(
            validate(b"<html>404</html>"),
            Err(ImageError::NotAnImage)
        ));
        assert!(matches!(
            validate(b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"5000\" height=\"1\"/>"),
            Err(ImageError::TooManyPixels { .. })
        ));
        assert!(
            validate(b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\"/>").is_ok()
        );
    }

    #[test]
    fn stores_atomically_in_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("images");
        let fetcher = Fetcher::new(cache.clone(), 2);
        let url = "https://example.com/icon.png";
        assert_eq!(fetcher.cached(url), None);
        let path = store(&cache, url, b"\x89PNG\r\n\x1a\n").unwrap();
        assert_eq!(fetcher.cached(url), Some(path.clone()));
        assert_eq!(fs::read(path).unwrap(), b"\x89PNG\r\n\x1a\n");
        // Only the image itself is left, no temporary files.
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn refuses_non_http_urls_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = Fetcher::new(dir.path().to_owned(), 1);
        assert!(matches!(
            fetcher.get("file:///etc/hosts").await,
            Err(ImageError::InvalidUrl(_))
        ));
    }

    #[test]
    fn markdown_images_become_alt_text() {
        let text = "# Title\n\
                    [![Build](https://ci/badge.svg)](https://ci) text\n\
                    ![Logo](https://x/logo.png \"The logo\") and ![](rel.png)\n";
        assert_eq!(
            markdown_without_images(text),
            "# Title\n[Build](https://ci) text\nLogo and \n"
        );
    }

    #[test]
    fn html_images_become_alt_text() {
        let text = "<p align=\"center\"><img src=\"https://x/a.png\" alt='Chart logo' width=100/></p>\n\
                    <IMG SRC=x.png>";
        assert_eq!(
            markdown_without_images(text),
            "<p align=\"center\">Chart logo</p>\n"
        );
    }

    #[test]
    fn code_is_left_alone() {
        let text = "Use `![x](y)` here\n```md\n![a](b)\n<img src=c>\n```\n![d](e)\n";
        assert_eq!(
            markdown_without_images(text),
            "Use `![x](y)` here\n```md\n![a](b)\n<img src=c>\n```\nd\n"
        );
    }

    #[test]
    fn not_images_stay() {
        let text = "Wow! <b>bold</b> <imgx> ![broken](no-close\n";
        assert_eq!(markdown_without_images(text), text);
    }
}
