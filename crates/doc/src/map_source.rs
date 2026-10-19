//! Custom tile map sources (`docs/ovimap-link.md` §9.5–§9.6).
//!
//! The built-in sources are exactly two, both OpenStreetMap data: the OSM
//! standard tiles and CARTO Voyager (the fallback). Nothing else is
//! pre-installed: no proprietary source URL lives in this code, in the
//! defaults, in tests or in fixtures. A user who has a lawful source of
//! their own can add it through the Map Sources dialog — the URL template,
//! the attribution (mandatory, shown verbatim) and the coordinate type are
//! theirs, and the authorisation responsibility is theirs (§9.5).
//!
//! Coordinate types (`coordType` in the OviMap `customMapSource` XML):
//!
//! * `Mercator` — the tiles are rendered in WGS-84 Web Mercator, the same
//!   frame the vectors use. No conversion.
//! * `Mercator(china)` — the tiles are rendered in GCJ-02. The conversion is
//!   display-only: tile corners are converted GCJ-02 → WGS-84 before they
//!   are projected, and the view bounds are converted WGS-84 → GCJ-02
//!   before tile lookup. Stored coordinates (GeoJSON, DXF, the drawing)
//!   stay WGS-84 and are never polluted (§1.1: conversion only at the
//!   display boundary).
//!
//! URL templates use `{z}` / `{x}` / `{y}` (or the OviMap forms `{$z}` /
//! `{$x}` / `{$y}`). `{$s}` and `{$Galileo}` are subdomain rotations: both
//! expand to one digit, `(x + y) % 4`, so a four-host source spreads its
//! requests. A template is validated before it is stored: http(s) only, a
//! real host, no userinfo/credentials, no whitespace.
//!
//! The OviMap `customMapSource` XML import parses `mapID` / `name` /
//! `version` / `maxZoom` / `coordType` / `tileType` / `tileFormat` /
//! `tileSize` / `protocol` / `host` / `port` / `url`. `mapID`, `name` and
//! `url` are required; the rest fall back to sane defaults (WGS-84
//! Mercator, max zoom 19, PNG, 256 px, the scheme of an absolute `url`).
//! `url` may be absolute (`https://host/path?x={$x}…`) or a path, in which
//! case `protocol` + `host` (+ `port`) supply the base. The XML carries no
//! attribution field, so the caller supplies one — an empty attribution
//! refuses the import, exactly as an empty attribution refuses a source.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::gis::{DEFAULT_TILE_ATTRIBUTION, DEFAULT_TILE_MAX_ZOOM, DEFAULT_TILE_URL, FALLBACK_TILE_ATTRIBUTION, FALLBACK_TILE_URL, TileLayer};

/// Most bytes one imported `customMapSource` XML document may hold (a
/// hostile file is capped, never ballooned).
const MAX_XML_BYTES: usize = 64 * 1024;
/// Most custom sources one library holds (plus the two built-ins).
const MAX_SOURCES: usize = 64;
/// Longest source id / name, in characters.
const MAX_ID_CHARS: usize = 128;

/// The coordinate frame a tile source's tiles are rendered in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoordType {
    /// `Mercator`: WGS-84 Web Mercator, no conversion.
    #[default]
    #[serde(rename = "Mercator")]
    Wgs84,
    /// `Mercator(china)`: GCJ-02, display-only conversion (§1.1).
    #[serde(rename = "Mercator(china)")]
    Gcj02,
}

impl CoordType {
    /// The OviMap `coordType` spelling (`Mercator` / `Mercator(china)`).
    pub fn as_str(self) -> &'static str {
        match self {
            CoordType::Wgs84 => "Mercator",
            CoordType::Gcj02 => "Mercator(china)",
        }
    }

    /// The dialog's label for this type.
    pub fn label(self) -> &'static str {
        match self {
            CoordType::Wgs84 => "Mercator (WGS84)",
            CoordType::Gcj02 => "Mercator (china, GCJ-02)",
        }
    }

    /// Parse an OviMap `coordType` value (and the engine's `coordType`
    /// parameter, which accepts the same spellings plus `wgs84` / `gcj02`).
    /// `None` for anything else — the caller reports the accepted values.
    pub fn parse(s: &str) -> Option<CoordType> {
        let t = s.trim();
        let lower = t.to_ascii_lowercase().replace([' ', '_', '-'], "");
        match lower.as_str() {
            "mercator" | "wgs84" | "wgs84mercator" => Some(CoordType::Wgs84),
            "mercator(china)" | "mercatorchina" | "china" | "gcj02" | "gcj" => Some(CoordType::Gcj02),
            _ => None,
        }
    }
}

/// Whether `(lat, lon)` is inside mainland China's GCJ-02 region (the
/// standard algorithm's bounding box, with a small margin). Outside it the
/// offset is zero and the conversion is the identity.
fn in_china(lat: f64, lon: f64) -> bool {
    (0.8293..=55.8271).contains(&lat) && (72.004..=137.8347).contains(&lon)
}

fn gcj_offset(lat: f64, lon: f64) -> (f64, f64) {
    // The public GCJ-02 offset algorithm (Krasovsky 1940 ellipsoid,
    // a = 6378245.0, ee = 0.00669342162296594323): the same maths the
    // OviMap `geo_convert.dart` implementation uses.
    let a = 6378245.0f64;
    let ee = 0.006_693_421_622_965_943_f64;
    let pi = std::f64::consts::PI;
    let rad = pi / 180.0;
    let x = lon - 105.0;
    let y = lat - 35.0;
    let d_lat = -100.0 + 2.0 * x + 3.0 * y + 0.2 * y * y + 0.1 * x * y + 0.2 * (x.abs().sqrt());
    let d_lon = 300.0 + x + 2.0 * y + 0.1 * x * x + 0.1 * x * y + 0.1 * (x.abs().sqrt());
    // NOTE: the sine arguments are `k * v * PI` with `v` in degrees (not
    // radians) — the canonical GCJ-02 form. Using `* PI / 180` here silently
    // produces a ~14 km latitude error.
    let mut d_lat = d_lat
        + (20.0 * (6.0 * x * pi).sin() + 20.0 * (2.0 * x * pi).sin()) * 2.0 / 3.0
        + (20.0 * (y * pi).sin() + 40.0 * (y / 3.0 * pi).sin()) * 2.0 / 3.0
        + (160.0 * (y / 12.0 * pi).sin() + 320.0 * (y / 30.0 * pi).sin()) * 2.0 / 3.0;
    let mut d_lon = d_lon
        + (20.0 * (6.0 * x * pi).sin() + 20.0 * (2.0 * x * pi).sin()) * 2.0 / 3.0
        + (20.0 * (x * pi).sin() + 40.0 * (x / 3.0 * pi).sin()) * 2.0 / 3.0
        + (150.0 * (x / 12.0 * pi).sin() + 300.0 * (x / 30.0 * pi).sin()) * 2.0 / 3.0;
    let sin_lat = (lat * rad).sin();
    let magic = 1.0 - ee * sin_lat * sin_lat;
    let sqrt_magic = magic.sqrt();
    // The scale factors multiply by PI, not PI / 180 (same canonical form).
    d_lat = (d_lat * 180.0) / ((a * (1.0 - ee)) / (magic * sqrt_magic) * pi);
    d_lon = (d_lon * 180.0) / (a / sqrt_magic * (lat * rad).cos() * pi);
    (d_lat, d_lon)
}

/// WGS-84 → GCJ-02 (display only). Outside China, or for non-finite input,
/// the input is returned unchanged.
pub fn wgs84_to_gcj02(lat: f64, lon: f64) -> (f64, f64) {
    if !lat.is_finite() || !lon.is_finite() || !in_china(lat, lon) {
        return (lat, lon);
    }
    let (d_lat, d_lon) = gcj_offset(lat, lon);
    (lat + d_lat, lon + d_lon)
}

/// GCJ-02 → WGS-84 (display only): the inverse of [`wgs84_to_gcj02`],
/// recovered by iterating the forward offset to sub-millimetre accuracy.
/// Outside China, or for non-finite input, the input is returned unchanged.
pub fn gcj02_to_wgs84(lat: f64, lon: f64) -> (f64, f64) {
    if !lat.is_finite() || !lon.is_finite() || !in_china(lat, lon) {
        return (lat, lon);
    }
    let mut wgs_lat = lat;
    let mut wgs_lon = lon;
    for _ in 0..3 {
        let (g_lat, g_lon) = wgs84_to_gcj02(wgs_lat, wgs_lon);
        let d_lat = lat - g_lat;
        let d_lon = lon - g_lon;
        wgs_lat += d_lat;
        wgs_lon += d_lon;
        if d_lat.abs() < 1e-9 && d_lon.abs() < 1e-9 {
            break;
        }
    }
    (wgs_lat, wgs_lon)
}

/// Expand a tile URL template for one tile. Supports `{z}` / `{x}` /
/// `{y}` and the OviMap forms `{$z}` / `{$x}` / `{$y}`; `{$s}` and
/// `{$Galileo}` (and the bare `{s}` / `{Galileo}` forms) expand to the
/// subdomain digit `(x + y) % 4`.
pub fn expand_tile_template(template: &str, z: u32, x: u32, y: u32) -> String {
    let sub = ((x.wrapping_add(y)) % 4).to_string();
    template
        .replace("{$x}", &x.to_string())
        .replace("{$y}", &y.to_string())
        .replace("{$z}", &z.to_string())
        .replace("{$s}", &sub)
        .replace("{$Galileo}", &sub)
        .replace("{$galileo}", &sub)
        .replace("{x}", &x.to_string())
        .replace("{y}", &y.to_string())
        .replace("{z}", &z.to_string())
        .replace("{s}", &sub)
        .replace("{Galileo}", &sub)
        .replace("{galileo}", &sub)
}

/// Whether `template` carries a tile coordinate placeholder (`{x}` or
/// `{$x}`, and the same for `y` / `z`).
fn has_placeholder(template: &str, coord: &str) -> bool {
    template.contains(&format!("{{{coord}}}")) || template.contains(&format!("{{${coord}}}"))
}

/// Validate a tile URL template's scheme and host: http(s) only, a real
/// host, no userinfo/credentials, no whitespace or backslashes. Placeholder
/// forms (`{$s}` …) are stripped before the host is checked. The caller
/// checks the coordinate placeholders separately, so the message can name
/// the missing one.
pub fn validate_template_scheme_host(template: &str) -> Result<(), String> {
    let t = template.trim();
    if t.len() > 2048 {
        return Err("瓦片 URL 模板过长（须 ≤ 2048 字符）".into());
    }
    let rest =
        t.strip_prefix("http://").or_else(|| t.strip_prefix("https://")).ok_or_else(|| "瓦片 URL 模板须以 http:// 或 https:// 开头".to_string())?;
    if rest.is_empty() {
        return Err("瓦片 URL 模板缺少主机".into());
    }
    let host_port = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host_port.is_empty() {
        return Err("瓦片 URL 模板缺少主机".into());
    }
    if host_port.contains('@') {
        return Err("瓦片 URL 模板不得含用户信息（@，不得在 URL 中携带账号密码）".into());
    }
    if t.chars().any(|c| c.is_whitespace() || c == '\\') {
        return Err("瓦片 URL 模板不得含空白或反斜杠".into());
    }
    validate_host(host_port)?;
    Ok(())
}

/// Validate a host (with an optional `:port`): after placeholder forms are
/// stripped, labels of letters / digits / `-` / `_`, no leading or trailing
/// dot or hyphen, at most 253 characters.
pub fn validate_host(host_port: &str) -> Result<(), String> {
    let stripped = host_port.replace("{$s}", "").replace("{$Galileo}", "").replace("{$galileo}", "").replace("{s}", "").replace("{Galileo}", "");
    let host = stripped.split(':').next().unwrap_or("");
    if host.is_empty() || host.len() > 253 {
        return Err(format!("图源主机 `{host_port}` 无效（主机为空或过长）"));
    }
    if host.starts_with('.') || host.ends_with('.') || host.starts_with('-') || host.ends_with('-') {
        return Err(format!("图源主机 `{host_port}` 无效（不得以 . 或 - 开头/结尾）"));
    }
    if !host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) {
        return Err(format!("图源主机 `{host_port}` 无效（主机只能含字母、数字、.、-、_）"));
    }
    for label in host.split('.') {
        if label.is_empty() || label.starts_with('-') || label.ends_with('-') {
            return Err(format!("图源主机 `{host_port}` 无效（域名段为空或以 - 开头/结尾）"));
        }
    }
    if let Some(port) = stripped.split(':').nth(1) {
        let port = port.split('/').next().unwrap_or("");
        if !port.is_empty() {
            match port.parse::<u32>() {
                Ok(p) if (1..=65535).contains(&p) => {}
                _ => return Err(format!("图源端口 `{port}` 无效（须为 1~65535）")),
            }
        }
    }
    Ok(())
}

/// Validate a full tile URL template: placeholders + scheme/host.
pub fn validate_tile_template(template: &str) -> Result<(), String> {
    for coord in ["x", "y", "z"] {
        if !has_placeholder(template, coord) {
            return Err(format!("瓦片 URL 模板缺少占位符 {{{coord}}}（须含 {{z}}/{{x}}/{{y}} 或 {{$z}}/{{$x}}/{{$y}}）"));
        }
    }
    validate_template_scheme_host(template)
}

/// One tile source: a URL template, its mandatory attribution, its
/// coordinate type and its display limits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MapSource {
    /// Stable id (the library key; `osm` / `carto` for the built-ins).
    pub id: String,
    /// Display name.
    pub name: String,
    /// URL template with `{z}` / `{x}` / `{y}` (or `{$z}` / `{$x}` / `{$y}`).
    pub url_template: String,
    /// Attribution text, shown verbatim wherever tiles show. Mandatory.
    pub attribution: String,
    /// Highest zoom requested.
    pub max_zoom: u32,
    /// The coordinate frame the tiles are rendered in.
    pub coord_type: CoordType,
    /// Tile image format (`png`, `jpg`, `webp`): informational, the URL
    /// template decides the bytes actually fetched.
    pub tile_format: String,
    /// Tile size in pixels (256 / 512): informational.
    pub tile_size: u32,
    /// `true` for the two built-ins: they cannot be deleted or renamed.
    pub builtin: bool,
}

impl Default for MapSource {
    fn default() -> Self {
        MapSource {
            id: String::new(),
            name: String::new(),
            url_template: DEFAULT_TILE_URL.into(),
            attribution: DEFAULT_TILE_ATTRIBUTION.into(),
            max_zoom: DEFAULT_TILE_MAX_ZOOM,
            coord_type: CoordType::Wgs84,
            tile_format: "png".into(),
            tile_size: 256,
            builtin: false,
        }
    }
}

impl MapSource {
    /// The built-in OSM standard tiles (the default source).
    pub fn osm() -> Self {
        MapSource {
            id: "osm".into(),
            name: "OpenStreetMap".into(),
            url_template: DEFAULT_TILE_URL.into(),
            attribution: DEFAULT_TILE_ATTRIBUTION.into(),
            max_zoom: DEFAULT_TILE_MAX_ZOOM,
            coord_type: CoordType::Wgs84,
            tile_format: "png".into(),
            tile_size: 256,
            builtin: true,
        }
    }

    /// The built-in CARTO Voyager tiles (OSM data; the OSM default's fallback).
    pub fn carto() -> Self {
        MapSource {
            id: "carto".into(),
            name: "CARTO Voyager".into(),
            url_template: FALLBACK_TILE_URL.into(),
            attribution: FALLBACK_TILE_ATTRIBUTION.into(),
            max_zoom: DEFAULT_TILE_MAX_ZOOM,
            coord_type: CoordType::Wgs84,
            tile_format: "png".into(),
            tile_size: 256,
            builtin: true,
        }
    }

    /// Validate the source: id, name, template, mandatory attribution,
    /// zoom cap (the OSM default never requests above z=19), tile format/size.
    /// Errors are in Chinese for the dialog / command line.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() || self.id.chars().count() > MAX_ID_CHARS {
            return Err("图源 ID 为空或过长（须为 1~128 字符）".into());
        }
        if self.id.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\' | ':' | '"' | '<' | '>' | '|' | '?' | '*')) {
            return Err(format!("图源 ID `{}` 含非法字符（不得含空白、控制字符或 /\\:\"<>|?*）", self.id));
        }
        if self.name.trim().is_empty() || self.name.chars().count() > MAX_ID_CHARS {
            return Err("图源名称为空或过长（须为 1~128 字符）".into());
        }
        validate_tile_template(&self.url_template)?;
        if self.attribution.trim().is_empty() {
            return Err("自定义图源必须填写署名（署名为空时拒绝保存）".into());
        }
        if self.url_template.trim() == DEFAULT_TILE_URL && self.max_zoom > DEFAULT_TILE_MAX_ZOOM {
            return Err(format!("默认 OSM 瓦片最高级别为 {DEFAULT_TILE_MAX_ZOOM}，不得请求更高级别"));
        }
        if self.max_zoom > 30 {
            return Err("图源最高级别须 ≤ 30".into());
        }
        let fmt = self.tile_format.trim().to_ascii_lowercase();
        if !matches!(fmt.as_str(), "png" | "jpg" | "jpeg" | "webp") {
            return Err(format!("图源瓦片格式 `{}` 不支持（须为 png/jpg/webp）", self.tile_format));
        }
        if !(64..=1024).contains(&self.tile_size) {
            return Err("图源瓦片尺寸须在 64~1024 像素之间".into());
        }
        Ok(())
    }

    /// This source as the drawing's tile layer (opacity 0.85, enabled, no
    /// fallback — the fallback belongs to the OSM default, not to a source).
    pub fn to_tile_layer(&self) -> TileLayer {
        TileLayer {
            url_template: self.url_template.clone(),
            attribution: self.attribution.clone(),
            opacity: 0.85,
            enabled: true,
            max_zoom: self.max_zoom,
            coord_type: self.coord_type,
            fallback_url_template: None,
            fallback_attribution: None,
        }
    }
}

/// The user's map-source library: the two built-ins plus their own sources,
/// and which one is current. Serialised into the app config
/// (`prefs_json` → `mapSources`) and restored at startup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MapSourceLibrary {
    pub sources: Vec<MapSource>,
    pub current_id: String,
}

impl Default for MapSourceLibrary {
    fn default() -> Self {
        MapSourceLibrary { sources: vec![MapSource::osm(), MapSource::carto()], current_id: "osm".into() }
    }
}

impl MapSourceLibrary {
    /// The source currently selected, or the OSM default when the id is stale.
    pub fn current(&self) -> MapSource {
        self.sources.iter().find(|s| s.id == self.current_id).cloned().unwrap_or_else(MapSource::osm)
    }

    /// Find a source by id.
    pub fn find(&self, id: &str) -> Option<&MapSource> {
        self.sources.iter().find(|s| s.id == id)
    }

    /// Add a source: validated, id unique, library cap enforced.
    pub fn add(&mut self, source: MapSource) -> Result<(), String> {
        source.validate()?;
        if self.find(&source.id).is_some() {
            return Err(format!("图源 ID `{}` 已存在", source.id));
        }
        if self.sources.len() >= MAX_SOURCES + 2 {
            return Err(format!("图源数量已达上限 {}", MAX_SOURCES + 2));
        }
        self.sources.push(source);
        Ok(())
    }

    /// Replace the source with `id` by `source` (edit): validated, and a
    /// built-in keeps its id / URL / built-in flag (only its display fields
    /// may change — and the built-ins' attribution must stay intact, so a
    /// built-in's attribution is never overwritten here).
    pub fn replace(&mut self, id: &str, source: MapSource) -> Result<(), String> {
        source.validate()?;
        if source.id != id {
            return Err("编辑图源时 ID 不得更改".into());
        }
        let Some(pos) = self.sources.iter().position(|s| s.id == id) else {
            return Err(format!("图源 `{id}` 不存在"));
        };
        if self.sources[pos].builtin {
            if source.url_template.trim() != self.sources[pos].url_template.trim() || source.attribution != self.sources[pos].attribution {
                return Err("内置图源的 URL 与署名不可修改".into());
            }
            if source.coord_type != CoordType::Wgs84 {
                return Err("内置图源为 WGS84 瓦片，不可设为 Mercator(china)".into());
            }
        }
        self.sources[pos] = source;
        Ok(())
    }

    /// Remove a source: built-ins cannot be removed; removing the current
    /// source falls back to OSM.
    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        let Some(pos) = self.sources.iter().position(|s| s.id == id) else {
            return Err(format!("图源 `{id}` 不存在"));
        };
        if self.sources[pos].builtin {
            return Err(format!("内置图源 `{}` 不可删除", self.sources[pos].name));
        }
        self.sources.remove(pos);
        if self.current_id == id {
            self.current_id = "osm".into();
        }
        Ok(())
    }

    /// Make `id` the current source.
    pub fn set_current(&mut self, id: &str) -> Result<(), String> {
        if self.find(id).is_none() {
            return Err(format!("图源 `{id}` 不存在"));
        }
        self.current_id = id.to_string();
        Ok(())
    }

    /// Serialise for the app config.
    pub fn to_json(&self) -> Value {
        json!({ "sources": self.sources, "currentId": self.current_id })
    }

    /// Parse a config value saved by [`MapSourceLibrary::to_json`]: unknown
    /// or malformed values fall back to the default library (never a
    /// half-loaded library with a missing built-in or a sourceless current).
    pub fn from_json(v: &Value) -> MapSourceLibrary {
        let mut lib = MapSourceLibrary::default();
        let Some(obj) = v.as_object() else { return lib };
        let Some(arr) = obj.get("sources").and_then(Value::as_array) else { return lib };
        let mut sources: Vec<MapSource> = Vec::new();
        for item in arr {
            if let Ok(s) = serde_json::from_value::<MapSource>(item.clone())
                && s.validate().is_ok()
            {
                sources.push(s);
            }
        }
        if !sources.iter().any(|s| s.id == "osm") {
            sources.insert(0, MapSource::osm());
        }
        if !sources.iter().any(|s| s.id == "carto") {
            sources.insert(1, MapSource::carto());
        }
        sources.truncate(MAX_SOURCES + 2);
        lib.sources = sources;
        if let Some(id) = obj.get("currentId").and_then(Value::as_str)
            && lib.find(id).is_some()
        {
            lib.current_id = id.to_string();
        }
        lib
    }
}

/// Find `<tag …>…</tag>` in `xml` (case-insensitive tag name) and return
/// its inner text, CDATA-unwrapped and entity-unescaped. `None` when the
/// tag is absent or unclosed.
fn tag_text(xml: &str, tag: &str) -> Option<String> {
    let lower = xml.to_ascii_lowercase();
    let mut search_from = 0usize;
    loop {
        let lt = lower[search_from..].find('<')? + search_from;
        let after_lt = lt + 1;
        if lower[after_lt..].starts_with('/') || lower[after_lt..].starts_with('!') || lower[after_lt..].starts_with('?') {
            search_from = after_lt;
            continue;
        }
        let name_end = lower[after_lt..].find(|c: char| c.is_whitespace() || c == '>' || c == '/').map(|i| i + after_lt).unwrap_or(lower.len());
        if lower[after_lt..name_end].eq_ignore_ascii_case(tag) {
            if lower[name_end..].starts_with("/>") {
                return Some(String::new());
            }
            let content_start = lower[name_end..].find('>')? + name_end + 1;
            let close = format!("</{}>", tag.to_ascii_lowercase());
            let close_pos = lower[content_start..].find(&close)? + content_start;
            let raw = xml.get(content_start..close_pos)?;
            return Some(unescape_xml(unwrap_cdata(raw)));
        }
        search_from = after_lt;
    }
}

/// Find `attr="…"` on the first opening tag in `xml`. `None` when absent.
fn root_attr(xml: &str, attr: &str) -> Option<String> {
    let lower = xml.to_ascii_lowercase();
    let lt = lower.find('<')?;
    let gt = lower[lt..].find('>')? + lt;
    let tag = xml.get(lt..gt)?;
    let tag_lower = tag.to_ascii_lowercase();
    let needle = format!("{attr}=");
    let pos = tag_lower.find(&needle)?;
    let rest = tag.get(pos + needle.len()..)?.trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let end = rest[1..].find(quote)?;
    Some(unescape_xml(rest.get(1..1 + end)?))
}

fn unwrap_cdata(raw: &str) -> &str {
    let t = raw.trim();
    t.strip_prefix("<![CDATA[").and_then(|r| r.strip_suffix("]]>")).unwrap_or(t)
}

fn unescape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos + 1..];
        let Some(semi) = tail.find(';') else {
            out.push('&');
            rest = tail;
            continue;
        };
        let entity = &tail[..semi];
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ => {
                if let Some(hex) = entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) {
                    if let Some(n) = u32::from_str_radix(hex, 16).ok().filter(|n| *n != 0).and_then(char::from_u32) {
                        out.push(n);
                    }
                } else if let Some(dec) = entity.strip_prefix('#') {
                    if let Some(n) = dec.parse::<u32>().ok().filter(|n| *n != 0).and_then(char::from_u32) {
                        out.push(n);
                    }
                } else {
                    out.push('&');
                    out.push_str(entity);
                    out.push(';');
                }
            }
        }
        rest = &tail[semi + 1..];
    }
    out.push_str(rest);
    out
}

fn field(xml: &str, tag: &str) -> Option<String> {
    tag_text(xml, tag).or_else(|| root_attr(xml, tag)).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Parse an OviMap `customMapSource` XML document into a [`MapSource`].
/// `attribution` is supplied by the caller (the XML carries none) and must
/// be non-empty. Missing required fields, an illegal host or an illegal
/// template are refused with a Chinese error naming the problem.
pub fn parse_ovi_custom_map_source(xml: &str, attribution: &str) -> Result<MapSource, String> {
    if xml.len() > MAX_XML_BYTES {
        return Err(format!("图源 XML 过大（须 ≤ {} 字节）", MAX_XML_BYTES));
    }
    if xml.contains("<!ENTITY") || xml.contains("<!DOCTYPE") && xml.to_ascii_uppercase().contains("ENTITY") {
        return Err("图源 XML 不得含 ENTITY 定义".into());
    }
    let map_id = field(xml, "mapID").ok_or_else(|| "图源 XML 缺少 mapID（必填）".to_string())?;
    if map_id.chars().count() > MAX_ID_CHARS
        || map_id.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\' | ':' | '"' | '<' | '>' | '|' | '?' | '*'))
    {
        return Err(format!("图源 XML 的 mapID `{map_id}` 无效（须为 1~128 字符，不得含空白或 /\\:\"<>|?*）"));
    }
    let name = field(xml, "name").ok_or_else(|| "图源 XML 缺少 name（必填）".to_string())?;
    if name.chars().count() > MAX_ID_CHARS {
        return Err("图源 XML 的 name 过长（须 ≤ 128 字符）".into());
    }
    let url = field(xml, "url").ok_or_else(|| "图源 XML 缺少 url（必填）".to_string())?;
    let coord_type = match field(xml, "coordType") {
        None => CoordType::Wgs84,
        Some(v) => CoordType::parse(&v).ok_or_else(|| format!("图源 XML 的 coordType `{v}` 不支持（须为 Mercator 或 Mercator(china)）"))?,
    };
    let max_zoom = match field(xml, "maxZoom") {
        None => DEFAULT_TILE_MAX_ZOOM,
        Some(v) => v.parse::<u32>().ok().filter(|z| *z <= 30).ok_or_else(|| format!("图源 XML 的 maxZoom `{v}` 无效（须为 0~30 的整数）"))?,
    };
    let tile_format = field(xml, "tileFormat").or_else(|| field(xml, "tileType")).unwrap_or_else(|| "png".into());
    let tile_size = match field(xml, "tileSize") {
        None => 256,
        Some(v) => v
            .parse::<u32>()
            .ok()
            .filter(|s| (64..=1024).contains(s))
            .ok_or_else(|| format!("图源 XML 的 tileSize `{v}` 无效（须为 64~1024 的整数）"))?,
    };
    // Compose the template: an absolute `url` stands on its own; a
    // relative `url` (a path) is completed with protocol + host (+ port).
    let template = if url.starts_with("http://") || url.starts_with("https://") {
        url.clone()
    } else {
        let protocol = field(xml, "protocol").unwrap_or_else(|| "http".into());
        if protocol != "http" && protocol != "https" {
            return Err(format!("图源 XML 的 protocol `{protocol}` 不支持（须为 http 或 https）"));
        }
        let host = field(xml, "host").ok_or_else(|| "图源 XML 的 url 为相对路径时缺少 host（必填）".to_string())?;
        validate_host(&host).map_err(|e| format!("图源 XML 的 host 非法：{e}"))?;
        let port = match field(xml, "port") {
            None => None,
            Some(v) => Some(
                v.parse::<u32>().ok().filter(|p| (1..=65535).contains(p)).ok_or_else(|| format!("图源 XML 的 port `{v}` 无效（须为 1~65535）"))?,
            ),
        };
        let default_port = if protocol == "https" { 443 } else { 80 };
        let host_port = if port.is_some_and(|p| p != default_port) { format!("{}:{}", host, port.unwrap_or(default_port)) } else { host };
        let path = if url.starts_with('/') { url.clone() } else { format!("/{url}") };
        format!("{protocol}://{host_port}{path}")
    };
    if let Some(host) = field(xml, "host") {
        validate_host(&host).map_err(|e| format!("图源 XML 的 host 非法：{e}"))?;
    }
    if attribution.trim().is_empty() {
        return Err("导入图源必须填写署名（署名为空时拒绝导入）".into());
    }
    let source = MapSource {
        id: map_id,
        name,
        url_template: template,
        attribution: attribution.trim().to_string(),
        max_zoom,
        coord_type,
        tile_format: tile_format.trim().to_ascii_lowercase(),
        tile_size,
        builtin: false,
    };
    source.validate().map_err(|e| format!("图源 XML 校验失败：{e}"))?;
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xml_template() -> &'static str {
        r#"<customMapSource><mapID>demo</mapID><name>演示图源</name><version>1</version><maxZoom>18</maxZoom><coordType>Mercator(china)</coordType><tileType>png</tileType><tileFormat>png</tileFormat><tileSize>256</tileSize><protocol>https</protocol><host>tiles.example.com</host><port>443</port><url>https://tiles.example.com/{z}/{x}/{y}.png</url></customMapSource>"#
    }

    #[test]
    fn gcj02_matches_the_documented_test_vector() {
        // ovimap-link §1.3: WGS84 (32.1301, 114.0814) → GCJ-02 (32.1281536, 114.0872867).
        let (lat, lon) = wgs84_to_gcj02(32.1301, 114.0814);
        assert!((lat - 32.1281536).abs() < 1e-6, "lat {lat}");
        assert!((lon - 114.0872867).abs() < 1e-6, "lon {lon}");
        // The inverse restores the WGS-84 value (display-only round trip).
        let (back_lat, back_lon) = gcj02_to_wgs84(lat, lon);
        assert!((back_lat - 32.1301).abs() < 1e-9, "back lat {back_lat}");
        assert!((back_lon - 114.0814).abs() < 1e-9, "back lon {back_lon}");
    }

    #[test]
    fn gcj02_is_identity_outside_china() {
        assert_eq!(wgs84_to_gcj02(48.85, 2.35), (48.85, 2.35));
        assert_eq!(gcj02_to_wgs84(48.85, 2.35), (48.85, 2.35));
        assert!(wgs84_to_gcj02(f64::NAN, 114.0).0.is_nan(), "NaN passes through unchanged");
    }

    #[test]
    fn template_expansion_supports_ovi_placeholders() {
        assert_eq!(expand_tile_template("https://h.example.com/{z}/{x}/{y}.png", 3, 5, 2), "https://h.example.com/3/5/2.png");
        assert_eq!(
            expand_tile_template("https://mt{$s}.example.com/vt?x={$x}&y={$y}&z={$z}", 3, 5, 2),
            "https://mt3.example.com/vt?x=5&y=2&z=3",
            "subdomain digit is (x + y) % 4"
        );
        assert_eq!(expand_tile_template("https://mt{$Galileo}.example.com/{$z}/{$x}/{$y}.png", 1, 1, 1), "https://mt2.example.com/1/1/1.png");
    }

    #[test]
    fn template_validation_rejects_hostile_templates() {
        assert!(validate_tile_template("https://example.com/{z}/{x}/{y}.png").is_ok());
        assert!(validate_tile_template("https://example.com/{$z}/{$x}/{$y}.png").is_ok());
        assert!(validate_tile_template("https://example.com/tiles.png").is_err(), "missing placeholders");
        assert!(validate_tile_template("ftp://example.com/{z}/{x}/{y}.png").is_err(), "non-http scheme");
        assert!(validate_tile_template("https://user:pass@example.com/{z}/{x}/{y}.png").is_err(), "userinfo");
        assert!(validate_tile_template("https://exa mple.com/{z}/{x}/{y}.png").is_err(), "whitespace in host");
        assert!(validate_tile_template("https:///path/{z}/{x}/{y}.png").is_err(), "missing host");
    }

    #[test]
    fn host_validation_rejects_bad_hosts() {
        assert!(validate_host("tiles.example.com").is_ok());
        assert!(validate_host("mt{$s}.example.com").is_ok(), "subdomain placeholder allowed in host");
        assert!(validate_host("127.0.0.1:8080").is_ok(), "a local tile server is a legitimate user source");
        assert!(validate_host("").is_err());
        assert!(validate_host("-bad.example.com").is_err());
        assert!(validate_host("bad_host!.example.com").is_err());
        assert!(validate_host("example.com:99999").is_err());
    }

    #[test]
    fn ovi_xml_import_absolute_url() {
        let s = parse_ovi_custom_map_source(xml_template(), "演示 贡献者").expect("valid XML imports");
        assert_eq!(s.id, "demo");
        assert_eq!(s.name, "演示图源");
        assert_eq!(s.url_template, "https://tiles.example.com/{z}/{x}/{y}.png");
        assert_eq!(s.attribution, "演示 贡献者");
        assert_eq!(s.max_zoom, 18);
        assert_eq!(s.coord_type, CoordType::Gcj02);
        assert_eq!(s.tile_size, 256);
        assert!(!s.builtin);
    }

    #[test]
    fn ovi_xml_import_relative_url_composes_base() {
        let xml = r#"<customMapSource><mapID>rel</mapID><name>相对</name><coordType>Mercator</coordType><protocol>https</protocol><host>tiles.example.com</host><port>8443</port><url>/tiles/{$z}/{$x}/{$y}.png</url></customMapSource>"#;
        let s = parse_ovi_custom_map_source(xml, "Example 贡献者").expect("relative url composes");
        assert_eq!(s.url_template, "https://tiles.example.com:8443/tiles/{$z}/{$x}/{$y}.png");
        assert_eq!(s.coord_type, CoordType::Wgs84);
        assert_eq!(s.max_zoom, DEFAULT_TILE_MAX_ZOOM, "maxZoom absent → default 19");
    }

    #[test]
    fn ovi_xml_import_rejects_missing_fields_and_bad_values() {
        let no_id = xml_template().replace("<mapID>demo</mapID>", "");
        assert!(parse_ovi_custom_map_source(&no_id, "署名").unwrap_err().contains("mapID"));
        let no_name = xml_template().replace("<name>演示图源</name>", "");
        assert!(parse_ovi_custom_map_source(&no_name, "署名").unwrap_err().contains("name"));
        let no_url = xml_template().replace("<url>https://tiles.example.com/{z}/{x}/{y}.png</url>", "");
        assert!(parse_ovi_custom_map_source(&no_url, "署名").unwrap_err().contains("url"));
        let bad_coord = xml_template().replace("Mercator(china)", "BD-09");
        assert!(parse_ovi_custom_map_source(&bad_coord, "署名").unwrap_err().contains("coordType"));
        let bad_host =
            xml_template().replace("tiles.example.com/{z}", "bad host/{z}").replace("<host>tiles.example.com</host>", "<host>bad host</host>");
        assert!(parse_ovi_custom_map_source(&bad_host, "署名").is_err(), "illegal host rejected");
        let no_attr = xml_template();
        assert!(parse_ovi_custom_map_source(no_attr, "   ").unwrap_err().contains("署名"), "empty attribution rejected");
        let no_template = xml_template().replace("https://tiles.example.com/{z}/{x}/{y}.png", "https://tiles.example.com/tiles.png");
        assert!(parse_ovi_custom_map_source(&no_template, "署名").is_err(), "template without placeholders rejected");
    }

    #[test]
    fn library_add_remove_current_and_persistence() {
        let mut lib = MapSourceLibrary::default();
        assert_eq!(lib.current().id, "osm");
        assert_eq!(lib.sources.len(), 2);
        let custom = parse_ovi_custom_map_source(xml_template(), "演示 贡献者").expect("valid");
        lib.add(custom).expect("add");
        assert!(lib.add(MapSource::osm()).is_err(), "duplicate id refused");
        lib.set_current("demo").expect("set current");
        assert_eq!(lib.current().id, "demo");
        assert!(lib.remove("osm").is_err(), "built-ins cannot be removed");
        lib.remove("demo").expect("remove custom");
        assert_eq!(lib.current().id, "osm", "removing the current source falls back to OSM");
        // Config round trip.
        lib.add(parse_ovi_custom_map_source(xml_template(), "演示 贡献者").expect("valid")).expect("re-add");
        lib.set_current("demo").expect("set current again");
        let back = MapSourceLibrary::from_json(&lib.to_json());
        assert_eq!(back, lib);
        // A hostile config falls back to the defaults, never a broken library.
        let hostile = json!({ "sources": [{ "id": "x" }], "currentId": "missing" });
        let back = MapSourceLibrary::from_json(&hostile);
        assert_eq!(back.current().id, "osm");
        assert!(back.find("carto").is_some(), "the CARTO built-in is restored");
    }

    #[test]
    fn coord_type_spellings() {
        assert_eq!(CoordType::parse("Mercator"), Some(CoordType::Wgs84));
        assert_eq!(CoordType::parse("Mercator(china)"), Some(CoordType::Gcj02));
        assert_eq!(CoordType::parse("Mercator (china)"), Some(CoordType::Gcj02));
        assert_eq!(CoordType::parse("gcj02"), Some(CoordType::Gcj02));
        assert_eq!(CoordType::parse("BD-09"), None);
        assert_eq!(CoordType::Wgs84.as_str(), "Mercator");
        assert_eq!(CoordType::Gcj02.as_str(), "Mercator(china)");
    }

    /// The ten user sources, as handed out in OviMap XML: import must
    /// reproduce ovimap's URLs exactly (same host, path, query, datum,
    /// zoom cap) — an import that silently rewrites a URL sends the
    /// fetcher somewhere the source does not serve, and the tile blanks.
    /// Each case is `(xml, expected expanded tile url at z=12,x=3345,y=1661,
    /// expected coord type, expected max zoom)`.
    #[test]
    fn ovi_xml_import_matches_ovimap_urls() {
        fn xml(id: &str, name: &str, max_zoom: u32, coord: &str, format: &str, host: &str, url: &str) -> String {
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><customMapSource><mapID>{id}</mapID><name>{name}</name><version>0</version><maxZoom>{max_zoom}</maxZoom><coordType>{coord}</coordType><tileType>Map</tileType><tileFormat>{format}</tileFormat><tileSize>256</tileSize><protocol>https</protocol><host>{host}</host><port>443</port><url>{url}</url></customMapSource>"#
            )
        }
        let cases: [(String, &str, CoordType, u32); 10] = [
            (
                xml(
                    "sydt",
                    "联通卫星+注记",
                    20,
                    "Mercator(china)",
                    "JPG",
                    "sydt.hainasi.eu.org",
                    "/vt/lyrs=s,h&amp;hl=zh-CN&amp;gl=CN&amp;src=app&amp;x={$x}&amp;y={$y}&amp;z={$z}&amp;scale=1",
                ),
                "https://sydt.hainasi.eu.org/vt/lyrs=s,h&hl=zh-CN&gl=CN&src=app&x=3345&y=1661&z=12&scale=1",
                CoordType::Gcj02,
                20,
            ),
            (
                xml(
                    "sydt-s",
                    "联通纯卫星",
                    20,
                    "Mercator(china)",
                    "JPG",
                    "sydt.hainasi.eu.org",
                    "/vt/lyrs=s&amp;hl=zh-CN&amp;gl=CN&amp;src=app&amp;x={$x}&amp;y={$y}&amp;z={$z}&amp;scale=1",
                ),
                "https://sydt.hainasi.eu.org/vt/lyrs=s&hl=zh-CN&gl=CN&src=app&x=3345&y=1661&z=12&scale=1",
                CoordType::Gcj02,
                20,
            ),
            (
                xml(
                    "sydt-m",
                    "联通街道图",
                    20,
                    "Mercator(china)",
                    "JPG",
                    "sydt.hainasi.eu.org",
                    "/vt/lyrs=m&amp;hl=zh-CN&amp;gl=CN&amp;src=app&amp;x={$x}&amp;y={$y}&amp;z={$z}&amp;scale=1",
                ),
                "https://sydt.hainasi.eu.org/vt/lyrs=m&hl=zh-CN&gl=CN&src=app&x=3345&y=1661&z=12&scale=1",
                CoordType::Gcj02,
                20,
            ),
            (
                xml(
                    "sydt-p",
                    "联通地形图",
                    20,
                    "Mercator(china)",
                    "JPG",
                    "sydt.hainasi.eu.org",
                    "/vt/lyrs=p&amp;hl=zh-CN&amp;gl=CN&amp;src=app&amp;x={$x}&amp;y={$y}&amp;z={$z}&amp;scale=1",
                ),
                "https://sydt.hainasi.eu.org/vt/lyrs=p&hl=zh-CN&gl=CN&src=app&x=3345&y=1661&z=12&scale=1",
                CoordType::Gcj02,
                20,
            ),
            (
                xml(
                    "amap-s",
                    "高德卫星",
                    18,
                    "Mercator(china)",
                    "JPG",
                    "webst02.is.autonavi.com",
                    "/appmaptile?style=6&amp;x={$x}&amp;y={$y}&amp;z={$z}",
                ),
                "https://webst02.is.autonavi.com/appmaptile?style=6&x=3345&y=1661&z=12",
                CoordType::Gcj02,
                18,
            ),
            (
                xml(
                    "amap-road",
                    "高德街道",
                    18,
                    "Mercator(china)",
                    "PNG",
                    "webrd02.is.autonavi.com",
                    "/appmaptile?lang=zh_cn&amp;size=1&amp;scale=1&amp;style=8&amp;x={$x}&amp;y={$y}&amp;z={$z}",
                ),
                "https://webrd02.is.autonavi.com/appmaptile?lang=zh_cn&size=1&scale=1&style=8&x=3345&y=1661&z=12",
                CoordType::Gcj02,
                18,
            ),
            (
                xml("geovis-img", "星图地球影像", 18, "Mercator", "JPG", "tiles1.geovisearth.com", "/base/v1/img/{$z}/{$x}/{$y}"),
                "https://tiles1.geovisearth.com/base/v1/img/12/3345/1661",
                CoordType::Wgs84,
                18,
            ),
            (
                xml("osm-proxy", "OSM 街道(反代加速)", 19, "Mercator", "PNG", "hzyt.hainasi.eu.org", "/tile/{$z}/{$x}/{$y}.png"),
                "https://hzyt.hainasi.eu.org/tile/12/3345/1661.png",
                CoordType::Wgs84,
                19,
            ),
            (
                xml("osm", "OSM 街道(备援)", 19, "Mercator", "PNG", "tile.openstreetmap.org", "/{$z}/{$x}/{$y}.png"),
                "https://tile.openstreetmap.org/12/3345/1661.png",
                CoordType::Wgs84,
                19,
            ),
            (
                xml(
                    "amap-ann",
                    "高德注记",
                    18,
                    "Mercator(china)",
                    "PNG",
                    "webst01.is.autonavi.com",
                    "/appmaptile?style=8&amp;x={$x}&amp;y={$y}&amp;z={$z}",
                ),
                "https://webst01.is.autonavi.com/appmaptile?style=8&x=3345&y=1661&z=12",
                CoordType::Gcj02,
                18,
            ),
        ];
        for (doc, expected_url, expected_coord, expected_zoom) in cases {
            let s = parse_ovi_custom_map_source(&doc, "测试署名").expect("the handed-out XML must import");
            let layer = s.to_tile_layer();
            assert_eq!(layer.tile_url(12, 3345, 1661), expected_url, "import must not rewrite the URL for {}", s.name);
            assert_eq!(s.coord_type, expected_coord, "coord type for {}", s.name);
            assert_eq!(s.max_zoom, expected_zoom, "max zoom for {}", s.name);
        }
    }
}
