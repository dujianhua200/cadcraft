//! Utilities: CAL (expression calculator), FIND (find and replace text), MASSPROP, MEASUREGEOM.

use cadcraft_doc::{EntityKind, Handle};
use cadcraft_geom::Vec2;
use serde_json::{Value, json};

use super::*;
use crate::{Accept, Input, Interactive, Prompt, Result, Session, Step};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("cal", "QuickCalc", run_cal)
            .menu(&["Tools", "Inquiry", "Quick"])
            .alias(&["quickcalc", "qc"])
            .params(
                "{expr: \"(3+4)*2^2\" | \"dist([0,0],[3,4])\", vars?: {x: 1, p: [x, y]}} → {value} | {point} | \
                 {convert: {type?: length|area|volume|angle, from, to, value}} → {value} | {units: true} → unit lists | \
                 {pick: point|distance|angle|intersection, points}",
            )
            .enabled(always)
            .noundo()
            .transparent()
            .interactive(|_| Ok(Box::new(CalM))),
        CommandSpec::new("quickcalc.pick", "QuickCalc Pick", run_quickcalc_pick)
            .params("{mode: point|distance|angle|intersection, points: [[x,y],...]} → {value} | {point}")
            .noundo()
            .interactive(|_| Ok(Box::<PickM>::default())),
        CommandSpec::new("find", "Find...", run_find)
            .menu(&["Edit", "Find..."])
            .params("{find, replace?, matchCase?: bool, wholeWord?: bool, handles?: [hex]}")
            .interactive(|_| Ok(Box::<FindM>::default())),
        CommandSpec::new("measuregeom", "Measure Geometry", run_measuregeom)
            .params("{mode: distance|radius|angle|area, ...}")
            .noundo()
            .interactive(|_| Ok(Box::<MeasuregeomM>::default())),
        CommandSpec::new("radius.measure", "Radius", run_radius)
            .menu(&["Tools", "Inquiry", "Radius"])
            .params("{handle}")
            .noundo()
            .interactive(|_| Ok(Box::<RadiusM>::default())),
        CommandSpec::new("angle.measure", "Angle", run_angle)
            .menu(&["Tools", "Inquiry", "Angle"])
            .params("{vertex, p1, p2} | {h1, h2}")
            .noundo()
            .interactive(|_| Ok(Box::<AngleM>::default())),
    ]
}

// ---------------- CAL ----------------

mod calc;

/// What QuickCalc's "get from the drawing" picks put on the command line before the value (the
/// palette reads it back from there).
const PICKED: &str = "QuickCalc: ";

/// Evaluate an arithmetic expression to a number (degrees for trig, `^` power, functions, pi).
/// Expressions with points are evaluated by [`calc::eval_val`].
pub fn eval(expr: &str, vars: &serde_json::Map<String, Value>) -> std::result::Result<f64, String> {
    match calc::eval_val(expr, vars)? {
        calc::Val::Num(v) => Ok(v),
        calc::Val::Pt(_) => Err("the result is a point, not a number".into()),
    }
}

fn run_cal(_s: &mut Session, p: &Value) -> Result<Value> {
    if bool_or(p, "units", false) {
        return Ok(json!({ "units": calc::units_json() }));
    }
    if let Some(c) = p.get("convert").filter(|v| !v.is_null()) {
        let text = |k: &str| c.get(k).and_then(Value::as_str).ok_or_else(|| bad("cal", format!("`convert.{k}` is required")));
        let vars = p.get("vars").and_then(Value::as_object).cloned().unwrap_or_default();
        let v = match c.get("value") {
            Some(Value::String(e)) => eval(e, &vars).map_err(|m| bad("cal", m))?,
            Some(v) => v.as_f64().ok_or_else(|| bad("cal", "`convert.value` must be a number or an expression"))?,
            None => return Err(bad("cal", "`convert.value` is required")),
        };
        let (from, to) = (text("from")?, text("to")?);
        let r = calc::convert(c.get("type").and_then(Value::as_str), from, to, v).map_err(|m| bad("cal", m))?;
        return Ok(json!({ "value": r, "message": format!("{} {from} = {} {to}", calc::num_text(v), calc::num_text(r)) }));
    }
    if let Some(m) = p.get("pick").filter(|v| !v.is_null()) {
        let mode = m.as_str().and_then(calc::Pick::parse).ok_or_else(|| bad("cal", "`pick` must be point, distance, angle or intersection"))?;
        let pts = points_param(p, "points").ok_or_else(|| bad("cal", "`points` must be a list of [x, y] points"))?;
        let v = mode.value(&pts).map_err(|m| bad("cal", m))?;
        let mut r = v.json();
        r["message"] = json!(v.text());
        return Ok(r);
    }
    let e = str_param(p, "expr").ok_or_else(|| bad("cal", "`expr` is required"))?;
    let vars = p.get("vars").and_then(Value::as_object).cloned().unwrap_or_default();
    let v = calc::eval_val(e, &vars).map_err(|m| bad("cal", m))?;
    let mut r = v.json();
    r["message"] = json!(v.text());
    Ok(r)
}

/// CAL at the command line: one expression, its value echoed.
struct CalM;

impl Interactive for CalM {
    fn name(&self) -> &'static str {
        "CAL"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new(">> Expression", Accept::TEXT)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let Input::Text(t) = i else { return Ok(Step::Done) };
        if t.trim().is_empty() {
            return Ok(Step::Done);
        }
        match calc::eval_val(&t, &serde_json::Map::new()) {
            Ok(v) => s.echo(v.text()),
            Err(m) => s.echo(format!("CAL: {m}")),
        }
        Ok(Step::Done)
    }
}

/// QuickCalc's "get from the drawing": pick the points for a coordinate, distance, angle or
/// intersection; the value is echoed after [`PICKED`].
#[derive(Default)]
struct PickM {
    mode: Option<calc::Pick>,
    pts: Vec<Vec2>,
}

impl PickM {
    fn finish(&self, s: &mut Session, mode: calc::Pick) -> Result<Step> {
        match mode.value(&self.pts) {
            Ok(v) => s.echo(format!("{PICKED}{}", v.text())),
            Err(m) => s.echo(format!("QuickCalc: {m}")),
        }
        Ok(Step::Done)
    }
}

impl Interactive for PickM {
    fn name(&self) -> &'static str {
        "QUICKCALC"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        let msg = match (self.mode, self.pts.len()) {
            (None, _) => {
                return Prompt::new("Get from the drawing or [Point/Distance/Angle/Intersection]", Accept::POINT).kw(&[
                    "Point",
                    "Distance",
                    "Angle",
                    "Intersection",
                ]);
            }
            (Some(calc::Pick::Point), _) => "Specify point",
            (Some(calc::Pick::Intersection), 0) => "Specify first point of first line",
            (Some(calc::Pick::Intersection), 1) => "Specify second point of first line",
            (Some(calc::Pick::Intersection), 2) => "Specify first point of second line",
            (Some(calc::Pick::Intersection), _) => "Specify second point of second line",
            (Some(_), 0) => "Specify first point",
            (Some(_), _) => "Specify second point",
        };
        let base = if self.pts.len() % 2 == 1 { self.pts.last().copied() } else { None };
        Prompt::new(msg, Accept::POINT).base_opt(base)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Keyword(k) if self.mode.is_none() => {
                self.mode = calc::Pick::parse(&k);
                Ok(Step::Continue)
            }
            Input::Point(p) => {
                let mode = *self.mode.get_or_insert(calc::Pick::Point);
                self.pts.push(p);
                if self.pts.len() >= mode.points() { self.finish(s, mode) } else { Ok(Step::Continue) }
            }
            Input::Enter => Ok(Step::Done),
            _ => Ok(Step::Continue),
        }
    }
    fn preview(&self, _s: &Session, c: Vec2) -> Vec<EntityKind> {
        match self.pts.last() {
            Some(a) if self.pts.len() % 2 == 1 => vec![super::helpers::line(*a, c)],
            _ => Vec::new(),
        }
    }
}

fn run_quickcalc_pick(s: &mut Session, p: &Value) -> Result<Value> {
    let mode = str_param(p, "mode").unwrap_or("point");
    run_cal(s, &json!({ "pick": mode, "points": p.get("points").cloned().unwrap_or(Value::Null) }))
}

// ---------------- FIND ----------------

fn replace_in(text: &str, find: &str, rep: &str, case: bool, whole: bool) -> (String, usize) {
    if find.is_empty() {
        return (text.to_string(), 0);
    }
    let hay = if case { text.to_string() } else { text.to_lowercase() };
    let needle = if case { find.to_string() } else { find.to_lowercase() };
    let mut out = String::new();
    let mut n = 0;
    let mut last = 0;
    let mut start = 0;
    while let Some(pos) = hay.get(start..).and_then(|h| h.find(&needle)).map(|p| p + start) {
        let end = pos + needle.len();
        let boundary = |i: usize| hay.get(..i).and_then(|s| s.chars().last()).is_none_or(|c| !c.is_alphanumeric());
        let after = hay.get(end..).and_then(|s| s.chars().next()).is_none_or(|c| !c.is_alphanumeric());
        if !whole || (boundary(pos) && after) {
            out.push_str(text.get(last..pos).unwrap_or(""));
            out.push_str(rep);
            last = end;
            n += 1;
        }
        start = end.max(pos + 1);
        if start >= hay.len() {
            break;
        }
    }
    out.push_str(text.get(last..).unwrap_or(""));
    (out, n)
}

fn run_find(s: &mut Session, p: &Value) -> Result<Value> {
    let find = str_param(p, "find").ok_or_else(|| bad("find", "`find` is required"))?.to_string();
    let rep = str_param(p, "replace").map(str::to_string);
    let case = bool_or(p, "matchCase", false);
    let whole = bool_or(p, "wholeWord", false);
    let space = s.space();
    let d = s.doc()?;
    // `handles` scopes the search to those objects (the dialog's Replace for the located
    // match); a handle that can't be read is an error, as in `targets`.
    let hs: Vec<Handle> = match p.get("handles") {
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_str().and_then(Handle::parse_hex).ok_or_else(|| bad("find", format!("`handles`: {v} is not an object handle"))))
            .collect::<std::result::Result<_, _>>()?,
        _ => d.space(&space).map(|st| st.handles()).unwrap_or_default(),
    };
    let mut found: Vec<Value> = Vec::new();
    let mut total = 0;
    let mut edits: Vec<(Handle, EntityKind)> = Vec::new();
    for h in hs {
        let Some(e) = d.entity(h) else { continue };
        let mut k = e.kind.clone();
        let mut hits = 0;
        let r = rep.as_deref().unwrap_or(&find);
        let mut apply = |t: &mut String| {
            let (nt, n) = replace_in(t, &find, r, case, whole);
            if n > 0 {
                hits += n;
                if rep.is_some() {
                    *t = nt;
                }
            }
        };
        match &mut k {
            EntityKind::Text(t) => apply(&mut t.value),
            EntityKind::MText(t) => apply(&mut t.contents),
            EntityKind::Insert(i) => i.attribs.iter_mut().for_each(|a| apply(&mut a.text.value)),
            EntityKind::Dimension(dm) => apply(&mut dm.text),
            EntityKind::MLeader(m) => {
                if let Some(t) = &mut m.text {
                    apply(&mut t.contents)
                }
            }
            EntityKind::Table(t) => t.cells.iter_mut().flatten().for_each(|c| apply(&mut c.text)),
            _ => {}
        }
        if hits > 0 {
            total += hits;
            found.push(json!({ "handle": h.hex(), "type": e.kind.type_name(), "count": hits }));
            if rep.is_some() {
                edits.push((h, k));
            }
        }
    }
    let n = edits.len();
    if rep.is_some() {
        let doc = s.doc_mut()?;
        for (h, k) in edits {
            doc.modify_entity(h, |e| {
                e.kind = k;
                if let EntityKind::Dimension(dm) = &mut e.kind {
                    dm.block = None;
                }
            })?;
        }
    } else {
        let sel: Vec<Handle> = found.iter().filter_map(|v| v["handle"].as_str().and_then(Handle::parse_hex)).collect();
        s.set_selection(sel);
    }
    let msg =
        if rep.is_some() { format!("{total} replacement(s) in {n} object(s)") } else { format!("{total} match(es) in {} object(s)", found.len()) };
    Ok(json!({ "matches": found, "total": total, "message": msg }))
}

/// FIND at the command line (the dialog is the primary surface in the GUI; this is the
/// prompt sequence scripts and the CLI get): find text, replacement text (Enter = find
/// only, which selects the matches), then Replace all or one object at a time.
#[derive(Default)]
struct FindM {
    find: Option<String>,
    replace: Option<String>,
    case: bool,
    whole: bool,
    /// One-at-a-time queue: (handle, type name, match count) not yet offered.
    queue: Vec<(Handle, String, usize)>,
    /// In the one-at-a-time phase, an object has been located and is waiting for a verdict.
    located: bool,
}

impl FindM {
    fn params(&self, replace: bool, handles: Option<&[Handle]>) -> Value {
        let mut p = json!({ "find": self.find.clone().unwrap_or_default(), "matchCase": self.case, "wholeWord": self.whole });
        if replace {
            p["replace"] = json!(self.replace.clone().unwrap_or_default());
        }
        if let Some(hs) = handles {
            p["handles"] = json!(hs.iter().map(|h| h.hex()).collect::<Vec<_>>());
        }
        p
    }
    /// Find only: run the search (it selects the matches), echo the count, finish.
    fn find_only(&mut self, s: &mut Session) -> Result<Step> {
        let r = run_find(s, &self.params(false, None))?;
        if let Some(m) = r["message"].as_str() {
            s.echo(m.to_string());
        }
        Ok(Step::Done)
    }
    /// Start the one-at-a-time phase: collect the matching objects, locate the first.
    fn begin_one_by_one(&mut self, s: &mut Session) -> Result<Step> {
        let r = run_find(s, &self.params(false, None))?;
        self.queue = r["matches"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| {
                        Some((Handle::parse_hex(m["handle"].as_str()?)?, m["type"].as_str()?.to_string(), m["count"].as_u64().unwrap_or(0) as usize))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if self.queue.is_empty() {
            s.echo("0 match(es) in 0 object(s)");
            return Ok(Step::Done);
        }
        self.locate_next(s);
        Ok(Step::Continue)
    }
    /// Select the next queued object and say what it holds.
    fn locate_next(&mut self, s: &mut Session) {
        if let Some((h, ty, n)) = self.queue.first().cloned() {
            s.set_selection(vec![h]);
            s.echo(format!("{ty}: {n} match(es)"));
            self.located = true;
        }
    }
}

impl Interactive for FindM {
    fn name(&self) -> &'static str {
        "FIND"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.located {
            return Prompt::new("Replace in this object", Accept::TEXT).kw(&["Replace", "Skip", "All", "Quit"]).default("Replace");
        }
        if self.replace.is_some() {
            return Prompt::new("Replace in all matching objects, or one by one", Accept::TEXT).kw(&["All", "OneByOne"]).default("All");
        }
        if self.find.is_some() {
            return Prompt::new("Enter replacement text, or press Enter to find only", Accept::TEXT);
        }
        let opts = format!("Match case {}, whole word {}", if self.case { "on" } else { "off" }, if self.whole { "on" } else { "off" });
        Prompt::new(format!("Enter text to find [{opts}]"), Accept::TEXT).kw(&["MatchCase", "WholeWord"])
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Cancel => Ok(Step::Done),
            Input::Keyword(k) => {
                let k = k.to_ascii_lowercase();
                if self.located {
                    match k.as_str() {
                        "replace" => {
                            let (h, _, _) = self.queue.remove(0);
                            let r = run_find(s, &self.params(true, Some(&[h])))?;
                            if let Some(m) = r["message"].as_str() {
                                s.echo(m.to_string());
                            }
                            self.located = false;
                            if self.queue.is_empty() {
                                return Ok(Step::Done);
                            }
                            self.locate_next(s);
                            Ok(Step::Continue)
                        }
                        "skip" => {
                            self.queue.remove(0);
                            self.located = false;
                            if self.queue.is_empty() {
                                return Ok(Step::Done);
                            }
                            self.locate_next(s);
                            Ok(Step::Continue)
                        }
                        "all" => {
                            let rest: Vec<Handle> = self.queue.iter().map(|(h, _, _)| *h).collect();
                            let r = run_find(s, &self.params(true, Some(&rest)))?;
                            if let Some(m) = r["message"].as_str() {
                                s.echo(m.to_string());
                            }
                            Ok(Step::Done)
                        }
                        "quit" => Ok(Step::Done),
                        _ => Ok(Step::Continue),
                    }
                } else if self.replace.is_some() {
                    if k == "onebyone" {
                        self.begin_one_by_one(s)
                    } else {
                        let r = run_find(s, &self.params(true, None))?;
                        if let Some(m) = r["message"].as_str() {
                            s.echo(m.to_string());
                        }
                        Ok(Step::Done)
                    }
                } else if self.find.is_none() {
                    match k.as_str() {
                        "matchcase" => self.case = !self.case,
                        "wholeword" => self.whole = !self.whole,
                        _ => {}
                    }
                    Ok(Step::Continue)
                } else {
                    Ok(Step::Continue)
                }
            }
            Input::Text(t) => {
                if self.located {
                    // A typed verdict, matched like a keyword.
                    return self.input(s, Input::Keyword(t.trim().to_string()));
                }
                if self.replace.is_some() {
                    // At the mode prompt only the two keywords are answers; anything else
                    // must not fall through to Replace all.
                    match t.trim().to_ascii_lowercase().as_str() {
                        "all" => return self.input(s, Input::Keyword("All".into())),
                        "onebyone" | "one by one" => return self.input(s, Input::Keyword("OneByOne".into())),
                        _ => {
                            s.echo("*Invalid option*");
                            return Ok(Step::Continue);
                        }
                    }
                }
                if self.find.is_none() {
                    let t = t.trim().to_string();
                    if t.is_empty() {
                        return Ok(Step::Done);
                    }
                    self.find = Some(t);
                    return Ok(Step::Continue);
                }
                // Replacement text: whatever was typed (may be empty = delete the matches).
                self.replace = Some(t);
                Ok(Step::Continue)
            }
            Input::Enter => {
                if self.located {
                    return self.input(s, Input::Keyword("Replace".into()));
                }
                if self.replace.is_some() {
                    // The default at the Replace prompt is All.
                    let r = run_find(s, &self.params(true, None))?;
                    if let Some(m) = r["message"].as_str() {
                        s.echo(m.to_string());
                    }
                    return Ok(Step::Done);
                }
                if self.find.is_some() {
                    return self.find_only(s);
                }
                Ok(Step::Done)
            }
            _ => Ok(Step::Continue),
        }
    }
}

// ---------------- MEASUREGEOM ----------------
// (MASSPROP is in `inquiry/regions.rs`.)

fn run_radius(s: &mut Session, p: &Value) -> Result<Value> {
    let h = targets(s, p)?.first().copied().ok_or_else(|| bad("radius.measure", "`handle` is required"))?;
    let r = match s.doc()?.entity(h).map(|e| e.kind.clone()) {
        Some(EntityKind::Circle(c)) => c.radius,
        Some(EntityKind::Arc(a)) => a.radius,
        _ => return Err(bad("radius.measure", "select an arc or circle")),
    };
    Ok(json!({ "radius": r, "diameter": 2.0 * r, "message": format!("Radius = {r:.4}\nDiameter = {:.4}", 2.0 * r) }))
}

fn run_angle(s: &mut Session, p: &Value) -> Result<Value> {
    let (v, a, b) = if let (Some(v), Some(a), Some(b)) = (point_param(p, "vertex"), point_param(p, "p1"), point_param(p, "p2")) {
        (v, a, b)
    } else {
        let hp = |k: &str| p.get(k).and_then(Value::as_str).and_then(Handle::parse_hex);
        let (Some(h1), Some(h2)) = (hp("h1"), hp("h2")) else { return Err(bad("angle.measure", "give {vertex,p1,p2} or {h1,h2}")) };
        let d = s.doc()?;
        let line = |h: Handle| match d.entity(h).map(|e| e.kind.clone()) {
            Some(EntityKind::Line(l)) => Some((l.a.xy(), l.b.xy())),
            _ => None,
        };
        let ((a1, a2), (b1, b2)) =
            (line(h1).ok_or_else(|| bad("angle.measure", "h1 is not a line"))?, line(h2).ok_or_else(|| bad("angle.measure", "h2 is not a line"))?);
        let (x, _, _) = cadcraft_geom::line_line_infinite(a1, a2, b1, b2).ok_or_else(|| bad("angle.measure", "lines are parallel"))?;
        let far = |p: Vec2, q: Vec2| if p.dist(x) > q.dist(x) { p } else { q };
        (x, far(a1, a2), far(b1, b2))
    };
    let mut ang = (cadcraft_geom::norm_angle(v.angle_to(b) - v.angle_to(a))).to_degrees();
    if ang > 180.0 {
        ang = 360.0 - ang;
    }
    Ok(json!({ "angle": ang, "message": format!("Angle = {ang:.4}°") }))
}

fn run_measuregeom(s: &mut Session, p: &Value) -> Result<Value> {
    match str_param(p, "mode").unwrap_or("distance").to_ascii_lowercase().as_str() {
        "distance" | "d" => s.execute("dist", p),
        "radius" | "r" => run_radius(s, p),
        "angle" | "a" => run_angle(s, p),
        "area" | "ar" => s.execute("area", p),
        _ => Err(bad("measuregeom", "mode must be distance, radius, angle or area")),
    }
}

// ---------- MEASUREGEOM / ANGLE / RADIUS typed flows (audit F-class) ----------

/// MEASUREGEOM typed: AutoCAD keeps the command open, offering
/// [Distance/Radius/Angle/Area] again after every measurement until Enter/Esc.
#[derive(Default)]
pub struct MeasuregeomM {
    /// 0 choose, 1 distance, 2 radius, 3 angle, 4 area.
    mode: u8,
    pts: Vec<Vec2>,
}

impl MeasuregeomM {
    fn back_to_options(&mut self) {
        self.mode = 0;
        self.pts.clear();
    }
    fn echo_result(s: &mut Session, r: Result<Value>) {
        match r {
            Ok(v) => {
                if let Some(m) = v.get("message").and_then(Value::as_str) {
                    s.echo(m);
                }
            }
            Err(e) => s.echo(e.to_string()),
        }
    }
}

impl Interactive for MeasuregeomM {
    fn name(&self) -> &'static str {
        "MEASUREGEOM"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.mode {
            1 if self.pts.is_empty() => Prompt::new("Specify first point", Accept::POINT),
            1 => Prompt::new("Specify second point", Accept::POINT).base_opt(self.pts.first().copied()),
            2 => Prompt::new("Select arc or circle", Accept::SELECT),
            3 => Prompt::new(["Specify vertex", "Specify first point", "Specify second point"][self.pts.len().min(2)], Accept::POINT),
            4 => Prompt::new("Specify a point", Accept::POINT).base_opt(self.pts.last().copied()),
            _ => Prompt::new("Select an option", Accept::TEXT).kw(&["Distance", "Radius", "Angle", "Area"]),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Cancel => return Ok(Step::Done),
            Input::Enter if self.mode == 0 => return Ok(Step::Done),
            _ => {}
        }
        match self.mode {
            0 => {
                let name = match &i {
                    Input::Keyword(k) => Some(k.clone()),
                    Input::Text(t) => Some(t.trim().to_string()),
                    _ => None,
                };
                match name.as_deref().map(str::to_ascii_lowercase).as_deref() {
                    Some("distance") | Some("d") => self.mode = 1,
                    Some("radius") | Some("r") => self.mode = 2,
                    Some("angle") | Some("a") => self.mode = 3,
                    Some("area") | Some("ar") => self.mode = 4,
                    Some("") | None => {}
                    Some(_) => s.echo("*Invalid option*"),
                }
                Ok(Step::Continue)
            }
            1 => match i {
                Input::Point(p) => {
                    self.pts.push(p);
                    if self.pts.len() == 2 {
                        let r = s.execute("dist", &json!({ "p1": [self.pts[0].x, self.pts[0].y], "p2": [self.pts[1].x, self.pts[1].y] }));
                        Self::echo_result(s, r);
                        self.back_to_options();
                    }
                    Ok(Step::Continue)
                }
                Input::Enter => {
                    self.back_to_options();
                    Ok(Step::Continue)
                }
                _ => Ok(Step::Continue),
            },
            2 => match i {
                Input::Pick(hs) => {
                    if let Some(h) = hs.first() {
                        let r = run_radius(s, &json!({ "handle": h.hex() }));
                        Self::echo_result(s, r);
                        self.back_to_options();
                    }
                    Ok(Step::Continue)
                }
                Input::Enter => {
                    self.back_to_options();
                    Ok(Step::Continue)
                }
                _ => Ok(Step::Continue),
            },
            3 => match i {
                Input::Point(p) => {
                    self.pts.push(p);
                    if self.pts.len() == 3 {
                        let r = run_angle(
                            s,
                            &json!({ "vertex": [self.pts[0].x, self.pts[0].y], "p1": [self.pts[1].x, self.pts[1].y], "p2": [self.pts[2].x, self.pts[2].y] }),
                        );
                        Self::echo_result(s, r);
                        self.back_to_options();
                    }
                    Ok(Step::Continue)
                }
                Input::Enter => {
                    self.back_to_options();
                    Ok(Step::Continue)
                }
                _ => Ok(Step::Continue),
            },
            _ => match i {
                Input::Point(p) => {
                    self.pts.push(p);
                    Ok(Step::Continue)
                }
                Input::Enter => {
                    if self.pts.len() >= 3 {
                        let points: Vec<Value> = self.pts.iter().map(|p| json!([p.x, p.y])).collect();
                        let r = s.execute("area", &json!({ "points": points }));
                        Self::echo_result(s, r);
                    }
                    self.back_to_options();
                    Ok(Step::Continue)
                }
                _ => Ok(Step::Continue),
            },
        }
    }
}

/// ANGLE (Tools ▸ Inquiry ▸ Angle) typed: vertex, then two points.
#[derive(Default)]
pub struct AngleM {
    pts: Vec<Vec2>,
}

impl Interactive for AngleM {
    fn name(&self) -> &'static str {
        "ANGLE"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new(["Specify vertex", "Specify first point", "Specify second point"][self.pts.len().min(2)], Accept::POINT)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Cancel => Ok(Step::Done),
            Input::Point(p) => {
                self.pts.push(p);
                if self.pts.len() == 3 {
                    let r = run_angle(
                        s,
                        &json!({ "vertex": [self.pts[0].x, self.pts[0].y], "p1": [self.pts[1].x, self.pts[1].y], "p2": [self.pts[2].x, self.pts[2].y] }),
                    )?;
                    if let Some(m) = r.get("message").and_then(Value::as_str) {
                        s.echo(m);
                    }
                    return Ok(Step::Done);
                }
                Ok(Step::Continue)
            }
            _ => Ok(Step::Continue),
        }
    }
}

/// RADIUS (Tools ▸ Inquiry ▸ Radius) typed: pick an arc or circle.
#[derive(Default)]
pub struct RadiusM;

impl Interactive for RadiusM {
    fn name(&self) -> &'static str {
        "RADIUS"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Select arc or circle", Accept::SELECT)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Cancel | Input::Enter => Ok(Step::Done),
            Input::Pick(hs) => {
                let Some(h) = hs.first() else { return Ok(Step::Continue) };
                match run_radius(s, &json!({ "handle": h.hex() })) {
                    Ok(r) => {
                        if let Some(m) = r.get("message").and_then(Value::as_str) {
                            s.echo(m);
                        }
                        Ok(Step::Done)
                    }
                    Err(e) => {
                        s.echo(e.to_string());
                        Ok(Step::Continue)
                    }
                }
            }
            _ => Ok(Step::Continue),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculator() {
        let v = serde_json::Map::new();
        assert_eq!(eval("(3+4)*2^2", &v), Ok(28.0));
        assert!((eval("sin(30)", &v).unwrap() - 0.5).abs() < 1e-12);
        assert!((eval("2*pi", &v).unwrap() - std::f64::consts::TAU).abs() < 1e-12);
        assert_eq!(eval("-2^2", &v), Ok(-4.0));
        assert!(eval("1/0", &v).is_err());
        assert!(eval("((((1", &v).is_err());
        assert!(eval("foo(1)", &v).is_err());
        assert!(eval(&"(".repeat(5000), &v).is_err());
        let mut vars = serde_json::Map::new();
        vars.insert("x".into(), json!(3));
        assert_eq!(eval("x*x", &vars), Ok(9.0));
    }

    #[test]
    fn find_and_replace() {
        assert_eq!(replace_in("Bolt bolt BOLTS", "bolt", "Pin", false, true), ("Pin Pin BOLTS".to_string(), 2));
        assert_eq!(replace_in("aaa", "a", "b", true, false), ("bbb".to_string(), 3));
        assert_eq!(replace_in("x", "", "y", true, false).1, 0);
    }
}
