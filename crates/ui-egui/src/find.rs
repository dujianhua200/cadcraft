//! Find and Replace (FIND): the dialog AutoCAD opens, driving the engine `find`
//! command. Find selects the matches and reports the count; Find Next cycles
//! through them, selecting and zooming to each; Replace acts on the located
//! object; Replace All acts on the whole drawing.

use egui::vec2;
use serde_json::{Value, json};

use crate::CadApp;

#[derive(Clone, Debug, Default)]
struct FindState {
    find: String,
    replace: String,
    match_case: bool,
    whole_word: bool,
    /// Handles of the current match set (objects, in drawing order).
    matches: Vec<String>,
    /// Index of the located match within `matches` (Find Next position).
    at: Option<usize>,
    /// Total occurrences across `matches`, as reported by the engine.
    total: usize,
    /// Last status line (counts, errors).
    status: String,
}

fn params(st: &FindState, replace: bool, handles: Option<&[String]>) -> Value {
    let mut p = json!({ "find": st.find, "matchCase": st.match_case, "wholeWord": st.whole_word });
    if replace {
        p["replace"] = json!(st.replace);
    }
    if let Some(hs) = handles {
        p["handles"] = json!(hs);
    }
    p
}

/// Run a find-only search and refresh the match set.
fn search(app: &mut CadApp, st: &mut FindState) {
    if st.find.is_empty() {
        st.status = crate::tl!("Enter the text to find.").to_string();
        return;
    }
    match app.session.execute("find", &params(st, false, None)) {
        Ok(r) => {
            st.matches =
                r["matches"].as_array().map(|a| a.iter().filter_map(|m| m["handle"].as_str().map(str::to_string)).collect()).unwrap_or_default();
            st.total = r["total"].as_u64().unwrap_or(0) as usize;
            st.at = None;
            st.status = r["message"].as_str().unwrap_or_default().to_string();
        }
        Err(e) => st.status = e.to_string(),
    }
}

/// Select the match at `at` and zoom to it.
fn locate(app: &mut CadApp, st: &mut FindState) {
    let Some(i) = st.at else { return };
    let Some(h) = st.matches.get(i).cloned() else { return };
    let _ = app.run("zoom", json!({ "mode": "object", "handles": [h] }));
    st.status = format!("{} {} {} {}", crate::tl!("Match"), i + 1, crate::tl!("of"), st.matches.len());
}

pub fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let sid = egui::Id::new("find_state");
    let mut st: FindState = ctx.data_mut(|d| d.get_temp(sid)).unwrap_or_default();
    let mut close = false;
    egui::Window::new(crate::tl!("Find and Replace"))
        .id(egui::Id::new("Find and Replace"))
        .open(open)
        .collapsible(false)
        .resizable(false)
        .default_width(420.0)
        .show(ctx, |ui| {
            egui::Grid::new("find_grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                ui.label(crate::tl!("Find what:"));
                ui.add(egui::TextEdit::singleline(&mut st.find).desired_width(280.0));
                ui.end_row();
                ui.label(crate::tl!("Replace with:"));
                ui.add(egui::TextEdit::singleline(&mut st.replace).desired_width(280.0));
                ui.end_row();
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut st.match_case, crate::tl!("Match case"));
                ui.checkbox(&mut st.whole_word, crate::tl!("Find whole words only"));
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(crate::tl!("Find")).clicked() {
                    search(app, &mut st);
                }
                let has_matches = !st.matches.is_empty();
                if ui.add_enabled(has_matches, egui::Button::new(crate::tl!("Find Next"))).clicked() {
                    st.at = Some(match st.at {
                        None => 0,
                        Some(i) => (i + 1) % st.matches.len().max(1),
                    });
                    locate(app, &mut st);
                }
                if ui.add_enabled(st.at.is_some(), egui::Button::new(crate::tl!("Replace"))).clicked()
                    && let Some(i) = st.at
                    && let Some(h) = st.matches.get(i).cloned()
                {
                    match app.session.execute("find", &params(&st, true, Some(&[h]))) {
                        Ok(r) => {
                            st.status = r["message"].as_str().unwrap_or_default().to_string();
                            // The located object no longer matches; refresh the set.
                            search(app, &mut st);
                        }
                        Err(e) => st.status = e.to_string(),
                    }
                }
                if ui.button(crate::tl!("Replace All")).clicked() {
                    if st.find.is_empty() {
                        st.status = crate::tl!("Enter the text to find.").to_string();
                    } else {
                        match app.session.execute("find", &params(&st, true, None)) {
                            Ok(r) => {
                                st.status = r["message"].as_str().unwrap_or_default().to_string();
                                st.matches.clear();
                                st.at = None;
                                st.total = 0;
                            }
                            Err(e) => st.status = e.to_string(),
                        }
                    }
                }
                if ui.button(crate::tl!("Close")).clicked() {
                    close = true;
                }
            });
            if !st.status.is_empty() {
                ui.separator();
                ui.label(&st.status);
            }
            if st.total > 0 {
                ui.label(format!("{} {} {} {}", st.total, crate::tl!("match(es) in"), st.matches.len(), crate::tl!("object(s)")));
            }
        });
    ctx.data_mut(|d| d.insert_temp(sid, st));
    if close {
        *open = false;
    }
}
