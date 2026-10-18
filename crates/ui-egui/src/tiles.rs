//! Basemap tile overlay on the canvas (`docs/ovimap-link.md` §9).
//!
//! The canvas draws the tiles **under** the vector display list, one textured
//! quad per tile: the tile's WGS-84 corners through the drawing's §3 georef
//! ([`cadcraft_doc::gis::tile_placements`]), so tiles and vectors share one
//! projection and align. The layer's opacity scales the quad's alpha; the
//! zoom comes from [`cadcraft_doc::gis::tiles_for_view`] (finest zoom whose
//! tile count fits, capped by the layer — above the cap the cached tiles are
//! drawn enlarged, never re-requested, §9.1).
//!
//! Tiles a view needs but the disk cache lacks are enqueued with the
//! background fetcher ([`cadcraft_engine::tilefetch::TileFetcher`]: OSM Tile
//! Usage Policy, cache-first, ≤ 4 concurrent); a fetch landing clears the
//! decoded textures so the next frame re-decodes the fresh bytes, and asks
//! egui for a repaint. A missing, undecodable or failed tile draws nothing —
//! the background shows through as a blank placeholder (§9.4) — and the
//! failure is remembered with exponential back-off
//! ([`cadcraft_doc::gis::tile_backoff_delay_ms`], at most 3 retries' worth of
//! delay growth) so a dead source is not re-read every frame. Vectors are
//! never blocked by tiles: this function returns early on any error.

use std::collections::HashMap;
use std::time::Instant;

use cadcraft_doc::Drawing;
use cadcraft_doc::gis::{TILE_ATTRIBUTION_FULL, TILE_COPYRIGHT_URL, tile_backoff_delay_ms, tile_cache_rel_path, tile_placements};
use cadcraft_engine::tilefetch::TileFetcher;
use cadcraft_engine::tilemap::{effective_tilemap, is_default_osm, load_cached_tile_with_fallback};
use cadcraft_geom::Vec2;
use cadcraft_render::TileImage;
use egui::{Color32, ColorImage, Mesh, Painter, Pos2, TextureFilter, TextureHandle, TextureOptions};

use crate::canvas::Xf;

/// The canvas tile state: decoded textures by cache-relative path, and
/// failed tiles with their retry time (back-off, §9.4).
#[derive(Default)]
pub struct TileCache {
    textures: HashMap<String, TextureHandle>,
    failed: HashMap<String, (u32, Instant)>,
}

impl TileCache {
    /// Whether any tile recently failed to load (the status bar's one-line
    /// hint reads this; failures clear when a tile loads).
    pub fn has_failures(&self) -> bool {
        !self.failed.is_empty()
    }

    /// Decoded textures currently held (test support: a drawn tile lands here).
    pub fn texture_count(&self) -> usize {
        self.textures.len()
    }
}

/// Most tile textures held at once; past that the cache is cleared wholesale
/// (tiles are cheap to re-decode from the disk cache, and a view rarely
/// needs more than a few dozen).
const MAX_TEXTURES: usize = 256;

/// The attribution to show in the status bar (§9.2): the layer's display
/// attribution (primary + fallback credit when a fallback is configured, so
/// tiles from either source are credited) when tiles are enabled, `None`
/// when they are off (the attribution hides with the tiles). A custom
/// source's attribution is shown verbatim.
pub fn attribution(doc: &Drawing) -> Option<String> {
    let layer = effective_tilemap(doc);
    if layer.enabled && !layer.display_attribution().trim().is_empty() { Some(layer.display_attribution()) } else { None }
}

/// The full attribution for the layer-properties / about panel (§9.2): the
/// text plus the copyright link. The OSM default gets the full text +
/// `https://www.openstreetmap.org/copyright` + ODbL note; a custom source's
/// attribution is shown verbatim with no link (its licence is the user's).
/// `None` when tiles are off.
pub fn attribution_full(doc: &Drawing) -> Option<(String, Option<String>)> {
    let layer = effective_tilemap(doc);
    if !layer.enabled || layer.attribution.trim().is_empty() {
        return None;
    }
    if is_default_osm(&layer) {
        Some((TILE_ATTRIBUTION_FULL.to_string(), Some(TILE_COPYRIGHT_URL.to_string())))
    } else {
        Some((layer.attribution.clone(), None))
    }
}

/// The model→screen transform for drawing tiles inside one layout viewport:
/// a model point `m` sits at paper `center + s·(m − view_center)` with
/// `s = vp_height / view_height` (the same mapping the vector contents use),
/// and paper maps to screen through `xf_paper`. `None` for a degenerate
/// viewport or paper transform. Each viewport gets its own transform (and so
/// its own `tiles_for_view`), never the canvas's.
pub fn viewport_tile_xf(xf_paper: &Xf, vp_center: Vec2, vp_height: f64, view_center: Vec2, view_height: f64) -> Option<Xf> {
    if !(vp_height.is_finite()
        && vp_height > 0.0
        && view_height.is_finite()
        && view_height > 0.0
        && vp_center.is_finite()
        && view_center.is_finite()
        && xf_paper.scale.is_finite()
        && xf_paper.scale > 0.0)
    {
        return None;
    }
    let s = vp_height / view_height;
    if !(s.is_finite() && s > 0.0) {
        return None;
    }
    let center = view_center + (xf_paper.center - vp_center) / s;
    let xf = Xf { rect: xf_paper.rect, center, scale: xf_paper.scale * s };
    (xf.center.is_finite() && xf.scale.is_finite() && xf.scale > 0.0).then_some(xf)
}

/// Draw the tile basemap under the vectors for the model-space view `xf`,
/// loading tile bytes from the disk cache. No georef, a disabled layer, or
/// a tile error → nothing is drawn.
pub fn draw_tilemap(cache: &mut TileCache, doc: &Drawing, painter: &Painter, xf: &Xf, ctx: &egui::Context) {
    // A fetch landed since the last frame: drop the decoded textures so the
    // fresh bytes are re-decoded, and repaint.
    if TileFetcher::global().take_dirty() {
        cache.textures.clear();
        ctx.request_repaint();
    }
    draw_tilemap_with_hook(cache, doc, painter, xf, ctx, &|layer, tile| load_cached_tile_with_fallback(layer, tile), &|layer, tile| {
        TileFetcher::global().request(layer, tile);
    });
}

/// [`draw_tilemap`] with an injectable byte loader (tests seed a scratch dir
/// instead of the real cache; production always passes the disk cache).
pub fn draw_tilemap_with(
    cache: &mut TileCache,
    doc: &Drawing,
    painter: &Painter,
    xf: &Xf,
    ctx: &egui::Context,
    load: &dyn Fn(&cadcraft_doc::gis::TileLayer, &cadcraft_doc::gis::TileRef) -> Option<Vec<u8>>,
) {
    draw_tilemap_with_hook(cache, doc, painter, xf, ctx, load, &|_, _| {});
}

/// [`draw_tilemap_with`] with an `on_placement` hook, called for every tile
/// the view needs (production: enqueue it with the background fetcher, whose
/// freshness check makes this a no-op for fresh tiles).
pub fn draw_tilemap_with_hook(
    cache: &mut TileCache,
    doc: &Drawing,
    painter: &Painter,
    xf: &Xf,
    ctx: &egui::Context,
    load: &dyn Fn(&cadcraft_doc::gis::TileLayer, &cadcraft_doc::gis::TileRef) -> Option<Vec<u8>>,
    on_placement: &dyn Fn(&cadcraft_doc::gis::TileLayer, &cadcraft_doc::gis::TileRef),
) {
    let layer = effective_tilemap(doc);
    if !layer.enabled || layer.opacity <= 0.0 {
        return;
    }
    let Some(georef) = doc.georef else { return };
    let bounds = xf.world_bounds();
    let placements = tile_placements(&layer, bounds.min, bounds.max, &georef, None).unwrap_or_default();
    if cache.textures.len() > MAX_TEXTURES {
        cache.textures.clear();
    }
    let now = Instant::now();
    let alpha = (layer.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
    for p in &placements {
        on_placement(&layer, &p.tile);
        let key = tile_cache_rel_path(&layer, &p.tile);
        if let Some((_, retry_at)) = cache.failed.get(&key)
            && now < *retry_at
        {
            continue;
        }
        if !cache.textures.contains_key(&key) {
            let decoded = load(&layer, &p.tile).and_then(|bytes| TileImage::decode_png(&bytes));
            match decoded {
                Some(img) => {
                    let (w, h) = (img.width as usize, img.height as usize);
                    let tex = ctx.load_texture(
                        key.clone(),
                        ColorImage::from_rgba_unmultiplied([w, h], &img.rgba),
                        TextureOptions { magnification: TextureFilter::Linear, minification: TextureFilter::Linear, ..Default::default() },
                    );
                    cache.textures.insert(key.clone(), tex);
                    cache.failed.remove(&key);
                }
                None => {
                    let attempts = cache.failed.get(&key).map(|(a, _)| *a).unwrap_or(0).saturating_add(1);
                    let delay = tile_backoff_delay_ms(attempts);
                    cache.failed.insert(key.clone(), (attempts, now + std::time::Duration::from_millis(delay)));
                    continue;
                }
            }
        }
        let Some(tex) = cache.textures.get(&key) else { continue };
        let screen: Vec<Pos2> = p.corners.iter().map(|c| xf.to_screen(*c)).collect();
        if screen.len() != 4 || !screen.iter().all(|s| s.x.is_finite() && s.y.is_finite()) {
            continue;
        }
        let color = Color32::from_white_alpha(alpha);
        let mut mesh = Mesh { texture_id: tex.id(), ..Default::default() };
        // Corners are NW, NE, SE, SW; UVs follow the same order.
        for (i, pos) in screen.iter().enumerate() {
            let uv = match i {
                0 => Pos2::new(0.0, 0.0),
                1 => Pos2::new(1.0, 0.0),
                2 => Pos2::new(1.0, 1.0),
                _ => Pos2::new(0.0, 1.0),
            };
            mesh.vertices.push(egui::epaint::Vertex { pos: *pos, uv, color });
        }
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(egui::Shape::mesh(mesh));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_doc::gis::{GeoRef, TileLayer, tile_placements};
    use cadcraft_geom::Vec2;
    use egui::{Pos2, Rect, vec2};

    #[test]
    fn tilemap_attribution_hides_with_tiles() {
        let mut d = Drawing::new_metric();
        // No stored layer → the OSM default, enabled, with the combined
        // attribution (primary OSM + CARTO fallback, both credited).
        assert_eq!(attribution(&d), Some("© OpenStreetMap 贡献者 © CARTO".to_string()));
        d.tilemap = Some(TileLayer { enabled: false, ..TileLayer::default_osm() });
        assert_eq!(attribution(&d), None, "attribution hides with the tiles (§9.2)");
        d.tilemap = Some(TileLayer {
            url_template: "https://example.com/{z}/{x}/{y}.png".into(),
            attribution: "Example 贡献者".into(),
            opacity: 1.0,
            enabled: true,
            max_zoom: 19,
            coord_type: cadcraft_doc::gis::CoordType::Wgs84,
            fallback_url_template: None,
            fallback_attribution: None,
        });
        assert_eq!(attribution(&d), Some("Example 贡献者".to_string()), "a custom source's attribution shows verbatim");
    }

    fn red_png_bytes() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap_or(());
        bytes
    }

    #[test]
    fn tilemap_overlay_cache_behaviour() {
        // 1) Seeded cache: every tile the view needs becomes a texture.
        let dir = std::env::temp_dir().join(format!("cadcraft-tilemap-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let layer = TileLayer::default_osm();
        let georef = GeoRef::centred(32.1301, 114.0814);
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 400.0));
        let xf = crate::canvas::Xf { rect, center: Vec2::ZERO, scale: 0.18 };
        let bounds = xf.world_bounds();
        let placements = tile_placements(&layer, bounds.min, bounds.max, &georef, None).unwrap_or_default();
        assert!(!placements.is_empty());
        for p in &placements {
            let path = dir.join(tile_cache_rel_path(&layer, &p.tile));
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, red_png_bytes());
        }
        let mut d = Drawing::new_metric();
        d.georef = Some(georef);
        d.tilemap = Some(layer);
        let ctx = egui::Context::default();
        let painter = egui::Painter::new(ctx.clone(), egui::LayerId::background(), rect);
        let mut cache = TileCache::default();
        let load = |layer: &TileLayer, tile: &cadcraft_doc::gis::TileRef| std::fs::read(dir.join(tile_cache_rel_path(layer, tile))).ok();
        draw_tilemap_with(&mut cache, &d, &painter, &xf, &ctx, &load);
        assert_eq!(cache.texture_count(), placements.len(), "every cached tile becomes a texture");
        // 2) Layer off: nothing is drawn, no new textures.
        d.tilemap = Some(TileLayer { enabled: false, ..TileLayer::default_osm() });
        draw_tilemap_with(&mut cache, &d, &painter, &xf, &ctx, &load);
        assert_eq!(cache.texture_count(), placements.len());
        // No georef: no alignment, nothing drawn, no crash.
        let mut d2 = Drawing::new_metric();
        d2.tilemap = Some(TileLayer::default_osm());
        draw_tilemap_with(&mut cache, &d2, &painter, &xf, &ctx, &load);
        assert_eq!(cache.texture_count(), placements.len());
        // 3) Empty cache: every tile is a blank placeholder — no texture, no panic.
        let empty = std::env::temp_dir().join(format!("cadcraft-tilemap-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&empty);
        let _ = std::fs::create_dir_all(&empty);
        let mut d3 = Drawing::new_metric();
        d3.georef = Some(GeoRef::centred(32.1301, 114.0814));
        d3.tilemap = Some(TileLayer::default_osm());
        let mut cache2 = TileCache::default();
        let load_empty = |layer: &TileLayer, tile: &cadcraft_doc::gis::TileRef| std::fs::read(empty.join(tile_cache_rel_path(layer, tile))).ok();
        draw_tilemap_with(&mut cache2, &d3, &painter, &xf, &ctx, &load_empty);
        assert_eq!(cache2.texture_count(), 0, "a cache miss draws blank, never a texture");
        // A second draw inside the back-off window does not re-read the cache.
        draw_tilemap_with(&mut cache2, &d3, &painter, &xf, &ctx, &load_empty);
        assert_eq!(cache2.texture_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn attribution_full_default_and_custom() {
        let d = Drawing::new_metric();
        let (text, url) = attribution_full(&d).expect("the OSM default has full attribution");
        assert!(text.contains("OpenStreetMap"), "full text names OSM");
        assert!(text.contains("ODbL"), "full text names the licence");
        assert_eq!(url.as_deref(), Some("https://www.openstreetmap.org/copyright"));
        let mut off = Drawing::new_metric();
        off.tilemap = Some(TileLayer { enabled: false, ..TileLayer::default_osm() });
        assert_eq!(attribution_full(&off), None, "attribution hides with the tiles");
        let mut custom = Drawing::new_metric();
        custom.tilemap = Some(TileLayer {
            url_template: "https://example.com/{z}/{x}/{y}.png".into(),
            attribution: "Example 贡献者".into(),
            opacity: 1.0,
            enabled: true,
            max_zoom: 19,
            coord_type: cadcraft_doc::gis::CoordType::Wgs84,
            fallback_url_template: None,
            fallback_attribution: None,
        });
        assert_eq!(
            attribution_full(&custom),
            Some(("Example 贡献者".to_string(), None)),
            "a custom source's attribution shows verbatim, with no link"
        );
    }

    #[test]
    fn viewport_tile_xf_matches_paper_mapping() {
        // Paper: centre (100, 100), 2 px per unit. Viewport: paper centre
        // (100, 100), 50 tall, showing model centre (0, 0), 100 tall.
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 400.0));
        let xf_paper = crate::canvas::Xf { rect, center: Vec2::new(100.0, 100.0), scale: 2.0 };
        let xf = viewport_tile_xf(&xf_paper, Vec2::new(100.0, 100.0), 50.0, Vec2::ZERO, 100.0).expect("a valid viewport maps");
        assert!((xf.scale - 1.0).abs() < 1e-9, "scale = paper scale × (50 / 100)");
        // A model point maps through the viewport exactly as through paper:
        // paper = centre + 0.5·(m − view centre), then the paper transform.
        for m in [Vec2::new(10.0, 10.0), Vec2::new(-40.0, 25.0), Vec2::new(0.0, -50.0)] {
            let paper = Vec2::new(100.0, 100.0) + (m - Vec2::ZERO) * 0.5;
            let a = xf.to_screen(m);
            let b = xf_paper.to_screen(paper);
            assert!((a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3, "model {m:?}: viewport {a:?} vs paper {b:?}");
        }
        assert!(viewport_tile_xf(&xf_paper, Vec2::new(100.0, 100.0), 50.0, Vec2::ZERO, 0.0).is_none(), "zero view height: no mapping");
        assert!(viewport_tile_xf(&xf_paper, Vec2::new(100.0, 100.0), -5.0, Vec2::ZERO, 100.0).is_none(), "negative viewport height: no mapping");
        assert!(viewport_tile_xf(&xf_paper, Vec2::new(f64::NAN, 100.0), 50.0, Vec2::ZERO, 100.0).is_none(), "NaN centre: no mapping");
    }

    #[test]
    fn tilemap_per_viewport_independent() {
        // Two views (two split viewports) over different ground: each draws
        // its own tiles_for_view, and the texture cache holds the union.
        let dir = std::env::temp_dir().join(format!("cadcraft-tilemap-split-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let layer = TileLayer::default_osm();
        let georef = GeoRef::centred(32.1301, 114.0814);
        // Zoomed-in views: each covers ~1 m of ground, so even at the finest
        // zoom (19) it needs only a couple dozen tiles, and the union of both
        // views fits the texture cache (MAX_TEXTURES). (tiles_for_view fills
        // up to 512 tiles per view, so a zoomed-out view alone would already
        // trip the cache's wholesale clear.)
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(200.0, 200.0));
        let xf1 = crate::canvas::Xf { rect, center: Vec2::ZERO, scale: 200.0 };
        let xf2 = crate::canvas::Xf { rect, center: Vec2::new(50_000.0, 0.0), scale: 200.0 };
        let mut all = std::collections::HashSet::new();
        for xf in [&xf1, &xf2] {
            let bounds = xf.world_bounds();
            for p in tile_placements(&layer, bounds.min, bounds.max, &georef, None).unwrap_or_default() {
                all.insert(tile_cache_rel_path(&layer, &p.tile));
                let path = dir.join(tile_cache_rel_path(&layer, &p.tile));
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&path, red_png_bytes());
            }
        }
        assert!(all.len() > 1, "the two views need different tiles");
        assert!(all.len() <= MAX_TEXTURES, "the union must fit the texture cache, got {}", all.len());
        let mut d = Drawing::new_metric();
        d.georef = Some(georef);
        d.tilemap = Some(layer);
        let ctx = egui::Context::default();
        let painter = egui::Painter::new(ctx.clone(), egui::LayerId::background(), rect);
        let mut cache = TileCache::default();
        let load = |layer: &TileLayer, tile: &cadcraft_doc::gis::TileRef| std::fs::read(dir.join(tile_cache_rel_path(layer, tile))).ok();
        draw_tilemap_with(&mut cache, &d, &painter, &xf1, &ctx, &load);
        let first = cache.texture_count();
        assert!(first > 0 && first < all.len(), "the first viewport draws only its own tiles ({first} of {})", all.len());
        draw_tilemap_with(&mut cache, &d, &painter, &xf2, &ctx, &load);
        assert_eq!(cache.texture_count(), all.len(), "each viewport's tiles are drawn, independently");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
