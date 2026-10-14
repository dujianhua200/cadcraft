//! OviMap GIS link: geographic reference, tile layers, OVIMAP xdata and GeoJSON.
//!
//! Pure data-model mapping (entities `<->` GeoJSON values, lon/lat `<->` CAD
//! coordinates), with no file or network I/O, so the engine can use it directly while
//! the io crate owns the DXF storage (the `OVIMAP_GEOREF` dictionary, the `GEOREF` and
//! `VERTICES_*` XRECORDs) and the tile disk cache. Nothing here reads a file, opens a
//! socket or spawns a thread.
//!
//! The wire format is `docs/ovimap-link.md` v1.0 (Schema `"1.0"`):
//!
//! - GeoJSON `FeatureCollection` + foreign members `ovimap_georef` / `ovimap_meta` /
//!   `ovimap_deletes`; coordinates are WGS-84 `[lon, lat]` (a third element, if
//!   present, is ignored — the link is 2D).
//! - `timestamps` is `null` (ovimap has no per-point timestamps; never fabricate).
//!   An array is accepted on import (length must equal the coordinate count) but is
//!   not stored: CadCraft has nowhere to keep it, and export writes `null` again.
//! - Track vertex identities live in `vertices[]` (GeoJSON) / the `VERTICES_*`
//!   XRECORD (DXF), never in LWPOLYLINE vertex xdata (DXF vertices have no xdata).
//! - Entity xdata is exactly `1001 OVIMAP` + `1000` ovimap_id / name / kind / type
//!   (§4.4, order fixed, each at most 255 characters). Everything else a feature
//!   carries (note, symbol, photos, business fields, passthrough, vertex details)
//!   lives in the drawing's [`OvimapExt`] map — the feature's "private extension
//!   area". DXF save drops that area except the vertex ids (§4.6: DXF preserves
//!   geometry + ids, not business fields / passthrough / times).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use cadcraft_color::Color;
use cadcraft_geom::{Vec2, Vec3};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Common, Drawing, EntityKind, LwPolyline, MText, Point, RawTag, Text};

/// Registered application name of the OVIMAP xdata (and the APPID table entry).
pub const OVIMAP_APP: &str = "OVIMAP";
/// Layer of marker POINT + TEXT (§4.5).
pub const LAYER_MARKER: &str = "OVIMAP_MARKER";
/// Layer of track LWPOLYLINEs (§4.5).
pub const LAYER_TRACK: &str = "OVIMAP_TRACK";
/// Layer of label TEXT / MTEXT (§4.5).
pub const LAYER_LABEL: &str = "OVIMAP_LABEL";
/// Layer of photo hint TEXTs (§4.5).
pub const LAYER_PHOTO: &str = "OVIMAP_PHOTO";
/// Named-object-dictionary key of the `OVIMAP_GEOREF` dictionary (§4.3).
pub const GEOREF_DICT_KEY: &str = "OVIMAP_GEOREF";
/// Dictionary entry of the georef XRECORD inside `OVIMAP_GEOREF`.
pub const GEOREF_ENTRY: &str = "GEOREF";
/// Dictionary-entry prefix of a track's vertex-id XRECORD (`VERTICES_<lineGroupId>`).
pub const VERTICES_PREFIX: &str = "VERTICES_";
/// Projection name (`ovimap_georef.projection`, GEOREF XRECORD group 1000).
pub const PROJECTION_NAME: &str = "OVIMAP_SPHERICAL_TM";
/// The ellipsoidal projection name: recognised only to refuse it with a clear error (§3.7).
pub const ELLIPSOIDAL_PROJECTION_NAME: &str = "OVIMAP_ELLIPSOIDAL_TM";
/// GeoJSON schema version (`ovimap_meta.schema_version`).
pub const SCHEMA_VERSION: &str = "1.0";
/// Text height of marker/label/photo texts, in drawing units (metres, §5.2).
pub const TEXT_HEIGHT: f64 = 2.5;
/// How far above its point a marker's label text sits, in drawing units (display only).
pub const MARKER_TEXT_DY: f64 = 3.0;
/// How far below its feature a photo hint text sits, in drawing units (display only).
pub const PHOTO_TEXT_DY: f64 = -3.0;
/// Default tile service: OpenStreetMap's standard tile server (§9.1, the only default).
pub const DEFAULT_TILE_URL: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
/// Attribution shown with the default tile service (status bar, §9.2).
pub const DEFAULT_TILE_ATTRIBUTION: &str = "© OpenStreetMap 贡献者";
/// OSM copyright page (layer properties / about panel, §9.2).
pub const TILE_COPYRIGHT_URL: &str = "https://www.openstreetmap.org/copyright";
/// Full attribution for the layer-properties / about panel (§9.2).
pub const TILE_ATTRIBUTION_FULL: &str = "© OpenStreetMap 贡献者 (https://www.openstreetmap.org/copyright)，数据 © OpenStreetMap 贡献者，ODbL";
/// The attribution line plots carry: the ASCII form of the OSM attribution.
/// Plot text is drawn with the Latin-only drafting font (and PDF's standard
/// fonts are Latin-only), so the full Chinese form (`TILE_ATTRIBUTION_FULL`)
/// lives in the UI panels, not on the sheet.
pub const PLOT_ATTRIBUTION: &str = "(c) OpenStreetMap contributors";

/// Whether the basemap shows for this drawing — and so its attribution must
/// (§9.2): the effective tile layer is enabled and a georef aligns it. No
/// georef → no tiles → no attribution (the attribution hides with the tiles).
pub fn basemap_visible(d: &Drawing) -> bool {
    // No stored layer means the OSM default, which is enabled.
    d.georef.is_some() && d.tilemap.as_ref().is_none_or(|t| t.enabled)
}
/// Highest zoom the default OSM tiles are requested at (§9.1: never request z > 19).
pub const DEFAULT_TILE_MAX_ZOOM: u32 = 19;

/// Most features one GeoJSON import accepts (hostile files are capped, never ballooned).
const MAX_FEATURES: usize = 100_000;
/// Most points one feature may carry.
const MAX_POINTS_PER_FEATURE: usize = 100_000;
/// Most points one import may carry in total.
const MAX_TOTAL_POINTS: usize = 1_000_000;
/// Most tiles one `tiles_for_view` call returns.
const MAX_TILES: usize = 512;
/// Longest string one DXF 1000 xdata group may hold, in characters (§4.4).
const MAX_XDATA_STR_CHARS: usize = 255;
/// The drawing's geographic reference: the project centre + projection parameters
/// (`ovimap_georef`, §2.2 / §3.1). All fields are fixed by the spec except the centre:
/// spherical transverse Mercator, `k0 = 1`, false easting/northing = 0, `R = 6378137`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct GeoRef {
    /// Latitude of the projection origin (project centre), in degrees.
    pub origin_lat: f64,
    /// Longitude of the projection origin (project centre), in degrees.
    pub origin_lon: f64,
    /// Central meridian, in degrees (= `origin_lon`).
    pub central_meridian: f64,
    /// Scale factor `k0` (always 1).
    pub scale_factor: f64,
    /// False easting, in metres (always 0).
    pub false_easting: f64,
    /// False northing, in metres (always 0).
    pub false_northing: f64,
    /// Sphere radius, in metres (always 6378137).
    pub radius: f64,
}

impl GeoRef {
    /// A georef centred on `lat`/`lon` (origin = centre, central meridian = centre longitude).
    pub fn centred(lat: f64, lon: f64) -> Self {
        GeoRef {
            origin_lat: lat,
            origin_lon: lon,
            central_meridian: lon,
            scale_factor: 1.0,
            false_easting: 0.0,
            false_northing: 0.0,
            radius: cadcraft_geom::tmerc::EARTH_RADIUS,
        }
    }

    /// Whether the reference is usable per §3.1 (ranges + the fixed parameters).
    pub fn is_valid(&self) -> bool {
        self.origin_lat.is_finite()
            && (-90.0..=90.0).contains(&self.origin_lat)
            && self.origin_lon.is_finite()
            && (-180.0..=180.0).contains(&self.origin_lon)
            && self.central_meridian == self.origin_lon
            && self.scale_factor == 1.0
            && self.false_easting == 0.0
            && self.false_northing == 0.0
            && self.radius == cadcraft_geom::tmerc::EARTH_RADIUS
    }

    fn to_params(self) -> cadcraft_geom::tmerc::TmercParams {
        cadcraft_geom::tmerc::TmercParams { origin_lat: self.origin_lat, central_meridian: self.central_meridian }
    }

    /// Project longitude/latitude (degrees) to CAD coordinates (metres, §3.2).
    pub fn project(&self, lon: f64, lat: f64) -> Option<Vec2> {
        cadcraft_geom::tmerc::forward(lon, lat, &self.to_params()).map(|(x, y)| Vec2::new(x, y))
    }

    /// Inverse-project CAD coordinates (metres) to longitude/latitude (degrees, §3.3).
    pub fn unproject(&self, p: Vec2) -> Option<(f64, f64)> {
        cadcraft_geom::tmerc::inverse(p.x, p.y, &self.to_params())
    }

    /// The GeoJSON `ovimap_georef` object for this reference (§2.2).
    pub fn to_json(&self) -> Value {
        json!({
            "projection": PROJECTION_NAME,
            "source_crs": "WGS84",
            "origin_lat": self.origin_lat,
            "origin_lon": self.origin_lon,
            "central_meridian": self.central_meridian,
            "scale_factor": self.scale_factor,
            "false_easting": self.false_easting,
            "false_northing": self.false_northing,
            "radius": self.radius,
            "units": "metre",
        })
    }

    /// Parse a GeoJSON `ovimap_georef` object. `projection` must be
    /// `OVIMAP_SPHERICAL_TM` (an ellipsoidal file is refused, §3.7, never silently
    /// computed with the spherical formula). Errors are in Chinese for the command line.
    pub fn from_json(v: &Value) -> Result<GeoRef, String> {
        let obj = v.as_object().ok_or_else(|| "ovimap_georef 必须是对象".to_string())?;
        let projection = obj.get("projection").and_then(Value::as_str).unwrap_or("");
        if projection == ELLIPSOIDAL_PROJECTION_NAME {
            return Err("文件使用椭球横轴墨卡托（OVIMAP_ELLIPSOIDAL_TM），当前版本仅支持球面版，请用球面版文件".into());
        }
        if projection != PROJECTION_NAME {
            return Err(format!("ovimap_georef.projection 必须是 {PROJECTION_NAME}，得到 `{projection}`"));
        }
        if let Some(crs) = obj.get("source_crs").and_then(Value::as_str)
            && crs != "WGS84"
        {
            return Err(format!("ovimap_georef.source_crs 必须是 WGS84，得到 `{crs}`"));
        }
        if let Some(units) = obj.get("units").and_then(Value::as_str)
            && units != "metre"
        {
            return Err(format!("ovimap_georef.units 必须是 metre，得到 `{units}`"));
        }
        let num = |key: &str, default: Option<f64>| -> Result<f64, String> {
            match obj.get(key).and_then(Value::as_f64).filter(|x| x.is_finite()) {
                Some(x) => Ok(x),
                None => default.ok_or_else(|| format!("ovimap_georef 缺少 `{key}`（须为数字）")),
            }
        };
        let origin_lat = num("origin_lat", None)?;
        let origin_lon = num("origin_lon", None)?;
        let g = GeoRef {
            origin_lat,
            origin_lon,
            central_meridian: num("central_meridian", Some(origin_lon))?,
            scale_factor: num("scale_factor", Some(1.0))?,
            false_easting: num("false_easting", Some(0.0))?,
            false_northing: num("false_northing", Some(0.0))?,
            radius: num("radius", Some(cadcraft_geom::tmerc::EARTH_RADIUS))?,
        };
        if g.is_valid() {
            Ok(g)
        } else {
            Err("ovimap_georef 数值不符合规范（中央子午线须等于原点经度、scale_factor 须为 1、false_easting/false_northing 须为 0、radius 须为 6378137）".into())
        }
    }

    /// The DXF GEOREF XRECORD groups, in the fixed order of §4.3: `1000` projection,
    /// `1040` origin_lat / origin_lon / central_meridian / scale_factor / radius,
    /// `1000` metre / WGS84 / `1.0` (schema version).
    pub fn to_xrecord_tags(&self) -> Vec<RawTag> {
        let mut tags = vec![RawTag { code: 1000, value: PROJECTION_NAME.to_string() }];
        for v in [self.origin_lat, self.origin_lon, self.central_meridian, self.scale_factor, self.radius] {
            tags.push(RawTag { code: 1040, value: v.to_string() });
        }
        for s in ["metre", "WGS84", SCHEMA_VERSION] {
            tags.push(RawTag { code: 1000, value: s.to_string() });
        }
        tags
    }

    /// Parse the DXF GEOREF XRECORD groups written by [`GeoRef::to_xrecord_tags`]:
    /// projection, k0 and R are validated (§4.3); anything else yields `None`.
    pub fn from_xrecord_tags(tags: &[RawTag]) -> Option<GeoRef> {
        let mut strings = tags.iter().filter(|t| t.code == 1000).map(|t| t.value.trim());
        let projection = strings.next()?;
        if projection != PROJECTION_NAME {
            return None;
        }
        let nums: Vec<f64> =
            tags.iter().filter(|t| t.code == 1040).filter_map(|t| t.value.trim().parse::<f64>().ok().filter(|x| x.is_finite())).collect();
        if nums.len() != 5 {
            return None;
        }
        let (units, crs, version) = (strings.next(), strings.next(), strings.next());
        if units != Some("metre") || crs != Some("WGS84") || version != Some(SCHEMA_VERSION) {
            return None;
        }
        let g = GeoRef {
            origin_lat: nums[0],
            origin_lon: nums[1],
            central_meridian: nums[2],
            scale_factor: nums[3],
            false_easting: 0.0,
            false_northing: 0.0,
            radius: nums[4],
        };
        g.is_valid().then_some(g)
    }
}

/// An XYZ tile layer (a web-map basemap): a URL template plus its attribution (§9).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TileLayer {
    /// URL template with `{z}`, `{x}`, `{y}` placeholders.
    pub url_template: String,
    /// Attribution text shown with the tiles (required for a custom source, §9.5).
    pub attribution: String,
    /// Tile opacity, 0 (transparent) to 1 (opaque).
    pub opacity: f64,
    /// Whether the layer draws.
    pub enabled: bool,
    /// Highest zoom requested (never above 19 for the OSM default, §9.1).
    pub max_zoom: u32,
}

impl TileLayer {
    /// The default layer: OSM standard tiles with their attribution (§9.1).
    pub fn default_osm() -> Self {
        TileLayer {
            url_template: DEFAULT_TILE_URL.into(),
            attribution: DEFAULT_TILE_ATTRIBUTION.into(),
            opacity: 0.85,
            enabled: true,
            max_zoom: DEFAULT_TILE_MAX_ZOOM,
        }
    }

    /// Validate the layer: the template must carry `{z}`, `{x}` and `{y}`; a custom
    /// source must carry an attribution (§9.5); opacity must be in 0..=1; the OSM
    /// default never requests above z=19. Errors are in Chinese for the command line.
    pub fn validate(&self) -> Result<(), String> {
        if self.url_template.trim().is_empty() {
            return Err("瓦片 URL 模板为空".into());
        }
        for ph in ["{z}", "{x}", "{y}"] {
            if !self.url_template.contains(ph) {
                return Err(format!("瓦片 URL 模板缺少占位符 {ph}（须含 {{z}}/{{x}}/{{y}}）"));
            }
        }
        if self.enabled && self.attribution.trim().is_empty() {
            return Err("自定义瓦片源必须填写署名（署名为空时拒绝启用）".into());
        }
        if !self.opacity.is_finite() || !(0.0..=1.0).contains(&self.opacity) {
            return Err("瓦片透明度须在 0~1 之间".into());
        }
        if self.url_template.trim() == DEFAULT_TILE_URL && self.max_zoom > DEFAULT_TILE_MAX_ZOOM {
            return Err(format!("默认 OSM 瓦片最高级别为 {DEFAULT_TILE_MAX_ZOOM}，不得请求更高级别"));
        }
        if self.max_zoom > 30 {
            return Err("瓦片最高级别须 ≤ 30".into());
        }
        Ok(())
    }

    /// Expand the template for one tile.
    pub fn tile_url(&self, z: u32, x: u32, y: u32) -> String {
        self.url_template.replace("{z}", &z.to_string()).replace("{x}", &x.to_string()).replace("{y}", &y.to_string())
    }

    /// Cache file name for one tile: `z/x/y.<ext>` (extension from the template, else png).
    pub fn cache_key(&self, z: u32, x: u32, y: u32) -> String {
        let ext = self.url_template.rsplit('.').next().unwrap_or("png");
        let ext = if !ext.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()) { ext } else { "png" };
        format!("{z}/{x}/{y}.{ext}")
    }
}

/// One tile the current view needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TileRef {
    pub z: u32,
    pub x: u32,
    pub y: u32,
    pub url: String,
    pub cache_key: String,
}

/// Longitude/latitude (degrees) to the Web-Mercator tile containing it at zoom `z`.
/// Returns `None` for invalid input or `z > 30`.
pub fn lonlat_to_tile(lon: f64, lat: f64, z: u32) -> Option<(u32, u32)> {
    if !lon.is_finite() || !lat.is_finite() || z > 30 {
        return None;
    }
    let n = 2f64.powi(i32::try_from(z).ok()?);
    let x = ((lon + 180.0) / 360.0 * n).floor();
    let lat_c = lat.clamp(-85.051_128_78, 85.051_128_78).to_radians();
    let y = ((1.0 - (lat_c.tan() + 1.0 / lat_c.cos()).ln() / std::f64::consts::PI) / 2.0 * n).floor();
    if !(0.0..n).contains(&x) || !(0.0..n).contains(&y) {
        return None;
    }
    Some((x as u32, y as u32))
}

/// Tiles covering a CAD view (`min`/`max` in drawing units). With no explicit zoom, the
/// finest zoom (up to the layer's `max_zoom`) whose tile count fits [`MAX_TILES`] is
/// used. A requested zoom above `max_zoom` is served from `max_zoom` (zoomed display,
/// never a request above the cap, §9.1). Errors are in Chinese.
pub fn tiles_for_view(layer: &TileLayer, min: Vec2, max: Vec2, georef: &GeoRef, zoom: Option<u32>) -> Result<Vec<TileRef>, String> {
    if !min.is_finite() || !max.is_finite() || max.x <= min.x || max.y <= min.y {
        return Err("视图范围无效（须为有限矩形，且右上角大于左下角）".into());
    }
    let (lon0, lat0) = georef.unproject(min).ok_or_else(|| "视图左下角超出投影范围".to_string())?;
    let (lon1, lat1) = georef.unproject(max).ok_or_else(|| "视图右上角超出投影范围".to_string())?;
    let (lon_min, lon_max) = (lon0.min(lon1), lon0.max(lon1));
    let (lat_min, lat_max) = (lat0.min(lat1), lat0.max(lat1));
    let count_at = |z: u32| -> Option<usize> {
        let (x0, y0) = lonlat_to_tile(lon_min, lat_max, z)?;
        let (x1, y1) = lonlat_to_tile(lon_max, lat_min, z)?;
        let (dx, dy) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
        Some((dx as usize + 1).saturating_mul(dy as usize + 1))
    };
    let z = match zoom {
        Some(z) => z.min(layer.max_zoom),
        None => (0..=layer.max_zoom).rev().find(|&z| count_at(z).is_some_and(|c| c <= MAX_TILES)).unwrap_or(0),
    };
    let (x0, y0) = lonlat_to_tile(lon_min, lat_max, z).ok_or_else(|| "视图超出瓦片范围".to_string())?;
    let (x1, y1) = lonlat_to_tile(lon_max, lat_min, z).ok_or_else(|| "视图超出瓦片范围".to_string())?;
    let mut out = Vec::new();
    let mut x = x0;
    while x <= x1 {
        let mut y = y0;
        while y <= y1 {
            out.push(TileRef { z, x, y, url: layer.tile_url(z, x, y), cache_key: layer.cache_key(z, x, y) });
            if out.len() >= MAX_TILES {
                return Ok(out);
            }
            y = y.saturating_add(1);
        }
        x = x.saturating_add(1);
    }
    Ok(out)
}

/// The WGS-84 bounds of one Web-Mercator tile: `(lon_min, lat_min, lon_max,
/// lat_max)` in degrees. The inverse of [`lonlat_to_tile`]; `None` for `z > 30`.
pub fn tile_bounds_lonlat(z: u32, x: u32, y: u32) -> Option<(f64, f64, f64, f64)> {
    if z > 30 {
        return None;
    }
    let n = 2f64.powi(i32::try_from(z).ok()?);
    let lon = |tx: u32| f64::from(tx) / n * 360.0 - 180.0;
    let lat = |ty: u32| {
        let t = std::f64::consts::PI * (1.0 - 2.0 * f64::from(ty) / n);
        t.sinh().atan().to_degrees()
    };
    let (lon_min, lon_max) = (lon(x), lon(x.saturating_add(1)));
    let (lat_min, lat_max) = (lat(y.saturating_add(1)), lat(y));
    if lon_min.is_finite() && lon_max.is_finite() && lat_min.is_finite() && lat_max.is_finite() {
        Some((lon_min, lat_min, lon_max, lat_max))
    } else {
        None
    }
}

/// One tile placed in CAD coordinates: its four corners in the drawing's §3
/// spherical-TM metres, in NW, NE, SE, SW order (the same projection the vector
/// entities use, so tiles and vectors align, §9.3). The quad is very slightly
/// non-rectangular (the TM projection is not axis-aligned with the tile grid);
/// renderers fit an affine map through NW/NE/SW — the residual at SE is sub-metre
/// at tile scale and shrinks with zoom.
#[derive(Clone, Debug, PartialEq)]
pub struct TilePlacement {
    pub tile: TileRef,
    pub corners: [Vec2; 4],
}

/// The CAD corners of one tile: its WGS-84 corners through the georef (§9.3).
/// `None` when a corner fails to project.
pub fn tile_corners(tile: &TileRef, georef: &GeoRef) -> Option<[Vec2; 4]> {
    let (lon_min, lat_min, lon_max, lat_max) = tile_bounds_lonlat(tile.z, tile.x, tile.y)?;
    let nw = georef.project(lon_min, lat_max)?;
    let ne = georef.project(lon_max, lat_max)?;
    let se = georef.project(lon_max, lat_min)?;
    let sw = georef.project(lon_min, lat_min)?;
    Some([nw, ne, se, sw])
}

/// The tiles a CAD view needs, each with its CAD corners ([`tiles_for_view`] +
/// [`tile_corners`]). Tiles whose corners fail to project are skipped (a missing
/// tile draws blank, never an error, §9.4).
pub fn tile_placements(layer: &TileLayer, min: Vec2, max: Vec2, georef: &GeoRef, zoom: Option<u32>) -> Result<Vec<TilePlacement>, String> {
    let mut out = Vec::new();
    for tile in tiles_for_view(layer, min, max, georef, zoom)? {
        if let Some(corners) = tile_corners(&tile, georef) {
            out.push(TilePlacement { tile, corners });
        }
    }
    Ok(out)
}

/// Delay before retrying a failed tile fetch, in milliseconds: exponential
/// back-off, 1 s → 2 s → 4 s …, capped at 30 s (§9.4, at most 3 retries). A
/// zero attempt count means "no failure yet" and needs no delay.
pub fn tile_backoff_delay_ms(attempt: u32) -> u64 {
    if attempt == 0 {
        return 0;
    }
    let shift = attempt.saturating_sub(1).min(5);
    (1000u64 << shift).min(30_000)
}

/// The cache-relative path of one tile: `<fnv1a(source url template)>/<z>/<x>/<y>.<ext>`
/// (the same scheme `cadcraft-io`'s tile disk cache uses, so a cache file written
/// by one path is found by the other). Pure string math; no file access.
pub fn tile_cache_rel_path(layer: &TileLayer, tile: &TileRef) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in layer.url_template.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}/{}", tile.cache_key)
}

/// The suggested display colour of a link layer (§4.5, display only, never semantic).
pub fn layer_color(layer: &str) -> Option<Color> {
    match layer {
        LAYER_MARKER => Some(Color::Index(7)),
        LAYER_TRACK => Some(Color::Index(3)),
        LAYER_LABEL => Some(Color::Index(2)),
        LAYER_PHOTO => Some(Color::Index(6)),
        _ => None,
    }
}

/// The OVIMAP xdata of one entity, parsed from its `1001 OVIMAP` run (§4.4).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OvimapXdata {
    /// Stable id (the sync primary key).
    pub id: String,
    /// Display name.
    pub name: String,
    /// `marker` | `track` | `label` | `photo`.
    pub kind: String,
    /// Ovimap type id (or `"none"` for CadCraft-native features).
    pub type_: String,
}

/// A feature's private extension area: everything xdata cannot carry (see the module
/// docs). In-memory only; DXF save keeps just the vertex ids (§4.6).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OvimapExt {
    /// Note text (`MapLabel.note`).
    pub note: String,
    /// One-character symbol (e.g. `"电"`), display reference only.
    pub symbol: String,
    /// Photo file names only (basenames).
    pub photos: Vec<String>,
    /// Creation time (ISO-8601 UTC, or `None` = null). Reference only, never a key (§2.7).
    pub created_at: Option<String>,
    /// Update time (ISO-8601 UTC, or `None` = null). Reference only (§2.7).
    pub updated_at: Option<String>,
    /// Passthrough bag (§2.6): unknown properties, preserved verbatim.
    pub passthrough: Value,
    /// Known ovimap-native optional fields (§2.3.2: `seq`, `line_group`, `distanceM`,
    /// `segKind`, `segCable`, …, `closed`, `reno`), preserved verbatim.
    pub extra: Value,
    /// Track vertex objects (`vertices[]`), raw JSON, one per coordinate.
    pub vertices: Vec<Value>,
}

/// Whether a dictionary key is legal for a `VERTICES_<lineGroupId>` entry (R2): a
/// non-empty key without whitespace, control characters or the symbol-name characters
/// `<>/:\";?*|=,`. An illegal key must fail the export, never be silently renamed
/// (renaming breaks the key ↔ track link).
pub fn valid_dict_key(key: &str) -> bool {
    !key.is_empty()
        && key.chars().count() <= 255
        && key.chars().all(|c| !c.is_control() && !c.is_whitespace() && !matches!(c, '<' | '>' | '/' | ':' | '"' | ';' | '?' | '*' | '|' | '=' | ','))
}

/// Cut `s` to at most [`MAX_XDATA_STR_CHARS`] characters (§4.4: 255 characters).
fn cut_255(s: &str) -> &str {
    if s.chars().count() <= MAX_XDATA_STR_CHARS {
        return s;
    }
    let end = s.char_indices().nth(MAX_XDATA_STR_CHARS).map(|(i, _)| i).unwrap_or(s.len());
    &s[..end]
}

/// Build the OVIMAP xdata groups (§4.4, order fixed): `1001 OVIMAP`, then `1000`
/// ovimap_id / name / kind / type. Over-long strings are truncated (the caller warns;
/// GeoJSON is the authoritative path for full names).
pub fn ovimap_xdata(id: &str, name: &str, kind: &str, type_: &str) -> Vec<RawTag> {
    let mut tags = vec![RawTag { code: 1001, value: OVIMAP_APP.to_string() }];
    for v in [id, name, kind, type_] {
        tags.push(RawTag { code: 1000, value: cut_255(v).to_string() });
    }
    tags
}

/// Parse the OVIMAP xdata of an entity: the four `1000` groups after `1001 OVIMAP`,
/// in order. Returns `None` when the entity carries no (or malformed) OVIMAP xdata.
pub fn parse_ovimap_xdata(tags: &[RawTag]) -> Option<OvimapXdata> {
    let start = tags.iter().position(|t| t.code == 1001 && t.value.trim().eq_ignore_ascii_case(OVIMAP_APP))?;
    let rest = tags.get(start + 1..).unwrap_or(&[]);
    let vals: Vec<&str> = rest.iter().take_while(|t| t.code == 1000).map(|t| t.value.as_str()).collect();
    if vals.len() < 4 {
        return None;
    }
    Some(OvimapXdata { id: vals[0].to_string(), name: vals[1].to_string(), kind: vals[2].to_string(), type_: vals[3].to_string() })
}

/// A fresh UUID v4 (for CadCraft-native identities, §5.4, and regenerated vertex ids,
/// §4.6). Best-effort randomness from time + process id + an atomic counter through a
/// xorshift generator: unique in practice for this use, format-correct (version/variant
/// bits set). No `rand`/`uuid` dependency in this crate.
pub fn new_uuid_v4() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    let mut x = now
        ^ (std::process::id() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ COUNTER.fetch_add(1, Ordering::Relaxed).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let mut next = || {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (x >> 32) as u64
    };
    let mut b = [0u8; 16];
    for chunk in b.chunks_mut(4) {
        let v = next() as u32;
        chunk.copy_from_slice(&v.to_be_bytes());
    }
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// The current UTC time as ISO-8601 with milliseconds (`2026-10-14T05:00:00.000Z`),
/// for `ovimap_meta.exported_at` (no `chrono` dependency in this crate).
pub fn utc_now_iso() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z")
}

/// The content of a photo hint TEXT (§5.2): `PHOTO: <basename1>, <basename2>`.
pub fn photo_text_contents(photos: &[String]) -> String {
    format!("PHOTO: {}", photos.join(", "))
}

/// Parse a photo hint TEXT back into basenames: strips the `PHOTO: ` prefix and a
/// trailing `（文件缺失）` marker (§5.5). Returns `None` for non-hint texts.
pub fn parse_photo_text(contents: &str) -> Option<Vec<String>> {
    let rest = contents.strip_prefix("PHOTO:")?.trim();
    let rest = rest.strip_suffix("（文件缺失）").map(str::trim).unwrap_or(rest);
    if rest.is_empty() {
        return Some(Vec::new());
    }
    Some(rest.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect())
}

/// Whether a `photos[]` entry is a legal basename: non-empty, no `/`, `\` or `..`.
pub fn is_legal_photo_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && !name.contains('\\') && !name.contains("..")
}

/// Clean a `photos[]` value: keep legal basenames, drop the rest. Dropped entries are
/// reported through `warnings` (never a whole-file rejection, §2.5.3).
fn clean_photos(v: Option<&Value>, feat_no: usize, warnings: &mut Vec<String>) -> Vec<String> {
    let Some(arr) = v.and_then(Value::as_array) else { return Vec::new() };
    let mut out = Vec::new();
    for item in arr {
        match item.as_str() {
            Some(s) if is_legal_photo_name(s) => out.push(s.to_string()),
            Some(s) => warnings.push(format!("第 {feat_no} 个要素的 photos 条目 `{s}` 非法（须为纯文件名），已丢弃")),
            None => warnings.push(format!("第 {feat_no} 个要素的 photos 条目不是字符串，已丢弃")),
        }
    }
    out
}

/// What a GeoJSON import produced.
#[derive(Debug, Default)]
pub struct GeoJsonImport {
    /// The file's `ovimap_georef` (required by the schema).
    pub georef: GeoRef,
    /// The file's `ovimap_meta` object (carried back verbatim on export).
    pub meta: Value,
    /// The file's `ovimap_deletes` (effective only with `delete_mode == "sync"`).
    pub deletes: Vec<String>,
    /// `delete_mode` from `ovimap_meta` (`"none"` or `"sync"`).
    pub delete_mode: String,
    /// New entities: (common properties, geometry), grouped per feature in file order.
    pub entities: Vec<(Common, EntityKind)>,
    /// Per-feature extension areas, keyed by `ovimap_id`.
    pub ext: BTreeMap<String, OvimapExt>,
    /// Non-fatal import notes (dropped photo entries, generated vertex ids, …).
    pub warnings: Vec<String>,
    /// Features imported, by kind.
    pub markers: usize,
    /// Track features imported.
    pub tracks: usize,
    /// Label features imported.
    pub labels: usize,
    /// Photo-pin features imported.
    pub photos: usize,
}

fn prop_str(props: &serde_json::Map<String, Value>, key: &str) -> String {
    props.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn parse_lonlat(v: &Value, feat: usize) -> Result<(f64, f64), String> {
    let arr = v.as_array().ok_or_else(|| format!("第 {feat} 个要素的坐标不是数组"))?;
    if arr.len() < 2 {
        return Err(format!("第 {feat} 个要素的坐标至少需要 [经度, 纬度]"));
    }
    let lon = arr[0].as_f64().filter(|x| x.is_finite()).ok_or_else(|| format!("第 {feat} 个要素的经度不是数字"))?;
    let lat = arr[1].as_f64().filter(|x| x.is_finite()).ok_or_else(|| format!("第 {feat} 个要素的纬度不是数字"))?;
    if !(-180.0..=180.0).contains(&lon) || !(-90.0..=90.0).contains(&lat) {
        return Err(format!("第 {feat} 个要素的坐标超出范围（经度 ±180、纬度 ±90）：{lon}, {lat}"));
    }
    Ok((lon, lat))
}

fn parse_coord_list(v: &Value, feat: usize) -> Result<Vec<(f64, f64)>, String> {
    let arr = v.as_array().ok_or_else(|| format!("第 {feat} 个要素的坐标不是数组"))?;
    if arr.len() > MAX_POINTS_PER_FEATURE {
        return Err(format!("第 {feat} 个要素的坐标点数超过上限 {MAX_POINTS_PER_FEATURE}"));
    }
    arr.iter().map(|p| parse_lonlat(p, feat)).collect()
}

fn project_many(coords: &[(f64, f64)], georef: &GeoRef, feat: usize) -> Result<Vec<Vec2>, String> {
    coords.iter().map(|&(lon, lat)| georef.project(lon, lat).ok_or_else(|| format!("第 {feat} 个要素的坐标 ({lon}, {lat}) 投影失败"))).collect()
}

/// Drop consecutive duplicate points (a zero-length segment is degenerate in CAD). A
/// closed ring's repeated first point at the end survives: it is not consecutive with
/// the first point.
fn dedupe(pts: Vec<Vec2>) -> Vec<Vec2> {
    let mut out: Vec<Vec2> = Vec::with_capacity(pts.len());
    for p in pts {
        if !out.last().is_some_and(|l| l.near(p, 1e-12)) {
            out.push(p);
        }
    }
    out
}

fn common_for(layer: &str, id: &str, name: &str, kind: &str, type_: &str) -> Common {
    Common { layer: layer.to_string(), xdata: ovimap_xdata(id, name, kind, type_), ..Common::default() }
}

fn text_at(at: Vec2, value: &str, layer: &str, id: &str, name: &str, kind: &str, type_: &str) -> (Common, EntityKind) {
    (
        common_for(layer, id, name, kind, type_),
        EntityKind::Text(Text {
            insert: Vec3::new(at.x, at.y, 0.0),
            align_pt: None,
            height: TEXT_HEIGHT,
            value: value.to_string(),
            rotation: 0.0,
            width_factor: 1.0,
            oblique: 0.0,
            style: "Standard".into(),
            halign: crate::HAlign::Left,
            valign: crate::VAlign::Baseline,
        }),
    )
}

/// Known ovimap-native optional properties (§2.3.2) + `closed`: collected verbatim into
/// the feature's `extra` bag. Everything else unknown goes to `passthrough` (§2.6).
const KNOWN_PROPS: &[&str] = &[
    "ovimap_id",
    "name",
    "note",
    "kind",
    "type",
    "symbol",
    "created_at",
    "updated_at",
    "photos",
    "passthrough",
    "seq",
    "line_group",
    "distanceM",
    "distLabel",
    "segKind",
    "segCable",
    "slackM",
    "holes",
    "usedHoles",
    "splitterRatio",
    "topoParentId",
    "cableSpec",
    "cableCores",
    "reno",
    "closed",
    "vertices",
    "timestamps",
];
const EXTRA_PROPS: &[&str] = &[
    "seq",
    "line_group",
    "distanceM",
    "distLabel",
    "segKind",
    "segCable",
    "slackM",
    "holes",
    "usedHoles",
    "splitterRatio",
    "topoParentId",
    "cableSpec",
    "cableCores",
    "reno",
];

/// Parse GeoJSON text (ovimap-link §2) into CAD entities. Fail-closed: any schema
/// violation rejects the whole file with a Chinese error naming the offending feature.
pub fn geojson_to_entities(text: &str) -> Result<GeoJsonImport, String> {
    let doc: Value = serde_json::from_str(text).map_err(|e| format!("GeoJSON 解析失败：{e}"))?;
    if doc.get("type").and_then(Value::as_str) != Some("FeatureCollection") {
        return Err("GeoJSON 顶层 type 必须是 FeatureCollection".into());
    }
    let meta = doc.get("ovimap_meta").cloned().unwrap_or(Value::Null);
    let meta_obj = meta.as_object().ok_or_else(|| "缺少 ovimap_meta（须含 schema_version）".to_string())?;
    let schema = meta_obj.get("schema_version").and_then(Value::as_str).unwrap_or("");
    let major: Option<u32> = schema.split('.').next().and_then(|m| m.parse().ok());
    match major {
        Some(1) => {}
        Some(m) => return Err(format!("不支持的 Schema 大版本 {m}（需要 1.x），请升级 CadCraft")),
        None => return Err(format!("ovimap_meta.schema_version 无效：`{schema}`（须为 \"1.0\" 形式）")),
    }
    let delete_mode = meta_obj.get("delete_mode").and_then(Value::as_str).unwrap_or("none").to_string();
    if delete_mode != "none" && delete_mode != "sync" {
        return Err(format!("ovimap_meta.delete_mode 必须是 none 或 sync，得到 `{delete_mode}`"));
    }
    let deletes = match doc.get("ovimap_deletes") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => {
            let mut out = Vec::new();
            for d in a {
                let Some(s) = d.as_str() else { return Err("ovimap_deletes 元素必须是字符串".into()) };
                out.push(s.to_string());
            }
            out
        }
        Some(_) => return Err("ovimap_deletes 必须是字符串数组".into()),
    };
    let georef = match doc.get("ovimap_georef") {
        Some(g) => GeoRef::from_json(g)?,
        None => return Err("缺少 ovimap_georef（必填，导入方必须以文件内的地理参照为准）".into()),
    };
    let features = doc.get("features").and_then(Value::as_array).ok_or_else(|| "GeoJSON 缺少 features 数组".to_string())?;
    if features.len() > MAX_FEATURES {
        return Err(format!("要素数量超过上限 {MAX_FEATURES}"));
    }
    let mut out = GeoJsonImport { georef, meta: Value::Object(meta_obj.clone()), deletes, delete_mode, ..GeoJsonImport::default() };
    let mut seen_ids: BTreeMap<String, usize> = BTreeMap::new();
    let mut total_points = 0usize;
    let mut warned_timestamps = false;
    for (feat_idx, feat) in features.iter().enumerate() {
        let feat_no = feat_idx + 1;
        let props = feat.get("properties").and_then(Value::as_object).ok_or_else(|| format!("第 {feat_no} 个要素缺少 properties 对象"))?;
        let id = props.get("ovimap_id").and_then(Value::as_str).unwrap_or("");
        if id.is_empty() {
            return Err(format!("第 {feat_no} 个要素缺少 ovimap_id（必填，同步主键）"));
        }
        if let Some(prev) = seen_ids.insert(id.to_string(), feat_no) {
            return Err(format!("ovimap_id `{id}` 重复（第 {prev} 个与第 {feat_no} 个要素）：id 是同步主键，同一文件内不得重合"));
        }
        let name = match props.get("name") {
            Some(Value::String(s)) => s.clone(),
            _ => return Err(format!("第 {feat_no} 个要素缺少 name（必填，可为空串）")),
        };
        let note = prop_str(props, "note");
        let kind = props.get("kind").and_then(Value::as_str).unwrap_or("");
        if !["marker", "track", "label", "photo"].contains(&kind) {
            return Err(format!("第 {feat_no} 个要素的 kind 必须是 marker/track/label/photo 之一，得到 `{kind}`"));
        }
        let type_ = match props.get("type") {
            Some(Value::String(s)) => s.clone(),
            _ => return Err(format!("第 {feat_no} 个要素缺少 type（必填）")),
        };
        let symbol = prop_str(props, "symbol");
        let mut opt_time = |key: &str| -> Option<String> {
            match props.get(key) {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.clone()),
                Some(_) => {
                    out.warnings.push(format!("第 {feat_no} 个要素的 {key} 不是字符串，已忽略"));
                    None
                }
            }
        };
        let created_at = opt_time("created_at");
        let updated_at = opt_time("updated_at");
        let photos = clean_photos(props.get("photos"), feat_no, &mut out.warnings);
        let mut passthrough = props.get("passthrough").cloned().unwrap_or(json!({}));
        if !passthrough.is_object() {
            out.warnings.push(format!("第 {feat_no} 个要素的 passthrough 不是对象，已忽略"));
            passthrough = json!({});
        }
        let mut extra = serde_json::Map::new();
        for key in EXTRA_PROPS {
            if let Some(v) = props.get(*key) {
                extra.insert((*key).to_string(), v.clone());
            }
        }
        for (key, v) in props {
            if !KNOWN_PROPS.contains(&key.as_str()) && !passthrough.as_object().is_some_and(|m| m.contains_key(key)) {
                passthrough.as_object_mut().map(|m| m.insert(key.clone(), v.clone()));
            }
        }
        let geom = feat.get("geometry").ok_or_else(|| format!("第 {feat_no} 个要素缺少 geometry"))?;
        let gtype = geom.get("type").and_then(Value::as_str).unwrap_or("");
        let coords = geom.get("coordinates").ok_or_else(|| format!("第 {feat_no} 个要素缺少 coordinates"))?;
        match (kind, gtype) {
            ("marker" | "photo", "Point") | ("label", "Point") => {
                let (lon, lat) = parse_lonlat(coords, feat_no)?;
                total_points += 1;
                let at = georef.project(lon, lat).ok_or_else(|| format!("第 {feat_no} 个要素的坐标 ({lon}, {lat}) 投影失败"))?;
                let ext = OvimapExt {
                    note: note.clone(),
                    symbol: symbol.clone(),
                    photos: photos.clone(),
                    created_at: created_at.clone(),
                    updated_at: updated_at.clone(),
                    passthrough: passthrough.clone(),
                    extra: Value::Object(extra.clone()),
                    vertices: Vec::new(),
                };
                out.ext.insert(id.to_string(), ext);
                match kind {
                    "marker" => {
                        out.markers += 1;
                        out.entities.push((
                            common_for(LAYER_MARKER, id, &name, kind, &type_),
                            EntityKind::Point(Point { p: Vec3::new(at.x, at.y, 0.0), angle: 0.0 }),
                        ));
                        let label = if !name.is_empty() { name.clone() } else { symbol.clone() };
                        if !label.is_empty() {
                            out.entities.push(text_at(Vec2::new(at.x, at.y + MARKER_TEXT_DY), &label, LAYER_MARKER, id, &name, kind, &type_));
                        }
                        if !photos.is_empty() {
                            out.entities.push(text_at(
                                Vec2::new(at.x, at.y + PHOTO_TEXT_DY),
                                &photo_text_contents(&photos),
                                LAYER_PHOTO,
                                id,
                                &name,
                                kind,
                                &type_,
                            ));
                        }
                    }
                    "photo" => {
                        out.photos += 1;
                        out.entities.push((
                            common_for(LAYER_PHOTO, id, &name, kind, &type_),
                            EntityKind::Point(Point { p: Vec3::new(at.x, at.y, 0.0), angle: 0.0 }),
                        ));
                        if !photos.is_empty() {
                            out.entities.push(text_at(at, &photo_text_contents(&photos), LAYER_PHOTO, id, &name, kind, &type_));
                        } else {
                            out.warnings.push(format!("第 {feat_no} 个要素是照片钉但 photos 为空"));
                        }
                    }
                    _ => {
                        // label
                        if name.is_empty() {
                            out.warnings.push(format!("第 {feat_no} 个要素是标签但 name 为空，已跳过该要素"));
                            continue;
                        }
                        out.labels += 1;
                        if name.contains('\n') {
                            out.entities.push((
                                common_for(LAYER_LABEL, id, &name, kind, &type_),
                                EntityKind::MText(MText {
                                    insert: Vec3::new(at.x, at.y, 0.0),
                                    height: TEXT_HEIGHT,
                                    width: 0.0,
                                    attach: 1,
                                    rotation: 0.0,
                                    style: "Standard".into(),
                                    contents: name.clone(),
                                    line_spacing: 1.0,
                                    line_spacing_exact: false,
                                    columns: None,
                                }),
                            ));
                        } else {
                            out.entities.push(text_at(at, &name, LAYER_LABEL, id, &name, kind, &type_));
                        }
                    }
                }
            }
            ("track", "LineString") => {
                let list = parse_coord_list(coords, feat_no)?;
                total_points += list.len();
                if list.len() < 2 {
                    return Err(format!("第 {feat_no} 个要素的轨迹至少需要 2 个坐标点"));
                }
                let closed = props.get("closed").and_then(Value::as_bool).unwrap_or(false);
                if closed && (list.first() != list.last()) {
                    return Err(format!("第 {feat_no} 个要素的 closed 为 true，但坐标首尾不重复"));
                }
                if total_points > MAX_TOTAL_POINTS {
                    return Err(format!("导入总点数超过上限 {MAX_TOTAL_POINTS}"));
                }
                let pts = dedupe(project_many(&list, &georef, feat_no)?);
                if closed && pts.len() < 4 {
                    return Err(format!("第 {feat_no} 个要素的闭合轨迹至少需要 3 个不同顶点 + 首尾重复"));
                }
                if !closed && pts.len() < 2 {
                    return Err(format!("第 {feat_no} 个要素的轨迹去重后不足 2 个不同坐标点"));
                }
                // Vertex identities (§2.5.2): `vertices[]` must align with the
                // coordinates; absent → fresh ids + a warning (degraded, §4.6-style).
                let mut vertices: Vec<Value> = Vec::new();
                match props.get("vertices") {
                    None | Some(Value::Null) => {
                        out.warnings.push(format!("轨迹 `{id}` 未提供 vertices，已为顶点生成新 ID"));
                        for _ in 0..list.len() {
                            vertices.push(json!({ "ovimap_id": new_uuid_v4() }));
                        }
                    }
                    Some(Value::Array(a)) => {
                        if a.len() != list.len() {
                            return Err(format!("轨迹 `{id}` 的 vertices 长度 {} 与坐标数 {} 不一致", a.len(), list.len()));
                        }
                        for v in a {
                            let mut v = v.clone();
                            let vid = v.get("ovimap_id").and_then(Value::as_str).unwrap_or("");
                            if vid.is_empty() {
                                let fresh = new_uuid_v4();
                                out.warnings.push(format!("轨迹 `{id}` 的某个顶点缺少 ovimap_id，已生成 {fresh}"));
                                v.as_object_mut().map(|m| m.insert("ovimap_id".into(), json!(fresh)));
                            }
                            if let Some(arr) = v.get("photos").and_then(Value::as_array) {
                                let cleaned: Vec<String> =
                                    arr.iter().filter_map(Value::as_str).filter(|s| is_legal_photo_name(s)).map(str::to_string).collect();
                                if cleaned.len() != arr.len() {
                                    out.warnings.push(format!("轨迹 `{id}` 的某个顶点有非法 photos 条目，已丢弃"));
                                }
                                v.as_object_mut().map(|m| m.insert("photos".into(), json!(cleaned)));
                            }
                            vertices.push(v);
                        }
                    }
                    Some(_) => return Err(format!("轨迹 `{id}` 的 vertices 必须是对象数组")),
                }
                if let Some(ts) = props.get("timestamps")
                    && !ts.is_null()
                {
                    match ts.as_array() {
                        Some(a) if a.len() == list.len() => {
                            if !warned_timestamps {
                                out.warnings.push("timestamps 数组已校验，但 CadCraft 不存储逐点时间戳，导出时写 null".into());
                                warned_timestamps = true;
                            }
                        }
                        _ => return Err(format!("轨迹 `{id}` 的 timestamps 长度与坐标数不一致（须等长或为 null）")),
                    }
                }
                if let Some(lg) = props.get("line_group").and_then(Value::as_str)
                    && lg != id
                {
                    return Err(format!("轨迹 `{id}` 的 line_group `{lg}` 与 ovimap_id 不一致"));
                }
                extra.insert("line_group".to_string(), json!(id));
                extra.insert("closed".to_string(), json!(closed));
                let ext = OvimapExt {
                    note: note.clone(),
                    symbol: symbol.clone(),
                    photos: photos.clone(),
                    created_at: created_at.clone(),
                    updated_at: updated_at.clone(),
                    passthrough: passthrough.clone(),
                    extra: Value::Object(extra.clone()),
                    vertices: vertices.clone(),
                };
                out.ext.insert(id.to_string(), ext);
                out.tracks += 1;
                let poly_vertices = pts.iter().map(|p| cadcraft_geom::PolyVertex::new(*p)).collect();
                out.entities.push((
                    common_for(LAYER_TRACK, id, &name, kind, &type_),
                    EntityKind::LwPolyline(LwPolyline { vertices: poly_vertices, closed, const_width: 0.0, elevation: 0.0, plinegen: false }),
                ));
                // Per-vertex photo hints (§5.2): one TEXT below each vertex with photos.
                for (i, v) in vertices.iter().enumerate() {
                    let vphotos = clean_photos(v.get("photos"), feat_no, &mut out.warnings);
                    if vphotos.is_empty() {
                        continue;
                    }
                    let Some(at) = pts.get(i) else { continue };
                    out.entities.push(text_at(
                        Vec2::new(at.x, at.y + PHOTO_TEXT_DY),
                        &photo_text_contents(&vphotos),
                        LAYER_PHOTO,
                        id,
                        &name,
                        kind,
                        &type_,
                    ));
                }
            }
            (k, g) => {
                return Err(format!(
                    "第 {feat_no} 个要素 kind=`{k}` 与几何类型 `{g}` 不匹配（marker/label/photo 须为 Point，track 须为 LineString）"
                ));
            }
        }
    }
    if total_points > MAX_TOTAL_POINTS {
        return Err(format!("导入总点数超过上限 {MAX_TOTAL_POINTS}"));
    }
    Ok(out)
}

/// Rebuild per-feature extension areas from DXF entities: the file's `ovimap_ext` map
/// holds only vertex ids (DXF drops the rest, §4.6), and photo lists are recovered from
/// `PHOTO: …` hint texts by position (a hint below a vertex belongs to that vertex; a
/// hint below a POINT belongs to the feature). `vertices` maps track id → vertex ids.
pub fn rebuild_ovimap_ext(items: &[(Common, EntityKind)], vertices: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, OvimapExt> {
    let mut ext: BTreeMap<String, OvimapExt> = BTreeMap::new();
    let mut points: BTreeMap<String, Vec2> = BTreeMap::new();
    let mut polylines: BTreeMap<String, Vec<Vec2>> = BTreeMap::new();
    for (common, kind) in items {
        let Some(xd) = parse_ovimap_xdata(&common.xdata) else { continue };
        ext.entry(xd.id.clone()).or_default();
        match kind {
            EntityKind::Point(p) => {
                points.insert(xd.id, p.p.xy());
            }
            EntityKind::LwPolyline(pl) => {
                polylines.insert(xd.id, pl.vertices.iter().map(|v| v.p).collect());
            }
            _ => {}
        }
    }
    for (id, vids) in vertices {
        let e = ext.entry(id.clone()).or_default();
        e.vertices = vids.iter().map(|v| json!({ "ovimap_id": v })).collect();
    }
    for (common, kind) in items {
        let EntityKind::Text(t) = kind else { continue };
        if !common.layer.eq_ignore_ascii_case(LAYER_PHOTO) {
            continue;
        }
        let Some(xd) = parse_ovimap_xdata(&common.xdata) else { continue };
        let Some(photos) = parse_photo_text(&t.value) else { continue };
        if photos.is_empty() {
            continue;
        }
        let at = t.insert.xy();
        // A hint below a polyline vertex belongs to that vertex.
        if let Some(verts) = polylines.get(&xd.id) {
            let mut placed = false;
            for (i, v) in verts.iter().enumerate() {
                if (at.x - v.x).abs() < 1e-9 && (at.y - (v.y + PHOTO_TEXT_DY)).abs() < 1e-9 {
                    if let Some(entry) = ext.get_mut(&xd.id).and_then(|e| e.vertices.get_mut(i)) {
                        entry.as_object_mut().map(|m| m.insert("photos".into(), json!(photos)));
                    }
                    placed = true;
                    break;
                }
            }
            if placed {
                continue;
            }
        }
        // Otherwise it belongs to the feature (a marker's or photo pin's own photos).
        if let Some(e) = ext.get_mut(&xd.id)
            && e.photos.is_empty()
        {
            e.photos = photos;
        }
        let _ = points.get(&xd.id);
    }
    ext
}

/// The plain text of an MTEXT's contents: `\P` becomes a newline, font/format codes are
/// stripped. (CadCraft stores MTEXT contents with DXF inline codes.)
pub fn mtext_plain(contents: &str) -> String {
    let mut out = String::with_capacity(contents.len());
    let mut chars = contents.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('P') => {
                    chars.next();
                    out.push('\n');
                }
                Some('f') | Some('F') => {
                    // \f…; font code: skip to the semicolon.
                    chars.next();
                    for c2 in chars.by_ref() {
                        if c2 == ';' {
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
        } else if c == '{' || c == '}' {
            // Formatting group braces: dropped.
        } else {
            out.push(c);
        }
    }
    out
}

/// Serialize entities carrying OVIMAP xdata back to GeoJSON text (projected back to
/// WGS-84 lon/lat, §3.3). Entities without OVIMAP xdata are skipped. A marker's label
/// TEXT (same `ovimap_id` as a POINT on `OVIMAP_MARKER`) is not emitted twice; a
/// `PHOTO: …` hint TEXT is not a feature either — photos come from `ext`. `meta` is
/// the drawing's `ovimap_meta` (its `exporter`/`exported_at` are refreshed, the rest
/// carried back). Errors are in Chinese.
pub fn entities_to_geojson<'a>(
    items: impl IntoIterator<Item = (&'a Common, &'a EntityKind)>,
    georef: &GeoRef,
    meta: &Value,
    ext: &BTreeMap<String, OvimapExt>,
) -> Result<String, String> {
    let items: Vec<(&Common, &EntityKind)> = items.into_iter().collect();
    // Group entities by ovimap_id, in first-seen order.
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<(&Common, &EntityKind)>> = BTreeMap::new();
    for (common, kind) in &items {
        let Some(xd) = parse_ovimap_xdata(&common.xdata) else { continue };
        if !groups.contains_key(&xd.id) {
            order.push(xd.id.clone());
        }
        groups.entry(xd.id.clone()).or_default().push((*common, *kind));
    }
    let mut features: Vec<Value> = Vec::new();
    for id in &order {
        let group = groups.get(id).cloned().unwrap_or_default();
        let Some(first_xd) = group.first().and_then(|(c, _)| parse_ovimap_xdata(&c.xdata)) else { continue };
        for (common, _) in &group {
            let Some(xd) = parse_ovimap_xdata(&common.xdata) else { continue };
            if xd.kind != first_xd.kind {
                return Err(format!("ovimap_id `{id}` 在图纸里对应了不同的 kind（{}/{}），请先修复", first_xd.kind, xd.kind));
            }
        }
        let e = ext.get(id).cloned().unwrap_or_default();
        let kind = first_xd.kind.clone();
        let type_ = first_xd.type_.clone();
        // Effective name: a paired label TEXT's content wins over the xdata name
        // (the user may have edited the text in CAD, §5.3).
        let mut name = first_xd.name.clone();
        let mut point: Option<Vec2> = None;
        let mut polyline: Option<&LwPolyline> = None;
        let mut label_pos: Option<Vec2> = None;
        for (common, entity_kind) in &group {
            match entity_kind {
                EntityKind::Point(p) => point = Some(p.p.xy()),
                EntityKind::LwPolyline(pl) => polyline = Some(pl),
                EntityKind::Text(t) => {
                    if common.layer.eq_ignore_ascii_case(LAYER_PHOTO) {
                        continue; // a PHOTO hint, not a name
                    }
                    if common.layer.eq_ignore_ascii_case(LAYER_MARKER) || common.layer.eq_ignore_ascii_case(LAYER_LABEL) {
                        if !t.value.is_empty() {
                            name.clone_from(&t.value);
                        }
                        label_pos = Some(t.insert.xy());
                    }
                }
                EntityKind::MText(t) if common.layer.eq_ignore_ascii_case(LAYER_LABEL) => {
                    let plain = mtext_plain(&t.contents);
                    if !plain.is_empty() {
                        name = plain;
                    }
                    label_pos = Some(t.insert.xy());
                }
                _ => {}
            }
        }
        let mut props = serde_json::Map::new();
        props.insert("ovimap_id".into(), json!(id));
        props.insert("name".into(), json!(name));
        props.insert("note".into(), json!(e.note));
        props.insert("kind".into(), json!(kind));
        props.insert("type".into(), json!(type_));
        if !e.symbol.is_empty() {
            props.insert("symbol".into(), json!(e.symbol));
        }
        props.insert("created_at".into(), e.created_at.clone().map(Value::String).unwrap_or(Value::Null));
        props.insert("updated_at".into(), e.updated_at.clone().map(Value::String).unwrap_or(Value::Null));
        props.insert("photos".into(), json!(e.photos));
        props.insert("passthrough".into(), e.passthrough.clone());
        if let Some(extra) = e.extra.as_object() {
            for (k, v) in extra {
                if !props.contains_key(k) {
                    props.insert(k.clone(), v.clone());
                }
            }
        }
        let feature =
            |geom: Value, props: serde_json::Map<String, Value>| -> Value { json!({ "type": "Feature", "properties": props, "geometry": geom }) };
        match kind.as_str() {
            "marker" | "photo" => {
                let at = point.or(label_pos).ok_or_else(|| format!("要素 `{id}` 没有坐标（缺少 POINT）"))?;
                let (lon, lat) = georef.unproject(at).ok_or_else(|| format!("要素 `{id}` 的坐标超出投影范围"))?;
                features.push(feature(json!({ "type": "Point", "coordinates": [lon, lat] }), props));
            }
            "label" => {
                let at = label_pos.or(point).ok_or_else(|| format!("标签 `{id}` 没有坐标（缺少 TEXT）"))?;
                let (lon, lat) = georef.unproject(at).ok_or_else(|| format!("标签 `{id}` 的坐标超出投影范围"))?;
                features.push(feature(json!({ "type": "Point", "coordinates": [lon, lat] }), props));
            }
            "track" => {
                let Some(pl) = polyline else { return Err(format!("轨迹 `{id}` 没有 LWPOLYLINE 几何")) };
                let mut coords: Vec<Value> = Vec::new();
                for v in &pl.vertices {
                    let (lon, lat) = georef.unproject(v.p).ok_or_else(|| format!("轨迹 `{id}` 的坐标超出投影范围"))?;
                    coords.push(json!([lon, lat]));
                }
                if pl.closed {
                    if coords.len() < 3 {
                        continue;
                    }
                    if coords.first() != coords.last() {
                        let first = coords.first().cloned().unwrap_or(Value::Null);
                        coords.push(first);
                    }
                } else if coords.len() < 2 {
                    continue;
                }
                props.insert("line_group".into(), json!(id));
                props.insert("closed".into(), json!(pl.closed));
                props.insert("timestamps".into(), Value::Null);
                let mut vertices = e.vertices.clone();
                if vertices.len() != coords.len() {
                    vertices = coords.iter().map(|_| json!({ "ovimap_id": new_uuid_v4() })).collect();
                }
                props.insert("vertices".into(), json!(vertices));
                features.push(feature(json!({ "type": "LineString", "coordinates": coords }), props));
            }
            other => return Err(format!("要素 `{id}` 的 kind `{other}` 无效")),
        }
    }
    let mut out_meta = meta.clone();
    if !out_meta.is_object() {
        out_meta = json!({});
    }
    if let Some(m) = out_meta.as_object_mut() {
        m.insert("schema_version".into(), json!(SCHEMA_VERSION));
        m.insert("exporter".into(), json!("cadcraft"));
        m.insert("exported_at".into(), json!(utc_now_iso()));
        m.insert("delete_mode".into(), json!("none"));
    }
    let out = json!({ "type": "FeatureCollection", "ovimap_georef": georef.to_json(), "ovimap_meta": out_meta, "ovimap_deletes": [], "features": features });
    serde_json::to_string_pretty(&out).map_err(|e| format!("GeoJSON 生成失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn georef() -> GeoRef {
        GeoRef::centred(32.1301, 114.0814)
    }

    #[test]
    fn xdata_is_exactly_four_positional_groups() {
        let tags = ovimap_xdata("id-1", "1号杆", "marker", "concrete");
        assert_eq!(tags.len(), 5);
        assert_eq!(tags[0].code, 1001);
        assert_eq!(tags[0].value, "OVIMAP");
        let vals: Vec<&str> = tags[1..].iter().map(|t| t.value.as_str()).collect();
        assert_eq!(vals, vec!["id-1", "1号杆", "marker", "concrete"]);
        let back = parse_ovimap_xdata(&tags).unwrap_or_default();
        assert_eq!(back.id, "id-1");
        assert_eq!(back.name, "1号杆");
        assert_eq!(back.kind, "marker");
        assert_eq!(back.type_, "concrete");
        // A truncated long name stays within 255 characters.
        let long = "杆".repeat(300);
        let tags = ovimap_xdata("x", &long, "marker", "concrete");
        assert_eq!(tags[2].value.chars().count(), 255);
        // A foreign app's xdata after ours does not confuse the parser.
        let mut mixed = ovimap_xdata("id-1", "n", "marker", "concrete");
        mixed.push(RawTag { code: 1001, value: "OTHER".into() });
        mixed.push(RawTag { code: 1000, value: "spoof".into() });
        assert_eq!(parse_ovimap_xdata(&mixed).unwrap_or_default().id, "id-1");
    }

    #[test]
    fn georef_json_and_xrecord_roundtrip() {
        let g = georef();
        let back = GeoRef::from_json(&g.to_json()).unwrap_or_default();
        assert_eq!(back, g);
        let back = GeoRef::from_xrecord_tags(&g.to_xrecord_tags()).unwrap_or_default();
        assert_eq!(back, g);
        // XRECORD order per §4.3: projection, lat, lon, cm, scale, radius, metre, WGS84, 1.0.
        let tags = g.to_xrecord_tags();
        assert_eq!(tags[0].value, "OVIMAP_SPHERICAL_TM");
        assert_eq!(tags[1].value, "32.1301");
        assert_eq!(tags[2].value, "114.0814");
        assert_eq!(tags[tags.len() - 1].value, "1.0");
        // Ellipsoidal files are refused, never silently computed as spherical.
        let mut bad = g.to_json();
        bad.as_object_mut().map(|m| m.insert("projection".into(), json!("OVIMAP_ELLIPSOIDAL_TM")));
        assert!(GeoRef::from_json(&bad).unwrap_err().contains("椭球"));
        // A central meridian different from the origin longitude is invalid.
        let mut bad = g.to_json();
        bad.as_object_mut().map(|m| m.insert("central_meridian".into(), json!(115.0)));
        assert!(GeoRef::from_json(&bad).is_err());
    }

    #[test]
    fn tile_math_and_osm_default() {
        assert_eq!(lonlat_to_tile(0.0, 0.0, 1), Some((1, 1)));
        assert_eq!(lonlat_to_tile(0.0, 0.0, 0), Some((0, 0)));
        let layer = TileLayer::default_osm();
        assert!(layer.validate().is_ok());
        assert_eq!(layer.tile_url(3, 5, 2), "https://tile.openstreetmap.org/3/5/2.png");
        assert_eq!(layer.cache_key(3, 5, 2), "3/5/2.png");
        assert_eq!(layer.attribution, "© OpenStreetMap 贡献者");
        assert_eq!(layer.max_zoom, 19);
        // A requested zoom above the cap is served from the cap, never requested higher.
        let tiles = tiles_for_view(&layer, Vec2::new(-100.0, -100.0), Vec2::new(100.0, 100.0), &georef(), Some(25)).unwrap_or_default();
        assert!(tiles.iter().all(|t| t.z <= 19));
        // A custom source without attribution is refused.
        let mut bad = TileLayer::default_osm();
        bad.url_template = "https://example.com/{z}/{x}/{y}.png".into();
        bad.attribution = String::new();
        assert!(bad.validate().unwrap_err().contains("署名"));
        // A template without placeholders is refused.
        let mut bad = TileLayer::default_osm();
        bad.url_template = "https://example.com/tiles.png".into();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn tile_placement_aligns_with_vectors() {
        // A tile's CAD corners, unprojected, are its WGS-84 tile bounds: the tile
        // grid and the vector projection agree (§9.3), so tiles and vectors align.
        let g = georef();
        let layer = TileLayer::default_osm();
        let (x, y) = lonlat_to_tile(114.0814, 32.1301, 14).unwrap_or((0, 0));
        let tile = TileRef { z: 14, x, y, url: layer.tile_url(14, x, y), cache_key: layer.cache_key(14, x, y) };
        let (lon_min, lat_min, lon_max, lat_max) = tile_bounds_lonlat(14, x, y).unwrap_or((0.0, 0.0, 0.0, 0.0));
        assert!(lon_min <= 114.0814 && 114.0814 <= lon_max);
        assert!(lat_min <= 32.1301 && 32.1301 <= lat_max);
        let corners = tile_corners(&tile, &g).unwrap_or([Vec2::ZERO; 4]);
        // NW corner unprojects to the tile's NW bound (within projection round-trip).
        let (lon, lat) = g.unproject(corners[0]).unwrap_or((0.0, 0.0));
        assert!((lon - lon_min).abs() < 1e-6, "{lon} vs {lon_min}");
        assert!((lat - lat_max).abs() < 1e-6, "{lat} vs {lat_max}");
        // SE corner unprojects to the tile's SE bound.
        let (lon, lat) = g.unproject(corners[2]).unwrap_or((0.0, 0.0));
        assert!((lon - lon_max).abs() < 1e-6, "{lon} vs {lon_max}");
        assert!((lat - lat_min).abs() < 1e-6, "{lat} vs {lat_min}");
        // Placements for a small view around the centre: all at one zoom, corners finite.
        let lo = g.project(114.07, 32.12).unwrap_or(Vec2::ZERO);
        let hi = g.project(114.09, 32.14).unwrap_or(Vec2::ZERO);
        let placed = tile_placements(&layer, lo, hi, &g, Some(14)).unwrap_or_default();
        assert!(!placed.is_empty());
        assert!(placed.iter().all(|p| p.tile.z == 14 && p.corners.iter().all(|c| c.is_finite())));
        // A requested zoom above the cap is served from the cap, never above it.
        let placed = tile_placements(&layer, lo, hi, &g, Some(25)).unwrap_or_default();
        assert!(placed.iter().all(|p| p.tile.z <= 19));
        // Out-of-range zoom / bad tile bounds are refused, never panics.
        assert!(tile_bounds_lonlat(31, 0, 0).is_none());
    }

    #[test]
    fn tile_backoff_and_cache_path() {
        assert_eq!(tile_backoff_delay_ms(0), 0);
        assert_eq!(tile_backoff_delay_ms(1), 1_000);
        assert_eq!(tile_backoff_delay_ms(2), 2_000);
        assert_eq!(tile_backoff_delay_ms(3), 4_000);
        assert_eq!(tile_backoff_delay_ms(99), 30_000);
        // The cache path is stable per (source, tile) and differs per source.
        let layer = TileLayer::default_osm();
        let tile = TileRef { z: 3, x: 5, y: 2, url: layer.tile_url(3, 5, 2), cache_key: layer.cache_key(3, 5, 2) };
        let a = tile_cache_rel_path(&layer, &tile);
        assert_eq!(a, tile_cache_rel_path(&layer, &tile));
        assert!(a.ends_with("/3/5/2.png"), "{a}");
        let mut other = TileLayer::default_osm();
        other.url_template = "https://example.com/{z}/{x}/{y}.png".into();
        assert_ne!(a, tile_cache_rel_path(&other, &tile));
    }

    #[test]
    fn uuid_v4_shape() {
        let u = new_uuid_v4();
        assert_eq!(u.len(), 36);
        assert_eq!(u.chars().nth(8), Some('-'));
        assert_eq!(u.chars().nth(14), Some('4'));
        assert!(matches!(u.chars().nth(19), Some('8') | Some('9') | Some('a') | Some('b')));
        assert_ne!(new_uuid_v4(), u);
    }

    #[test]
    fn utc_now_iso_shape() {
        let s = utc_now_iso();
        assert!(s.ends_with('Z') && s.contains('T'), "{s}");
        assert_eq!(s.len(), 24, "{s}");
    }

    #[test]
    fn t1_schema_validation() {
        // Good file: the §2.4-shaped sample passes.
        let good = sample_geojson();
        assert!(geojson_to_entities(&good).is_ok());
        // Bad 1: out-of-range longitude → whole file rejected, naming the feature.
        let bad = good.replace("[114.0814000, 32.1301000]", "[181.0, 32.1301000]");
        assert!(geojson_to_entities(&bad).unwrap_err().contains("超出范围"));
        // Bad 2: track vertices length != coordinate count → rejected.
        let mut doc: Value = serde_json::from_str(&good).unwrap_or(Value::Null);
        if let Some(track) = doc.get_mut("features").and_then(|f| f.as_array_mut()).and_then(|a| a.get_mut(1)) {
            track["properties"]["vertices"].as_array_mut().map(|v| v.pop());
        }
        let err = geojson_to_entities(&serde_json::to_string(&doc).unwrap_or_default()).unwrap_err();
        assert!(err.contains("vertices") && err.contains("不一致"), "{err}");
        // Bad 3: timestamps array length != coordinate count → rejected.
        let mut doc: Value = serde_json::from_str(&good).unwrap_or(Value::Null);
        if let Some(track) = doc.get_mut("features").and_then(|f| f.as_array_mut()).and_then(|a| a.get_mut(1)) {
            track["properties"]["timestamps"] = json!(["2026-10-14T00:00:00Z"]);
        }
        let err = geojson_to_entities(&serde_json::to_string(&doc).unwrap_or_default()).unwrap_err();
        assert!(err.contains("timestamps"), "{err}");
        // Bad 4: track id colliding with a marker id → whole file rejected.
        let mut doc: Value = serde_json::from_str(&good).unwrap_or(Value::Null);
        if let Some(track) = doc.get_mut("features").and_then(|f| f.as_array_mut()).and_then(|a| a.get_mut(1)) {
            track["properties"]["ovimap_id"] = json!("11111111-1111-4111-8111-111111111111");
        }
        assert!(geojson_to_entities(&serde_json::to_string(&doc).unwrap_or_default()).unwrap_err().contains("重复"));
        // Bad 5: unknown major schema version → rejected.
        let mut doc: Value = serde_json::from_str(&good).unwrap_or(Value::Null);
        if let Some(meta) = doc.get_mut("ovimap_meta").and_then(|m| m.as_object_mut()) {
            meta.insert("schema_version".into(), json!("2.0"));
        }
        assert!(geojson_to_entities(&serde_json::to_string(&doc).unwrap_or_default()).unwrap_err().contains("大版本"));
    }

    /// The §10.1-shaped sample: 2 markers (1 with a photo), 1 track (3 vertices),
    /// 1 label, 1 photo pin. The full 12/3/4/1 sample lives in the engine tests.
    pub fn sample_geojson() -> String {
        r#"{
          "type": "FeatureCollection",
          "ovimap_georef": {"projection": "OVIMAP_SPHERICAL_TM", "source_crs": "WGS84",
            "origin_lat": 32.1301, "origin_lon": 114.0814,
            "central_meridian": 114.0814, "scale_factor": 1.0,
            "false_easting": 0.0, "false_northing": 0.0,
            "radius": 6378137.0, "units": "metre"},
          "ovimap_meta": {"schema_version": "1.0", "exporter": "ovimap",
            "exported_at": "2026-10-14T05:00:00.000Z",
            "source_collection_id": "c-001", "source_collection_name": "测试工程",
            "delete_mode": "none"},
          "ovimap_deletes": [],
          "features": [
            {"type": "Feature", "geometry": {"type": "Point", "coordinates": [114.0814000, 32.1301000]},
             "properties": {"ovimap_id": "11111111-1111-4111-8111-111111111111",
              "name": "1号杆", "note": "路口东侧", "kind": "marker",
              "type": "concrete", "symbol": "电", "seq": 1,
              "photos": ["IMG_001.jpg"],
              "created_at": "2026-10-13T08:00:00.000Z", "updated_at": "2026-10-14T05:00:00.000Z"}},
            {"type": "Feature", "geometry": {"type": "LineString", "coordinates": [
              [114.0814000, 32.1301000],
              [114.0824000, 32.1305000],
              [114.0834000, 32.1302000]]},
             "properties": {"ovimap_id": "line-001", "name": "杆路1", "note": "", "kind": "track", "type": "track",
              "line_group": "line-001", "closed": false, "timestamps": null,
              "vertices": [
                {"ovimap_id": "11111111-1111-4111-8111-111111111111", "name": "1号杆", "type": "concrete", "note": "路口东侧", "seq": 1, "photos": ["IMG_001.jpg"]},
                {"ovimap_id": "22222222-2222-4222-8222-222222222222", "name": "2号杆", "type": "concrete", "note": "", "seq": 2, "distanceM": 105.5, "segKind": 1, "segCable": "48芯GYTS"},
                {"ovimap_id": "33333333-3333-4333-8333-333333333333", "name": "3号杆", "type": "concrete", "note": "", "seq": 3, "distanceM": 98.0}],
              "created_at": "2026-10-13T08:00:00.000Z", "updated_at": "2026-10-14T05:00:00.000Z"}},
            {"type": "Feature", "geometry": {"type": "Point", "coordinates": [114.0820000, 32.1310000]},
             "properties": {"ovimap_id": "44444444-4444-4444-8444-444444444444",
              "name": "此处需埋地", "note": "", "kind": "label", "type": "text"}},
            {"type": "Feature", "geometry": {"type": "Point", "coordinates": [114.0819000, 32.1308000]},
             "properties": {"ovimap_id": "55555555-5555-4555-8555-555555555555",
              "name": "现场照片", "note": "箱体取证", "kind": "photo",
              "type": "none", "photos": ["IMG_009.jpg"]}}
          ]
        }"#
        .to_string()
    }

    #[test]
    fn t2_geojson_roundtrip_preserves_ids_coords_business_fields() {
        let text = sample_geojson();
        let imp = geojson_to_entities(&text).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(imp.markers, 1);
        assert_eq!(imp.tracks, 1);
        assert_eq!(imp.labels, 1);
        assert_eq!(imp.photos, 1);
        // Marker: POINT + TEXT + PHOTO hint; track: 1 polyline + 1 vertex PHOTO hint;
        // label: 1 TEXT; photo pin: POINT + TEXT.
        assert_eq!(imp.entities.len(), 3 + 2 + 1 + 2);
        // The marker's photos survived (basename only) in the extension area.
        let ext = imp.ext.get("11111111-1111-4111-8111-111111111111").cloned().unwrap_or_default();
        assert_eq!(ext.photos, vec!["IMG_001.jpg"]);
        assert_eq!(ext.note, "路口东侧");
        // The track's vertex business fields survived in the extension area.
        let text2 = imp.ext.get("line-001").cloned().unwrap_or_default();
        assert_eq!(text2.vertices.len(), 3);
        assert_eq!(text2.vertices[1].get("segCable").and_then(Value::as_str), Some("48芯GYTS"));
        assert_eq!(text2.vertices[1].get("distanceM").and_then(Value::as_f64), Some(105.5));
        // Export: ids, coordinates (≤1e-9°) and business fields all come back.
        let refs: Vec<(&Common, &EntityKind)> = imp.entities.iter().map(|(c, k)| (c, k)).collect();
        let out = entities_to_geojson(refs, &georef(), &imp.meta, &imp.ext).unwrap_or_else(|e| panic!("{e}"));
        let back: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
        let feats = back.get("features").and_then(Value::as_array).cloned().unwrap_or_default();
        assert_eq!(feats.len(), 4, "{out}");
        let ids: Vec<String> = feats.iter().filter_map(|f| f.get("properties")?.get("ovimap_id")?.as_str().map(str::to_string)).collect();
        for want in
            ["11111111-1111-4111-8111-111111111111", "line-001", "44444444-4444-4444-8444-444444444444", "55555555-5555-4555-8555-555555555555"]
        {
            assert!(ids.contains(&want.to_string()), "missing {want} in {ids:?}");
        }
        let marker = feats.iter().find(|f| f["properties"]["ovimap_id"] == "11111111-1111-4111-8111-111111111111").cloned().unwrap_or(Value::Null);
        let c = marker["geometry"]["coordinates"].as_array().cloned().unwrap_or_default();
        assert!((c[0].as_f64().unwrap_or(-1.0) - 114.0814).abs() <= 1e-9, "{c:?}");
        assert!((c[1].as_f64().unwrap_or(-1.0) - 32.1301).abs() <= 1e-9, "{c:?}");
        assert_eq!(marker["properties"]["photos"].as_array().map(Vec::len), Some(1));
        assert_eq!(marker["properties"]["note"].as_str(), Some("路口东侧"));
        let track = feats.iter().find(|f| f["properties"]["ovimap_id"] == "line-001").cloned().unwrap_or(Value::Null);
        assert_eq!(track["properties"]["vertices"][1]["segCable"].as_str(), Some("48芯GYTS"));
        assert_eq!(track["properties"]["vertices"][1]["distanceM"].as_f64(), Some(105.5));
        assert_eq!(track["properties"]["timestamps"], Value::Null);
        assert_eq!(track["properties"]["line_group"].as_str(), Some("line-001"));
        assert_eq!(back["ovimap_meta"]["exporter"].as_str(), Some("cadcraft"));
        // Illegal photo entries are dropped with a warning, not a rejection.
        let bad_photo = text.replace("\"photos\": [\"IMG_001.jpg\"]", "\"photos\": [\"../etc/passwd\", \"IMG_001.jpg\"]");
        let imp = geojson_to_entities(&bad_photo).unwrap_or_else(|e| panic!("{e}"));
        assert!(imp.warnings.iter().any(|w| w.contains("非法")), "{:?}", imp.warnings);
        assert_eq!(imp.ext.get("11111111-1111-4111-8111-111111111111").map(|e| e.photos.len()), Some(1));
    }

    #[test]
    fn closed_track_roundtrip() {
        let text = r#"{"type": "FeatureCollection",
          "ovimap_georef": {"projection": "OVIMAP_SPHERICAL_TM", "origin_lat": 32.1301, "origin_lon": 114.0814},
          "ovimap_meta": {"schema_version": "1.0", "exporter": "ovimap", "delete_mode": "none"},
          "ovimap_deletes": [],
          "features": [{"type": "Feature",
            "geometry": {"type": "LineString", "coordinates": [[114.0814, 32.1301], [114.0914, 32.1301], [114.0914, 32.1401], [114.0814, 32.1301]]},
            "properties": {"ovimap_id": "ring-1", "name": "区域", "kind": "track", "type": "track", "line_group": "ring-1", "closed": true,
              "vertices": [{"ovimap_id": "a"}, {"ovimap_id": "b"}, {"ovimap_id": "c"}, {"ovimap_id": "a"}]}}]}"#;
        let imp = geojson_to_entities(text).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(imp.entities.len(), 1);
        let (_, kind) = &imp.entities[0];
        assert!(matches!(kind, EntityKind::LwPolyline(pl) if pl.closed && pl.vertices.len() == 4));
        let refs: Vec<(&Common, &EntityKind)> = imp.entities.iter().map(|(c, k)| (c, k)).collect();
        let out = entities_to_geojson(refs, &georef(), &imp.meta, &imp.ext).unwrap_or_else(|e| panic!("{e}"));
        let back: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
        let feat = back["features"][0].clone();
        assert_eq!(feat["properties"]["closed"].as_bool(), Some(true));
        assert_eq!(feat["geometry"]["coordinates"].as_array().map(Vec::len), Some(4));
    }
}
