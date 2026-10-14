//! OviMap GIS link end-to-end tests (design doc `docs/ovimap-link.md` §10):
//! T2 GeoJSON roundtrip, T3 DXF roundtrip (XRECORD survival, R1),
//! T4 stripped-VERTICES degradation, T5 delete semantics, T6 conflict rule,
//! T7 control-channel import counts, plus assign/georef/tile defaults.

use cadcraft_engine::Session;
use cadcraft_engine::cmd::file::{IoHooks, set_io};
use cadcraft_engine::doc::geom::Vec3;
use cadcraft_engine::doc::gis;
use cadcraft_engine::doc::{Common, Drawing, EntityKind, Handle, LwPolyline, Point, Space};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn install_real_hooks() {
    set_io(IoHooks {
        read_progress: None,
        read: |b, n| cadcraft_io::read(b, n).map_err(|e| e.to_string()),
        write: |d, n| cadcraft_io::write(d, n).map_err(|e| e.to_string()),
        plot: None,
    });
}

fn tmpdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("cadcraft-gis-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn feat(id: &str, name: &str, kind: &str, ty: &str, geom: Value, extra: Value) -> Value {
    let mut p = json!({"ovimap_id": id, "name": name, "kind": kind, "type": ty});
    if let Value::Object(m) = extra {
        p.as_object_mut().unwrap().extend(m);
    }
    json!({"type": "Feature", "properties": p, "geometry": geom})
}

fn point_geom(lon: f64, lat: f64) -> Value {
    json!({"type": "Point", "coordinates": [lon, lat]})
}

fn line_geom(coords: &[(f64, f64)]) -> Value {
    json!({"type": "LineString", "coordinates": coords})
}

/// §10.1 sample project: 12 markers, 3 tracks, 4 labels, 1 photo pin.
fn sample() -> Value {
    let mut features = Vec::new();
    // 12 markers: 4 concrete, 2 electric, 2 manhole, 2 fiberbox, 1 splitterbox, 1 crossbox.
    let specs: [(&str, &str, &str); 12] = [
        ("m-01", "杆-01", "concrete"),
        ("m-02", "杆-02", "electric"),
        ("m-03", "杆-03", "concrete"),
        ("m-04", "杆-04", "concrete"),
        ("m-05", "杆-05", "electric"),
        ("m-06", "杆-06", "electric"),
        ("m-07", "井-07", "manhole"),
        ("m-08", "井-08", "manhole"),
        ("m-09", "纤-09", "fiberbox"),
        ("m-10", "纤-10", "fiberbox"),
        ("m-11", "分光-11", "splitterbox"),
        ("m-12", "交接-12", "crossbox"),
    ];
    for (i, (id, name, ty)) in specs.iter().enumerate() {
        let lon = 114.0814 + 0.001 * i as f64;
        let lat = 32.1301 + 0.001 * (i % 4) as f64;
        let mut extra = json!({});
        match *id {
            "m-01" => extra = json!({"photos": ["IMG_001.jpg"]}),
            "m-02" => extra = json!({"photos": ["IMG_002.jpg"]}),
            "m-03" => extra = json!({"note": "井盖破损"}),
            "m-07" => extra = json!({"holes": 12, "usedHoles": 5}),
            "m-08" => extra = json!({"segCable": "48芯GYTS", "slackM": 8.5}),
            _ => {}
        }
        features.push(feat(id, name, "marker", ty, point_geom(lon, lat), extra));
    }
    // Track t-1: 5-vertex pole-line chain with per-vertex business fields.
    let t1_coords = [(114.0814, 32.1301), (114.0824, 32.1301), (114.0834, 32.1302), (114.0844, 32.1302), (114.0854, 32.1303)];
    let t1_vertices = json!([
        {"ovimap_id": "v11", "seq": 1, "distanceM": 0.0},
        {"ovimap_id": "v12", "seq": 2, "distanceM": 120.5, "segKind": 1, "segCable": "48芯GYTS", "photos": ["IMG_001.jpg"]},
        {"ovimap_id": "v13", "seq": 3, "distanceM": 240.0, "segKind": 2},
        {"ovimap_id": "v14", "seq": 4, "distanceM": 360.25},
        {"ovimap_id": "v15", "seq": 5, "distanceM": 480.75},
    ]);
    features.push(feat(
        "t-1",
        "杆路段1",
        "track",
        "track",
        line_geom(&t1_coords),
        json!({"line_group": "t-1", "distanceM": 480.75, "vertices": t1_vertices, "timestamps": null}),
    ));
    // Track t-2: plain 3-vertex chain.
    let t2_coords = [(114.0860, 32.1310), (114.0870, 32.1312), (114.0880, 32.1311)];
    features.push(feat(
        "t-2",
        "杆路段2",
        "track",
        "track",
        line_geom(&t2_coords),
        json!({"line_group": "t-2", "vertices": [
            {"ovimap_id": "w21", "seq": 1},
            {"ovimap_id": "w22", "seq": 2},
            {"ovimap_id": "w23", "seq": 3},
        ], "timestamps": null}),
    ));
    // Track t-3: closed area ring, coords repeat the first point, repeated vertex id.
    let t3_coords = [(114.0900, 32.1400), (114.0910, 32.1400), (114.0910, 32.1410), (114.0900, 32.1400)];
    features.push(feat(
        "t-3",
        "闭合区域",
        "track",
        "track",
        line_geom(&t3_coords),
        json!({"line_group": "t-3", "closed": true, "vertices": [
            {"ovimap_id": "c1", "seq": 1},
            {"ovimap_id": "c2", "seq": 2},
            {"ovimap_id": "c3", "seq": 3},
            {"ovimap_id": "c1", "seq": 4},
        ], "timestamps": null}),
    ));
    // 4 labels, one with a newline (→ MTEXT).
    for (i, name) in ["标签一", "标签二", "标签三", "多行\n标签四"].iter().enumerate() {
        let lon = 114.0950 + 0.001 * i as f64;
        features.push(feat(&format!("l-{}", i + 1), name, "label", "text", point_geom(lon, 32.1450), json!({})));
    }
    // 1 photo pin.
    features.push(feat("p-1", "现场照片", "photo", "none", point_geom(114.0990, 32.1490), json!({"photos": ["IMG_009.jpg"]})));
    json!({
        "type": "FeatureCollection",
        "ovimap_georef": {
            "projection": "OVIMAP_SPHERICAL_TM",
            "origin_lat": 32.1301, "origin_lon": 114.0814, "central_meridian": 114.0814,
            "scale_factor": 1.0, "false_easting": 0.0, "false_northing": 0.0,
            "radius": 6378137.0, "units": "metre", "source_crs": "WGS84", "schema_version": "1.0",
        },
        "ovimap_meta": {
            "schema_version": "1.0", "exporter": "ovimap",
            "exported_at": "2026-10-14T10:00:00.000Z",
            "source_collection_id": "col-1", "source_collection_name": "测试工程",
            "delete_mode": "none",
        },
        "ovimap_deletes": [],
        "features": features,
    })
}

fn import(s: &mut Session, doc: &Value) -> Value {
    s.execute("ovimap.import", &json!({"text": doc.to_string()})).unwrap()
}

fn coords_by_id(geo: &Value) -> BTreeMap<String, Vec<[f64; 2]>> {
    let mut out = BTreeMap::new();
    if let Some(fs) = geo.get("features").and_then(Value::as_array) {
        for f in fs {
            let id = f["properties"]["ovimap_id"].as_str().unwrap_or("").to_string();
            let coords = f["geometry"]["coordinates"]
                .as_array()
                .map(|cs| cs.iter().map(|c| [c[0].as_f64().unwrap_or(0.0), c[1].as_f64().unwrap_or(0.0)]).collect())
                .unwrap_or_default();
            out.insert(id, coords);
        }
    }
    out
}

fn max_coord_diff(a: &Value, b: &Value) -> f64 {
    let (ma, mb) = (coords_by_id(a), coords_by_id(b));
    let mut max = 0.0f64;
    for (id, ca) in &ma {
        let cb = mb.get(id).unwrap_or_else(|| panic!("missing feature {id} after roundtrip"));
        assert_eq!(ca.len(), cb.len(), "coord count changed for {id}");
        for (p, q) in ca.iter().zip(cb.iter()) {
            max = max.max((p[0] - q[0]).abs()).max((p[1] - q[1]).abs());
        }
    }
    max
}

fn track_vertices(geo: &Value, id: &str) -> Vec<String> {
    geo["features"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["properties"]["ovimap_id"] == id)
        .and_then(|f| f["properties"]["vertices"].as_array())
        .map(|vs| vs.iter().filter_map(|v| v["ovimap_id"].as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn feature_by_id<'a>(geo: &'a Value, id: &str) -> &'a Value {
    geo["features"].as_array().unwrap().iter().find(|f| f["properties"]["ovimap_id"] == id).unwrap_or_else(|| panic!("missing feature {id}"))
}

/// T7: sample import counts + entity counts + inspect agreement.
#[test]
fn t7_import_sample_counts() {
    install_real_hooks();
    let mut s = Session::new();
    let before = s.doc().unwrap().model.len();
    let rep = import(&mut s, &sample());
    assert_eq!(rep["added"], 20, "report: {rep}");
    assert_eq!(rep["skipped"], 0);
    assert_eq!(rep["conflicts"], json!([]));
    // Entities: 12 markers×2 + 2 marker PHOTO texts + 3 polylines + 1 vertex PHOTO text
    // + 4 labels + photo pin (POINT + TEXT) = 36.
    assert_eq!(s.doc().unwrap().model.len() - before, 36);
    let insp = s.execute("ovimap.inspect", &json!({})).unwrap();
    assert_eq!(insp["counts"]["marker"], 12);
    assert_eq!(insp["counts"]["track"], 3);
    assert_eq!(insp["counts"]["label"], 4);
    assert_eq!(insp["counts"]["photo"], 1);
    assert_eq!(insp["counts"]["entities"], 36);
    assert_eq!(insp["georef"]["origin_lat"], 32.1301);
}

/// §6.5 idempotency: re-importing the same file changes nothing.
#[test]
fn import_is_idempotent() {
    install_real_hooks();
    let mut s = Session::new();
    let doc = sample();
    let rep1 = import(&mut s, &doc);
    assert_eq!(rep1["added"], 20);
    let rep2 = import(&mut s, &doc);
    assert_eq!(rep2["added"], 0);
    assert_eq!(rep2["updated"], 0);
    assert_eq!(rep2["skipped"], 20, "report: {rep2}");
}

/// §6.2: a renamed marker counts as an update; everything else is skipped.
#[test]
fn import_update_changes_name() {
    install_real_hooks();
    let mut s = Session::new();
    import(&mut s, &sample());
    let mut v2 = sample();
    v2["features"][0]["properties"]["name"] = json!("杆-01（改名）");
    let rep = import(&mut s, &v2);
    assert_eq!(rep["updated"], 1, "report: {rep}");
    assert_eq!(rep["skipped"], 19);
    // The marker TEXT carries the new name.
    let has_new = s.doc().unwrap().model.iter().any(|e| matches!(&e.kind, EntityKind::Text(t) if t.value == "杆-01（改名）"));
    assert!(has_new, "marker TEXT was not updated");
}

/// T5: deletes are ignored with delete_mode=none, applied with delete_mode=sync.
#[test]
fn t5_delete_semantics() {
    install_real_hooks();
    let mut s = Session::new();
    import(&mut s, &sample());
    let mut v_none = sample();
    v_none["ovimap_meta"]["delete_mode"] = json!("none");
    v_none["ovimap_deletes"] = json!(["m-01"]);
    let rep = import(&mut s, &v_none);
    let warnings = rep["warnings"].as_array().unwrap();
    assert!(warnings.iter().any(|w| w.as_str().unwrap_or("").contains("已忽略")), "report: {rep}");
    let still_there = s.doc().unwrap().model.iter().any(|e| gis::parse_ovimap_xdata(&e.common.xdata).is_some_and(|xd| xd.id == "m-01"));
    assert!(still_there, "delete_mode=none must not delete");

    let mut v_sync = sample();
    v_sync["ovimap_meta"]["delete_mode"] = json!("sync");
    v_sync["ovimap_deletes"] = json!(["m-01"]);
    let rep2 = import(&mut s, &v_sync);
    let warnings2 = rep2["warnings"].as_array().unwrap();
    assert!(warnings2.iter().any(|w| w.as_str().unwrap_or("").contains("已删除")), "report: {rep2}");
    let gone = !s.doc().unwrap().model.iter().any(|e| gis::parse_ovimap_xdata(&e.common.xdata).is_some_and(|xd| xd.id == "m-01"));
    assert!(gone, "delete_mode=sync must delete m-01");
}

/// T6: the "ovimap wins" conflict rule lives on the ovimap side (§6.4).
/// CadCraft import applies the file and reports no conflicts.
#[test]
fn t6_conflict_rule_is_ovimap_side() {
    install_real_hooks();
    let mut s = Session::new();
    import(&mut s, &sample());
    let mut v2 = sample();
    v2["ovimap_meta"]["exported_at"] = json!("2026-10-13T10:00:00.000Z"); // older stamp
    v2["features"][0]["properties"]["name"] = json!("杆-01（旧文件改名）");
    let rep = import(&mut s, &v2);
    assert_eq!(rep["conflicts"], json!([]));
    assert_eq!(rep["updated"], 1, "CadCraft applies the file; ovimap adjudicates conflicts: {rep}");
}

/// T2: GeoJSON roundtrip keeps ids, coords (≤1e-9°), business fields; timestamps null.
#[test]
fn t2_geojson_roundtrip() {
    install_real_hooks();
    let mut s = Session::new();
    import(&mut s, &sample());
    let out = s.execute("ovimap.export", &json!({})).unwrap();
    assert_eq!(out["features"], 20);
    let back: Value = serde_json::from_str(out["geojson"].as_str().unwrap()).unwrap();
    let orig = sample();
    assert_eq!(back["features"].as_array().unwrap().len(), 20);
    assert!(max_coord_diff(&orig, &back) <= 1e-9, "GeoJSON roundtrip drift");
    assert_eq!(back["ovimap_meta"]["exporter"], "cadcraft");
    // Business fields survive (marker m-08, track t-1 vertex v12).
    assert_eq!(feature_by_id(&back, "m-08")["properties"]["segCable"], "48芯GYTS");
    assert_eq!(feature_by_id(&back, "m-08")["properties"]["slackM"], 8.5);
    assert_eq!(feature_by_id(&back, "m-07")["properties"]["holes"], 12);
    let t1 = feature_by_id(&back, "t-1");
    assert_eq!(t1["properties"]["timestamps"], Value::Null);
    let v12 = &t1["properties"]["vertices"][1];
    assert_eq!(v12["segCable"], "48芯GYTS");
    assert_eq!(v12["distanceM"], 120.5);
    assert_eq!(track_vertices(&back, "t-1"), vec!["v11", "v12", "v13", "v14", "v15"]);
    // Closed ring: closed flag + repeated first point + repeated vertex id.
    let t3 = feature_by_id(&back, "t-3");
    assert_eq!(t3["properties"]["closed"], true);
    assert_eq!(track_vertices(&back, "t-3"), vec!["c1", "c2", "c3", "c1"]);
    // Multi-line label keeps its newline.
    assert_eq!(feature_by_id(&back, "l-4")["properties"]["name"], "多行\n标签四");
}

/// T3 / R1: CadCraft DXF save keeps OVIMAP XRECORDs; re-import restores ids,
/// geometry (≤1 mm ≈ 9e-9°) and track vertex ids.
#[test]
fn t3_dxf_roundtrip_keeps_xrecords() {
    install_real_hooks();
    let dir = tmpdir("t3");
    let mut s = Session::new();
    let rep = import(&mut s, &sample());
    assert_eq!(rep["added"], 20);
    let dxf_path = dir.join("sample.ovimap.dxf");
    let out = s.execute("ovimap.export", &json!({"path": dxf_path.to_string_lossy(), "format": "dxf"})).unwrap();
    assert_eq!(out["format"], "dxf");
    let text = std::fs::read_to_string(&dxf_path).unwrap();
    assert!(text.contains("OVIMAP_GEOREF"), "GEOREF dictionary missing from saved DXF");
    assert!(text.contains("VERTICES_t-1"), "VERTICES XRECORD missing from saved DXF");
    assert!(text.contains("VERTICES_t-3"), "VERTICES XRECORD for closed track missing");
    assert!(text.contains("INSUNITS"), "$INSUNITS missing");

    let mut s2 = Session::new();
    let rep2 = s2.execute("ovimap.import", &json!({"path": dxf_path.to_string_lossy()})).unwrap();
    assert_eq!(rep2["added"], 20, "report: {rep2}");
    let insp = s2.execute("ovimap.inspect", &json!({})).unwrap();
    assert_eq!(insp["georef"]["origin_lat"], 32.1301);
    assert_eq!(insp["georef"]["origin_lon"], 114.0814);

    let geo = s2.execute("ovimap.export", &json!({})).unwrap();
    let back: Value = serde_json::from_str(geo["geojson"].as_str().unwrap()).unwrap();
    let orig = sample();
    let drift = max_coord_diff(&orig, &back);
    assert!(drift <= 1e-8, "DXF roundtrip drift {drift}° exceeds ~1 mm");
    assert_eq!(track_vertices(&back, "t-1"), vec!["v11", "v12", "v13", "v14", "v15"]);
    assert_eq!(track_vertices(&back, "t-3"), vec!["c1", "c2", "c3", "c1"]);
    // Business fields are NOT in DXF (§4.6): the re-exported GeoJSON must not
    // invent them back from the DXF; they are absent (ovimap keeps its originals).
    assert!(feature_by_id(&back, "m-08")["properties"].get("segCable").is_none());
}

/// T4: a DXF whose VERTICES XRECORDs were stripped degrades gracefully:
/// import warns + names the track, export regenerates fresh vertex ids.
#[test]
fn t4_stripped_vertices_degrade() {
    install_real_hooks();
    let dir = tmpdir("t4");
    let mut s = Session::new();
    import(&mut s, &sample());
    // Simulate a third-party resave that stripped VERTICES entries: clear them.
    s.doc_mut().unwrap().ovimap_ext.get_mut("t-1").unwrap().vertices.clear();
    s.doc_mut().unwrap().ovimap_ext.get_mut("t-3").unwrap().vertices.clear();
    let dxf_path = dir.join("stripped.ovimap.dxf");
    s.execute("ovimap.export", &json!({"path": dxf_path.to_string_lossy(), "format": "dxf"})).unwrap();
    let text = std::fs::read_to_string(&dxf_path).unwrap();
    assert!(!text.contains("VERTICES_t-1"), "stripped track t-1 must have no VERTICES entry");
    assert!(!text.contains("VERTICES_t-3"), "stripped track t-3 must have no VERTICES entry");
    assert!(text.contains("VERTICES_t-2"), "untouched track t-2 keeps its VERTICES entry");

    let mut s2 = Session::new();
    let rep = s2.execute("ovimap.import", &json!({"path": dxf_path.to_string_lossy()})).unwrap();
    assert_eq!(rep["added"], 20, "report: {rep}");
    let warnings = rep["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w.as_str().unwrap_or("").contains("t-1") && w.as_str().unwrap_or("").contains("VERTICES")),
        "report must warn about stripped VERTICES for t-1: {rep}"
    );
    // Export regenerates vertex ids (fresh, length == coord count).
    let geo = s2.execute("ovimap.export", &json!({})).unwrap();
    let back: Value = serde_json::from_str(geo["geojson"].as_str().unwrap()).unwrap();
    let verts = track_vertices(&back, "t-1");
    assert_eq!(verts.len(), 5);
    assert!(verts.iter().all(|v| !v.is_empty()));
    assert!(verts.iter().all(|v| !["v11", "v12", "v13", "v14", "v15"].contains(&v.as_str())));
}

/// C5 / §5.4: ovimap.assign stamps a fresh identity; re-assign never overwrites.
#[test]
fn assign_identity() {
    install_real_hooks();
    let mut s = Session::new();
    let h: Handle =
        s.doc_mut().unwrap().add(&Space::Model, Common::default(), EntityKind::Point(Point { p: Vec3::new(1.0, 2.0, 0.0), angle: 0.0 })).unwrap();
    s.set_selection(vec![h]);
    let rep = s.execute("ovimap.assign", &json!({})).unwrap();
    assert_eq!(rep["assigned"].as_array().unwrap().len(), 1);
    let id = rep["assigned"][0]["ovimap_id"].as_str().unwrap().to_string();
    assert!(!id.is_empty());
    let has_xdata = s.doc().unwrap().model.iter().any(|e| e.handle == h && gis::parse_ovimap_xdata(&e.common.xdata).is_some_and(|xd| xd.id == id));
    assert!(has_xdata, "assigned xdata missing on the entity");
    // Second assign: skipped, identity untouched.
    let rep2 = s.execute("ovimap.assign", &json!({})).unwrap();
    assert_eq!(rep2["assigned"].as_array().unwrap().len(), 0);
    assert_eq!(rep2["skipped"], 1);
}

/// ovimap.georef: valid origin accepted; central meridian != origin lon rejected.
/// ovimap.export without any georef refuses (never silently uses (0,0)).
#[test]
fn georef_command_and_export_requires_it() {
    install_real_hooks();
    let mut s = Session::new();
    let err = s.execute("ovimap.export", &json!({}));
    assert!(err.is_err(), "export without georef must refuse");
    let bad = s.execute("ovimap.georef", &json!({"originLat": 32.0, "originLon": 114.0, "centralMeridian": 115.0}));
    assert!(bad.is_err(), "central meridian must equal origin longitude");
    let ok = s.execute("ovimap.georef", &json!({"originLat": 32.1301, "originLon": 114.0814})).unwrap();
    assert_eq!(ok["georef"]["origin_lat"], 32.1301);
    assert_eq!(ok["georef"]["central_meridian"], 114.0814);
}

/// R2: a track id that cannot be a DXF dictionary key imports fine from GeoJSON
/// but DXF export refuses (never silently renames).
#[test]
fn dxf_export_rejects_illegal_dict_key() {
    install_real_hooks();
    let mut s = Session::new();
    let mut doc = sample();
    for f in doc["features"].as_array_mut().unwrap() {
        if f["properties"]["ovimap_id"] == "t-1" {
            f["properties"]["ovimap_id"] = json!("t/bad");
            f["properties"]["line_group"] = json!("t/bad");
        }
    }
    let rep = import(&mut s, &doc);
    assert_eq!(rep["added"], 20);
    let err = s.execute("ovimap.export", &json!({"format": "dxf"}));
    assert!(err.is_err(), "DXF export must refuse the illegal dict key");
    assert!(err.unwrap_err().to_string().contains("字典键"));
}

/// Tiles (§9): inspect reports the OSM default with attribution and z≤19.
#[test]
fn tilemap_inspect_defaults() {
    install_real_hooks();
    let mut s = Session::new();
    let insp = s.execute("ovimap.inspect", &json!({})).unwrap();
    assert_eq!(insp["tilemap"]["url_template"], "https://tile.openstreetmap.org/{z}/{x}/{y}.png");
    assert_eq!(insp["tilemap"]["attribution"], "© OpenStreetMap 贡献者");
    assert_eq!(insp["tilemap"]["max_zoom"], 19);
    assert!(insp["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap_or("").contains("未设置地理参照")));
}

/// Photo check (§5.5): missing photos are listed, not an error.
#[test]
fn missing_photos_reported() {
    install_real_hooks();
    let dir = tmpdir("photos");
    // Create 2 of the 3 photo files; IMG_002.jpg stays missing.
    std::fs::write(dir.join("IMG_001.jpg"), b"fake").unwrap();
    std::fs::write(dir.join("IMG_009.jpg"), b"fake").unwrap();
    let mut s = Session::new();
    import(&mut s, &sample());
    s.state_mut().unwrap().path = Some(dir.join("drawing.dxf").to_string_lossy().into_owned());
    let insp = s.execute("ovimap.inspect", &json!({})).unwrap();
    assert_eq!(insp["missingPhotos"], json!(["IMG_002.jpg"]), "inspect: {insp}");
}

/// A hand-built drawing with georef + track but no VERTICES ext roundtrips
/// through plain DXF write/read (the T4 stripped shape at io level).
#[test]
fn stripped_drawing_keeps_georef() {
    install_real_hooks();
    let mut d = Drawing::new_metric();
    d.georef = Some(gis::GeoRef::centred(32.1301, 114.0814));
    let common = Common { layer: "OVIMAP_TRACK".into(), xdata: gis::ovimap_xdata("t-9", "轨迹", "track", "track"), ..Common::default() };
    let mut pl = LwPolyline { vertices: Vec::new(), closed: false, const_width: 0.0, elevation: 0.0, plinegen: false };
    pl.vertices.push(cadcraft_engine::doc::geom::PolyVertex {
        p: cadcraft_engine::doc::geom::Vec2::new(0.0, 0.0),
        start_width: 0.0,
        end_width: 0.0,
        bulge: 0.0,
    });
    pl.vertices.push(cadcraft_engine::doc::geom::PolyVertex {
        p: cadcraft_engine::doc::geom::Vec2::new(100.0, 0.0),
        start_width: 0.0,
        end_width: 0.0,
        bulge: 0.0,
    });
    pl.vertices.push(cadcraft_engine::doc::geom::PolyVertex {
        p: cadcraft_engine::doc::geom::Vec2::new(100.0, 50.0),
        start_width: 0.0,
        end_width: 0.0,
        bulge: 0.0,
    });
    d.add(&Space::Model, common, EntityKind::LwPolyline(pl)).unwrap();
    // ext present but with NO vertex ids (stripped shape).
    d.ovimap_ext.insert("t-9".into(), gis::OvimapExt::default());
    let bytes = cadcraft_io::write(&d, "x.dxf").unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(!text.contains("VERTICES_"));
    let back = cadcraft_io::read(&bytes, "x.dxf").unwrap();
    assert_eq!(back.georef, d.georef);
    let track = back.model.iter().find(|e| gis::parse_ovimap_xdata(&e.common.xdata).is_some_and(|xd| xd.id == "t-9")).unwrap();
    assert!(matches!(track.kind, EntityKind::LwPolyline(_)));
    assert!(back.ovimap_ext.get("t-9").map(|e| e.vertices.is_empty()).unwrap_or(true));
}

/// TILEMAP (§9): JSON on/off/source/opacity/zoom + interactive prompts.
#[test]
fn tilemap_command_json() {
    install_real_hooks();
    let mut s = Session::new();
    // Status with no params: OSM default, enabled.
    let st = s.execute("tilemap", &json!({})).unwrap();
    assert_eq!(st["tilemap"]["enabled"], true);
    assert_eq!(st["tilemap"]["default_osm"], true);
    // (a): the network status now describes background fetching, not cache-only.
    let net = st["network"].as_str().unwrap_or("");
    assert!(net.contains("后台联网抓取") && net.contains("等待抓取"), "network status describes the background fetcher, got {net}");
    // Off / on round-trip.
    let off = s.execute("tilemap", &json!({ "enabled": false })).unwrap();
    assert_eq!(off["tilemap"]["enabled"], false);
    let on = s.execute("tilemap", &json!({ "action": "on" })).unwrap();
    assert_eq!(on["tilemap"]["enabled"], true);
    // Opacity out of range is refused and the stored layer is untouched.
    assert!(s.execute("tilemap", &json!({ "opacity": 1.5 })).is_err());
    assert!(s.execute("tilemap", &json!({ "opacity": -0.1 })).is_err());
    let ok = s.execute("tilemap", &json!({ "opacity": 0.5 })).unwrap();
    assert_eq!(ok["tilemap"]["opacity"], 0.5);
    // A custom source without attribution in the same call is refused (§9.5).
    assert!(s.execute("tilemap", &json!({ "urlTemplate": "https://example.com/{z}/{x}/{y}.png" })).is_err());
    assert!(s.execute("tilemap", &json!({ "urlTemplate": "https://example.com/{z}/{x}/{y}.png", "attribution": "" })).is_err());
    let custom = s.execute("tilemap", &json!({ "urlTemplate": "https://example.com/{z}/{x}/{y}.png", "attribution": "Example 贡献者" })).unwrap();
    assert_eq!(custom["tilemap"]["url_template"], "https://example.com/{z}/{x}/{y}.png");
    assert_eq!(custom["tilemap"]["attribution"], "Example 贡献者");
    assert_eq!(custom["tilemap"]["default_osm"], false);
    // A custom source may set maxZoom above 19; switching back to OSM restores
    // its attribution and re-applies the z≤19 cap.
    let z = s.execute("tilemap", &json!({ "maxZoom": 22 })).unwrap();
    assert_eq!(z["tilemap"]["max_zoom"], 22);
    assert!(s.execute("tilemap", &json!({ "urlTemplate": "https://tile.openstreetmap.org/{z}/{x}/{y}.png", "maxZoom": 22 })).is_err());
    let back = s.execute("tilemap", &json!({ "urlTemplate": "https://tile.openstreetmap.org/{z}/{x}/{y}.png" })).unwrap();
    assert_eq!(back["tilemap"]["attribution"], "© OpenStreetMap 贡献者");
    assert_eq!(back["tilemap"]["max_zoom"], 19, "switching back to OSM re-applies the z≤19 cap");
    let reset = s.execute("tilemap", &json!({ "action": "reset" })).unwrap();
    assert_eq!(reset["tilemap"]["max_zoom"], 19);
    assert_eq!(reset["tilemap"]["opacity"], 0.85);
    // `zoom` is an alias of `maxZoom`; an unknown action is refused.
    let zoom = s.execute("tilemap", &json!({ "zoom": 12 })).unwrap();
    assert_eq!(zoom["tilemap"]["max_zoom"], 12);
    assert!(s.execute("tilemap", &json!({ "action": "explode" })).is_err());
    // ovimap.inspect sees the same layer (single source of truth).
    let insp = s.execute("ovimap.inspect", &json!({})).unwrap();
    assert_eq!(insp["tilemap"]["max_zoom"], 12);
    assert_eq!(insp["tilemap"]["enabled"], true);
}

#[test]
fn tilemap_command_interactive() {
    install_real_hooks();
    let mut s = Session::new();
    s.cmdline("tilemap off").unwrap();
    assert_eq!(s.doc().unwrap().tilemap.as_ref().map(|t| t.enabled), Some(false));
    // Source flow: URL, then the required attribution.
    s.cmdline("tilemap").unwrap();
    s.cmdline("source").unwrap();
    s.cmdline("https://example.com/{z}/{x}/{y}.png").unwrap();
    s.cmdline("Example 贡献者").unwrap();
    let layer = s.doc().unwrap().tilemap.clone().unwrap_or_default();
    assert_eq!(layer.url_template, "https://example.com/{z}/{x}/{y}.png");
    assert_eq!(layer.attribution, "Example 贡献者");
    // Opacity flow with a bad value re-prompts instead of failing the command.
    s.cmdline("tilemap").unwrap();
    s.cmdline("opacity").unwrap();
    s.cmdline("not-a-number").unwrap();
    assert!(s.running.is_some(), "bad opacity re-prompts");
    s.cmdline("0.4").unwrap();
    assert_eq!(s.doc().unwrap().tilemap.as_ref().map(|t| t.opacity), Some(0.4));
}
