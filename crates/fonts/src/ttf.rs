//! TrueType/OpenType text: system fonts found at run time (never bundled), glyph outlines as
//! closed polylines in CAD text units (text height = cap height).
//!
//! Fonts are found by file stem (`arial.ttf`, `msjh.ttc`) and, failing that, by family name
//! (`Noto Sans CJK TC`) among installed files with a similar name. TrueType collections
//! (`.ttc` / `.otc`) are scanned too; a stem lookup takes their first face.
//!
//! Two process-wide settings (like AutoCAD's profile variables) control substitution:
//! - [`font_alt`] (`FONTALT`): the font used for a text style font that can't be found (an SHX
//!   file we don't read, a missing TTF). Empty = the built-in stroke font.
//! - [`fallback_fonts`] (`FONTFALLBACK`): fonts tried first for characters the text's font
//!   lacks (CJK in an SHX big font, for example), before the automatic system font search.
//!
//! On native builds both start from the `CADCRAFT_FONTALT` / `CADCRAFT_FONTFALLBACK`
//! environment variables.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock};

use cadcraft_geom::Vec2;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::FileRef;
use skrifa::raw::types::Tag;
use skrifa::{FontRef, GlyphId, MetadataProvider};

use crate::{Run, Shaped};

/// One face of a font file (a TrueType collection holds several).
#[derive(Clone)]
pub struct Face {
    pub bytes: Arc<Vec<u8>>,
    /// Face index inside a collection (0 for a plain font file).
    pub index: u32,
}

/// The same face of the same loaded file.
impl PartialEq for Face {
    fn eq(&self, other: &Face) -> bool {
        Arc::ptr_eq(&self.bytes, &other.bytes) && self.index == other.index
    }
}

impl std::fmt::Debug for Face {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Face").field("len", &self.bytes.len()).field("index", &self.index).finish()
    }
}

impl Face {
    /// Face `index` of font file `bytes`, if it parses.
    pub fn new(bytes: Arc<Vec<u8>>, index: u32) -> Option<Face> {
        FontRef::from_index(&bytes, index).ok()?;
        Some(Face { bytes, index })
    }
    pub(crate) fn font(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(&self.bytes, self.index).ok()
    }
    /// Whether two handles are the same face of the same loaded file.
    pub(crate) fn same(&self, other: &Face) -> bool {
        Arc::ptr_eq(&self.bytes, &other.bytes) && self.index == other.index
    }
    /// Whether the face has OpenType Arabic joining features (`init` / `medi` / `fina`). Faces
    /// without them get Unicode presentation forms instead ([`crate::bidi::presentation_forms`]).
    pub(crate) fn has_joining_features(&self) -> bool {
        use skrifa::raw::TableProvider;
        let tags = [Tag::new(b"init"), Tag::new(b"medi"), Tag::new(b"fina")];
        self.font()
            .and_then(|f| f.gsub().ok())
            .and_then(|g| g.feature_list().ok())
            .is_some_and(|l| l.feature_records().iter().any(|r| tags.contains(&r.feature_tag())))
    }
    /// Whether the face maps `c` to a glyph.
    pub fn has_glyph(&self, c: char) -> bool {
        self.font().is_some_and(|f| f.charmap().map(c).is_some())
    }
    fn names(&self) -> Vec<String> {
        let Some(f) = self.font() else { return Vec::new() };
        [skrifa::string::StringId::FAMILY_NAME, skrifa::string::StringId::TYPOGRAPHIC_FAMILY_NAME, skrifa::string::StringId::FULL_NAME]
            .into_iter()
            .flat_map(|id| f.localized_strings(id).map(|s| norm(&s.to_string())).collect::<Vec<_>>())
            .collect()
    }
}

/// Faces of a font file: every face of a collection (capped), or the single font.
pub fn faces(bytes: &Arc<Vec<u8>>) -> Vec<Face> {
    match FileRef::new(bytes) {
        Ok(FileRef::Collection(c)) => (0..c.len().min(MAX_FACES_PER_FILE)).filter_map(|i| Face::new(bytes.clone(), i)).collect(),
        Ok(FileRef::Font(_)) => vec![Face { bytes: bytes.clone(), index: 0 }],
        Err(_) => Vec::new(),
    }
}

const MAX_FACES_PER_FILE: u32 = 32;
/// Bounds of the missing-glyph search: files loaded, bytes loaded, faces checked per character.
const MAX_FALLBACK_FILES: usize = 24;
const MAX_FALLBACK_BYTES: usize = 160 << 20;
const MAX_FALLBACK_FACES: usize = 256;
/// Files loaded by one family-name lookup.
const MAX_FAMILY_FILES: usize = 6;
/// Further files a family-name lookup loads: those starting with the name's first word.
const MAX_FIRST_WORD_FILES: usize = 24;
/// Total bytes of system font files loaded from disk: a drawing naming many styles can't pull
/// every installed font into memory.
const MAX_LOADED_BYTES: usize = 256 << 20;
const MAX_CACHED_NAMES: usize = 4096;
const MAX_CACHED_CHARS: usize = 50_000;

/// Fonts known to cover many scripts (CJK first), tried first for missing glyphs. Matched as
/// substrings of the normalised file stem.
const PREFERRED_FALLBACK: &[&str] = &[
    "notosanscjk",
    "sourcehansans",
    "notoserifcjk",
    "sourcehanserif",
    "wqyzenhei",
    "wqymicrohei",
    "droidsansfallback",
    "pingfang",
    "hiraginosans",
    "stheiti",
    "msjh",
    "msyh",
    "yugoth",
    "msgothic",
    "malgun",
    "simsun",
    "mingliu",
    "arialuni",
    "notosans",
    "dejavusans",
    "segoeui",
    "arial",
];
/// Fonts known to cover a right-to-left script, tried before the general search for its
/// characters (the general search loads a bounded number of files, and wide-coverage CJK fonts
/// come first in it). Matched as substrings of the normalised file stem.
const ARABIC_FALLBACK: &[&str] = &[
    "vazirmatn",
    "vazir",
    "notonaskharabic",
    "notosansarabic",
    "notokufiarabic",
    "amiri",
    "sahel",
    "samim",
    "geezapro",
    "segoeui",
    "tahoma",
    "arial",
    "dejavusans",
];
const HEBREW_FALLBACK: &[&str] = &["notosanshebrew", "notoserifhebrew", "arial", "tahoma", "segoeui", "dejavusans"];
/// Script-specific fallback files loaded per character, at most.
const MAX_SCRIPT_FILES: usize = 4;

/// Script-specific fallback font names for `c`, if any.
fn script_fallback(c: char) -> &'static [&'static str] {
    match c {
        '\u{0600}'..='\u{08FF}' | '\u{FB50}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFF}' => ARABIC_FALLBACK,
        '\u{0590}'..='\u{05FF}' | '\u{FB1D}'..='\u{FB4F}' => HEBREW_FALLBACK,
        _ => &[],
    }
}

const WEIGHT_WORDS: &[&str] = &["thin", "light", "medium", "semibold", "bold", "black", "heavy", "italic", "oblique", "mono", "condensed"];

#[derive(Default)]
struct Db {
    scanned: bool,
    /// Font files found by the scan and fonts compiled into the binary, by lower-case file stem.
    paths: HashMap<String, Source>,
    /// Loaded or registered font file bytes, by lower-case file stem.
    files: HashMap<String, Arc<Vec<u8>>>,
    /// Name lookups already answered, found or not.
    resolved: HashMap<String, Option<Face>>,
    fallback: Fallback,
    /// Bytes of font files loaded from disk so far (bounded by `MAX_LOADED_BYTES`).
    loaded_bytes: usize,
}

/// State of the missing-glyph search.
#[derive(Default)]
struct Fallback {
    /// Candidate file stems, best first (built on first use).
    order: Option<Vec<String>>,
    /// Next candidate in `order` to load.
    next: usize,
    files: usize,
    bytes: usize,
    /// Faces of the loaded candidates, in candidate order.
    faces: Vec<Face>,
    /// Character → index in `faces` (None: nothing covers it).
    by_char: HashMap<char, Option<usize>>,
}

/// Where a font file's bytes come from.
enum Source {
    /// An installed font file, read on first use.
    File(PathBuf),
    /// A font compiled into the binary ([`crate::CRAFT_FONTS`]), copied on first use.
    Static(&'static [u8]),
}

fn lock() -> MutexGuard<'static, Db> {
    static DB: OnceLock<Mutex<Db>> = OnceLock::new();
    let mut g = DB.get_or_init(|| Mutex::new(Db::default())).lock().unwrap_or_else(PoisonError::into_inner);
    if !g.scanned {
        g.scanned = true;
        g.paths = scan().into_iter().map(|(stem, p)| (stem, Source::File(p))).collect();
        // Embedded craft-fonts fonts take the place of an installed file with the same stem.
        for f in crate::CRAFT_FONTS {
            g.paths.insert(f.stem(), Source::Static(f.bytes));
        }
    }
    g
}

#[cfg(not(target_arch = "wasm32"))]
fn scan() -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        dirs.extend(["/System/Library/Fonts", "/System/Library/Fonts/Supplemental", "/Library/Fonts"].map(Into::into));
        if let Some(h) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(h).join("Library/Fonts"));
        }
    } else if cfg!(windows) {
        dirs.push("C:\\Windows\\Fonts".into());
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(l).join("Microsoft\\Windows\\Fonts"));
        }
    } else {
        dirs.extend(["/usr/share/fonts", "/usr/local/share/fonts"].map(Into::into));
        if let Some(h) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(&h).join(".fonts"));
            dirs.push(PathBuf::from(h).join(".local/share/fonts"));
        }
    }
    let mut stack = dirs;
    let mut seen = 0usize;
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > 20_000 {
                return out;
            }
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let ext = p.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            if !matches!(ext.as_str(), "ttf" | "otf" | "ttc" | "otc") {
                continue;
            }
            if let Some(stem) = p.file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()) {
                out.entry(stem).or_insert(p);
            }
        }
    }
    out
}

#[cfg(target_arch = "wasm32")]
fn scan() -> HashMap<String, PathBuf> {
    HashMap::new()
}

#[cfg(not(target_arch = "wasm32"))]
fn read_file(path: &std::path::Path) -> Option<Vec<u8>> {
    /// Font files larger than this are never read.
    const MAX_FILE_BYTES: u64 = 96 << 20;
    if std::fs::metadata(path).ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    std::fs::read(path).ok()
}

#[cfg(target_arch = "wasm32")]
fn read_file(_path: &std::path::Path) -> Option<Vec<u8>> {
    None
}

impl Db {
    /// Bytes of the font file with this stem, loading it on first use.
    fn file(&mut self, stem: &str) -> Option<Arc<Vec<u8>>> {
        if let Some(b) = self.files.get(stem) {
            return Some(b.clone());
        }
        if self.loaded_bytes >= MAX_LOADED_BYTES {
            return None;
        }
        let bytes = Arc::new(match self.paths.get(stem)? {
            Source::File(path) => read_file(path)?,
            Source::Static(bytes) => bytes.to_vec(),
        });
        self.loaded_bytes = self.loaded_bytes.saturating_add(bytes.len());
        self.files.insert(stem.to_string(), bytes.clone());
        Some(bytes)
    }

    fn lookup(&mut self, k: &str) -> Option<Face> {
        let candidates = [k.to_string(), k.replace(' ', ""), format!("{k} regular"), format!("{}-regular", k.replace(' ', ""))];
        for c in &candidates {
            if let Some(face) = self.file(c).and_then(|b| faces(&b).into_iter().next()) {
                return Some(face);
            }
        }
        self.by_family(k)
    }

    /// A face whose family or full name is `k`, among files with a similar stem, then among the
    /// files whose stem begins with the name's first word (`GOST.TTF` for "GOST type A").
    fn by_family(&mut self, k: &str) -> Option<Face> {
        let want = norm(k);
        if want.len() < 5 {
            return None;
        }
        let stems: Vec<String> = self.paths.keys().chain(self.files.keys()).cloned().collect();
        for stem in family_candidates(&stems, &want, &norm(k.split_whitespace().next().unwrap_or(""))) {
            let Some(bytes) = self.file(&stem) else { continue };
            if let Some(face) = faces(&bytes).into_iter().find(|f| f.names().iter().any(|n| *n == want || format!("{n}regular") == want)) {
                return Some(face);
            }
        }
        None
    }

    /// Load the next fallback candidate file. False when none is left or a bound is reached.
    fn load_next_fallback(&mut self) -> bool {
        if self.fallback.order.is_none() {
            let stems: Vec<String> = self.paths.keys().chain(self.files.keys()).cloned().collect();
            self.fallback.order = Some(rank_fallback(&stems));
        }
        loop {
            if self.fallback.files >= MAX_FALLBACK_FILES || self.fallback.bytes >= MAX_FALLBACK_BYTES {
                return false;
            }
            let Some(stem) = self.fallback.order.as_ref().and_then(|o| o.get(self.fallback.next)).cloned() else { return false };
            self.fallback.next += 1;
            let Some(bytes) = self.file(&stem) else { continue };
            self.fallback.files += 1;
            self.fallback.bytes = self.fallback.bytes.saturating_add(bytes.len());
            let fs = faces(&bytes);
            if !fs.is_empty() {
                self.fallback.faces.extend(fs);
                return true;
            }
        }
    }

    /// Index in `fallback.faces` of a face of a font known to cover `c`'s script (loaded and
    /// added on demand), if one is installed and has `c`.
    fn search_script_fallback(&mut self, c: char) -> Option<usize> {
        let names = script_fallback(c);
        if names.is_empty() {
            return None;
        }
        let mut stems: Vec<(usize, usize, String)> = self
            .paths
            .keys()
            .chain(self.files.keys())
            .filter_map(|s| {
                let n = norm(s);
                names.iter().position(|p| n.contains(p)).map(|pref| (weight_penalty(s, ""), pref, s.clone()))
            })
            .collect();
        stems.sort();
        stems.dedup_by(|a, b| a.2 == b.2);
        for (_, _, stem) in stems.into_iter().take(MAX_SCRIPT_FILES) {
            let Some(bytes) = self.file(&stem) else { continue };
            let Some(face) = faces(&bytes).into_iter().find(|f| f.has_glyph(c)) else { continue };
            if let Some(i) = self.fallback.faces.iter().position(|f| f.same(&face)) {
                return Some(i);
            }
            if self.fallback.faces.len() >= MAX_FALLBACK_FACES {
                return None;
            }
            self.fallback.faces.push(face);
            return Some(self.fallback.faces.len() - 1);
        }
        None
    }

    /// Index in `fallback.faces` of the first face that has `c`.
    fn search_fallback(&mut self, c: char) -> Option<usize> {
        if let Some(r) = self.fallback.by_char.get(&c) {
            return *r;
        }
        let mut found = self.search_script_fallback(c);
        let mut i = 0;
        while found.is_none() && i < MAX_FALLBACK_FACES {
            if i >= self.fallback.faces.len() && !self.load_next_fallback() {
                break;
            }
            if self.fallback.faces.get(i).is_some_and(|f| f.has_glyph(c)) {
                found = Some(i);
                break;
            }
            i += 1;
        }
        // Only a found face is cached: a miss may just mean the bounded search
        // has not reached the covering face yet, and caching it would keep the
        // character a missing glyph for the whole session.
        if found.is_some() {
            if self.fallback.by_char.len() >= MAX_CACHED_CHARS {
                self.fallback.by_char.clear();
            }
            self.fallback.by_char.insert(c, found);
        }
        found
    }
}

/// Lower-case, without spaces, hyphens and underscores.
fn norm(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, ' ' | '-' | '_')).flat_map(char::to_lowercase).collect()
}

/// The font file stems to check for the family `want` (normalised), best first: those sharing a
/// long prefix with it ([`MAX_FAMILY_FILES`]), then those starting with its `first` word
/// ([`MAX_FIRST_WORD_FILES`]).
fn family_candidates(stems: &[String], want: &str, first: &str) -> Vec<String> {
    let mut by_prefix: Vec<(usize, usize, String)> =
        stems.iter().map(|s| (weight_penalty(s, want), common_prefix(&norm(s), want), s.clone())).filter(|(_, p, _)| *p >= 5).collect();
    by_prefix.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    by_prefix.dedup_by(|a, b| a.2 == b.2);
    let mut out: Vec<String> = by_prefix.into_iter().take(MAX_FAMILY_FILES).map(|(_, _, s)| s).collect();
    if first.chars().count() >= 4 {
        let mut by_word: Vec<(usize, usize, String)> =
            stems.iter().filter(|s| norm(s).starts_with(first) && !out.contains(s)).map(|s| (weight_penalty(s, want), s.len(), s.clone())).collect();
        by_word.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        by_word.dedup_by(|a, b| a.2 == b.2);
        out.extend(by_word.into_iter().take(MAX_FIRST_WORD_FILES).map(|(_, _, s)| s));
    }
    out
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Weight / style words in a file name that the wanted name doesn't ask for.
fn weight_penalty(stem: &str, want: &str) -> usize {
    let n = norm(stem);
    WEIGHT_WORDS.iter().filter(|w| n.contains(*w) && !want.contains(*w)).count()
}

/// Order font file stems for the missing-glyph search: known wide-coverage (CJK) fonts first,
/// regular weights before bold, light and the like, then by name.
pub(crate) fn rank_fallback(stems: &[String]) -> Vec<String> {
    let embedded_rank = PREFERRED_FALLBACK.iter().position(|p| *p == "notosans").unwrap_or(PREFERRED_FALLBACK.len());
    let mut v: Vec<(usize, usize, String)> = stems
        .iter()
        .map(|s| {
            let n = norm(s);
            let pref = PREFERRED_FALLBACK.iter().position(|p| n.contains(p)).unwrap_or(PREFERRED_FALLBACK.len());
            // Embedded craft-fonts fonts come right after the installed wide-coverage (CJK) fonts,
            // before the general-purpose ones, so the bounded search always reaches them.
            let pref = if crate::craft_fonts::is_embedded(s) { pref.min(embedded_rank) } else { pref };
            (pref, weight_penalty(s, ""), s.clone())
        })
        .collect();
    v.sort();
    v.dedup_by(|a, b| a.2 == b.2);
    v.into_iter().map(|(_, _, s)| s).collect()
}

/// Register font bytes under a name (web builds and tests).
pub fn register(name: &str, bytes: Vec<u8>) {
    let mut g = lock();
    g.files.insert(key(name), Arc::new(bytes));
    g.resolved.clear();
    g.fallback = Fallback::default();
}

/// Register font bytes that live for the whole process under a name; they are copied only when
/// the font is first used, the way the embedded craft-fonts fonts are listed.
#[cfg(test)]
pub(crate) fn register_static(name: &str, bytes: &'static [u8]) {
    let mut g = lock();
    let k = key(name);
    g.files.remove(&k);
    g.paths.insert(k, Source::Static(bytes));
    g.resolved.clear();
    g.fallback = Fallback::default();
}

/// Normalise a style font name ("arial.ttf", "Arial", "ARIALBD.TTF") to a lookup key.
fn key(name: &str) -> String {
    let n = name.trim().to_ascii_lowercase();
    n.trim_end_matches(".ttf").trim_end_matches(".otf").trim_end_matches(".ttc").trim_end_matches(".otc").to_string()
}

/// Names that mean a stroke font: never looked up as TrueType.
fn is_stroke_name(k: &str) -> bool {
    k.is_empty() || k.ends_with(".shx") || k == crate::BUILTIN_FONT.to_ascii_lowercase() || k == "txt" || k == "simplex" || k == "romans"
}

/// Well-known font names and the installed equivalents to try when the named
/// font is missing. A drawing that asks for SimSun (the Windows Song font) opens
/// on macOS as Songti SC, and vice versa; without this, the style falls back to
/// the single-stroke font, so ASCII renders as hairline strokes while CJK
/// renders as filled fallback outlines — the "only Chinese looks right" split.
/// Keys and candidates are `key()`-normalised; the primary name is always tried
/// first, so an installed SimSun still wins over its aliases.
const FONT_ALIASES: &[(&str, &[&str])] = &[
    ("simsun", &["songti sc", "stsongti", "songti", "noto serif cjk sc", "source han serif sc"]),
    ("宋体", &["songti sc", "stsongti", "songti", "noto serif cjk sc", "source han serif sc"]),
    ("stsong", &["songti sc", "stsongti", "songti", "noto serif cjk sc", "source han serif sc"]),
    ("nsimsun", &["songti sc", "stsongti", "songti", "noto serif cjk sc", "source han serif sc"]),
    ("simsun-extb", &["songti sc", "stsongti", "songti", "noto serif cjk sc", "source han serif sc"]),
    ("songti sc", &["simsun", "stsong", "noto serif cjk sc", "source han serif sc"]),
    ("stsongti", &["simsun", "stsong", "noto serif cjk sc", "source han serif sc"]),
    ("songti", &["simsun", "stsong", "noto serif cjk sc", "source han serif sc"]),
    ("simhei", &["heiti sc", "stheiti", "pingfang sc", "noto sans cjk sc", "source han sans sc"]),
    ("黑体", &["heiti sc", "stheiti", "pingfang sc", "noto sans cjk sc", "source han sans sc"]),
    ("stheiti", &["simhei", "heiti sc", "pingfang sc", "noto sans cjk sc", "source han sans sc"]),
    ("heiti sc", &["simhei", "stheiti", "pingfang sc", "noto sans cjk sc", "source han sans sc"]),
    ("simkai", &["kaiti sc", "stkaiti", "kaiti", "noto serif cjk sc", "source han serif sc"]),
    ("楷体", &["kaiti sc", "stkaiti", "kaiti", "noto serif cjk sc", "source han serif sc"]),
    ("stkaiti", &["simkai", "kaiti sc", "kaiti", "noto serif cjk sc", "source han serif sc"]),
    ("kaiti sc", &["simkai", "stkaiti", "kaiti", "noto serif cjk sc", "source han serif sc"]),
    ("simfang", &["stfangsong", "fangsong", "songti sc", "simsun"]),
    ("仿宋", &["stfangsong", "fangsong", "songti sc", "simsun"]),
    ("stfangsong", &["simfang", "fangsong", "songti sc", "simsun"]),
    ("stsong", &["songti sc", "stsongti", "songti", "simsun", "noto serif cjk sc", "source han serif sc"]),
    ("华文宋体", &["songti sc", "stsongti", "songti", "simsun", "noto serif cjk sc", "source han serif sc"]),
    ("stzhongsong", &["songti sc", "stsongti", "songti", "simsun", "noto serif cjk sc", "source han serif sc"]),
    ("华文中宋", &["songti sc", "stsongti", "songti", "simsun", "noto serif cjk sc", "source han serif sc"]),
    ("新宋体", &["simsun", "nsimsun", "songti sc", "stsongti", "noto serif cjk sc", "source han serif sc"]),
    ("隶书", &["songti sc", "stsongti", "songti", "simsun", "noto serif cjk sc", "source han serif sc"]),
    ("华文隶书", &["songti sc", "stsongti", "songti", "simsun", "noto serif cjk sc", "source han serif sc"]),
    ("微软雅黑", &["pingfang sc", "microsoft yahei", "msyh", "noto sans cjk sc", "source han sans sc"]),
    ("microsoft yahei", &["pingfang sc", "msyh", "微软雅黑", "noto sans cjk sc", "source han sans sc"]),
    ("msyh", &["microsoft yahei", "pingfang sc", "noto sans cjk sc", "source han sans sc"]),
    ("等线", &["pingfang sc", "dengxian", "noto sans cjk sc", "source han sans sc", "microsoft yahei"]),
    ("dengxian", &["pingfang sc", "等线", "noto sans cjk sc", "source han sans sc"]),
    ("华文黑体", &["heiti sc", "stheiti", "simhei", "pingfang sc", "noto sans cjk sc", "source han sans sc"]),
    ("幼圆", &["pingfang sc", "youyuan", "noto sans cjk sc", "source han sans sc"]),
    ("youyuan", &["幼圆", "pingfang sc", "noto sans cjk sc", "source han sans sc"]),
    ("华文楷体", &["kaiti sc", "stkaiti", "kaiti", "simkai", "noto serif cjk sc", "source han serif sc"]),
    ("楷体_gb2312", &["simkai", "kaiti sc", "stkaiti", "kaiti", "noto serif cjk sc", "source han serif sc"]),
    ("华文仿宋", &["stfangsong", "fangsong", "simfang", "songti sc", "simsun"]),
    ("仿宋_gb2312", &["simfang", "stfangsong", "fangsong", "songti sc", "simsun"]),
];

/// File stems of the stroke fonts of Chinese CAD drawings. Those drawings set
/// Latin and CJK in the same SHX face; the unified Song face (see
/// [`song_face`]) keeps both scripts in one filled family instead of hairline
/// strokes next to filled TTF text. Latin SHX names (`txt`, `simplex`,
/// `romans`) are not here: they keep the built-in stroke font.
const CJK_SHX_STEMS: &[&str] = &["hztxt", "gbcbig", "hzfs", "hzkt", "hzst", "hzsong", "tssdchn", "gbhzfs", "gbfts", "stzhongsong"];

/// Whether `c` is a CJK ideograph (the common and extension-A blocks).
fn is_cjk(c: char) -> bool {
    ('\u{3400}'..='\u{4DBF}').contains(&c) || ('\u{4E00}'..='\u{9FFF}').contains(&c)
}

/// Whether `stem` (a `key()`-normalised name without any ".shx") is one of the
/// Chinese SHX stroke fonts ([`CJK_SHX_STEMS`]).
pub(crate) fn is_cjk_shx(stem: &str) -> bool {
    CJK_SHX_STEMS.contains(&stem)
}

/// Installed-equivalent names for `k` (a `key()`-normalised name), if it is a
/// well-known name with equivalents. Single-level: candidates are looked up
/// directly, never re-aliased, so the chain cannot loop.
fn alias_keys(k: &str) -> &'static [&'static str] {
    FONT_ALIASES.iter().find(|(n, _)| *n == k).map(|(_, c)| *c).unwrap_or(&[])
}

/// One name lookup with the resolved-name cache (found or not) applied.
fn lookup_cached(g: &mut Db, cand: &str) -> Option<Face> {
    if let Some(r) = g.resolved.get(cand) {
        return r.clone();
    }
    let r = g.lookup(cand);
    if g.resolved.len() >= MAX_CACHED_NAMES {
        g.resolved.clear();
    }
    g.resolved.insert(cand.to_string(), r.clone());
    r
}

/// The unified Song face: the first installed equivalent of SimSun (Songti SC
/// on macOS, Noto Serif CJK elsewhere, SimSun itself on Windows).
fn song_face(g: &mut Db) -> Option<Face> {
    for cand in std::iter::once("simsun").chain(alias_keys("simsun").iter().copied()) {
        if let Some(f) = lookup_cached(g, cand) {
            return Some(f);
        }
    }
    None
}

/// A face of an installed (or registered) font by file name or family name. Stroke-font names
/// (".shx", empty) return `None` — except the Chinese SHX fonts ([`CJK_SHX_STEMS`]) and,
/// when the named font is missing, any name written in Chinese: both resolve to the unified
/// Song face, so a Chinese drawing's text is one filled family on every machine.
pub fn find(name: &str) -> Option<Face> {
    let k = key(name);
    let mut g = lock();
    let stem = k.strip_suffix(".shx").unwrap_or(&k);
    if CJK_SHX_STEMS.contains(&stem) {
        return song_face(&mut g);
    }
    if is_stroke_name(&k) {
        return None;
    }
    let aliases = alias_keys(&k);
    for cand in std::iter::once(k.clone()).chain(aliases.iter().map(|s| s.to_string())) {
        if let Some(f) = lookup_cached(&mut g, &cand) {
            return Some(f);
        }
    }
    if k.chars().any(is_cjk) {
        return song_face(&mut g);
    }
    None
}

/// Resolve the unified families once, synchronously: the system scan and the
/// first face loads happen now (at app startup), not inside the first
/// display-list build, so the first frame already shows final glyphs.
pub fn warm_up() {
    let _ = find("simsun");
    let _ = find("simhei");
    let _ = fallback_for('\u{6587}');
}

struct Prefs {
    alt: String,
    fallback: Vec<String>,
}

fn prefs() -> MutexGuard<'static, Prefs> {
    static P: OnceLock<Mutex<Prefs>> = OnceLock::new();
    P.get_or_init(|| {
        #[cfg(not(target_arch = "wasm32"))]
        let env = |k: &str| std::env::var(k).unwrap_or_default();
        #[cfg(target_arch = "wasm32")]
        let env = |_: &str| String::new();
        Mutex::new(Prefs { alt: clean_name(&env("CADCRAFT_FONTALT")), fallback: split_list(&env("CADCRAFT_FONTFALLBACK")) })
    })
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
}

/// A font name setting: trimmed, capped, "." = none.
fn clean_name(s: &str) -> String {
    let s = s.trim();
    if s == "." { String::new() } else { s.chars().take(255).collect() }
}

fn split_list(s: &str) -> Vec<String> {
    s.split([',', ';']).map(clean_name).filter(|n| !n.is_empty()).take(16).collect()
}

/// FONTALT: the font used for a text style font that can't be found. Empty = the built-in
/// stroke font.
pub fn font_alt() -> String {
    prefs().alt.clone()
}

/// Set FONTALT ("" or "." clears it).
pub fn set_font_alt(name: &str) {
    prefs().alt = clean_name(name);
}

/// FONTFALLBACK: fonts tried first for characters a text's font lacks, comma-separated.
pub fn fallback_fonts() -> String {
    prefs().fallback.join(", ")
}

/// Set FONTFALLBACK from a comma- or semicolon-separated list ("" or "." clears it).
pub fn set_fallback_fonts(list: &str) {
    prefs().fallback = split_list(list);
}

/// Characters worth a fallback search (not spaces, controls or private-use code points).
fn wants_fallback(c: char) -> bool {
    let u = u32::from(c);
    !(c.is_whitespace() || c.is_control() || (0xE000..=0xF8FF).contains(&u) || u >= 0xF_0000 || (u & 0xFFFE) == 0xFFFE)
}

/// A face that has `c`, for text whose own font lacks it: FONTFALLBACK fonts, then FONTALT,
/// then installed fonts (wide-coverage CJK fonts first). Bounded and cached.
pub fn fallback_for(c: char) -> Option<Face> {
    if !wants_fallback(c) {
        return None;
    }
    let (list, alt) = {
        let p = prefs();
        (p.fallback.clone(), p.alt.clone())
    };
    for n in list.iter().chain(std::iter::once(&alt)) {
        if let Some(f) = find(n).filter(|f| f.has_glyph(c)) {
            return Some(f);
        }
    }
    let mut g = lock();
    let i = g.search_fallback(c)?;
    g.fallback.faces.get(i).cloned()
}

/// A glyph outline in font units (at 1000 units per em), flattened, plus its advance and the
/// font's cap height.
#[derive(Default)]
pub(crate) struct GlyphOutline {
    pub(crate) contours: Arc<[Vec<Vec2>]>,
    pub(crate) advance: f64,
    pub(crate) cap: f64,
    /// The font maps the character (false: the missing-glyph shape).
    mapped: bool,
    /// Its glyph id and face, for output that keeps text as text ([`crate::TextGlyph`]).
    gid: u32,
    face: Option<Face>,
}

/// A glyph by character (through the face's character map) or by glyph id (shaper output).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum GlyphKey {
    Char(char),
    Id(u32),
}

/// Cached glyph outlines keyed by (font bytes address, length, face, glyph). Fonts live for the
/// whole process in the font table, so the address identifies them. A read-write lock: display
/// lists are built on all cores and nearly every lookup is a hit, which a mutex serialized.
type GlyphCache = HashMap<(usize, usize, u32, GlyphKey), Arc<GlyphOutline>>;

fn glyph_cache() -> &'static RwLock<GlyphCache> {
    static C: OnceLock<RwLock<GlyphCache>> = OnceLock::new();
    C.get_or_init(|| RwLock::new(HashMap::new()))
}

const MAX_CACHED_GLYPHS: usize = 40_000;
const UPEM: f32 = 1000.0;

struct Pen {
    contours: Vec<Vec<Vec2>>,
    cur: Vec<Vec2>,
}

impl Pen {
    fn pt(x: f32, y: f32) -> Vec2 {
        Vec2::new(f64::from(x), f64::from(y))
    }
    fn flush(&mut self) {
        if self.cur.len() > 2 {
            if let (Some(f), Some(l)) = (self.cur.first().copied(), self.cur.last().copied())
                && f != l
            {
                self.cur.push(f);
            }
            self.contours.push(std::mem::take(&mut self.cur));
        } else {
            self.cur.clear();
        }
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.flush();
        self.cur.push(Self::pt(x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.cur.push(Self::pt(x, y));
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let a = self.cur.last().copied().unwrap_or_default();
        let c = Self::pt(cx0, cy0);
        let b = Self::pt(x, y);
        for i in 1..=6 {
            let t = f64::from(i) / 6.0;
            let u = 1.0 - t;
            self.cur.push(a * (u * u) + c * (2.0 * u * t) + b * (t * t));
        }
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let a = self.cur.last().copied().unwrap_or_default();
        let c0 = Self::pt(cx0, cy0);
        let c1 = Self::pt(cx1, cy1);
        let b = Self::pt(x, y);
        for i in 1..=8 {
            let t = f64::from(i) / 8.0;
            let u = 1.0 - t;
            self.cur.push(a * (u * u * u) + c0 * (3.0 * u * u * t) + c1 * (3.0 * u * t * t) + b * (t * t * t));
        }
    }
    fn close(&mut self) {
        self.flush();
    }
}

/// Cap height of a font in font units at 1000 units per em: the OS/2 value, else the height
/// of its "H", else an estimate from the ascent.
fn cap_height(f: &FontRef) -> f64 {
    let metrics = f.metrics(Size::new(UPEM), LocationRef::default());
    let measured = || {
        let gid = f.charmap().map('H')?;
        let b = f.glyph_metrics(Size::new(UPEM), LocationRef::default()).bounds(gid)?;
        Some(b.y_max).filter(|y| y.is_finite() && *y > 0.0)
    };
    let cap = metrics.cap_height.filter(|c| c.is_finite() && *c > 0.0).or_else(measured).unwrap_or(metrics.ascent * 0.72);
    f64::from(cap).max(1.0)
}

/// The outline of `c` in `face` (its missing-glyph shape when unmapped; empty when the face
/// doesn't parse).
pub(crate) fn glyph(face: &Face, c: char) -> Arc<GlyphOutline> {
    outline(face, GlyphKey::Char(c))
}

/// The outline of glyph `gid` of `face` (as a shaper returns it); its advance is the font's
/// nominal one.
pub(crate) fn glyph_by_id(face: &Face, gid: u32) -> Arc<GlyphOutline> {
    outline(face, GlyphKey::Id(gid))
}

/// Closed, flattened contours of a glyph at 1000 font units per em. Shared with the outline
/// cache so renderers can tessellate once before applying each occurrence's affine placement.
pub fn glyph_contours(face: &Face, gid: u16) -> Arc<[Vec<Vec2>]> {
    glyph_by_id(face, u32::from(gid)).contours.clone()
}

fn outline(face: &Face, k: GlyphKey) -> Arc<GlyphOutline> {
    let key = (face.bytes.as_ptr() as usize, face.bytes.len(), face.index, k);
    if let Some(g) = glyph_cache().read().unwrap_or_else(PoisonError::into_inner).get(&key) {
        return g.clone();
    }
    let Some(f) = face.font() else { return Arc::new(GlyphOutline { cap: 1.0, ..Default::default() }) };
    let mapped = match k {
        GlyphKey::Char(c) => f.charmap().map(c),
        GlyphKey::Id(id) => Some(GlyphId::new(id)),
    };
    let gid = mapped.unwrap_or_default();
    let mut pen = Pen { contours: Vec::new(), cur: Vec::new() };
    if let Some(g) = f.outline_glyphs().get(gid) {
        let _ = g.draw(DrawSettings::unhinted(Size::new(UPEM), LocationRef::default()), &mut pen);
        pen.flush();
    }
    let advance = f64::from(f.glyph_metrics(Size::new(UPEM), LocationRef::default()).advance_width(gid).unwrap_or(UPEM * 0.5));
    let (cap, mapped, gid) = (cap_height(&f), mapped.is_some(), gid.to_u32());
    let g = Arc::new(GlyphOutline { contours: pen.contours.into(), advance, cap, mapped, gid, face: Some(face.clone()) });
    let mut cache = glyph_cache().write().unwrap_or_else(PoisonError::into_inner);
    if cache.len() > MAX_CACHED_GLYPHS {
        cache.clear();
    }
    cache.insert(key, g.clone());
    g
}

/// The outline of `c` from a fallback font, if any installed font has it.
pub(crate) fn fallback_glyph(c: char) -> Option<Arc<GlyphOutline>> {
    let f = fallback_for(c)?;
    Some(glyph(&f, c)).filter(|g| g.mapped)
}

/// The glyph of `c` in `face`, or from a fallback font when `face` lacks it.
fn glyph_or_fallback(face: &Face, c: char) -> Arc<GlyphOutline> {
    let g = glyph(face, c);
    if g.mapped || c.is_whitespace() {
        return g;
    }
    fallback_glyph(c).unwrap_or(g)
}

/// The [`crate::TextGlyph`] of a glyph placed like [`place_glyph`] does (its outline index is
/// set when it is added). `None` for the missing-glyph shape.
pub(crate) fn text_glyph(g: &GlyphOutline, c: char, x: f64, h: f64, wf: f64, shear: f64) -> Option<crate::TextGlyph> {
    // Font units at 1000 per em scale by h / cap.
    text_glyph_at(g, c, Vec2::new(x, 0.0), h / g.cap, wf, shear)
}

/// [`text_glyph`] with the glyph's origin at `origin` and font units scaled by `scale`.
pub(crate) fn text_glyph_at(g: &GlyphOutline, c: char, origin: Vec2, scale: f64, wf: f64, shear: f64) -> Option<crate::TextGlyph> {
    let gid = u16::try_from(g.gid).ok().filter(|_| g.mapped)?;
    let face = g.face.clone()?;
    let em = 1000.0 * scale;
    (em.is_finite() && em > 0.0).then(|| crate::TextGlyph {
        face,
        gid,
        ch: c,
        outline: 0,
        origin,
        x: Vec2::new(em * wf, 0.0),
        y: Vec2::new(em * shear, em),
    })
}

/// Contours of a glyph scaled so its font's cap height is `h`, at pen position `x`.
pub(crate) fn place_glyph(g: &GlyphOutline, x: f64, h: f64, wf: f64, shear: f64) -> Vec<Vec<Vec2>> {
    place_glyph_at(g, Vec2::new(x, 0.0), h / g.cap, wf, shear)
}

/// Contours of a glyph with its origin at `origin` and font units scaled by `scale`; the shear
/// slants about the baseline (y = 0).
pub(crate) fn place_glyph_at(g: &GlyphOutline, origin: Vec2, scale: f64, wf: f64, shear: f64) -> Vec<Vec<Vec2>> {
    g.contours
        .iter()
        .map(|ct| {
            ct.iter()
                .map(|p| {
                    let y = origin.y + p.y * scale;
                    Vec2::new(origin.x + p.x * scale * wf + y * shear, y)
                })
                .collect()
        })
        .collect()
}

/// Shape one line with a TrueType face: closed glyph contours grouped per glyph (baseline at
/// y = 0, x from 0) plus `%%u`/`%%o` decorations as strokes. Text height = cap height.
/// Characters the face lacks come from a fallback font, scaled to its own cap height.
pub fn shape(face: &Face, s: &str, height: f64, width_factor: f64, oblique: f64) -> Option<Shaped> {
    face.font()?;
    if crate::bidi::has_rtl(s)
        && let Some(sh) = crate::complex::shape(Some(face), s, height, width_factor, oblique)
    {
        return Some(sh);
    }
    let h = if height.is_finite() && height > 0.0 { height } else { 1.0 };
    let wf = if width_factor.is_finite() && width_factor.abs() > 1e-6 { width_factor } else { 1.0 };
    let shear = oblique.tan().clamp(-10.0, 10.0);
    let mut out = Shaped::default();
    let mut spans = Vec::new();
    let mut x = 0.0;
    for (c, under, over) in crate::decode_controls(s) {
        if crate::bidi::is_format_control(c) {
            continue;
        }
        let g = glyph_or_fallback(face, c);
        if !c.is_whitespace() {
            let contours = place_glyph(&g, x, h, wf, shear);
            if !contours.is_empty() {
                out.push_glyph(contours, text_glyph(&g, c, x, h, wf, shear));
            }
        }
        let adv = g.advance * h / g.cap * wf;
        spans.push((x, x + adv, under, over));
        x += adv;
    }
    out.strokes.extend(crate::decorations(&spans, h));
    out.width = x;
    Some(out)
}

/// Width of one line set in a TrueType face.
pub fn width(face: &Face, s: &str, height: f64, width_factor: f64) -> Option<f64> {
    face.font()?;
    if crate::bidi::has_rtl(s)
        && let Some(sh) = crate::complex::shape(Some(face), s, height, width_factor, 0.0)
    {
        return Some(sh.width);
    }
    let wf = if width_factor.is_finite() && width_factor.abs() > 1e-6 { width_factor } else { 1.0 };
    Some(
        crate::decode_controls(s)
            .iter()
            .filter(|(c, _, _)| !crate::bidi::is_format_control(*c))
            .map(|(c, _, _)| {
                let g = glyph_or_fallback(face, *c);
                g.advance * height / g.cap * wf
            })
            .sum(),
    )
}

/// Lay out one line with a TrueType face: closed glyph contours (baseline at y = 0).
pub fn layout_line(face: &Face, s: &str, height: f64, width_factor: f64, oblique: f64) -> Option<Run> {
    let sh = shape(face, s, height, width_factor, oblique)?;
    let mut strokes: Vec<Vec<Vec2>> = sh.glyphs.into_iter().flatten().collect();
    strokes.extend(sh.strokes);
    Some(Run { strokes, width: sh.width })
}

#[cfg(test)]
mod family_tests {
    use super::*;

    #[test]
    fn alias_table_is_single_level_and_covers_the_four_families() {
        // Every alias list is non-empty, never contains its own key (no self-loop),
        // and the chain is single-level by construction (find() never re-aliases).
        for (k, v) in FONT_ALIASES {
            assert!(!v.is_empty(), "{k} has no aliases");
            assert!(!v.contains(k), "{k} aliases to itself");
        }
        assert!(alias_keys("simsun").contains(&"songti sc"));
        assert!(alias_keys("宋体").contains(&"songti sc"));
        assert!(alias_keys("songti sc").contains(&"simsun"));
        assert!(alias_keys("simhei").contains(&"heiti sc"));
        assert!(alias_keys("黑体").contains(&"heiti sc"));
        assert!(alias_keys("simkai").contains(&"kaiti sc"));
        assert!(alias_keys("楷体").contains(&"kaiti sc"));
        assert!(alias_keys("simfang").contains(&"stfangsong"));
        assert!(alias_keys("仿宋").contains(&"stfangsong"));
        assert!(alias_keys("arial").is_empty());
        assert!(alias_keys("txt").is_empty());
    }

    /// A SimSun-styled drawing on a machine without SimSun (macOS) must resolve
    /// to the installed Songti SC equivalent and shape ASCII as filled outlines,
    /// not as single-stroke hairlines — the "only Chinese looks right" split.
    #[test]
    fn simsun_resolves_to_registered_songti_sc_and_shapes_ascii_as_outlines() {
        let Ok(regular) = std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf") else { return };
        let Ok(bold) = std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf") else { return };
        // (a) SimSun missing: the Songti SC equivalent answers.
        crate::ttf::register("Songti SC", regular.clone());
        let face = crate::ttf::find("SimSun").expect("SimSun should alias to Songti SC");
        assert_eq!(face.bytes.as_slice(), regular.as_slice());
        let face_cn = crate::ttf::find("宋体").expect("宋体 should alias to Songti SC");
        assert_eq!(face_cn.bytes.as_slice(), regular.as_slice());
        assert!(crate::TextFont::resolve("SimSun").is_outline(), "SimSun style must be an outline font");
        // (b) An installed SimSun still wins over its aliases.
        crate::ttf::register("SimSun", bold.clone());
        crate::ttf::register("Songti SC", regular.clone());
        let face = crate::ttf::find("SimSun").expect("installed SimSun");
        assert_eq!(face.bytes.as_slice(), bold.as_slice());
        // (c) ASCII shaped with the resolved face is filled outlines, not strokes.
        let sh = crate::ttf::shape(&face, "GYTS-6B1", 1.0, 1.0, 0.0).expect("shape");
        assert_eq!(sh.glyphs.len(), 8, "one filled outline per ASCII char");
        assert!(sh.glyphs.iter().all(|g| !g.is_empty()), "every ASCII glyph has contours");
        assert!(sh.strokes.is_empty(), "no stroke-font hairlines for ASCII");
        let stroke = crate::shape_stroke("GYTS-6B1", 1.0, 1.0, 0.0);
        assert!(!stroke.strokes.is_empty() && stroke.glyphs.is_empty(), "stroke font is the old broken path");
    }

    #[test]
    fn alias_table_covers_office_and_st_font_names() {
        assert!(alias_keys("微软雅黑").contains(&"pingfang sc"));
        assert!(alias_keys("microsoft yahei").contains(&"pingfang sc"));
        assert!(alias_keys("等线").contains(&"pingfang sc"));
        assert!(alias_keys("dengxian").contains(&"pingfang sc"));
        assert!(alias_keys("华文宋体").contains(&"songti sc"));
        assert!(alias_keys("华文中宋").contains(&"songti sc"));
        assert!(alias_keys("新宋体").contains(&"simsun"));
        assert!(alias_keys("华文楷体").contains(&"kaiti sc"));
        assert!(alias_keys("楷体_gb2312").contains(&"kaiti sc"));
        assert!(alias_keys("华文仿宋").contains(&"stfangsong"));
        assert!(alias_keys("仿宋_gb2312").contains(&"stfangsong"));
        assert!(alias_keys("华文黑体").contains(&"heiti sc"));
    }

    /// Chinese SHX stroke fonts and unknown Chinese font names resolve to the
    /// unified Song face (filled outlines), while Latin SHX names keep the
    /// stroke font.
    #[test]
    fn chinese_shx_and_unknown_chinese_names_resolve_to_song() {
        let Ok(regular) = std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf") else { return };
        crate::ttf::register("Songti SC", regular.clone());
        for name in ["hztxt.shx", "gbcbig.shx", "HZFS.SHX", "某不知名字体", "方正小标宋"] {
            let face = crate::ttf::find(name).unwrap_or_else(|| panic!("{name} should resolve to the Song face"));
            assert_eq!(face.bytes.as_slice(), regular.as_slice(), "{name}");
            assert!(crate::TextFont::resolve(name).is_outline(), "{name} must be an outline font");
        }
        assert!(crate::ttf::find("txt.shx").is_none(), "Latin SHX keeps the stroke font");
        assert!(crate::ttf::find("simplex.shx").is_none());
        crate::ttf::warm_up();
    }

    /// A style pairing a Latin stroke font with a Chinese big font (the sample
    /// drawing's `Standard` = txt + gbcbig.shx, `HZ` = TXT + HZTXT) resolves to
    /// the unified Song face; without a Chinese big font the stroke font stays.
    #[test]
    fn chinese_bigfont_styles_resolve_to_song() {
        let Ok(regular) = std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf") else { return };
        crate::ttf::register("Songti SC", regular.clone());
        assert!(crate::TextFont::resolve_styled("txt", "gbcbig.shx").is_outline());
        assert!(crate::TextFont::resolve_styled("TXT", "HZTXT").is_outline());
        assert!(crate::TextFont::resolve_styled("hztxt.shx", "").is_outline());
        assert!(!crate::TextFont::resolve_styled("txt", "").is_outline(), "no Chinese big font: stroke stays");
        assert!(!crate::TextFont::resolve_styled("romans.shx", "").is_outline());
    }

    /// A fallback miss is not cached: the character is searched again next time
    /// (a font registered or installed later can still cover it), and a cached
    /// entry is always a found face.
    #[test]
    fn fallback_miss_is_not_cached() {
        // U+FFF0 (a "specials" code point) is in no installed font: a certain miss.
        let c = '\u{FFF0}';
        assert!(crate::ttf::fallback_for(c).is_none());
        assert!(crate::ttf::fallback_for(c).is_none(), "a miss stays a miss, without poisoning the cache");
        let g = lock();
        assert!(!g.fallback.by_char.contains_key(&c), "the miss must not be cached");
        assert!(g.fallback.by_char.values().all(Option::is_some), "cached entries are found faces only");
    }

    #[test]
    fn a_family_is_looked_for_in_files_starting_with_its_first_word() {
        let stems: Vec<String> = ["gost", "gost2304a", "gost common", "arial", "isocpeur"].map(String::from).to_vec();
        let c = family_candidates(&stems, &norm("gost type a"), "gost");
        assert!(c.contains(&"gost".to_string()), "GOST.TTF holds \"GOST type A\": {c:?}");
        assert!(!c.contains(&"arial".to_string()));
        assert!(family_candidates(&stems, &norm("gost type a"), "go").is_empty(), "too short a word matches nothing");
        let many: Vec<String> = (0..100).map(|i| format!("gost{i:03}")).collect();
        assert!(family_candidates(&many, "gosttypea", "gost").len() <= MAX_FAMILY_FILES + MAX_FIRST_WORD_FILES);
    }
}
