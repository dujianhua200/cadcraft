//! `TILEMAP`: the basemap tile layer (`docs/ovimap-link.md` §9).
//!
//! Everything-is-a-command: JSON `run` + an interactive prompt machine.
//! The layer lives on the drawing (`Drawing.tilemap`, display-only, never
//! written to DXF). Missing tiles are fetched in the background by
//! [`crate::tilefetch::TileFetcher`] (OSM Tile Usage Policy: real User-Agent,
//! cache-first, ≤ 4 concurrent) into the disk cache; a cache miss draws a
//! blank placeholder until the fetch lands.
//!
//! JSON form:
//! `{}` → status. `{enabled: bool}` → on/off. `{action: "on"|"off"|"status"|"reset"}`
//! → the same. `{urlTemplate, attribution?}` → switch source: a custom source
//! (anything but the OSM default URL) **must** carry a non-empty `attribution`
//! in the same call (§9.5, never silently keep the OSM attribution for a
//! foreign source); switching back to the OSM URL restores its attribution
//! and its CARTO fallback unless fallback params are given in the same call.
//! `{fallbackUrlTemplate, fallbackAttribution?}` → set the fallback source
//! (tried when the primary fails, §9.4): a non-empty fallback URL **must**
//! carry a non-empty `fallbackAttribution` in the same call; an empty
//! `fallbackUrlTemplate` clears the fallback. `{opacity: 0..=1,
//! maxZoom?: 0..=30, zoom?: alias of maxZoom}` → settings. A custom source's `maxZoom` may exceed 19; the OSM
//! default never requests above z=19 (`TileLayer::validate`).

use cadcraft_doc::gis::{DEFAULT_TILE_ATTRIBUTION, DEFAULT_TILE_MAX_ZOOM, DEFAULT_TILE_URL, TileLayer};
use serde_json::{Value, json};

use super::*;
use crate::tilemap::{effective_tilemap, is_default_osm};
use crate::{Accept, Input, Interactive, Prompt, Result, Session, Step};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("tilemap", "Tilemap…", run_tilemap)
            .menu(&["Tools", "OviMap", "Tilemap…"])
            .params(
                "{} → status; {enabled?: bool} on/off; {action?: on|off|status|reset}; {urlTemplate?, attribution? (custom source: required)}; {fallbackUrlTemplate?, fallbackAttribution? (fallback source: required when set; empty urlTemplate clears)}; {opacity?: 0..=1, maxZoom? | zoom?: 0..=30} → {tilemap, message, network}",
            )
            .alias(&["tilemapset"])
            .noundo()
            .interactive(|_| Ok(Box::new(TilemapM::default()))),
    ]
}

/// The status payload: the layer plus how tiles arrive (background fetch, §9).
fn status(s: &Session) -> Result<Value> {
    let layer = s.doc().map(effective_tilemap).unwrap_or_else(|_| TileLayer::default_osm());
    Ok(json!({
        "tilemap": {
            "url_template": layer.url_template,
            "attribution": layer.attribution,
            "display_attribution": layer.display_attribution(),
            "fallback_url_template": layer.fallback_url_template,
            "fallback_attribution": layer.fallback_attribution,
            "opacity": layer.opacity,
            "enabled": layer.enabled,
            "max_zoom": layer.max_zoom,
            "default_osm": is_default_osm(&layer),
        },
        "network": format!("后台联网抓取：User-Agent CadCraft（OSM 瓦片使用政策），缓存优先，并发≤4；失败显示空白占位（等待抓取：{} 张）", crate::tilefetch::TileFetcher::global().pending()),
        "message": tilemap_message(&layer),
    }))
}

fn tilemap_message(layer: &TileLayer) -> String {
    if !layer.enabled {
        "<Tilemap off>".to_string()
    } else if is_default_osm(layer) {
        format!("<Tilemap on: OSM, 透明度 {}, 最高级别 {}，主源失败时自动试 CARTO 备用源>", layer.opacity, layer.max_zoom)
    } else if layer.fallback_layer().is_some() {
        format!("<Tilemap on: 自定义源 + 备用源, 透明度 {}, 最高级别 {}>", layer.opacity, layer.max_zoom)
    } else {
        format!("<Tilemap on: 自定义源, 透明度 {}, 最高级别 {}>", layer.opacity, layer.max_zoom)
    }
}

fn run_tilemap(s: &mut Session, p: &Value) -> Result<Value> {
    let mut layer = s.doc().map(effective_tilemap).unwrap_or_else(|_| TileLayer::default_osm());
    let mut changed = false;
    // Action verbs first.
    if let Some(action) = str_param(p, "action") {
        match action.to_ascii_lowercase().as_str() {
            "on" => {
                layer.enabled = true;
                changed = true;
            }
            "off" => {
                layer.enabled = false;
                changed = true;
            }
            "reset" => {
                layer = TileLayer::default_osm();
                changed = true;
            }
            "status" => {}
            other => return Err(bad("tilemap", format!("`action` 须为 on/off/status/reset 之一，得到 `{other}`"))),
        }
    }
    if let Some(enabled) = p.get("enabled").and_then(Value::as_bool) {
        layer.enabled = enabled;
        changed = true;
    }
    // Source switch: a custom URL without an attribution in the same call is
    // refused — the OSM attribution must never label a foreign source (§9.5).
    if let Some(template) = str_param(p, "urlTemplate") {
        let template = template.trim();
        if template.is_empty() {
            return Err(bad("tilemap", "`urlTemplate` 为空"));
        }
        if template != DEFAULT_TILE_URL {
            match str_param(p, "attribution") {
                Some(a) if !a.trim().is_empty() => layer.attribution = a.trim().to_string(),
                _ => {
                    return Err(bad("tilemap", "自定义瓦片源必须在同一次调用中提供非空 `attribution`（署名必填，见 ovimap-link §9.5）"));
                }
            }
        } else if let Some(a) = str_param(p, "attribution") {
            layer.attribution = if a.trim().is_empty() { DEFAULT_TILE_ATTRIBUTION.to_string() } else { a.trim().to_string() };
        } else {
            layer.attribution = DEFAULT_TILE_ATTRIBUTION.to_string();
        }
        // Switching to the OSM source re-applies its z≤19 cap (§9.1): a custom
        // source's higher cap must not leak into OSM requests.
        if template == DEFAULT_TILE_URL {
            layer.max_zoom = layer.max_zoom.min(DEFAULT_TILE_MAX_ZOOM);
            if str_param(p, "fallbackUrlTemplate").is_none() {
                // Switching back to OSM restores its CARTO fallback too, unless
                // the caller sets a fallback explicitly in this same call.
                let d = TileLayer::default_osm();
                layer.fallback_url_template = d.fallback_url_template;
                layer.fallback_attribution = d.fallback_attribution;
            }
        }
        layer.url_template = template.to_string();
        changed = true;
    } else if let Some(a) = str_param(p, "attribution") {
        layer.attribution = a.trim().to_string();
        changed = true;
    }
    // Fallback switch: a non-empty fallback URL without its attribution in the
    // same call is refused; an empty fallback URL clears the fallback.
    if let Some(fb) = str_param(p, "fallbackUrlTemplate") {
        let fb = fb.trim();
        if fb.is_empty() {
            layer.fallback_url_template = None;
            layer.fallback_attribution = None;
        } else {
            match str_param(p, "fallbackAttribution") {
                Some(a) if !a.trim().is_empty() => {
                    layer.fallback_url_template = Some(fb.to_string());
                    layer.fallback_attribution = Some(a.trim().to_string());
                }
                _ => {
                    return Err(bad("tilemap", "备用瓦片源必须在同一次调用中提供非空 `fallbackAttribution`（署名必填，见 ovimap-link §9.5）"));
                }
            }
        }
        changed = true;
    } else if str_param(p, "fallbackAttribution").is_some() {
        return Err(bad("tilemap", "`fallbackAttribution` 须与 `fallbackUrlTemplate` 一起提供"));
    }
    if let Some(opacity) = p.get("opacity").and_then(Value::as_f64) {
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(bad("tilemap", "`opacity` 须在 0~1 之间"));
        }
        layer.opacity = opacity;
        changed = true;
    }
    // `zoom` is the user-facing name for the requested zoom cap (`maxZoom`).
    let zoom = p.get("maxZoom").or_else(|| p.get("zoom")).and_then(Value::as_u64);
    if let Some(z) = zoom {
        if z > 30 {
            return Err(bad("tilemap", "`maxZoom` 须 ≤ 30"));
        }
        layer.max_zoom = u32::try_from(z).unwrap_or(DEFAULT_TILE_MAX_ZOOM);
        changed = true;
    }
    // Full-layer validation (template placeholders, attribution-when-enabled,
    // OSM z≤19 cap). An invalid layer is never stored.
    layer.validate().map_err(|e| bad("tilemap", e))?;
    if changed {
        s.doc_mut()?.tilemap = Some(layer);
        s.touch();
    }
    let mut out = status(s)?;
    if changed {
        out["message"] = json!(tilemap_message(&s.doc().map(effective_tilemap).unwrap_or_else(|_| TileLayer::default_osm())));
    }
    Ok(out)
}

/// TILEMAP at the command line: pick an action, then the value it needs.
#[derive(Default)]
struct TilemapM {
    stage: Stage,
    pending_url: String,
}

#[derive(Default, PartialEq)]
enum Stage {
    #[default]
    Action,
    Url,
    Attribution,
    Opacity,
    Zoom,
}

impl Interactive for TilemapM {
    fn name(&self) -> &'static str {
        "TILEMAP"
    }

    fn prompt(&self, s: &Session) -> Prompt {
        let layer = s.doc().map(effective_tilemap).unwrap_or_else(|_| TileLayer::default_osm());
        match self.stage {
            Stage::Action => Prompt::new(
                format!("底图瓦片 ({}) [开/关/图源/透明度/缩放/状态]", if layer.enabled { "开" } else { "关" }),
                Accept { enter: true, ..Accept::default() },
            )
            .kw(&["On", "Off", "Source", "Opacity", "Zoom", "Status"])
            .default("Status"),
            Stage::Url => Prompt::new("瓦片 URL 模板（含 {z}/{x}/{y}，回车恢复 OSM 默认）:", Accept::TEXT),
            Stage::Attribution => Prompt::new("自定义图源署名（必填，原样显示）:", Accept::TEXT),
            Stage::Opacity => Prompt::new(format!("透明度 0~1（当前 {}）:", layer.opacity), Accept::TEXT),
            Stage::Zoom => Prompt::new(format!("最高级别 0~30（当前 {}，OSM 默认 ≤19）:", layer.max_zoom), Accept::TEXT),
        }
    }

    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Cancel => return Ok(Step::Cancel),
            Input::Enter if self.stage == Stage::Action => {
                return self.finish(s, json!({}));
            }
            Input::Enter => return Ok(Step::Continue),
            _ => {}
        }
        match self.stage {
            Stage::Action => {
                let kw = match i {
                    Input::Keyword(k) | Input::Text(k) => k,
                    _ => return Ok(Step::Continue),
                };
                let kw = kw.trim().to_ascii_lowercase();
                if "on".starts_with(&kw) && !kw.is_empty() {
                    return self.finish(s, json!({ "enabled": true }));
                }
                if "off".starts_with(&kw) && !kw.is_empty() {
                    return self.finish(s, json!({ "enabled": false }));
                }
                if "source".starts_with(&kw) && !kw.is_empty() {
                    self.stage = Stage::Url;
                    return Ok(Step::Continue);
                }
                if "opacity".starts_with(&kw) && !kw.is_empty() {
                    self.stage = Stage::Opacity;
                    return Ok(Step::Continue);
                }
                if "zoom".starts_with(&kw) && !kw.is_empty() {
                    self.stage = Stage::Zoom;
                    return Ok(Step::Continue);
                }
                if "status".starts_with(&kw) && !kw.is_empty() {
                    return self.finish(s, json!({}));
                }
                s.echo("未知选项：请输入 开/关/图源/透明度/缩放/状态".to_string());
                Ok(Step::Continue)
            }
            Stage::Url => {
                let text = match i {
                    Input::Keyword(k) | Input::Text(k) => k,
                    _ => return Ok(Step::Continue),
                };
                let text = text.trim().to_string();
                if text.is_empty() {
                    return self.finish(s, json!({ "urlTemplate": DEFAULT_TILE_URL }));
                }
                self.pending_url = text;
                self.stage = Stage::Attribution;
                Ok(Step::Continue)
            }
            Stage::Attribution => {
                let text = match i {
                    Input::Keyword(k) | Input::Text(k) => k,
                    _ => return Ok(Step::Continue),
                };
                let params = json!({ "urlTemplate": self.pending_url, "attribution": text.trim() });
                match run_tilemap(s, &params) {
                    Ok(v) => {
                        if let Some(m) = v.get("message").and_then(Value::as_str) {
                            s.echo(m.to_string());
                        }
                        Ok(Step::Done)
                    }
                    Err(e) => {
                        s.echo(format!("{e}，请重新输入署名"));
                        Ok(Step::Continue)
                    }
                }
            }
            Stage::Opacity => {
                let text = match i {
                    Input::Keyword(k) | Input::Text(k) => k,
                    _ => return Ok(Step::Continue),
                };
                match text.trim().parse::<f64>() {
                    Ok(v) => self.finish(s, json!({ "opacity": v })),
                    Err(_) => {
                        s.echo("透明度须为数字（0~1）".to_string());
                        Ok(Step::Continue)
                    }
                }
            }
            Stage::Zoom => {
                let text = match i {
                    Input::Keyword(k) | Input::Text(k) => k,
                    _ => return Ok(Step::Continue),
                };
                match text.trim().parse::<u64>() {
                    Ok(v) => self.finish(s, json!({ "maxZoom": v })),
                    Err(_) => {
                        s.echo("最高级别须为整数（0~30）".to_string());
                        Ok(Step::Continue)
                    }
                }
            }
        }
    }
}

impl TilemapM {
    fn finish(&mut self, s: &mut Session, params: Value) -> Result<Step> {
        let v = run_tilemap(s, &params)?;
        if let Some(m) = v.get("message").and_then(Value::as_str) {
            s.echo(m.to_string());
        }
        Ok(Step::Done)
    }
}
