//! OviMap link DXF roundtrip at io level (design doc `docs/ovimap-link.md` §4, §10):
//! T3 (R1) GEOREF/VERTICES XRECORDs survive a DXF save, T4 stripped-VERTICES
//! degradation, R2 pre-flight validation, and §9.4 tile cache behaviour.
//! No network is used anywhere in this file.

use cadcraft_doc::geom::Vec3;
use cadcraft_doc::gis;
use cadcraft_doc::{Drawing, EntityKind, HVal, LwPolyline, Point, Space, Text};
use cadcraft_io::geojson::{tile_cache_load, tile_cache_path, tile_cache_store, validate_ovimap_dxf};

fn xdata(id: &str, name: &str, kind: &str, ty: &str) -> Vec<cadcraft_doc::RawTag> {
    gis::ovimap_xdata(id, name, kind, ty)
}

fn point_entity(id: &str, name: &str, kind: &str, ty: &str, x: f64, y: f64) -> cadcraft_doc::Entity {
    let mut e = cadcraft_doc::Entity::new(cadcraft_doc::Handle(0), EntityKind::Point(Point { p: Vec3::new(x, y, 0.0), angle: 0.0 }));
    e.common.xdata = xdata(id, name, kind, ty);
    e
}

fn text_entity(id: &str, name: &str, kind: &str, ty: &str, x: f64, y: f64, value: &str) -> cadcraft_doc::Entity {
    let mut e = cadcraft_doc::Entity::new(
        cadcraft_doc::Handle(0),
        EntityKind::Text(Text {
            insert: Vec3::new(x, y, 0.0),
            align_pt: None,
            height: 2.5,
            value: value.to_string(),
            rotation: 0.0,
            width_factor: 1.0,
            oblique: 0.0,
            style: "Standard".into(),
            halign: cadcraft_doc::HAlign::Left,
            valign: cadcraft_doc::VAlign::Baseline,
        }),
    );
    e.common.xdata = xdata(id, name, kind, ty);
    e
}

fn track_entity(id: &str, name: &str, pts: &[(f64, f64)], closed: bool) -> cadcraft_doc::Entity {
    let mut e = cadcraft_doc::Entity::new(
        cadcraft_doc::Handle(0),
        EntityKind::LwPolyline(LwPolyline {
            vertices: pts
                .iter()
                .map(|&(x, y)| cadcraft_doc::geom::PolyVertex {
                    p: cadcraft_doc::geom::Vec2::new(x, y),
                    bulge: 0.0,
                    start_width: 0.0,
                    end_width: 0.0,
                })
                .collect(),
            closed,
            const_width: 0.0,
            elevation: 0.0,
            plinegen: false,
        }),
    );
    e.common.xdata = xdata(id, name, "track", "track");
    e
}

fn link_drawing() -> Drawing {
    let mut d = Drawing::new_metric();
    d.header.set_i64("INSUNITS", 6);
    d.georef = Some(gis::GeoRef::centred(32.1301, 114.0814));
    // Marker: POINT + TEXT, same xdata.
    let p = point_entity("m-01", "杆-01", "marker", "concrete", 10.0, 20.0);
    d.add(&Space::Model, p.common, p.kind).unwrap();
    let t = text_entity("m-01", "杆-01", "marker", "concrete", 10.0, 23.0, "杆-01");
    d.add(&Space::Model, t.common, t.kind).unwrap();
    // Track with 3 vertices + VERTICES ext.
    let tr = track_entity("t-1", "杆路段1", &[(0.0, 0.0), (120.5, 0.0), (240.0, 10.0)], false);
    d.add(&Space::Model, tr.common, tr.kind).unwrap();
    let ext = gis::OvimapExt {
        vertices: vec![
            serde_json::json!({"ovimap_id": "v11", "seq": 1}),
            serde_json::json!({"ovimap_id": "v12", "seq": 2, "segCable": "48芯GYTS"}),
            serde_json::json!({"ovimap_id": "v13", "seq": 3}),
        ],
        ..Default::default()
    };
    d.ovimap_ext.insert("t-1".into(), ext);
    d
}

/// T3 / R1: GEOREF + VERTICES XRECORDs survive a plain DXF save; xdata, georef
/// and vertex ids read back intact; $INSUNITS is 6.
#[test]
fn t3_georef_and_vertices_survive_save() {
    let d = link_drawing();
    let text = cadcraft_io::write_dxf(&d);
    assert!(text.contains("OVIMAP_GEOREF"), "GEOREF dictionary missing");
    assert!(text.contains("VERTICES_t-1"), "VERTICES XRECORD missing");
    assert!(text.contains("OVIMAP_SPHERICAL_TM"), "projection name missing");
    assert!(text.contains("1001"), "xdata APPID marker missing");

    let back = cadcraft_io::read_dxf(text.as_bytes()).unwrap();
    assert_eq!(back.header.get("INSUNITS"), Some(&HVal::Int(6)));
    assert_eq!(back.georef, d.georef);
    // Marker xdata survives.
    let marker = back
        .model
        .iter()
        .find(|e| matches!(e.kind, EntityKind::Point(_)) && gis::parse_ovimap_xdata(&e.common.xdata).is_some_and(|xd| xd.id == "m-01"))
        .expect("marker POINT missing after save");
    assert_eq!(gis::parse_ovimap_xdata(&marker.common.xdata).unwrap().kind, "marker");
    // Track vertex ids survive via the VERTICES XRECORD.
    let verts = back.ovimap_ext.get("t-1").expect("track ext missing after save").vertices.clone();
    let ids: Vec<&str> = verts.iter().filter_map(|v| v.get("ovimap_id").and_then(|x| x.as_str())).collect();
    assert_eq!(ids, vec!["v11", "v12", "v13"]);
    // Track geometry survives.
    let track = back.model.iter().find(|e| matches!(e.kind, EntityKind::LwPolyline(_))).expect("track missing after save");
    if let EntityKind::LwPolyline(pl) = &track.kind {
        assert_eq!(pl.vertices.len(), 3);
        assert!((pl.vertices[1].p.x - 120.5).abs() < 1e-9);
    }
}

/// T4: a drawing with no VERTICES ext writes no VERTICES entries and reads back
/// with empty track vertices (degraded, never a crash).
#[test]
fn t4_stripped_vertices_read_empty() {
    let mut d = link_drawing();
    d.ovimap_ext.get_mut("t-1").unwrap().vertices.clear();
    let text = cadcraft_io::write_dxf(&d);
    assert!(!text.contains("VERTICES_"), "stripped drawing must not write VERTICES entries");
    let back = cadcraft_io::read_dxf(text.as_bytes()).unwrap();
    assert_eq!(back.georef, d.georef, "georef must survive even when VERTICES are stripped");
    let track = back
        .model
        .iter()
        .find(|e| gis::parse_ovimap_xdata(&e.common.xdata).is_some_and(|xd| xd.id == "t-1"))
        .expect("track missing after stripped save");
    assert!(matches!(track.kind, EntityKind::LwPolyline(_)));
    assert!(back.ovimap_ext.get("t-1").map(|e| e.vertices.is_empty()).unwrap_or(true));
}

/// R2: pre-flight validation refuses illegal VERTICES keys and vertices without
/// ids, and accepts a clean drawing.
#[test]
fn r2_validate_ovimap_dxf() {
    let d = link_drawing();
    assert!(validate_ovimap_dxf(&d).is_ok());
    // Illegal dict key: track id with '/'.
    let mut bad = link_drawing();
    bad.ovimap_ext.remove("t-1");
    let ext = gis::OvimapExt { vertices: vec![serde_json::json!({"ovimap_id": "v1"})], ..Default::default() };
    bad.ovimap_ext.insert("t/bad".into(), ext);
    assert!(validate_ovimap_dxf(&bad).is_err());
    // Vertex without an id.
    let mut bad2 = link_drawing();
    bad2.ovimap_ext.get_mut("t-1").unwrap().vertices[0] = serde_json::json!({"seq": 1});
    assert!(validate_ovimap_dxf(&bad2).is_err());
}

/// §9.4: tile cache stores and loads, refuses oversized tiles, and treats stale
/// entries as misses. The cache root comes from `CADCRAFT_TILE_CACHE` (test
/// support in `geojson.rs`); without it the test skips rather than touching the
/// real `~/.cache` (same convention as the `CADCRAFT_LISP_*` corpus tests).
/// No network is involved.
#[test]
fn tile_cache_roundtrip_and_limits() {
    let Ok(dir) = std::env::var("CADCRAFT_TILE_CACHE") else {
        eprintln!("skipped: set CADCRAFT_TILE_CACHE to a scratch dir to run the tile cache test");
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let _ = std::fs::remove_dir_all(&dir);
    let layer = gis::TileLayer::default_osm();
    let tile = gis::TileRef { z: 15, x: 12345, y: 6789, url: layer.tile_url(15, 12345, 6789), cache_key: layer.cache_key(15, 12345, 6789) };
    // Miss before store.
    assert_eq!(tile_cache_load(&layer, &tile), None);
    // Store + load roundtrip.
    tile_cache_store(&layer, &tile, b"fake-png-bytes").unwrap();
    assert_eq!(tile_cache_load(&layer, &tile).as_deref(), Some(b"fake-png-bytes".as_slice()));
    // Oversized tile (>1 MB) is refused and does not clobber the good entry.
    let big = vec![0u8; (1 << 20) + 1];
    assert!(tile_cache_store(&layer, &tile, &big).is_err());
    assert_eq!(tile_cache_load(&layer, &tile).as_deref(), Some(b"fake-png-bytes".as_slice()));
    // Stale entry (mtime forced 8 days back) counts as a miss.
    let path = tile_cache_path(&layer, &tile).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 24 * 3600);
    std::fs::File::open(&path).unwrap().set_times(std::fs::FileTimes::new().set_modified(old)).unwrap();
    assert_eq!(tile_cache_load(&layer, &tile), None);
    let _ = std::fs::remove_dir_all(&dir);
}
