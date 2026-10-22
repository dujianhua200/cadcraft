//! EMF (Enhanced Metafile) preview playback into display-list vector geometry.
//!
//! An OLE2FRAME's cached preview is an EMF record stream (reassembled from the Office
//! "WMFC" comment chunks by [`cadcraft_dwg::ole_emf_bytes`]). EMF is natively vector, so
//! it is played back into the same vector primitives the rest of the drawing uses
//! (polylines, filled triangles, shaped text) instead of rasterising it: the preview then
//! shows on the GPU path, the CPU rasteriser and plots with no blit/texture channel, and
//! the display list stays vector-only.
//!
//! What is implemented (MS-EMF record semantics):
//! - DC state: map modes (MM_TEXT, MM_ANISOTROPIC, MM_ISOTROPIC and the fixed
//!   LOMETRIC/HIMETRIC/LOENGLISH/HIENGLISH/TWIPS modes via the header DPI), window and
//!   viewport origin/extent (including SCALEWINDOW/VIEWPORTEXTEX), world transform,
//!   SaveDC/RestoreDC, text/bk colour, bk mode, poly fill mode, text alignment, arc
//!   direction, and a single active clip rectangle (intersect semantics).
//! - Objects: CREATEPEN / EXTCREATEPEN / CREATEBRUSHINDIRECT / EXTCREATEFONTINDIRECTW /
//!   DELETEOBJECT / SELECTOBJECT, including the stock objects (NULL_PEN, NULL_BRUSH,
//!   WHITE/BLACK pen/brush, DC_PEN/DC_BRUSH) and the stock fonts (drawn with the
//!   fallback face at a fixed height).
//! - Drawing: POLYLINE/POLYGON/POLYBEZIER (+TO, 16-bit and POLYPOLY variants, POLYDRAW),
//!   MOVETOEX/LINETO, RECTANGLE/ELLIPSE/ROUNDRECT/ARC/ARCTO/CHORD/PIE/ANGLEARC, path
//!   brackets (BeginPath/EndPath/CloseFigure/FillPath/StrokePath/StrokeAndFillPath,
//!   FlattenPath is a no-op because curves are flattened as recorded),
//!   FILLRGN/FRAMERGN/PAINTRGN (region bounds), SETPIXELV (a 1-device-pixel fill),
//!   EXTTEXTOUTA/W (with ETO_OPAQUE background and per-character Dx advances).
//! - Text is shaped with the resolved font ([`cadcraft_fonts::place`] without Dx, per
//!   character with Dx), the escapement rotation applied in device space after the
//!   logical-to-device map, as LOGFONT specifies it on the device x-axis.
//!
//! Deliberate approximations, all documented here so a reader knows where fidelity ends:
//! - Raster records (BITBLT, STRETCHBLT, MASKBLT, PLGBLT, SETDIBITSTODEVICE,
//!   STRETCHDIBITS, ALPHABLEND, TRANSPARENTBLT, GRADIENTFILL) are skipped: the display
//!   list has no raster-blit channel, and a metafile that is only blits yields no
//!   geometry, so its frame falls back to the placeholder. The same goes for EMF+
//!   (GDI+ records inside an EMR_COMMENT) and for POLYTEXTOUT / SMALLTEXTOUT.
//! - Dash/dot pen styles draw solid; hatched brushes draw solid; WINDING fill mode draws
//!   even-odd; EXCLUDECLIPRECT / RGN_XOR / RGN_DIFF / RGN_OR clips are ignored (only
//!   RGN_AND / RGN_COPY through a rectangle are honoured); WidenPath is a no-op; text is
//!   not clipped to its ETO_CLIPPED rectangle; underline / strikeout / weight are ignored;
//!   `lfWidth` is ignored (the resolved face's own advance is used); SETPIXELV aside, a
//!   flood fill (EXTFLOODFILL) does nothing.
//! - EXTTEXTOUTA strings are decoded Latin-1 (no code-page table in this crate);
//!   EXTTEXTOUTW strings are UTF-16 and carry CJK correctly.
//! - The preview is stretched to fill the OLE frame (what OLE containers do), even when
//!   the frame's aspect differs from the metafile's: the frame is the object's size.
//! - A 64-bit content hash keys the parse cache; two different EMFs colliding in
//!   (length, hash) would show each other's preview. At 157 previews the chance is
//!   negligible, and a collision only mis-renders a preview, never corrupts a drawing.
//!
//! Hostile input: every read is bounds-checked, record and point counts are capped, and
//! a malformed stream ends playback (already-emitted geometry is kept) instead of
//! failing the whole display-list build.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Mutex;

use cadcraft_color::Rgb;
use cadcraft_doc::{OleFrame, RawTag};
use cadcraft_fonts::{Align, TextFont, TextParams, VAlign};
use cadcraft_geom::{Bounds2, Mat3, Vec2};

use super::{Builder, Ctx, Ink, clip, fill};

/// Largest OLE payload played at all (mirrors the DWG bridge's cap).
const MAX_EMF_BYTES: usize = 4 << 20;
/// Largest record count walked in one EMF (hostile streams).
const MAX_RECORDS: usize = 200_000;
/// Largest point count emitted by one EMF, across all records.
const MAX_TOTAL_POINTS: usize = 1_000_000;
/// Largest point count in one path bracket.
const MAX_PATH_POINTS: usize = 200_000;
/// Largest SaveDC nesting (hostile streams).
const MAX_SAVED_DC: usize = 64;
/// Largest character count in one text record.
const MAX_TEXT_CHARS: usize = 100_000;
/// Parsed-EMF cache entries (one per distinct preview; the user's drawing has 157).
const MAX_CACHE_ENTRIES: usize = 32;

/// One parsed EMF, in EMF device coordinates (y down): the frame mapping turns it into
/// drawing coordinates at emit time.
struct ParsedEmf {
    /// The header's `rclBounds` (device units), for the frame mapping.
    bounds: Bounds2,
    /// The union of everything actually drawn (device units): a metafile whose
    /// header bounds carry empty margins (a whole sheet around a small table)
    /// maps by its ink instead, so the content fills its OLE frame.
    ink: Bounds2,
    /// Strokes: colour, width in device units, polyline points.
    strokes: Vec<EmfStroke>,
    /// Fills: colour, triangle vertices.
    fills: Vec<(Rgb, Vec<Vec2>)>,
    /// Text: colour, shaped glyphs in device coordinates.
    texts: Vec<(Rgb, cadcraft_fonts::Shaped)>,
}

struct EmfStroke {
    rgb: Rgb,
    width: f64,
    points: Vec<Vec2>,
}

impl ParsedEmf {
    /// Only the tests need this (`draw_ole_emf` applies the `opts.text` gate instead).
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.strokes.is_empty() && self.fills.is_empty() && self.texts.is_empty()
    }
}

/// Parsed previews, keyed by (hex length, hash of the 310 hex strings): re-parsing 157
/// previews on every display-list rebuild (every 2x zoom step) would jank the canvas.
static CACHE: std::sync::LazyLock<Mutex<HashMap<(usize, u64), std::sync::Arc<ParsedEmf>>>> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Draw an OLE2FRAME's EMF preview into the display list, mapped onto the frame.
/// Returns true when preview geometry was emitted (the caller then skips the
/// placeholder label). Never fails the build: any malformed input falls back to the
/// placeholder. Text runs follow the same `opts.text` gate as TEXT/MTEXT: with text
/// off, a text-only preview falls back to the frame (no white background either, so
/// the frame's rectangle and diagonals stay visible as the OLE indicator).
pub(crate) fn draw_ole_emf(b: &mut Builder, ctx: &Ctx, tags: &[RawTag], frame: &OleFrame) -> bool {
    let parsed = match cached_parse(tags) {
        Some(p) => p,
        None => return false,
    };
    let draw_text = b.opts.text && parsed.texts.iter().any(|(_, sh)| !sh.is_empty());
    if parsed.strokes.is_empty() && parsed.fills.is_empty() && !draw_text {
        return false;
    }
    let c = frame.corners();
    // Frame: c[0] upper-left, c[1] upper-right, c[2] lower-right. EMF device y points
    // down, drawing y points up, so the frame mapping flips y.
    let (fx0, fx1) = (c[0].x, c[1].x);
    let (fy0, fy1) = (c[2].y, c[0].y);
    let bounds = frame_bounds(&parsed);
    let bw = bounds.width();
    let bh = bounds.height();
    if !(bw > 0.0 && bh > 0.0 && bw.is_finite() && bh.is_finite()) {
        return false;
    }
    let (fw, fh) = (fx1 - fx0, fy1 - fy0);
    if !(fw > 0.0 && fh > 0.0) {
        return false;
    }
    let sx = fw / bw;
    let sy = fh / bh;
    // Line widths scale with the mapping (largest axis, as `max_scale` does elsewhere).
    let wscale = sx.abs().max(sy.abs());
    let map = |p: Vec2| Vec2::new(fx0 + (p.x - bounds.min.x) * sx, fy1 - (p.y - bounds.min.y) * sy);
    let ink = |rgb: Rgb| Ink { rgb, aci7: false, aci: 0 };
    // The preview is opaque: paint the frame white first so the frame's own rectangle
    // and diagonals (drawn before this call) do not show through thin geometry.
    let white = Rgb(255, 255, 255);
    let bg = [Vec2::new(fx0, fy0), Vec2::new(fx1, fy0), Vec2::new(fx1, fy1), Vec2::new(fx0, fy0), Vec2::new(fx1, fy1), Vec2::new(fx0, fy1)];
    b.tris(ctx, ink(white), &bg);
    // The background alone counts as drawing: the preview covers the frame.
    let mut drew = true;
    for s in &parsed.strokes {
        if s.points.len() < 2 {
            continue;
        }
        let mapped: Vec<Vec2> = s.points.iter().map(|&p| map(p)).collect();
        let lw = (s.width * wscale).clamp(0.0, 1e6);
        b.polyline(ctx, ink(s.rgb), lw as f32, &mapped);
        drew = true;
    }
    for (rgb, tris) in &parsed.fills {
        if tris.len() < 3 {
            continue;
        }
        let mapped: Vec<Vec2> = tris.iter().map(|&p| map(p)).collect();
        b.tris(ctx, ink(*rgb), &mapped);
        drew = true;
    }
    for (rgb, sh) in &parsed.texts {
        if !b.opts.text || sh.is_empty() {
            continue;
        }
        let mut sh = sh.clone();
        sh.map(map);
        b.shaped(ctx, ink(*rgb), 0.0, &sh);
        drew = true;
    }
    drew
}

/// The bounds a preview maps onto its frame by: the ink bounds when the ink
/// sits inside the header bounds and covers only a small part of them — the
/// header bounds of an Excel preview cover the whole sheet, not the table, and
/// mapping by them shrinks the content into a corner of its frame. A preview
/// whose ink roughly fills its header keeps the header bounds (its size
/// relationship to the drawing around it is the author's).
fn frame_bounds(p: &ParsedEmf) -> Bounds2 {
    let header = p.bounds;
    let ink = p.ink;
    let slack = header.width().max(header.height()) * 1e-9 + 1e-9;
    if !ink.is_empty() && header.expand(slack).contains_box(&ink) && ink.width() < header.width() * 0.6 && ink.height() < header.height() * 0.6 {
        ink
    } else {
        header
    }
}

/// Parse (or fetch from cache) the EMF in an entity's 310 groups.
fn cached_parse(tags: &[RawTag]) -> Option<std::sync::Arc<ParsedEmf>> {
    let mut len = 0usize;
    let mut hasher = DefaultHasher::new();
    let mut count = 0usize;
    for t in tags.iter().filter(|t| t.code == 310) {
        len += t.value.len();
        t.value.hash(&mut hasher);
        count += 1;
        if len > 2 * MAX_EMF_BYTES {
            return None;
        }
    }
    if count == 0 || len == 0 {
        return None;
    }
    let key = (len, hasher.finish());
    if let Ok(cache) = CACHE.lock()
        && let Some(hit) = cache.get(&key)
    {
        return Some(std::sync::Arc::clone(hit));
    }
    let mut bytes: Vec<u8> = Vec::new();
    for t in tags.iter().filter(|t| t.code == 310) {
        if !hex_decode_into(&t.value, &mut bytes) {
            return None;
        }
        if bytes.len() > MAX_EMF_BYTES {
            return None;
        }
    }
    if bytes.is_empty() {
        return None;
    }
    let emf = cadcraft_dwg::ole_emf_bytes(&bytes)?;
    let parsed = std::sync::Arc::new(parse_emf(&emf));
    if let Ok(mut cache) = CACHE.lock() {
        if cache.len() >= MAX_CACHE_ENTRIES {
            cache.clear();
        }
        cache.insert(key, std::sync::Arc::clone(&parsed));
    }
    Some(parsed)
}

/// Hex-decode `s` (whitespace skipped) into `out`; false on any other character or an
/// odd nibble count. Mirrors the DXF reader's 310 decode.
fn hex_decode_into(s: &str, out: &mut Vec<u8>) -> bool {
    let mut hi: Option<u8> = None;
    for b in s.bytes() {
        let n = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return false,
        };
        match hi.take() {
            None => hi = Some(n),
            Some(h) => out.push((h << 4) | n),
        }
    }
    hi.is_none()
}

/// Parse an EMF record stream. Never panics; a malformed record ends the walk and the
/// geometry so far is kept.
fn parse_emf(emf: &[u8]) -> ParsedEmf {
    let mut out = Out::default();
    if emf.len() < 88 || rd_u32(emf, 0) != Some(1) {
        return out.parsed(Bounds2::EMPTY);
    }
    let bounds = rd_rect(emf, 8).unwrap_or(Bounds2::EMPTY);
    // Header DPI for the fixed map modes: szlDevice (pixels) over szlMillimeters (mm).
    let dpi = header_dpi(emf);
    let mut dc = Dc::default();
    dc.refresh(dpi);
    let mut pos = 0usize;
    let mut records = 0usize;
    while pos + 8 <= emf.len() && records < MAX_RECORDS {
        records += 1;
        let (Some(ty), Some(size)) = (rd_u32(emf, pos), rd_u32(emf, pos + 4)) else { break };
        let size = size as usize;
        if size < 8 || pos + size > emf.len() {
            break;
        }
        let rec = &emf[pos..pos + size];
        if ty == 14 {
            break; // EMR_EOF
        }
        if out.exhausted {
            break;
        }
        handle_record(rec, ty, &mut dc, &mut out, dpi);
        pos += size;
    }
    out.parsed(bounds)
}

/// Pixels per inch from the header (szlDevice / szlMillimeters), 96 when degenerate.
fn header_dpi(emf: &[u8]) -> (f64, f64) {
    let dpi = |dev: i32, mm: i32| {
        if dev > 0 && mm > 0 {
            let d = f64::from(dev) * 25.4 / f64::from(mm);
            if d.is_finite() && (1.0..=10_000.0).contains(&d) { d } else { 96.0 }
        } else {
            96.0
        }
    };
    // ENHMETAHEADER: szlDevice at 72 (cx, cy), szlMillimeters at 80 (cx, cy).
    let dx = dpi(rd_i32(emf, 72).unwrap_or(0), rd_i32(emf, 80).unwrap_or(0));
    let dy = dpi(rd_i32(emf, 76).unwrap_or(0), rd_i32(emf, 84).unwrap_or(0));
    (dx, dy)
}

// ---- bounds-checked readers (all offsets relative to a record or the header) ----

fn rd_u32(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn rd_i32(b: &[u8], off: usize) -> Option<i32> {
    rd_u32(b, off).map(|v| v as i32)
}

fn rd_f32(b: &[u8], off: usize) -> Option<f32> {
    rd_u32(b, off).map(f32::from_bits)
}

fn rd_i16(b: &[u8], off: usize) -> Option<i16> {
    b.get(off..off + 2).map(|s| i16::from_le_bytes([s[0], s[1]]))
}

fn rd_u16(b: &[u8], off: usize) -> Option<u16> {
    rd_i16(b, off).map(|v| v as u16)
}

fn rd_pointl(b: &[u8], off: usize) -> Option<Vec2> {
    let (x, y) = (rd_i32(b, off)?, rd_i32(b, off + 4)?);
    Some(Vec2::new(f64::from(x), f64::from(y)))
}

fn rd_points(b: &[u8], off: usize) -> Option<Vec2> {
    let (x, y) = (rd_i16(b, off)?, rd_i16(b, off + 2)?);
    Some(Vec2::new(f64::from(x), f64::from(y)))
}

fn rd_rect(b: &[u8], off: usize) -> Option<Bounds2> {
    let (l, t, r, bot) = (rd_i32(b, off)?, rd_i32(b, off + 4)?, rd_i32(b, off + 8)?, rd_i32(b, off + 12)?);
    let (x0, x1) = (f64::from(l.min(r)), f64::from(l.max(r)));
    let (y0, y1) = (f64::from(t.min(bot)), f64::from(t.max(bot)));
    if (x0.is_finite() && x1.is_finite() && y0.is_finite() && y1.is_finite()) && x1 > x0 && y1 > y0 {
        Some(Bounds2::new(Vec2::new(x0, y0), Vec2::new(x1, y1)))
    } else {
        None
    }
}

/// A COLORREF (0x00BBGGRR) to RGB.
fn rd_color(v: u32) -> Rgb {
    Rgb((v & 0xff) as u8, ((v >> 8) & 0xff) as u8, ((v >> 16) & 0xff) as u8)
}

/// A GDI object in the handle table (palettes carry no drawing state and are not stored).
enum Obj {
    Pen(Pen),
    Brush(Brush),
    Font(Font),
}

#[derive(Clone)]
struct Pen {
    /// PS_* style (5 = PS_NULL: no stroke).
    style: u32,
    /// Width in logical units (0 = cosmetic, one device pixel).
    width: f64,
    color: Rgb,
}

#[derive(Clone)]
struct Brush {
    /// BS_* style (1 = BS_NULL: no fill; anything but 0 draws solid).
    style: u32,
    color: Rgb,
}

#[derive(Clone)]
struct Font {
    face: String,
    /// Character height in logical units (|lfHeight|).
    height: f64,
    /// Escapement in radians (lfEscapement, tenths of a degree, device space).
    escapement: f64,
    italic: bool,
}

/// One figure of the current path bracket (points in logical coordinates).
struct PathFig {
    points: Vec<Vec2>,
    closed: bool,
}

/// Playback state: the DC plus the object table, the path bracket and the output.
struct Dc {
    map_mode: u32,
    win_org: Vec2,
    win_ext: Vec2,
    vp_org: Vec2,
    vp_ext: Vec2,
    world: Mat3,
    /// Logical-to-device map, refreshed when any of the above changes.
    xf: Mat3,
    pen: Option<Pen>,
    brush: Option<Brush>,
    font: Option<Font>,
    text_color: Rgb,
    bk_color: Rgb,
    /// 1 = TRANSPARENT, 2 = OPAQUE (GDI default OPAQUE).
    bk_mode: u32,
    /// 1 = ALTERNATE, 2 = WINDING (drawn even-odd either way).
    poly_fill_mode: u32,
    text_align: u32,
    /// 1 = counterclockwise (GDI default), 2 = clockwise.
    arc_direction: u32,
    /// Active clip rectangle, in device coordinates (`None` = unclipped).
    clip: Option<Bounds2>,
    /// Current position, in logical coordinates.
    pos: Vec2,
    /// Colours for the DC_PEN / DC_BRUSH stock objects.
    dc_pen: Rgb,
    dc_brush: Rgb,
    objects: HashMap<u32, Obj>,
    saved: Vec<DcState>,
    path: Option<Vec<PathFig>>,
}

/// The part of the DC that SaveDC / RestoreDC stacks (everything but the object table
/// and the path bracket, which GDI does not save).
#[derive(Clone)]
struct DcState {
    map_mode: u32,
    win_org: Vec2,
    win_ext: Vec2,
    vp_org: Vec2,
    vp_ext: Vec2,
    world: Mat3,
    pen: Option<Pen>,
    brush: Option<Brush>,
    font: Option<Font>,
    text_color: Rgb,
    bk_color: Rgb,
    bk_mode: u32,
    poly_fill_mode: u32,
    text_align: u32,
    arc_direction: u32,
    clip: Option<Bounds2>,
    pos: Vec2,
    dc_pen: Rgb,
    dc_brush: Rgb,
}

impl Default for Dc {
    fn default() -> Self {
        Dc {
            map_mode: 1, // MM_TEXT
            win_org: Vec2::ZERO,
            win_ext: Vec2::new(1.0, 1.0),
            vp_org: Vec2::ZERO,
            vp_ext: Vec2::new(1.0, 1.0),
            world: Mat3::IDENTITY,
            xf: Mat3::IDENTITY,
            pen: None,
            brush: None,
            font: None,
            text_color: Rgb(0, 0, 0),
            bk_color: Rgb(255, 255, 255),
            bk_mode: 2,
            poly_fill_mode: 1,
            text_align: 0,
            arc_direction: 1,
            clip: None,
            pos: Vec2::ZERO,
            dc_pen: Rgb(0, 0, 0),
            dc_brush: Rgb(255, 255, 255),
            objects: HashMap::new(),
            saved: Vec::new(),
            path: None,
        }
    }
}

impl Dc {
    fn snapshot(&self) -> DcState {
        DcState {
            map_mode: self.map_mode,
            win_org: self.win_org,
            win_ext: self.win_ext,
            vp_org: self.vp_org,
            vp_ext: self.vp_ext,
            world: self.world,
            pen: self.pen.clone(),
            brush: self.brush.clone(),
            font: self.font.clone(),
            text_color: self.text_color,
            bk_color: self.bk_color,
            bk_mode: self.bk_mode,
            poly_fill_mode: self.poly_fill_mode,
            text_align: self.text_align,
            arc_direction: self.arc_direction,
            clip: self.clip,
            pos: self.pos,
            dc_pen: self.dc_pen,
            dc_brush: self.dc_brush,
        }
    }

    fn restore(&mut self, s: DcState) {
        self.map_mode = s.map_mode;
        self.win_org = s.win_org;
        self.win_ext = s.win_ext;
        self.vp_org = s.vp_org;
        self.vp_ext = s.vp_ext;
        self.world = s.world;
        self.pen = s.pen;
        self.brush = s.brush;
        self.font = s.font;
        self.text_color = s.text_color;
        self.bk_color = s.bk_color;
        self.bk_mode = s.bk_mode;
        self.poly_fill_mode = s.poly_fill_mode;
        self.text_align = s.text_align;
        self.arc_direction = s.arc_direction;
        self.clip = s.clip;
        self.pos = s.pos;
        self.dc_pen = s.dc_pen;
        self.dc_brush = s.dc_brush;
    }

    /// Rebuild the logical-to-device map: device = vp_org + (world * logical - win_org) * s.
    /// MM_TEXT scales 1:1; MM_ANISOTROPIC uses vp_ext / win_ext; MM_ISOTROPIC the same
    /// with the smaller axis scale on both axes; the fixed modes use the header DPI.
    fn refresh(&mut self, dpi: (f64, f64)) {
        let (mut sx, mut sy) = match self.map_mode {
            8 => {
                // MM_ANISOTROPIC
                let (sx, sy) = (self.vp_ext.x / self.win_ext.x, self.vp_ext.y / self.win_ext.y);
                (sx, sy)
            }
            7 => {
                // MM_ISOTROPIC: equal scales, the smaller axis wins.
                let (sx, sy) = (self.vp_ext.x / self.win_ext.x, self.vp_ext.y / self.win_ext.y);
                let m = sx.abs().min(sy.abs());
                (sx.signum() * m, sy.signum() * m)
            }
            2 => (dpi.0 / 254.0, -dpi.1 / 254.0),   // MM_LOMETRIC: 0.1 mm, y up
            3 => (dpi.0 / 2540.0, -dpi.1 / 2540.0), // MM_HIMETRIC: 0.01 mm, y up
            4 => (dpi.0 / 100.0, -dpi.1 / 100.0),   // MM_LOENGLISH: 0.01 inch, y up
            5 => (dpi.0 / 1000.0, -dpi.1 / 1000.0), // MM_HIENGLISH: 0.001 inch, y up
            6 => (dpi.0 / 1440.0, -dpi.1 / 1440.0), // MM_TWIPS: 1/1440 inch, y up
            _ => (1.0, 1.0),                        // MM_TEXT and unknown modes
        };
        if !sx.is_finite() || sx == 0.0 {
            sx = 1.0;
        }
        if !sy.is_finite() || sy == 0.0 {
            sy = 1.0;
        }
        self.xf = Mat3::translate(self.vp_org).then_before(Mat3::scale(sx, sy)).then_before(Mat3::translate(-self.win_org)).then_before(self.world);
    }

    /// The largest singular value of the map's linear part: a logical length measures at
    /// most this many device units after the map (used for pen widths).
    fn scale(&self) -> f64 {
        let (a, b, c, d) = (self.xf.a, self.xf.b, self.xf.c, self.xf.d);
        let e = (a * a + b * b + c * c + d * d) / 2.0;
        let f = (((a * a + b * b - c * c - d * d) / 2.0).powi(2) + (a * c + b * d).powi(2)).sqrt();
        let s = (e + f).max(0.0).sqrt();
        if s.is_finite() && s > 0.0 { s } else { 1.0 }
    }

    fn map(&self, p: Vec2) -> Vec2 {
        self.xf.apply(p)
    }
}

/// Accumulated geometry plus the spend budget.
#[derive(Default)]
struct Out {
    strokes: Vec<EmfStroke>,
    fills: Vec<(Rgb, Vec<Vec2>)>,
    texts: Vec<(Rgb, cadcraft_fonts::Shaped)>,
    points: usize,
    exhausted: bool,
}

impl Out {
    fn parsed(self, bounds: Bounds2) -> ParsedEmf {
        let mut ink = Bounds2::EMPTY;
        for s in &self.strokes {
            for p in &s.points {
                ink.add(*p);
            }
        }
        for (_, pts) in &self.fills {
            for p in pts {
                ink.add(*p);
            }
        }
        for (_, sh) in &self.texts {
            for g in &sh.glyphs {
                for c in g {
                    for p in c {
                        ink.add(*p);
                    }
                }
            }
            for s in &sh.strokes {
                for p in s {
                    ink.add(*p);
                }
            }
        }
        ParsedEmf { bounds, ink, strokes: self.strokes, fills: self.fills, texts: self.texts }
    }

    /// Charge `n` points to the budget; false once it is spent.
    fn charge(&mut self, n: usize) -> bool {
        if self.exhausted {
            return false;
        }
        self.points = self.points.saturating_add(n);
        if self.points > MAX_TOTAL_POINTS {
            self.exhausted = true;
            return false;
        }
        true
    }
}

/// Intersect two rectangles (either may be empty).
fn intersect_bounds(a: Bounds2, b: Bounds2) -> Bounds2 {
    if a.is_empty() || b.is_empty() {
        return Bounds2::EMPTY;
    }
    let min = Vec2::new(a.min.x.max(b.min.x), a.min.y.max(b.min.y));
    let max = Vec2::new(a.max.x.min(b.max.x), a.max.y.min(b.max.y));
    if min.x > max.x || min.y > max.y { Bounds2::EMPTY } else { Bounds2::new(min, max) }
}

/// Clip a device-space loop to the active clip rectangle (none when unclipped).
fn clip_loop(dc: &Dc, loop_: &[Vec2]) -> Vec<Vec2> {
    match dc.clip {
        Some(r) => clip::clip_polygon(loop_, &r),
        None => loop_.to_vec(),
    }
}

/// Stroke a logical polyline with the current pen: map, clip, emit. `closed` appends
/// the first point so a closed figure strokes shut.
fn emit_stroke(dc: &Dc, out: &mut Out, pts: &[Vec2], closed: bool) {
    if let Some(pen) = dc.pen.clone() {
        emit_stroke_with(dc, out, pts, closed, &pen);
    }
}

/// Fill logical loops with `color` (the current brush unless overridden): map, clip,
/// triangulate even-odd.
fn emit_fill(dc: &Dc, out: &mut Out, loops: &[Vec<Vec2>], color: Option<Rgb>) {
    let rgb = match color {
        Some(c) => c,
        None => match &dc.brush {
            Some(b) if b.style == 0 => b.color, // BS_SOLID only
            _ => return,
        },
    };
    let mut clipped: Vec<Vec<Vec2>> = Vec::new();
    for l in loops {
        if l.len() < 3 || !out.charge(l.len()) {
            return;
        }
        let mapped: Vec<Vec2> = l.iter().map(|&p| dc.map(p)).collect();
        let c = clip_loop(dc, &mapped);
        if c.len() >= 3 {
            clipped.push(c);
        }
    }
    if clipped.is_empty() {
        return;
    }
    let tris = fill::triangulate_evenodd(&clipped);
    if tris.len() >= 3 {
        out.fills.push((rgb, tris));
    }
}

/// Fill one device-space loop (used for SETPIXELV, which is already in device units).
fn emit_device_loop(dc: &Dc, out: &mut Out, loop_: &[Vec2], rgb: Rgb) {
    if loop_.len() < 3 || !out.charge(loop_.len()) {
        return;
    }
    let c = clip_loop(dc, loop_);
    if c.len() < 3 {
        return;
    }
    let tris = fill::triangulate_evenodd(&[c]);
    if tris.len() >= 3 {
        out.fills.push((rgb, tris));
    }
}

/// Fill + outline a rectangle given by logical corners (RECTANGLE / ETO_OPAQUE).
fn emit_rect(dc: &Dc, out: &mut Out, rect: Bounds2, fill: bool, stroke: bool) {
    let corners = vec![rect.min, Vec2::new(rect.max.x, rect.min.y), rect.max, Vec2::new(rect.min.x, rect.max.y)];
    if fill {
        emit_fill(dc, out, std::slice::from_ref(&corners), None);
    }
    if stroke {
        emit_stroke(dc, out, &corners, true);
    }
}

/// Flatten one cubic Bezier into `out` (adaptive, explicit stack, depth-capped).
fn flatten_cubic(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, tol: f64, out: &mut Vec<Vec2>) {
    // (p0, p1, p2, p3, depth); emit p3 when flat enough.
    let mut stack: Vec<(Vec2, Vec2, Vec2, Vec2, u32)> = vec![(p0, p1, p2, p3, 0)];
    while let Some((a, b, c, d, depth)) = stack.pop() {
        // Flatness: the control points' distance from the chord, over-estimated by the
        // deviation of the chord midpoint from the curve midpoint (1/8 rule).
        let mid = (a + d) * 0.5;
        let cmid = (b + c) * 0.125 + (a + d) * 0.375;
        let flat = (cmid - mid).len() <= tol || depth >= 10;
        if flat {
            if !out.last().is_some_and(|l| l.near(d, 1e-12)) {
                out.push(d);
            }
        } else {
            let ab = (a + b) * 0.5;
            let bc = (b + c) * 0.5;
            let cd = (c + d) * 0.5;
            let abc = (ab + bc) * 0.5;
            let bcd = (bc + cd) * 0.5;
            let m = (abc + bcd) * 0.5;
            stack.push((m, bcd, cd, d, depth + 1));
            stack.push((a, ab, abc, m, depth + 1));
        }
    }
}

/// Tessellate an elliptical arc in logical coordinates: centre, radii, start/end angles
/// in the (x right, y down) parametrisation, where the angle *decreases* for a
/// counterclockwise-on-screen sweep. Returns the polyline (without a closing point).
fn arc_points(center: Vec2, rx: f64, ry: f64, start: f64, sweep: f64, tol: f64) -> Vec<Vec2> {
    let r = rx.max(ry);
    if !(r > 0.0 && r.is_finite() && sweep.is_finite() && sweep.abs() > 1e-12) {
        return Vec::new();
    }
    // About one segment per `tol` of arc length, bounded.
    let n = ((sweep.abs() * r / tol).ceil() as usize).clamp(8, 720);
    let mut pts = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let t = start + sweep * (i as f64 / n as f64);
        pts.push(Vec2::new(center.x + rx * t.cos(), center.y + ry * t.sin()));
    }
    pts
}

/// Angles (in the arc parametrisation above) of the start/end points of an ellipse arc.
/// `ccw` selects the GDI default direction (counterclockwise on screen = decreasing
/// angle, so a CCW sweep is negative). Returns (start_angle, sweep) with |sweep| == 2π
/// for a full ellipse.
fn arc_span(center: Vec2, start: Vec2, end: Vec2, ccw: bool) -> (f64, f64) {
    let a0 = (start.y - center.y).atan2(start.x - center.x);
    let a1 = (end.y - center.y).atan2(end.x - center.x);
    let tau = std::f64::consts::TAU;
    let sweep = if ccw {
        let mut s = (a0 - a1).rem_euclid(tau);
        if s < 1e-12 {
            s = tau;
        }
        -s
    } else {
        let mut s = (a1 - a0).rem_euclid(tau);
        if s < 1e-12 {
            s = tau;
        }
        s
    };
    (a0, sweep)
}

/// Rotate `p` around `center` by `rad` counterclockwise *on screen* (y down): a
/// 90° rotation takes 3 o'clock to 12 o'clock. Used for LOGFONT escapement, which is
/// specified on the device x-axis.
fn rotate_screen_ccw(p: Vec2, center: Vec2, rad: f64) -> Vec2 {
    let d = p - center;
    let (s, c) = rad.sin_cos();
    center + Vec2::new(d.x * c + d.y * s, -d.x * s + d.y * c)
}

/// Read `count` PointL values (8 bytes each) at `off`; `None` when they do not fit.
fn read_pointls(rec: &[u8], off: usize, count: usize) -> Option<Vec<Vec2>> {
    if count > rec.len().saturating_sub(off) / 8 {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        out.push(rd_pointl(rec, off + i * 8)?);
    }
    Some(out)
}

/// Read `count` POINTS values (4 bytes each) at `off`; `None` when they do not fit.
fn read_pointss(rec: &[u8], off: usize, count: usize) -> Option<Vec<Vec2>> {
    if count > rec.len().saturating_sub(off) / 4 {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        out.push(rd_points(rec, off + i * 4)?);
    }
    Some(out)
}

/// The current path's figures, creating the bracket when it does not exist (used by
/// the POLYDRAW-outside-a-bracket path, which plays into a temporary bracket).
fn path_figs(dc: &mut Dc) -> &mut Vec<PathFig> {
    dc.path.get_or_insert_with(Vec::new)
}

/// Total points in the current path bracket.
fn path_len(dc: &Dc) -> usize {
    dc.path.as_ref().map(|p| p.iter().map(|f| f.points.len()).sum()).unwrap_or(0)
}

/// Start a new path figure at `p` (a moveto).
fn path_moveto(dc: &mut Dc, p: Vec2) {
    if path_len(dc) >= MAX_PATH_POINTS {
        dc.pos = p;
        return;
    }
    path_figs(dc).push(PathFig { points: vec![p], closed: false });
    dc.pos = p;
}

/// Append a line point to the current figure, starting a figure at the current
/// position when there is none or the last one is closed.
fn path_lineto(dc: &mut Dc, p: Vec2) {
    if path_len(dc) >= MAX_PATH_POINTS {
        dc.pos = p;
        return;
    }
    let pos = dc.pos;
    let figs = path_figs(dc);
    let start_new = match figs.last() {
        None => true,
        Some(f) => f.closed,
    };
    if start_new {
        figs.push(PathFig { points: vec![pos, p], closed: false });
    } else if let Some(f) = figs.last_mut() {
        f.points.push(p);
    }
    dc.pos = p;
}

/// Append a flattened cubic (from `p0`) to the current figure.
fn path_bezier(dc: &mut Dc, p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2) {
    if path_len(dc) >= MAX_PATH_POINTS {
        dc.pos = p3;
        return;
    }
    let tol = 0.5 / dc.scale();
    let mut flat = vec![p0];
    flatten_cubic(p0, p1, p2, p3, tol, &mut flat);
    let figs = path_figs(dc);
    let start_new = match figs.last() {
        None => true,
        Some(f) => f.closed,
    };
    if start_new {
        figs.push(PathFig { points: flat, closed: false });
    } else if let Some(f) = figs.last_mut() {
        // The flattened curve starts at the figure's last point: skip the duplicate.
        if flat.len() > 1 {
            f.points.extend_from_slice(&flat[1..]);
        }
    }
    dc.pos = p3;
}

/// Close the current figure: the current position becomes the figure's first point.
fn path_close(dc: &mut Dc) {
    let mut start = None;
    if let Some(figs) = dc.path.as_mut()
        && let Some(f) = figs.last_mut()
        && !f.closed
        && f.points.len() >= 2
    {
        start = Some(f.points[0]);
        f.closed = true;
    }
    if let Some(s) = start {
        dc.pos = s;
    }
}

/// Emit the current path bracket as fills and/or strokes (the bracket itself stays).
fn emit_path(dc: &Dc, out: &mut Out, do_fill: bool, do_stroke: bool) {
    let Some(figs) = dc.path.as_ref() else { return };
    if do_fill {
        let loops: Vec<Vec<Vec2>> = figs.iter().map(|f| f.points.clone()).collect();
        emit_fill(dc, out, &loops, None);
    }
    if do_stroke {
        for f in figs {
            emit_stroke(dc, out, &f.points, f.closed);
        }
    }
}

/// Stroke `pts` with an explicit pen (FRAMERGN), bypassing the current pen.
fn emit_stroke_with(dc: &Dc, out: &mut Out, pts: &[Vec2], closed: bool, pen: &Pen) {
    if pen.style == 5 || pts.len() < 2 || !out.charge(pts.len()) {
        return;
    }
    let mut mapped: Vec<Vec2> = pts.iter().map(|&p| dc.map(p)).collect();
    if closed
        && let (Some(&first), Some(&last)) = (mapped.first(), mapped.last())
        && !first.near(last, 1e-12)
    {
        mapped.push(first);
    }
    let width = if pen.width > 0.0 { pen.width * dc.scale() } else { 1.0 };
    let pieces = match dc.clip {
        Some(r) => clip::clip_polyline(&mapped, &r),
        None => vec![mapped],
    };
    for piece in pieces {
        if piece.len() >= 2 {
            out.strokes.push(EmfStroke { rgb: pen.color, width, points: piece });
        }
    }
}

/// A rounded-rectangle loop in logical coordinates (corner radii clamped to the rect).
fn round_rect_loop(rect: Bounds2, corner: Vec2) -> Vec<Vec2> {
    let (mut rx, mut ry) = (corner.x.abs() / 2.0, corner.y.abs() / 2.0);
    rx = rx.min(rect.width() / 2.0);
    ry = ry.min(rect.height() / 2.0);
    if !(rx > 0.0 && ry > 0.0) {
        return vec![rect.min, Vec2::new(rect.max.x, rect.min.y), rect.max, Vec2::new(rect.min.x, rect.max.y)];
    }
    let (x0, x1, y0, y1) = (rect.min.x, rect.max.x, rect.min.y, rect.max.y);
    // Quarter-ellipse corners, angle increasing (clockwise on screen in the y-down
    // parametrisation), from the incoming edge to the outgoing edge.
    let corner_arc = |cx: f64, cy: f64, a0: f64| {
        let mut pts = Vec::new();
        let n = 8usize;
        for i in 1..=n {
            let t = a0 + (std::f64::consts::FRAC_PI_2) * (i as f64 / n as f64);
            pts.push(Vec2::new(cx + rx * t.cos(), cy + ry * t.sin()));
        }
        pts
    };
    let mut pts = Vec::new();
    // Start at the top edge, after the top-right corner, going clockwise on screen
    // (increasing angle in the y-down parametrisation).
    pts.push(Vec2::new(x0 + rx, y0));
    pts.extend(corner_arc(x1 - rx, y0 + ry, -std::f64::consts::FRAC_PI_2));
    pts.push(Vec2::new(x1, y1 - ry));
    pts.extend(corner_arc(x1 - rx, y1 - ry, 0.0));
    pts.push(Vec2::new(x0 + rx, y1));
    pts.extend(corner_arc(x0 + rx, y1 - ry, std::f64::consts::FRAC_PI_2));
    pts.push(Vec2::new(x0, y0 + ry));
    pts.extend(corner_arc(x0 + rx, y0 + ry, std::f64::consts::PI));
    pts
}

/// Dispatch one record. Unknown and raster records are skipped by falling through.
#[allow(clippy::too_many_lines)]
fn handle_record(rec: &[u8], ty: u32, dc: &mut Dc, out: &mut Out, dpi: (f64, f64)) {
    match ty {
        // ---- state ----
        9 => {
            if let Some(e) = rd_pointl(rec, 8) {
                dc.win_ext = e;
                dc.refresh(dpi);
            }
        }
        10 => {
            if let Some(o) = rd_pointl(rec, 8) {
                dc.win_org = o;
                dc.refresh(dpi);
            }
        }
        11 => {
            if let Some(e) = rd_pointl(rec, 8) {
                dc.vp_ext = e;
                dc.refresh(dpi);
            }
        }
        12 => {
            if let Some(o) = rd_pointl(rec, 8) {
                dc.vp_org = o;
                dc.refresh(dpi);
            }
        }
        31 => {
            // SCALEWVIEWPORTEXTEX: xNum, xDenom, yNum, yDenom.
            if let (Some(xn), Some(xd), Some(yn), Some(yd)) = (rd_i32(rec, 8), rd_i32(rec, 12), rd_i32(rec, 16), rd_i32(rec, 20))
                && xd != 0
                && yd != 0
            {
                dc.vp_ext = Vec2::new(dc.vp_ext.x * f64::from(xn) / f64::from(xd), dc.vp_ext.y * f64::from(yn) / f64::from(yd));
                dc.refresh(dpi);
            }
        }
        32 => {
            // SCALEWINDOWEXTEX
            if let (Some(xn), Some(xd), Some(yn), Some(yd)) = (rd_i32(rec, 8), rd_i32(rec, 12), rd_i32(rec, 16), rd_i32(rec, 20))
                && xd != 0
                && yd != 0
            {
                dc.win_ext = Vec2::new(dc.win_ext.x * f64::from(xn) / f64::from(xd), dc.win_ext.y * f64::from(yn) / f64::from(yd));
                dc.refresh(dpi);
            }
        }
        17 => {
            if let Some(m) = rd_u32(rec, 8) {
                dc.map_mode = m;
                dc.refresh(dpi);
            }
        }
        18 => {
            if let Some(m) = rd_u32(rec, 8) {
                dc.bk_mode = m;
            }
        }
        19 => {
            if let Some(m) = rd_u32(rec, 8) {
                dc.poly_fill_mode = m;
            }
        }
        22 => {
            if let Some(m) = rd_u32(rec, 8) {
                dc.text_align = m;
            }
        }
        24 => {
            if let Some(c) = rd_u32(rec, 8) {
                dc.text_color = rd_color(c);
            }
        }
        25 => {
            if let Some(c) = rd_u32(rec, 8) {
                dc.bk_color = rd_color(c);
            }
        }
        57 => {
            if let Some(m) = rd_u32(rec, 8) {
                dc.arc_direction = m;
            }
        }
        13 | 16 | 20 | 21 | 23 | 58 => {} // brush org, mapper flags, ROP2, stretch mode, color adjust, miter limit
        33 => {
            if dc.saved.len() < MAX_SAVED_DC {
                dc.saved.push(dc.snapshot());
            }
        }
        34 => {
            if let Some(rel) = rd_i32(rec, 8) {
                if rel < 0 {
                    let n = rel.unsigned_abs() as usize;
                    let mut restored = None;
                    for _ in 0..n {
                        match dc.saved.pop() {
                            Some(s) => restored = Some(s),
                            None => break,
                        }
                    }
                    if let Some(s) = restored {
                        dc.restore(s);
                        dc.refresh(dpi);
                    }
                } else if rel > 0 {
                    let n = rel as usize;
                    if n <= dc.saved.len() {
                        let s = dc.saved[n - 1].clone();
                        dc.saved.truncate(n - 1);
                        dc.restore(s);
                        dc.refresh(dpi);
                    }
                }
            }
        }
        35 => {
            if let Some(xf) = rd_xform(rec, 8) {
                dc.world = xf;
                dc.refresh(dpi);
            }
        }
        36 => {
            if let (Some(xf), Some(mode)) = (rd_xform(rec, 8), rd_u32(rec, 32)) {
                match mode {
                    1 => dc.world = Mat3::IDENTITY,           // MWT_IDENTITY
                    2 => dc.world = xf.then_before(dc.world), // MWT_LEFTMULTIPLY
                    3 => dc.world = dc.world.then_before(xf), // MWT_RIGHTMULTIPLY
                    _ => {}
                }
                dc.refresh(dpi);
            }
        }
        26 => {
            // OFFSETCLIPRGN: the offset is logical; the clip is in device coordinates.
            if let Some(off) = rd_pointl(rec, 8)
                && let Some(c) = dc.clip
            {
                let d = dc.xf.apply_vec(off);
                dc.clip = Some(Bounds2::new(c.min + d, c.max + d));
            }
        }
        28 => dc.clip = None, // SETMETARGN
        30 => {
            // INTERSECTCLIPRECT: map the rect's corners, take their bounds, intersect.
            if let Some(r) = rd_rect(rec, 8) {
                let corners = [r.min, Vec2::new(r.max.x, r.min.y), r.max, Vec2::new(r.min.x, r.max.y)];
                let dev = Bounds2::from_points(corners.iter().map(|&p| dc.map(p)));
                dc.clip = Some(match dc.clip {
                    Some(c) => intersect_bounds(c, dev),
                    None => dev,
                });
            }
        }
        29 => {} // EXCLUDECLIPRECT: not representable in a single rect; ignored.
        75 => {
            // EXTSELECTCLIPRGN: cbRgnData, iMode, RgnData (region bounds at +16).
            if let (Some(mode), Some(r)) = (rd_u32(rec, 12), rd_rect(rec, 32)) {
                let corners = [r.min, Vec2::new(r.max.x, r.min.y), r.max, Vec2::new(r.min.x, r.max.y)];
                let dev = Bounds2::from_points(corners.iter().map(|&p| dc.map(p)));
                match mode {
                    1 => {
                        // RGN_AND
                        dc.clip = Some(match dc.clip {
                            Some(c) => intersect_bounds(c, dev),
                            None => dev,
                        });
                    }
                    5 => dc.clip = Some(dev), // RGN_COPY
                    _ => {}                   // RGN_OR/XOR/DIFF: ignored.
                }
            }
        }
        67 => {
            // SELECTCLIPPATH: the path's bounds become (or intersect) the clip.
            if let Some(mode) = rd_u32(rec, 8)
                && let Some(figs) = dc.path.as_ref()
            {
                let all: Vec<Vec2> = figs.iter().flat_map(|f| f.points.clone()).collect();
                let dev = Bounds2::from_points(all.iter().map(|&p| dc.map(p)));
                match mode {
                    1 => {
                        dc.clip = Some(match dc.clip {
                            Some(c) => intersect_bounds(c, dev),
                            None => dev,
                        });
                    }
                    5 => dc.clip = Some(dev),
                    _ => {}
                }
            }
        }
        // ---- objects ----
        37 => select_object(dc, rd_u32(rec, 8).unwrap_or(u32::MAX)),
        38 => {
            // CREATEPEN: ihPen, LOGPEN (style, width PointL, color).
            if let (Some(ih), Some(style), Some(w), Some(c)) = (rd_u32(rec, 8), rd_u32(rec, 12), rd_pointl(rec, 16), rd_u32(rec, 24)) {
                dc.objects.insert(ih, Obj::Pen(Pen { style, width: w.x, color: rd_color(c) }));
            }
        }
        39 => {
            // CREATEBRUSHINDIRECT: ihBrush, LOGBRUSH (style, color, hatch).
            if let (Some(ih), Some(style), Some(c)) = (rd_u32(rec, 8), rd_u32(rec, 12), rd_u32(rec, 16)) {
                dc.objects.insert(ih, Obj::Brush(Brush { style, color: rd_color(c) }));
            }
        }
        40 => {
            if let Some(ih) = rd_u32(rec, 8) {
                dc.objects.remove(&ih);
            }
        }
        82 => {
            // EXTCREATEFONTINDIRECTW: ihFont, LOGFONTW.
            if let Some(ih) = rd_u32(rec, 8)
                && let Some(f) = rd_logfontw(rec, 12)
            {
                dc.objects.insert(ih, Obj::Font(f));
            }
        }
        95 => {
            // EXTCREATEPEN: ihPen, offBmi, cbBmi, offBits, cbBits, EXTLOGPEN.
            if let (Some(ih), Some(pstyle), Some(width), Some(bstyle), Some(c)) =
                (rd_u32(rec, 8), rd_u32(rec, 28), rd_u32(rec, 32), rd_u32(rec, 36), rd_u32(rec, 40))
            {
                let geometric = pstyle & 0x1_0000 != 0;
                let style = if bstyle == 1 { 5 } else { pstyle & 0xff }; // BS_NULL brush -> no stroke
                let w = if geometric { f64::from(width) } else { 0.0 };
                dc.objects.insert(ih, Obj::Pen(Pen { style, width: w, color: rd_color(c) }));
            }
        }
        93 | 94 => {
            // CREATEMONOBRUSH / CREATEDIBPATTERNBRUSHPT: pattern brushes never fill here.
            if let Some(ih) = rd_u32(rec, 8) {
                dc.objects.insert(ih, Obj::Brush(Brush { style: 3, color: Rgb(0, 0, 0) }));
            }
        }
        48..=52 => {} // palette records
        // ---- drawing ----
        27 => {
            if let Some(p) = rd_pointl(rec, 8) {
                if dc.path.is_some() {
                    path_moveto(dc, p);
                } else {
                    dc.pos = p;
                }
            }
        }
        54 => {
            if let Some(p) = rd_pointl(rec, 8) {
                if dc.path.is_some() {
                    path_lineto(dc, p);
                } else {
                    emit_stroke(dc, out, &[dc.pos, p], false);
                    dc.pos = p;
                }
            }
        }
        4 | 87 => handle_polyline(rec, ty == 87, dc, out),
        6 | 89 => handle_polylineto(rec, ty == 89, dc, out),
        3 | 86 => handle_polygon(rec, ty == 86, dc, out),
        2 | 85 => handle_polybezier(rec, ty == 85, dc, out, false),
        5 | 88 => handle_polybezier(rec, ty == 88, dc, out, true),
        7 | 90 => handle_polypoly(rec, ty == 90, dc, out, false),
        8 | 91 => handle_polypoly(rec, ty == 91, dc, out, true),
        56 | 92 => handle_polydraw(rec, ty == 92, dc, out),
        42 => {
            if let Some(r) = rd_rect(rec, 8) {
                let pts = ellipse_points(r, dc);
                if dc.path.is_some() {
                    if let Some(&first) = pts.first() {
                        path_moveto(dc, first);
                        for p in &pts[1..] {
                            path_lineto(dc, *p);
                        }
                        path_close(dc);
                    }
                } else {
                    emit_fill(dc, out, std::slice::from_ref(&pts), None);
                    emit_stroke(dc, out, &pts, true);
                }
            }
        }
        43 => {
            if let Some(r) = rd_rect(rec, 8) {
                if dc.path.is_some() {
                    let corners = [r.min, Vec2::new(r.max.x, r.min.y), r.max, Vec2::new(r.min.x, r.max.y)];
                    path_moveto(dc, corners[0]);
                    for p in &corners[1..] {
                        path_lineto(dc, *p);
                    }
                    path_close(dc);
                } else {
                    emit_rect(dc, out, r, true, true);
                }
            }
        }
        44 => {
            if let (Some(r), Some(corner)) = (rd_rect(rec, 8), rd_pointl(rec, 24)) {
                if dc.path.is_some() {
                    let pts = round_rect_loop(r, corner);
                    if let Some(&first) = pts.first() {
                        path_moveto(dc, first);
                        for p in &pts[1..] {
                            path_lineto(dc, *p);
                        }
                        path_close(dc);
                    }
                } else {
                    let pts = round_rect_loop(r, corner);
                    emit_fill(dc, out, std::slice::from_ref(&pts), None);
                    emit_stroke(dc, out, &pts, true);
                }
            }
        }
        45..=47 => handle_arc(rec, ty, dc, out),
        55 => {
            // ARCTO: line from the current position to the arc start, then the arc.
            if let (Some(r), Some(start), Some(end)) = (rd_rect(rec, 8), rd_pointl(rec, 24), rd_pointl(rec, 32)) {
                let center = Vec2::new((r.min.x + r.max.x) / 2.0, (r.min.y + r.max.y) / 2.0);
                let (a0, sweep) = arc_span(center, start, end, dc.arc_direction == 1);
                let tol = 0.5 / dc.scale();
                let pts = arc_points(center, (r.max.x - r.min.x) / 2.0, (r.max.y - r.min.y) / 2.0, a0, sweep, tol);
                if dc.path.is_some() {
                    path_lineto(dc, pts.first().copied().unwrap_or(start));
                    for p in &pts[1..] {
                        path_lineto(dc, *p);
                    }
                } else {
                    let mut line = vec![dc.pos];
                    line.extend_from_slice(&pts);
                    emit_stroke(dc, out, &line, false);
                }
                dc.pos = pts.last().copied().unwrap_or(end);
            }
        }
        41 => {
            // ANGLEARC: centre, radius, start angle (deg), sweep angle (deg), both CCW.
            if let (Some(c), Some(radius), Some(a0), Some(sweep)) = (rd_pointl(rec, 8), rd_u32(rec, 16), rd_f32(rec, 20), rd_f32(rec, 24)) {
                let r = f64::from(radius);
                let start_rad = f64::from(a0) * std::f64::consts::PI / 180.0;
                let sweep_rad = f64::from(sweep) * std::f64::consts::PI / 180.0;
                let start_pt = Vec2::new(c.x + r * (-start_rad).cos(), c.y + r * (-start_rad).sin());
                if sweep_rad.abs() > 1e-12 {
                    let tol = 0.5 / dc.scale();
                    let pts = arc_points(c, r, r, -start_rad, -sweep_rad, tol);
                    if dc.path.is_some() {
                        for p in &pts {
                            path_lineto(dc, *p);
                        }
                    } else {
                        emit_stroke(dc, out, &pts, false);
                    }
                    dc.pos = pts.last().copied().unwrap_or(start_pt);
                } else {
                    dc.pos = start_pt;
                }
            }
        }
        15 => {
            // SETPIXELV: one device pixel.
            if let (Some(p), Some(c)) = (rd_pointl(rec, 8), rd_u32(rec, 16)) {
                let d = dc.map(p);
                let loop_ = [d, d + Vec2::new(1.0, 0.0), d + Vec2::new(1.0, 1.0), d + Vec2::new(0.0, 1.0)];
                emit_device_loop(dc, out, &loop_, rd_color(c));
            }
        }
        59 => dc.path = Some(Vec::new()), // BEGINPATH
        60 => {}                          // ENDPATH: the bracket stays for Fill/Stroke.
        61 => {
            if dc.path.is_some() {
                path_close(dc);
            }
        }
        62 => emit_path(dc, out, true, false), // FILLPATH
        63 => emit_path(dc, out, true, true),  // STROKEANDFILLPATH
        64 => emit_path(dc, out, false, true), // STROKEPATH
        65 => {}                               // FLATTENPATH: curves are already flat.
        66 => {}                               // WIDENPATH: not implemented.
        68 => dc.path = None,                  // ABORTPATH
        71 | 74 => {
            // FILLRGN / PAINTRGN: fill the region's bounds with the named brush.
            if let (Some(ih), Some(r)) = (rd_u32(rec, 28), rd_rect(rec, 48))
                && let Some(Obj::Brush(b)) = dc.objects.get(&ih)
                && b.style == 0
            {
                let corners = vec![r.min, Vec2::new(r.max.x, r.min.y), r.max, Vec2::new(r.min.x, r.max.y)];
                emit_fill(dc, out, &[corners], Some(b.color));
            }
        }
        72 => {
            // FRAMERGN: stroke the region's bounds with the named brush.
            if let (Some(ih), Some(r), Some(stroke)) = (rd_u32(rec, 28), rd_rect(rec, 56), rd_pointl(rec, 32))
                && let Some(Obj::Brush(b)) = dc.objects.get(&ih)
                && b.style == 0
            {
                let corners = vec![r.min, Vec2::new(r.max.x, r.min.y), r.max, Vec2::new(r.min.x, r.max.y)];
                let pen = Pen { style: 0, width: stroke.x, color: b.color };
                emit_stroke_with(dc, out, &corners, true, &pen);
            }
        }
        83 | 84 => handle_ext_text(rec, ty == 84, dc, out),
        // Raster records, EMF+ comments, POLYTEXTOUT and everything else: skipped.
        _ => {}
    }
}

/// Read an XFORM (6 f32: eM11, eM12, eM21, eM22, eDx, eDy) as a [`Mat3`].
fn rd_xform(rec: &[u8], off: usize) -> Option<Mat3> {
    let (a, b, c, d, e, f) =
        (rd_f32(rec, off)?, rd_f32(rec, off + 4)?, rd_f32(rec, off + 8)?, rd_f32(rec, off + 12)?, rd_f32(rec, off + 16)?, rd_f32(rec, off + 20)?);
    let m = Mat3 { a: f64::from(a), b: f64::from(b), c: f64::from(c), d: f64::from(d), e: f64::from(e), f: f64::from(f) };
    if m.det().abs() < 1e-300 || !m.det().is_finite() {
        return None;
    }
    Some(m)
}

/// Read a LOGFONTW (at `off`): height, escapement, italic, face name.
fn rd_logfontw(rec: &[u8], off: usize) -> Option<Font> {
    let height = rd_i32(rec, off)?;
    let escapement = rd_i32(rec, off + 8)?;
    let italic = rec.get(off + 20).is_some_and(|v| *v != 0);
    let mut units: Vec<u16> = Vec::new();
    for i in 0..32 {
        let u = rd_u16(rec, off + 28 + i * 2)?;
        if u == 0 {
            break;
        }
        units.push(u);
    }
    let face = String::from_utf16_lossy(&units);
    let h = f64::from(height).abs();
    Some(Font { face, height: if h > 0.0 { h } else { 12.0 }, escapement: f64::from(escapement) / 10.0 * std::f64::consts::PI / 180.0, italic })
}

/// SELECTOBJECT: stock objects (high bit set) or the handle table.
fn select_object(dc: &mut Dc, ih: u32) {
    if ih & 0x8000_0000 != 0 {
        match ih & 0x7fff_ffff {
            0 => dc.brush = Some(Brush { style: 0, color: Rgb(255, 255, 255) }), // WHITE_BRUSH
            1 => dc.brush = Some(Brush { style: 0, color: Rgb(192, 192, 192) }), // LTGRAY_BRUSH
            2 => dc.brush = Some(Brush { style: 0, color: Rgb(128, 128, 128) }), // GRAY_BRUSH
            3 => dc.brush = Some(Brush { style: 0, color: Rgb(64, 64, 64) }),    // DKGRAY_BRUSH
            4 => dc.brush = Some(Brush { style: 0, color: Rgb(0, 0, 0) }),       // BLACK_BRUSH
            5 => dc.brush = Some(Brush { style: 1, color: Rgb(0, 0, 0) }),       // NULL_BRUSH
            6 => dc.pen = Some(Pen { style: 0, width: 0.0, color: Rgb(255, 255, 255) }), // WHITE_PEN
            7 => dc.pen = Some(Pen { style: 0, width: 0.0, color: Rgb(0, 0, 0) }), // BLACK_PEN
            8 => dc.pen = Some(Pen { style: 5, width: 0.0, color: Rgb(0, 0, 0) }), // NULL_PEN
            10 | 11 | 12 | 13 | 14 | 16 | 17 => {
                dc.font = Some(Font { face: String::new(), height: 12.0, escapement: 0.0, italic: false });
            }
            15 => {}                                                             // DEFAULT_PALETTE
            18 => dc.brush = Some(Brush { style: 0, color: dc.dc_brush }),       // DC_BRUSH
            19 => dc.pen = Some(Pen { style: 0, width: 0.0, color: dc.dc_pen }), // DC_PEN
            _ => {}
        }
        return;
    }
    match dc.objects.get(&ih) {
        Some(Obj::Pen(p)) => dc.pen = Some(p.clone()),
        Some(Obj::Brush(b)) => dc.brush = Some(b.clone()),
        Some(Obj::Font(f)) => dc.font = Some(f.clone()),
        _ => {}
    }
}

/// Ellipse points in logical coordinates for `rclBox`.
fn ellipse_points(r: Bounds2, dc: &Dc) -> Vec<Vec2> {
    let center = Vec2::new((r.min.x + r.max.x) / 2.0, (r.min.y + r.max.y) / 2.0);
    let tol = 0.5 / dc.scale();
    arc_points(center, r.width() / 2.0, r.height() / 2.0, 0.0, std::f64::consts::TAU, tol)
}

/// Begin figure work at `p0` for a Polyline/PolyBezier in a path: a fresh or closed
/// figure starts there; an open figure connects with a line when asked.
fn path_start(dc: &mut Dc, p0: Vec2, connect: bool) {
    let (fresh, last_matches) = match dc.path.as_ref().and_then(|f| f.last()) {
        None => (true, false),
        Some(f) if f.closed => (true, false),
        Some(f) => (false, f.points.last().is_some_and(|l| l.near(p0, 1e-12))),
    };
    if fresh {
        path_moveto(dc, p0);
    } else if connect && !last_matches {
        path_lineto(dc, p0);
    }
}

/// Read the points of a poly record (Bounds + Count, points at 28).
fn poly_points(rec: &[u8], is16: bool) -> Option<Vec<Vec2>> {
    let count = rd_u32(rec, 24)? as usize;
    if is16 { read_pointss(rec, 28, count) } else { read_pointls(rec, 28, count) }
}

fn handle_polyline(rec: &[u8], is16: bool, dc: &mut Dc, out: &mut Out) {
    let Some(pts) = poly_points(rec, is16) else { return };
    if dc.path.is_some() {
        if let Some(&first) = pts.first() {
            path_start(dc, first, true);
            for p in &pts[1..] {
                path_lineto(dc, *p);
            }
        }
        if let Some(&last) = pts.last() {
            dc.pos = last;
        }
    } else {
        emit_stroke(dc, out, &pts, false);
    }
}

fn handle_polylineto(rec: &[u8], is16: bool, dc: &mut Dc, out: &mut Out) {
    let Some(pts) = poly_points(rec, is16) else { return };
    if dc.path.is_some() {
        for p in &pts {
            path_lineto(dc, *p);
        }
    } else {
        let mut line = vec![dc.pos];
        line.extend_from_slice(&pts);
        emit_stroke(dc, out, &line, false);
        if let Some(&last) = pts.last() {
            dc.pos = last;
        }
    }
}

fn handle_polygon(rec: &[u8], is16: bool, dc: &mut Dc, out: &mut Out) {
    let Some(pts) = poly_points(rec, is16) else { return };
    if dc.path.is_some() {
        if let Some(&first) = pts.first() {
            path_moveto(dc, first);
            for p in &pts[1..] {
                path_lineto(dc, *p);
            }
            path_close(dc);
        }
    } else {
        emit_fill(dc, out, &[pts], None);
    }
}

fn handle_polybezier(rec: &[u8], is16: bool, dc: &mut Dc, out: &mut Out, to: bool) {
    let Some(pts) = poly_points(rec, is16) else { return };
    if pts.is_empty() {
        return;
    }
    if dc.path.is_some() {
        if to {
            if pts.len() % 3 != 0 {
                return;
            }
            let mut p0 = dc.pos;
            for w in pts.as_chunks::<3>().0 {
                let (Some(&c1), Some(&c2), Some(&c3)) = (w.first(), w.get(1), w.get(2)) else { return };
                path_bezier(dc, p0, c1, c2, c3);
                p0 = c3;
            }
        } else {
            if pts.len() < 4 || (pts.len() - 1) % 3 != 0 {
                return;
            }
            let mut p0 = pts[0];
            for w in pts[1..].as_chunks::<3>().0 {
                let (Some(&c1), Some(&c2), Some(&c3)) = (w.first(), w.get(1), w.get(2)) else { return };
                path_bezier(dc, p0, c1, c2, c3);
                p0 = c3;
            }
        }
    } else {
        let tol = 0.5 / dc.scale();
        let start = if to { dc.pos } else { pts[0] };
        let mut flat = vec![start];
        if to {
            if pts.len() % 3 != 0 {
                return;
            }
            let mut p0 = dc.pos;
            for w in pts.as_chunks::<3>().0 {
                let (Some(&c1), Some(&c2), Some(&c3)) = (w.first(), w.get(1), w.get(2)) else { return };
                flatten_cubic(p0, c1, c2, c3, tol, &mut flat);
                p0 = c3;
            }
        } else {
            if (pts.len() - 1) % 3 != 0 {
                return;
            }
            let mut p0 = pts[0];
            for w in pts[1..].as_chunks::<3>().0 {
                let (Some(&c1), Some(&c2), Some(&c3)) = (w.first(), w.get(1), w.get(2)) else { return };
                flatten_cubic(p0, c1, c2, c3, tol, &mut flat);
                p0 = c3;
            }
        }
        emit_stroke(dc, out, &flat, false);
        if to && let Some(&last) = flat.last() {
            dc.pos = last;
        }
    }
}

/// POLYPOLYLINE / POLYPOLYGON (+16-bit): counts at 32, points after the counts.
fn handle_polypoly(rec: &[u8], is16: bool, dc: &mut Dc, out: &mut Out, fill: bool) {
    let (Some(n), Some(total)) = (rd_u32(rec, 24), rd_u32(rec, 28)) else { return };
    let (n, total) = (n as usize, total as usize);
    let pt_size = if is16 { 4 } else { 8 };
    if n > rec.len().saturating_sub(32) / 4 || total > rec.len().saturating_sub(32 + n * 4) / pt_size {
        return;
    }
    let mut counts = Vec::with_capacity(n);
    for i in 0..n {
        match rd_u32(rec, 32 + i * 4) {
            Some(c) => counts.push(c as usize),
            None => return,
        }
    }
    let mut off = 32 + n * 4;
    let mut loops: Vec<Vec<Vec2>> = Vec::new();
    for &c in &counts {
        let pts = if is16 { read_pointss(rec, off, c) } else { read_pointls(rec, off, c) };
        let Some(pts) = pts else { return };
        off += c * pt_size;
        if dc.path.is_some() {
            if fill {
                if let Some(&first) = pts.first() {
                    path_moveto(dc, first);
                    for p in &pts[1..] {
                        path_lineto(dc, *p);
                    }
                    path_close(dc);
                }
            } else if let Some(&first) = pts.first() {
                path_start(dc, first, true);
                for p in &pts[1..] {
                    path_lineto(dc, *p);
                }
            }
        } else if fill {
            loops.push(pts);
        } else {
            emit_stroke(dc, out, &pts, false);
        }
    }
    if fill && !loops.is_empty() {
        emit_fill(dc, out, &loops, None);
    }
}

/// POLYDRAW / POLYDRAW16: points + per-point types (PT_MOVETO 6, PT_LINETO 2,
/// PT_BEZIERTO 4, PT_CLOSEFIGURE 1 or'd in). Outside a path bracket it plays into a
/// temporary bracket whose figures are stroked at once.
fn handle_polydraw(rec: &[u8], is16: bool, dc: &mut Dc, out: &mut Out) {
    let Some(count) = rd_u32(rec, 24).map(|v| v as usize) else { return };
    let pt_size = if is16 { 4 } else { 8 };
    let (pts_off, types_off) = (28, 28 + count * pt_size);
    if count > rec.len().saturating_sub(pts_off) / pt_size || count > rec.len().saturating_sub(types_off) {
        return;
    }
    let in_path = dc.path.is_some();
    if !in_path {
        dc.path = Some(Vec::new());
    }
    let mut i = 0usize;
    while i < count {
        let ty = rec.get(types_off + i).copied().unwrap_or(0);
        let pt = if is16 { rd_points(rec, pts_off + i * pt_size) } else { rd_pointl(rec, pts_off + i * pt_size) };
        let Some(pt) = pt else { break };
        match ty & !1 {
            6 => path_moveto(dc, pt),
            2 => path_lineto(dc, pt),
            4 => {
                if i + 2 >= count {
                    break;
                }
                let (p2, p3) = (
                    if is16 { rd_points(rec, pts_off + (i + 1) * pt_size) } else { rd_pointl(rec, pts_off + (i + 1) * pt_size) },
                    if is16 { rd_points(rec, pts_off + (i + 2) * pt_size) } else { rd_pointl(rec, pts_off + (i + 2) * pt_size) },
                );
                let (Some(p2), Some(p3)) = (p2, p3) else { break };
                let start = dc.pos;
                path_bezier(dc, start, pt, p2, p3);
                // The close flag lives on the last point of the triple.
                if rec.get(types_off + i + 2).copied().unwrap_or(0) & 1 != 0 {
                    path_close(dc);
                }
                i += 2;
            }
            _ => {}
        }
        if ty & 1 != 0 && ty & !1 != 4 {
            path_close(dc);
        }
        i += 1;
    }
    if !in_path {
        let figs = dc.path.take().unwrap_or_default();
        for f in &figs {
            emit_stroke(dc, out, &f.points, f.closed);
        }
    }
}

/// ARC (stroke only), CHORD (fill + closed stroke), PIE (fill + closed stroke).
fn handle_arc(rec: &[u8], ty: u32, dc: &mut Dc, out: &mut Out) {
    let (Some(r), Some(start), Some(end)) = (rd_rect(rec, 8), rd_pointl(rec, 24), rd_pointl(rec, 32)) else { return };
    let center = Vec2::new((r.min.x + r.max.x) / 2.0, (r.min.y + r.max.y) / 2.0);
    let (a0, sweep) = arc_span(center, start, end, dc.arc_direction == 1);
    let tol = 0.5 / dc.scale();
    let pts = arc_points(center, r.width() / 2.0, r.height() / 2.0, a0, sweep, tol);
    match ty {
        45 => {
            // ARC
            if dc.path.is_some() {
                for p in &pts {
                    path_lineto(dc, *p);
                }
            } else {
                emit_stroke(dc, out, &pts, false);
            }
        }
        46 => {
            // CHORD: a closed figure on its own.
            if dc.path.is_some() {
                if let Some(&first) = pts.first() {
                    path_moveto(dc, first);
                    for p in &pts[1..] {
                        path_lineto(dc, *p);
                    }
                    path_close(dc);
                }
            } else {
                emit_fill(dc, out, std::slice::from_ref(&pts), None);
                emit_stroke(dc, out, &pts, true);
            }
        }
        47 => {
            // PIE: centre + arc + close.
            if dc.path.is_some() {
                path_moveto(dc, center);
                for p in &pts {
                    path_lineto(dc, *p);
                }
                path_close(dc);
            } else {
                let mut loop_ = vec![center];
                loop_.extend_from_slice(&pts);
                emit_fill(dc, out, &[loop_.clone()], None);
                emit_stroke(dc, out, &loop_, true);
            }
        }
        _ => {}
    }
}

/// (Align, VAlign) from a SetTextAlign value (TA_LEFT/RIGHT/CENTER, TA_TOP/BOTTOM/BASELINE).
fn text_align(flags: u32) -> (Align, VAlign) {
    let h = match flags & 0x6 {
        2 => Align::Right,
        6 => Align::Center,
        _ => Align::Left,
    };
    let v = match flags & 0x18 {
        8 => VAlign::Bottom,
        24 => VAlign::Baseline,
        _ => VAlign::Top,
    };
    (h, v)
}

/// The vertical alignment offset in glyph space, mirroring [`cadcraft_fonts::place`]:
/// glyph space has the baseline at 0 and bodies above it.
fn v_offset(v: VAlign, height: f64) -> f64 {
    match v {
        VAlign::Baseline => 0.0,
        VAlign::Bottom => height / 3.0,
        VAlign::Middle => -height / 2.0,
        VAlign::Top => -height,
    }
}

/// Decode an EXTTEXTOUT string: W strings are UTF-16 code units, A strings bytes.
fn ext_text_units(rec: &[u8], wide: bool, nchars: usize, off: usize) -> Option<Vec<u16>> {
    if wide {
        if nchars > rec.len().saturating_sub(off) / 2 {
            return None;
        }
        let mut units = Vec::with_capacity(nchars);
        for i in 0..nchars {
            units.push(rd_u16(rec, off + i * 2)?);
        }
        Some(units)
    } else {
        if nchars > rec.len().saturating_sub(off) {
            return None;
        }
        let mut units = Vec::with_capacity(nchars);
        for i in 0..nchars {
            units.push(u16::from(rec.get(off + i).copied()?));
        }
        Some(units)
    }
}

/// The Dx advance array (one i32 per code unit / byte), when present.
fn ext_text_dx(rec: &[u8], off_dx: usize, nchars: usize) -> Option<Vec<f64>> {
    if nchars > rec.len().saturating_sub(off_dx) / 4 {
        return None;
    }
    let mut out = Vec::with_capacity(nchars);
    for i in 0..nchars {
        out.push(f64::from(rd_i32(rec, off_dx + i * 4)?));
    }
    Some(out)
}

/// EXTTEXTOUTA / EXTTEXTOUTW. EMRTEXT at 36: ptlReference, nChars, offString,
/// fOptions, rcl, offDx. The string is shaped in glyph space (baseline at 0, bodies
/// above), mapped to device space by B = A ∘ diag(1, s) where s flips glyph-up onto
/// device-up (s = -1 when A preserves orientation, as in MM_TEXT), aligned by the
/// TA_ flags, then rotated by the font escapement in device space.
fn handle_ext_text(rec: &[u8], wide: bool, dc: &mut Dc, out: &mut Out) {
    let (Some(reference), Some(nchars), Some(off_string), Some(options)) = (rd_pointl(rec, 36), rd_u32(rec, 44), rd_u32(rec, 48), rd_u32(rec, 52))
    else {
        return;
    };
    let nchars = nchars as usize;
    if nchars == 0 || nchars > MAX_TEXT_CHARS {
        return;
    }
    if !out.charge(nchars) {
        return;
    }
    // ETO_OPAQUE (0x2): fill the rcl rectangle with the background colour.
    if options & 0x2 != 0
        && let Some(r) = rd_rect(rec, 56)
    {
        let corners = vec![r.min, Vec2::new(r.max.x, r.min.y), r.max, Vec2::new(r.min.x, r.max.y)];
        emit_fill(dc, out, &[corners], Some(dc.bk_color));
    }
    let font = dc.font.clone().unwrap_or(Font { face: String::new(), height: 12.0, escapement: 0.0, italic: false });
    let text_font = TextFont::resolve(&font.face);
    let oblique = if font.italic { 0.2 } else { 0.0 };
    let (h, v) = text_align(dc.text_align);
    let mapped_ref = dc.map(reference);
    // Glyph space -> device offsets: A with the y axis flipped onto device-up when A
    // preserves orientation (MM_TEXT and friends); the fixed y-up map modes already
    // flip, so they need no extra flip.
    let flip = if dc.xf.det() >= 0.0 { -1.0 } else { 1.0 };
    let b = Mat3 { a: dc.xf.a, b: dc.xf.b, c: dc.xf.c * flip, d: dc.xf.d * flip, e: 0.0, f: 0.0 };
    let to_dev = |p: Vec2| mapped_ref + b.apply(p);
    let off_dx = rd_u32(rec, 72).unwrap_or(0) as usize;
    let total_width: f64;
    if off_dx != 0 {
        // Per-character advances: shape each character at its cumulative Dx.
        let (Some(units), Some(dx)) = (ext_text_units(rec, wide, nchars, off_string as usize), ext_text_dx(rec, off_dx, nchars)) else {
            return;
        };
        let mut pairs: Vec<(char, f64)> = Vec::with_capacity(units.len());
        let mut i = 0usize;
        while i < units.len() {
            let u = units[i];
            if wide && (0xD800..0xDC00).contains(&u) && i + 1 < units.len() && (0xDC00..0xE000).contains(&units[i + 1]) {
                let c = char::decode_utf16([u, units[i + 1]]).next().and_then(Result::ok).unwrap_or('\u{FFFD}');
                let adv = dx.get(i).copied().unwrap_or(0.0) + dx.get(i + 1).copied().unwrap_or(0.0);
                pairs.push((c, adv));
                i += 2;
            } else {
                let c = if wide { char::decode_utf16([u]).next().and_then(Result::ok).unwrap_or('\u{FFFD}') } else { char::from(u as u8) };
                pairs.push((c, dx.get(i).copied().unwrap_or(0.0)));
                i += 1;
            }
        }
        total_width = pairs.iter().map(|&(_, a)| a).sum();
        let align_off = match h {
            Align::Left => 0.0,
            Align::Center => -total_width / 2.0,
            Align::Right => -total_width,
            _ => 0.0,
        };
        let dy = v_offset(v, font.height);
        let mut run = cadcraft_fonts::Shaped::default();
        let mut cum = 0.0;
        for (ch, adv) in &pairs {
            let mut buf = [0u8; 4];
            let s = ch.encode_utf8(&mut buf);
            let mut sh = cadcraft_fonts::shape_line(&text_font, s, font.height, 1.0, oblique);
            let x = cum + align_off;
            sh.map(|p| to_dev(p + Vec2::new(x, dy)));
            run.extend(sh);
            cum += *adv;
        }
        if font.escapement != 0.0 {
            run.map(|p| rotate_screen_ccw(p, mapped_ref, font.escapement));
        }
        if !run.is_empty() {
            out.texts.push((dc.text_color, run));
        }
    } else {
        let Some(units) = ext_text_units(rec, wide, nchars, off_string as usize) else { return };
        let text = if wide { String::from_utf16_lossy(&units) } else { units.iter().map(|&u| char::from(u as u8)).collect() };
        // Place at the glyph-space origin (both points at 0,0): the alignment in
        // `place` is in glyph space, exactly what the device map below expects.
        let params = TextParams {
            insert: Vec2::ZERO,
            align_pt: Some(Vec2::ZERO),
            height: font.height,
            rotation: 0.0,
            width_factor: 1.0,
            oblique,
            h,
            v,
            backwards: false,
            upside_down: false,
        };
        let (mut sh, _) = cadcraft_fonts::place(&text_font, &text, &params);
        total_width = sh.width;
        sh.map(to_dev);
        if font.escapement != 0.0 {
            sh.map(|p| rotate_screen_ccw(p, mapped_ref, font.escapement));
        }
        if !sh.is_empty() {
            out.texts.push((dc.text_color, sh));
        }
    }
    // TA_UPDATECP (0x1): the current position becomes the end of the string.
    if dc.text_align & 0x1 != 0
        && let Some(inv) = dc.xf.inverse()
    {
        let adv = b.apply_vec(Vec2::new(total_width, 0.0));
        let end_dev = mapped_ref + rotate_screen_ccw(adv, Vec2::ZERO, font.escapement);
        dc.pos = inv.apply(end_dev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32b(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    fn i32b(v: i32) -> [u8; 4] {
        v.to_le_bytes()
    }

    fn point(x: i32, y: i32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&i32b(x));
        v.extend_from_slice(&i32b(y));
        v
    }

    fn rect(l: i32, t: i32, r: i32, b: i32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&i32b(l));
        v.extend_from_slice(&i32b(t));
        v.extend_from_slice(&i32b(r));
        v.extend_from_slice(&i32b(b));
        v
    }

    fn rec(ty: u32, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&u32b(ty));
        v.extend_from_slice(&u32b((8 + payload.len()) as u32));
        v.extend_from_slice(payload);
        v
    }

    /// Assemble an EMF: header + records + EOF, with nBytes / nRecords patched.
    fn emf(bounds: (i32, i32, i32, i32), records: Vec<Vec<u8>>) -> Vec<u8> {
        let mut v = Vec::new();
        let nrecords = (records.len() + 2) as u32;
        let mut header = Vec::new();
        header.extend_from_slice(&u32b(1));
        header.extend_from_slice(&u32b(108));
        header.extend_from_slice(&rect(bounds.0, bounds.1, bounds.2, bounds.3));
        header.extend_from_slice(&rect(0, 0, 2000, 1000));
        header.extend_from_slice(&u32b(0x464d_4520)); // " EMF"
        header.extend_from_slice(&u32b(0x0001_0000));
        header.extend_from_slice(&u32b(0)); // nBytes, patched below
        header.extend_from_slice(&u32b(nrecords));
        header.extend_from_slice(&[0, 0, 0, 0]); // nHandles + sReserved
        header.extend_from_slice(&u32b(0)); // nDescription
        header.extend_from_slice(&u32b(0)); // offDescription
        header.extend_from_slice(&u32b(0)); // nPalEntries
        header.extend_from_slice(&i32b(0));
        header.extend_from_slice(&i32b(0)); // szlDevice
        header.extend_from_slice(&i32b(0));
        header.extend_from_slice(&i32b(0)); // szlMillimeters
        header.extend_from_slice(&u32b(0)); // cbPixelFormat
        header.extend_from_slice(&u32b(0)); // offPixelFormat
        header.extend_from_slice(&u32b(0)); // bOpenGL
        header.extend_from_slice(&i32b(0));
        header.extend_from_slice(&i32b(0)); // szlMicrometers
        v.extend_from_slice(&header);
        for r in records {
            v.extend_from_slice(&r);
        }
        v.extend_from_slice(&rec(14, &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        let nbytes = v.len() as u32;
        v[48..52].copy_from_slice(&u32b(nbytes));
        v
    }

    fn create_pen(ih: u32, style: u32, width: i32, color: u32) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&u32b(ih));
        p.extend_from_slice(&u32b(style));
        p.extend_from_slice(&point(width, 0));
        p.extend_from_slice(&u32b(color));
        rec(38, &p)
    }

    fn create_brush(ih: u32, style: u32, color: u32) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&u32b(ih));
        p.extend_from_slice(&u32b(style));
        p.extend_from_slice(&u32b(color));
        p.extend_from_slice(&u32b(0));
        rec(39, &p)
    }

    fn select(ih: u32) -> Vec<u8> {
        rec(37, &u32b(ih))
    }

    /// A synthetic EMF: red pen, blue brush, rectangle + line + "Hi" text.
    fn sample() -> Vec<u8> {
        let mut r = rect(10, 10, 110, 60);
        let rect_rec = rec(43, &r);
        r = point(20, 80);
        let moveto = rec(27, &r); // MOVETOEX
        r = point(180, 80);
        let lineto = rec(54, &r); // LINETO
        // EXTCREATEFONTINDIRECTW: ihFont 3, LOGFONTW with lfHeight -20, empty face.
        let mut f = Vec::new();
        f.extend_from_slice(&u32b(3));
        f.extend_from_slice(&i32b(-20)); // lfHeight
        f.extend_from_slice(&i32b(0)); // lfWidth
        f.extend_from_slice(&i32b(0)); // lfEscapement
        f.extend_from_slice(&i32b(0)); // lfOrientation
        f.extend_from_slice(&i32b(400)); // lfWeight
        f.extend_from_slice(&[0; 8]); // italic..pitchAndFamily
        f.extend_from_slice(&[0; 64]); // lfFaceName (empty -> stroke font)
        let font_rec = rec(82, &f);
        // EXTTEXTOUTW "Hi" at (20, 70).
        let mut t = Vec::new();
        t.extend_from_slice(&rect(20, 70, 60, 90)); // Bounds
        t.extend_from_slice(&u32b(0)); // iGraphicsMode
        t.extend_from_slice(&1.0f32.to_le_bytes()); // exScale
        t.extend_from_slice(&1.0f32.to_le_bytes()); // eyScale
        t.extend_from_slice(&point(20, 70)); // ptlReference
        t.extend_from_slice(&u32b(2)); // nChars
        t.extend_from_slice(&u32b(76)); // offString
        t.extend_from_slice(&u32b(0)); // fOptions
        t.extend_from_slice(&rect(20, 70, 60, 90)); // rcl
        t.extend_from_slice(&u32b(0)); // offDx
        t.extend_from_slice(&[0x48, 0x00, 0x69, 0x00]); // "Hi" UTF-16LE
        let text_rec = rec(84, &t);
        let records = vec![
            create_pen(1, 0, 2, 0x0000_00ff), // red pen, width 2
            select(1),
            create_brush(2, 0, 0x00ff_0000), // blue brush
            select(2),
            rect_rec,
            moveto,
            lineto,
            font_rec,
            select(3),
            rec(24, &u32b(0)), // SETTEXTCOLOR black
            rec(22, &u32b(0)), // SETTEXTALIGN TA_LEFT | TA_TOP
            text_rec,
        ];
        emf((0, 0, 200, 100), records)
    }

    #[test]
    fn synthetic_emf_draws_rect_line_and_text() {
        let parsed = parse_emf(&sample());
        assert_eq!(parsed.bounds, Bounds2::new(Vec2::new(0.0, 0.0), Vec2::new(200.0, 100.0)));
        // The rectangle fill: blue triangles inside the rect.
        assert_eq!(parsed.fills.len(), 1);
        let (rgb, tris) = &parsed.fills[0];
        assert_eq!(*rgb, Rgb(0, 0, 255));
        assert!(tris.len() >= 6 && tris.len().is_multiple_of(3));
        assert!(tris.iter().all(|p| (10.0..=110.0).contains(&p.x) && (10.0..=60.0).contains(&p.y)));
        // The strokes: rectangle outline (red, width 2) + line (red).
        assert_eq!(parsed.strokes.len(), 2);
        assert!(parsed.strokes.iter().all(|s| s.rgb == Rgb(255, 0, 0)));
        let line = parsed.strokes.iter().find(|s| s.points.len() == 2).expect("the line");
        assert!(line.points[0].near(Vec2::new(20.0, 80.0), 1e-9));
        assert!(line.points[1].near(Vec2::new(180.0, 80.0), 1e-9));
        assert!((line.width - 2.0).abs() < 1e-9);
        // The text: one run, glyphs with the top at y = 70 (TA_TOP).
        assert_eq!(parsed.texts.len(), 1);
        let (rgb, sh) = &parsed.texts[0];
        assert_eq!(*rgb, Rgb(0, 0, 0));
        assert!(!sh.is_empty());
        let ys: Vec<f64> = sh.outlines().iter().flatten().map(|p| p.y).collect();
        assert!(!ys.is_empty());
        assert!(ys.iter().all(|&y| (70.0..=90.0).contains(&y)), "text y range {ys:?}");
        let xs: Vec<f64> = sh.outlines().iter().flatten().map(|p| p.x).collect();
        assert!(xs.iter().all(|&x| x >= 20.0 - 1e-9));
    }

    #[test]
    fn ink_bounds_cover_what_was_drawn_and_frame_mapping_prefers_them_over_sheet_margins() {
        // The sample's ink: rect from (10,10), line to x = 180, text at y 70..90 —
        // strictly inside its (0,0)-(200,100) header bounds.
        let parsed = parse_emf(&sample());
        assert!(!parsed.ink.is_empty());
        assert_eq!(parsed.ink.min.x, 10.0);
        assert_eq!(parsed.ink.max.x, 180.0);
        assert!(parsed.bounds.contains_box(&parsed.ink));
        // Ink nearly fills the header here, so the header stays the map bounds.
        assert_eq!(frame_bounds(&parsed), parsed.bounds);
        // The Excel case: the same content under sheet-sized header bounds maps
        // by its ink, so the table fills its frame instead of shrinking into a
        // corner of it.
        let mut wide = parse_emf(&sample());
        wide.bounds = Bounds2::new(Vec2::new(0.0, 0.0), Vec2::new(2000.0, 1000.0));
        assert_eq!(frame_bounds(&wide), wide.ink);
        // No ink (an empty preview) keeps the header bounds.
        let empty = ParsedEmf { bounds: wide.bounds, ink: Bounds2::EMPTY, strokes: Vec::new(), fills: Vec::new(), texts: Vec::new() };
        assert_eq!(frame_bounds(&empty), wide.bounds);
    }

    #[test]
    fn clip_rect_and_savedc_restore() {
        let records = vec![
            create_brush(2, 0, 0x0000_00ff), // red brush
            select(2),
            rec(33, &[]),                   // SAVEDC
            rec(30, &rect(0, 0, 50, 50)),   // INTERSECTCLIPRECT
            rec(43, &rect(0, 0, 100, 100)), // clipped rect
            rec(34, &i32b(-1)),             // RESTOREDC
            rec(43, &rect(0, 0, 100, 100)), // full rect
        ];
        let parsed = parse_emf(&emf((0, 0, 200, 200), records));
        assert_eq!(parsed.fills.len(), 2);
        // The clipped fill stays inside the 50x50 clip.
        assert!(parsed.fills[0].1.iter().all(|p| p.x <= 50.0 + 1e-9 && p.y <= 50.0 + 1e-9));
        // After RestoreDC the clip is gone: the second fill reaches past 50.
        assert!(parsed.fills[1].1.iter().any(|p| p.x > 50.0 + 1e-9));
    }

    #[test]
    fn polyline16_and_polydraw() {
        // POLYLINE16: (0,0) (10,0) (10,10).
        let mut p = Vec::new();
        p.extend_from_slice(&rect(0, 0, 10, 10));
        p.extend_from_slice(&u32b(3));
        for (x, y) in [(0i16, 0i16), (10, 0), (10, 10)] {
            p.extend_from_slice(&x.to_le_bytes());
            p.extend_from_slice(&y.to_le_bytes());
        }
        // POLYDRAW: moveto (20,0), lineto (30,0), lineto (30,10) + close.
        let mut d = Vec::new();
        d.extend_from_slice(&rect(20, 0, 30, 10));
        d.extend_from_slice(&u32b(3));
        for (x, y) in [(20i32, 0i32), (30, 0), (30, 10)] {
            d.extend_from_slice(&i32b(x));
            d.extend_from_slice(&i32b(y));
        }
        d.extend_from_slice(&[6, 2, 2 | 1]);
        let records = vec![
            create_pen(1, 0, 0, 0), // black cosmetic pen
            select(1),
            rec(87, &p),
            rec(56, &d),
        ];
        let parsed = parse_emf(&emf((0, 0, 200, 200), records));
        assert_eq!(parsed.strokes.len(), 2);
        assert_eq!(parsed.strokes[0].points.len(), 3);
        // The polydraw figure is closed: first point appended at the end.
        assert_eq!(parsed.strokes[1].points.len(), 4);
        assert!(parsed.strokes[1].points[0].near(parsed.strokes[1].points[3], 1e-9));
    }

    #[test]
    fn hostile_input_never_panics() {
        assert!(parse_emf(&[]).is_empty());
        assert!(parse_emf(&[1, 0, 0, 0, 8, 0, 0, 0]).is_empty());
        assert!(parse_emf(&[0xff; 200]).is_empty());
        // A record whose size runs past the end of the stream.
        let mut bad = emf((0, 0, 10, 10), vec![]);
        bad.extend_from_slice(&[4, 0, 0, 0, 0xff, 0xff, 0xff, 0x7f]);
        assert!(parse_emf(&bad).is_empty());
        // A POLYLINE claiming a million points in a tiny record.
        let mut p = Vec::new();
        p.extend_from_slice(&rect(0, 0, 10, 10));
        p.extend_from_slice(&u32b(1_000_000));
        let parsed = parse_emf(&emf((0, 0, 10, 10), vec![rec(4, &p)]));
        assert!(parsed.is_empty());
        // Deep SaveDC nesting is capped, not a stack overflow.
        let mut records = Vec::new();
        for _ in 0..1000 {
            records.push(rec(33, &[]));
        }
        let parsed = parse_emf(&emf((0, 0, 10, 10), records));
        assert!(parsed.is_empty());
        // Hex decode rejects garbage and odd nibbles (a failed decode may leave a
        // partial prefix in `out`; the caller discards it).
        let mut out = Vec::new();
        assert!(!hex_decode_into("zz", &mut out));
        assert!(!hex_decode_into("abc", &mut out));
        let mut out = Vec::new();
        assert!(hex_decode_into(" 0A0b\n", &mut out));
        assert_eq!(out, vec![0x0a, 0x0b]);
    }
}
