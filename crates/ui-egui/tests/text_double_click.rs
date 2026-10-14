//! Double-clicking text with no command running edits it, as in AutoCAD (#842).

use cadcraft_engine::doc::{EntityKind, entity_bounds};
use cadcraft_engine::geom::Vec2;
use cadcraft_engine::{Input, Session};
use cadcraft_ui_egui::i18n::Preference;
use cadcraft_ui_egui::{CadApp, Services};
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use serde_json::json;

fn harness_from(mut app: CadApp) -> Harness<'static, CadApp> {
    app.canvas.zoom_pending = false;
    // 60 frames a second: the default step (0.25 s) spreads two clicks past the double-click delay.
    let mut h = Harness::builder().with_size(vec2(1600.0, 1000.0)).with_step_dt(1.0 / 60.0).build_ui_state(
        |ui, app: &mut CadApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(5);
    h
}

fn harness() -> Harness<'static, CadApp> {
    let mut app = CadApp::new(Session::new(), Services::default());
    app.run("text", json!({ "at": [0, 0], "text": "HELLO", "height": 2.5 })).unwrap();
    app.run("mtext", json!({ "at": [0, 20], "text": "first\\Psecond", "height": 2.5, "width": 20 })).unwrap();
    app.run("line", json!({ "points": [[30, 0], [30, 20]] })).unwrap();
    app.session.execute("view.set", &json!({ "center": [15, 10], "height": 40 })).unwrap();
    harness_from(app)
}

fn double_click(h: &mut Harness<'_, CadApp>, at: Vec2) {
    let pos: Pos2 = h.state().canvas.xf.unwrap().to_screen(at);
    h.event(Event::PointerMoved(pos));
    h.run_steps(2);
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
            h.run_steps(1);
        }
    }
    h.run_steps(2);
}

fn text_at(h: &Harness<'_, CadApp>, i: usize) -> String {
    let d = h.state().session.doc().unwrap();
    let e = d.model.iter().nth(i).unwrap();
    match &e.kind {
        EntityKind::Text(t) => t.value.clone(),
        EntityKind::MText(t) => t.contents.clone(),
        k => panic!("not text: {}", k.type_name()),
    }
}

#[test]
fn double_click_on_text_starts_textedit_for_that_object() {
    for (i, at, old, new) in [(0, Vec2::new(4.0, 1.2), "HELLO", "HELLO WORLD"), (1, Vec2::new(3.0, 18.8), "first\nsecond", "edited")] {
        let mut h = harness();
        double_click(&mut h, at);
        let s = &h.state().session;
        assert_eq!(s.running.as_ref().map(|r| r.id.as_str()), Some("textedit"), "object {i}");
        let p = s.current_prompt().unwrap();
        assert_eq!(p.message, "Enter new text", "object {i}: the clicked text is already picked");
        assert_eq!(p.default.as_deref(), Some(old), "object {i}");
        h.state_mut().session.input(Input::Text(new.into())).unwrap();
        h.run_steps(2);
        assert_eq!(text_at(&h, i), new);
        // TEXTEDIT goes on asking for objects (its Multiple mode), as AutoCAD's does.
        assert_eq!(h.state().session.current_prompt().unwrap().message, "Select an annotation object");
    }
}

#[test]
fn double_click_elsewhere_starts_no_command() {
    for at in [Vec2::new(30.0, 5.0), Vec2::new(15.0, 30.0)] {
        let mut h = harness();
        double_click(&mut h, at);
        assert_eq!(h.state().session.running.as_ref().map(|r| r.id.as_str()), None, "{at:?}");
    }
}

/// A glyph point at the start/end of the first TEXT. Before the text_box root fix
/// the end glyphs overshot the pick box (it assumed 0.9 em/character while CJK
/// glyphs are ~1 em), and only the expanded-box retry found them; the box now
/// measures by renderer advance, so the end glyphs sit inside it and a
/// double-click on them must start TEXTEDIT through the plain pick.
fn end_glyph_point(app: &CadApp, last: bool) -> Option<Vec2> {
    let d = app.session.doc().ok()?;
    let e = d.model.iter().find(|e| matches!(e.kind, EntityKind::Text(_)))?;
    let EntityKind::Text(t) = &e.kind else { return None };
    let (shaped, _) = cadcraft_engine::render::place_text_entity(d, t, &t.value);
    let outline_pts = shaped.glyphs.into_iter().flatten().flatten();
    let pts: Vec<Vec2> = shaped.strokes.into_iter().flatten().chain(outline_pts).collect();
    if last {
        pts.into_iter().max_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    } else {
        pts.into_iter().min_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    }
}

#[test]
fn double_click_on_overshoot_glyph_of_cjk_text_starts_textedit() {
    // Zoomed-in view: double-click the first glyph of a CJK label. The pick box now
    // measures by renderer advance, so the first glyph is inside it.
    let mut app = CadApp::new(Session::new(), Services::default());
    app.ui.interface_language = Preference::English;
    app.run("text", json!({ "at": [0, 0], "text": "中文标签测试", "height": 2.5 })).unwrap();
    app.session.execute("view.set", &json!({ "center": [5, 1], "height": 20 })).unwrap();
    let at = end_glyph_point(&app, false).expect("CJK label has a first glyph");
    let mut h = harness_from(app);
    double_click(&mut h, at);
    let s = &h.state().session;
    assert_eq!(s.running.as_ref().map(|r| r.id.as_str()), Some("textedit"), "click at {at:?}");
    let p = s.current_prompt().unwrap();
    assert_eq!(p.message, "Enter new text");
    assert_eq!(p.default.as_deref(), Some("中文标签测试"));
}

#[test]
fn double_click_on_overshoot_glyph_of_long_cjk_text_starts_textedit() {
    // Double-click the last glyph of a long CJK label: the pick box now measures by
    // renderer advance, so the last glyph is inside it (previously it overshot by
    // ~4.75 units and only the expanded-box retry found it).
    let mut app = CadApp::new(Session::new(), Services::default());
    app.ui.interface_language = Preference::English;
    app.run("text", json!({ "at": [0, 0], "text": "这是一个很长的中文标签用于测试双击", "height": 2.5 })).unwrap();
    app.session.execute("view.set", &json!({ "center": [24, 1], "height": 40 })).unwrap();
    let at = end_glyph_point(&app, true).expect("long CJK label has a last glyph");
    let mut h = harness_from(app);
    double_click(&mut h, at);
    let s = &h.state().session;
    assert_eq!(s.running.as_ref().map(|r| r.id.as_str()), Some("textedit"), "click at {at:?}");
    let p = s.current_prompt().unwrap();
    assert_eq!(p.message, "Enter new text");
    assert_eq!(p.default.as_deref(), Some("这是一个很长的中文标签用于测试双击"));
}

/// A laid-out glyph point of the first MTEXT that draws past its pick box: with automatic
/// width the box assumes 0.8 em/character, but CJK glyphs are ~1 em wide.
fn overshoot_point_mtext(app: &CadApp) -> Option<Vec2> {
    let d = app.session.doc().ok()?;
    let e = d.model.iter().find(|e| matches!(e.kind, EntityKind::MText(_)))?;
    let EntityKind::MText(t) = &e.kind else { return None };
    let layout = cadcraft_engine::render::layout_mtext_entity(d, t);
    let b = entity_bounds(d, e, 0);
    layout.strokes.into_iter().flatten().find(|pt| !b.contains(*pt))
}

#[test]
fn double_click_on_overshoot_glyph_of_cjk_mtext_starts_textedit() {
    let mut app = CadApp::new(Session::new(), Services::default());
    app.ui.interface_language = Preference::English;
    app.run("mtext", json!({ "at": [0, 0], "text": "中文多行文字测试内容", "height": 2.5, "width": 0 })).unwrap();
    app.session.execute("view.set", &json!({ "center": [12, 0], "height": 40 })).unwrap();
    let at = overshoot_point_mtext(&app).expect("CJK MTEXT overshoots the pick box");
    let mut h = harness_from(app);
    double_click(&mut h, at);
    let s = &h.state().session;
    assert_eq!(s.running.as_ref().map(|r| r.id.as_str()), Some("textedit"), "click at {at:?}");
    let p = s.current_prompt().unwrap();
    assert_eq!(p.message, "Enter new text");
    assert_eq!(p.default.as_deref(), Some("中文多行文字测试内容"));
}
#[test]
fn double_click_small_text_at_large_coordinates_starts_textedit() {
    let mut app = CadApp::new(Session::new(), Services::default());
    app.ui.interface_language = Preference::English;
    app.run("text", json!({ "at": [30000, -10000], "text": "FAR", "height": 0.5 })).unwrap();
    app.session.execute("view.set", &json!({ "center": [30002, -9999], "height": 40 })).unwrap();
    let mut h = harness_from(app);
    double_click(&mut h, Vec2::new(30000.7, -9999.7));
    assert_eq!(h.state().session.running.as_ref().map(|r| r.id.as_str()), Some("textedit"));
}

#[test]
fn double_click_on_locked_layer_text_hints_unlock() {
    let mut app = CadApp::new(Session::new(), Services::default());
    app.ui.interface_language = Preference::English;
    app.run("text", json!({ "at": [0, 0], "text": "LOCKED", "height": 2.5 })).unwrap();
    let handle = app.session.doc().unwrap().model.iter().next().unwrap().handle.hex();
    app.run("laylck", json!({ "handles": [handle] })).unwrap();
    app.session.execute("view.set", &json!({ "center": [5, 1], "height": 40 })).unwrap();
    let mut h = harness_from(app);
    double_click(&mut h, Vec2::new(4.0, 1.2));
    assert_eq!(h.state().session.running.as_ref().map(|r| r.id.as_str()), None);
    assert_eq!(h.state().session.log.last().map(String::as_str), Some("Text on a locked layer cannot be edited. Unlock the layer first."));
}

/// Block "LBL": TEXT "INBLOCK" at (0, 0), inserted at (50, 50). The click (55, 51) is on the
/// text's glyphs but outside the reference's own geometry, so picking claims nothing and only
/// the block-text scan answers.
#[test]
fn double_click_on_text_inside_a_block_hints_block_editor() {
    let mut app = CadApp::new(Session::new(), Services::default());
    app.ui.interface_language = Preference::English;
    app.run("text", json!({ "at": [0, 0], "text": "INBLOCK", "height": 2.5 })).unwrap();
    let handle = app.session.doc().unwrap().model.iter().next().unwrap().handle.hex();
    app.run("block", json!({ "name": "LBL", "base": [0, 0], "handles": [handle], "keep": "delete" })).unwrap();
    app.run("insert", json!({ "name": "LBL", "at": [50, 50] })).unwrap();
    app.session.execute("view.set", &json!({ "center": [58, 51], "height": 40 })).unwrap();
    let mut h = harness_from(app);
    double_click(&mut h, Vec2::new(55.0, 51.0));
    assert_eq!(h.state().session.running.as_ref().map(|r| r.id.as_str()), None);
    assert_eq!(
        h.state().session.log.last().map(String::as_str),
        Some("Text inside a block cannot be edited here. Enter the block editor to edit it.")
    );
}

/// Same block, but zoomed out far enough that picking returns the INSERT itself: the
/// picked-reference path must give the same hint.
#[test]
fn double_click_on_picked_block_reference_hints_block_editor() {
    let mut app = CadApp::new(Session::new(), Services::default());
    app.ui.interface_language = Preference::English;
    app.run("text", json!({ "at": [0, 0], "text": "INBLOCK", "height": 2.5 })).unwrap();
    let handle = app.session.doc().unwrap().model.iter().next().unwrap().handle.hex();
    app.run("block", json!({ "name": "LBL", "base": [0, 0], "handles": [handle], "keep": "delete" })).unwrap();
    app.run("insert", json!({ "name": "LBL", "at": [50, 50] })).unwrap();
    app.session.execute("view.set", &json!({ "center": [58, 51], "height": 400 })).unwrap();
    let mut h = harness_from(app);
    double_click(&mut h, Vec2::new(50.5, 51.0));
    assert_eq!(h.state().session.running.as_ref().map(|r| r.id.as_str()), None);
    assert_eq!(
        h.state().session.log.last().map(String::as_str),
        Some("Text inside a block cannot be edited here. Enter the block editor to edit it.")
    );
}

/// Block "B": a line plus an attribute definition (default "one"), inserted at (50, 50).
/// Double-clicking the attribute's value hints at the block editor, as for block text.
#[test]
fn double_click_on_block_attribute_hints_block_editor() {
    let mut app = CadApp::new(Session::new(), Services::default());
    app.ui.interface_language = Preference::English;
    app.run("attdef", json!({ "tag": "T", "default": "one", "at": [0, 1], "height": 2.5 })).unwrap();
    app.run("line", json!({ "points": [[0, 0], [10, 0]] })).unwrap();
    let hs: Vec<String> = app.session.doc().unwrap().model.iter().map(|e| e.handle.hex()).collect();
    app.run("block", json!({ "name": "B", "base": [0, 0], "handles": hs, "keep": "delete" })).unwrap();
    app.run("insert", json!({ "name": "B", "at": [50, 50], "attribs": {} })).unwrap();
    app.session.execute("view.set", &json!({ "center": [55, 51], "height": 40 })).unwrap();
    let mut h = harness_from(app);
    double_click(&mut h, Vec2::new(52.0, 52.0));
    assert_eq!(h.state().session.running.as_ref().map(|r| r.id.as_str()), None);
    assert_eq!(
        h.state().session.log.last().map(String::as_str),
        Some("Text inside a block cannot be edited here. Enter the block editor to edit it.")
    );
}

#[test]
fn double_click_hints_are_translated_to_chinese() {
    use cadcraft_ui_egui::i18n::tr;
    assert_eq!(
        tr("zh", "Text inside a block cannot be edited here. Enter the block editor to edit it."),
        "块内文字不能在此编辑，请进入块编辑后再编辑。"
    );
    assert_eq!(tr("zh", "Text on a locked layer cannot be edited. Unlock the layer first."), "文字所在图层已锁定，无法编辑。请先解锁图层。");
    assert_eq!(tr("zh", "The object is outside the reference-edit working set and cannot be edited."), "该对象不在参照编辑工作集内，无法编辑。");
}
