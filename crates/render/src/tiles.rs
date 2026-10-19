//! Basemap tiles in a display list (`docs/ovimap-link.md` §9).
//!
//! A [`TileQuad`] is one XYZ tile placed in CAD coordinates: its four corners
//! come from [`cadcraft_doc::gis::tile_corners`] (WGS-84 tile bounds through the
//! drawing's §3 spherical-TM georef — the same projection the vector entities
//! use, so tiles and vectors align). The image is decoded RGBA (straight alpha);
//! a tile with `image: None` is a blank placeholder for a missing or failed
//! tile (§9.4): it draws nothing, never an error, never a crash.
//!
//! This module never touches the network or the file system: the caller supplies
//! tile bytes (the UI reads the disk cache; a miss is `None`). The rasteriser
//! ([`crate::raster`]) draws the quads under the vector primitives.

use cadcraft_doc::gis::{GeoRef, TileLayer, TilePlacement, TileRef, tile_placements};
use cadcraft_geom::Vec2;

/// Most pixels on one side of a decoded tile (OSM serves 256 px; 1024 leaves
/// room for retina custom sources while capping hostile images).
const MAX_TILE_DIM: u32 = 1024;

/// A decoded tile image: straight (unpremultiplied) RGBA, row-major, top row first.
#[derive(Clone, Debug, PartialEq)]
pub struct TileImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl TileImage {
    /// Build from straight-alpha RGBA bytes; `None` on a length mismatch or
    /// an absurd size (hostile input is refused, never ballooned).
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        if width == 0 || height == 0 || width > MAX_TILE_DIM || height > MAX_TILE_DIM {
            return None;
        }
        let want = (width as usize).saturating_mul(height as usize).saturating_mul(4);
        if rgba.len() != want {
            return None;
        }
        Some(TileImage { width, height, rgba })
    }

    /// Decode a tile image to straight-alpha RGBA, sniffing the format
    /// from the magic bytes: PNG (`89 50 4E 47…`) or JPEG (`FF D8 FF…`).
    /// Anything else (WebP, HTML error pages, garbage) is `None` — the
    /// caller draws a blank placeholder. Sniffing beats trusting the
    /// source's declared `tileFormat`: several real sources serve JPEG
    /// bytes (satellite imagery) while the dialog defaults to PNG, and a
    /// PNG-only decoder silently blanked every one of them (§9.5).
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > 1 << 22 {
            return None;
        }
        if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
            return Self::decode_png(bytes);
        }
        if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            return Self::decode_jpeg(bytes);
        }
        None
    }

    /// Decode a JPEG tile (satellite imagery) to straight-alpha RGBA
    /// (opaque: JPEG has no alpha). Dimensions are checked *before* the
    /// full decode so a hostile header cannot balloon an allocation.
    /// `None` for undecodable, truncated, or over-sized images.
    pub fn decode_jpeg(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > 1 << 22 {
            return None;
        }
        use std::io::Cursor;
        let (w, h) = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Jpeg)
            .into_dimensions()
            .ok()
            .filter(|(w, h)| *w > 0 && *h > 0 && *w <= MAX_TILE_DIM && *h <= MAX_TILE_DIM)?;
        let rgba = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).ok()?.to_rgba8();
        if rgba.width() != w || rgba.height() != h {
            return None;
        }
        TileImage::from_rgba(w, h, rgba.into_raw())
    }

    /// Decode a PNG (the tile format) to straight-alpha RGBA. `tiny-skia`
    /// decodes to premultiplied RGBA, so the alpha is divided back out; a
    /// fully transparent pixel becomes (0, 0, 0, 0). `None` for undecodable,
    /// indexed, or over-sized images — the caller draws a blank placeholder.
    /// Prefer [`TileImage::decode`]: it sniffs the format instead of
    /// assuming PNG.
    pub fn decode_png(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > 1 << 22 {
            return None;
        }
        let pm = tiny_skia::Pixmap::decode_png(bytes).ok()?;
        let (w, h) = (pm.width(), pm.height());
        if w == 0 || h == 0 || w > MAX_TILE_DIM || h > MAX_TILE_DIM {
            return None;
        }
        let mut rgba = Vec::with_capacity(pm.data().len());
        let (chunks, _) = pm.data().as_chunks::<4>();
        for px in chunks {
            let (r, g, b, a) = (u32::from(px[0]), u32::from(px[1]), u32::from(px[2]), u32::from(px[3]));
            if a == 0 {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                // Round-to-nearest unpremultiply, clamped.
                let un = |c: u32| u8::try_from((c * 255 + a / 2) / a).unwrap_or(u8::MAX);
                rgba.extend_from_slice(&[un(r), un(g), un(b), u8::try_from(a).unwrap_or(u8::MAX)]);
            }
        }
        TileImage::from_rgba(w, h, rgba)
    }

    /// Premultiplied RGBA for `tiny-skia` (`Pixmap::from_vec` wants premultiplied).
    pub fn premultiplied(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.rgba.len());
        let (chunks, _) = self.rgba.as_chunks::<4>();
        for px in chunks {
            let a = u32::from(px[3]);
            let mul = |c: u32| u8::try_from((c * a + 127) / 255).unwrap_or(u8::MAX);
            out.extend_from_slice(&[mul(u32::from(px[0])), mul(u32::from(px[1])), mul(u32::from(px[2])), px[3]]);
        }
        out
    }
}

/// One tile placed in CAD coordinates, with its image (or `None` = blank
/// placeholder for a missing/failed tile). Corners are NW, NE, SE, SW.
#[derive(Clone, Debug, PartialEq)]
pub struct TileQuad {
    pub tile: TileRef,
    pub corners: [Vec2; 4],
    pub opacity: f64,
    pub image: Option<TileImage>,
}

/// Build the tile quads for a CAD view: placements from
/// [`tile_placements`] (zoom capped by the layer, §9.1), each tile's bytes
/// fetched through `load` (cache-only; `None` = miss). A load or decode
/// failure keeps the quad with `image: None` (blank), never an error — the
/// vector drawing stays usable when tiles fail (§9.4).
pub fn build_tile_quads(
    layer: &TileLayer,
    georef: &GeoRef,
    min: Vec2,
    max: Vec2,
    zoom: Option<u32>,
    load: &dyn Fn(&TileRef) -> Option<Vec<u8>>,
) -> Vec<TileQuad> {
    let placements: Vec<TilePlacement> = tile_placements(layer, min, max, georef, zoom).unwrap_or_default();
    let opacity = layer.opacity.clamp(0.0, 1.0);
    placements
        .into_iter()
        .map(|p| {
            let image = load(&p.tile).and_then(|bytes| TileImage::decode(&bytes));
            TileQuad { tile: p.tile, corners: p.corners, opacity, image }
        })
        .collect()
}
