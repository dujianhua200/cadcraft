//! A real window for a LISP DCL dialog: while a routine waits on `start_dialog`, this
//! shows its tiles as egui widgets — buttons run their `action_tile` expression (which
//! usually calls `done_dialog`), edit boxes / toggles / radios / lists / sliders write
//! back through `set_tile` semantics before a button press, and `image` tiles stay a
//! blank placeholder (their slides are not decoded). Closing the window or pressing Esc
//! dismisses the dialog (`start_dialog` returns 0); the headless path (a blocking
//! evaluation, tests driving `lisp_dialog_press` directly) is untouched.
//!
//! Layout attributes (`width`, `fixed_width`, `alignment`, …) are hints the window does
//! not honour precisely: the dialog auto-sizes and scrolls. `mode_tile` 1 disables a
//! tile, 2 hides it.

use std::collections::HashMap;

use cadcraft_engine::lisp::dcl::{DclDialogView, DclTile};
use egui::{Color32, Stroke, vec2};

use crate::CadApp;

#[derive(Clone, Default)]
struct State {
    dialog: String,
    /// Tile values the user changed in this dialog, not yet pushed to the session.
    edits: HashMap<String, String>,
}

fn state_id() -> egui::Id {
    egui::Id::new("dcl_dialog_state")
}

/// Show the dialog a LISP routine waits on, if any.
pub fn show(app: &mut CadApp, ctx: &egui::Context) {
    let Some(view) = app.session.lisp_dialog_open() else {
        ctx.data_mut(|d| d.remove::<State>(state_id()));
        return;
    };
    let mut st = ctx.data_mut(|d| d.get_temp::<State>(state_id())).unwrap_or_default();
    if st.dialog != view.name {
        st = State { dialog: view.name.clone(), edits: HashMap::new() };
    }
    let title = if view.label.is_empty() { view.name.clone() } else { view.label.clone() };
    let mut open = true;
    let mut pressed: Option<String> = None;
    let mut dismiss = false;
    egui::Window::new(title)
        .id(egui::Id::new(("dcl_dialog", view.name.clone())))
        .collapsible(false)
        .resizable(true)
        .default_width(420.0)
        .open(&mut open)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for t in &view.tiles {
                    render_tile(ui, app, &view, &mut st, t, None, &mut pressed);
                }
            });
        });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        dismiss = true;
    }
    if !open {
        dismiss = true;
    }
    if let Some(key) = pressed {
        press(app, &mut st, &key);
    } else if dismiss {
        // Closing the window or Esc discards the edits: the dialog is cancelled.
        ctx.data_mut(|d| d.remove::<State>(state_id()));
        app.session.lisp_dialog_finish(0);
    } else {
        ctx.data_mut(|d| d.insert_temp(state_id(), st));
    }
}

/// Push the edits, run the button's action, and finish the dialog when the action
/// recorded a `done_dialog` status.
fn press(app: &mut CadApp, st: &mut State, key: &str) {
    for (k, v) in &st.edits {
        app.session.lisp_dialog_set_tile(k, v);
    }
    st.edits.clear();
    match app.session.lisp_dialog_press(key) {
        Ok(Some(Err(msg))) => app.session.echo(format!("对话框按钮 {key} 的动作失败：{msg}")),
        Err(e) => app.session.echo(format!("对话框按钮 {key} 的动作失败：{e}")),
        _ => {}
    }
    if let Some(status) = app.session.lisp_dialog_done_status() {
        app.session.lisp_dialog_finish(status);
    }
}

/// Whether the tile is shown and enabled: `mode_tile` 1 disables, 2 hides; the
/// `is_enabled = false` attribute also disables.
fn enabled(view: &DclDialogView, t: &DclTile) -> Option<bool> {
    if view.modes.get(&t.key).is_some_and(|m| *m == 2) {
        return None;
    }
    let mut on = !view.modes.get(&t.key).is_some_and(|m| *m == 1);
    if t.attrs.get("is_enabled").is_some_and(|v| v.eq_ignore_ascii_case("false")) {
        on = false;
    }
    Some(on)
}

/// The tile's current value: an edit the user made wins, else the session's value.
fn value(view: &DclDialogView, st: &State, key: &str) -> String {
    st.edits.get(key).cloned().or_else(|| view.values.get(key).cloned()).unwrap_or_default()
}

/// Items of a list / popup tile: its `list` attribute, one item per line.
fn items(t: &DclTile) -> Vec<String> {
    t.attrs.get("list").map(|s| s.split('\n').map(str::to_string).collect()).unwrap_or_default()
}

/// The selected index of a list / popup tile, clamped to its items.
fn selected(view: &DclDialogView, st: &State, t: &DclTile) -> usize {
    let n = items(t).len();
    if n == 0 {
        return 0;
    }
    value(view, st, &t.key).parse::<usize>().unwrap_or(0).min(n - 1)
}

fn render_tile(
    ui: &mut egui::Ui,
    app: &mut CadApp,
    view: &DclDialogView,
    st: &mut State,
    t: &DclTile,
    group: Option<&[String]>,
    pressed: &mut Option<String>,
) {
    match t.kind.as_str() {
        "row" | "concatenation" => {
            ui.horizontal(|ui| {
                for c in &t.children {
                    render_tile(ui, app, view, st, c, None, pressed);
                }
            });
        }
        "column" | "paragraph" => {
            ui.vertical(|ui| {
                for c in &t.children {
                    render_tile(ui, app, view, st, c, None, pressed);
                }
            });
        }
        "boxed_row" | "boxed_column" => {
            let title = if t.label.is_empty() { None } else { Some(t.label.clone()) };
            egui::Frame::NONE.stroke(Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color)).inner_margin(egui::Margin::same(6)).show(
                ui,
                |ui| {
                    if let Some(title) = title {
                        ui.label(egui::RichText::new(title).strong());
                    }
                    if t.kind == "boxed_row" {
                        ui.horizontal(|ui| {
                            for c in &t.children {
                                render_tile(ui, app, view, st, c, None, pressed);
                            }
                        });
                    } else {
                        ui.vertical(|ui| {
                            for c in &t.children {
                                render_tile(ui, app, view, st, c, None, pressed);
                            }
                        });
                    }
                },
            );
        }
        "radio_column" | "radio_row" => {
            let keys: Vec<String> = t.children.iter().filter(|c| c.kind == "radio_button").map(|c| c.key.clone()).collect();
            if t.kind == "radio_column" {
                ui.vertical(|ui| {
                    for c in &t.children {
                        render_tile(ui, app, view, st, c, Some(&keys), pressed);
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    for c in &t.children {
                        render_tile(ui, app, view, st, c, Some(&keys), pressed);
                    }
                });
            }
        }
        "spacer" => {
            ui.add_space(8.0);
        }
        "text" | "label" => {
            if !t.label.is_empty() {
                ui.label(&t.label);
            }
        }
        "button" | "image_button" => {
            let Some(on) = enabled(view, t) else { return };
            let label = if t.label.is_empty() { t.key.clone() } else { t.label.clone() };
            let r = ui.add_enabled(on, egui::Button::new(label));
            if r.clicked() {
                if view.actions.iter().any(|a| a == &t.key) {
                    *pressed = Some(t.key.clone());
                } else if t.attrs.get("is_cancel").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
                    // A cancel button without an action still closes the dialog, and
                    // discards the edits like closing the window does.
                    app.session.lisp_dialog_finish(0);
                }
            }
        }
        "edit_box" => {
            let Some(on) = enabled(view, t) else { return };
            let mut v = value(view, st, &t.key);
            ui.horizontal(|ui| {
                if !t.label.is_empty() {
                    ui.label(&t.label);
                }
                let r = ui.add_enabled(on, egui::TextEdit::singleline(&mut v).id(egui::Id::new(("dcl_edit", t.key.clone()))).desired_width(180.0));
                if r.changed() {
                    st.edits.insert(t.key.clone(), v);
                }
            });
        }
        "toggle" => {
            let Some(on) = enabled(view, t) else { return };
            let mut checked = value(view, st, &t.key) == "1";
            let label = if t.label.is_empty() { t.key.clone() } else { t.label.clone() };
            if ui.add_enabled(on, egui::Checkbox::new(&mut checked, label)).changed() {
                st.edits.insert(t.key.clone(), if checked { "1".into() } else { "0".into() });
            }
        }
        "radio_button" => {
            let Some(on) = enabled(view, t) else { return };
            let label = if t.label.is_empty() { t.key.clone() } else { t.label.clone() };
            // In a radio group exactly one button is on: this one when its value is 1.
            let selected = value(view, st, &t.key) == "1";
            if ui.add_enabled(on, egui::RadioButton::new(selected, label)).clicked() {
                if let Some(keys) = group {
                    for k in keys {
                        st.edits.insert(k.clone(), if *k == t.key { "1".into() } else { "0".into() });
                    }
                } else {
                    st.edits.insert(t.key.clone(), "1".into());
                }
            }
        }
        "slider" => {
            let Some(on) = enabled(view, t) else { return };
            let min = t.attrs.get("min_value").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
            let max = t.attrs.get("max_value").and_then(|v| v.parse::<f64>().ok()).unwrap_or(100.0);
            let (lo, hi) = (min.min(max), min.max(max));
            let mut v = value(view, st, &t.key).parse::<f64>().unwrap_or(lo).clamp(lo, hi);
            ui.horizontal(|ui| {
                if !t.label.is_empty() {
                    ui.label(&t.label);
                }
                if ui.add_enabled(on, egui::Slider::new(&mut v, lo..=hi)).changed() {
                    st.edits.insert(t.key.clone(), format!("{}", v.round() as i64));
                }
            });
        }
        "list_box" => {
            let Some(on) = enabled(view, t) else { return };
            let list = items(t);
            if !t.label.is_empty() {
                ui.label(&t.label);
            }
            let mut sel = selected(view, st, t);
            egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                for (i, item) in list.iter().enumerate() {
                    if ui.add_enabled_ui(on, |ui| ui.selectable_label(sel == i, item)).inner.clicked() {
                        sel = i;
                        st.edits.insert(t.key.clone(), i.to_string());
                    }
                }
            });
        }
        "popup_list" => {
            let Some(on) = enabled(view, t) else { return };
            let list = items(t);
            let mut sel = selected(view, st, t);
            ui.horizontal(|ui| {
                if !t.label.is_empty() {
                    ui.label(&t.label);
                }
                let shown = list.get(sel).cloned().unwrap_or_default();
                egui::ComboBox::from_id_salt(("dcl_popup", t.key.clone())).selected_text(shown).show_ui(ui, |ui| {
                    for (i, item) in list.iter().enumerate() {
                        if ui.add_enabled_ui(on, |ui| ui.selectable_label(sel == i, item)).inner.clicked() {
                            sel = i;
                            st.edits.insert(t.key.clone(), i.to_string());
                        }
                    }
                });
            });
        }
        "image" => {
            // Blank placeholder: the slide is not decoded (see the module docs).
            let w = t.attrs.get("width").and_then(|v| v.parse::<f32>().ok()).unwrap_or(32.0).clamp(4.0, 80.0) * 6.0;
            let h = t.attrs.get("height").and_then(|v| v.parse::<f32>().ok()).unwrap_or(16.0).clamp(1.0, 40.0) * 14.0;
            let (rect, _) = ui.allocate_exact_size(vec2(w, h), egui::Sense::hover());
            ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::from_gray(140)), egui::StrokeKind::Outside);
        }
        _ => {
            // A tile kind this window does not model yet: show its label, never a hole.
            if !t.label.is_empty() {
                ui.label(&t.label);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_engine::Session;

    use crate::Services;

    #[test]
    fn dialog_window_renders_and_leaves_the_waiting_routine_alone() {
        let dir = std::env::temp_dir().join("cadcraft-dcl-ui-show-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("box.dcl"),
            "mainbox : dialog { label = \"Test\"; : text { label = \"Hi\"; } : edit_box { label = \"Name\"; key = \"name\"; } : toggle { label = \"On\"; key = \"on\"; } : slider { label = \"Level\"; key = \"level\"; min_value = 0; max_value = 10; } : list_box { label = \"Pick\"; key = \"pick\"; list = \"a\\nb\\nc\"; } : image { key = \"img\"; } : row { : button { label = \"Go\"; key = \"b1\"; } : button { label = \"Close\"; key = \"exitbtn\"; is_cancel = true; } } }\n",
        )
        .unwrap();
        let ctx = egui::Context::default();
        let mut app = CadApp::new(Session::new(), Services::default());
        app.session.set_lisp_support_dir(Some(dir.clone()));
        let r = app
            .session
            .lisp_eval(
                "(progn (setq id (load_dialog \"box.dcl\")) (new_dialog \"mainbox\" id) (action_tile \"b1\" \"(done_dialog 1)\") (setq dlg_result (start_dialog)))",
                false,
                false,
                Vec::new(),
            )
            .unwrap();
        assert!(r.is_none(), "the routine waits on the dialog");
        // Two frames: the window shows (every tile kind above renders), the routine
        // still waits — showing must neither close the dialog nor finish the routine.
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1200.0, 800.0))),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| show(&mut app, ui.ctx()));
            out.textures_delta.clear();
        }
        assert!(app.session.lisp_dialog_open().is_some(), "showing leaves the dialog open");
        assert!(app.session.lisp_dialog_finish(0), "dismiss finishes the routine");
        assert!(app.session.lisp_dialog_open().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
