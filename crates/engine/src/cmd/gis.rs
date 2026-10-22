//! OviMap link commands (`docs/ovimap-link.md` §7.2, C5, C15): `ovimap.import`,
//! `ovimap.export`, `ovimap.inspect`, plus `ovimap.assign` (give a CadCraft-native
//! entity an OviMap identity, §5.4), `ovimap.georef` (set the drawing's geographic
//! reference, §3), and the sync bridge `ovimap.push` / `ovimap.pull` (§7.3:
//! the JSON envelope, never bare GeoJSON; 409 → conflict, never an overwrite).
//!
//! Import is an upsert by `ovimap_id` (§6.1): a repeated import of the same file is
//! a no-op (idempotent, §6.5). Deletion only happens through `ovimap_deletes` +
//! `delete_mode == "sync"` (§6.3); otherwise deletes are ignored with a warning.
//! CadCraft-side import never reports conflicts (§6.4 is ovimap's rule when it
//! imports CadCraft's export: CadCraft has no sync stamp to compare against).
//! Programmatic calls never open dialogs: file picking is the caller's job (the UI
//! asks for the file, like IMPORT/EXPORT).

use std::collections::{BTreeMap, BTreeSet};

use cadcraft_doc::gis::{self, GeoRef, OvimapExt};
use cadcraft_doc::{Common, Drawing, EntityKind, Handle, Space};
use serde_json::{Value, json};

use super::file::{base64_decode, base64_encode, io};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("ovimap.import", "Import OviMap GeoJSON…", run_import)
            .menu(&["File", "Import", "OviMap GeoJSON…"])
            .params("{path | text (raw GeoJSON) | data (base64), name?: \"x.ovimap.geojson\" | \"x.dxf\"} → import report {added, updated, skipped, deleted, conflicts, missingPhotos, warnings}")
            .alias(&["ovimapimport"]),
        CommandSpec::new("ovimap.export", "Export OviMap…", run_export)
            .menu(&["File", "Export", "OviMap…"])
            .params("{path?, format?: \"geojson\" | \"dxf\" (default geojson)} → {path? | geojson | data (base64 DXF), features}")
            .alias(&["ovimapexport"])
            .noundo(),
        CommandSpec::new("ovimap.inspect", "Inspect OviMap Link…", run_inspect)
            .menu(&["Tools", "OviMap", "Inspect Link…"])
            .params("{} → {georef, counts, missingPhotos, warnings, tilemap}")
            .noundo(),
        CommandSpec::new("ovimap.assign", "Assign OviMap Identity", run_assign)
            .menu(&["Tools", "OviMap", "Assign OviMap Identity"])
            .params("{handles? (default: selection), kind?: marker|track|label|photo, type?: (default \"none\")} → {assigned: [{handle, ovimap_id}], skipped, warnings}")
            .alias(&["ovimapassign"]),
        CommandSpec::new("ovimap.georef", "Set OviMap Georef…", run_georef)
            .menu(&["Tools", "OviMap", "Set Georef…"])
            .params("{originLat, originLon, centralMeridian? (= originLon)} → {georef}")
            .alias(&["ovimapgeoref"])
            .interactive(|_| Ok(Box::<GeorefM>::default())),
        CommandSpec::new("ovimap.push", "Push to OviMap…", run_push)
            .menu(&["Tools", "OviMap", "Push to OviMap…"])
            .params("{projectId, baseRev? (= 0), geojson? (default: export of the current drawing), dxfBase64? | dxf?: true (export DXF too), baseUrl? | token? | deviceId? | deviceName? (else CADCRAFT_OVIMAP_* env)} → {outcome: updated | conflict, rev?, serverGeojson?, serverDxfBase64?}; 409 → conflict, never an overwrite")
            .alias(&["ovimappush"])
            .noundo()
            .interactive(|_| Ok(Box::new(ProjectIdM { push: true }))),
        CommandSpec::new("ovimap.pull", "Pull from OviMap…", run_pull)
            .menu(&["Tools", "OviMap", "Pull from OviMap…"])
            .params("{projectId, baseUrl? | token? | deviceId? | deviceName? (else CADCRAFT_OVIMAP_* env)} → {rev, updatedAt, geojson, dxfBase64?, hasDxf}; returns the server copy only — import it with ovimap.import")
            .alias(&["ovimappull"])
            .noundo()
            .interactive(|_| Ok(Box::new(ProjectIdM { push: false }))),
    ]
}

fn read_file(path: &str) -> Result<Vec<u8>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::fs::read(path).map_err(|e| bad("ovimap.import", format!("{path}: {e}")))
    }
    #[cfg(target_arch = "wasm32")]
    Err(bad("ovimap.import", "paths are not available on the web; pass `text` (GeoJSON) or `data` (base64)"))
}

fn write_file(cmd: &str, path: &str, bytes: &[u8]) -> Result<()> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let tmp = format!("{path}.cadcraft-tmp");
        std::fs::write(&tmp, bytes).map_err(|e| bad(cmd, format!("{path}: {e}")))?;
        std::fs::rename(&tmp, path).map_err(|e| bad(cmd, format!("{path}: {e}")))?;
        Ok(())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (path, bytes);
        Err(bad(cmd, "paths are not available on the web; omit `path` to get the file back"))
    }
}

/// The drawing's directory (its file's parent), for the photo lookup (§5.5).
fn drawing_dir(s: &Session) -> Option<String> {
    let path = s.state().ok().and_then(|st| st.path.clone())?;
    std::path::Path::new(&path).parent().map(|p| p.to_string_lossy().to_string())
}

/// Ensure an OviMap layer exists (never delete or modify a user's existing layer,
/// §4.5); a newly created one gets the spec's display colour.
fn ensure_ovimap_layer(d: &mut Drawing, layer: &str) {
    if d.ensure_layer(layer)
        && let Some(c) = gis::layer_color(layer)
        && let Some(l) = d.layer_mut(layer)
    {
        l.color = c;
    }
}

/// All photo file names a drawing references (feature + vertex photos).
fn all_photos(ext: &BTreeMap<String, OvimapExt>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for e in ext.values() {
        out.extend(e.photos.iter().cloned());
        for v in &e.vertices {
            if let Some(photos) = v.get("photos").and_then(Value::as_array) {
                out.extend(photos.iter().filter_map(Value::as_str).map(str::to_string));
            }
        }
    }
    out
}

/// Photo files missing from the drawing's directory (and its `photos/` subdir, §5.5).
/// Without a saved drawing path nothing can be checked: a warning, not a missing list.
fn missing_photos(ext: &BTreeMap<String, OvimapExt>, dir: Option<&str>) -> (Vec<String>, Vec<String>) {
    let Some(dir) = dir else {
        return (Vec::new(), vec!["图纸未保存，无法核查照片文件是否存在".to_string()]);
    };
    #[cfg(target_arch = "wasm32")]
    {
        let _ = dir;
        return (Vec::new(), vec!["网页版无法核查照片文件是否存在".to_string()]);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut missing = Vec::new();
        for name in all_photos(ext) {
            let here = std::path::Path::new(dir).join(&name).exists();
            let sub = std::path::Path::new(dir).join("photos").join(&name).exists();
            if !here && !sub {
                missing.push(name);
            }
        }
        (missing, Vec::new())
    }
}

/// One incoming feature's entities + extension area, as produced by a GeoJSON or DXF
/// import, ready for upsert.
struct Incoming {
    entities: Vec<(Common, EntityKind)>,
    ext: BTreeMap<String, OvimapExt>,
    georef: Option<GeoRef>,
    meta: Option<Value>,
    deletes: Vec<String>,
    delete_mode: String,
    warnings: Vec<String>,
}

/// The canonical form of one entity for upsert comparison (§6.2): geometry + identity.
/// TEXT position, height, colour and layer are decoration and never count as a change.
fn canonical_entity(_common: &Common, kind: &EntityKind) -> String {
    let num = |x: f64| x.to_string();
    match kind {
        EntityKind::Point(p) => format!("P:{}:{}", num(p.p.x), num(p.p.y)),
        EntityKind::LwPolyline(pl) => {
            let verts: Vec<String> = pl.vertices.iter().map(|v| format!("{},{}", num(v.p.x), num(v.p.y))).collect();
            format!("L:{}:{}", pl.closed, verts.join(";"))
        }
        EntityKind::Text(t) => format!("T:{}", t.value),
        EntityKind::MText(t) => format!("M:{}", gis::mtext_plain(&t.contents)),
        other => format!("U:{other:?}"),
    }
}

/// The canonical form of one feature group: identity + effective name + entities.
/// The effective name is a paired TEXT's content when one exists (the user may have
/// edited the text in CAD, §5.3), else the xdata name.
fn canonical_group(group: &[(&Common, &EntityKind)]) -> Option<String> {
    let mut id = String::new();
    let mut kind = String::new();
    let mut type_ = String::new();
    let mut xdata_name = String::new();
    let mut text_name: Option<String> = None;
    let mut parts: Vec<String> = Vec::new();
    for (common, entity_kind) in group {
        let Some(xd) = gis::parse_ovimap_xdata(&common.xdata) else { continue };
        if id.is_empty() {
            id = xd.id.clone();
            kind = xd.kind.clone();
            type_ = xd.type_.clone();
            xdata_name = xd.name.clone();
        }
        match entity_kind {
            EntityKind::Text(t) => {
                if !common.layer.eq_ignore_ascii_case(gis::LAYER_PHOTO) && !t.value.is_empty() {
                    text_name = Some(t.value.clone());
                }
            }
            EntityKind::MText(t) => {
                let plain = gis::mtext_plain(&t.contents);
                if !plain.is_empty() {
                    text_name = Some(plain);
                }
            }
            _ => {}
        }
        parts.push(canonical_entity(common, entity_kind));
    }
    if id.is_empty() {
        return None;
    }
    parts.sort();
    Some(format!("{id}|{kind}|{type_}|{}|{}", text_name.unwrap_or(xdata_name), parts.join("|")))
}

/// Upsert `incoming` into the current drawing (§6.1): new features are added, changed
/// ones replaced, identical ones skipped. Returns the import report.
fn upsert(s: &mut Session, incoming: Incoming) -> Result<Value> {
    // Geographic reference: the file's wins (§2.2/§3.1); a drawing that already has a
    // different one refuses, rather than silently mixing two projections.
    if let Some(g) = incoming.georef {
        let d = s.doc()?;
        match &d.georef {
            None => {
                s.doc_mut()?.georef = Some(g);
            }
            Some(old) if *old != g => {
                return Err(bad(
                    "ovimap.import",
                    "图纸已有不同的地理参照（原点/中央子午线不一致），请新建图纸后再导入，或先用 ovimap.georef 统一地理参照",
                ));
            }
            Some(_) => {}
        }
    } else if s.doc()?.georef.is_none() {
        return Err(bad("ovimap.import", "文件未携带地理参照，且图纸未设置地理参照：请先运行 ovimap.georef 设置原点经纬度"));
    }
    if let Some(meta) = incoming.meta {
        s.doc_mut()?.ovimap_meta = Some(meta);
    }
    // Incoming groups, in first-seen order.
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<(Common, EntityKind)>> = BTreeMap::new();
    for (common, kind) in incoming.entities {
        let Some(xd) = gis::parse_ovimap_xdata(&common.xdata) else { continue };
        if !groups.contains_key(&xd.id) {
            order.push(xd.id.clone());
        }
        groups.entry(xd.id).or_default().push((common, kind));
    }
    let mut added = 0usize;
    let mut updated = 0usize;
    let mut skipped = 0usize;
    let mut warnings = incoming.warnings.clone();
    for id in &order {
        let group = groups.get(id).cloned().unwrap_or_default();
        let refs: Vec<(&Common, &EntityKind)> = group.iter().map(|(c, k)| (c, k)).collect();
        let new_form = canonical_group(&refs);
        // Existing entities with this id (model space only).
        let existing: Vec<(Handle, Common, EntityKind)> = {
            let d = s.doc()?;
            d.model
                .iter()
                .filter(|e| gis::parse_ovimap_xdata(&e.common.xdata).is_some_and(|xd| &xd.id == id))
                .map(|e| (e.handle, e.common.clone(), e.kind.clone()))
                .collect()
        };
        let old_refs: Vec<(&Common, &EntityKind)> = existing.iter().map(|(_, c, k)| (c, k)).collect();
        if !existing.is_empty() && canonical_group(&old_refs) == new_form {
            skipped += 1;
            continue;
        }
        let was_present = !existing.is_empty();
        for (h, _, _) in &existing {
            s.doc_mut()?.remove_entity(*h);
        }
        for (common, kind) in &group {
            ensure_ovimap_layer(s.doc_mut()?, &common.layer);
            s.doc_mut()?.add(&Space::Model, common.clone(), kind.clone()).map_err(|e| EngineError::Other(e.to_string()))?;
        }
        if let Some(e) = incoming.ext.get(id) {
            s.doc_mut()?.ovimap_ext.insert(id.clone(), e.clone());
        }
        if was_present {
            updated += 1;
        } else {
            added += 1;
        }
    }
    // Deletes: only with delete_mode == "sync" (§6.3). The report shape is fixed
    // (§10.3: no `deleted` field), so deletions are reported via warnings.
    if !incoming.deletes.is_empty() {
        if incoming.delete_mode == "sync" {
            let mut deleted_ids: Vec<String> = Vec::new();
            for id in &incoming.deletes {
                let handles: Vec<Handle> = {
                    let d = s.doc()?;
                    d.model.iter().filter(|e| gis::parse_ovimap_xdata(&e.common.xdata).is_some_and(|xd| &xd.id == id)).map(|e| e.handle).collect()
                };
                if handles.is_empty() {
                    continue;
                }
                for h in handles {
                    s.doc_mut()?.remove_entity(h);
                }
                s.doc_mut()?.ovimap_ext.remove(id);
                deleted_ids.push(id.clone());
            }
            if !deleted_ids.is_empty() {
                warnings.push(format!("已删除 {} 个要素（delete_mode=sync）：{}", deleted_ids.len(), deleted_ids.join(", ")));
            }
        } else {
            warnings.push(format!(
                "ovimap_deletes 已忽略：delete_mode 不是 sync（删除默认关闭，见 ovimap-link §6.3）；涉及 {} 个 id",
                incoming.deletes.len()
            ));
        }
    }
    // Prune extension areas whose feature no longer has an entity in model space.
    let live: BTreeSet<String> = {
        let d = s.doc()?;
        d.model.iter().filter_map(|e| gis::parse_ovimap_xdata(&e.common.xdata).map(|xd| xd.id)).collect()
    };
    s.doc_mut()?.ovimap_ext.retain(|id, _| live.contains(id));
    // Photo check against the (possibly new) extension areas.
    let (missing, photo_warnings) = missing_photos(&s.doc()?.ovimap_ext, drawing_dir(s).as_deref());
    let mut all_warnings = warnings;
    all_warnings.extend(photo_warnings);
    Ok(json!({
        "added": added,
        "updated": updated,
        "skipped": skipped,
        "conflicts": [],
        "missingPhotos": missing,
        "warnings": all_warnings,
        "message": format!("导入完成：新增 {added}，更新 {updated}，跳过 {skipped}"),
    }))
}

fn run_import(s: &mut Session, p: &Value) -> Result<Value> {
    let (bytes, name) = if let Some(path) = str_param(p, "path") {
        (read_file(path)?, path.to_string())
    } else if let Some(text) = str_param(p, "text") {
        (text.as_bytes().to_vec(), str_param(p, "name").unwrap_or("import.ovimap.geojson").to_string())
    } else {
        let data = str_param(p, "data").ok_or_else(|| bad("ovimap.import", "`path`、`text`（GeoJSON 文本）或 `data`（base64）须给一个"))?;
        (
            base64_decode(data).ok_or_else(|| bad("ovimap.import", "invalid base64"))?,
            str_param(p, "name").unwrap_or("import.ovimap.geojson").to_string(),
        )
    };
    let is_dxf = name.to_ascii_lowercase().ends_with(".dxf");
    if is_dxf {
        let hooks = io().ok_or_else(|| bad("ovimap.import", "file formats are not available in this build"))?;
        let src = (hooks.read)(&bytes, &name).map_err(|e| bad("ovimap.import", format!("DXF 读取失败：{e}")))?;
        // $INSUNITS must be 6 (metres) or absent (treated as 6, §4.5).
        if let Some(insunits) = src.header.get("INSUNITS").and_then(|v| v.as_i64())
            && insunits != 6
        {
            return Err(bad("ovimap.import", format!("DXF 的 $INSUNITS 为 {insunits}，须为 6（米）；其他单位会按错误单位误算，已拒绝导入")));
        }
        let entities: Vec<(Common, EntityKind)> =
            src.model.iter().filter(|e| gis::parse_ovimap_xdata(&e.common.xdata).is_some()).map(|e| (e.common.clone(), e.kind.clone())).collect();
        if entities.is_empty() {
            return Err(bad("ovimap.import", "DXF 里没有带 OVIMAP xdata 的实体（无联动数据）"));
        }
        // Rebuild extension areas: DXF keeps only vertex ids (§4.6); photo lists come
        // back from the PHOTO: … hint texts, matched by position.
        let vertex_ids: BTreeMap<String, Vec<String>> = src
            .ovimap_ext
            .iter()
            .map(|(id, e)| (id.clone(), e.vertices.iter().filter_map(|v| v.get("ovimap_id").and_then(|x| x.as_str()).map(str::to_string)).collect()))
            .filter(|(_, ids): &(String, Vec<String>)| !ids.is_empty())
            .collect();
        let ext = gis::rebuild_ovimap_ext(&entities, &vertex_ids);
        let mut warnings = Vec::new();
        if !src.read_notes.is_empty() {
            warnings.extend(src.read_notes.clone());
        }
        // A track whose VERTICES entry is missing gets fresh vertex ids on export;
        // warn now, naming the track (degraded import, §4.6).
        for (common, _) in &entities {
            if let Some(xd) = gis::parse_ovimap_xdata(&common.xdata)
                && xd.kind == "track"
                && !vertex_ids.contains_key(&xd.id)
            {
                warnings.push(format!("轨迹 `{}` 缺少 VERTICES XRECORD（文件经第三方转存被剥离），导出时将为顶点生成新 ID", xd.id));
            }
        }
        upsert(s, Incoming { entities, ext, georef: src.georef, meta: None, deletes: Vec::new(), delete_mode: "none".into(), warnings })
    } else {
        let text = String::from_utf8(bytes).map_err(|_| bad("ovimap.import", "GeoJSON 不是有效的 UTF-8 文本"))?;
        let imp = gis::geojson_to_entities(&text).map_err(|e| bad("ovimap.import", e))?;
        // Warn about track ids that DXF cannot carry as dictionary keys (R2): the
        // GeoJSON import itself stays valid, but a later DXF export would refuse.
        let mut warnings = imp.warnings.clone();
        for (id, e) in &imp.ext {
            if !e.vertices.is_empty() {
                let key = format!("{}{}", gis::VERTICES_PREFIX, id);
                if !gis::valid_dict_key(&key) {
                    warnings.push(format!("轨迹 `{id}` 的 DXF 字典键 `{key}` 含 DXF 不接受的字符，DXF 导出将拒绝该文件（不得静默改名）"));
                }
            }
        }
        upsert(
            s,
            Incoming {
                entities: imp.entities,
                ext: imp.ext,
                georef: Some(imp.georef),
                meta: Some(imp.meta),
                deletes: imp.deletes,
                delete_mode: imp.delete_mode,
                warnings,
            },
        )
    }
}

fn run_export(s: &mut Session, p: &Value) -> Result<Value> {
    let format = str_param(p, "format").unwrap_or("geojson").trim().to_ascii_lowercase();
    if format != "geojson" && format != "dxf" {
        return Err(bad("ovimap.export", "`format` 须为 geojson 或 dxf"));
    }
    let mut georef = s.doc()?.georef;
    if georef.is_none()
        && let Some(g) = p.get("georef")
    {
        let parsed = GeoRef::from_json(g).map_err(|e| bad("ovimap.export", e))?;
        s.doc_mut()?.georef = Some(parsed);
        georef = Some(parsed);
    }
    let Some(georef) = georef else {
        return Err(bad("ovimap.export", "图纸未设置地理参照：请先导入带 ovimap_georef 的文件，或用 ovimap.georef 设置原点经纬度"));
    };
    let path = str_param(p, "path");
    if format == "geojson" {
        let d = s.doc()?;
        let items: Vec<(&Common, &EntityKind)> = d.model.iter().map(|e| (&e.common, &e.kind)).collect();
        let meta = d.ovimap_meta.clone().unwrap_or(Value::Null);
        let text = gis::entities_to_geojson(items, &georef, &meta, &d.ovimap_ext).map_err(|e| bad("ovimap.export", e))?;
        let features: BTreeSet<String> = d.model.iter().filter_map(|e| gis::parse_ovimap_xdata(&e.common.xdata).map(|xd| xd.id)).collect();
        let features = features.len();
        return match path {
            Some(path) => {
                write_file("ovimap.export", path, text.as_bytes())?;
                Ok(
                    json!({ "path": path, "format": "geojson", "bytes": text.len(), "features": features, "message": format!("已导出 GeoJSON 到 {path}") }),
                )
            }
            None => Ok(json!({ "geojson": text, "format": "geojson", "features": features })),
        };
    }
    // DXF: validate the VERTICES keys first (R2: fail, never silently rename), then
    // write a filtered drawing with only the link entities.
    {
        let d = s.doc()?;
        for (id, e) in &d.ovimap_ext {
            if e.vertices.is_empty() {
                continue;
            }
            let key = format!("{}{}", gis::VERTICES_PREFIX, id);
            if !gis::valid_dict_key(&key) {
                return Err(bad(
                    "ovimap.export",
                    format!("轨迹 `{id}` 的 DXF 字典键 `{key}` 含 DXF 不接受的字符，DXF 导出已拒绝（不得静默改名，见 ovimap-link §4.3/R2）"),
                ));
            }
            for v in &e.vertices {
                if v.get("ovimap_id").and_then(|x| x.as_str()).is_none_or(str::is_empty) {
                    return Err(bad("ovimap.export", format!("轨迹 `{id}` 有顶点缺少 ovimap_id，DXF 导出已拒绝")));
                }
            }
        }
    }
    let hooks = io().ok_or_else(|| bad("ovimap.export", "file formats are not available in this build"))?;
    let mut filtered = s.doc()?.clone();
    let drop: Vec<Handle> = filtered.model.iter().filter(|e| gis::parse_ovimap_xdata(&e.common.xdata).is_none()).map(|e| e.handle).collect();
    for h in drop {
        filtered.remove_entity(h);
    }
    filtered.header.set_i64("INSUNITS", 6);
    let live: BTreeSet<String> = filtered.model.iter().filter_map(|e| gis::parse_ovimap_xdata(&e.common.xdata).map(|xd| xd.id)).collect();
    filtered.ovimap_ext.retain(|id, _| live.contains(id));
    let bytes = (hooks.write)(&filtered, "export.dxf").map_err(|e| bad("ovimap.export", format!("DXF 写出失败：{e}")))?;
    match path {
        Some(path) => {
            write_file("ovimap.export", path, &bytes)?;
            Ok(json!({ "path": path, "format": "dxf", "bytes": bytes.len(), "message": format!("已导出 DXF 到 {path}") }))
        }
        None => Ok(json!({ "data": base64_encode(&bytes), "format": "dxf", "bytes": bytes.len() })),
    }
}

fn run_inspect(s: &mut Session, _p: &Value) -> Result<Value> {
    let d = s.doc()?;
    let mut markers = BTreeSet::new();
    let mut tracks = BTreeSet::new();
    let mut labels = BTreeSet::new();
    let mut photos = BTreeSet::new();
    let mut entities = 0usize;
    for e in d.model.iter() {
        let Some(xd) = gis::parse_ovimap_xdata(&e.common.xdata) else { continue };
        entities += 1;
        match xd.kind.as_str() {
            "marker" => {
                markers.insert(xd.id);
            }
            "track" => {
                tracks.insert(xd.id);
            }
            "label" => {
                labels.insert(xd.id);
            }
            "photo" => {
                photos.insert(xd.id);
            }
            _ => {}
        }
    }
    let mut warnings = Vec::new();
    if d.georef.is_none() {
        warnings.push("图纸未设置地理参照（ovimap.import 会从文件采用，ovimap.export 需要它）".to_string());
    }
    let (missing, mut photo_warnings) = missing_photos(&d.ovimap_ext, drawing_dir(s).as_deref());
    warnings.append(&mut photo_warnings);
    let tilemap = d.tilemap.clone().unwrap_or_else(gis::TileLayer::default_osm);
    Ok(json!({
        "georef": d.georef.map(|g| g.to_json()).unwrap_or(Value::Null),
        "counts": { "marker": markers.len(), "track": tracks.len(), "label": labels.len(), "photo": photos.len(), "entities": entities },
        "missingPhotos": missing,
        "warnings": warnings,
        "tilemap": { "url_template": tilemap.url_template, "attribution": tilemap.attribution, "display_attribution": tilemap.display_attribution(), "fallback_url_template": tilemap.fallback_url_template, "fallback_attribution": tilemap.fallback_attribution, "opacity": tilemap.opacity, "enabled": tilemap.enabled, "max_zoom": tilemap.max_zoom },
    }))
}

fn run_assign(s: &mut Session, p: &Value) -> Result<Value> {
    let kind_param = str_param(p, "kind");
    let type_param = str_param(p, "type").unwrap_or("none");
    if let Some(k) = kind_param
        && !["marker", "track", "label", "photo"].contains(&k)
    {
        return Err(bad("ovimap.assign", format!("`kind` 须为 marker/track/label/photo 之一，得到 `{k}`")));
    }
    let targets = match p.get("handles") {
        Some(Value::Array(a)) => {
            let mut out = Vec::new();
            for v in a {
                let h = v
                    .as_str()
                    .and_then(Handle::parse_hex)
                    .or_else(|| v.as_u64().map(Handle))
                    .ok_or_else(|| bad("ovimap.assign", format!("`handles` 中的 {v} 不是对象句柄")))?;
                out.push(h);
            }
            out
        }
        _ => s.selection(),
    };
    if targets.is_empty() {
        return Err(bad("ovimap.assign", "没有选中对象：请先选中要赋予身份的实体，或传 `handles`"));
    }
    let mut assigned = Vec::new();
    let mut skipped = 0usize;
    let mut warnings = Vec::new();
    for h in targets {
        let (common, kind) = {
            let d = s.doc()?;
            let Some(e) = d.model.iter().find(|e| e.handle == h).or_else(|| d.layouts.iter().flat_map(|l| l.entities.iter()).find(|e| e.handle == h))
            else {
                warnings.push(format!("句柄 {} 不存在，已跳过", h.hex()));
                skipped += 1;
                continue;
            };
            (e.common.clone(), e.kind.clone())
        };
        if gis::parse_ovimap_xdata(&common.xdata).is_some() {
            warnings.push(format!("句柄 {} 已有 OVIMAP 身份，未覆盖", h.hex()));
            skipped += 1;
            continue;
        }
        let derived = match &kind {
            EntityKind::Point(_) => "marker",
            EntityKind::LwPolyline(_) => "track",
            EntityKind::Text(_) | EntityKind::MText(_) => "label",
            _ => {
                warnings.push(format!("句柄 {} 是 {:?} 类型，不能赋予 ovimap 身份（仅 POINT / LWPOLYLINE / TEXT / MTEXT）", h.hex(), kind));
                skipped += 1;
                continue;
            }
        };
        let want = kind_param.unwrap_or(derived);
        let ok = matches!(
            (&kind, want),
            (EntityKind::Point(_), "marker" | "photo")
                | (EntityKind::LwPolyline(_), "track")
                | (EntityKind::Text(_) | EntityKind::MText(_), "label" | "marker")
        );
        if !ok {
            return Err(bad("ovimap.assign", format!("句柄 {} 是 {:?}，不能赋予 kind=`{want}` 身份", h.hex(), kind)));
        }
        let id = gis::new_uuid_v4();
        let mut ext = OvimapExt::default();
        if want == "track"
            && let EntityKind::LwPolyline(pl) = &kind
        {
            ext.vertices = pl.vertices.iter().map(|_| json!({ "ovimap_id": gis::new_uuid_v4() })).collect();
        }
        let d = s.doc_mut()?;
        d.modify_entity(h, |e| {
            e.common.xdata = gis::ovimap_xdata(&id, "", want, type_param);
        })
        .map_err(|e| EngineError::Other(e.to_string()))?;
        // Keep the layer's link membership explicit: a native entity on another layer
        // keeps its layer (layers are display, not identity); only its xdata changes.
        d.ovimap_ext.insert(id.clone(), ext);
        assigned.push(json!({ "handle": h.hex(), "ovimap_id": id }));
    }
    Ok(json!({ "assigned": assigned, "skipped": skipped, "warnings": warnings }))
}

fn run_georef(s: &mut Session, p: &Value) -> Result<Value> {
    let lat = f64_req("ovimap.georef", p, "originLat")?;
    let lon = f64_req("ovimap.georef", p, "originLon")?;
    let cm = match p.get("centralMeridian") {
        None | Some(Value::Null) => lon,
        Some(v) => v.as_f64().filter(|x| x.is_finite()).ok_or_else(|| bad("ovimap.georef", "`centralMeridian` (number) is required"))?,
    };
    let mut g = GeoRef::centred(lat, lon);
    g.central_meridian = cm;
    if !g.is_valid() {
        return Err(bad("ovimap.georef", "地理参照无效：中央子午线须等于原点经度（originLon），经度 ±180、纬度 ±90"));
    }
    let mut warnings = Vec::new();
    if let Some(old) = s.doc()?.georef
        && old != g
    {
        warnings.push("地理参照已覆盖：已有 OVIMAP 实体的坐标含义随之改变，请确认".to_string());
    }
    s.doc_mut()?.georef = Some(g);
    Ok(json!({ "georef": g.to_json(), "warnings": warnings }))
}

/// ovimap.georef typed (audit F-class): the Tools menu item asks for the origin
/// latitude and longitude as prompts (central meridian = origin longitude). The
/// basemap dialog keeps calling the JSON form with its own picked centre.
#[derive(Default)]
pub struct GeorefM {
    lat: Option<f64>,
}

impl Interactive for GeorefM {
    fn name(&self) -> &'static str {
        "OVIMAP.GEOREF"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        let (msg, cur) = match self.lat {
            None => ("Enter origin latitude", s.doc().ok().and_then(|d| d.georef).map(|g| g.origin_lat)),
            Some(_) => ("Enter origin longitude", s.doc().ok().and_then(|d| d.georef).map(|g| g.origin_lon)),
        };
        let p = Prompt::new(msg, Accept::TEXT);
        match cur {
            Some(v) => p.default(format!("{v:.6}")),
            None => p,
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let text = match i {
            Input::Cancel => return Ok(Step::Done),
            Input::Enter => None,
            Input::Text(t) => Some(t.trim().to_string()),
            _ => return Ok(Step::Continue),
        };
        let cur = s.doc().ok().and_then(|d| d.georef);
        let value = match text.as_deref() {
            None | Some("") => match self.lat {
                None => cur.map(|g| g.origin_lat),
                Some(_) => cur.map(|g| g.origin_lon),
            },
            Some(t) => super::machines::number(t),
        };
        let Some(v) = value else {
            s.echo("*Invalid number*");
            return Ok(Step::Continue);
        };
        match self.lat {
            None => {
                if !(-90.0..=90.0).contains(&v) {
                    s.echo("Latitude must be between -90 and 90.");
                    return Ok(Step::Continue);
                }
                self.lat = Some(v);
                Ok(Step::Continue)
            }
            Some(lat) => {
                if !(-180.0..=180.0).contains(&v) {
                    s.echo("Longitude must be between -180 and 180.");
                    return Ok(Step::Continue);
                }
                let r = run_georef(s, &json!({ "originLat": lat, "originLon": v }))?;
                if let Some(ws) = r.get("warnings").and_then(Value::as_array) {
                    for w in ws {
                        if let Some(w) = w.as_str() {
                            s.echo(w);
                        }
                    }
                }
                s.echo(format!("Georeference set: origin {lat:.6}, {v:.6}."));
                Ok(Step::Done)
            }
        }
    }
}

/// ovimap.push / ovimap.pull typed (audit F-class): both need a project id; the
/// bridge settings (base URL, token) come from the CADCRAFT_OVIMAP_* environment,
/// exactly as the JSON form documents.
pub struct ProjectIdM {
    push: bool,
}

impl Interactive for ProjectIdM {
    fn name(&self) -> &'static str {
        if self.push { "OVIMAP.PUSH" } else { "OVIMAP.PULL" }
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter OviMap project id", Accept::TEXT)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Cancel => Ok(Step::Done),
            Input::Text(t) => {
                let id = t.trim().to_string();
                if id.is_empty() {
                    return Ok(Step::Continue);
                }
                if self.push {
                    match run_push(s, &json!({ "projectId": id })) {
                        Ok(r) => {
                            let outcome = r.get("outcome").and_then(Value::as_str).unwrap_or("");
                            let rev = r.get("rev").and_then(Value::as_u64);
                            match (outcome, rev) {
                                ("conflict", _) => s.echo("Push conflict: the server copy is newer; pull first. Nothing was overwritten."),
                                (_, Some(rev)) => s.echo(format!("Pushed to OviMap (rev {rev}).")),
                                _ => s.echo("Pushed to OviMap."),
                            }
                        }
                        Err(e) => s.echo(e.to_string()),
                    }
                } else {
                    match run_pull(s, &json!({ "projectId": id })) {
                        Ok(r) => {
                            let rev = r.get("rev").and_then(Value::as_u64).unwrap_or(0);
                            s.echo(format!("Pulled rev {rev} from OviMap. Apply it with ovimap.import on the returned data (JSON form)."));
                        }
                        Err(e) => s.echo(e.to_string()),
                    }
                }
                Ok(Step::Done)
            }
            _ => Ok(Step::Continue),
        }
    }
}

/// The bridge client for a call: explicit params win, then the
/// `CADCRAFT_OVIMAP_*` environment. The token is never stored or logged.
fn bridge_client(p: &Value, cmd: &str) -> Result<crate::bridge::BridgeClient> {
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let env_base = env("CADCRAFT_OVIMAP_BASE_URL");
    let env_token = env("CADCRAFT_OVIMAP_TOKEN");
    let env_device = env("CADCRAFT_OVIMAP_DEVICE_ID");
    let env_name = env("CADCRAFT_OVIMAP_DEVICE_NAME");
    let base_url = str_param(p, "baseUrl").or_else(|| str_param(p, "base_url")).or(env_base.as_deref()).unwrap_or_default();
    let token = str_param(p, "token").or(env_token.as_deref()).unwrap_or_default();
    let device_id = str_param(p, "deviceId").or_else(|| str_param(p, "device_id")).or(env_device.as_deref()).unwrap_or("cadcraft-desktop");
    let device_name = str_param(p, "deviceName").or_else(|| str_param(p, "device_name")).or(env_name.as_deref()).unwrap_or("CadCraft");
    crate::bridge::BridgeClient::new(base_url, token, device_id, device_name).map_err(|e| bad(cmd, e))
}

fn bridge_project_id(p: &Value, cmd: &str) -> Result<String> {
    str_param(p, "projectId")
        .or_else(|| str_param(p, "project_id"))
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| bad(cmd, "`projectId` 为空"))
}

fn run_push(s: &mut Session, p: &Value) -> Result<Value> {
    let project = bridge_project_id(p, "ovimap.push")?;
    let geojson = match str_param(p, "geojson") {
        Some(g) if !g.trim().is_empty() => g.to_string(),
        _ => {
            let exported = run_export(s, &json!({ "format": "geojson" }))?;
            exported.get("geojson").and_then(Value::as_str).unwrap_or("").to_string()
        }
    };
    if geojson.trim().is_empty() {
        return Err(bad("ovimap.push", "GeoJSON 为空：图纸中没有可推送的联动要素"));
    }
    let dxf: Option<Vec<u8>> = match p.get("dxfBase64").and_then(Value::as_str) {
        Some(b64) => Some(base64_decode(b64).ok_or_else(|| bad("ovimap.push", "`dxfBase64` 不是合法 base64"))?),
        None if p.get("dxf").and_then(Value::as_bool) == Some(true) => {
            let exported = run_export(s, &json!({ "format": "dxf" }))?;
            exported.get("data").and_then(|v| v.as_str()).and_then(base64_decode)
        }
        _ => None,
    };
    let base_rev = p.get("baseRev").and_then(Value::as_u64).unwrap_or(0);
    let client = bridge_client(p, "ovimap.push")?;
    let envelope = client.envelope(base_rev, geojson, dxf);
    match client.push(&project, &envelope).map_err(|e| bad("ovimap.push", e))? {
        crate::bridge::PushOutcome::Updated { rev } => Ok(json!({ "projectId": project, "outcome": "updated", "rev": rev })),
        crate::bridge::PushOutcome::Conflict { server_rev, server_geojson, server_dxf } => Ok(json!({
            "projectId": project,
            "outcome": "conflict",
            "serverRev": server_rev,
            "serverGeojson": server_geojson,
            "serverDxfBase64": server_dxf.map(|b| base64_encode(&b)),
            "message": "服务端版本已更新（baseRev 过期）：本地改动未覆盖，请用 ovimap.pull 取回服务端副本核对后再推送",
        })),
    }
}

fn run_pull(_s: &mut Session, p: &Value) -> Result<Value> {
    let project = bridge_project_id(p, "ovimap.pull")?;
    let client = bridge_client(p, "ovimap.pull")?;
    let pulled = client.pull(&project).map_err(|e| bad("ovimap.pull", e))?;
    Ok(json!({
        "projectId": project,
        "rev": pulled.rev,
        "updatedAt": pulled.updated_at,
        "geojson": pulled.geojson,
        "dxfBase64": pulled.dxf.as_ref().map(|b| base64_encode(b)),
        "hasDxf": pulled.dxf.is_some(),
        "message": "已取回服务端副本（未自动导入）：请用 ovimap.import 导入",
    }))
}
