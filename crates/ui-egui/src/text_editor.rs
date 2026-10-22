//! The in-place TEXT editor: while TEXTEDIT collects a single-line text's new
//! value, a text box opens on the canvas at the text's insertion point, exactly
//! as the MTEXT editor does for multiline text. A double-click on a TEXT lands
//! here; the command line's "Enter new text" prompt is only the fallback for
//! objects with no in-place editor (attribute definitions).

use egui::{RichText, Stroke, pos2};

use crate::CadApp;
use crate::theme::Tokens;

/// Smallest and largest size of the text in the box, in points: the box stays readable at any zoom.
const MIN_PX: f32 = 13.0;
const MAX_PX: f32 = 36.0;

#[derive(Clone)]
struct State {
    /// The text the editor was opened for (its value when picked): a different
    /// pick re-initialises the buffer.
    picked: String,
    buf: String,
}

fn state_id() -> egui::Id {
    egui::Id::new("text_editor_state")
}

fn edit_id() -> egui::Id {
    egui::Id::new("text_editor_text")
}

/// Show the editor while TEXTEDIT collects a TEXT's value; clears its state when it isn't running.
pub fn show(app: &mut CadApp, ctx: &egui::Context) {
    let text = app.session.editing_text();
    let asking = app.session.current_prompt().is_some_and(|p| p.accept.text && !p.accept.point);
    let (Some(t), true) = (text, asking) else {
        ctx.data_mut(|d| d.remove::<State>(state_id()));
        return;
    };
    let tk = Tokens::get();
    let (rect, scale) = app.canvas.xf.map_or((ctx.content_rect(), 20.0), |x| (x.rect, x.scale.max(1e-6)));
    let mut st = ctx
        .data_mut(|d| d.get_temp::<State>(state_id()))
        .filter(|s| s.picked == t.value)
        .unwrap_or_else(|| State { picked: t.value.clone(), buf: t.value.clone() });
    let base_h = if t.height > 0.0 { t.height } else { 0.2 };
    let px = ((base_h * scale) as f32).clamp(MIN_PX, MAX_PX);
    // Roughly one em per CJK character, half an em per Latin one; the box is a
    // minimum of 260 px and never wider than the canvas.
    let est: f32 = t.value.chars().map(|c| if cadcraft_fonts::is_wide(c) { px } else { px * 0.55 }).sum();
    let width = est.clamp(260.0, (rect.width() - 16.0).max(260.0));
    let at = app.canvas.xf.map_or(rect.center(), |x| x.to_screen(t.insert.xy()));
    let pos =
        pos2(at.x.clamp(rect.left(), (rect.right() - width - 8.0).max(rect.left())), at.y.clamp(rect.top(), (rect.bottom() - 90.0).max(rect.top())));

    let mut done = Done::No;
    egui::Area::new(egui::Id::new("text_editor_area")).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        egui::Frame::NONE.fill(tk.panel).stroke(Stroke::new(1.0, tk.accent)).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
            ui.set_width(width);
            let out = egui::TextEdit::singleline(&mut st.buf)
                .id(edit_id())
                .desired_width(width)
                .font(egui::FontId::proportional(px))
                .lock_focus(true)
                .char_limit(cadcraft_engine::mtext_edit::MAX_CHARS)
                .show(ui);
            if !out.response.has_focus() {
                out.response.request_focus();
            }
            ui.horizontal(|ui| {
                if ui.button(crate::tl!("OK")).clicked() {
                    done = Done::Ok;
                }
                if ui.button(crate::tl!("Cancel")).clicked() {
                    done = Done::Cancel;
                }
                ui.label(RichText::new(crate::tl!("Enter to finish")).small().color(tk.text_dim));
            });
        });
    });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        done = Done::Cancel;
    } else if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
        done = Done::Ok;
    }
    match done {
        Done::No => {
            ctx.data_mut(|d| d.insert_temp(state_id(), st));
        }
        Done::Ok => {
            ctx.data_mut(|d| d.remove::<State>(state_id()));
            // TEXTEDIT takes the text even when the editor was emptied (clearing
            // the TEXT), exactly as it does from the MTEXT editor.
            let _ = app.session.input(cadcraft_engine::Input::Text(st.buf));
        }
        Done::Cancel => {
            ctx.data_mut(|d| d.remove::<State>(state_id()));
            app.session.cancel();
        }
    }
}

#[derive(PartialEq)]
enum Done {
    No,
    Ok,
    Cancel,
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_engine::Session;
    use egui::{Rect, vec2};
    use serde_json::json;

    use crate::Services;

    #[test]
    fn editor_opens_for_textedit_on_a_text_with_its_value() {
        let ctx = egui::Context::default();
        let mut app = CadApp::new(Session::new(), Services::default());
        let frame = |app: &mut CadApp| {
            let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))), ..Default::default() };
            for _ in 0..2 {
                let mut out = ctx.run_ui(input.clone(), |ui| show(app, ui.ctx()));
                out.textures_delta.clear();
            }
        };
        app.session.execute("text", &json!({"at": [0, 0], "text": "hello"})).unwrap();
        app.start("textedit");
        let d = app.session.doc().unwrap();
        let e = d.model.iter().find(|e| matches!(&e.kind, cadcraft_engine::doc::EntityKind::Text(_))).unwrap().clone();
        let b = cadcraft_doc::entity_bounds(d, &e, 0);
        // On the first letter's stem: text picks land on glyph ink now, not the frame.
        let on_ink = cadcraft_engine::geom::Vec2::new(b.min.x + b.width() * 0.03, b.center().y);
        let _ = d;
        app.session.input(cadcraft_engine::Input::Point(on_ink)).unwrap();
        assert!(app.session.editing_text().is_some(), "textedit offers the picked text");
        frame(&mut app);
        let st = ctx.data_mut(|d| d.get_temp::<State>(state_id())).expect("editor open for textedit on a TEXT");
        assert_eq!(st.buf, "hello", "the editor starts from the existing value");
    }
}
