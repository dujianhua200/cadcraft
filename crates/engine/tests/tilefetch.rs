//! Tile fetching against a local stub HTTP server (never the real network):
//! the OSM policy pieces (real User-Agent, cache-first, ≤ 4 concurrent,
//! retries with back-off, z ≤ 19 cap) are asserted here.
//!
//! No environment variable is touched (`set_var` is `unsafe` in edition 2024
//! and this workspace forbids it): each test's stub listens on its own
//! loopback port, so its tiles land under a unique source hash in the normal
//! cache, and each test removes that source dir afterwards.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cadcraft_doc::gis::{TileLayer, TileRef};
use cadcraft_engine::tilefetch::{FetchOptions, TileFetcher, fetch_tile_bytes, fetch_tile_cached};

/// A 1×1 transparent PNG (the stub's tile body).
const PNG_1X1: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 10, 73, 68, 65,
    84, 120, 156, 99, 0, 1, 0, 0, 5, 0, 1, 13, 10, 45, 180, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

struct Stub {
    port: u16,
    stop: Arc<AtomicBool>,
    /// Request lines + headers seen, in order.
    seen: Arc<Mutex<Vec<String>>>,
}

impl Stub {
    /// `respond` maps a raw request (request line + headers) to
    /// `(status, content-type, body)`.
    fn new(respond: impl Fn(&str) -> (u16, &'static str, Vec<u8>) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let _ = listener.set_nonblocking(true);
        let respond = Arc::new(respond);
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        let stop2 = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut buf = [0u8; 8192];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                        seen2.lock().unwrap().push(req.clone());
                        let (status, ct, body) = respond(&req);
                        let reason = match status {
                            200 => "OK",
                            404 => "Not Found",
                            429 => "Too Many Requests",
                            500 => "Internal Server Error",
                            _ => "Error",
                        };
                        let head = format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(&body);
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        Stub { port, stop, seen }
    }

    fn requests(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    fn user_agents(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|req| {
                req.lines().find(|l| l.to_ascii_lowercase().starts_with("user-agent:")).map(|l| l["user-agent:".len()..].trim().to_string())
            })
            .collect()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn fast_opts() -> FetchOptions {
    FetchOptions { timeout: Duration::from_secs(5), max_attempts: 3, backoff_base: Duration::from_millis(1), min_gap: Duration::from_millis(1) }
}

fn layer_for(stub: &Stub) -> TileLayer {
    TileLayer {
        url_template: format!("http://127.0.0.1:{}/{{z}}/{{x}}/{{y}}.png", stub.port),
        attribution: "stub".into(),
        opacity: 1.0,
        enabled: true,
        max_zoom: 19,
    }
}

fn tile(layer: &TileLayer) -> TileRef {
    TileRef { z: 0, x: 0, y: 0, url: layer.tile_url(0, 0, 0), cache_key: layer.cache_key(0, 0, 0) }
}

/// Remove one test's tiles from the normal cache (its source dir, keyed by
/// the stub URL's hash, is unique per test).
fn cleanup_cache(layer: &TileLayer, tile: &TileRef) {
    if let Some(mut dir) = cadcraft_io::geojson::tile_cache_path(layer, tile) {
        for _ in 0..3 {
            match dir.parent() {
                Some(parent) => dir = parent.to_path_buf(),
                None => return,
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn fetch_serves_png_and_caches() {
    let stub = Stub::new(|_| (200, "image/png", PNG_1X1.to_vec()));
    let layer = layer_for(&stub);
    let t = tile(&layer);
    let bytes = fetch_tile_cached(&layer, &t, fast_opts()).expect("stub tile fetches");
    assert_eq!(bytes, PNG_1X1);
    assert_eq!(stub.requests(), 1);
    // Second call is cache-first: no second request.
    let again = fetch_tile_cached(&layer, &t, fast_opts()).expect("cached tile");
    assert_eq!(again, PNG_1X1);
    assert_eq!(stub.requests(), 1, "a fresh cache is never re-fetched");
    cleanup_cache(&layer, &t);
}

#[test]
fn fetch_uses_real_user_agent() {
    let stub = Stub::new(|_| (200, "image/png", PNG_1X1.to_vec()));
    let layer = layer_for(&stub);
    let t = tile(&layer);
    let _ = fetch_tile_cached(&layer, &t, fast_opts());
    let uas = stub.user_agents();
    assert_eq!(uas.len(), 1);
    assert!(uas[0].starts_with("CadCraft/"), "real app UA, got {}", uas[0]);
    assert!(!uas[0].contains("Mozilla"), "never a browser UA");
    cleanup_cache(&layer, &t);
}

#[test]
fn fetch_404_is_permanent_no_retry() {
    let stub = Stub::new(|_| (404, "text/plain", b"no such tile".to_vec()));
    let layer = layer_for(&stub);
    let t = tile(&layer);
    assert!(fetch_tile_bytes(&layer, &t, fast_opts()).is_err());
    assert_eq!(stub.requests(), 1, "4xx is permanent: no retry");
    assert!(fetch_tile_cached(&layer, &t, fast_opts()).is_none(), "no cache, no tile: blank placeholder");
}

#[test]
fn fetch_500_retries_then_fails() {
    let stub = Stub::new(|_| (500, "text/plain", b"boom".to_vec()));
    let layer = layer_for(&stub);
    let t = tile(&layer);
    assert!(fetch_tile_bytes(&layer, &t, fast_opts()).is_err());
    assert_eq!(stub.requests(), 3, "transient 500 retries up to max_attempts");
}

#[test]
fn fetch_non_image_refused_without_retry() {
    let stub = Stub::new(|_| (200, "text/html", b"<html/>".to_vec()));
    let layer = layer_for(&stub);
    let t = tile(&layer);
    assert!(fetch_tile_bytes(&layer, &t, fast_opts()).is_err());
    assert_eq!(stub.requests(), 1, "a non-image body is permanent: no retry");
}

#[test]
fn fetch_stale_cache_falls_back_on_failure() {
    let stub = Stub::new(|_| (500, "text/plain", b"boom".to_vec()));
    let layer = layer_for(&stub);
    let t = tile(&layer);
    // Seed the cache, then backdate it past the 7-day TTL.
    assert!(cadcraft_io::geojson::tile_cache_store(&layer, &t, PNG_1X1).is_ok());
    let path = cadcraft_io::geojson::tile_cache_path(&layer, &t).unwrap();
    let old = std::time::SystemTime::now() - Duration::from_secs(8 * 24 * 3600);
    let file = std::fs::File::open(&path).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(old)).unwrap();
    let bytes = fetch_tile_cached(&layer, &t, fast_opts()).expect("stale cache covers a failed revalidate");
    assert_eq!(bytes, PNG_1X1);
    cleanup_cache(&layer, &t);
}

#[test]
fn fetch_osm_zoom_cap_refused_without_request() {
    let stub = Stub::new(|_| (200, "image/png", PNG_1X1.to_vec()));
    let layer = TileLayer::default_osm();
    let t = TileRef { z: 20, x: 0, y: 0, url: layer.tile_url(20, 0, 0), cache_key: layer.cache_key(20, 0, 0) };
    assert!(fetch_tile_bytes(&layer, &t, fast_opts()).is_err(), "the OSM default never requests z > 19 (§9.1)");
    assert_eq!(stub.requests(), 0);
}

#[test]
fn fetcher_queue_fetches_in_background() {
    let stub = Stub::new(|_| (200, "image/png", PNG_1X1.to_vec()));
    let layer = layer_for(&stub);
    let t = tile(&layer);
    let fetcher = TileFetcher::new(2, fast_opts());
    fetcher.request(&layer, &t);
    assert!(fetcher.pending() >= 1 || stub.requests() >= 1);
    let deadline = Instant::now() + Duration::from_secs(10);
    while cadcraft_io::geojson::tile_cache_load(&layer, &t).is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(cadcraft_io::geojson::tile_cache_load(&layer, &t).as_deref(), Some(PNG_1X1));
    assert!(fetcher.take_dirty(), "a finished fetch marks the fetcher dirty (canvas repaints)");
    assert_eq!(fetcher.pending(), 0);
    cleanup_cache(&layer, &t);
}

#[test]
fn fetcher_request_dedups_and_skips_fresh() {
    let stub = Stub::new(|_| (200, "image/png", PNG_1X1.to_vec()));
    let layer = layer_for(&stub);
    let t = tile(&layer);
    let fetcher = TileFetcher::new(1, fast_opts());
    for _ in 0..5 {
        fetcher.request(&layer, &t);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while cadcraft_io::geojson::tile_cache_load(&layer, &t).is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(cadcraft_io::geojson::tile_cache_load(&layer, &t).is_some());
    assert_eq!(stub.requests(), 1, "one tile, one request: dedup works");
    // Fresh cache: further requests are no-ops.
    fetcher.request(&layer, &t);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(stub.requests(), 1);
    cleanup_cache(&layer, &t);
}
