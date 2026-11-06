//! Drawing → DXF (AC1015 / R2000 structure, readable by every DXF consumer since 2000). Later
//! versions keep that structure and differ in `$ACADVER`, from 2004 in CLASS instance counts and
//! the newer DIMSTYLE groups, and from 2007 in UTF-8 text (the caller picks the text encoding) and
//! MULTILEADER entities (older files get LEADER + MTEXT).

use std::collections::HashMap;

use cadcraft_color::Color;
use cadcraft_doc::{
    AssocSnap, Attrib, Block, Common, DimAssoc, DimKind, DimStyle, Dimension, Drawing, Entity, EntityKind, EntityStore, Gradient, Group, HAlign,
    HVal, Handle, Hatch, Insert, Layout, Leader, Line, Lineweight, LwPolyline, MLeader, MLeaderStyle, MText, PageSetup, Point, Solid, Space, Table,
    TableStyle, Text, Transparency, VAlign, VportConfig, VportTile,
};
use cadcraft_dxf::{Tag, Version};
use cadcraft_geom::{Bounds2, Vec2, Vec3};

use crate::dxf_ext::{self, DimVal, K};

mod assoc;
mod blocks;
mod common;
mod ctx;
mod entity;
mod header;
mod layout;
mod objects;
mod sections;

#[cfg(test)]
pub(crate) use assoc::polyline_vertex_ref;
pub(crate) use blocks::is_generated_arrow;
#[cfg(test)]
pub(crate) use blocks::table_block_entities;
use ctx::Ctx;
use entity::entity;
pub(crate) use header::code_page;
use sections::tags_for;

struct W {
    t: Vec<Tag>,
    next: u64,
    /// Upper-case plot style name → its `ACDBPLACEHOLDER` handle (group 390 of layers and objects).
    pstyles: HashMap<String, String>,
    /// Upper-case layer name → its LAYER table record handle (group 331/341 of viewports).
    layers: HashMap<String, String>,
    /// Persistent reactors of source geometry: the associative hatches and DIMASSOC objects
    /// that follow it (groups `{ACAD_REACTORS 330 …}`).
    reactors: HashMap<Handle, Vec<String>>,
}

impl W {
    fn s(&mut self, c: i32, v: impl Into<String>) {
        self.t.push(Tag::s(c, v));
    }
    fn f(&mut self, c: i32, v: f64) {
        self.t.push(Tag::f(c, v));
    }
    fn i(&mut self, c: i32, v: i64) {
        self.t.push(Tag::i(c, v));
    }
    fn p(&mut self, c: i32, p: Vec3) {
        self.f(c, p.x);
        self.f(c + 10, p.y);
        self.f(c + 20, p.z);
    }
    fn p2(&mut self, c: i32, p: Vec2) {
        self.f(c, p.x);
        self.f(c + 10, p.y);
    }
    fn h(&mut self) -> String {
        format!("{:X}", self.next_handle())
    }
    /// A new handle for a record the drawing doesn't hold (as [`W::h`]).
    fn next_handle(&mut self) -> u64 {
        let h = self.next;
        self.next = self.next.saturating_add(1);
        h
    }
    /// A `102 {NAME` … `102 }` group of handles.
    fn group(&mut self, name: &str, code: i32, handles: &[&str]) {
        self.s(102, format!("{{{name}"));
        for h in handles {
            self.s(code, *h);
        }
        self.s(102, "}");
    }
    /// A dimension variable as a group (DIMSTYLE records) or as an xdata value (DSTYLE).
    fn dimval(&mut self, code: i32, v: DimVal, xdata: bool) {
        match v {
            DimVal::Real(x) => self.f(if xdata { 1040 } else { code }, x),
            DimVal::Int(i) => self.i(if xdata { 1070 } else { code }, i),
            DimVal::Str(t) => {
                if xdata || !t.is_empty() {
                    self.s(if xdata { 1000 } else { code }, t);
                }
            }
            DimVal::Handle(h) => {
                if xdata {
                    self.s(1005, h);
                } else if h != "0" {
                    self.s(code, h);
                }
            }
        }
    }
    fn xdata(&mut self, tags: Vec<Tag>) {
        self.t.extend(tags);
    }
}

/// Write a drawing as ASCII DXF.
/// The drawing as R2000 ASCII DXF.
pub fn write(d: &Drawing) -> String {
    cadcraft_dxf::write_ascii(&tags(d, Version::R2000))
}

/// The drawing's DXF tags, declaring `version` in `$ACADVER`.
pub fn tags(d: &Drawing, version: Version) -> Vec<Tag> {
    let mut tags = tags_for(d, version, version, version < Version::R2004);
    append_georef(&mut tags, d);
    tags
}

/// The R2000 DXF the DWG bridge converts to a `dwg` version DWG: its entity types are the DWG
/// version's (MULTILEADER from 2007).
pub(crate) fn dwg_bridge(d: &Drawing, dwg: Version) -> String {
    let mut tags = tags_for(d, Version::R2000, dwg, false);
    append_georef(&mut tags, d);
    bridge_markers(&mut tags);
    cadcraft_dxf::write_ascii(&tags)
}

/// OviMap link (§4.3): the `OVIMAP_GEOREF` dictionary with its GEOREF XRECORD and one
/// VERTICES_<lineGroupId> XRECORD per track, appended to the OBJECTS section the writer
/// produced. The split writer has no hook for extra root-dictionary objects, so the
/// records are added here on the finished tags: the entry goes into the root dictionary
/// (the first DICTIONARY of OBJECTS, owner `0`), the new records just before its ENDSEC,
/// with handles above every handle the writer used. A VERTICES entry whose key is illegal
/// for a DXF dictionary is skipped: the link export path (`ovimap.export`) validates
/// first and fails instead, and silently renaming the key would break the key ↔ track
/// link (R2). A vertex without an id drops its whole entry (a short entry would make the
/// file's VERTICES count disagree and get the file rejected on import; a missing entry
/// degrades to regenerated ids + a warning, §4.6).
fn append_georef(tags: &mut Vec<Tag>, d: &Drawing) {
    // Collect the VERTICES entries first: nothing is written unless there is a georef
    // or at least one writable track.
    let mut vertices: Vec<(String, Vec<String>)> = Vec::new();
    for (id, e) in &d.ovimap_ext {
        if e.vertices.is_empty() {
            continue;
        }
        let key = format!("{}{}", dxf_ext::OVIMAP_VERTICES_PREFIX, id);
        if !dxf_ext::valid_dict_key(&key) {
            continue;
        }
        let mut ids = Vec::new();
        let mut ok = true;
        for v in &e.vertices {
            match v.get("ovimap_id").and_then(serde_json::Value::as_str) {
                Some(s) if !s.is_empty() => ids.push(s.to_string()),
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            vertices.push((key, ids));
        }
    }
    if d.georef.is_none() && vertices.is_empty() {
        return;
    }
    // The OBJECTS section and its root dictionary (first DICTIONARY record, owner `0`).
    let Some(sec) = tags.windows(2).position(|w| w[0].code == 0 && w[0].str() == "SECTION" && w[1].code == 2 && w[1].str() == "OBJECTS") else {
        return;
    };
    let Some(dict_at) = tags.iter().skip(sec).position(|t| t.code == 0 && t.str() == "DICTIONARY").map(|i| sec + i) else { return };
    let dict_end = tags.iter().skip(dict_at + 1).position(|t| t.code == 0).map_or(tags.len(), |i| dict_at + 1 + i);
    let root = tags.get(dict_at..dict_end).and_then(|rec| rec.iter().find(|t| t.code == 5).map(|t| t.str().to_string())).unwrap_or_default();
    if root.is_empty() {
        return;
    }
    // Handles above every handle in the file (handles are hex in groups 5 and 105).
    let mut next = tags
        .iter()
        .filter(|t| t.code == 5 || t.code == 105)
        .filter_map(|t| u64::from_str_radix(t.str().trim(), 16).ok())
        .max()
        .unwrap_or(0x100);
    let mut alloc = move || {
        next = next.saturating_add(1);
        format!("{next:X}")
    };
    let dict_handle = alloc();
    let georef_handle = d.georef.as_ref().map(|_| alloc());
    let vertex_handles: Vec<String> = vertices.iter().map(|_| alloc()).collect();
    // The root dictionary entry, at the end of its record.
    tags.splice(
        dict_end..dict_end,
        [Tag::s(3, dxf_ext::OVIMAP_GEOREF_KEY), Tag::s(350, dict_handle.clone())],
    );
    // The new records, just before the OBJECTS ENDSEC (which the splice just shifted by 2).
    let Some(endsec) = tags.iter().skip(sec).position(|t| t.code == 0 && t.str() == "ENDSEC").map(|i| sec + i) else { return };
    let mut recs: Vec<Tag> = Vec::new();
    recs.push(Tag::s(0, "DICTIONARY"));
    recs.push(Tag::s(5, dict_handle.clone()));
    recs.push(Tag::s(330, root));
    recs.push(Tag::s(100, "AcDbDictionary"));
    recs.push(Tag::i(280, 1));
    recs.push(Tag::i(281, 1));
    if let Some(h) = &georef_handle {
        recs.push(Tag::s(3, dxf_ext::OVIMAP_GEOREF_ENTRY));
        recs.push(Tag::s(350, h.clone()));
    }
    for ((key, _), h) in vertices.iter().zip(&vertex_handles) {
        recs.push(Tag::s(3, key.clone()));
        recs.push(Tag::s(350, h.clone()));
    }
    if let (Some(h), Some(g)) = (&georef_handle, &d.georef) {
        recs.push(Tag::s(0, "XRECORD"));
        recs.push(Tag::s(5, h.clone()));
        recs.push(Tag::s(102, "{ACAD_REACTORS"));
        recs.push(Tag::s(330, dict_handle.clone()));
        recs.push(Tag::s(102, "}"));
        recs.push(Tag::s(330, dict_handle.clone()));
        recs.push(Tag::s(100, "AcDbXrecord"));
        recs.push(Tag::i(280, 1));
        recs.extend(dxf_ext::georef_tags(g));
    }
    for ((_, ids), h) in vertices.iter().zip(&vertex_handles) {
        recs.push(Tag::s(0, "XRECORD"));
        recs.push(Tag::s(5, h.clone()));
        recs.push(Tag::s(102, "{ACAD_REACTORS"));
        recs.push(Tag::s(330, dict_handle.clone()));
        recs.push(Tag::s(102, "}"));
        recs.push(Tag::s(330, dict_handle.clone()));
        recs.push(Tag::s(100, "AcDbXrecord"));
        recs.push(Tag::i(280, 1));
        recs.extend(dxf_ext::vertices_tags(ids));
    }
    tags.splice(endsec..endsec, recs);
}

/// Subclass markers the DWG bridge knows by another spelling (it matches them exactly):
/// AutoCAD writes a dynamic block lookup parameter's as `AcDbBlockLookUpParameter`, the bridge
/// reads `AcDbBlockLookupParameter`; otherwise the parameter loses its name and its lookup
/// action in the DWG, and AutoCAD's lookups stop working.
fn bridge_markers(tags: &mut [Tag]) {
    for t in tags.iter_mut().filter(|t| t.code == 100) {
        if t.str().trim().eq_ignore_ascii_case("AcDbBlockLookupParameter") {
            *t = Tag::s(100, "AcDbBlockLookupParameter");
        }
    }
}

/// One entity's DXF groups, as the writer stores them, without its `0` group: its type name
/// and the groups of its record (an entity written as several records, a POLYLINE and its
/// VERTEXes, gives its first). Handles of objects outside the entity (owner, styles) are left
/// out. AutoLISP's `entget` is built on it.
pub fn entity_groups(d: &Drawing, e: &Entity) -> (String, Vec<Tag>) {
    let mut w = W {
        t: Vec::new(),
        next: d.handseed.max(0x100).saturating_add(0x1000),
        pstyles: HashMap::new(),
        layers: HashMap::new(),
        reactors: HashMap::new(),
    };
    let cx = Ctx { target: Version::R2018, legacy_columns: true, ..Ctx::default() };
    let paper = d.space_of(e.handle).is_some_and(|s| !matches!(s, cadcraft_doc::Space::Model));
    entity(&mut w, d, e, "0", paper, &cx);
    let mut tags = w.t.into_iter();
    let kind = tags.next().filter(|t| t.code == 0).map(|t| t.value.as_str()).unwrap_or_default();
    let mut tags: Vec<Tag> = tags.take_while(|t| t.code != 0).filter(|t| t.code != 330).collect();
    // Strings as AutoLISP sees them: `10^2`, not the file's `10^ 2`.
    crate::dxf_read::mtext_dxf::map_text_groups(&kind, &mut tags, crate::dxf_read::mtext_dxf::decode_line);
    (kind, tags)
}
