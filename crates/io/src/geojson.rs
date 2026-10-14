//! OviMap link file helpers (`docs/ovimap-link.md`).
//!
//! The GeoJSON conversion itself lives in [`cadcraft_doc::gis`] (pure data mapping, no
//! I/O). This module adds what needs the file system: the tile disk cache (§9.4) and
//! the pre-flight validation for a link DXF export (§4.3, R2).
//!
//! There is no HTTP client here: the engine fetches tiles into this cache
//! ([`cadcraft_engine::tilefetch`], R3 resolved). A cache miss is never an
//! error: the caller draws a blank placeholder (§9.4).

use std::collections::BTreeMap;
use std::path::PathBuf;

use cadcraft_doc::Drawing;
pub use cadcraft_doc::gis::{
    GeoRef, OvimapExt, TileLayer, TileRef, entities_to_geojson, geojson_to_entities, lonlat_to_tile, new_uuid_v4, parse_ovimap_xdata,
    rebuild_ovimap_ext, tiles_for_view,
};

/// Most bytes one cached tile may hold (a tile PNG is far smaller; this caps abuse).
const MAX_TILE_BYTES: usize = 1 << 20;
/// Tile cache capacity (§9.4: 500 MB LRU).
const MAX_CACHE_BYTES: u64 = 500 << 20;
/// Tile cache freshness (§9.4: 7 days; stale tiles are revalidated by the host).
const CACHE_TTL_SECS: u64 = 7 * 24 * 3600;

/// The tile cache root: `$CADCRAFT_TILE_CACHE` when set to a non-empty value
/// (test support: points the cache at a scratch dir), else
/// `$HOME/.cache/cadcraft/tiles` (or the temp dir when no home is known).
/// `None` on wasm, where there is no file system.
fn cache_root() -> Option<PathBuf> {
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

/// A stable, non-reversible directory name for one tile source (FNV-1a over the URL
/// template): cache keys are `(source, z, x, y)` (§9.4).
fn source_dir(layer: &TileLayer) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in layer.url_template.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// The cache file for one tile, if the platform has a file system.
pub fn tile_cache_path(layer: &TileLayer, tile: &TileRef) -> Option<PathBuf> {
    cache_root().map(|root| root.join(source_dir(layer)).join(&tile.cache_key))
}

/// A cached tile's bytes, when present and fresh (≤ 7 days). Stale or missing → `None`
/// (the caller draws a placeholder, §9.4). Never fails.
pub fn tile_cache_load(layer: &TileLayer, tile: &TileRef) -> Option<Vec<u8>> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (layer, tile);
        None
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = tile_cache_path(layer, tile)?;
        let meta = std::fs::metadata(&path).ok()?;
        let fresh = meta.modified().ok().and_then(|m| m.elapsed().ok()).is_some_and(|age| age.as_secs() <= CACHE_TTL_SECS);
        if !fresh {
            return None;
        }
        let bytes = std::fs::read(&path).ok()?;
        if bytes.len() > MAX_TILE_BYTES { None } else { Some(bytes) }
    }
}

/// Store a tile's bytes in the cache (atomic: temp file + rename), then evict the
/// oldest files while the cache is over 500 MB (§9.4 LRU). Errors are in Chinese.
/// A tile over 1 MB is refused, never stored.
pub fn tile_cache_store(layer: &TileLayer, tile: &TileRef, bytes: &[u8]) -> Result<(), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (layer, tile, bytes);
        Err("瓦片缓存不可用于网页版".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if bytes.len() > MAX_TILE_BYTES {
            return Err(format!("瓦片 {} 超过 1 MB 上限，未缓存", tile.cache_key));
        }
        let Some(path) = tile_cache_path(layer, tile) else { return Err("无法确定瓦片缓存目录".into()) };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建瓦片缓存目录失败：{e}"))?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes).map_err(|e| format!("写瓦片缓存失败：{e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("写瓦片缓存失败：{e}"))?;
        evict_old();
        Ok(())
    }
}

/// Delete the oldest cached tiles until the cache fits 500 MB (best effort: failures
/// are ignored, the cache still works).
#[cfg(not(target_arch = "wasm32"))]
fn evict_old() {
    let Some(root) = cache_root() else { return };
    let mut files: Vec<(u64, u64, PathBuf)> = Vec::new(); // (mtime secs, bytes, path)
    let mut total: u64 = 0;
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(meta) = entry.metadata() {
                let mtime = meta.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
                total += meta.len();
                files.push((mtime, meta.len(), path));
            }
        }
    }
    if total <= MAX_CACHE_BYTES {
        return;
    }
    files.sort_by_key(|(mtime, _, _)| *mtime);
    for (_, size, path) in files {
        if total <= MAX_CACHE_BYTES {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

/// Pre-flight validation for a link DXF export (R2): every `VERTICES_<lineGroupId>`
/// key must be a legal DXF dictionary key, and every stored vertex must carry an
/// `ovimap_id`. The DXF writer itself cannot fail (it is infallible by design), so
/// the export command runs this first and fails with the error instead of silently
/// renaming a key or writing a short VERTICES entry.
pub fn validate_ovimap_dxf(d: &Drawing) -> Result<(), String> {
    for (id, e) in &d.ovimap_ext {
        if e.vertices.is_empty() {
            continue;
        }
        let key = format!("{}{}", cadcraft_doc::gis::VERTICES_PREFIX, id);
        if !crate::dxf_ext::valid_dict_key(&key) {
            return Err(format!("轨迹 `{id}` 的 DXF 字典键 `{key}` 含 DXF 不接受的字符，DXF 导出已拒绝（不得静默改名，见 ovimap-link §4.3/R2）"));
        }
        for v in &e.vertices {
            if v.get("ovimap_id").and_then(|x| x.as_str()).is_none_or(str::is_empty) {
                return Err(format!("轨迹 `{id}` 有顶点缺少 ovimap_id，DXF 导出已拒绝"));
            }
        }
    }
    Ok(())
}

/// The vertex ids a drawing's `ovimap_ext` holds, by track id (for the DXF reader's
/// VERTICES entries, kept in sync by the engine's import).
pub fn track_vertex_ids(ext: &BTreeMap<String, OvimapExt>) -> BTreeMap<String, Vec<String>> {
    ext.iter()
        .map(|(id, e)| (id.clone(), e.vertices.iter().filter_map(|v| v.get("ovimap_id").and_then(|x| x.as_str()).map(str::to_string)).collect()))
        .filter(|(_, ids): &(String, Vec<String>)| !ids.is_empty())
        .collect()
}
