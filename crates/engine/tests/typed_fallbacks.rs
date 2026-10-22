//! Parity audit, batch 2: commands whose JSON form has required parameters must
//! still work when *typed* (AutoCAD drives them with prompts or a dialog). Each test
//! starts the command the way the command line does and drives the prompt flow.

use cadcraft_engine::{Input, Session};
use cadcraft_geom::Vec2;
use serde_json::json;

fn prompt_text(s: &Session) -> String {
    s.current_prompt().map(|p| p.message).unwrap_or_default()
}

fn logged(s: &Session, needle: &str) -> bool {
    s.log.iter().any(|l| l.contains(needle))
}

#[test]
fn ltscale_typed_prompts_for_a_value() {
    let mut s = Session::new();
    s.start("ltscale").unwrap();
    assert!(prompt_text(&s).contains("linetype scale"), "{}", prompt_text(&s));
    s.input(Input::Text("2.5".into())).unwrap();
    assert!(s.running.is_none());
    assert_eq!(s.doc().unwrap().header.f64("LTSCALE", 1.0), 2.5);
}

#[test]
fn color_typed_sets_cecolor() {
    let mut s = Session::new();
    s.start("color").unwrap();
    s.input(Input::Text("red".into())).unwrap();
    assert!(s.running.is_none());
    assert!(logged(&s, "Current color: red"), "{:?}", s.log);
    // ByLayer through the keyword.
    s.start("color").unwrap();
    s.input(Input::Keyword("ByLayer".into())).unwrap();
    assert!(s.running.is_none());
    assert!(logged(&s, "Current color: ByLayer"), "{:?}", s.log);
}

#[test]
fn lweight_typed_accepts_a_number() {
    let mut s = Session::new();
    s.start("lweight").unwrap();
    s.input(Input::Text("0.5".into())).unwrap();
    assert!(s.running.is_none());
    assert!(logged(&s, "Current lineweight: 0.5"), "{:?}", s.log);
}

#[test]
fn limits_typed_two_corners_and_on_off() {
    let mut s = Session::new();
    s.start("limits").unwrap();
    s.input(Input::Point(Vec2::new(0.0, 0.0))).unwrap();
    s.input(Input::Point(Vec2::new(100.0, 80.0))).unwrap();
    assert!(s.running.is_none());
    let d = s.doc().unwrap();
    assert_eq!(d.header.point("LIMMIN").map(|p| (p.x, p.y)), Some((0.0, 0.0)));
    assert_eq!(d.header.point("LIMMAX").map(|p| (p.x, p.y)), Some((100.0, 80.0)));
    // ON keyword toggles limit checking and ends the command.
    s.start("limits").unwrap();
    s.input(Input::Keyword("ON".into())).unwrap();
    assert!(s.running.is_none());
    assert_eq!(s.doc().unwrap().header.f64("LIMCHECK", 0.0), 1.0);
}

#[test]
fn style_typed_lists_and_sets_current() {
    let mut s = Session::new();
    s.start("style").unwrap();
    s.input(Input::Keyword("?".into())).unwrap();
    assert!(logged(&s, "Standard"), "{:?}", s.log);
    s.input(Input::Text("MyStyle".into())).unwrap();
    assert!(s.running.is_none());
    let d = s.doc().unwrap();
    assert_eq!(d.header.str("TEXTSTYLE", ""), "MyStyle");
    assert!(d.text_styles.iter().any(|t| t.name == "MyStyle"));
}

#[test]
fn dimstyle_typed_save_then_restore() {
    let mut s = Session::new();
    s.start("dimstyle").unwrap();
    s.input(Input::Keyword("Save".into())).unwrap();
    s.input(Input::Text("Mine".into())).unwrap();
    assert!(s.running.is_none());
    let d = s.doc().unwrap();
    assert!(d.dim_styles.iter().any(|t| t.name == "Mine"));
    assert_eq!(d.header.str("DIMSTYLE", ""), "Standard", "Save must not switch the current style");
    s.start("dimstyle").unwrap();
    s.input(Input::Text("Mine".into())).unwrap();
    assert!(s.running.is_none());
    assert_eq!(s.doc().unwrap().header.str("DIMSTYLE", ""), "Mine");
}

#[test]
fn leader_typed_points_then_text() {
    let mut s = Session::new();
    s.start("leader").unwrap();
    s.input(Input::Point(Vec2::new(0.0, 0.0))).unwrap();
    s.input(Input::Point(Vec2::new(10.0, 10.0))).unwrap();
    s.input(Input::Enter).unwrap();
    assert!(prompt_text(&s).contains("text"), "{}", prompt_text(&s));
    s.input(Input::Text("note".into())).unwrap();
    assert!(s.running.is_none());
    assert_eq!(s.doc().unwrap().model.len(), 2, "leader + its text");
}

#[test]
fn base_typed_sets_insbase() {
    let mut s = Session::new();
    s.start("base").unwrap();
    s.input(Input::Point(Vec2::new(5.0, 6.0))).unwrap();
    assert!(s.running.is_none());
    assert_eq!(s.doc().unwrap().header.point("INSBASE").map(|p| (p.x, p.y)), Some((5.0, 6.0)));
}

#[test]
fn breakatpoint_typed_pick_then_point() {
    let mut s = Session::new();
    s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap();
    let h = s.doc().unwrap().model.last().unwrap().handle;
    s.start("breakatpoint").unwrap();
    s.input(Input::Pick(vec![h])).unwrap();
    assert!(prompt_text(&s).contains("break point"), "{}", prompt_text(&s));
    s.input(Input::Point(Vec2::new(5.0, 0.0))).unwrap();
    assert!(s.running.is_none());
    assert_eq!(s.doc().unwrap().model.len(), 2, "one line split into two");
}

#[test]
fn laymch_typed_pick_destination_object() {
    let mut s = Session::new();
    s.execute("layer.new", &json!({ "name": "A" })).unwrap();
    s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap();
    s.execute("line", &json!({ "points": [[0, 5], [10, 5]] })).unwrap();
    let (h1, h2) = {
        let d = s.doc().unwrap();
        (d.model.iter().next().unwrap().handle, d.model.iter().nth(1).unwrap().handle)
    };
    s.execute("properties.set", &json!({ "handles": [h2.hex()], "layer": "A" })).unwrap();
    s.start("laymch").unwrap();
    s.input(Input::Pick(vec![h1])).unwrap();
    s.input(Input::Enter).unwrap();
    s.input(Input::Pick(vec![h2])).unwrap();
    assert!(s.running.is_none());
    let d = s.doc().unwrap();
    assert_eq!(d.model.iter().next().unwrap().common.layer, "A");
    assert!(logged(&s, "changed to layer A"), "{:?}", s.log);
}

#[test]
fn measuregeom_typed_distance_loops_back_to_options() {
    let mut s = Session::new();
    s.start("measuregeom").unwrap();
    s.input(Input::Keyword("Distance".into())).unwrap();
    s.input(Input::Point(Vec2::new(0.0, 0.0))).unwrap();
    s.input(Input::Point(Vec2::new(3.0, 4.0))).unwrap();
    assert!(logged(&s, "Distance = 5"), "{:?}", s.log);
    assert!(s.running.is_some(), "MEASUREGEOM stays open for another option, like AutoCAD");
    s.input(Input::Enter).unwrap();
    assert!(s.running.is_none());
}

#[test]
fn angle_measure_typed_three_points() {
    let mut s = Session::new();
    s.start("angle.measure").unwrap();
    s.input(Input::Point(Vec2::new(0.0, 0.0))).unwrap();
    s.input(Input::Point(Vec2::new(10.0, 0.0))).unwrap();
    s.input(Input::Point(Vec2::new(0.0, 10.0))).unwrap();
    assert!(s.running.is_none());
    assert!(logged(&s, "Angle = 90"), "{:?}", s.log);
}

#[test]
fn radius_measure_typed_pick_circle() {
    let mut s = Session::new();
    s.execute("circle", &json!({ "center": [0, 0], "radius": 7.0 })).unwrap();
    let h = s.doc().unwrap().model.last().unwrap().handle;
    s.start("radius.measure").unwrap();
    s.input(Input::Pick(vec![h])).unwrap();
    assert!(s.running.is_none());
    assert!(logged(&s, "Radius = 7"), "{:?}", s.log);
}

#[test]
fn wblock_typed_reaches_the_file_prompt() {
    let mut s = Session::new();
    s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap();
    let h = s.doc().unwrap().model.last().unwrap().handle;
    s.start("wblock").unwrap();
    s.input(Input::Enter).unwrap(); // blank name = select objects
    s.input(Input::Pick(vec![h])).unwrap();
    s.input(Input::Enter).unwrap(); // finish selection
    s.input(Input::Point(Vec2::new(0.0, 0.0))).unwrap(); // base point
    assert!(prompt_text(&s).contains("file name"), "{}", prompt_text(&s));
    s.input(Input::Cancel).unwrap();
    assert!(s.running.is_none());
}

#[test]
fn datalink_typed_selects_table_then_path() {
    let mut s = Session::new();
    s.execute("table", &json!({ "at": [0, 0], "rows": 3, "cols": 2 })).unwrap();
    let h = s.doc().unwrap().model.last().unwrap().handle;
    let csv = std::env::temp_dir().join("cadcraft-audit-datalink.csv");
    std::fs::write(&csv, "a,b\n1,2\n3,4\n").unwrap();
    s.start("datalink").unwrap();
    s.input(Input::Pick(vec![h])).unwrap();
    assert!(prompt_text(&s).contains("CSV"), "{}", prompt_text(&s));
    s.input(Input::Text(csv.to_string_lossy().to_string())).unwrap();
    assert!(s.running.is_none());
    assert!(logged(&s, "Table linked"), "{:?}", s.log);
    let _ = std::fs::remove_file(&csv);
}

#[test]
fn ovimap_georef_typed_sets_origin() {
    let mut s = Session::new();
    s.start("ovimap.georef").unwrap();
    s.input(Input::Text("32.1301".into())).unwrap();
    s.input(Input::Text("114.0814".into())).unwrap();
    assert!(s.running.is_none());
    let g = s.doc().unwrap().georef.expect("georef set");
    assert!((g.origin_lat - 32.1301).abs() < 1e-9);
    assert!((g.origin_lon - 114.0814).abs() < 1e-9);
}

#[test]
fn dist_multiple_points_arc_segment() {
    let mut s = Session::new();
    s.start("dist").unwrap();
    s.input(Input::Point(Vec2::new(0.0, 0.0))).unwrap();
    s.input(Input::Keyword("Multiple points".into())).unwrap();
    // The Arc keyword used to answer "Arc: not available yet" despite being offered.
    s.input(Input::Keyword("Arc".into())).unwrap();
    s.input(Input::Point(Vec2::new(5.0, 5.0))).unwrap(); // point on the arc
    s.input(Input::Point(Vec2::new(10.0, 0.0))).unwrap(); // arc end: semicircle, r = 5
    assert!(logged(&s, "15.708"), "{:?}", s.log); // 5 * pi
    s.input(Input::Keyword("Total".into())).unwrap();
    assert!(s.running.is_none());
}

#[test]
fn area_with_an_arc_side() {
    let mut s = Session::new();
    s.start("area").unwrap();
    s.input(Input::Point(Vec2::new(0.0, 0.0))).unwrap();
    s.input(Input::Point(Vec2::new(10.0, 0.0))).unwrap();
    s.input(Input::Point(Vec2::new(10.0, 10.0))).unwrap();
    s.input(Input::Keyword("Arc".into())).unwrap();
    s.input(Input::Point(Vec2::new(5.0, 15.0))).unwrap(); // outward semicircle on top
    s.input(Input::Point(Vec2::new(0.0, 10.0))).unwrap();
    s.input(Input::Enter).unwrap();
    // 100 (square) + 39.2699 (semicircle area) = 139.2699
    assert!(logged(&s, "139.2699"), "{:?}", s.log);
    assert!(s.running.is_none());
}

#[test]
fn dimtedit_typed_prompts_for_a_dimension() {
    let mut s = Session::new();
    s.execute("dimlinear", &json!({ "p1": [0, 0], "p2": [10, 0], "at": [5, 4] })).unwrap();
    let h = s.doc().unwrap().model.last().unwrap().handle;
    // No pickfirst selection: typed DIMTEDIT Left must prompt, not error out.
    s.start("dimtedit.left").unwrap();
    s.input(Input::Pick(vec![h])).unwrap();
    assert!(s.running.is_none(), "single-pick SelectRun finishes the command");
}

#[test]
fn ovimap_push_typed_asks_for_project_id() {
    let mut s = Session::new();
    s.start("ovimap.push").unwrap();
    assert!(prompt_text(&s).contains("project id"), "{}", prompt_text(&s));
    s.input(Input::Cancel).unwrap();
    assert!(s.running.is_none());
}
