//! Background tile fetching (`docs/ovimap-link.md` §9).
//!
//! The canvas used to be cache-only: a tile not already on disk drew blank
//! until some external downloader seeded the cache. This module fetches
//! missing tiles in the background, so the basemap fills in on its own:
//!
//! * **One client**: `ureq` + rustls, the workspace's only HTTP/TLS dependency.
//!   Wasm has no fetcher (no sockets, no threads): [`TileFetcher::request`] is
//!   a no-op there and [`fetch_tile_cached`] returns the disk cache only.
//! * **Policy**: the request carries the app's real User-Agent
//!   ([`USER_AGENT`], never a browser UA), `Accept: image/*`, and a per-request
//!   timeout. Requests are cache-first (a fresh tile is never re-fetched), at
//!   most 4 concurrent (4 worker threads), and spaced at least
//!   [`FetchOptions::min_gap`] apart, so panning is not a bulk download.
//!   The zoom cap is enforced before any request: the OSM default never asks
//!   for z > 19 (§9.1).
//! * **Cache**: [`cadcraft_io::geojson`]'s disk cache is reused verbatim
//!   (7-day TTL, 500 MB LRU, atomic temp-file + rename writes). A failed fetch
//!   falls back to a stale cached tile; a tile with no cache at all stays a
//!   blank placeholder (§9.4) — a fetch failure never reaches the UI.
//! * **Proxy**: the agent is built from ureq's default config, which reads
//!   `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` (and `NO_PROXY`) from the
//!   environment — launching the app with a proxy set routes tile requests
//!   through it. A macOS system proxy set in System Settings is *not* picked
//!   up (reading it needs platform APIs this crate cannot call); export the
//!   proxy variables when launching the app instead.
//! * **Fallback**: when the primary source fails (any failure: timeout,
//!   connection error, 4xx, non-image body), the layer's fallback source —
//!   CARTO Voyager for the OSM default, OSM data on a CDN — is tried with
//!   the same policy, under its own cache directory. Only when both fail
//!   does the tile stay a blank placeholder.
//! * **Retries**: at most [`FetchOptions::max_attempts`] attempts per tile,
//!   exponential back-off between attempts. 4xx (other than 429), a
//!   non-image content type and an oversize body are permanent: no retry.
//!
//! Failures are `Err(String)` here and `None` at the cache boundary; nothing
//! in this module panics, blocks the UI thread (the workers sleep, the canvas
//! only enqueues), or retries forever.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(not(target_arch = "wasm32"))]
use cadcraft_doc::gis::{DEFAULT_TILE_MAX_ZOOM, DEFAULT_TILE_URL, tile_cache_rel_path};
use cadcraft_doc::gis::{TileLayer, TileRef};

/// The User-Agent every tile (and bridge) request carries: the app's real
/// identity, never a browser UA (OSM Tile Usage Policy, §9.1).
pub const USER_AGENT: &str = concat!("CadCraft/", env!("CARGO_PKG_VERSION"), " (https://github.com/storytold/cadcraft)");

/// Most bytes one fetched tile may hold (matches the cache cap: a tile PNG is
/// far smaller; this caps abuse from a hostile server).
#[cfg(not(target_arch = "wasm32"))]
const MAX_TILE_BYTES: usize = 1 << 20;
/// Tile cache freshness (§9.4: 7 days).
#[cfg(not(target_arch = "wasm32"))]
const CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// Worker threads: the concurrency cap (§9.1, ≤ 4).
const WORKERS: usize = 4;
/// Most tiles waiting in the fetch queue. A pan enqueues a whole new view's
/// tiles every frame; without a bound, a few pans bury the current view's
/// tiles behind hundreds of stale ones (FIFO) and the map stops following
/// the pan for minutes. Past the cap the oldest queued tiles are dropped —
/// safe, because the canvas re-requests the tiles it still needs every
/// frame: a dropped tile for an abandoned view never comes back, a dropped
/// tile for the current view is re-queued at once. Dropped keys are
/// removed from `in_flight` too, or the dedup would block the re-request
/// forever (a permanent blank).
const MAX_QUEUE: usize = 512;

/// Fetch tuning. The defaults are the production policy; tests pass small
/// timeouts/back-offs so a stub server's failures stay fast.
#[derive(Clone, Copy, Debug)]
pub struct FetchOptions {
    /// Per-request timeout (connect + body).
    pub timeout: Duration,
    /// Attempts per tile, including the first (≤ 3 in production, §9.4).
    pub max_attempts: u32,
    /// First back-off delay; doubles per retry, capped at 30 s.
    pub backoff_base: Duration,
    /// Minimum gap between the start of two requests (rate limit).
    pub min_gap: Duration,
}

impl Default for FetchOptions {
    fn default() -> Self {
        FetchOptions { timeout: Duration::from_secs(15), max_attempts: 3, backoff_base: Duration::from_secs(1), min_gap: Duration::from_millis(100) }
    }
}

fn poisoned<'a, T>(
    guard: Result<std::sync::MutexGuard<'a, T>, std::sync::PoisonError<std::sync::MutexGuard<'a, T>>>,
) -> std::sync::MutexGuard<'a, T> {
    guard.unwrap_or_else(|e| e.into_inner())
}

/// Whether the disk cache holds a fresh copy of this tile (metadata only: no
/// bytes are read).
pub fn is_cache_fresh(layer: &TileLayer, tile: &TileRef) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (layer, tile);
        false
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(path) = cadcraft_io::geojson::tile_cache_path(layer, tile) else { return false };
        let Ok(meta) = std::fs::metadata(&path) else { return false };
        meta.modified().ok().and_then(|m| m.elapsed().ok()).is_some_and(|age| age <= CACHE_TTL)
    }
}

/// Fetch one tile's bytes from its URL. No cache is consulted or written;
/// callers that want the cache use [`fetch_tile_cached`]. Permanent failures
/// (4xx other than 429, non-image content type, oversize body) return
/// immediately; transient ones are retried with exponential back-off.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_tile_bytes(layer: &TileLayer, tile: &TileRef, opts: FetchOptions) -> Result<Vec<u8>, String> {
    if !layer.enabled {
        return Err("瓦片图层已关闭".into());
    }
    if tile.z > layer.max_zoom {
        return Err(format!("瓦片级别 z={} 超出图层上限 {}", tile.z, layer.max_zoom));
    }
    if layer.url_template.trim() == DEFAULT_TILE_URL && tile.z > DEFAULT_TILE_MAX_ZOOM {
        return Err(format!("默认 OSM 瓦片最高级别为 {DEFAULT_TILE_MAX_ZOOM}，不得请求 z={}", tile.z));
    }
    layer.validate().map_err(|e| format!("瓦片图层无效：{e}"))?;
    let url = layer.tile_url(tile.z, tile.x, tile.y);
    let agent = agent(opts);
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match fetch_once(&agent, &url) {
            Ok(bytes) => return Ok(bytes),
            Err(e) => {
                if attempt >= opts.max_attempts.max(1) || !e.retryable {
                    return Err(e.message);
                }
                let shift = attempt.saturating_sub(1).min(5);
                let delay = opts.backoff_base.saturating_mul(1 << shift).min(Duration::from_secs(30));
                std::thread::sleep(delay);
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub fn fetch_tile_bytes(_layer: &TileLayer, _tile: &TileRef, _opts: FetchOptions) -> Result<Vec<u8>, String> {
    Err("网页版不支持联网抓取瓦片".into())
}

struct FetchFailure {
    message: String,
    retryable: bool,
}

#[cfg(not(target_arch = "wasm32"))]
fn agent(opts: FetchOptions) -> ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    // The agent is shared (connection reuse); the timeout is per-request
    // policy, so the first caller wins — all callers use the default.
    AGENT.get_or_init(|| ureq::Agent::config_builder().timeout_global(Some(opts.timeout)).build().into()).clone()
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch_once(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, FetchFailure> {
    let mut resp = agent.get(url).header("User-Agent", USER_AGENT).header("Accept", "image/*").call().map_err(|e| match &e {
        ureq::Error::StatusCode(code) => {
            FetchFailure { message: format!("瓦片请求失败：HTTP {code}（{url}）"), retryable: *code == 429 || (500..600).contains(code) }
        }
        ureq::Error::Timeout(_) => FetchFailure { message: format!("瓦片请求超时（{url}）"), retryable: true },
        ureq::Error::ConnectionFailed => FetchFailure { message: format!("瓦片连接失败（{url}）"), retryable: true },
        ureq::Error::Io(_) => FetchFailure { message: format!("瓦片读取失败（{url}）"), retryable: true },
        other => FetchFailure { message: format!("瓦片请求失败：{other}（{url}）"), retryable: false },
    })?;
    if let Some(ct) = resp.headers().get("content-type").and_then(|v| v.to_str().ok()) {
        let ct = ct.trim().to_ascii_lowercase();
        if !ct.is_empty() && !ct.starts_with("image/") {
            return Err(FetchFailure { message: format!("瓦片响应不是图片（Content-Type: {ct}）"), retryable: false });
        }
    }
    let bytes = resp.body_mut().read_to_vec().map_err(|e| FetchFailure { message: format!("瓦片读取失败：{e}"), retryable: true })?;
    if bytes.is_empty() {
        return Err(FetchFailure { message: "瓦片响应为空".into(), retryable: true });
    }
    if bytes.len() > MAX_TILE_BYTES {
        return Err(FetchFailure { message: format!("瓦片超过 1 MB 上限（{} 字节），未缓存", bytes.len()), retryable: false });
    }
    Ok(bytes)
}

/// The cache-first fetch: a fresh cached tile wins; otherwise fetch, store,
/// and on failure fall back to a stale cached tile. `None` only when there is
/// no tile to show (cache miss + fetch failure) — the caller draws a blank
/// placeholder (§9.4). Never fails, never panics.
///
/// Primary and fallback are tried in order, each cache-first under its own
/// cache directory: a fallback tile is stored under the fallback source, so
/// the two sources never overwrite each other. Stale fallbacks cover the
/// same way: a stale primary tile wins over a fresh fallback fetch (same
/// style as the rest of the view), and a stale fallback tile is the last
/// resort before blank.
pub fn fetch_tile_cached(layer: &TileLayer, tile: &TileRef, opts: FetchOptions) -> Option<Vec<u8>> {
    if let Some(bytes) = fetch_one_cached(layer, tile, opts) {
        return Some(bytes);
    }
    if let Some(fb) = layer.fallback_layer()
        && let Some(bytes) = fetch_one_cached(&fb, tile, opts)
    {
        return Some(bytes);
    }
    crate::tilemap::load_cached_tile_with_fallback(layer, tile)
}

/// One source's cache-first fetch: fresh cache, else fetch + store, else `None`
/// (the caller tries the next source or the stale cache).
fn fetch_one_cached(layer: &TileLayer, tile: &TileRef, opts: FetchOptions) -> Option<Vec<u8>> {
    if let Some(bytes) = cadcraft_io::geojson::tile_cache_load(layer, tile) {
        return Some(bytes);
    }
    match fetch_tile_bytes(layer, tile, opts) {
        Ok(bytes) => {
            let _ = cadcraft_io::geojson::tile_cache_store(layer, tile, &bytes);
            Some(bytes)
        }
        Err(_) => None,
    }
}

struct Shared {
    queue: Mutex<VecDeque<(TileLayer, TileRef)>>,
    notify: Condvar,
    in_flight: Mutex<HashSet<String>>,
    /// Start time of the last request (rate limiting).
    last_start: Mutex<Instant>,
    /// Set when a worker finished a tile: the canvas repaints to show it.
    dirty: AtomicBool,
    opts: FetchOptions,
}

/// The background tile fetcher: the canvas enqueues the tiles its views need
/// ([`TileFetcher::request`]); up to 4 workers fetch them into the disk cache
/// and the canvas repaints when one lands. Enqueueing never blocks and never
/// fetches on the calling thread.
pub struct TileFetcher {
    inner: Arc<Shared>,
}

impl TileFetcher {
    /// A fetcher with `workers` background threads (clamped to 1..=4, §9.1).
    /// A spawn failure degrades to no fetching (the canvas keeps its cache),
    /// never an error.
    pub fn new(workers: usize, opts: FetchOptions) -> Self {
        let inner = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            notify: Condvar::new(),
            in_flight: Mutex::new(HashSet::new()),
            last_start: Mutex::new(Instant::now() - opts.min_gap),
            dirty: AtomicBool::new(false),
            opts,
        });
        #[cfg(not(target_arch = "wasm32"))]
        for i in 0..workers.clamp(1, WORKERS) {
            let shared = Arc::clone(&inner);
            let _ = std::thread::Builder::new().name(format!("cadcraft-tilefetch-{i}")).spawn(move || worker(&shared));
        }
        TileFetcher { inner }
    }

    /// The process-wide fetcher (4 workers, default policy), created once.
    pub fn global() -> &'static TileFetcher {
        static GLOBAL: OnceLock<TileFetcher> = OnceLock::new();
        GLOBAL.get_or_init(|| TileFetcher::new(WORKERS, FetchOptions::default()))
    }

    /// Enqueue a tile for background fetch: a no-op when the layer is off, the
    /// cache is fresh, or the tile is already queued/in flight. Never blocks,
    /// never fetches on the calling thread. Past [`MAX_QUEUE`] queued tiles
    /// the oldest are dropped (they belong to views the user already left;
    /// the canvas re-requests the ones it still needs).
    pub fn request(&self, layer: &TileLayer, tile: &TileRef) {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (layer, tile);
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if !layer.enabled || tile.z > layer.max_zoom || is_cache_fresh(layer, tile) {
                return;
            }
            let key = tile_cache_rel_path(layer, tile);
            {
                let mut in_flight = poisoned(self.inner.in_flight.lock());
                if !in_flight.insert(key) {
                    return;
                }
            }
            {
                let mut queue = poisoned(self.inner.queue.lock());
                queue.push_back((layer.clone(), tile.clone()));
                // Drop stale queued tiles past the cap (oldest first: they
                // belong to abandoned views), freeing their dedup keys so a
                // re-request for a tile still needed can queue it again.
                while queue.len() > MAX_QUEUE {
                    if let Some((old_layer, old_tile)) = queue.pop_front() {
                        let old_key = tile_cache_rel_path(&old_layer, &old_tile);
                        poisoned(self.inner.in_flight.lock()).remove(&old_key);
                    } else {
                        break;
                    }
                }
            }
            self.inner.notify.notify_one();
        }
    }

    /// Whether this tile is queued or being fetched (the canvas uses this
    /// to tell "fetch in flight, bytes coming" apart from "fetch failed":
    /// an in-flight tile is skipped without back-off so it decodes promptly
    /// when its bytes land).
    pub fn is_pending(&self, layer: &TileLayer, tile: &TileRef) -> bool {
        poisoned(self.inner.in_flight.lock()).contains(&tile_cache_rel_path(layer, tile))
    }

    /// Whether a worker finished a tile since the last call (the canvas
    /// repaints once when this is true).
    pub fn take_dirty(&self) -> bool {
        self.inner.dirty.swap(false, Ordering::SeqCst)
    }

    /// Tiles queued or in flight (status reporting).
    pub fn pending(&self) -> usize {
        poisoned(self.inner.queue.lock()).len() + poisoned(self.inner.in_flight.lock()).len()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn worker(shared: &Arc<Shared>) {
    loop {
        let (layer, tile) = {
            let mut queue = poisoned(shared.queue.lock());
            while queue.is_empty() {
                queue = poisoned(shared.notify.wait(queue));
            }
            match queue.pop_front() {
                Some(job) => job,
                None => continue,
            }
        };
        // Rate limit: space request starts by at least `min_gap`.
        let wait = {
            let mut last = poisoned(shared.last_start.lock());
            let now = Instant::now();
            let elapsed = now.duration_since(*last);
            let wait = if elapsed < shared.opts.min_gap { shared.opts.min_gap - elapsed } else { Duration::ZERO };
            if wait.is_zero() {
                *last = now;
            }
            wait
        };
        if !wait.is_zero() {
            std::thread::sleep(wait);
            *poisoned(shared.last_start.lock()) = Instant::now();
        }
        let _ = fetch_tile_cached(&layer, &tile, shared.opts);
        {
            let mut in_flight = poisoned(shared.in_flight.lock());
            in_flight.remove(&tile_cache_rel_path(&layer, &tile));
        }
        shared.dirty.store(true, Ordering::SeqCst);
    }
}
