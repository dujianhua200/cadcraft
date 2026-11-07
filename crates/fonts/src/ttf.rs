//! TrueType/OpenType text: system fonts found at run time (never bundled), glyph outlines as
//! closed polylines in CAD text units (text height = cap height).
//!
//! Fonts are found by file stem (`arial.ttf`, `msjh.ttc`) and, failing that, by family name
//! (`Noto Sans CJK TC`) among installed files with a similar name. TrueType collections
//! (`.ttc` / `.otc`) are scanned too; a stem lookup takes their first face.
//!
//! System fonts are searched in the platform's own directories: `/System/Library/Fonts` and
//! friends on macOS, `C:\Windows\Fonts` on Windows, `/system/fonts` and the product/vendor
//! partitions on Android and HarmonyOS, and the XDG directories elsewhere. Web builds have no
//! system fonts.
//!
//! Two process-wide settings (like AutoCAD's profile variables) control substitution:
//! - [`font_alt`] (`FONTALT`): the font used for a text style font that can't be found (an SHX
//!   file we don't read, a missing TTF). Empty = the built-in stroke font.
//! - [`fallback_fonts`] (`FONTFALLBACK`): fonts tried first for characters the text's font
//!   lacks (CJK in an SHX big font, for example), before the automatic system font search.
//!
//! Both start empty; the host sets its starting values ([`set_font_alt`], [`set_fallback_fonts`]).
//!
//! Besides the system font folders, the user's own font folders ([`set_font_dirs`], Options ▸
//! Fonts) are searched first: a font placed there wins over an installed one with the same file
//! name.

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
    /// The family name, as the font gives it (the typographic family, else the family when that
    /// is missing or blank).
    pub fn family_name(&self) -> Option<String> {
        let f = self.font()?;
        [skrifa::string::StringId::TYPOGRAPHIC_FAMILY_NAME, skrifa::string::StringId::FAMILY_NAME].into_iter().find_map(|id| {
            let s = f.localized_strings(id).next()?.to_string();
            Some(s.chars().filter(|c| !c.is_control()).take(80).collect::<String>()).filter(|s| !s.trim().is_empty())
        })
    }

    /// The glyph ids the face's character map gives `chars` (`None` for an unmapped one).
    pub(crate) fn glyph_ids(&self, chars: &[char]) -> Vec<Option<u32>> {
        let Some(f) = self.font() else { return vec![None; chars.len()] };
        let map = f.charmap();
        chars.iter().map(|c| map.map(*c).map(|g| g.to_u32())).collect()
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
/// Cumulative outline points one shaped line may retain, so an oversized expanded glyph repeated
/// across a string cannot grow the result without bound (CWE-400).
pub(crate) const MAX_SHAPED_POINTS: usize = 8_000_000;

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
/// Fonts known to cover Thai, with the OpenType mark positioning its stacked vowels and tone
/// marks need. First the Windows faces Thai drawings are laid out with (Cordia New, Angsana New,
/// Browallia New: Thai SHX fonts such as `cordia2.shx` copy their design and widths, so text
/// keeps its fit in title blocks and tables), then IBM Plex Sans Thai (installed, or embedded
/// from craft-fonts, which makes it the web build's Thai font), then Noto and the fonts each OS
/// ships (macOS Thonburi and Sukhumvit, Windows Leelawadee and Tahoma, the Linux TLWG fonts).
/// Matched as parts of file names: `angsa` and `browa` find both today's Windows collections
/// (`angsana.ttc`, `browalia.ttc`) and the older single faces (`angsa.ttf`, `browab.ttf`).
const THAI_FALLBACK: &[&str] = &[
    "cordia",
    "angsa",
    "browa",
    "ibmplexsansthai",
    "notosansthai",
    "notoserifthai",
    "thonburi",
    "sukhumvit",
    "leelawui",
    "leelawadeeui",
    "leelawad",
    "tahoma",
    "kinnari",
    "norasi",
    "garuda",
    "loma",
    "waree",
    "sarabun",
];
/// Script-specific fallback files loaded per character, at most.
const MAX_SCRIPT_FILES: usize = 4;

/// The font file stems matching a script's fallback `names`, best first: regular weights before
/// bold, light and the like, then in the order of `names`, then by name.
fn script_candidates(stems: &[String], names: &[&str]) -> Vec<String> {
    let mut v: Vec<(usize, usize, String)> = stems
        .iter()
        .filter_map(|s| {
            let n = norm(s);
            names.iter().position(|p| n.contains(p)).map(|pref| (weight_penalty(s, ""), pref, s.clone()))
        })
        .collect();
    v.sort();
    v.dedup_by(|a, b| a.2 == b.2);
    v.into_iter().map(|(_, _, s)| s).collect()
}

/// Script-specific fallback font names for `c`, if any.
fn script_fallback(c: char) -> &'static [&'static str] {
    match c {
        '\u{0600}'..='\u{08FF}' | '\u{FB50}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFF}' => ARABIC_FALLBACK,
        '\u{0590}'..='\u{05FF}' | '\u{FB1D}'..='\u{FB4F}' => HEBREW_FALLBACK,
        '\u{0E00}'..='\u{0E7F}' => THAI_FALLBACK,
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
    /// The keys of `files` given by [`register`]: kept over a file of the same stem, and by a
    /// rescan.
    registered: std::collections::HashSet<String>,
    /// Name lookups already answered, found or not.
    resolved: HashMap<String, Option<Face>>,
    fallback: Fallback,
    /// Bytes of font files loaded from disk so far (bounded by `MAX_LOADED_BYTES`).
    loaded_bytes: usize,
    /// Stems of the fonts found in the user's font folders.
    user: std::collections::HashSet<String>,
    /// Names read from user font files for lists and family searches, by stem. Reading them
    /// does not keep the file's bytes, so it does not use up `MAX_LOADED_BYTES`.
    face_names: HashMap<String, Arc<FaceNames>>,
}

/// The names inside one font file.
#[derive(Default)]
struct FaceNames {
    /// The first face's family name.
    family: Option<String>,
    /// Each face's normalized names (family, typographic family, full name).
    faces: Vec<Vec<String>>,
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
        let user = scan_dirs(&user_dirs(), true);
        g.user = user.keys().cloned().collect();
        let mut all = scan();
        all.extend(user);
        g.paths = all.into_iter().map(|(stem, p)| (stem, Source::File(p))).collect();
        // Embedded craft-fonts fonts take the place of an installed file with the same stem,
        // but not of a font in the user's own folders (which they chose to use).
        for f in crate::CRAFT_FONTS {
            let stem = f.stem();
            if !g.user.contains(&stem) {
                g.paths.insert(stem, Source::Static(f.bytes));
            }
        }
    }
    g
}

/// The platform's system font directories, in search order.
#[cfg(not(target_arch = "wasm32"))]
fn system_font_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        dirs.extend(["/System/Library/Fonts", "/System/Library/Fonts/Supplemental", "/Library/Fonts"].map(Into::into));
        if let Some(h) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(h).join("Library/Fonts"));
        }
    } else if cfg!(any(target_os = "android", target_env = "ohos")) {
        // Android and HarmonyOS keep system fonts under /system/fonts; Android devices also use
        // the product and vendor partitions for faces of their own. An app may read all of them,
        // and directories a device lacks are skipped.
        dirs.extend(["/system/fonts", "/product/fonts", "/system_ext/fonts", "/vendor/fonts"].map(Into::into));
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
    dirs
}

/// Every font file under [`system_font_dirs`], by lower-case file stem.
#[cfg(not(target_arch = "wasm32"))]
fn scan() -> HashMap<String, PathBuf> {
    scan_dirs(&system_font_dirs(), false)
}

/// Font files under `dirs` by lower-case stem; the first file found with a stem wins
/// (`in_order`: a folder listed earlier wins over a later one).
#[cfg(not(target_arch = "wasm32"))]
fn scan_dirs(dirs: &[PathBuf], in_order: bool) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    let mut stack: Vec<PathBuf> = if in_order { dirs.iter().rev().cloned().collect() } else { dirs.to_vec() };
    let mut seen = 0usize;
    let mut visited = std::collections::HashSet::new();
    while let Some(d) = stack.pop() {
        if !first_visit(&mut visited, &d) {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > 20_000 {
                return out;
            }
            let p = e.path();
            // Linked folders are followed (font folders often are links); `first_visit` keeps a
            // circle of links from reading a folder twice.
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

#[cfg(target_arch = "wasm32")]
fn scan_dirs(_dirs: &[PathBuf], _in_order: bool) -> HashMap<String, PathBuf> {
    HashMap::new()
}

/// Most user font folders.
pub const MAX_FONT_DIRS: usize = 16;
/// The longest font folder path kept.
const MAX_DIR_CHARS: usize = 1024;

fn dirs_lock() -> MutexGuard<'static, Vec<PathBuf>> {
    static DIRS: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();
    DIRS.get_or_init(|| Mutex::new(Vec::new())).lock().unwrap_or_else(PoisonError::into_inner)
}

fn user_dirs() -> Vec<PathBuf> {
    dirs_lock().clone()
}

/// Counts changes to the set of fonts ([`set_font_dirs`], [`rescan`]): text drawn before a change
/// may have been drawn with another font.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The font set's change count (see [`GENERATION`]).
pub fn generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// The user's font folders, searched before the system's.
pub fn font_dirs() -> Vec<String> {
    user_dirs().iter().map(|p| p.to_string_lossy().to_string()).collect()
}

/// Clean a font folder list: trimmed, without empty, repeated or over-long entries, at most
/// [`MAX_FONT_DIRS`].
pub fn clean_dirs<S: AsRef<str>>(dirs: &[S]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for d in dirs.iter().take(MAX_FONT_DIRS * 4) {
        let d = d.as_ref().trim();
        if d.is_empty() || d.chars().count() > MAX_DIR_CHARS || d.chars().any(char::is_control) || out.iter().any(|o| o == d) {
            continue;
        }
        out.push(d.to_string());
        if out.len() >= MAX_FONT_DIRS {
            break;
        }
    }
    out
}

/// Set the user's font folders (Options ▸ Fonts) and look for fonts again.
pub fn set_font_dirs<S: AsRef<str>>(dirs: &[S]) {
    let dirs: Vec<PathBuf> = clean_dirs(dirs).into_iter().map(PathBuf::from).collect();
    {
        let mut current = dirs_lock();
        if *current == dirs {
            return;
        }
        *current = dirs;
    }
    rescan();
}

/// Forget what was found and look for fonts again (files added to a font folder).
pub fn rescan() {
    let mut g = lock();
    // Fonts registered by name stay, also where a found file has the same stem; bytes read
    // from found files go.
    let paths = std::mem::take(&mut g.paths);
    let Db { files, registered, .. } = &mut *g;
    files.retain(|k, _| registered.contains(k) || !paths.contains_key(k));
    g.scanned = false;
    g.resolved.clear();
    g.fallback = Fallback::default();
    g.loaded_bytes = 0;
    g.user.clear();
    g.face_names.clear();
    drop(g);
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // SHX files in the same folders: the SHX font table looks there too.
    crate::shx::set_font_dirs(shx_dirs(&user_dirs()));
}

/// Whether folder `d` is read for the first time (by its real path: a link to a folder already
/// read, or a circle of links, is not read again).
#[cfg(not(target_arch = "wasm32"))]
fn first_visit(visited: &mut std::collections::HashSet<PathBuf>, d: &std::path::Path) -> bool {
    visited.insert(std::fs::canonicalize(d).unwrap_or_else(|_| d.to_path_buf()))
}

/// The folders under `dirs` (them included) that hold SHX files, in order, bounded like the
/// font scan.
#[cfg(not(target_arch = "wasm32"))]
fn shx_dirs(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut stack: Vec<PathBuf> = dirs.iter().rev().cloned().collect();
    let mut seen = 0usize;
    let mut visited = std::collections::HashSet::new();
    while let Some(d) = stack.pop() {
        if !first_visit(&mut visited, &d) {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut subs = Vec::new();
        let mut has_shx = false;
        for e in rd.flatten() {
            seen += 1;
            if seen > 20_000 {
                return out;
            }
            if e.path().is_dir() {
                subs.push(e.path());
            } else if e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("shx")) {
                has_shx = true;
            }
        }
        if has_shx && !out.contains(&d) {
            out.push(d);
        }
        stack.extend(subs.into_iter().rev());
    }
    out
}

#[cfg(target_arch = "wasm32")]
fn shx_dirs(_dirs: &[PathBuf]) -> Vec<PathBuf> {
    Vec::new()
}

/// A font file CADCraft can use, for font lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontFile {
    /// The name a text style stores: the file name (`arial.ttf`), or the name of a font built
    /// into CADCraft.
    pub name: String,
    /// The family name inside the file, read for fonts in the user's folders.
    pub family: Option<String>,
    /// Found in one of the user's font folders.
    pub user: bool,
}

/// The fonts CADCraft can use: those in the user's folders first (with their family names),
/// then the installed and built-in ones, by name.
pub fn installed() -> Vec<FontFile> {
    /// Fonts whose family name is read for the list.
    const MAX_NAMED: usize = 200;
    let mut g = lock();
    let mut list: Vec<(String, String, bool)> = g
        .paths
        .iter()
        .map(|(stem, src)| {
            let name = match src {
                Source::File(p) => p.file_name().map_or_else(|| stem.clone(), |n| n.to_string_lossy().to_string()),
                Source::Static(_) => stem.clone(),
            };
            (stem.clone(), name, g.user.contains(stem))
        })
        .collect();
    list.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    let mut named = 0usize;
    list.into_iter()
        .map(|(stem, name, user)| {
            let family = if user && named < MAX_NAMED {
                named += 1;
                g.face_names(&stem).and_then(|n| n.family.clone())
            } else {
                None
            };
            FontFile { name, family, user }
        })
        .collect()
}

/// Whether a text style font is drawn as named: a font CADCraft found, or its own stroke font.
/// Other names (an SHX file, a missing TTF) are drawn with a substitute ([`crate::TextFont::resolve`]).
pub fn available(name: &str) -> bool {
    let k = key(name);
    // The built-in stroke font is CADCraft's own stand-in for the basic stroke fonts.
    let stand_in = matches!(k.trim_end_matches(".shx"), "txt" | "simplex" | "romans");
    k.is_empty() || stand_in || k == crate::BUILTIN_FONT.to_ascii_lowercase() || crate::shx::find(name).is_some() || find(name).is_some()
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
        let bytes = self.peek(stem)?;
        self.loaded_bytes = self.loaded_bytes.saturating_add(bytes.len());
        self.files.insert(stem.to_string(), bytes.clone());
        Some(bytes)
    }

    /// Bytes of the font file with this stem, without keeping them or charging the load budget.
    fn peek(&self, stem: &str) -> Option<Arc<Vec<u8>>> {
        if let Some(b) = self.files.get(stem) {
            return Some(b.clone());
        }
        Some(Arc::new(match self.paths.get(stem)? {
            Source::File(path) => read_file(path)?,
            Source::Static(bytes) => bytes.to_vec(),
        }))
    }

    /// The names inside the font file with this stem, read once (the bytes are dropped after).
    fn face_names(&mut self, stem: &str) -> Option<Arc<FaceNames>> {
        if let Some(n) = self.face_names.get(stem) {
            return Some(n.clone());
        }
        let bytes = self.peek(stem)?;
        let all = faces(&bytes);
        let names = Arc::new(FaceNames { family: all.first().and_then(Face::family_name), faces: all.iter().map(Face::names).collect() });
        self.face_names.insert(stem.to_string(), names.clone());
        Some(names)
    }

    fn lookup(&mut self, k: &str) -> Option<Face> {
        let candidates = [k.to_string(), k.replace(' ', ""), format!("{k} regular"), format!("{}-regular", k.replace(' ', ""))];
        for c in &candidates {
            if let Some(face) = self.file(c).and_then(|b| faces(&b).into_iter().next()) {
                return Some(face);
            }
        }
        self.by_family(k).or_else(|| self.user_family(k))
    }

    /// A face in the user's font folders whose family or full name is `k`, whatever its file is
    /// called (the folders are small; at most 200 files are looked at).
    fn user_family(&mut self, k: &str) -> Option<Face> {
        let want = norm(k);
        if want.is_empty() {
            return None;
        }
        let mut stems: Vec<String> = self.user.iter().cloned().collect();
        stems.sort();
        for stem in stems.into_iter().take(200) {
            let Some(names) = self.face_names(&stem) else { continue };
            let Some(i) = names.faces.iter().position(|f| f.iter().any(|n| *n == want || format!("{n}regular") == want)) else { continue };
            // Only the file that has the face is kept, charged to the load budget like any other
            // (once it is used up, no more faces are loaded). A file that cannot be read now
            // (deleted, too large) does not end the search: another file may hold the family.
            let Some(bytes) = self.file(&stem) else { continue };
            return faces(&bytes).into_iter().nth(i);
        }
        None
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
        let stems: Vec<String> = self.paths.keys().chain(self.files.keys()).cloned().collect();
        for stem in script_candidates(&stems, names).into_iter().take(MAX_SCRIPT_FILES) {
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
    let k = key(name);
    g.files.insert(k.clone(), Arc::new(bytes));
    g.registered.insert(k);
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
    g.registered.remove(&k);
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
    static P: Mutex<Prefs> = Mutex::new(Prefs { alt: String::new(), fallback: Vec::new() });
    P.lock().unwrap_or_else(PoisonError::into_inner)
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
    points: usize,
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
#[derive(Default)]
struct GlyphCache {
    glyphs: HashMap<(usize, usize, u32, GlyphKey), Arc<GlyphOutline>>,
    points: usize,
}

/// Return the process-wide outline cache whose retained glyph and point counts are bounded during insertion.
fn glyph_cache() -> &'static RwLock<GlyphCache> {
    static C: OnceLock<RwLock<GlyphCache>> = OnceLock::new();
    C.get_or_init(|| RwLock::new(GlyphCache::default()))
}

const MAX_CACHED_GLYPHS: usize = 40_000;
const UPEM: f32 = 1000.0;

#[derive(Default)]
struct Pen {
    contours: Vec<Vec<Vec2>>,
    cur: Vec<Vec2>,
    points: usize,
    truncated: bool,
}

const MAX_OUTLINE_POINTS: usize = 65_536;

impl Pen {
    /// Append one flattened outline point, discarding all contours if the per-glyph point limit is exceeded.
    fn push(&mut self, point: Vec2) {
        if self.truncated {
            return;
        }
        if self.points == MAX_OUTLINE_POINTS {
            // An incomplete contour is not safe to fill; retain only the advance for this glyph.
            self.truncated = true;
            self.cur.clear();
            self.contours.clear();
            return;
        }
        self.points += 1;
        self.cur.push(point);
    }

    fn pt(x: f32, y: f32) -> Vec2 {
        Vec2::new(f64::from(x), f64::from(y))
    }
    /// Close and retain a nondegenerate contour only if its closing point also fits the outline budget.
    fn flush(&mut self) {
        if self.cur.len() > 2 {
            if let (Some(f), Some(l)) = (self.cur.first().copied(), self.cur.last().copied())
                && f != l
            {
                self.push(f);
            }
            if !self.truncated {
                self.contours.push(std::mem::take(&mut self.cur));
            }
        } else {
            self.cur.clear();
        }
    }
}

impl OutlinePen for Pen {
    /// Finish the preceding contour and begin a new one through the budgeted point writer.
    fn move_to(&mut self, x: f32, y: f32) {
        self.flush();
        self.push(Self::pt(x, y));
    }
    /// Append a line endpoint through the budgeted point writer.
    fn line_to(&mut self, x: f32, y: f32) {
        self.push(Self::pt(x, y));
    }
    /// Flatten a quadratic segment into six samples, charging each retained point to the glyph budget.
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let a = self.cur.last().copied().unwrap_or_default();
        let c = Self::pt(cx0, cy0);
        let b = Self::pt(x, y);
        for i in 1..=6 {
            let t = f64::from(i) / 6.0;
            let u = 1.0 - t;
            self.push(a * (u * u) + c * (2.0 * u * t) + b * (t * t));
        }
    }
    /// Flatten a cubic segment into eight samples, charging each retained point to the glyph budget.
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let a = self.cur.last().copied().unwrap_or_default();
        let c0 = Self::pt(cx0, cy0);
        let c1 = Self::pt(cx1, cy1);
        let b = Self::pt(x, y);
        for i in 1..=8 {
            let t = f64::from(i) / 8.0;
            let u = 1.0 - t;
            self.push(a * (u * u * u) + c0 * (3.0 * u * u * t) + c1 * (3.0 * u * t * t) + b * (t * t * t));
        }
    }
    fn close(&mut self) {
        self.flush();
    }
}

/// How far below the baseline a face reaches, as a fraction of the text height (which is the
/// face's cap height): its descent over its cap height. A bottom-justified text's alignment
/// point is there, as AutoCAD places it (#2229). `None` when the face has no usable descent.
pub fn descent_ratio(face: &Face) -> Option<f64> {
    let f = face.font()?;
    let descent = -f64::from(f.metrics(Size::new(UPEM), LocationRef::default()).descent);
    let r = descent / cap_height(&f);
    (r.is_finite() && r > 0.0 && r < 2.0).then_some(r)
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

/// The face's cap height as a fraction of its em, in (0, 1]: the factor that turns a
/// GDI-style em height (LOGFONT `|lfHeight|`) into this crate's cap-height text size.
/// `None` when the face does not parse.
pub fn cap_ratio(face: &Face) -> Option<f64> {
    let f = face.font()?;
    let r = cap_height(&f) / f64::from(UPEM);
    (r.is_finite() && r > 0.0).then(|| r.min(1.0))
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

/// Load or build a glyph outline with bounded flattening, retaining its advance even when contours are discarded.
/// Cache insertion clears existing entries when the glyph-count or aggregate point allowance would be exceeded.
fn outline(face: &Face, k: GlyphKey) -> Arc<GlyphOutline> {
    let key = (face.bytes.as_ptr() as usize, face.bytes.len(), face.index, k);
    if let Some(g) = glyph_cache().read().unwrap_or_else(PoisonError::into_inner).glyphs.get(&key) {
        return g.clone();
    }
    let Some(f) = face.font() else { return Arc::new(GlyphOutline { cap: 1.0, ..Default::default() }) };
    let mapped = match k {
        GlyphKey::Char(c) => f.charmap().map(c),
        GlyphKey::Id(id) => Some(GlyphId::new(id)),
    };
    let gid = mapped.unwrap_or_default();
    let mut pen = Pen::default();
    if let Some(g) = f.outline_glyphs().get(gid) {
        let _ = g.draw(DrawSettings::unhinted(Size::new(UPEM), LocationRef::default()), &mut pen);
        pen.flush();
    }
    let advance = f64::from(f.glyph_metrics(Size::new(UPEM), LocationRef::default()).advance_width(gid).unwrap_or(UPEM * 0.5));
    let (cap, mapped, gid) = (cap_height(&f), mapped.is_some(), gid.to_u32());
    let points = pen.contours.iter().map(Vec::len).sum();
    let g = Arc::new(GlyphOutline { points, contours: pen.contours.into(), advance, cap, mapped, gid, face: Some(face.clone()) });
    let mut cache = glyph_cache().write().unwrap_or_else(PoisonError::into_inner);
    if cache.glyphs.len() >= MAX_CACHED_GLYPHS || cache.points.saturating_add(points) > MAX_SHAPED_POINTS {
        cache.glyphs.clear();
        cache.points = 0;
    }
    if let Some(old) = cache.glyphs.insert(key, g.clone()) {
        cache.points -= old.points;
    }
    cache.points += points;
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

/// Charge source outline points before copying and transforming a glyph.
/// Return `None` without spending fuel when it cannot fit; empty outlines remain valid at zero budget.
pub(crate) fn place_glyph_budgeted(g: &GlyphOutline, x: f64, h: f64, wf: f64, shear: f64, points_left: &mut usize) -> Option<Vec<Vec<Vec2>>> {
    *points_left = points_left.checked_sub(g.points)?;
    Some(place_glyph(g, x, h, wf, shear))
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
    shape_dir(face, s, height, width_factor, oblique, None)
}

/// [`shape`] in the direction `base_rtl` (`None`: that of the line's first strong character).
pub fn shape_dir(face: &Face, s: &str, height: f64, width_factor: f64, oblique: f64, base_rtl: Option<bool>) -> Option<Shaped> {
    face.font()?;
    // As in [`crate::shape_stroke_dir`]: a line asked to run right to left takes the bidi path.
    if (crate::complex::needed(s) || base_rtl == Some(true))
        && let Some(sh) = crate::complex::shape(Some(face), s, height, width_factor, oblique, base_rtl)
    {
        return Some(sh);
    }
    let h = if height.is_finite() && height > 0.0 { height } else { 1.0 };
    let wf = if width_factor.is_finite() && width_factor.abs() > 1e-6 { width_factor } else { 1.0 };
    let shear = oblique.tan().clamp(-10.0, 10.0);
    let mut out = Shaped::default();
    let mut spans = Vec::new();
    let mut x = 0.0;
    // Cumulative retained-point budget: a compact CFF/OTF font can expand one glyph into a huge
    // outline, and a short repeated string copies it per occurrence, so cap the total geometry
    // kept in the Shaped result (CWE-400). Advances still accumulate so the width stays sensible.
    let mut points_left: usize = MAX_SHAPED_POINTS;
    for (c, under, over) in crate::decode_controls(s) {
        if crate::bidi::is_format_control(c) {
            continue;
        }
        let g = glyph_or_fallback(face, c);
        if !c.is_whitespace()
            && let Some(contours) = place_glyph_budgeted(&g, x, h, wf, shear, &mut points_left)
        {
            // Even an empty mapped glyph needs a valid outline slot for text export.
            out.push_glyph(contours, text_glyph(&g, c, x, h, wf, shear));
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
    if crate::complex::needed(s)
        && let Some(sh) = crate::complex::shape(Some(face), s, height, width_factor, 0.0, None)
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
    fn font_folders_are_cleaned_and_unknown_shx_names_are_not_available() {
        let long = "x".repeat(MAX_DIR_CHARS + 1);
        let many: Vec<String> = (0..40).map(|i| format!("/fonts/{i}")).collect();
        assert_eq!(clean_dirs(&["  /a ", "", "/a", "/b", long.as_str(), "bad\u{0}dir"]), vec!["/a".to_string(), "/b".to_string()]);
        assert_eq!(clean_dirs(&many).len(), MAX_FONT_DIRS);
        assert!(!available("gost_cadcraft.shx"));
        assert!(available("txt.shx") && available("SIMPLEX.SHX"), "the built-in stroke font stands in for these");
        assert!(available(""));
        assert!(available(crate::BUILTIN_FONT));
    }

    /// Reading a user font's names for a list keeps none of its bytes and uses none of the load
    /// budget. (Font folders themselves are process-wide: `tests/ttf_font_folders.rs`.)
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn reading_a_user_fonts_names_keeps_no_font_bytes() {
        let Some(src) = scan().into_values().find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ttf"))) else { return };
        let mut db = Db::default();
        db.user.insert("face".into());
        db.paths.insert("face".into(), Source::File(src));
        assert!(db.face_names("face").is_some_and(|n| n.family.is_some()));
        assert!(db.files.is_empty() && db.loaded_bytes == 0);
    }

    /// One unreadable user font file with the family does not stop the search: the next file
    /// that has the family is still read.
    #[test]
    fn an_unreadable_user_font_does_not_end_the_family_search() {
        let mut db = Db::default();
        let names = || Arc::new(FaceNames { family: Some("Foo".into()), faces: vec![vec!["foo".into()]] });
        for stem in ["a", "b"] {
            db.user.insert(stem.into());
            db.face_names.insert(stem.into(), names());
        }
        // "a" has no file left on disk; "b" can be read.
        db.paths.insert("b".into(), Source::Static(b"not a font"));
        let _ = db.user_family("Foo");
        assert!(db.files.contains_key("b"), "the second file with the family was tried");
    }

    /// A face found by family name is loaded within the font-byte budget: once it is used up,
    /// none is read past it (#2316 review).
    #[test]
    fn a_family_search_keeps_to_the_load_budget() {
        let mut db = Db::default();
        db.user.insert("b".into());
        db.face_names.insert("b".into(), Arc::new(FaceNames { family: Some("Foo".into()), faces: vec![vec!["foo".into()]] }));
        db.paths.insert("b".into(), Source::Static(b"not a font"));
        db.loaded_bytes = MAX_LOADED_BYTES;
        assert!(db.user_family("Foo").is_none());
        assert!(db.files.is_empty(), "nothing read past the budget");
    }

    /// Linked font folders are followed, and a circle of links reads each folder once.
    #[cfg(unix)]
    #[test]
    fn linked_font_folders_are_followed_once() {
        let root = std::env::temp_dir().join(format!("cadcraft-fontlinks-{}", std::process::id()));
        let real = root.join("real");
        let _ = std::fs::create_dir_all(&real);
        let Ok(()) = std::fs::write(real.join("LinkedFace.ttf"), b"x") else { return };
        let _ = std::fs::write(real.join("linked.shx"), b"x");
        let _ = std::os::unix::fs::symlink(&real, root.join("link"));
        // A link back up: a circle.
        let _ = std::os::unix::fs::symlink(&root, real.join("up"));
        let found = scan_dirs(&[root.join("link")], true);
        assert!(found.contains_key("linkedface"), "{found:?}");
        assert_eq!(shx_dirs(std::slice::from_ref(&root)).len(), 1, "the folder with the SHX file, once");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn thai_looks_in_thai_fonts_before_the_general_search() {
        // Letters, vowels, tone marks and digits: the general search loads a bounded number of
        // files with the CJK fonts first, none of which has Thai.
        for c in ['\u{0E01}', '\u{0E33}', '\u{0E34}', '\u{0E4C}', '\u{0E50}'] {
            assert!(script_fallback(c).contains(&"notosansthai"), "{c}");
        }
        assert!(script_fallback('a').is_empty() && script_fallback('\u{0E80}').is_empty());
        // Windows, macOS and Linux each ship one of them under these file names.
        let stems: Vec<String> = ["LeelawUI", "Thonburi", "Loma", "msyh"].map(norm).to_vec();
        assert!(stems.iter().filter(|s| THAI_FALLBACK.iter().any(|p| s.contains(p))).count() == 3);
        // The faces Thai drawings are laid out with come first, then IBM Plex Sans Thai (an
        // installed file or the craft-fonts face, both under this stem), then the others; regular
        // weights before bold.
        let stems: Vec<String> =
            ["notosansthai-regular", "ibmplexsansthai-bold", "ibmplexsansthai-regular", "msyh", "cordiab", "cordia", "ibmplexsansthailooped-regular"]
                .map(String::from)
                .to_vec();
        assert_eq!(
            script_candidates(&stems, THAI_FALLBACK),
            ["cordia", "cordiab", "ibmplexsansthai-regular", "ibmplexsansthailooped-regular", "notosansthai-regular", "ibmplexsansthai-bold"]
        );
        // A text style naming the family looks in its regular file first, not the Latin Plex
        // Sans or the looped or bold variants.
        let stems: Vec<String> = ["ibmplexsans-regular", "ibmplexsansthai-bold", "ibmplexsansthailooped-regular", "ibmplexsansthai-regular", "arial"]
            .map(String::from)
            .to_vec();
        let found = family_candidates(&stems, &norm("IBM Plex Sans Thai"), "ibm");
        assert_eq!(found.first().map(String::as_str), Some("ibmplexsansthai-regular"), "{found:?}");
        // Angsana New and Browallia New under today's Windows file names and the older ones.
        let stems: Vec<String> = ["browa", "notosansthai-regular", "angsana", "browalia", "angsa", "arial"].map(String::from).to_vec();
        assert_eq!(script_candidates(&stems, THAI_FALLBACK), ["angsa", "angsana", "browa", "browalia", "notosansthai-regular"]);
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

#[cfg(test)]
mod geometry_budget_tests {
    use super::*;
    /// Placement checks the source point cost before copying and accepts empty outlines without spending points.
    #[test]
    fn placement_checks_source_size_and_accepts_empty_glyphs() {
        let glyph = GlyphOutline { contours: vec![vec![Vec2::ZERO; 4]].into(), points: 4, cap: 1.0, ..Default::default() };
        let mut budget = 3;
        assert!(place_glyph_budgeted(&glyph, 0.0, 1.0, 1.0, 0.0, &mut budget).is_none());
        assert_eq!(budget, 3);
        let empty = GlyphOutline { cap: 1.0, ..Default::default() };
        assert_eq!(place_glyph_budgeted(&empty, 0.0, 1.0, 1.0, 0.0, &mut 0), Some(Vec::new()));
        let mut budget = 4;
        assert!(place_glyph_budgeted(&glyph, 0.0, 1.0, 1.0, 0.0, &mut budget).is_some());
        assert_eq!(budget, 0);
        assert!(place_glyph_budgeted(&glyph, 0.0, 1.0, 1.0, 0.0, &mut budget).is_none());
    }
    /// Exceeding the per-glyph flattening limit discards all contour geometry before it reaches the cache.
    #[test]
    fn flattened_outlines_are_bounded_before_caching() {
        let mut pen = Pen::default();
        for _ in 0..MAX_OUTLINE_POINTS {
            pen.line_to(0.0, 0.0);
        }
        pen.line_to(1.0, 1.0);
        pen.close();
        assert!(pen.truncated);
        assert!(pen.contours.is_empty() && pen.cur.is_empty());
        assert_eq!(pen.points, MAX_OUTLINE_POINTS);
    }
    /// A mapped empty glyph retains text metadata referring to an existing empty outline slot.
    #[test]
    fn mapped_empty_glyph_keeps_text_and_a_valid_outline_slot() {
        // Metadata placement does not parse the face; an empty face suffices to check the index contract.
        let glyph = GlyphOutline { cap: 1.0, mapped: true, gid: 1, face: Some(Face { bytes: Arc::new(Vec::new()), index: 0 }), ..Default::default() };
        let contours = place_glyph_budgeted(&glyph, 0.0, 1.0, 1.0, 0.0, &mut 0).unwrap();
        let mut shaped = Shaped::default();
        shaped.push_glyph(contours, text_glyph(&glyph, 'x', 0.0, 1.0, 1.0, 0.0));
        assert_eq!(shaped.text.len(), 1);
        assert_eq!(shaped.text[0].outline, 0);
        assert!(shaped.glyphs[shaped.text[0].outline].is_empty());
    }

    /// An empty glyph with no text mapping leaves the shaped result empty.
    #[test]
    fn unmapped_empty_glyph_does_not_make_the_shape_nonempty() {
        let glyph = GlyphOutline { cap: 1.0, ..Default::default() };
        let contours = place_glyph_budgeted(&glyph, 0.0, 1.0, 1.0, 0.0, &mut 0).unwrap();
        let mut shaped = Shaped::default();
        shaped.push_glyph(contours, text_glyph(&glyph, 'x', 0.0, 1.0, 1.0, 0.0));
        assert!(shaped.is_empty());
        assert!(shaped.text.is_empty());
    }

    /// Missing text metadata does not discard a glyph that has drawable contours.
    #[test]
    fn an_unmapped_glyph_with_contours_still_draws() {
        let mut shaped = Shaped::default();
        let contours = vec![vec![Vec2::ZERO, Vec2::new(1.0, 1.0)]];
        shaped.push_glyph(contours.clone(), None);
        assert!(!shaped.is_empty());
        assert_eq!(shaped.glyphs, vec![contours]);
        assert!(shaped.text.is_empty());
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod scan_tests {
    use super::*;

    /// Each platform searches its own system font directories. A branch that is missing or
    /// misnamed leaves a device with no fonts at all, so the platform's main location is checked
    /// here rather than discovered by a drawing full of missing-glyph boxes.
    #[test]
    fn the_platforms_system_font_directory_is_searched() {
        let dirs = system_font_dirs();
        #[cfg(target_os = "macos")]
        assert!(dirs.iter().any(|d| d == std::path::Path::new("/System/Library/Fonts")), "{dirs:?}");
        #[cfg(target_os = "android")]
        assert!(dirs.iter().any(|d| d == std::path::Path::new("/system/fonts")), "{dirs:?}");
        #[cfg(target_env = "ohos")]
        assert!(dirs.iter().any(|d| d == std::path::Path::new("/system/fonts")), "{dirs:?}");
        #[cfg(windows)]
        assert!(dirs.iter().any(|d| d == std::path::Path::new("C:\\Windows\\Fonts")), "{dirs:?}");
        #[cfg(all(unix, not(any(target_os = "macos", target_os = "android", target_env = "ohos"))))]
        assert!(dirs.iter().any(|d| d == std::path::Path::new("/usr/share/fonts")), "{dirs:?}");
    }
}
