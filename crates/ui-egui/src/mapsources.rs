//! Map Sources dialog (Tools ▸ OviMap ▸ Map Sources…, and the toolbar
//! button beside Online Map): the AutoCAD-style front end for the tile
//! source library (`docs/ovimap-link.md` §9.5–§9.6).
//!
//! The dialog edits a working copy of the library. Nothing is committed
//! until OK: OK saves the library (it persists with the app config, so it
//! survives restarts) and applies the current source to the drawing through
//! the `tilemap` command; Cancel discards the whole draft. A source's
//! attribution is mandatory — a source without one cannot be saved, and an
//! OviMap XML import without an attribution is refused, because the XML
//! carries no attribution field.
//!
//! The two built-ins (OSM, CARTO) cannot be deleted and their URL /
//! attribution cannot be changed; only their max zoom may be edited.

use cadcraft_doc::gis::{CoordType, DEFAULT_TILE_MAX_ZOOM, MapSource, MapSourceLibrary, parse_ovi_custom_map_source};
use serde_json::json;

use crate::CadApp;

/// One source being added or edited in the dialog.
#[derive(Clone, Debug, Default)]
pub struct EditDraft {
    /// Source id (editable; empty → derived from the name on save).
    pub id: String,
    pub name: String,
    pub url_template: String,
    pub attribution: String,
    pub max_zoom: String,
    /// `true` → `Mercator(china)` (GCJ-02, display-only); `false` → `Mercator`.
    pub coord_gcj: bool,
    pub tile_format: String,
    pub tile_size: String,
    /// `true` for a new source, `false` when editing an existing one.
    pub is_new: bool,
    /// `true` when editing a built-in (URL / attribution locked).
    pub builtin: bool,
}

impl EditDraft {
    fn from_source(s: &MapSource, is_new: bool) -> Self {
        EditDraft {
            id: s.id.clone(),
            name: s.name.clone(),
            url_template: s.url_template.clone(),
            attribution: s.attribution.clone(),
            max_zoom: s.max_zoom.to_string(),
            coord_gcj: s.coord_type == CoordType::Gcj02,
            tile_format: s.tile_format.clone(),
            tile_size: s.tile_size.to_string(),
            is_new,
            builtin: s.builtin,
        }
    }

    /// Derive a library id from a name: lowercase ASCII letters / digits /
    /// `-`, everything else collapsed to `-`. Empty → `"custom"`.
    fn slug(name: &str) -> String {
        let mut out = String::new();
        let mut dash = true;
        for c in name.chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c.to_ascii_lowercase());
                dash = false;
            } else if !dash {
                out.push('-');
                dash = true;
            }
        }
        while out.ends_with('-') {
            out.pop();
        }
        if out.is_empty() { "custom".into() } else { out }
    }

    /// Build the source: parses the numeric fields (empty ones fall back
    /// to the OviMap import defaults: max zoom 19, PNG, 256 px), derives
    /// an id when the field is empty, and validates (attribution
    /// mandatory, template, caps).
    fn build(&self) -> Result<MapSource, String> {
        let id = if self.id.trim().is_empty() { Self::slug(&self.name) } else { self.id.trim().to_string() };
        let max_zoom = if self.max_zoom.trim().is_empty() {
            DEFAULT_TILE_MAX_ZOOM
        } else {
            self.max_zoom.trim().parse::<u32>().ok().filter(|z| *z <= 30).ok_or_else(|| "最高级别须为 0~30 的整数".to_string())?
        };
        let tile_size = if self.tile_size.trim().is_empty() {
            256
        } else {
            self.tile_size.trim().parse::<u32>().ok().filter(|s| (64..=1024).contains(s)).ok_or_else(|| "瓦片尺寸须为 64~1024 的整数".to_string())?
        };
        let tile_format = if self.tile_format.trim().is_empty() { "png".to_string() } else { self.tile_format.trim().to_ascii_lowercase() };
        let source = MapSource {
            id,
            name: self.name.trim().to_string(),
            url_template: self.url_template.trim().to_string(),
            attribution: self.attribution.trim().to_string(),
            max_zoom,
            coord_type: if self.coord_gcj { CoordType::Gcj02 } else { CoordType::Wgs84 },
            tile_format,
            tile_size,
            builtin: self.builtin && !self.is_new,
        };
        source.validate()?;
        Ok(source)
    }
}

/// The dialog's working state: a draft library plus the edit / import
/// drafts. All transitions are pure (no egui), so tests drive them.
#[derive(Clone, Debug)]
pub struct MapSourceDialog {
    pub library: MapSourceLibrary,
    pub selected: Option<String>,
    pub editing: Option<EditDraft>,
    pub import_xml: String,
    pub import_attribution: String,
    pub error: Option<String>,
    pub notice: Option<String>,
}

impl MapSourceDialog {
    pub fn open(library: &MapSourceLibrary) -> Self {
        MapSourceDialog {
            library: library.clone(),
            selected: Some(library.current_id.clone()),
            editing: None,
            import_xml: String::new(),
            import_attribution: String::new(),
            error: None,
            notice: None,
        }
    }

    /// Start adding a source (blank form).
    pub fn start_new(&mut self) {
        self.editing = Some(EditDraft { is_new: true, ..EditDraft::default() });
        self.error = None;
        self.notice = None;
    }

    /// Start editing the selected source.
    pub fn start_edit(&mut self) -> Result<(), String> {
        let Some(id) = self.selected.clone() else { return Err("请先在列表中选择一个图源".into()) };
        let Some(s) = self.library.find(&id).cloned() else { return Err(format!("图源 `{id}` 不存在")) };
        self.editing = Some(EditDraft::from_source(&s, false));
        self.error = None;
        self.notice = None;
        Ok(())
    }

    /// Save the edit form into the draft library (add or replace).
    pub fn save_edit(&mut self) -> Result<(), String> {
        let Some(draft) = self.editing.clone() else { return Err("没有正在编辑的图源".into()) };
        let source = draft.build()?;
        if draft.is_new {
            self.library.add(source.clone())?;
        } else {
            self.library.replace(&draft.id, source.clone())?;
        }
        self.selected = Some(source.id.clone());
        self.editing = None;
        self.notice = Some(format!("已保存图源“{}”", source.name));
        self.error = None;
        Ok(())
    }

    /// Delete the selected source (built-ins are refused by the library).
    pub fn delete_selected(&mut self) -> Result<(), String> {
        let Some(id) = self.selected.clone() else { return Err("请先在列表中选择一个图源".into()) };
        self.library.remove(&id)?;
        if self.editing.as_ref().is_some_and(|e| e.id == id) {
            self.editing = None;
        }
        self.selected = Some(self.library.current_id.clone());
        self.notice = Some("已删除图源".into());
        self.error = None;
        Ok(())
    }

    /// Make the selected source the current one (applied to the drawing on OK).
    pub fn set_current_selected(&mut self) -> Result<(), String> {
        let Some(id) = self.selected.clone() else { return Err("请先在列表中选择一个图源".into()) };
        self.library.set_current(&id)?;
        self.notice = Some("已设为当前图源（点“确定”后应用到图纸）".to_string());
        self.error = None;
        Ok(())
    }

    /// Import an OviMap `customMapSource` XML document into the draft library.
    /// The XML carries no attribution, so the import attribution (filled in
    /// by the user) must be non-empty.
    pub fn import(&mut self) -> Result<(), String> {
        let source = parse_ovi_custom_map_source(&self.import_xml, &self.import_attribution)?;
        self.library.add(source.clone())?;
        self.selected = Some(source.id.clone());
        self.notice = Some(format!("已导入图源“{}”", source.name));
        self.error = None;
        Ok(())
    }
}

/// Open the dialog from the menu / toolbar.
pub fn open(app: &mut CadApp) {
    let library = app.map_sources.clone();
    app.map_source_dialog = Some(MapSourceDialog::open(&library));
    app.ui.dialog = Some("mapsources".into());
}

impl CadApp {
    /// Switch the active drawing to the library's current source when the
    /// drawing has no stored tile layer (a fresh drawing follows the
    /// library current instead of silently falling back to OSM). No-op
    /// when the drawing already has a layer (its own explicit choice
    /// wins) or there is no drawing open. Returns whether the layer was
    /// switched. Runs through the `tilemap` command, so validation and
    /// the OSM fallback-restore rules match the dialog OK path; errors
    /// surface in Chinese — never a silent stay on the default.
    pub(crate) fn apply_library_current_tilemap(&mut self) -> Result<bool, String> {
        let has_layer = self.session.doc().map(|d| d.tilemap.is_some()).unwrap_or(false);
        if has_layer || self.session.doc().is_err() {
            return Ok(false);
        }
        let current = self.map_sources.current();
        self.session
            .execute(
                "tilemap",
                &json!({
                    "urlTemplate": current.url_template,
                    "attribution": current.attribution,
                    "maxZoom": current.max_zoom,
                    "coordType": current.coord_type.as_str(),
                }),
            )
            .map_err(|e| e.to_string())?;
        Ok(true)
    }
}

/// The dialog: source list + New/Edit/Delete/Set Current, the edit form,
/// the OviMap XML import, and OK/Cancel.
pub fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let Some(mut state) = app.map_source_dialog.clone() else {
        *open = false;
        return;
    };
    let mut commit = false;
    let mut cancel = false;
    egui::Window::new(crate::tl!("Map Sources…")).id(egui::Id::new("map_sources")).open(open).resizable(true).default_size(egui::vec2(680.0, 540.0)).show(ctx, |ui| {
        ui.label(crate::tl!("Tile sources for the online map. Only OpenStreetMap and CARTO are built in; add your own source only if you have the right to use it. Attribution is mandatory and is shown verbatim."));
        ui.add_space(6.0);
        // The source list: name / coordinate type / max zoom; the current
        // source carries a filled dot, the others an empty one.
        egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
            for s in state.library.sources.clone() {
                let current = s.id == state.library.current_id;
                let mark = if current { "●" } else { "○" };
                let label = format!("{mark} {} — {} · ≤{} · {}", s.name, s.coord_type.label(), s.max_zoom, s.attribution);
                let selected = state.selected.as_deref() == Some(s.id.as_str());
                if ui.selectable_label(selected, label).on_hover_text(s.url_template.clone()).clicked() {
                    state.selected = Some(s.id.clone());
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button(crate::tl!("New")).clicked() {
                state.start_new();
            }
            if ui.button(crate::tl!("Edit")).clicked()
                && let Err(e) = state.start_edit()
            {
                state.error = Some(e);
            }
            if ui.button(crate::tl!("Delete")).clicked()
                && let Err(e) = state.delete_selected()
            {
                state.error = Some(e);
            }
            if ui.button(crate::tl!("Set as Current")).clicked()
                && let Err(e) = state.set_current_selected()
            {
                state.error = Some(e);
            }
        });
        // The edit form.
        if let Some(mut draft) = state.editing.clone() {
            ui.separator();
            ui.strong(crate::tl!("Edit Source"));
            let builtin = draft.builtin;
            egui::Grid::new("mapsources_edit").num_columns(2).spacing(egui::vec2(12.0, 4.0)).show(ui, |ui| {
                ui.label(crate::tl!("ID"));
                ui.add_enabled(!builtin, egui::TextEdit::singleline(&mut draft.id).desired_width(260.0));
                ui.end_row();
                ui.label(crate::tl!("Name"));
                ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(260.0));
                ui.end_row();
                ui.label(crate::tl!("URL Template"));
                ui.add_enabled(!builtin, egui::TextEdit::singleline(&mut draft.url_template).desired_width(420.0));
                ui.end_row();
                ui.label(crate::tl!("Attribution"));
                ui.add_enabled(!builtin, egui::TextEdit::singleline(&mut draft.attribution).desired_width(420.0));
                ui.end_row();
                ui.label(crate::tl!("Max Zoom"));
                ui.add(egui::TextEdit::singleline(&mut draft.max_zoom).desired_width(80.0));
                ui.end_row();
                ui.label(crate::tl!("Coordinate Type"));
                ui.horizontal(|ui| {
                    ui.radio_value(&mut draft.coord_gcj, false, "Mercator (WGS84)");
                    ui.radio_value(&mut draft.coord_gcj, true, "Mercator (china, GCJ-02)");
                });
                ui.end_row();
                ui.label(crate::tl!("Tile Format"));
                ui.add(egui::TextEdit::singleline(&mut draft.tile_format).desired_width(80.0));
                ui.end_row();
                ui.label(crate::tl!("Tile Size"));
                ui.add(egui::TextEdit::singleline(&mut draft.tile_size).desired_width(80.0));
                ui.end_row();
            });
            if builtin {
                ui.label(crate::tl!("Built-in sources keep their URL and attribution; only the max zoom can be changed."));
            }
            ui.horizontal(|ui| {
                if ui.button(crate::tl!("Save")).clicked() {
                    state.editing = Some(draft.clone());
                    if let Err(e) = state.save_edit() {
                        state.error = Some(e);
                    }
                }
                if ui.button(crate::tl!("Cancel")).clicked() {
                    state.editing = None;
                }
            });
        }
        // OviMap XML import.
        ui.separator();
        ui.strong(crate::tl!("Import OviMap customMapSource XML"));
        ui.label(crate::tl!("Paste a customMapSource document below. The XML carries no attribution, so fill in the attribution field — an import without one is refused."));
        ui.add(egui::TextEdit::multiline(&mut state.import_xml).desired_width(620.0).desired_rows(4));
        ui.horizontal(|ui| {
            ui.label(crate::tl!("Attribution"));
            ui.add(egui::TextEdit::singleline(&mut state.import_attribution).desired_width(300.0));
            if ui.button(crate::tl!("Import")).clicked()
                && let Err(e) = state.import()
            {
                state.error = Some(e);
            }
            #[cfg(not(target_arch = "wasm32"))]
            if ui.button(crate::tl!("Load from File…")).clicked()
                && let Some(path) = app.services.pick_open.as_ref().and_then(|f| f())
            {
                match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        state.import_xml = text;
                        state.notice = Some(crate::tl!("File loaded. Fill in the attribution, then press Import.").to_string());
                        state.error = None;
                    }
                    Err(_) => state.error = Some(crate::tl!("Could not read that file.").to_string()),
                }
            }
        });
        if let Some(e) = &state.error {
            ui.add_space(4.0);
            ui.colored_label(egui::Color32::from_rgb(0xc0, 0x40, 0x40), e);
        }
        if let Some(n) = &state.notice {
            ui.add_space(4.0);
            ui.colored_label(egui::Color32::from_rgb(0x20, 0x70, 0x20), n);
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button(crate::tl!("OK")).clicked() {
                commit = true;
            }
            if ui.button(crate::tl!("Cancel")).clicked() {
                cancel = true;
            }
        });
    });
    if cancel || !*open {
        app.map_source_dialog = None;
        *open = false;
        return;
    }
    if commit {
        // Commit the library, then apply the current source to the drawing.
        let current = state.library.current();
        app.map_sources = state.library.clone();
        if app.session.doc().is_ok() {
            let r = app.run(
                "tilemap",
                json!({
                    "urlTemplate": current.url_template,
                    "attribution": current.attribution,
                    "maxZoom": current.max_zoom,
                    "coordType": current.coord_type.as_str(),
                }),
            );
            if let Err(e) = r {
                state.error = Some(e);
                app.map_source_dialog = Some(state);
                return;
            }
            app.set_status(crate::tl!("Map source applied"));
        }
        app.map_source_dialog = None;
        *open = false;
        return;
    }
    app.map_source_dialog = Some(state);
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<customMapSource><mapID>demo</mapID><name>演示图源</name><maxZoom>18</maxZoom><coordType>Mercator(china)</coordType><tileFormat>png</tileFormat><tileSize>256</tileSize><url>https://tiles.example.com/{z}/{x}/{y}.png</url></customMapSource>"#;

    fn dialog() -> MapSourceDialog {
        MapSourceDialog::open(&MapSourceLibrary::default())
    }

    #[test]
    fn dialog_add_edit_delete_and_set_current() {
        let mut d = dialog();
        // Add via the edit form.
        d.start_new();
        let mut e = d.editing.clone().expect("editing");
        e.name = "演示图源".into();
        e.url_template = "https://tiles.example.com/{z}/{x}/{y}.png".into();
        e.attribution = "演示 贡献者".into();
        e.max_zoom = "18".into();
        e.coord_gcj = true;
        d.editing = Some(e);
        d.save_edit().expect("save new");
        assert_eq!(d.library.sources.len(), 3);
        // The id is the slug of the name: a non-ASCII name slugs to "custom".
        assert_eq!(d.selected.as_deref(), Some("custom"));
        // The derived id is the slug of the name; find it and set current.
        let id = d.library.sources.iter().find(|s| s.name == "演示图源").expect("added").id.clone();
        d.selected = Some(id.clone());
        d.set_current_selected().expect("set current");
        assert_eq!(d.library.current_id, id);
        // Edit: a blank attribution is refused.
        d.start_edit().expect("start edit");
        let mut e = d.editing.clone().expect("editing");
        e.attribution = String::new();
        d.editing = Some(e);
        assert!(d.save_edit().is_err(), "attribution is mandatory");
        // Delete restores OSM as current.
        d.delete_selected().expect("delete");
        assert_eq!(d.library.current_id, "osm");
        assert_eq!(d.library.sources.len(), 2);
    }

    #[test]
    fn dialog_builtins_cannot_be_deleted_or_repointed() {
        let mut d = dialog();
        d.selected = Some("osm".into());
        assert!(d.delete_selected().is_err(), "built-ins cannot be deleted");
        d.start_edit().expect("start edit");
        let mut e = d.editing.clone().expect("editing");
        e.url_template = "https://evil.example.com/{z}/{x}/{y}.png".into();
        d.editing = Some(e);
        assert!(d.save_edit().is_err(), "a built-in's URL cannot change");
    }

    #[test]
    fn dialog_import_needs_attribution() {
        let mut d = dialog();
        d.import_xml = XML.into();
        assert!(d.import().is_err(), "no attribution → refused");
        d.import_attribution = "演示 贡献者".into();
        d.import().expect("import with attribution");
        assert_eq!(d.library.sources.len(), 3);
        assert_eq!(d.library.find("demo").expect("imported").coord_type, CoordType::Gcj02);
        // A duplicate import is refused (same mapID).
        assert!(d.import().is_err(), "duplicate id refused");
    }

    #[test]
    fn slug_derivation() {
        assert_eq!(EditDraft::slug("My Source 1"), "my-source-1");
        assert_eq!(EditDraft::slug("演示图源"), "custom");
        assert_eq!(EditDraft::slug("  "), "custom");
    }

    /// Repro: set a library source current + OK, then the map must follow
    /// the library — across the Online Map toggle and across reopening a
    /// drawing. The library current is the user's choice; the drawing's
    /// `TileLayer` is what the canvas reads, and the two must not drift
    /// apart (a fresh drawing must not silently fall back to OSM).
    #[test]
    fn library_current_survives_toggle_and_reopen() {
        use cadcraft_doc::Drawing;
        use cadcraft_doc::gis::GeoRef;
        use cadcraft_engine::{Session, tilemap::effective_tilemap};

        let mut app = CadApp::new(Session::empty(), crate::Services::default());
        let mut d = Drawing::new_metric();
        d.georef = Some(GeoRef::centred(32.1301, 114.0814));
        app.session.open_drawing(d, "Test", None);

        // Library: add a custom source and make it current.
        let source = cadcraft_doc::gis::parse_ovi_custom_map_source(XML, "演示 贡献者").expect("valid XML");
        app.map_sources.add(source).expect("add");
        app.map_sources.set_current("demo").expect("set current");

        // Dialog OK applies the library current to the drawing.
        let current = app.map_sources.current();
        let r = app.run(
            "tilemap",
            serde_json::json!({
                "urlTemplate": current.url_template,
                "attribution": current.attribution,
                "maxZoom": current.max_zoom,
                "coordType": current.coord_type.as_str(),
            }),
        );
        assert!(r.is_ok(), "dialog OK applies: {r:?}");
        assert_eq!(
            effective_tilemap(app.session.doc().expect("doc")).url_template,
            "https://tiles.example.com/{z}/{x}/{y}.png",
            "dialog OK must switch the drawing to the library current"
        );

        // Toggle off + on must preserve the source (not reset to OSM).
        app.run("tilemap", serde_json::json!({ "action": "off" })).expect("off");
        app.run("tilemap", serde_json::json!({ "action": "on" })).expect("on");
        assert_eq!(
            effective_tilemap(app.session.doc().expect("doc")).url_template,
            "https://tiles.example.com/{z}/{x}/{y}.png",
            "toggle off/on must preserve the source"
        );

        // Reopening a drawing: the library current must win over OSM.
        // `open_drawing` alone does not apply it; `opened()` (the file-open
        // funnel) calls `apply_library_current_tilemap`, simulated here.
        let mut d2 = Drawing::new_metric();
        d2.georef = Some(GeoRef::centred(32.1301, 114.0814));
        app.session.open_drawing(d2, "Test2", None);
        assert!(app.apply_library_current_tilemap().expect("apply on open"));
        assert_eq!(
            effective_tilemap(app.session.doc().expect("doc")).url_template,
            "https://tiles.example.com/{z}/{x}/{y}.png",
            "a fresh drawing must follow the library current, not the OSM default"
        );

        // Toggling the map on in the fresh drawing must use the library
        // current, not materialise OSM.
        crate::basemap::toggle(&mut app);
        assert_eq!(
            effective_tilemap(app.session.doc().expect("doc")).url_template,
            "https://tiles.example.com/{z}/{x}/{y}.png",
            "toggle-on in a fresh drawing must use the library current"
        );

        // Switching the library current back to OSM must take effect on
        // the next open too (with the CARTO fallback restored).
        app.map_sources.set_current("osm").expect("set current OSM");
        let mut d3 = Drawing::new_metric();
        d3.georef = Some(GeoRef::centred(32.1301, 114.0814));
        app.session.open_drawing(d3, "Test3", None);
        assert!(app.apply_library_current_tilemap().expect("apply OSM on open"));
        let layer = effective_tilemap(app.session.doc().expect("doc"));
        assert_eq!(layer.url_template, "https://tile.openstreetmap.org/{z}/{x}/{y}.png", "switching back to the OSM default must take effect");
        assert!(layer.fallback_url_template.is_some(), "switching back to OSM must restore the CARTO fallback");

        // A drawing with an explicit layer wins over the library current:
        // the library current is OSM here, the drawing goes custom, and
        // applying the library current must not overwrite the drawing.
        app.run(
            "tilemap",
            serde_json::json!({
                "urlTemplate": "https://tiles.example.com/{z}/{x}/{y}.png",
                "attribution": "演示 贡献者",
                "maxZoom": 19,
                "coordType": "Mercator",
            }),
        )
        .expect("dialog OK custom");
        assert!(!app.apply_library_current_tilemap().expect("apply no-op"));
        assert_eq!(
            effective_tilemap(app.session.doc().expect("doc")).url_template,
            "https://tiles.example.com/{z}/{x}/{y}.png",
            "the drawing's explicit layer must win over the library current"
        );
    }
}
