//! DXF → Drawing.

use std::collections::HashMap;

use cadcraft_color::{Color, Rgb};
use cadcraft_doc::{
    Arc, AssocSnap, Attrib, AttribModes, AttribProps, Block, Circle, Common, DashElement, DimAssoc, DimKind, DimStyle, Dimension, Drawing, Ellipse,
    Entity, EntityKind, Face3d, Gradient, Group, HAlign, HVal, Handle, Hatch, HatchLoop, Helix, HelixConstrain, Insert, Layer, Layout, Leader, Line,
    Linetype, Lineweight, LwPolyline, MAX_FRAME_CHARS, MLeaderStyle, MText, MultilineAttrib, NamedView, OleFrame, OlePreviewKind, OpenProgress,
    OpenStage, PageSetup, Point, Polyline3d, RawTag, RayLine, Solid, Table, TableCell, Text, TextStyle, Tolerance, Transparency, Ucs, Unknown,
    VAlign, Viewport, VportConfig, VportTile, Wipeout,
};
use cadcraft_dxf::{Tag, records, sections};
use cadcraft_geom::{PolyVertex, Spline, Vec2, Vec3};

use crate::{IoError, Result};

mod common;
mod dimassoc;
mod document;
mod entities;
mod hatch;
mod objects;
mod tables;
mod text;

use document::{
    apply_sortents, count_skipped, dropped_paper_views, outside_sections, parse_entities, place_blocks, place_entities, read_blocks, read_objects,
    setup_layouts,
};
#[cfg(test)]
use entities::acad_table;
use entities::{entity, plot_style_names};
use objects::Objects;
use tables::{header, tables};

pub(crate) mod mtext_dxf;

/// Handle maps and references gathered while reading.
#[derive(Default)]
struct Rx {
    /// STYLE record handle (upper case) → text style name.
    styles: HashMap<String, String>,
    /// BLOCK_RECORD handle (upper case) → block name.
    brs: HashMap<String, String>,
    /// LTYPE record handle (upper case) → linetype name (DIMLTYPE, DIMLTEX1, DIMLTEX2).
    ltypes: HashMap<String, String>,
    /// Upper-case block name → (insertion units, explodable) from its BLOCK_RECORD.
    block_props: HashMap<String, (u8, bool)>,
    /// Upper-case names of anonymous `*T` blocks drawn by ACAD_TABLE entities.
    table_blocks: std::collections::HashSet<String>,
    /// Upper-case block names used by INSERTs.
    inserted: std::collections::HashSet<String>,
    /// Tables whose style is known only by TABLESTYLE handle: (table, style handle).
    table_style_fix: Vec<(Handle, String)>,
    /// Multileaders' style handles and block attribute values, resolved with the objects.
    mleader_fix: Vec<(Handle, crate::dxf_mleader::Pending)>,
    /// Upper-case xref block name → the external drawing's path (BLOCK group 1).
    xref_paths: HashMap<String, String>,
    /// Plot style name placeholder handle (upper case) → plot style name (`ACAD_PLOTSTYLENAME`).
    pstyles: HashMap<String, String>,
    /// Upper-case block name → associative array parameters from its BLOCK_RECORD.
    block_arrays: HashMap<String, cadcraft_doc::ArrayParams>,
    /// LAYER record handle (upper case) → layer name (VIEWPORT group 331/341 frozen layers).
    layers: HashMap<String, String>,
    /// Owner handle, paper flag and enclosing block (upper case) of each generated sheet viewport
    /// (PAPERVIEW) the reader dropped.
    paper_views_dropped: Vec<(Option<String>, bool, Option<String>)>,
    /// Upper-case name of the BLOCK whose entities are being read, if any.
    block: Option<String>,
}

/// Read a DXF file into a drawing.
pub fn read(bytes: &[u8]) -> Result<Drawing> {
    read_budgeted(bytes, 4_000_000)
}

/// [`read`], reporting the stage in `progress` (when given) and stopping early when it is
/// cancelled. A cancelled read returns [`IoError::Cancelled`] and no drawing.
pub fn read_with_progress(bytes: &[u8], progress: Option<&OpenProgress>) -> Result<Drawing> {
    read_budgeted_with_progress(bytes, 4_000_000, progress)
}

/// A block definition read from BLOCKS: name, base point, description, entities (owner, paper
/// flag, entity) and block flags.
type BlockDef = (String, Vec3, String, Vec<(Option<String>, bool, Entity)>, i64);

/// A layout read from OBJECTS: name, tab order, paper block handle and page setup.
/// A LAYOUT object: name, tab order, block record, page setup and its limits (groups 10/11).
type LayoutDef = (String, u32, String, PageSetup, Option<(Vec2, Vec2)>);

/// Import DXF sections while sharing one materialized-table-cell budget across blocks and entity sections.
fn read_budgeted(bytes: &[u8], table_cells_left: usize) -> Result<Drawing> {
    read_budgeted_with_progress(bytes, table_cells_left, None)
}

fn read_budgeted_with_progress(bytes: &[u8], mut table_cells_left: usize, progress: Option<&OpenProgress>) -> Result<Drawing> {
    if let Some(p) = progress {
        p.set_stage(OpenStage::DxfParse);
    }
    let tags = match progress {
        Some(p) => cadcraft_dxf::parse_with_progress(bytes, &mut |_| !p.is_cancelled()),
        None => cadcraft_dxf::parse(bytes),
    }
    .map_err(|e| match e {
        cadcraft_dxf::DxfError::Cancelled { .. } => IoError::Cancelled,
        e => IoError::Format(e.to_string()),
    })?;
    if progress.is_some_and(|p| p.is_cancelled()) {
        return Err(IoError::Cancelled);
    }
    if let Some(p) = progress {
        p.set_stage(OpenStage::Build);
    }
    let secs = sections(&tags);
    if secs.is_empty() {
        return Err(IoError::Format("no sections found".into()));
    }
    let mut d = Drawing::new_imperial();
    d.layouts.clear();
    // A file without $INSUNITS declares no units (#1209); only new drawings take the template's.
    d.header.set_i64("INSUNITS", 0);
    let mut rx = Rx { pstyles: plot_style_names(&secs), ..Rx::default() };
    let mut blocks: Vec<BlockDef> = Vec::new();
    let mut layout_objs: Vec<LayoutDef> = Vec::new();
    let mut entities = Vec::new();
    let mut objs = Objects::default();
    // Every handle another program's file already uses is taken before anything gets a new one:
    // its $HANDSEED can be lower than its handles, and entities made while reading (the faces of
    // a polygon mesh) would otherwise get handles that records further on still have. A file
    // CADCraft wrote names its own next handle, above which its rebuilt records are numbered
    // (#495), so it is left to that.
    let ours = secs.iter().any(|s| s.name == "OBJECTS" && s.tags.iter().any(|t| t.code == 3 && t.str() == crate::dxf_ext::HANDSEED_KEY));
    for t in secs.iter().filter(|_| !ours).flat_map(|s| s.tags.iter()).filter(|t| t.code == 5 || t.code == 105) {
        if let Some(h) = Handle::parse_hex(t.str().trim()) {
            d.bump_handseed(h);
        }
    }
    let mut skipped = std::collections::BTreeMap::new();
    count_skipped(&mut skipped, "(no section)", outside_sections(&tags));
    for s in &secs {
        if progress.is_some_and(|p| p.is_cancelled()) {
            return Err(IoError::Cancelled);
        }
        match s.name.as_str() {
            "HEADER" => header(&s.tags, &mut d),
            "TABLES" => tables(&s.tags, &mut d, &mut rx),
            "BLOCKS" => blocks.extend(read_blocks(&s.tags, &mut d, &mut rx, &mut table_cells_left, &mut skipped)),
            // A damaged file may split its entities over several ENTITIES sections (#865).
            "ENTITIES" => {
                let mut n = 0;
                entities.extend(parse_entities(&records(&s.tags), &mut d, &mut rx, &mut n, &mut table_cells_left));
                count_skipped(&mut skipped, "ENTITIES", n);
            }
            "OBJECTS" => read_objects(&s.tags, &mut objs, &mut layout_objs),
            _ => {}
        }
    }
    // A file CADCraft wrote names its own next handle (#495): its table records and objects
    // are numbered above it and rebuilt on save, so following $HANDSEED would drift every save.
    let own_seed = objs.xrecord_text(crate::dxf_ext::HANDSEED_KEY).and_then(|t| Handle::parse_hex(t.trim()));
    let (paper_block_for, first_layout) = setup_layouts(&mut d, &mut layout_objs, &rx);
    place_blocks(&mut d, blocks, &mut rx, &paper_block_for, &first_layout);
    place_entities(&mut d, entities, &rx, &paper_block_for, &first_layout);
    number_unknown_viewport_ids(&mut d, &dropped_paper_views(&rx, &paper_block_for, &first_layout));
    apply_sortents(&mut d, &objs.sortents, &rx.brs, &paper_block_for, &first_layout);
    objs.apply(&mut d, &rx);
    crate::dxf_image::resolve(&secs, &mut d);
    crate::dxf_mline::resolve(&secs, &mut d);
    // Keep handles unique against the header's seed.
    let seed = d.header.str("HANDSEED", "");
    if let Some(h) = own_seed.or_else(|| Handle::parse_hex(&seed)) {
        d.bump_handseed(Handle(h.0.saturating_sub(1)));
    }
    if d.layer("0").is_none() {
        d.layers.insert(0, Layer::default());
    }
    crate::dxf_raw::read(&secs, &mut d);
    crate::dxf_clip::read(&mut d);
    d.skipped_records = skipped;
    apply_georef(&secs, &mut d);
    fill_ole_frames(&mut d);
    ole_note(&mut d);
    if let Some(p) = progress {
        p.set_stage(OpenStage::Done);
    }
    Ok(d)
}

/// OviMap link (§4.3): the `OVIMAP_GEOREF` dictionary (an entry of any dictionary; normally
/// the root named object dictionary) holds the GEOREF XRECORD and one VERTICES_<lineGroupId>
/// XRECORD per track. The upstream reader's object table keeps dictionaries flat, so the
/// nested dictionaries and XRECORDs are re-collected here from the OBJECTS section. A
/// malformed entry is ignored with a read note, never a failed open.
fn apply_georef(secs: &[cadcraft_dxf::Section], d: &mut Drawing) {
    let mut xrecords: HashMap<String, Vec<Tag>> = HashMap::new();
    let mut dict_entries: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for s in secs.iter().filter(|s| s.name == "OBJECTS") {
        for (kind, tags) in records(&s.tags) {
            match kind.as_str() {
                "DICTIONARY" => {
                    let h = tags.iter().find(|t| t.code == 5).map(|t| t.str().trim().to_ascii_uppercase()).unwrap_or_default();
                    let mut name: Option<String> = None;
                    let mut entries: Vec<(String, String)> = Vec::new();
                    for tg in &tags {
                        match tg.code {
                            3 => name = Some(tg.str()),
                            350 | 360 => {
                                if let Some(n) = name.take() {
                                    entries.push((n, tg.str().trim().to_ascii_uppercase()));
                                }
                            }
                            _ => {}
                        }
                    }
                    if !h.is_empty() && !entries.is_empty() {
                        dict_entries.entry(h).or_default().extend(entries);
                    }
                }
                "XRECORD" => {
                    let h = tags.iter().find(|t| t.code == 5).map(|t| t.str().trim().to_ascii_uppercase()).unwrap_or_default();
                    if !h.is_empty() {
                        xrecords.insert(h, tags);
                    }
                }
                _ => {}
            }
        }
    }
    for entries in dict_entries.values() {
        let Some((_, dict_handle)) = entries.iter().find(|(k, _)| k == crate::dxf_ext::OVIMAP_GEOREF_KEY) else { continue };
        let Some(sub) = dict_entries.get(&dict_handle.to_ascii_uppercase()) else { continue };
        for (name, handle) in sub {
            let Some(tags) = xrecords.get(&handle.to_ascii_uppercase()) else { continue };
            if name == crate::dxf_ext::OVIMAP_GEOREF_ENTRY {
                match crate::dxf_ext::parse_georef_tags(tags) {
                    Some(g) => d.georef = Some(g),
                    None => d.read_notes.push("OVIMAP_GEOREF 字典中的 GEOREF XRECORD 无法解析（投影名/k0/半径/单位不符），已忽略".into()),
                }
            } else if let Some(track_id) = name.strip_prefix(crate::dxf_ext::OVIMAP_VERTICES_PREFIX) {
                match crate::dxf_ext::parse_vertices_tags(tags) {
                    Some(ids) => {
                        let e = d.ovimap_ext.entry(track_id.to_string()).or_default();
                        e.vertices = ids.iter().map(|v| serde_json::json!({ "ovimap_id": v })).collect();
                    }
                    None => d.read_notes.push(format!("轨迹 `{track_id}` 的 VERTICES XRECORD 损坏（1070 计数与 1000 数量不符），顶点 ID 已丢失")),
                }
            }
        }
    }
}

/// Parse an OLE2FRAME's frame from its kept raw groups (DXF Reference: 10/20/30 upper-left
/// corner, 11/21/31 lower-right corner, 3 source application, 71 object type). `None` when
/// the corners are missing, non-finite or degenerate. The 310 binary payload is decoded
/// (capped) only to classify the cached preview; the picture bytes are not kept.
fn ole_frame_from_raw(tags: &[RawTag]) -> Option<OleFrame> {
    let get = |code: i32| tags.iter().find(|t| t.code == code).map(|t| t.value.as_str());
    let num = |code: i32| get(code).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
    let upper_left = Vec3::new(num(10)?, num(20)?, num(30).unwrap_or(0.0));
    let lower_right = Vec3::new(num(11)?, num(21)?, num(31).unwrap_or(0.0));
    let frame = OleFrame {
        upper_left,
        lower_right,
        source_app: get(3).unwrap_or_default().chars().take(256).collect(),
        object_type: get(71).and_then(|v| v.trim().parse::<i64>().ok()).and_then(|v| i16::try_from(v.clamp(-32768, 32767)).ok()).unwrap_or(2),
        preview: ole_preview_from_raw(tags),
    };
    frame.is_visible().then_some(frame)
}

/// Classify the cached preview in an OLE2FRAME's 310 binary groups: hex-decode them (each
/// chunk is at most 127 bytes) and hand the bytes to the DWG bridge's classifier. The
/// decode stops and reports no preview when the payload is malformed or over the
/// classifier's cap — hostile input must not make opening a file allocate without bound.
fn ole_preview_from_raw(tags: &[RawTag]) -> Option<OlePreviewKind> {
    let mut bytes: Vec<u8> = Vec::new();
    for tg in tags.iter().filter(|x| x.code == 310) {
        let mut hi: Option<u8> = None;
        for b in tg.value.bytes() {
            let n = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                b' ' | b'\t' | b'\r' | b'\n' => continue,
                _ => return None,
            };
            match hi.take() {
                None => hi = Some(n),
                Some(h) => {
                    if bytes.len() >= cadcraft_dwg::MAX_OLE_PREVIEW_BYTES {
                        return None;
                    }
                    bytes.push((h << 4) | n);
                }
            }
        }
        if hi.is_some() {
            return None;
        }
    }
    if bytes.is_empty() {
        return None;
    }
    // The DWG bridge's kind is its own type (it cannot depend on the doc crate);
    // map it onto the doc's kind.
    match cadcraft_dwg::ole_preview_kind(&bytes) {
        Some(cadcraft_dwg::OlePreviewKind::Raster) => Some(OlePreviewKind::Raster),
        Some(cadcraft_dwg::OlePreviewKind::Dib) => Some(OlePreviewKind::Dib),
        Some(cadcraft_dwg::OlePreviewKind::Emf) => Some(OlePreviewKind::Emf),
        Some(cadcraft_dwg::OlePreviewKind::Wmf) => Some(OlePreviewKind::Wmf),
        None => None,
    }
}

/// Fill the parsed OLE frame of every OLE2FRAME the reader kept as an unmodelled record,
/// so it draws, selects and reports extents instead of staying invisible.
fn fill_ole_frames(d: &mut Drawing) {
    fn fill(store: &mut cadcraft_doc::EntityStore) {
        let handles: Vec<Handle> = store
            .iter()
            .filter(|e| matches!(&e.kind, EntityKind::Unknown(u) if u.is_ole_frame() && u.ole.is_none()))
            .map(|e| e.handle)
            .collect();
        for h in handles {
            store.modify(h, |e| {
                if let EntityKind::Unknown(u) = &mut e.kind {
                    u.ole = ole_frame_from_raw(&u.tags);
                }
            });
        }
    }
    fill(&mut d.model);
    for l in d.layouts.iter_mut() {
        fill(&mut l.entities);
    }
    for b in d.blocks.values_mut() {
        fill(&mut std::sync::Arc::make_mut(b).entities);
    }
}

/// The OLE objects in a drawing, by DXF type: entities CADCraft keeps only as unmodelled
/// records. Their embedded payload (the 310 binary groups) is carried in the record, not
/// interpreted, and the DWG writer cannot write them back.
fn ole_count(d: &Drawing) -> usize {
    let in_store = |ents: &cadcraft_doc::EntityStore| {
        ents.iter().filter(|e| matches!(&e.kind, EntityKind::Unknown(u) if u.dxf_type.eq_ignore_ascii_case("OLE2FRAME"))).count()
    };
    in_store(&d.model)
        + d.layouts.iter().map(|l| in_store(&l.entities)).sum::<usize>()
        + d.blocks.values().map(|b| in_store(&b.entities)).sum::<usize>()
}

/// Tell the reader, in the drawing's own notes, that OLE objects are present and what saving
/// does to them — never silently. The note goes on every read (DXF or DWG) that contains one.
fn ole_note(d: &mut Drawing) {
    let n = ole_count(d);
    if n == 0 {
        return;
    }
    d.read_notes.push(format!(
        "本文件含 {n} 个 OLE 对象（嵌入内容），打开时显示为占位框（嵌入内容本身未加载，不可就地编辑）；另存为 DWG 时这些对象将丢失，另存为 DXF 可保留。"
    ));
}

/// Viewport ids another program left unknown (ZWCAD writes 69 = -1, read as 0). A layout whose
/// viewports all lack an id gets them numbered in file order, as DWG readers do; when only
/// some lack one and none is 1, the first viewport is the sheet and gets id 1. Either way the
/// sheet keeps id 1, so saving does not add a second sheet viewport ahead of it (#1463). A layout
/// whose generated sheet (PAPERVIEW) the reader just dropped is left alone: none of its viewports
/// is the sheet, and numbering one as 1 would lose the generated sheet on the next save.
fn number_unknown_viewport_ids(d: &mut Drawing, sheet_dropped: &std::collections::HashSet<String>) {
    for l in d.layouts.iter_mut().filter(|l| !sheet_dropped.contains(&l.name)) {
        let mut vps: Vec<Entity> = l.entities.iter().filter(|e| matches!(&e.kind, EntityKind::Viewport(_))).map(|e| (**e).clone()).collect();
        let id = |e: &Entity| if let EntityKind::Viewport(v) = &e.kind { v.id } else { 0 };
        if vps.iter().all(|e| id(e) == 0) {
            for (i, e) in vps.iter_mut().enumerate() {
                if let EntityKind::Viewport(v) = &mut e.kind {
                    v.id = i as u32 + 1;
                }
            }
        } else if vps.iter().all(|e| id(e) != 1) && vps.first().is_some_and(|e| id(e) == 0) {
            if let Some(EntityKind::Viewport(v)) = vps.first_mut().map(|e| &mut e.kind) {
                v.id = 1;
            }
        } else {
            continue;
        }
        for e in vps {
            l.entities.replace(e);
        }
    }
}

/// An entity from the groups of a DXF record of type `kind` (without its `0` group), as the
/// reader makes it, with plain strings as [`crate::entity_groups`] gives them (not the file's
/// `^ ` escapes). AutoLISP's `entmake` and `entmod` are built on it.
pub fn entity_from_groups(kind: &str, tags: &[Tag]) -> Option<(Common, EntityKind)> {
    // AutoLISP's strings are plain: escape them as the file has them (see `entity_groups`).
    let mut tags = tags.to_vec();
    mtext_dxf::map_text_groups(kind, &mut tags, mtext_dxf::encode_line);
    entity(&kind.to_ascii_uppercase(), &tags)
}

#[cfg(test)]
mod table_budget_tests {
    use super::*;
    /// Build synthetic table tags with a requested row and column count for allocation-budget checks.
    fn table_tags(rows: i64, cols: i64) -> Vec<Tag> {
        vec![Tag::s(100, "AcDbTable"), Tag::i(91, rows), Tag::i(92, cols), Tag::i(171, 1), Tag::s(1, "cell")]
    }
    /// A table that cannot fit allocates no cell grid, while a smaller table spends only its cell count.
    #[test]
    fn exhausted_budget_rejects_before_materializing_cells() {
        let mut left = 3;
        let table = acad_table(&table_tags(2, 2), &mut left);
        assert!(table.cells.is_empty() && table.row_heights.is_empty() && table.col_widths.is_empty());
        assert_eq!(left, 3);
        let table = acad_table(&table_tags(1, 2), &mut left);
        assert_eq!(table.cells[0].len(), 2);
        assert_eq!(left, 1);
    }
    /// Tables in blocks and repeated entity sections consume one document-wide cell allowance.
    #[test]
    fn blocks_and_entity_sections_share_one_budget() {
        let table = "0\nACAD_TABLE\n100\nAcDbTable\n91\n1\n92\n2\n171\n1\n1\ncell\n";
        let dxf = format!(
            "0\nSECTION\n2\nBLOCKS\n0\nBLOCK\n2\nTEST\n10\n0\n20\n0\n{table}0\nENDBLK\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n{table}0\nENDSEC\n0\nSECTION\n2\nENTITIES\n{table}0\nENDSEC\n0\nEOF\n"
        );
        let d = read_budgeted(dxf.as_bytes(), 2).unwrap();
        let block = d.block("TEST").unwrap();
        let count = |e: &Entity| match &e.kind {
            EntityKind::Table(t) => t.cells.iter().map(Vec::len).sum::<usize>(),
            _ => 0,
        };
        assert_eq!(block.entities.iter().map(|e| count(e)).sum::<usize>(), 2);
        assert_eq!(d.model.iter().map(|e| count(e)).sum::<usize>(), 0);
    }
}
