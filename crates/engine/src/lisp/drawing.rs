//! Drawing access from LISP: `command`, system variables, entity lists (`entget`, `entmake`,
//! `entmod`, `entdel`, `entlast`, `entnext`, `handent`), selection sets (`ssget` and friends),
//! symbol tables (`tblsearch`, `tblnext`) and unit conversions (`rtos`, `angtos`, `distof`,
//! `angtof`).
//!
//! Entity lists use the DXF groups the file reader and writer use (through
//! [`crate::cmd::file::entity_codec`]), except angles: the codec stores most angles as
//! degrees in DXF files, while AutoLISP reports and takes them as radians (the DXF
//! Reference). `entget` converts degrees to radians, `entmake`/`entmod` convert back,
//! so a routine that reads an angle and writes it back round-trips exactly.

use std::sync::{Arc, Mutex};

use cadcraft_doc::{Entity, EntityKind, Handle, Space};
use cadcraft_dxf::Tag;
use cadcraft_geom::Vec2;

use super::LispError;
use super::builtins::args_err;
use super::eval::{Ctx, Flow, Need};
use super::value::{PickSet, Value, to_text};
use crate::{Input, Session};

fn arity(a: &[Value], min: usize, max: usize) -> Result<(), LispError> {
    if a.len() < min {
        return Err(args_err(true));
    }
    if a.len() > max {
        return Err(args_err(false));
    }
    Ok(())
}

fn ename(v: &Value) -> Result<Handle, LispError> {
    match v {
        Value::Ename(h) => Ok(*h),
        other => Err(LispError::bad_arg("lentityp", other)),
    }
}

fn string(v: &Value) -> Result<String, LispError> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        other => Err(LispError::bad_arg("stringp", other)),
    }
}

fn pickset(v: &Value) -> Result<PickSet, LispError> {
    match v {
        Value::PickSet(p) => Ok(p.clone()),
        other => Err(LispError::bad_arg("lselsetp", other)),
    }
}

fn new_pickset(ctx: &mut Ctx<'_>, items: Vec<Handle>) -> Value {
    let id = ctx.env.next_pickset;
    ctx.env.next_pickset = ctx.env.next_pickset.wrapping_add(1);
    Value::PickSet(PickSet { id, items: Arc::new(Mutex::new(items)) })
}

fn items(p: &PickSet) -> Vec<Handle> {
    p.items.lock().map(|v| v.clone()).unwrap_or_default()
}

// ---------------- command ----------------

/// `(command arg…)`: arguments go to the command line as typed input: a string starts a command
/// or answers a prompt ("" is Enter), a number or a point is entered, an entity name or a
/// selection set picks objects, and `pause` waits for the user. A command not finished by the
/// arguments stays active for the next `command` call. No arguments: cancel the active command.
pub fn command(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    if a.is_empty() {
        if ctx.s.running.is_some() {
            ctx.s.cancel();
        }
        return Ok(Flow::Value(Value::Nil));
    }
    for (i, arg) in a.iter().enumerate() {
        match arg {
            Value::Nil => {}
            Value::Str(s) if &**s == "\\" => {
                // `pause`: the user answers this prompt, then the rest goes in.
                if ctx.s.running.is_some() {
                    let rest = a[i + 1..].to_vec();
                    let f = super::eval::lookup(ctx.env, &super::value::Sym::new("COMMAND"));
                    return Ok(if rest.is_empty() { Flow::Wait(Need::Pause) } else { Flow::WaitThen(Need::Pause, f, rest) });
                }
            }
            Value::Str(s) => {
                let r = if ctx.s.running.is_none() {
                    if s.trim().is_empty() { Ok(()) } else { ctx.s.start(s) }
                } else if s.is_empty() {
                    ctx.s.input(Input::Enter)
                } else {
                    ctx.s.typed(s)
                };
                if let Err(e) = r {
                    ctx.s.echo(e.to_string());
                }
            }
            Value::Int(_) | Value::Real(_) => {
                if ctx.s.running.is_some() {
                    let text = match arg {
                        Value::Int(n) => n.to_string(),
                        Value::Real(r) => format!("{r}"),
                        _ => String::new(),
                    };
                    if let Err(e) = ctx.s.typed(&text) {
                        ctx.s.echo(e.to_string());
                    }
                }
            }
            Value::Ename(h) => feed(ctx.s, Input::Pick(vec![*h])),
            Value::PickSet(p) => feed(ctx.s, Input::Pick(items(p))),
            Value::Cons(_) => {
                if let Some(p) = arg.as_point() {
                    feed(ctx.s, Input::Point(Vec2::new(p[0], p[1])));
                } else if let Value::Ename(h) = arg.car() {
                    // An `entsel` result: (ename point).
                    feed(ctx.s, Input::Pick(vec![h]));
                } else {
                    return Err(LispError::bad_arg("command argument", arg));
                }
            }
            other => return Err(LispError::bad_arg("command argument", other)),
        }
    }
    Ok(Flow::Value(Value::Nil))
}

fn feed(s: &mut Session, input: Input) {
    if s.running.is_some()
        && let Err(e) = s.input(input)
    {
        s.echo(e.to_string());
    }
}

// ---------------- system variables ----------------

/// A JSON value as LISP: integers, reals, strings, lists (points).
pub fn from_json(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(b) => Value::Int(i32::from(*b)),
        serde_json::Value::Number(n) => match n.as_i64().and_then(|i| i32::try_from(i).ok()) {
            Some(i) => Value::Int(i),
            None => Value::Real(n.as_f64().unwrap_or(0.0)),
        },
        serde_json::Value::String(s) => Value::str(s),
        serde_json::Value::Array(items) => Value::list(items.iter().map(from_json).collect::<Vec<_>>()),
        serde_json::Value::Object(_) => Value::str(&v.to_string()),
    }
}

/// A LISP value as JSON (for `setvar`).
pub fn to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Nil => serde_json::Value::Null,
        Value::Int(i) => serde_json::json!(i),
        Value::Real(r) => serde_json::json!(r),
        Value::Str(s) => serde_json::json!(&**s),
        Value::Cons(_) => serde_json::Value::Array(v.iter().map(|x| to_json(&x)).collect()),
        other => serde_json::json!(to_text(other, false)),
    }
}

pub fn getvar(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let name = string(&a[0])?;
    Ok(Flow::Value(crate::sysvars::get(ctx.s, &name).map(|v| from_json(&v)).unwrap_or_default()))
}

pub fn setvar(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 2)?;
    let name = string(&a[0])?;
    match crate::sysvars::set(ctx.s, &name, &to_json(&a[1])) {
        Ok(()) => Ok(Flow::Value(a[1].clone())),
        Err(_) => Err(LispError::new(format!("AutoCAD variable setting rejected: {} {}", to_text(&a[0], true), to_text(&a[1], true)))),
    }
}

// ---------------- entity lists ----------------

/// Groups whose value is a point: 10–18, 110–112, 210, 1010–1013 (x), with y and z 10 and 20
/// higher.
fn point_group(code: i32) -> bool {
    (10..=18).contains(&code) || (110..=112).contains(&code) || code == 210 || (1010..=1013).contains(&code)
}

fn tag_value(t: &Tag) -> Value {
    match &t.value {
        cadcraft_dxf::Value::Str(s) | cadcraft_dxf::Value::Hex(s) => Value::str(s),
        cadcraft_dxf::Value::Real(r) => Value::Real(*r),
        cadcraft_dxf::Value::Int(i) => Value::Int(i32::try_from(*i).unwrap_or(if *i < 0 { i32::MIN } else { i32::MAX })),
        cadcraft_dxf::Value::Bool(b) => Value::Int(i32::from(*b)),
    }
}

/// DXF groups as an association list: points combined into lists `(10 x y z)`.
fn groups_to_list(tags: &[Tag]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(t) = tags.get(i) {
        if point_group(t.code) {
            let x = tag_value(t).as_number().unwrap_or(0.0);
            let y = tags.get(i + 1).filter(|n| n.code == t.code + 10).and_then(|n| tag_value(n).as_number());
            let z = tags.get(i + 2).filter(|n| n.code == t.code + 20 && y.is_some()).and_then(|n| tag_value(n).as_number());
            match (y, z) {
                (Some(y), Some(z)) => {
                    out.push(Value::list(vec![Value::Int(t.code), Value::Real(x), Value::Real(y), Value::Real(z)]));
                    i += 3;
                }
                (Some(y), None) => {
                    out.push(Value::list(vec![Value::Int(t.code), Value::Real(x), Value::Real(y)]));
                    i += 2;
                }
                _ => {
                    out.push(Value::cons(Value::Int(t.code), Value::Real(x)));
                    i += 1;
                }
            }
            continue;
        }
        out.push(Value::cons(Value::Int(t.code), tag_value(t)));
        i += 1;
    }
    out
}

/// An association list back to DXF groups (points split into x, y, z groups). The type
/// (`0`), the entity name (`-1`), the handle (`5`), the owner (`330`) and the layout (`410`) are
/// left out: they aren't the entity's to set.
fn list_to_groups(list: &Value) -> Result<(String, Vec<Tag>), LispError> {
    let mut kind = None;
    let mut tags = Vec::new();
    for item in list.iter() {
        let Value::Cons(c) = &item else { return Err(LispError::bad_arg("listp", &item)) };
        let Value::Int(code) = c.car else { return Err(LispError::bad_arg("fixnump", &c.car)) };
        let v = c.cdr.clone();
        match code {
            0 => {
                kind = Some(match &v {
                    Value::Str(s) => s.to_ascii_uppercase(),
                    other => return Err(LispError::bad_arg("stringp", other)),
                })
            }
            -1 | 5 | 330 | 360 | 410 | 102 => {}
            _ if point_group(code) && matches!(v, Value::Cons(_)) => {
                let p = v.as_point().ok_or_else(|| LispError::bad_arg("2D/3D point", &v))?;
                tags.push(Tag::f(code, p[0]));
                tags.push(Tag::f(code + 10, p[1]));
                tags.push(Tag::f(code + 20, p[2]));
            }
            _ => tags.push(match &v {
                Value::Int(i) => Tag::i(code, i64::from(*i)),
                Value::Real(r) => Tag::f(code, *r),
                Value::Str(s) => Tag::s(code, &**s),
                Value::Ename(h) => Tag::s(code, h.hex()),
                other => return Err(LispError::bad_arg("DXF group value", other)),
            }),
        }
    }
    Ok((kind.ok_or_else(|| LispError::new("bad DXF group: no entity type (0)"))?, tags))
}

fn codec() -> Result<&'static crate::cmd::file::EntityCodec, LispError> {
    crate::cmd::file::entity_codec().ok_or_else(|| LispError::new("entity lists are not available in this build"))
}

/// Group codes the codec stores as degrees but AutoLISP reports as radians (the DXF
/// Reference: angles are radians). ELLIPSE's 41/42 parameters are already radians in
/// the codec, so they are not listed. HATCH's 52 is the pattern angle (only written
/// for non-solid hatches). Unmodelled types (e.g. SHAPE) need no arm: the writer
/// emits no groups for them, so `entget` never sees their angles at all.
fn angle_groups(kind: &str) -> &'static [i32] {
    match kind {
        "POINT" => &[50],
        "TEXT" | "ATTRIB" | "ATTDEF" => &[50, 51],
        "MTEXT" => &[50],
        "ARC" => &[50, 51],
        "INSERT" => &[50],
        "DIMENSION" => &[50, 52, 53],
        "HATCH" => &[52],
        _ => &[],
    }
}

/// An `entget` list with the codec's degree angles turned into the radians AutoLISP
/// reports. A `0` group (an ATTRIB inlined in an INSERT) switches the entity type.
fn entget_angles(kind: &str, items: Vec<Value>) -> Vec<Value> {
    let mut cur = kind.to_ascii_uppercase();
    items
        .into_iter()
        .map(|item| {
            let (code, text) = match &item {
                Value::Cons(c) => (c.car.as_number(), c.cdr.clone()),
                _ => (None, Value::Nil),
            };
            if code == Some(0.0) {
                if let Value::Str(s) = &text {
                    cur = s.to_ascii_uppercase();
                }
                return item;
            }
            let Some(code) = code.map(|c| c as i32) else { return item };
            if !angle_groups(&cur).contains(&code) {
                return item;
            }
            match &item {
                Value::Cons(c) => match &c.cdr {
                    Value::Real(r) => Value::cons(c.car.clone(), Value::Real(r.to_radians())),
                    _ => item,
                },
                _ => item,
            }
        })
        .collect()
}

fn space_of(d: &cadcraft_doc::Drawing, h: Handle) -> Option<Space> {
    d.space_of(h)
}

pub fn entget(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 2)?;
    let h = ename(&a[0])?;
    let Ok(d) = ctx.s.doc() else { return Ok(Flow::Value(Value::Nil)) };
    let Some(e) = d.entity(h) else { return Ok(Flow::Value(Value::Nil)) };
    let (kind, tags) = (codec()?.groups)(d, e);
    let layout = match space_of(d, h) {
        Some(Space::Paper(name)) => name,
        _ => "Model".into(),
    };
    let mut items = vec![Value::cons(Value::Int(-1), Value::Ename(h)), Value::cons(Value::Int(0), Value::str(&kind))];
    let mut placed_layout = false;
    for v in entget_angles(&kind, groups_to_list(&tags)) {
        // The layout name follows the AcDbEntity marker, as AutoCAD lists it.
        let after_marker = !placed_layout && v.car().as_number() == Some(100.0) && matches!(v.cdr(), Value::Str(ref s) if &**s == "AcDbEntity");
        items.push(v);
        if after_marker {
            items.push(Value::cons(Value::Int(410), Value::str(&layout)));
            placed_layout = true;
        }
    }
    // Rotation (50) is always present in AutoCAD's `entget`, even when 0: routines read it
    // without checking. The writer omits a zero rotation, so it is added back here.
    if matches!(kind.as_str(), "TEXT" | "MTEXT" | "INSERT" | "ATTDEF") && !items.iter().any(|v| v.car().as_number() == Some(50.0)) {
        items.push(Value::cons(Value::Int(50), Value::Real(0.0)));
    }
    Ok(Flow::Value(Value::list(items)))
}

/// The entity a list describes, as the reader makes it; a missing layer (8) is the current one.
fn entity_from_list(ctx: &Ctx<'_>, list: &Value) -> Result<(String, cadcraft_doc::Common, cadcraft_doc::EntityKind), LispError> {
    let (kind, mut tags) = list_to_groups(list)?;
    // AutoLISP angle groups are radians; the codec expects degrees.
    for t in tags.iter_mut() {
        if angle_groups(&kind).contains(&t.code) {
            let v = t.value.as_f64();
            if v.is_finite() {
                t.value = cadcraft_dxf::Value::Real(v.to_degrees());
            }
        }
    }
    if !tags.iter().any(|t| t.code == 8) {
        let layer = crate::sysvars::get(ctx.s, "CLAYER").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "0".into());
        tags.insert(0, Tag::s(8, layer));
    }
    match (codec()?.entity)(&kind, &tags) {
        // A type the reader only carries through (an unknown or invalid name) isn't made.
        Some((_, cadcraft_doc::EntityKind::Unknown(_))) | None => Err(LispError::new(format!("bad DXF group: {kind} can't be made from this list"))),
        Some((common, k)) => Ok((kind, common, k)),
    }
}

/// Make the entity a list describes: its handle, or `None` when the list can't be made.
fn make_entity(ctx: &mut Ctx<'_>, list: &Value) -> Option<Handle> {
    let (_, common, kind) = entity_from_list(ctx, list).ok()?;
    let space = ctx.s.space();
    let Ok(d) = ctx.s.doc_mut() else { return None };
    // A layer entmake names that doesn't exist yet is created.
    d.ensure_layer(&common.layer);
    match d.add(&space, common, kind) {
        Ok(h) => {
            ctx.s.touch();
            Some(h)
        }
        Err(_) => None,
    }
}

pub fn entmake(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 0, 1)?;
    let Some(list) = a.first().filter(|v| !v.is_nil()) else { return Ok(Flow::Value(Value::Nil)) };
    Ok(Flow::Value(if make_entity(ctx, list).is_some() { list.clone() } else { Value::Nil }))
}

/// `(entmakex list)`: like `entmake`, but returns the new entity's name instead of the list.
pub fn entmakex(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 0, 1)?;
    let Some(list) = a.first().filter(|v| !v.is_nil()) else { return Ok(Flow::Value(Value::Nil)) };
    Ok(Flow::Value(make_entity(ctx, list).map(Value::Ename).unwrap_or_default()))
}

pub fn entmod(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let list = &a[0];
    let Some(Value::Ename(h)) = list.iter().find(|v| v.car().as_number() == Some(-1.0)).map(|v| v.cdr()) else {
        return Err(LispError::bad_arg("lentityp", &Value::Nil));
    };
    let Some(old) = ctx.s.doc().ok().and_then(|d| d.entity(h)).cloned() else { return Ok(Flow::Value(Value::Nil)) };
    let (kind_name, common, kind) = match entity_from_list(ctx, list) {
        Ok(x) => x,
        Err(_) => return Ok(Flow::Value(Value::Nil)),
    };
    // entmod changes an entity, never its type.
    if !kind_name.eq_ignore_ascii_case(old.kind.dxf_name()) {
        return Ok(Flow::Value(Value::Nil));
    }
    let Ok(d) = ctx.s.doc_mut() else { return Ok(Flow::Value(Value::Nil)) };
    d.ensure_layer(&common.layer);
    let ok = d
        .modify_entity(h, |e| {
            e.common = common;
            e.kind = kind;
        })
        .is_ok();
    if ok {
        ctx.s.touch();
    }
    Ok(Flow::Value(if ok { list.clone() } else { Value::Nil }))
}

/// `(entupd ename)`: redraw an entity (the canvas redraws on its own).
pub fn entupd(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let h = ename(&a[0])?;
    ctx.s.touch();
    Ok(Flow::Value(if ctx.s.doc().is_ok_and(|d| d.entity(h).is_some()) { Value::Ename(h) } else { Value::Nil }))
}

/// `(entdel ename)`: delete an entity, or bring back one this routine's drawing deleted.
pub fn entdel(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let h = ename(&a[0])?;
    let Ok(d) = ctx.s.doc_mut() else { return Ok(Flow::Value(Value::Nil)) };
    if let Some(space) = d.space_of(h) {
        if let Some(e) = d.remove_entity(h) {
            ctx.env.deleted.insert(h, (space, e));
            ctx.s.touch();
            return Ok(Flow::Value(Value::Ename(h)));
        }
        return Ok(Flow::Value(Value::Nil));
    }
    match ctx.env.deleted.remove(&h) {
        Some((space, e)) => {
            let restored = d.space_mut(&space).map(|store| store.push(Entity::clone(&e))).is_some();
            ctx.s.touch();
            Ok(Flow::Value(if restored { Value::Ename(h) } else { Value::Nil }))
        }
        None => Ok(Flow::Value(Value::Nil)),
    }
}

/// Every entity's handle in creation order (model space and layouts), cached per drawing.
fn handles_in_order(ctx: &mut Ctx<'_>) -> Arc<Vec<Handle>> {
    let Ok(st) = ctx.s.state() else { return Arc::default() };
    let key = Arc::as_ptr(&st.doc) as usize;
    if let Some((k, v)) = &ctx.env.order
        && *k == key
    {
        return v.clone();
    }
    let d = &st.doc;
    let mut v: Vec<Handle> = d.model.handles();
    for l in &d.layouts {
        v.extend(l.entities.handles());
    }
    v.sort_unstable();
    let v = Arc::new(v);
    ctx.env.order = Some((key, v.clone()));
    v
}

pub fn entlast(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 0, 0)?;
    Ok(Flow::Value(handles_in_order(ctx).last().map(|h| Value::Ename(*h)).unwrap_or_default()))
}

pub fn entnext(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 0, 1)?;
    let order = handles_in_order(ctx);
    let next = match a.first() {
        None | Some(Value::Nil) => order.first().copied(),
        Some(v) => {
            let h = ename(v)?;
            let i = order.partition_point(|x| *x <= h);
            order.get(i).copied()
        }
    };
    Ok(Flow::Value(next.map(Value::Ename).unwrap_or_default()))
}

pub fn handent(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let s = string(&a[0])?;
    let h = Handle::parse_hex(&s).filter(|h| ctx.s.doc().is_ok_and(|d| d.entity(*h).is_some()));
    Ok(Flow::Value(h.map(Value::Ename).unwrap_or_default()))
}

// ---------------- selection sets ----------------

/// `(ssget [mode] [pt1 [pt2]] [filter])`. Modes: "X" every entity, "L" the last, "P" the
/// previous selection, "I" the implied (pickfirst) one, "W"/"C" window and crossing with two
/// points, "F" fence, "WP"/"CP" polygons with a point list. With no mode the pickfirst
/// selection, or the user selects objects. The filter is a list of DXF groups (`-4` operators,
/// wildcards). nil when nothing matches.
pub fn ssget(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    let mut args = a.into_iter().peekable();
    let mode = match args.peek() {
        Some(Value::Str(s)) => {
            let m = s.trim_start_matches(['_', '+', '.', ':']).to_ascii_uppercase();
            args.next();
            Some(m)
        }
        _ => None,
    };
    let rest: Vec<Value> = args.collect();
    // A trailing list of dotted pairs is the filter.
    let (pts, filter) = match rest.last() {
        Some(f @ Value::Cons(_)) if f.car().car().as_number().is_some() && f.car().as_point().is_none() => {
            (rest[..rest.len() - 1].to_vec(), Some(f.clone()))
        }
        _ => (rest, None),
    };
    let point = |i: usize| -> Result<Vec2, LispError> {
        let v = pts.get(i).ok_or_else(|| args_err(true))?;
        let p = v.as_point().ok_or_else(|| LispError::bad_arg("2D/3D point", v))?;
        Ok(Vec2::new(p[0], p[1]))
    };
    let s = &mut *ctx.s;
    let all = |s: &Session, all_spaces: bool| -> Vec<Handle> {
        let Ok(d) = s.doc() else { return Vec::new() };
        if all_spaces {
            let mut v = d.model.handles();
            for l in &d.layouts {
                v.extend(l.entities.handles());
            }
            v
        } else {
            d.space(&s.state().map(|st| st.edit_space()).unwrap_or(Space::Model)).map(|st| st.handles()).unwrap_or_default()
        }
    };
    let space = s.state().map(|st| st.edit_space()).unwrap_or(Space::Model);
    let mut found: Vec<Handle> = match mode.as_deref() {
        Some("X") => all(s, true),
        Some("L") => {
            let order = handles_in_order(ctx);
            order.last().copied().into_iter().collect()
        }
        Some("P") => ctx.s.state().map(|st| st.previous_selection.clone()).unwrap_or_default(),
        Some("I") => ctx.s.selection(),
        Some("W") | Some("C") => {
            let (p1, p2) = (point(0)?, point(1)?);
            let s = &*ctx.s;
            let Ok(d) = s.doc() else { return Ok(Flow::Value(Value::Nil)) };
            crate::select::select_window(d, &space, cadcraft_geom::Bounds2::from_points([p1, p2]), mode.as_deref() == Some("C"))
        }
        Some("F") | Some("WP") | Some("CP") => {
            let poly: Vec<Vec2> =
                pts.first().map(|l| l.iter().filter_map(|p| p.as_point().map(|q| Vec2::new(q[0], q[1]))).collect()).unwrap_or_default();
            let s = &*ctx.s;
            let Ok(d) = s.doc() else { return Ok(Flow::Value(Value::Nil)) };
            match mode.as_deref() {
                Some("F") => crate::select::select_fence(d, &space, &poly),
                m => crate::select::select_polygon(d, &space, &poly, m == Some("CP")),
            }
        }
        Some(":S") | Some("A") | None => {
            let implied = ctx.s.selection();
            if !implied.is_empty() {
                implied
            } else if pts.is_empty() {
                // The user selects objects; the result arrives when SELECT ends.
                return Ok(Flow::Value(Value::Nil)).and_then(|_| interactive_ssget(ctx, filter));
            } else {
                // A point: the object there.
                let p = point(0)?;
                let s = &*ctx.s;
                let ap = s.settings.pickbox.max(1.0) * s.pixel_size();
                let Ok(d) = s.doc() else { return Ok(Flow::Value(Value::Nil)) };
                crate::select::pick(d, &space, p, ap).into_iter().collect()
            }
        }
        Some(m) => return Err(LispError::new(format!("bad argument value: ssget mode {m}"))),
    };
    if let Some(f) = &filter {
        let d = ctx.s.doc().map_err(|e| LispError::new(e.to_string()))?;
        let f = parse_filter(f)?;
        found.retain(|h| d.entity(*h).is_some_and(|e| matches_filter(d, e, &f)));
    }
    if found.is_empty() {
        return Ok(Flow::Value(Value::Nil));
    }
    Ok(Flow::Value(new_pickset(ctx, found)))
}

/// `ssget` asking the user: the SELECT command collects the objects.
fn interactive_ssget(ctx: &mut Ctx<'_>, filter: Option<Value>) -> Result<Flow, LispError> {
    if ctx.s.running.is_some() {
        return Ok(Flow::Value(Value::Nil));
    }
    if let Err(e) = ctx.s.start("select") {
        return Err(LispError::new(e.to_string()));
    }
    let f = super::eval::lookup(ctx.env, &super::value::Sym::new("SSGET"));
    // When SELECT ends, its objects are the previous selection: (ssget "P" [filter]).
    let mut args = vec![Value::str("P")];
    args.extend(filter);
    Ok(Flow::WaitThen(Need::Pause, f, args))
}

/// A parsed `ssget` filter.
enum Filter {
    /// One group to test, with the relational operator of a preceding `(-4 . "op")`.
    Group {
        code: i32,
        value: Value,
        op: String,
    },
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Xor(Vec<Filter>),
    Not(Box<Filter>),
}

fn parse_filter(list: &Value) -> Result<Filter, LispError> {
    let items = list.to_vec();
    let mut i = 0;
    let mut out = Vec::new();
    while i < items.len() {
        out.push(parse_filter_item(&items, &mut i, 0)?);
    }
    Ok(Filter::And(out))
}

fn parse_filter_item(items: &[Value], i: &mut usize, depth: usize) -> Result<Filter, LispError> {
    if depth > 64 {
        return Err(LispError::new("bad ssget filter: nested too deeply"));
    }
    let Some(item) = items.get(*i) else { return Err(LispError::new("bad ssget filter")) };
    *i += 1;
    let code = item.car().as_number().map(|c| c as i32).ok_or_else(|| LispError::bad_arg("ssget filter", item))?;
    let value = item.cdr();
    if code != -4 {
        return Ok(Filter::Group { code, value, op: "=".into() });
    }
    let op = match &value {
        Value::Str(s) => s.to_ascii_uppercase(),
        other => return Err(LispError::bad_arg("stringp", other)),
    };
    let group = |items: &[Value], i: &mut usize, depth: usize, end: &str| -> Result<Vec<Filter>, LispError> {
        let mut v = Vec::new();
        loop {
            let Some(next) = items.get(*i) else { return Err(LispError::new(format!("bad ssget filter: no {end}"))) };
            if next.car().as_number() == Some(-4.0) && matches!(next.cdr(), Value::Str(ref s) if s.eq_ignore_ascii_case(end)) {
                *i += 1;
                return Ok(v);
            }
            v.push(parse_filter_item(items, i, depth + 1)?);
        }
    };
    Ok(match op.as_str() {
        "<AND" => Filter::And(group(items, i, depth, "AND>")?),
        "<OR" => Filter::Or(group(items, i, depth, "OR>")?),
        "<XOR" => Filter::Xor(group(items, i, depth, "XOR>")?),
        "<NOT" => {
            let mut v = group(items, i, depth, "NOT>")?;
            Filter::Not(Box::new(if v.len() == 1 { v.remove(0) } else { Filter::And(v) }))
        }
        _ => {
            // A relational operator applies to the next group.
            let Some(next) = items.get(*i) else { return Err(LispError::new("bad ssget filter")) };
            *i += 1;
            let code = next.car().as_number().map(|c| c as i32).ok_or_else(|| LispError::bad_arg("ssget filter", next))?;
            Filter::Group { code, value: next.cdr(), op }
        }
    })
}

fn matches_filter(d: &cadcraft_doc::Drawing, e: &Entity, f: &Filter) -> bool {
    let groups = || match crate::cmd::file::entity_codec() {
        Some(c) => {
            let (kind, tags) = (c.groups)(d, e);
            let mut l = vec![Value::cons(Value::Int(0), Value::str(&kind))];
            l.extend(groups_to_list(&tags));
            l
        }
        None => vec![Value::cons(Value::Int(0), Value::str(e.kind.dxf_name())), Value::cons(Value::Int(8), Value::str(&e.common.layer))],
    };
    let groups = groups();
    eval_filter(&groups, f)
}

fn eval_filter(groups: &[Value], f: &Filter) -> bool {
    match f {
        Filter::And(v) => v.iter().all(|x| eval_filter(groups, x)),
        Filter::Or(v) => v.iter().any(|x| eval_filter(groups, x)),
        Filter::Xor(v) => v.iter().filter(|x| eval_filter(groups, x)).count() == 1,
        Filter::Not(x) => !eval_filter(groups, x),
        Filter::Group { code, value, op } => {
            let found: Vec<Value> = groups.iter().filter(|g| g.car().as_number() == Some(f64::from(*code))).map(Value::cdr).collect();
            if found.is_empty() {
                // A group the entity doesn't have: layer 0 / colour BYLAYER defaults count.
                return match (code, value) {
                    (62, Value::Int(256)) => op == "=",
                    _ => false,
                };
            }
            found.iter().any(|have| group_matches(have, value, op))
        }
    }
}

fn group_matches(have: &Value, want: &Value, op: &str) -> bool {
    if op == "*" {
        return true;
    }
    match (have, want) {
        (Value::Str(h), Value::Str(w)) => {
            // Strings match as wildcards, case-insensitively (type and layer names).
            let hit = super::builtins::wild_match(&h.to_uppercase(), &w.to_uppercase());
            if op == "!=" || op == "/=" || op == "<>" { !hit } else { hit }
        }
        _ => {
            if let (Some(h), Some(w)) = (have.as_number(), want.as_number()) {
                return compare(h, w, op);
            }
            if let (Some(h), Some(w)) = (have.as_point(), want.as_point()) {
                // Points: an operator per coordinate ("<,>,*") or one for all.
                let ops: Vec<&str> = op.split(',').map(str::trim).collect();
                return (0..3).all(|i| compare(h[i], w[i], ops.get(i).or(ops.first()).copied().unwrap_or("=")));
            }
            super::value::equal(have, want, 0.0)
        }
    }
}

fn compare(h: f64, w: f64, op: &str) -> bool {
    match op {
        "=" => h == w,
        "!=" | "/=" | "<>" => h != w,
        "<" => h < w,
        "<=" => h <= w,
        ">" => h > w,
        ">=" => h >= w,
        "&" => (h as i64) & (w as i64) != 0,
        "&=" => (h as i64) & (w as i64) == (w as i64),
        "*" => true,
        _ => h == w,
    }
}

pub fn ssadd(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 0, 2)?;
    match (a.first(), a.get(1)) {
        (None, _) => Ok(Flow::Value(new_pickset(ctx, Vec::new()))),
        (Some(e), None) => {
            let h = ename(e)?;
            Ok(Flow::Value(new_pickset(ctx, vec![h])))
        }
        (Some(e), Some(ss)) => {
            let h = ename(e)?;
            let p = pickset(ss)?;
            if let Ok(mut v) = p.items.lock()
                && !v.contains(&h)
            {
                v.push(h);
            }
            Ok(Flow::Value(ss.clone()))
        }
    }
}

pub fn ssdel(_ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 2)?;
    let h = ename(&a[0])?;
    let p = pickset(&a[1])?;
    let removed = p.items.lock().map(|mut v| {
        let n = v.len();
        v.retain(|x| *x != h);
        n != v.len()
    });
    Ok(Flow::Value(if removed.unwrap_or(false) { a[1].clone() } else { Value::Nil }))
}

pub fn sslength(_ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    Ok(Flow::Value(Value::Int(i32::try_from(items(&pickset(&a[0])?).len()).unwrap_or(i32::MAX))))
}

pub fn ssname(_ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 2)?;
    let p = pickset(&a[0])?;
    let i = a[1].as_number().ok_or_else(|| LispError::bad_arg("numberp", &a[1]))?;
    if i < 0.0 {
        return Ok(Flow::Value(Value::Nil));
    }
    Ok(Flow::Value(items(&p).get(i as usize).map(|h| Value::Ename(*h)).unwrap_or_default()))
}

pub fn ssmemb(_ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 2)?;
    let h = ename(&a[0])?;
    Ok(Flow::Value(if items(&pickset(&a[1])?).contains(&h) { Value::Ename(h) } else { Value::Nil }))
}

pub fn sssetfirst(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 2)?;
    let sel = match a.get(1) {
        Some(Value::PickSet(p)) => items(p),
        _ => Vec::new(),
    };
    ctx.s.set_selection(sel);
    Ok(Flow::Value(Value::list(a)))
}

pub fn ssgetfirst(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 0, 0)?;
    let sel = ctx.s.selection();
    let ss = if sel.is_empty() { Value::Nil } else { new_pickset(ctx, sel) };
    Ok(Flow::Value(Value::list(vec![Value::Nil, ss])))
}

// ---------------- symbol tables ----------------

fn color_index(c: cadcraft_color::Color) -> i32 {
    match c {
        cadcraft_color::Color::Index(i) => i32::from(i),
        cadcraft_color::Color::ByBlock => 0,
        cadcraft_color::Color::ByLayer => 256,
        cadcraft_color::Color::True(_) => 7,
    }
}

/// The records of a symbol table, as `tblsearch` lists them.
fn table_records(d: &cadcraft_doc::Drawing, table: &str) -> Vec<Value> {
    let pair = |c: i32, v: Value| Value::cons(Value::Int(c), v);
    match table {
        "LAYER" => d
            .layers
            .iter()
            .map(|l| {
                let flags = i32::from(l.frozen) | (i32::from(l.locked) << 2);
                let color = color_index(l.color);
                Value::list(vec![
                    pair(0, Value::str("LAYER")),
                    pair(2, Value::str(&l.name)),
                    pair(70, Value::Int(flags)),
                    pair(62, Value::Int(if l.on { color } else { -color })),
                    pair(6, Value::str(&l.linetype)),
                ])
            })
            .collect(),
        "LTYPE" => d
            .linetypes
            .iter()
            .map(|l| {
                Value::list(vec![
                    pair(0, Value::str("LTYPE")),
                    pair(2, Value::str(&l.name)),
                    pair(70, Value::Int(0)),
                    pair(3, Value::str(&l.description)),
                ])
            })
            .collect(),
        "STYLE" => d
            .text_styles
            .iter()
            .map(|s| {
                Value::list(vec![
                    pair(0, Value::str("STYLE")),
                    pair(2, Value::str(&s.name)),
                    pair(70, Value::Int(0)),
                    pair(40, Value::Real(s.height)),
                    pair(41, Value::Real(s.width_factor)),
                    pair(3, Value::str(&s.font)),
                ])
            })
            .collect(),
        "BLOCK" => d
            .blocks
            .values()
            .map(|b| {
                Value::list(vec![
                    pair(0, Value::str("BLOCK")),
                    pair(2, Value::str(&b.name)),
                    pair(70, Value::Int(0)),
                    Value::list(vec![Value::Int(10), Value::Real(b.base.x), Value::Real(b.base.y), Value::Real(b.base.z)]),
                ])
            })
            .collect(),
        "DIMSTYLE" => d
            .dim_styles
            .iter()
            .map(|s| Value::list(vec![pair(0, Value::str("DIMSTYLE")), pair(2, Value::str(&s.name)), pair(70, Value::Int(0))]))
            .collect(),
        "APPID" => vec![Value::list(vec![pair(0, Value::str("APPID")), pair(2, Value::str("ACAD")), pair(70, Value::Int(0))])],
        _ => Vec::new(),
    }
}

fn record_name(r: &Value) -> String {
    r.iter().find(|p| p.car().as_number() == Some(2.0)).map(|p| to_text(&p.cdr(), false)).unwrap_or_default()
}

pub fn tblsearch(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 3)?;
    let table = string(&a[0])?.to_ascii_uppercase();
    let name = string(&a[1])?;
    let Ok(d) = ctx.s.doc() else { return Ok(Flow::Value(Value::Nil)) };
    let records = table_records(d, &table);
    let found = records.iter().position(|r| record_name(r).eq_ignore_ascii_case(&name));
    if let Some(i) = found
        && a.get(2).is_some_and(|v| !v.is_nil())
    {
        ctx.env.tblnext.insert(table.clone(), i + 1);
    }
    Ok(Flow::Value(found.and_then(|i| records.get(i).cloned()).unwrap_or_default()))
}

pub fn tblnext(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 2)?;
    let table = string(&a[0])?.to_ascii_uppercase();
    let rewind = a.get(1).is_some_and(|v| !v.is_nil());
    let Ok(d) = ctx.s.doc() else { return Ok(Flow::Value(Value::Nil)) };
    let records = table_records(d, &table);
    let i = if rewind { 0 } else { ctx.env.tblnext.get(&table).copied().unwrap_or(0) };
    ctx.env.tblnext.insert(table, i + 1);
    Ok(Flow::Value(records.get(i).cloned().unwrap_or_default()))
}

// ---------------- units ----------------

fn int_var(s: &Session, name: &str, default: i64) -> i64 {
    crate::sysvars::get(s, name).and_then(|v| v.as_i64()).unwrap_or(default)
}

/// `(rtos number [mode [precision]])`: as LUNITS/LUPREC by default; decimal results honour
/// DIMZIN's zero suppression (4 leading, 8 trailing).
pub fn rtos(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 3)?;
    let v = a[0].as_number().ok_or_else(|| LispError::bad_arg("numberp", &a[0]))?;
    let mode = match a.get(1) {
        Some(Value::Int(m)) => i64::from(*m),
        _ => int_var(ctx.s, "LUNITS", 2),
    };
    let prec = match a.get(2) {
        Some(Value::Int(p)) => i64::from(*p),
        _ => int_var(ctx.s, "LUPREC", 4),
    };
    if !(1..=5).contains(&mode) {
        return Err(LispError::new(format!("bad argument value: rtos mode {mode}")));
    }
    let text = if mode == 2 {
        let dimzin = int_var(ctx.s, "DIMZIN", 0);
        decimal(v, prec.clamp(0, 8) as usize, dimzin)
    } else {
        crate::units::format_distance(v, mode, prec)
    };
    Ok(Flow::Value(Value::str(&text)))
}

fn decimal(v: f64, prec: usize, dimzin: i64) -> String {
    let mut s = format!("{v:.prec$}");
    if s.starts_with("-0") && s.trim_start_matches(['-', '0', '.']).is_empty() {
        s = s.trim_start_matches('-').to_string();
    }
    if dimzin & 8 != 0 && s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if dimzin & 4 != 0 {
        if let Some(r) = s.strip_prefix("0.") {
            s = format!(".{r}");
        } else if let Some(r) = s.strip_prefix("-0.") {
            s = format!("-.{r}");
        }
    }
    s
}

pub fn angtos(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 3)?;
    let v = a[0].as_number().ok_or_else(|| LispError::bad_arg("numberp", &a[0]))?;
    let mode = match a.get(1) {
        Some(Value::Int(m)) => i64::from(*m),
        _ => int_var(ctx.s, "AUNITS", 0),
    };
    let prec = match a.get(2) {
        Some(Value::Int(p)) => i64::from(*p),
        _ => int_var(ctx.s, "AUPREC", 0),
    };
    Ok(Flow::Value(Value::str(&crate::units::format_angle(v, mode, prec))))
}

pub fn distof(_ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 2)?;
    let s = string(&a[0])?;
    Ok(Flow::Value(crate::units::parse_distance(&s).map(Value::Real).unwrap_or_default()))
}

pub fn angtof(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 2)?;
    let s = string(&a[0])?;
    Ok(Flow::Value(ctx.s.angle_settings().direction(&s).map(Value::Real).unwrap_or_default()))
}

/// `(trans pt from to [disp])`: points are in the world system throughout (no UCS yet).
pub fn trans(_ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 3, 4)?;
    let p = a[0].as_point().ok_or_else(|| LispError::bad_arg("2D/3D point", &a[0]))?;
    Ok(Flow::Value(Value::point(&p)))
}

/// What `(load name)` reads: a file loaded through APPLOAD (the web), or one on disk, with
/// `.lsp` added when the name has no extension.
pub fn load_source(s: &Session, name: &str) -> Option<String> {
    if let Some(t) = s.lisp_file(name) {
        return Some(t);
    }
    let candidates =
        if std::path::Path::new(name).extension().is_none() { vec![format!("{name}.lsp"), name.to_string()] } else { vec![name.to_string()] };
    // As given (a path, or relative to the working folder), then in the support folder.
    let support =
        s.lisp_support_dir().map(|d| candidates.iter().map(|c| d.join(c).to_string_lossy().into_owned()).collect::<Vec<_>>()).unwrap_or_default();
    candidates.iter().chain(&support).find_map(|c| super::builtins::read_text_file(c))
}

// ---------------- Visual LISP ActiveX (a minimal set) ----------------
//
// A VLA object is the entity's name: `vlax-ename->vla-object` returns the ename, so a VLA
// object works everywhere an ename does (`entget`, `entmod`) and every `vlax-curve-…`
// function takes either. A safearray is a list (what `vla-getboundingbox` writes into its
// two output variables), so `vlax-safearray->list` passes a list through unchanged.
//
// Curve parameters are arc length from the curve's start, so `getparamatpoint` and
// `getfirstderiv` agree with each other; the derivative is the unit tangent.

fn vla_handle(v: &Value) -> Result<Handle, LispError> {
    match v {
        Value::Ename(h) => Ok(*h),
        other => Err(LispError::bad_arg("lentityp", other)),
    }
}

/// The entity list `entget` returns for `h`.
fn entget_value(ctx: &mut Ctx<'_>, h: Handle) -> Option<Value> {
    match entget(ctx, vec![Value::Ename(h)]) {
        Ok(Flow::Value(v)) => Some(v),
        _ => None,
    }
}

fn assoc_value(list: &Value, code: i32) -> Option<Value> {
    list.iter().find(|v| v.car().as_number() == Some(f64::from(code))).map(|v| v.cdr())
}

/// A property name as written (`TextString`, `TEXTSTRING`, `text-string`) in one form.
fn prop_name(v: &Value) -> Result<String, LispError> {
    let raw = match v {
        Value::Str(s) => s.to_string(),
        Value::Sym(s) => s.name().to_string(),
        other => return Err(LispError::bad_arg("stringp", other)),
    };
    Ok(raw.chars().filter(|c| *c != '-' && *c != '_' && *c != ' ').collect::<String>().to_ascii_uppercase())
}

/// The DXF group a property reads and writes, when it is a plain group.
fn prop_group(prop: &str) -> Option<i32> {
    Some(match prop {
        "TEXTSTRING" | "CONTENTS" => 1,
        "STYLENAME" => 7,
        "LAYER" => 8,
        "LINETYPE" => 6,
        "COLOR" => 62,
        "LINETYPESCALE" => 48,
        "HEIGHT" | "RADIUS" => 40,
        "ROTATION" | "STARTANGLE" => 50,
        "ENDANGLE" => 51,
        "INSERTIONPOINT" | "CENTER" => 10,
        "ALIGNMENTPOINT" => 11,
        "CONSTANTWIDTH" => 43,
        "ELEVATION" => 38,
        "THICKNESS" => 39,
        _ => return None,
    })
}

fn is_computed(prop: &str) -> bool {
    matches!(prop, "BOUNDINGBOX" | "LENGTH" | "AREA" | "STARTPOINT" | "ENDPOINT" | "OBJECTNAME" | "HANDLE" | "CLOSED")
}

/// The value of a computed property, or `None` when the entity has none.
fn computed_property(ctx: &mut Ctx<'_>, h: Handle, prop: &str) -> Option<Value> {
    let Ok(d) = ctx.s.doc() else { return None };
    let e = d.entity(h)?;
    match prop {
        "OBJECTNAME" => Some(Value::str(e.kind.dxf_name())),
        "HANDLE" => Some(Value::str(&h.hex())),
        "BOUNDINGBOX" => {
            let b = cadcraft_doc::entity_bounds(d, e, 0);
            if b.is_empty() {
                return None;
            }
            Some(Value::list(vec![Value::point(&[b.min.x, b.min.y, 0.0]), Value::point(&[b.max.x, b.max.y, 0.0])]))
        }
        "LENGTH" => curve_length(e).map(Value::Real),
        "AREA" => curve_area(e).map(Value::Real),
        "STARTPOINT" => curve_start(e).map(|p| Value::point(&p)),
        "ENDPOINT" => curve_end(e).map(|p| Value::point(&p)),
        "CLOSED" => {
            let list = entget_value(ctx, h)?;
            let flags = assoc_value(&list, 70)?.as_number().unwrap_or(0.0) as i64;
            Some(Value::bool(flags & 1 != 0))
        }
        _ => None,
    }
}

pub fn vla_get_property(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 2)?;
    let h = vla_handle(&a[0])?;
    let prop = prop_name(&a[1])?;
    if is_computed(&prop) {
        return Ok(Flow::Value(computed_property(ctx, h, &prop).unwrap_or_default()));
    }
    let Some(code) = prop_group(&prop) else {
        return Err(LispError::new(format!("bad property name: {}", a[1].as_sym().map_or_else(|| to_text(&a[1], false), |s| s.name().to_string()))));
    };
    let list = entget_value(ctx, h).unwrap_or_default();
    Ok(Flow::Value(assoc_value(&list, code).unwrap_or_default()))
}

pub fn vla_put_property(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 3, 3)?;
    let h = vla_handle(&a[0])?;
    let prop = prop_name(&a[1])?;
    if matches!(prop.as_str(), "BOUNDINGBOX" | "LENGTH" | "AREA" | "STARTPOINT" | "ENDPOINT" | "OBJECTNAME" | "HANDLE") {
        return Err(LispError::new(format!("bad property: {prop} is read-only")));
    }
    let mut list = entget_value(ctx, h).unwrap_or_default();
    if prop == "CLOSED" {
        let flags = assoc_value(&list, 70).and_then(|v| v.as_number()).unwrap_or(0.0) as i64;
        let flags = if a[2].is_nil() { flags & !1 } else { flags | 1 };
        list = subst_group(&list, 70, Value::Int(i32::try_from(flags).unwrap_or(0)));
    } else {
        let Some(code) = prop_group(&prop) else { return Err(LispError::new(format!("bad property name: {}", to_text(&a[1], false)))) };
        let v = check_group_value(code, &a[2])?;
        list = subst_group(&list, code, v);
    }
    match entmod(ctx, vec![list]) {
        Ok(Flow::Value(Value::Nil)) => Ok(Flow::Value(Value::Nil)),
        Ok(_) => Ok(Flow::Value(a[2].clone())),
        Err(e) => Err(e),
    }
}

/// A value fit for DXF group `code`: strings for names and text, numbers for the rest, a
/// point for point groups.
fn check_group_value(code: i32, v: &Value) -> Result<Value, LispError> {
    match code {
        1 | 6 | 7 | 8 => match v {
            Value::Str(_) => Ok(v.clone()),
            other => Err(LispError::bad_arg("stringp", other)),
        },
        10 | 11 => match v.as_point() {
            Some(p) => Ok(Value::point(&p)),
            None => Err(LispError::bad_arg("2D/3D point", v)),
        },
        _ => match v.as_number() {
            Some(n) => Ok(if matches!(v, Value::Int(_)) { Value::Int(n as i32) } else { Value::Real(n) }),
            None => Err(LispError::bad_arg("numberp", v)),
        },
    }
}

/// `list` with group `code` set to `v` (replaced, or appended when absent).
fn subst_group(list: &Value, code: i32, v: Value) -> Value {
    let mut items = list.to_vec();
    let mut found = false;
    for item in &mut items {
        if item.car().as_number() == Some(f64::from(code)) {
            *item = Value::cons(Value::Int(code), v.clone());
            found = true;
        }
    }
    if !found {
        items.push(Value::cons(Value::Int(code), v));
    }
    Value::list(items)
}

pub fn vla_get_textstring(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let h = vla_handle(&a[0])?;
    let list = entget_value(ctx, h).unwrap_or_default();
    Ok(Flow::Value(assoc_value(&list, 1).unwrap_or_default()))
}

pub fn vla_put_textstring(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 2)?;
    let h = vla_handle(&a[0])?;
    let s = match &a[1] {
        Value::Str(s) => s.to_string(),
        other => return Err(LispError::bad_arg("stringp", other)),
    };
    let mut list = entget_value(ctx, h).unwrap_or_default();
    list = subst_group(&list, 1, Value::str(&s));
    match entmod(ctx, vec![list]) {
        Ok(Flow::Value(Value::Nil)) => Ok(Flow::Value(Value::Nil)),
        Ok(_) => Ok(Flow::Value(Value::str(&s))),
        Err(e) => Err(e),
    }
}

/// `(vla-getboundingbox obj 'min 'max)`: writes the two corner points into the named
/// variables (nil when the entity has no bounds).
pub fn vla_getboundingbox(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 3, 3)?;
    let h = vla_handle(&a[0])?;
    let mut names = Vec::new();
    for v in &a[1..] {
        names.push(match v {
            Value::Sym(s) => s.clone(),
            Value::Str(s) => super::value::Sym::new(s),
            other => return Err(LispError::bad_arg("symbolp", other)),
        });
    }
    let Some(bounds) = computed_property(ctx, h, "BOUNDINGBOX") else { return Ok(Flow::Value(Value::Nil)) };
    let mut it = bounds.iter();
    let (Some(ll), Some(ur)) = (it.next(), it.next()) else { return Ok(Flow::Value(Value::Nil)) };
    ctx.env.set(&names[0], ll);
    ctx.env.set(&names[1], ur);
    Ok(Flow::Value(Value::Nil))
}

pub fn vlax_ename_to_vla_object(_ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    Ok(Flow::Value(Value::Ename(vla_handle(&a[0])?)))
}

pub fn vlax_safearray_to_list(_ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    Ok(Flow::Value(match &a[0] {
        Value::Cons(_) | Value::Nil => a[0].clone(),
        other => Value::list(vec![other.clone()]),
    }))
}

// ---------------- curves ----------------

/// A curve as a polyline: points, the distance along it to each, whether it closes.
struct Sample {
    pts: Vec<[f64; 3]>,
    cum: Vec<f64>,
    closed: bool,
    total: f64,
}

impl Sample {
    fn segments(&self) -> usize {
        if self.pts.len() < 2 {
            0
        } else if self.closed {
            self.pts.len()
        } else {
            self.pts.len() - 1
        }
    }

    fn seg(&self, i: usize) -> ([f64; 3], [f64; 3]) {
        let a = self.pts[i];
        let b = self.pts[if i + 1 < self.pts.len() { i + 1 } else { 0 }];
        (a, b)
    }

    fn seg_start(&self, i: usize) -> f64 {
        self.cum[i]
    }

    fn seg_len(&self, i: usize) -> f64 {
        let (a, b) = self.seg(i);
        dist(a, b)
    }
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn finish_sample(pts: Vec<[f64; 3]>, closed: bool) -> Option<Sample> {
    if pts.len() < 2 {
        return None;
    }
    let mut cum = vec![0.0];
    let mut total = 0.0;
    let n = if closed { pts.len() } else { pts.len() - 1 };
    for i in 0..n {
        let a = pts[i];
        let b = pts[if i + 1 < pts.len() { i + 1 } else { 0 }];
        total += dist(a, b);
        if i + 1 < pts.len() {
            cum.push(total);
        }
    }
    if !total.is_finite() || total <= 0.0 {
        return None;
    }
    Some(Sample { pts, cum, closed, total })
}

/// Points along an arc from `start`, sweeping `sweep` radians (either sign), `z` up.
fn arc_points(center: [f64; 3], radius: f64, start: f64, sweep: f64, z: f64) -> Vec<[f64; 3]> {
    let n = ((sweep.abs() / 0.05).ceil() as usize).clamp(4, 512);
    (0..=n)
        .map(|i| {
            let t = start + sweep * i as f64 / n as f64;
            [center[0] + radius * t.cos(), center[1] + radius * t.sin(), z]
        })
        .collect()
}

fn sample_curve(e: &Entity) -> Option<Sample> {
    match &e.kind {
        EntityKind::Line(l) => finish_sample(vec![[l.a.x, l.a.y, l.a.z], [l.b.x, l.b.y, l.b.z]], false),
        EntityKind::LwPolyline(p) => {
            let vs = &p.vertices;
            if vs.len() < 2 {
                return None;
            }
            let mut pts = vec![[vs[0].p.x, vs[0].p.y, p.elevation]];
            let segs = if p.closed { vs.len() } else { vs.len() - 1 };
            for i in 0..segs {
                let a = vs[i].p;
                let b = vs[(i + 1) % vs.len()].p;
                if vs[i].bulge.abs() > 1e-12
                    && let Some((arc, ccw)) = cadcraft_geom::bulge_to_arc(a, b, vs[i].bulge)
                {
                    let sweep = if ccw {
                        (arc.end - arc.start).rem_euclid(std::f64::consts::TAU)
                    } else {
                        -((arc.start - arc.end).rem_euclid(std::f64::consts::TAU))
                    };
                    let mut arc_pts = arc_points([arc.center.x, arc.center.y, p.elevation], arc.radius, arc.start, sweep, p.elevation);
                    arc_pts.remove(0);
                    pts.extend(arc_pts);
                    continue;
                }
                pts.push([b.x, b.y, p.elevation]);
            }
            if p.closed && pts.len() > 1 && pts.last().is_some_and(|q| *q == pts[0]) {
                pts.pop();
            }
            finish_sample(pts, p.closed)
        }
        EntityKind::Arc(a) => {
            let mut sweep = (a.end - a.start).rem_euclid(std::f64::consts::TAU);
            if sweep == 0.0 {
                sweep = std::f64::consts::TAU;
            }
            finish_sample(arc_points([a.center.x, a.center.y, a.center.z], a.radius, a.start, sweep, a.center.z), false)
        }
        EntityKind::Circle(c) => {
            let pts = arc_points([c.center.x, c.center.y, c.center.z], c.radius, 0.0, std::f64::consts::TAU, c.center.z);
            let mut pts = pts;
            pts.pop();
            finish_sample(pts, true)
        }
        EntityKind::Ellipse(el) => {
            let major = el.major;
            let len = (major.x * major.x + major.y * major.y).sqrt();
            if len <= 0.0 || !el.ratio.is_finite() {
                return None;
            }
            let minor = [(-major.y / len) * len * el.ratio, (major.x / len) * len * el.ratio, 0.0];
            let mut sweep = (el.end - el.start).rem_euclid(std::f64::consts::TAU);
            if sweep == 0.0 {
                sweep = std::f64::consts::TAU;
            }
            let n = ((sweep.abs() / 0.05).ceil() as usize).clamp(8, 512);
            let pts: Vec<[f64; 3]> = (0..=n)
                .map(|i| {
                    let t = el.start + sweep * i as f64 / n as f64;
                    [
                        el.center.x + major.x * t.cos() + minor[0] * t.sin(),
                        el.center.y + major.y * t.cos() + minor[1] * t.sin(),
                        el.center.z + major.z * t.cos(),
                    ]
                })
                .collect();
            finish_sample(pts, sweep >= std::f64::consts::TAU - 1e-9)
        }
        EntityKind::Spline(s) => {
            let pts: Vec<[f64; 3]> = s.tessellate(1e-3).into_iter().map(|p| [p.x, p.y, 0.0]).collect();
            finish_sample(pts, false)
        }
        EntityKind::Polyline3d(p) => {
            let pts: Vec<[f64; 3]> = p.points.iter().map(|v| [v.x, v.y, v.z]).collect();
            finish_sample(pts, p.closed)
        }
        EntityKind::Helix(h) => {
            let pts: Vec<[f64; 3]> = h.spline().tessellate(1e-3).into_iter().map(|p| [p.x, p.y, 0.0]).collect();
            finish_sample(pts, false)
        }
        _ => None,
    }
}

/// The curve's length, or `None` when it isn't a curve.
fn curve_length(e: &Entity) -> Option<f64> {
    match &e.kind {
        EntityKind::Line(l) => Some(dist([l.a.x, l.a.y, l.a.z], [l.b.x, l.b.y, l.b.z])),
        EntityKind::Circle(c) => Some(std::f64::consts::TAU * c.radius),
        EntityKind::Arc(a) => {
            let mut sweep = (a.end - a.start).rem_euclid(std::f64::consts::TAU);
            if sweep == 0.0 {
                sweep = std::f64::consts::TAU;
            }
            Some(a.radius * sweep)
        }
        _ => sample_curve(e).map(|s| s.total),
    }
}

/// The area a closed curve holds, or `None`.
fn curve_area(e: &Entity) -> Option<f64> {
    match &e.kind {
        EntityKind::Circle(c) => Some(std::f64::consts::PI * c.radius * c.radius),
        EntityKind::LwPolyline(p) if p.closed => {
            let mut a = 0.0;
            for i in 0..p.vertices.len() {
                let q = p.vertices[(i + 1) % p.vertices.len()].p;
                a += p.vertices[i].p.x * q.y - q.x * p.vertices[i].p.y;
            }
            Some(a.abs() / 2.0)
        }
        _ => None,
    }
}

fn curve_start(e: &Entity) -> Option<[f64; 3]> {
    match &e.kind {
        EntityKind::Ray(r) | EntityKind::XLine(r) => Some([r.base.x, r.base.y, r.base.z]),
        _ => sample_curve(e).and_then(|s| s.pts.first().copied()),
    }
}

fn curve_end(e: &Entity) -> Option<[f64; 3]> {
    match &e.kind {
        EntityKind::Ray(_) | EntityKind::XLine(_) => None,
        _ => sample_curve(e).map(|s| if s.closed { s.pts[0] } else { s.pts[s.pts.len() - 1] }),
    }
}

fn curve_entity(ctx: &mut Ctx<'_>, v: &Value) -> Result<Option<Entity>, LispError> {
    let h = vla_handle(v)?;
    let Ok(d) = ctx.s.doc() else { return Ok(None) };
    Ok(d.entity(h).map(|e| e.as_ref().clone()))
}

fn closest_on(s: &Sample, p: [f64; 3]) -> ([f64; 3], f64) {
    let mut best = (s.pts[0], 0.0, f64::INFINITY);
    for i in 0..s.segments() {
        let (a, b) = s.seg(i);
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let len2 = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
        let t = if len2 <= 0.0 { 0.0 } else { (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1] + (p[2] - a[2]) * ab[2]) / len2).clamp(0.0, 1.0) };
        let q = [a[0] + ab[0] * t, a[1] + ab[1] * t, a[2] + ab[2] * t];
        let d = dist(p, q);
        if d < best.2 {
            best = (q, s.seg_start(i) + t * s.seg_len(i), d);
        }
    }
    (best.0, best.1)
}

fn tangent_at(s: &Sample, param: f64) -> [f64; 3] {
    let t = param.clamp(0.0, s.total);
    for i in 0..s.segments() {
        if t <= s.seg_start(i) + s.seg_len(i) || i + 1 == s.segments() {
            let (a, b) = s.seg(i);
            let len = dist(a, b);
            if len <= 0.0 {
                return [0.0, 0.0, 0.0];
            }
            return [(b[0] - a[0]) / len, (b[1] - a[1]) / len, (b[2] - a[2]) / len];
        }
    }
    [0.0, 0.0, 0.0]
}

pub fn vlax_curve_getclosestpointto(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 2)?;
    let p = a[1].as_point().ok_or_else(|| LispError::bad_arg("2D/3D point", &a[1]))?;
    let Some(e) = curve_entity(ctx, &a[0])? else { return Ok(Flow::Value(Value::Nil)) };
    if let EntityKind::Ray(r) | EntityKind::XLine(r) = &e.kind {
        let d = [r.dir.x, r.dir.y, r.dir.z];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if len <= 0.0 {
            return Ok(Flow::Value(Value::Nil));
        }
        let n = [d[0] / len, d[1] / len, d[2] / len];
        let mut t = (p[0] - r.base.x) * n[0] + (p[1] - r.base.y) * n[1] + (p[2] - r.base.z) * n[2];
        if matches!(e.kind, EntityKind::Ray(_)) {
            t = t.max(0.0);
        }
        return Ok(Flow::Value(Value::point(&[r.base.x + n[0] * t, r.base.y + n[1] * t, r.base.z + n[2] * t])));
    }
    let Some(s) = sample_curve(&e) else { return Ok(Flow::Value(Value::Nil)) };
    Ok(Flow::Value(Value::point(&closest_on(&s, p).0)))
}

pub fn vlax_curve_getparamatpoint(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 2)?;
    let p = a[1].as_point().ok_or_else(|| LispError::bad_arg("2D/3D point", &a[1]))?;
    let Some(e) = curve_entity(ctx, &a[0])? else { return Ok(Flow::Value(Value::Nil)) };
    if let EntityKind::Ray(r) | EntityKind::XLine(r) = &e.kind {
        let d = [r.dir.x, r.dir.y, r.dir.z];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if len <= 0.0 {
            return Ok(Flow::Value(Value::Nil));
        }
        let mut t = ((p[0] - r.base.x) * d[0] + (p[1] - r.base.y) * d[1] + (p[2] - r.base.z) * d[2]) / len;
        if matches!(e.kind, EntityKind::Ray(_)) {
            t = t.max(0.0);
        }
        return Ok(Flow::Value(Value::Real(t)));
    }
    let Some(s) = sample_curve(&e) else { return Ok(Flow::Value(Value::Nil)) };
    Ok(Flow::Value(Value::Real(closest_on(&s, p).1)))
}

pub fn vlax_curve_getfirstderiv(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 2)?;
    let param = a[1].as_number().ok_or_else(|| LispError::bad_arg("numberp", &a[1]))?;
    let Some(e) = curve_entity(ctx, &a[0])? else { return Ok(Flow::Value(Value::Nil)) };
    if let EntityKind::Ray(r) | EntityKind::XLine(r) = &e.kind {
        let d = [r.dir.x, r.dir.y, r.dir.z];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if len <= 0.0 {
            return Ok(Flow::Value(Value::Nil));
        }
        return Ok(Flow::Value(Value::point(&[d[0] / len, d[1] / len, d[2] / len])));
    }
    let Some(s) = sample_curve(&e) else { return Ok(Flow::Value(Value::Nil)) };
    Ok(Flow::Value(Value::point(&tangent_at(&s, param))))
}

pub fn vlax_curve_getstartpoint(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let Some(e) = curve_entity(ctx, &a[0])? else { return Ok(Flow::Value(Value::Nil)) };
    Ok(Flow::Value(curve_start(&e).map(|p| Value::point(&p)).unwrap_or_default()))
}

pub fn vlax_curve_getendpoint(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let Some(e) = curve_entity(ctx, &a[0])? else { return Ok(Flow::Value(Value::Nil)) };
    Ok(Flow::Value(curve_end(&e).map(|p| Value::point(&p)).unwrap_or_default()))
}
