//! One-click online map: the toolbar toggle and the map-centre dialog.
//!
//! The tile layer itself lives in the engine (`TILEMAP`); this module is the
//! one-click front end. `toggle` turns the basemap off when it shows, turns
//! it on when the drawing already has a georef, and otherwise opens the
//! centre dialog: the user confirms the centre latitude/longitude (offered
//! as Xinyang, Henan — never silently applied), which is written with
//! `ovimap.georef` before the layer is switched on. A failed load never
//! raises a modal: the canvas keeps its blank placeholder and the status
//! bar carries the one-line hint.

use serde_json::json;

use crate::CadApp;

/// Default map centre offered when the drawing has no georef: Xinyang, Henan.
pub const DEFAULT_CENTER_LAT: f64 = 32.1301;
/// Default map centre longitude offered when the drawing has no georef.
pub const DEFAULT_CENTER_LON: f64 = 114.0814;

/// Draft latitude/longitude text while the centre dialog is open.
#[derive(Clone, Debug, Default)]
pub struct GeorefDraft {
    pub lat: String,
    pub lon: String,
    pub error: Option<String>,
}

/// Parse and validate latitude/longitude text: finite numbers, latitude
/// ±90, longitude ±180. `Err` is the i18n key of the message to show.
pub fn parse_lat_lon(lat_s: &str, lon_s: &str) -> Result<(f64, f64), &'static str> {
    let err = "Invalid latitude or longitude";
    let lat: f64 = lat_s.trim().parse().map_err(|_| err)?;
    let lon: f64 = lon_s.trim().parse().map_err(|_| err)?;
    if !lat.is_finite() || !(-90.0..=90.0).contains(&lat) || !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
        return Err(err);
    }
    Ok((lat, lon))
}

/// One click on the toolbar button (or the Tools ▸ OviMap ▸ Online Map
/// menu): off when the basemap shows, on when the drawing already has a
/// georef, else open the centre dialog.
pub fn toggle(app: &mut CadApp) {
    let Ok(doc) = app.session.doc() else {
        app.set_status(crate::tl!("Open or create a drawing first"));
        return;
    };
    if cadcraft_doc::gis::basemap_visible(doc) {
        if app.run("tilemap", json!({ "action": "off" })).is_ok() {
            app.set_status(crate::tl!("Online map off"));
        }
        return;
    }
    if doc.georef.is_some() {
        if app.run("tilemap", json!({ "action": "on" })).is_ok() {
            app.set_status(crate::tl!("Online map on"));
        }
        return;
    }
    app.basemap_georef = Some(GeorefDraft { lat: format!("{DEFAULT_CENTER_LAT}"), lon: format!("{DEFAULT_CENTER_LON}"), error: None });
    app.ui.dialog = Some("basemap_georef".into());
}

/// The centre dialog: confirm the latitude/longitude to align the online
/// map, or cancel. The defaults are only an offer — nothing is written
/// until the user confirms.
pub fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let Some(draft) = app.basemap_georef.clone() else {
        *open = false;
        return;
    };
    let mut lat = draft.lat.clone();
    let mut lon = draft.lon.clone();
    let error = draft.error.clone();
    let mut confirm = false;
    let mut cancel = false;
    egui::Window::new(crate::tl!("Online Map")).id(egui::Id::new("basemap_georef")).open(open).resizable(false).show(ctx, |ui| {
        ui.label(crate::tl!("The drawing has no geographic reference. Confirm the map centre below to align the online map."));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(crate::tl!("Latitude"));
            ui.add(egui::TextEdit::singleline(&mut lat).desired_width(140.0));
        });
        ui.horizontal(|ui| {
            ui.label(crate::tl!("Longitude"));
            ui.add(egui::TextEdit::singleline(&mut lon).desired_width(140.0));
        });
        if let Some(e) = &error {
            ui.add_space(4.0);
            ui.colored_label(egui::Color32::from_rgb(0xc0, 0x40, 0x40), e);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(crate::tl!("OK")).clicked() {
                confirm = true;
            }
            if ui.button(crate::tl!("Cancel")).clicked() {
                cancel = true;
            }
        });
    });
    if cancel || !*open {
        app.basemap_georef = None;
        *open = false;
        return;
    }
    if !confirm {
        // Keep the draft (and any error) for the next frame.
        app.basemap_georef = Some(GeorefDraft { lat, lon, error });
        return;
    }
    let (lat_v, lon_v) = match parse_lat_lon(&lat, &lon) {
        Ok(v) => v,
        Err(key) => {
            app.basemap_georef = Some(GeorefDraft { lat, lon, error: Some(crate::i18n::t(key).to_string()) });
            return;
        }
    };
    if let Err(e) = app.run("ovimap.georef", json!({ "originLat": lat_v, "originLon": lon_v, "centralMeridian": lon_v })) {
        app.basemap_georef = Some(GeorefDraft { lat, lon, error: Some(e) });
        return;
    }
    if let Err(e) = app.run("tilemap", json!({ "action": "on" })) {
        app.basemap_georef = Some(GeorefDraft { lat, lon, error: Some(e) });
        return;
    }
    app.basemap_georef = None;
    *open = false;
    app.set_status(crate::tl!("Online map on"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lat_lon_accepts_the_default_centre_and_rejects_hostile_text() {
        assert_eq!(parse_lat_lon("32.1301", "114.0814"), Ok((32.1301, 114.0814)));
        assert_eq!(parse_lat_lon("  32.1301  ", "  114.0814  "), Ok((32.1301, 114.0814)));
        assert!(parse_lat_lon("", "114.0814").is_err());
        assert!(parse_lat_lon("32.1301", "abc").is_err());
        assert!(parse_lat_lon("90.0001", "114.0814").is_err());
        assert!(parse_lat_lon("-90.0", "180.0").is_ok());
        assert!(parse_lat_lon("32.1301", "180.0001").is_err());
        assert!(parse_lat_lon("NaN", "114.0814").is_err());
        assert!(parse_lat_lon("inf", "114.0814").is_err());
    }

    #[test]
    fn default_centre_is_in_range() {
        assert!((-90.0..=90.0).contains(&DEFAULT_CENTER_LAT));
        assert!((-180.0..=180.0).contains(&DEFAULT_CENTER_LON));
    }
}
