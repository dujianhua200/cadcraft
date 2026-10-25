//! DCL dialogs from LISP: `load_dialog`, `new_dialog`, `start_dialog`, `action_tile`,
//! `set_tile`, `get_tile`, `mode_tile`, `dimx_tile`, `dimy_tile`, `done_dialog`,
//! `unload_dialog`, and the image functions `start_image`, `fill_image`, `slide_image`,
//! `end_image`.
//!
//! A dialog is parsed from its `.dcl` text and kept as data; tiles are read and written
//! without a window, so routines and tests can drive a dialog headlessly: `action_tile`
//! stores the expression, and the UI (or a test) runs it with
//! [`Session::lisp_dialog_press`], which evaluates the expression and lets `done_dialog`
//! record the result for `start_dialog` to return. With no press, `start_dialog` returns 0
//! (the dialog was dismissed). Slides (`.sld`) are placeholders: `slide_image` succeeds
//! without the file, it never blocks a dialog.
//!
//! `start_dialog` suspends the routine (a `Need::Dialog` wait) instead of returning at once:
//! the UI shows the real dialog from [`Session::lisp_dialog_open`] while the routine
//! waits, a button press runs its `action_tile` expression through
//! [`Session::lisp_dialog_press`], and [`Session::lisp_dialog_finish`] carries the
//! routine on with the `done_dialog` status. A blocking evaluation has no window to
//! show, so it takes the headless path (dismissed, status 0, or the status a press
//! already recorded). Dialog files resolve in this order: the support folder first (a
//! Windows-style path such as `d:\autolisp\dcl\auto.dcl` is read as
//! `support/autolisp/dcl/auto.dcl`, or by its file name when the folders differ), then
//! the path as given. A missing file makes `load_dialog` return nil and print a clear
//! message; it never fails silently.

use std::collections::HashMap;

use super::builtins::{args_err, read_text_file};
use super::eval::{Ctx, Flow, Machine, Need, Outcome};
use super::read::read_all;
use super::value::{self, Value};
use super::{Env, LispError};
use crate::Session;

fn arity(a: &[Value], min: usize, max: usize) -> Result<(), LispError> {
    if a.len() < min {
        return Err(args_err(true));
    }
    if a.len() > max {
        return Err(args_err(false));
    }
    Ok(())
}

fn string(v: &Value) -> Result<String, LispError> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        other => Err(LispError::bad_arg("stringp", other)),
    }
}

fn int_arg(v: &Value) -> Result<i32, LispError> {
    match v {
        Value::Int(i) => Ok(*i),
        other => Err(LispError::bad_arg("fixnump", other)),
    }
}

// ---------------- parsing ----------------

/// One DCL tile: a dialog, a row, a button, an edit box…
#[derive(Clone, Debug)]
pub struct DclTile {
    pub kind: String,
    pub key: String,
    pub label: String,
    pub attrs: HashMap<String, String>,
    pub children: Vec<DclTile>,
}

impl DclTile {
    /// Every keyed tile in this subtree, this tile first.
    fn keyed(&self, out: &mut Vec<DclTile>) {
        if !self.key.is_empty() {
            out.push(self.clone());
        }
        for c in &self.children {
            c.keyed(out);
        }
    }
}

/// One `name : dialog { … }` definition.
#[derive(Clone, Debug)]
pub struct DclDialog {
    pub name: String,
    pub label: String,
    pub tiles: Vec<DclTile>,
}

/// A parsed `.dcl` file: its dialog definitions.
#[derive(Clone, Debug, Default)]
pub struct DclFile {
    pub dialogs: Vec<DclDialog>,
    /// What the parser had to skip (`spacer_1;` with no definition, say), for the log.
    pub warnings: Vec<String>,
}

impl DclFile {
    fn find(&self, name: &str) -> Option<&DclDialog> {
        self.dialogs.iter().find(|d| d.name.eq_ignore_ascii_case(name))
    }
}

/// A dialog `new_dialog` opened: its tiles' values, its actions, its result.
#[derive(Clone, Debug)]
pub struct ActiveDialog {
    pub file_id: u32,
    pub name: String,
    pub values: HashMap<String, String>,
    pub actions: HashMap<String, String>,
    pub modes: HashMap<String, i32>,
    pub done: Option<i32>,
    pub image: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num(String),
    Punct(char),
}

fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let mut toks = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '/' {
            chars.next();
            if chars.peek() == Some(&'/') {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
                continue;
            }
            if chars.peek() == Some(&'*') {
                chars.next();
                let mut prev = ' ';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
                continue;
            }
            return Err("对话框文件中有孤立的 /".into());
        }
        if c == '"' {
            chars.next();
            let mut s = String::new();
            let mut escaped = false;
            for c in chars.by_ref() {
                if escaped {
                    match c {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        '"' => s.push('"'),
                        '\\' => s.push('\\'),
                        o => {
                            s.push('\\');
                            s.push(o);
                        }
                    }
                    escaped = false;
                    continue;
                }
                match c {
                    '"' => break,
                    '\\' => escaped = true,
                    c => s.push(c),
                }
            }
            toks.push(Tok::Str(s));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let mut s = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    s.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            toks.push(Tok::Ident(s));
            continue;
        }
        if c.is_ascii_digit() || (c == '-' && chars.clone().nth(1).is_some_and(|d| d.is_ascii_digit())) {
            let mut s = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() || c == '.' || c == '-' || c == '+' {
                    s.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            toks.push(Tok::Num(s));
            continue;
        }
        if matches!(c, ':' | ';' | '=' | '{' | '}') {
            toks.push(Tok::Punct(c));
            chars.next();
            continue;
        }
        return Err(format!("对话框文件中有不支持的字符：{c}"));
    }
    Ok(toks)
}

struct Parser {
    toks: Vec<Tok>,
    i: usize,
    /// Prototypes defined before the dialogs that use them (`name : tile { … }` at top level),
    /// instantiated by a bare `name;` inside a body, as AutoCAD's DCL does.
    protos: HashMap<String, DclTile>,
    /// What was skipped, in order.
    warnings: Vec<String>,
}

/// A tile made of the pieces DCL needs, with the attributes given.
fn tile(kind: &str, key: &str, label: &str, attrs: &[(&str, &str)], children: Vec<DclTile>) -> DclTile {
    let attrs = attrs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
    DclTile { kind: kind.to_string(), key: key.to_string(), label: label.to_string(), attrs, children }
}

/// The standard subassemblies AutoCAD's `base.dcl` defines, which dialogs everywhere reference as
/// `ok_cancel;`, `spacer_1;` and so on. Widths are the spacing the reference stands for.
fn standard_subassembly(name: &str) -> Option<DclTile> {
    let n = name.to_ascii_lowercase();
    if let Some(rest) = n.strip_prefix("spacer_")
        && let Ok(w) = rest.parse::<u32>()
    {
        return Some(tile("spacer", "", "", &[("width", &w.to_string())], Vec::new()));
    }
    let button = |label: &str, key: &str, extra: &[(&str, &str)]| {
        let mut attrs = vec![("key", key)];
        attrs.extend_from_slice(extra);
        tile("button", key, label, &attrs, Vec::new())
    };
    let mut children = match n.as_str() {
        "ok_only" => vec![button("OK", "accept", &[("is_default", "true")])],
        "ok_cancel" => vec![button("OK", "accept", &[("is_default", "true")]), button("Cancel", "cancel", &[("is_cancel", "true")])],
        "ok_cancel_help" => {
            vec![button("OK", "accept", &[("is_default", "true")]), button("Cancel", "cancel", &[("is_cancel", "true")]), button("Help", "help", &[])]
        }
        "ok_cancel_help_errtile" => {
            vec![button("OK", "accept", &[("is_default", "true")]), button("Cancel", "cancel", &[("is_cancel", "true")]), button("Help", "help", &[])]
        }
        _ => return None,
    };
    if n.ends_with("errtile") {
        // The error line sits under the buttons.
        let buttons = std::mem::take(&mut children);
        return Some(tile("column", "", "", &[], vec![tile("row", "", "", &[], buttons), tile("text", "error", "", &[], Vec::new())]));
    }
    Some(tile("row", "", "", &[], children))
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.i).cloned();
        if t.is_some() {
            self.i += 1;
        }
        t
    }

    fn eat_punct(&mut self, c: char) -> bool {
        if self.peek() == Some(&Tok::Punct(c)) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, c: char) -> Result<(), String> {
        if self.eat_punct(c) { Ok(()) } else { Err(format!("对话框文件缺少 {c}")) }
    }

    fn ident(&mut self) -> Result<String, String> {
        match self.next() {
            Some(Tok::Ident(s)) => Ok(s),
            _ => Err("对话框文件缺少名称".into()),
        }
    }

    fn value(&mut self) -> Result<String, String> {
        match self.next() {
            Some(Tok::Str(s)) | Some(Tok::Ident(s)) | Some(Tok::Num(s)) => Ok(s),
            _ => Err("对话框文件缺少属性值".into()),
        }
    }

    /// `name : type { … }` or `: type { … }`: a tile with its attributes and children.
    fn tile(&mut self) -> Result<DclTile, String> {
        let mut name = String::new();
        if self.peek() == Some(&Tok::Punct(':')) {
            self.next();
        } else {
            name = self.ident()?;
            self.expect_punct(':')?;
        }
        let kind = self.ident()?.to_ascii_lowercase();
        self.expect_punct('{')?;
        let (attrs, children) = self.body()?;
        let key = attrs.get("key").cloned().unwrap_or_else(|| name.clone());
        let label = attrs.get("label").cloned().unwrap_or_default();
        Ok(DclTile { kind, key, label, attrs, children })
    }

    /// A `{ … }` body: `attr = value;` lines and child tiles, up to the closing `}`.
    fn body(&mut self) -> Result<(HashMap<String, String>, Vec<DclTile>), String> {
        let mut attrs = HashMap::new();
        let mut children = Vec::new();
        loop {
            match self.peek() {
                None => return Err("对话框文件缺少 }".into()),
                Some(Tok::Punct('}')) => {
                    self.next();
                    return Ok((attrs, children));
                }
                Some(Tok::Punct(':')) => {
                    children.push(self.tile()?);
                }
                Some(Tok::Ident(_)) => {
                    // A lookahead: `name = value;` is an attribute, `name : type {` a tile, and a
                    // bare `name;` a subassembly reference (`spacer_1;`, `ok_cancel;`).
                    let save = self.i;
                    let name = self.ident()?;
                    if self.peek() == Some(&Tok::Punct('=')) {
                        self.next();
                        let v = self.value()?;
                        self.expect_punct(';')?;
                        attrs.insert(name.to_ascii_lowercase(), v);
                    } else if self.peek() == Some(&Tok::Punct(':')) {
                        self.i = save;
                        children.push(self.tile()?);
                    } else if self.peek() == Some(&Tok::Punct(';')) {
                        self.next();
                        // A prototype defined earlier in the file, else one of base.dcl's.
                        let sub = self.protos.get(&name.to_ascii_lowercase()).cloned().or_else(|| standard_subassembly(&name));
                        match sub {
                            Some(t) => children.push(t),
                            None => self.warnings.push(format!("对话框文件中的子装配 {name}; 未定义，已跳过")),
                        }
                    } else {
                        return Err(format!("对话框文件在 {name} 后缺少 = 或 :"));
                    }
                }
                Some(t) => return Err(format!("对话框文件中有不支持的内容：{t:?}")),
            }
        }
    }
}

/// Parse `.dcl` text: every top-level `name : dialog { … }` is a dialog definition; other
/// top-level `name : type { … }` entries are prototypes a later `name;` instantiates.
pub fn parse_dcl(src: &str) -> Result<DclFile, String> {
    let toks = tokenize(src)?;
    let mut p = Parser { toks, i: 0, protos: HashMap::new(), warnings: Vec::new() };
    let mut file = DclFile::default();
    while p.peek().is_some() {
        let t = p.tile()?;
        if t.kind == "dialog" {
            let label = t.label.clone();
            file.dialogs.push(DclDialog { name: t.key.clone(), label, tiles: t.children });
        } else if !t.key.is_empty() {
            p.protos.insert(t.key.to_ascii_lowercase(), t);
        }
    }
    file.warnings = p.warnings;
    Ok(file)
}

// ---------------- file resolution ----------------

/// A Windows-style path as a relative one: `d:\autolisp\dcl\auto.dcl` →
/// `autolisp/dcl/auto.dcl`.
fn normalize_path(path: &str) -> String {
    let mut p = path.replace('\\', "/");
    if p.len() >= 2 && p.as_bytes()[1] == b':' && p.as_bytes()[0].is_ascii_alphabetic() {
        p = p[2..].to_string();
    }
    p.trim_start_matches('/').to_string()
}

/// The text of a dialog file: the support folder first, then the path as given.
fn read_dcl(s: &Session, path: &str) -> Option<String> {
    let norm = normalize_path(path);
    let base = std::path::Path::new(&norm).file_name().and_then(|n| n.to_str()).unwrap_or(&norm).to_string();
    let mut candidates = Vec::new();
    if let Some(dir) = s.lisp_support_dir() {
        candidates.push(dir.join(&norm).to_string_lossy().into_owned());
        candidates.push(dir.join(&base).to_string_lossy().into_owned());
    }
    candidates.push(norm);
    candidates.push(base);
    candidates.push(path.to_string());
    candidates.into_iter().find_map(|c| read_text_file(&c))
}

// ---------------- the builtins ----------------

fn active(env: &mut Env) -> Result<&mut ActiveDialog, LispError> {
    env.dcl_active.as_mut().ok_or_else(|| LispError::new("没有打开的对话框".to_string()))
}

pub fn load_dialog(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let path = string(&a[0])?;
    match read_dcl(ctx.s, &path).map(|t| parse_dcl(&t)) {
        Some(Ok(file)) => {
            for w in &file.warnings {
                ctx.s.echo(w.clone());
            }
            let id = ctx.env.dcl_next;
            ctx.env.dcl_next = ctx.env.dcl_next.wrapping_add(1).max(1);
            ctx.env.dcl_files.insert(id, file);
            Ok(Flow::Value(Value::Int(i32::try_from(id).unwrap_or(i32::MAX))))
        }
        Some(Err(detail)) => {
            ctx.s.echo(format!("对话框文件解析失败：{path}：{detail}"));
            Ok(Flow::Value(Value::Nil))
        }
        None => {
            ctx.s.echo(format!("找不到对话框文件：{path}。请把 .dcl 文件放入 support 文件夹后重试。"));
            Ok(Flow::Value(Value::Nil))
        }
    }
}

pub fn new_dialog(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 2)?;
    let name = string(&a[0])?;
    let id = int_arg(&a[1])?;
    let Some(file) = ctx.env.dcl_files.get(&u32::try_from(id).unwrap_or(u32::MAX)) else { return Ok(Flow::Value(Value::Nil)) };
    let Some(d) = file.find(&name) else { return Ok(Flow::Value(Value::Nil)) };
    let mut values = HashMap::new();
    let mut keyed = Vec::new();
    for t in &d.tiles {
        t.keyed(&mut keyed);
    }
    for t in keyed {
        let v = t.attrs.get("value").cloned().unwrap_or_default();
        values.insert(t.key.clone(), v);
    }
    ctx.env.dcl_active = Some(ActiveDialog {
        file_id: u32::try_from(id).unwrap_or(0),
        name: d.name.clone(),
        values,
        actions: HashMap::new(),
        modes: HashMap::new(),
        done: None,
        image: None,
    });
    Ok(Flow::Value(Value::t()))
}

pub fn start_dialog(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 0, 0)?;
    if ctx.env.dcl_active.is_none() {
        return Ok(Flow::Value(Value::Nil));
    }
    // Suspend the routine: the UI shows the dialog while it waits, and a button press
    // (or a dismiss) finishes it with the `done_dialog` status. The active dialog stays
    // put until then, so `set_tile` / `get_tile` / actions still see it.
    Ok(Flow::Wait(Need::Dialog))
}

pub fn action_tile(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 2)?;
    let key = string(&a[0])?;
    let expr = string(&a[1])?;
    let d = active(ctx.env)?;
    d.actions.insert(key, expr);
    Ok(Flow::Value(Value::t()))
}

pub fn set_tile(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 2)?;
    let key = string(&a[0])?;
    let value = string(&a[1])?;
    let d = active(ctx.env)?;
    if !d.values.contains_key(&key) {
        return Ok(Flow::Value(Value::Nil));
    }
    d.values.insert(key, value.clone());
    Ok(Flow::Value(Value::str(&value)))
}

pub fn get_tile(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let key = string(&a[0])?;
    let v = ctx.env.dcl_active.as_ref().and_then(|d| d.values.get(&key).cloned()).unwrap_or_default();
    Ok(Flow::Value(Value::str(&v)))
}

pub fn mode_tile(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 2)?;
    let key = string(&a[0])?;
    let mode = int_arg(&a[1])?;
    let d = active(ctx.env)?;
    if !d.values.contains_key(&key) {
        return Ok(Flow::Value(Value::Nil));
    }
    d.modes.insert(key, mode);
    Ok(Flow::Value(Value::t()))
}

/// A tile's size in dialog units: its `width` / `height` attribute, else a default by kind.
fn tile_size(ctx: &mut Ctx<'_>, key: &str, attr: &str) -> Result<Flow, LispError> {
    let (kind, attrs) = {
        let Some(d) = ctx.env.dcl_active.as_ref() else { return Ok(Flow::Value(Value::Int(0))) };
        let Some(file) = ctx.env.dcl_files.get(&d.file_id) else { return Ok(Flow::Value(Value::Int(0))) };
        let Some(def) = file.find(&d.name) else { return Ok(Flow::Value(Value::Int(0))) };
        let mut keyed = Vec::new();
        for t in &def.tiles {
            t.keyed(&mut keyed);
        }
        match keyed.into_iter().find(|t| t.key == key) {
            Some(t) => (t.kind.clone(), t.attrs.clone()),
            None => return Ok(Flow::Value(Value::Int(0))),
        }
    };
    let v = attrs.get(attr).and_then(|s| s.parse::<f64>().ok()).unwrap_or(match (attr, kind.as_str()) {
        ("width", "image") => 32.0,
        ("height", "image") => 16.0,
        ("width", "button") => 16.0,
        ("width", "edit_box") => 20.0,
        ("width", _) => 10.0,
        ("height", _) => 1.0,
        _ => 1.0,
    });
    Ok(Flow::Value(Value::Int(v as i32)))
}

pub fn dimx_tile(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let key = string(&a[0])?;
    tile_size(ctx, &key, "width")
}

pub fn dimy_tile(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let key = string(&a[0])?;
    tile_size(ctx, &key, "height")
}

pub fn done_dialog(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 0, 1)?;
    let status = match a.first() {
        Some(v) => int_arg(v)?,
        None => 1,
    };
    let d = active(ctx.env)?;
    d.done = Some(status);
    Ok(Flow::Value(Value::Int(status)))
}

pub fn unload_dialog(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let id = u32::try_from(int_arg(&a[0])?).unwrap_or(u32::MAX);
    let existed = ctx.env.dcl_files.remove(&id).is_some();
    if ctx.env.dcl_active.as_ref().is_some_and(|d| d.file_id == id) {
        ctx.env.dcl_active = None;
    }
    Ok(Flow::Value(Value::bool(existed)))
}

pub fn start_image(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let key = string(&a[0])?;
    let d = active(ctx.env)?;
    if !d.values.contains_key(&key) {
        return Ok(Flow::Value(Value::Nil));
    }
    d.image = Some(key);
    Ok(Flow::Value(Value::t()))
}

pub fn fill_image(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 5, 5)?;
    for v in &a {
        int_arg(v)?;
    }
    if ctx.env.dcl_active.as_ref().is_none_or(|d| d.image.is_none()) {
        return Ok(Flow::Value(Value::Nil));
    }
    Ok(Flow::Value(Value::t()))
}

/// A slide file is a blank placeholder: a missing `.sld` never blocks the dialog.
pub fn slide_image(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 5, 5)?;
    for v in &a[..4] {
        int_arg(v)?;
    }
    string(&a[4])?;
    if ctx.env.dcl_active.as_ref().is_none_or(|d| d.image.is_none()) {
        return Ok(Flow::Value(Value::Nil));
    }
    Ok(Flow::Value(Value::t()))
}

pub fn end_image(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 0, 0)?;
    let d = active(ctx.env)?;
    d.image = None;
    Ok(Flow::Value(Value::t()))
}

/// What the UI shows for a dialog a routine waits on: its definition plus the live
/// values, modes and action keys.
#[derive(Clone, Debug)]
pub struct DclDialogView {
    pub name: String,
    pub label: String,
    pub tiles: Vec<DclTile>,
    pub values: HashMap<String, String>,
    pub modes: HashMap<String, i32>,
    /// Keys with an `action_tile` expression: the buttons that do something.
    pub actions: Vec<String>,
}

impl Session {
    /// The dialog a routine waits on (`start_dialog` suspended it), for the UI to show:
    /// `None` when no routine waits on a dialog, or the dialog is gone.
    pub fn lisp_dialog_open(&self) -> Option<DclDialogView> {
        let uid = self.state().map(|s| s.uid).unwrap_or(0);
        if !self.lisp.run.as_ref().is_some_and(|r| matches!(r.waiting, Some(Need::Dialog))) {
            return None;
        }
        let env = self.lisp.envs.get(&uid)?;
        let d = env.dcl_active.as_ref()?;
        let file = env.dcl_files.get(&d.file_id)?;
        let def = file.find(&d.name)?;
        Some(DclDialogView {
            name: d.name.clone(),
            label: def.label.clone(),
            tiles: def.tiles.clone(),
            values: d.values.clone(),
            modes: d.modes.clone(),
            actions: d.actions.keys().cloned().collect(),
        })
    }

    /// The open dialog's tile names, for the UI layer.
    pub fn lisp_dialog_tiles(&self) -> Vec<String> {
        let uid = self.state().map(|s| s.uid).unwrap_or(0);
        self.lisp.envs.get(&uid).and_then(|e| e.dcl_active.as_ref()).map(|d| d.values.keys().cloned().collect()).unwrap_or_default()
    }

    /// The `done_dialog` status the open dialog recorded, if any: a button press that set
    /// one means the dialog is finished and the UI should call [`Session::lisp_dialog_finish`].
    pub fn lisp_dialog_done_status(&self) -> Option<i32> {
        let uid = self.state().map(|s| s.uid).unwrap_or(0);
        self.lisp.envs.get(&uid).and_then(|e| e.dcl_active.as_ref()).and_then(|d| d.done)
    }

    /// Write a tile's value from the UI (the `set_tile` builtin's write, without LISP):
    /// `false` when there is no open dialog or no such tile.
    pub fn lisp_dialog_set_tile(&mut self, key: &str, value: &str) -> bool {
        let uid = self.state().map(|s| s.uid).unwrap_or(0);
        let Some(d) = self.lisp.envs.get_mut(&uid).and_then(|e| e.dcl_active.as_mut()) else { return false };
        if !d.values.contains_key(key) {
            return false;
        }
        d.values.insert(key.to_string(), value.to_string());
        true
    }

    /// Finish the dialog a routine waits on with `status` (`start_dialog` returns it) and
    /// carry the routine on. `false` when no routine waits on a dialog. The dialog is
    /// consumed, like `start_dialog` used to consume it.
    pub fn lisp_dialog_finish(&mut self, status: i32) -> bool {
        let uid = self.state().map(|s| s.uid).unwrap_or(0);
        if let Some(env) = self.lisp.envs.get_mut(&uid) {
            env.dcl_active = None;
        }
        if !self.lisp.run.as_ref().is_some_and(|r| matches!(r.waiting, Some(Need::Dialog))) {
            return false;
        }
        if let Some(r) = self.lisp.run.as_mut() {
            r.waiting = None;
            r.machine.resume(Value::Int(status));
        }
        self.lisp_drive();
        true
    }

    /// Take the open dialog and return the status `start_dialog` would report for it
    /// (its `done_dialog` status, or 0 when dismissed): the headless path for a blocking
    /// evaluation, which has no window to show.
    pub(crate) fn lisp_take_dialog_done(&mut self) -> i32 {
        let uid = self.state().map(|s| s.uid).unwrap_or(0);
        self.lisp.envs.get_mut(&uid).and_then(|e| e.dcl_active.take()).and_then(|d| d.done).unwrap_or(0)
    }

    /// Run the action `action_tile` stored for `key` in the open dialog: the expression a
    /// button press would evaluate (it usually sets variables and calls `done_dialog`).
    /// The dialog stays open until [`Session::lisp_dialog_finish`] reads the result.
    /// `Ok(None)`: no dialog, or no action for the key.
    ///
    /// The expression runs in its own machine on the drawing's environment: the routine
    /// that waits on the dialog is suspended inside `start_dialog`, and `lisp_eval`
    /// would refuse it ("a LISP routine is already running"). An action that itself
    /// waits (a `get…` call, a nested dialog) fails instead of suspending.
    pub fn lisp_dialog_press(&mut self, key: &str) -> crate::Result<Option<std::result::Result<Value, String>>> {
        let uid = self.state().map(|s| s.uid).unwrap_or(0);
        let Some(expr) = self.lisp.envs.get(&uid).and_then(|e| e.dcl_active.as_ref()).and_then(|d| d.actions.get(key).cloned()) else {
            return Ok(None);
        };
        let Ok(forms) = read_all(&expr) else {
            return Ok(Some(Err(format!("bad action for `{key}`"))));
        };
        // A panic in the action must not take the app down, and must not leave the
        // drawing half-edited: the drawing is restored to what the action found.
        let before = self.state().ok().map(|st| st.doc.clone());
        let mut env = self.lisp_env_take();
        let (string_bytes, cells_at_start) = self.lisp.run.as_ref().map(|r| (r.string_bytes, r.cells_at_start)).unwrap_or((0, value::live_cells()));
        value::set_string_budget(string_bytes, self.lisp.limits.string_bytes);
        value::set_cell_limit(self.lisp.limits.cells.max(cells_at_start));
        let outcome = {
            let mut machine = Machine::forms(forms);
            let mut ctx = Ctx { s: self, env: &mut env, depth: 0 };
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| machine.run(&mut ctx, 4_000_000)))
        };
        self.lisp_flush(&mut env, false);
        self.lisp_env_put(uid, env);
        let outcome = match outcome {
            Ok(o) => o,
            Err(p) => {
                if let Some(doc) = before
                    && let Some(st) = self.docs.iter_mut().find(|d| d.uid == uid)
                {
                    st.doc = doc;
                }
                return Ok(Some(Err(format!("internal error: {}", crate::guard::message(&*p)))));
            }
        };
        Ok(Some(match outcome {
            Outcome::Done(v) => Ok(v),
            Outcome::Error(e) => Err(e.msg),
            Outcome::Wait(need) => Err(match need {
                Need::Dialog => "a dialog action cannot open another dialog".to_string(),
                Need::Get(_) => "a dialog action cannot wait for input".to_string(),
                Need::Pause => "a dialog action cannot pause for a command".to_string(),
                Need::Yield => "a dialog action took too long".to_string(),
            }),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real AutoCAD dialogs are built from `base.dcl`'s subassemblies: `spacer_1;` and
    /// `ok_cancel;` are references, not attributes, and used to stop the parse with
    /// "missing = or :".
    #[test]
    fn base_dcl_subassemblies_expand() {
        let src = "auto : dialog {\n label = \"Auto\";\n : row { : edit_box { label = \"L\"; key = \"changdu\"; } }\n spacer_1;\n : row { : edit_box { key = \"bili\"; } }\n ok_cancel;\n}\n";
        let f = parse_dcl(src).unwrap();
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
        let d = &f.dialogs[0];
        assert_eq!(d.name, "auto");
        assert_eq!(d.tiles.len(), 4, "{:?}", d.tiles.iter().map(|t| &t.kind).collect::<Vec<_>>());
        assert_eq!(d.tiles[0].kind, "row");
        assert_eq!(d.tiles[1].kind, "spacer");
        assert_eq!(d.tiles[1].attrs.get("width").map(String::as_str), Some("1"));
        // ok_cancel: a row holding OK (accept) and Cancel (cancel).
        let cancel = &d.tiles[3];
        assert_eq!(cancel.kind, "row");
        let keys: Vec<&str> = cancel.children.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["accept", "cancel"]);
        assert_eq!(cancel.children[1].attrs.get("is_cancel").map(String::as_str), Some("true"));
    }

    /// A prototype defined before the dialog instantiates through its bare name.
    #[test]
    fn prototypes_instantiate_by_name() {
        let src = "myrow : row { : edit_box { key = \"a\"; } }\nmain : dialog { label = \"M\"; myrow; }\n";
        let f = parse_dcl(src).unwrap();
        let d = &f.dialogs[0];
        assert_eq!(d.tiles.len(), 1);
        assert_eq!(d.tiles[0].kind, "row");
        assert_eq!(d.tiles[0].children[0].key, "a");
    }

    /// An undefined reference is skipped with a warning; the rest of the dialog still loads.
    #[test]
    fn unknown_subassembly_is_a_warning_not_an_error() {
        let f = parse_dcl("main : dialog { label = \"M\"; nosuch_thing; : button { key = \"b\"; } }\n").unwrap();
        assert_eq!(f.dialogs[0].tiles.len(), 1);
        assert_eq!(f.warnings.len(), 1);
        assert!(f.warnings[0].contains("nosuch_thing"));
    }
}
