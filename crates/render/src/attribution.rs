//! The basemap attribution line appended to plots (`docs/ovimap-link.md` §9.2:
//! "导出/打印含底图时：图面角落带同样署名").
//!
//! Plots do not rasterise the tile quads, so the attribution is a vector
//! TEXT entity built into its own display list and merged into the plot's
//! list by the caller ([`crate::append_list`]). It uses the Latin-only
//! drafting font, so the text is the ASCII form
//! ([`cadcraft_doc::gis::PLOT_ATTRIBUTION`]); the full Chinese form lives in
//! the UI panels. The text sits on layer `0` (no link to the drawing's
//! layers) and is drawn in black on the white sheet.

use cadcraft_doc::{Common, Drawing, EntityKind, HAlign, Space, Text, VAlign};
use cadcraft_geom::{Vec2, Vec3};

use crate::{DisplayList, Options};

/// A display list holding one TEXT entity — `text` with its baseline at `at`
/// and cap height `height` (caller's coordinates) — ready to merge into a plot
/// list. Empty (no primitives) for empty text or an unusable size/position.
pub fn attribution_text_list(text: &str, at: Vec2, height: f64, opts: &Options) -> DisplayList {
    if text.is_empty() || !(height.is_finite() && height > 0.0) || !at.is_finite() {
        return DisplayList::default();
    }
    let mut d = Drawing::new_metric();
    let t = Text {
        insert: Vec3::new(at.x, at.y, 0.0),
        align_pt: None,
        height,
        value: text.to_string(),
        rotation: 0.0,
        width_factor: 1.0,
        oblique: 0.0,
        style: "Standard".to_string(),
        halign: HAlign::Left,
        valign: VAlign::Baseline,
    };
    if d.add(&Space::Model, Common::default(), EntityKind::Text(t)).is_err() {
        return DisplayList::default();
    }
    crate::build(&d, &Space::Model, opts)
}
