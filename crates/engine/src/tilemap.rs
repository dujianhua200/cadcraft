//! Basemap tile cache reads (`docs/ovimap-link.md` §9.4).
//!
//! Tiles are fetched in the background by [`crate::tilefetch::TileFetcher`]
//! and land in this disk cache; this module only reads. The cache layout
//! matches `cadcraft-io`'s tile cache (`$CADCRAFT_TILE_CACHE`, else
//! `$HOME/.cache/cadcraft/tiles`), keyed by
//! [`cadcraft_doc::gis::tile_cache_rel_path`], so both paths see the same files.

use std::path::PathBuf;

use cadcraft_doc::gis::{DEFAULT_TILE_ATTRIBUTION, DEFAULT_TILE_URL, TileLayer, TileRef, tile_cache_rel_path};

/// Most bytes one cached tile may hold (mirrors the io cache cap: a tile PNG
/// is far smaller; this caps abuse from a poisoned cache).
const MAX_TILE_BYTES: usize = 1 << 20;

/// The tile cache root: `$CADCRAFT_TILE_CACHE` when set to a non-empty value
/// (test support), else `$HOME/.cache/cadcraft/tiles` (or the temp dir when no
/// home is known). `None` on wasm, where there is no file system.
pub fn cache_root() -> Option<PathBuf> {
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(dir) = std::env::var("CADCRAFT_TILE_CACHE").ok().filter(|d| !d.trim().is_empty()) {
            return Some(PathBuf::from(dir));
        }
        let home = std::env::var("HOME").ok().filter(|h| !h.is_empty()).or_else(|| std::env::var("USERPROFILE").ok().filter(|h| !h.is_empty()));
        Some(match home {
            Some(h) => PathBuf::from(h).join(".cache").join("cadcraft").join("tiles"),
            None => std::env::temp_dir().join("cadcraft-tiles"),
        })
    }
}

/// The cache file of one tile, if the platform has a file system.
pub fn cache_path(layer: &TileLayer, tile: &TileRef) -> Option<PathBuf> {
    cache_root().map(|root| root.join(tile_cache_rel_path(layer, tile)))
}

/// A cached tile's bytes, when present and within the size cap. Freshness is
/// the cache writer's job (7-day TTL, §9.4); a stale file still reads — the
/// canvas would rather show a slightly old tile than a blank one, and a
/// missing file is `None` (blank placeholder). Never fails, never fetches.
pub fn load_cached_tile(layer: &TileLayer, tile: &TileRef) -> Option<Vec<u8>> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (layer, tile);
        None
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = cache_path(layer, tile)?;
        let bytes = std::fs::read(&path).ok()?;
        if bytes.is_empty() || bytes.len() > MAX_TILE_BYTES { None } else { Some(bytes) }
    }
}

/// The drawing's effective tile layer: its stored `tilemap`, else the OSM
/// default (§9.1). The default is enabled — a drawing with a georef shows the
/// OSM basemap as soon as its cache has tiles; without a georef tiles have no
/// alignment and the canvas draws none.
pub fn effective_tilemap(doc: &cadcraft_doc::Drawing) -> TileLayer {
    doc.tilemap.clone().unwrap_or_else(TileLayer::default_osm)
}

/// Whether the layer is the OSM default (its attribution is the built-in one).
pub fn is_default_osm(layer: &TileLayer) -> bool {
    layer.url_template.trim() == DEFAULT_TILE_URL && layer.attribution == DEFAULT_TILE_ATTRIBUTION
}
