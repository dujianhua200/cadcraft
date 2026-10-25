//! AutoCAD 2021 compatibility layer.
//!
//! Two things live here:
//!
//! * The **default command aliases** AutoCAD 2021 ships in `acad.pgp` (the `L` → LINE, `TR` →
//!   TRIM keys every drafter's fingers know). [`AUTOCAD_2021_ALIASES`] holds them; a user's own
//!   `.pgp` file (ALIASEDIT/REINIT) still overrides them, and a command name always wins.
//! * The **parity report** (`COMPAT`): how many of AutoCAD 2021's commands this build answers to,
//!   and which ones it doesn't. It is measured against the command registry, never asserted by
//!   hand, so it cannot drift from the code.
//!
//! The commands that AutoCAD names differently from this catalog but that already exist here are
//! aliased rather than reimplemented (DIMCENTER → CENTERMARK, QLEADER → LEADER). The few that
//! were genuinely missing and are cheap to build faithfully are implemented below (SOLID, OOPS,
//! TXT2MTXT, SCALETEXT).
//!
//! What is *not* here is listed by `COMPAT` as missing: the 3D solid kernel, xref clipping,
//! object filters, spell checking and the Express Tools. Aliasing those to something else would
//! lie about what the build does, so they stay missing until they are real.

use cadcraft_doc::{EntityKind, Handle, MText, Solid, Text};
use cadcraft_geom::Vec2;
use serde_json::{Value, json};

use super::machines::SelectPhase;
use super::*;
use crate::{Accept, Input, Interactive, Prompt, Step};

// ---------------------------------------------------------------------------------------------
// AutoCAD 2021 default aliases
// ---------------------------------------------------------------------------------------------

/// The default `acad.pgp` aliases of AutoCAD 2021 whose command this build implements.
///
/// An alias here means "typing this at the command line starts that command", exactly as in
/// AutoCAD. Aliases whose AutoCAD command is not implemented are deliberately absent: mapping
/// `SP` (SPELL) onto something else would silently do the wrong thing.
pub const AUTOCAD_2021_ALIASES: &[(&str, &str)] = &[
    ("-h", "hatch"),
    ("-i", "insert"),
    ("-t", "mtext"),
    ("-w", "wblock"),
    ("3do", "3dorbit"),
    ("a", "arc"),
    ("aa", "area"),
    ("al", "align"),
    ("ap", "appload"),
    ("ar", "array"),
    ("at", "attdef"),
    ("ate", "attedit"),
    ("b", "block"),
    ("be", "bedit"),
    ("bh", "hatch"),
    ("bo", "boundary"),
    ("br", "break"),
    ("bs", "bsave"),
    ("c", "circle"),
    ("ch", "properties"),
    ("cha", "chamfer"),
    ("col", "color"),
    ("co", "copy"),
    ("cp", "copy"),
    ("ct", "tablestyle"),
    ("d", "dimstyle"),
    ("dal", "dimaligned"),
    ("dan", "dimangular"),
    ("dar", "dimarc"),
    ("dba", "dimbaseline"),
    ("dce", "centermark"),
    ("dco", "dimcontinue"),
    ("dda", "dimdisassociate"),
    ("ddedit", "textedit"),
    ("ddi", "dimdiameter"),
    ("ded", "dimedit"),
    ("di", "dist"),
    ("div", "divide"),
    ("djl", "dimjogline"),
    ("djo", "dimjogged"),
    ("dl", "datalink"),
    ("dli", "dimlinear"),
    ("do", "donut"),
    ("dor", "dimordinate"),
    ("dov", "dimoverride"),
    ("dr", "draworder"),
    ("dra", "dimradius"),
    ("dre", "dimreassociate"),
    ("drm", "drawingrecovery"),
    ("ds", "dsettings"),
    ("dst", "dimstyle"),
    ("dt", "text"),
    ("e", "erase"),
    ("ed", "textedit"),
    ("el", "ellipse"),
    ("er", "xref"),
    ("ex", "extend"),
    ("exit", "close"),
    ("exp", "export"),
    ("f", "fillet"),
    ("g", "group"),
    ("gra", "gradient"),
    ("h", "hatch"),
    ("he", "hatchedit"),
    ("i", "insert"),
    ("iat", "attach"),
    ("imp", "import"),
    ("in", "intersect"),
    ("j", "join"),
    ("l", "line"),
    ("la", "layer"),
    ("las", "layerstate.list"),
    ("le", "leader"),
    ("len", "lengthen"),
    ("li", "list"),
    ("lo", "layout"),
    ("ls", "list"),
    ("lt", "linetype"),
    ("lts", "ltscale"),
    ("lw", "lweight"),
    ("m", "move"),
    ("ma", "matchprop"),
    ("me", "measure"),
    ("mi", "mirror"),
    ("ml", "mline"),
    ("mla", "mleaderalign"),
    ("mlc", "mleadercollect"),
    ("mle", "mleaderedit"),
    ("mls", "mleaderstyle"),
    ("mo", "properties"),
    ("ms", "mspace"),
    ("mt", "mtext"),
    ("mv", "mview"),
    ("o", "offset"),
    ("op", "options"),
    ("os", "osnap"),
    ("p", "pan"),
    ("par", "parameters"),
    ("pe", "pedit"),
    ("pl", "pline"),
    ("po", "point"),
    ("pol", "polygon"),
    ("pr", "properties"),
    ("pre", "preview"),
    ("print", "plot"),
    ("ps", "pspace"),
    ("pu", "purge"),
    ("qc", "cal"),
    ("qnew", "new"),
    ("r", "redraw"),
    ("ra", "redraw"),
    ("re", "regen"),
    ("rea", "regenall"),
    ("rec", "rectang"),
    ("reg", "region"),
    ("ren", "rename"),
    ("rm", "dsettings"),
    ("ro", "rotate"),
    ("s", "stretch"),
    ("save", "qsave"),
    ("sc", "scale"),
    ("scr", "script"),
    ("se", "dsettings"),
    ("set", "setvar"),
    ("sn", "snap"),
    ("so", "solid"),
    ("spl", "spline"),
    ("spe", "splinedit"),
    ("st", "style"),
    ("su", "subtract"),
    ("t", "mtext"),
    ("tb", "table"),
    ("tol", "tolerance"),
    ("tr", "trim"),
    ("ts", "tablestyle"),
    ("un", "units"),
    ("uni", "union"),
    ("v", "view"),
    ("w", "wblock"),
    ("x", "explode"),
    ("xa", "xattach"),
    ("xb", "xbind"),
    ("xl", "xline"),
    ("xr", "xref"),
    ("z", "zoom"),
    // AutoCAD command names this catalog spells differently.
    ("dimcenter", "centermark"),
    ("externalreferences", "xref"),
    ("layerstate", "layerstate.list"),
    ("qleader", "leader"),
    ("quit", "close"),
    ("redrawall", "redraw"),
];

/// The command an AutoCAD 2021 default alias stands for (`name` in lower case), if this build
/// has that command.
pub fn alias(name: &str) -> Option<&'static str> {
    AUTOCAD_2021_ALIASES.iter().find(|(a, _)| a.eq_ignore_ascii_case(name)).map(|(_, c)| *c)
}

// ---------------------------------------------------------------------------------------------
// The AutoCAD 2021 command set the report measures against
// ---------------------------------------------------------------------------------------------

/// AutoCAD 2021's commands, grouped by the menu they live under. The report resolves each one
/// against the live registry, so this table only has to name the reference commands; whether they
/// work is read from the code.
pub const AUTOCAD_2021_COMMANDS: &[(&str, &str)] = &[
    // Draw
    ("line", "Draw"),
    ("ray", "Draw"),
    ("xline", "Draw"),
    ("mline", "Draw"),
    ("pline", "Draw"),
    ("3dpoly", "Draw"),
    ("polygon", "Draw"),
    ("rectang", "Draw"),
    ("arc", "Draw"),
    ("circle", "Draw"),
    ("revcloud", "Draw"),
    ("spline", "Draw"),
    ("ellipse", "Draw"),
    ("helix", "Draw"),
    ("donut", "Draw"),
    ("point", "Draw"),
    ("solid", "Draw"),
    ("trace", "Draw"),
    ("hatch", "Draw"),
    ("gradient", "Draw"),
    ("boundary", "Draw"),
    ("region", "Draw"),
    ("wipeout", "Draw"),
    ("table", "Draw"),
    ("text", "Draw"),
    ("mtext", "Draw"),
    ("sketch", "Draw"),
    ("insert", "Draw"),
    // Modify
    ("erase", "Modify"),
    ("copy", "Modify"),
    ("mirror", "Modify"),
    ("offset", "Modify"),
    ("array", "Modify"),
    ("arrayrect", "Modify"),
    ("arraypolar", "Modify"),
    ("arraypath", "Modify"),
    ("move", "Modify"),
    ("rotate", "Modify"),
    ("scale", "Modify"),
    ("stretch", "Modify"),
    ("lengthen", "Modify"),
    ("trim", "Modify"),
    ("extend", "Modify"),
    ("break", "Modify"),
    ("breakatpoint", "Modify"),
    ("join", "Modify"),
    ("chamfer", "Modify"),
    ("fillet", "Modify"),
    ("blend", "Modify"),
    ("explode", "Modify"),
    ("align", "Modify"),
    ("pedit", "Modify"),
    ("splinedit", "Modify"),
    ("hatchedit", "Modify"),
    ("mledit", "Modify"),
    ("reverse", "Modify"),
    ("overkill", "Modify"),
    ("draworder", "Modify"),
    ("texttofront", "Modify"),
    ("flatten", "Modify"),
    ("ncopy", "Modify"),
    ("objectscale", "Modify"),
    ("divide", "Modify"),
    ("measure", "Modify"),
    ("group", "Modify"),
    ("ungroup", "Modify"),
    ("chprop", "Modify"),
    ("change", "Modify"),
    ("matchprop", "Modify"),
    ("properties", "Modify"),
    ("list", "Modify"),
    ("select", "Modify"),
    // Annotation
    ("dimlinear", "Annotate"),
    ("dimaligned", "Annotate"),
    ("dimangular", "Annotate"),
    ("dimarc", "Annotate"),
    ("dimordinate", "Annotate"),
    ("dimradius", "Annotate"),
    ("dimdiameter", "Annotate"),
    ("dimjogged", "Annotate"),
    ("dimjogline", "Annotate"),
    ("dimbaseline", "Annotate"),
    ("dimcontinue", "Annotate"),
    ("dimspace", "Annotate"),
    ("dimbreak", "Annotate"),
    ("dimcenter", "Annotate"),
    ("dimedit", "Annotate"),
    ("dimtedit", "Annotate"),
    ("dimoverride", "Annotate"),
    ("dimstyle", "Annotate"),
    ("dimreassociate", "Annotate"),
    ("dimdisassociate", "Annotate"),
    ("diminspect", "Annotate"),
    ("dim", "Annotate"),
    ("qdim", "Annotate"),
    ("leader", "Annotate"),
    ("qleader", "Annotate"),
    ("mleader", "Annotate"),
    ("mleaderalign", "Annotate"),
    ("mleadercollect", "Annotate"),
    ("mleaderedit", "Annotate"),
    ("mleaderstyle", "Annotate"),
    ("tolerance", "Annotate"),
    ("tablestyle", "Annotate"),
    ("tabledit", "Annotate"),
    ("textedit", "Annotate"),
    ("find", "Annotate"),
    ("spell", "Annotate"),
    ("scaletext", "Annotate"),
    ("justifytext", "Annotate"),
    ("textfit", "Annotate"),
    ("txt2mtxt", "Annotate"),
    ("txtexp", "Annotate"),
    ("field", "Annotate"),
    ("style", "Annotate"),
    ("attdef", "Annotate"),
    ("attedit", "Annotate"),
    ("attsync", "Annotate"),
    ("attdisp", "Annotate"),
    ("battman", "Annotate"),
    ("annoscale", "Annotate"),
    ("annoreset", "Annotate"),
    ("annoupdate", "Annotate"),
    // Blocks and references
    ("block", "Insert"),
    ("-block", "Insert"),
    ("wblock", "Insert"),
    ("bedit", "Insert"),
    ("bclose", "Insert"),
    ("bsave", "Insert"),
    ("bsaveas", "Insert"),
    ("refedit", "Insert"),
    ("refclose", "Insert"),
    ("refset", "Insert"),
    ("xattach", "Insert"),
    ("xref", "Insert"),
    ("xbind", "Insert"),
    ("xopen", "Insert"),
    ("xclip", "Insert"),
    ("attach", "Insert"),
    ("pasteblock", "Insert"),
    ("pasteorig", "Insert"),
    ("copybase", "Insert"),
    ("copyclip", "Insert"),
    ("cutclip", "Insert"),
    ("pasteclip", "Insert"),
    // Layers
    ("layer", "Format"),
    ("-layer", "Format"),
    ("layerstate", "Format"),
    ("layiso", "Format"),
    ("layuniso", "Format"),
    ("layoff", "Format"),
    ("layon", "Format"),
    ("layfrz", "Format"),
    ("laythw", "Format"),
    ("laylck", "Format"),
    ("layulk", "Format"),
    ("laymcur", "Format"),
    ("laymch", "Format"),
    ("laydel", "Format"),
    ("laymrg", "Format"),
    ("laycur", "Format"),
    ("laytrans", "Format"),
    ("laywalk", "Format"),
    ("setbylayer", "Format"),
    ("linetype", "Format"),
    ("lweight", "Format"),
    ("ltscale", "Format"),
    ("lwdisplay", "Format"),
    ("color", "Format"),
    ("rename", "Format"),
    ("units", "Format"),
    ("limits", "Format"),
    ("plotstyle", "Format"),
    ("scalelistedit", "Format"),
    // Inquiry and utilities
    ("dist", "Tools"),
    ("area", "Tools"),
    ("id", "Tools"),
    ("massprop", "Tools"),
    ("time", "Tools"),
    ("status", "Tools"),
    ("measuregeom", "Tools"),
    ("qselect", "Tools"),
    ("filter", "Tools"),
    ("count", "Tools"),
    ("selectsimilar", "Tools"),
    ("purge", "Tools"),
    ("audit", "Tools"),
    ("recover", "Tools"),
    ("drawingrecovery", "Tools"),
    ("oops", "Tools"),
    ("undo", "Tools"),
    ("redo", "Tools"),
    ("u", "Tools"),
    ("mredo", "Tools"),
    ("setvar", "Tools"),
    ("getvar", "Tools"),
    ("sysvars", "Tools"),
    ("reinit", "Tools"),
    ("aliasedit", "Tools"),
    ("script", "Tools"),
    ("lisp", "Tools"),
    ("appload", "Tools"),
    ("cal", "Tools"),
    ("options", "Tools"),
    ("dsettings", "Tools"),
    ("grid", "Tools"),
    ("snap", "Tools"),
    ("ortho", "Tools"),
    ("osnap", "Tools"),
    ("dynmode", "Tools"),
    ("isodraft", "Tools"),
    ("pickstyle", "Tools"),
    ("selectioncycling", "Tools"),
    ("transparencydisplay", "Tools"),
    ("dwgprops", "Tools"),
    ("about", "Tools"),
    ("help", "Tools"),
    ("isolateobjects", "Tools"),
    ("hideobjects", "Tools"),
    ("unisolateobjects", "Tools"),
    // File
    ("new", "File"),
    ("open", "File"),
    ("close", "File"),
    ("closeall", "File"),
    ("qsave", "File"),
    ("saveas", "File"),
    ("export", "File"),
    ("import", "File"),
    ("plot", "File"),
    ("preview", "File"),
    ("publish", "File"),
    ("exportpdf", "File"),
    ("plottermanager", "File"),
    ("pagesetup", "File"),
    ("layout", "File"),
    ("etransmit", "File"),
    ("compare", "File"),
    // View and layouts
    ("zoom", "View"),
    ("pan", "View"),
    ("view", "View"),
    ("vports", "View"),
    ("mview", "View"),
    ("vplayer", "View"),
    ("vpclip", "View"),
    ("regen", "View"),
    ("regenall", "View"),
    ("redraw", "View"),
    ("redrawall", "View"),
    ("3dorbit", "View"),
    ("3dcorbit", "View"),
    ("shademode", "View"),
    ("visualstyles", "View"),
    ("ucs", "View"),
    ("ucsicon", "View"),
    ("ucsman", "View"),
    ("plan", "View"),
    ("vpoint", "View"),
    ("dview", "View"),
    ("camera", "View"),
    ("hide", "View"),
    ("mspace", "View"),
    ("pspace", "View"),
    // Parametric
    ("parameters", "Parametric"),
    ("geomconstraint", "Parametric"),
    ("dimconstraint", "Parametric"),
    ("autoconstrain", "Parametric"),
    ("delconstraint", "Parametric"),
    ("constraintbar", "Parametric"),
    ("constraintsettings", "Parametric"),
    // 3D modelling
    ("3dface", "3D"),
    ("3dmesh", "3D"),
    ("3darray", "3D"),
    ("3dalign", "3D"),
    ("3dmirror", "3D"),
    ("mirror3d", "3D"),
    ("rotate3d", "3D"),
    ("move3d", "3D"),
    ("box", "3D"),
    ("cone", "3D"),
    ("cylinder", "3D"),
    ("sphere", "3D"),
    ("torus", "3D"),
    ("wedge", "3D"),
    ("pyramid", "3D"),
    ("extrude", "3D"),
    ("revolve", "3D"),
    ("sweep", "3D"),
    ("loft", "3D"),
    ("slice", "3D"),
    ("section", "3D"),
    ("interfere", "3D"),
    ("union", "3D"),
    ("subtract", "3D"),
    ("intersect", "3D"),
    ("presspull", "3D"),
    ("thicken", "3D"),
    ("chamferedge", "3D"),
    ("filletedge", "3D"),
    ("render", "3D"),
];

/// Whether this build answers to `name` (a command name or an AutoCAD default alias).
pub fn implements(name: &str) -> bool {
    let id = resolve_alias(name);
    find_command(&id).is_some()
}

// ---------------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------------

fn added(h: Handle) -> Result<Value> {
    Ok(json!({ "handle": h.hex() }))
}

/// A quirk of AutoCAD's SOLID: the third and fourth points are taken across the figure (a "Z"
/// order), so the quad reads `p1 p2 p4 p3`. Three points make a triangle.
fn solid_kind(p1: Vec2, p2: Vec2, p3: Vec2, p4: Vec2) -> EntityKind {
    EntityKind::Solid(Solid { corners: [v3(p1), v3(p2), v3(p4), v3(p3)] })
}

fn v3(p: Vec2) -> cadcraft_geom::Vec3 {
    p.to3(0.0)
}

/// SOLID `{p1, p2, p3, p4?}`: a filled 2D quadrilateral (three points make a triangle).
fn run_solid(s: &mut Session, p: &Value) -> Result<Value> {
    let p1 = point_req("solid", p, "p1")?;
    let p2 = point_req("solid", p, "p2")?;
    let p3 = point_req("solid", p, "p3")?;
    let p4 = point_param(p, "p4").unwrap_or(p3);
    added(s.add_entity(solid_kind(p1, p2, p3, p4))?)
}

#[derive(Default)]
struct SolidM {
    pts: Vec<Vec2>,
}

impl Interactive for SolidM {
    fn name(&self) -> &'static str {
        "SOLID"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        let msg = match self.pts.len() {
            0 => "Specify first point",
            1 => "Specify second point",
            2 => "Specify third point",
            _ => "Specify fourth point",
        };
        let p = Prompt::new(msg, Accept::POINT);
        // The rubber band starts at the point just picked (Enter after three closes the triangle).
        match self.pts.last() {
            Some(last) => p.base(*last),
            None => p,
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Point(p) | Input::PointShift(p) => {
                if self.pts.last().is_some_and(|l| l.near(p, 1e-12)) {
                    s.echo("That is the previous point; pick another.");
                    return Ok(Step::Continue);
                }
                self.pts.push(p);
                if self.pts.len() == 4 {
                    let (a, b, c, d) = (self.pts[0], self.pts[1], self.pts[2], self.pts[3]);
                    added(s.add_entity(solid_kind(a, b, c, d))?)?;
                    return Ok(Step::Done);
                }
                Ok(Step::Continue)
            }
            Input::Enter if self.pts.len() == 3 => {
                let (a, b, c) = (self.pts[0], self.pts[1], self.pts[2]);
                added(s.add_entity(solid_kind(a, b, c, c))?)?;
                Ok(Step::Done)
            }
            Input::Cancel => Ok(Step::Cancel),
            _ => Ok(Step::Continue),
        }
    }
}

/// OOPS: put back what the last ERASE (or a command that erased as it went) removed.
fn run_oops(s: &mut Session, _p: &Value) -> Result<Value> {
    let restored = s.restore_last_deleted()?;
    if restored == 0 {
        s.echo("Nothing to restore: no object was erased by the last command.");
    } else {
        s.echo(format!("{restored} object(s) restored."));
    }
    Ok(json!({ "restored": restored }))
}

/// Escape MTEXT's control characters so a TEXT's literal characters survive the conversion.
fn mtext_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '\n' => out.push_str("\\P"),
            c => out.push(c),
        }
    }
    out
}

/// The MTEXT a TEXT becomes, at the same point, height, rotation and style.
fn as_mtext(t: &Text) -> MText {
    MText {
        insert: t.insert,
        height: t.height,
        width: 0.0,
        attach: 1,
        rotation: t.rotation,
        style: t.style.clone(),
        contents: mtext_escape(&t.value),
        line_spacing: 1.0,
        line_spacing_exact: false,
        columns: None,
    }
}

/// Convert the TEXT objects among `hs` to MTEXT, leaving geometry alone. Returns the new handles.
fn txt_to_mtext(s: &mut Session, hs: &[Handle]) -> Result<Vec<Handle>> {
    let mut out = Vec::new();
    for h in hs {
        let Some(e) = s.doc()?.entity(*h).map(|e| (**e).clone()) else { continue };
        let EntityKind::Text(t) = &e.kind else { continue };
        let space = s.doc()?.space_of(*h).unwrap_or(cadcraft_doc::Space::Model);
        let m = as_mtext(t);
        let d = s.doc_mut()?;
        let nh = d.add(&space, e.common.clone(), EntityKind::MText(m))?;
        d.remove_entity(*h);
        out.push(nh);
    }
    Ok(out)
}

/// TXT2MTXT: convert the selected TEXT objects to MTEXT (MTEXT stays as it is).
fn run_txt2mtxt(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    if hs.is_empty() {
        return Err(bad("txt2mtxt", "nothing selected"));
    }
    let out = txt_to_mtext(s, &hs)?;
    s.echo(format!("{} object(s) converted to MTEXT.", out.len()));
    Ok(json!({ "converted": out.len(), "handles": out.iter().map(|h| h.hex()).collect::<Vec<_>>() }))
}

#[derive(Default)]
struct Txt2MTextM {
    sel: SelectPhase,
}

impl Interactive for Txt2MTextM {
    fn name(&self) -> &'static str {
        "TXT2MTXT"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            let hs = self.sel.picked.clone();
            let n = txt_to_mtext(s, &hs)?.len();
            s.echo(format!("{n} object(s) converted to MTEXT."));
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        self.sel.prompt()
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match self.sel.feed(s, &i)? {
            super::machines::SelOutcome::More => Ok(Step::Continue),
            super::machines::SelOutcome::Empty => Ok(Step::Cancel),
            super::machines::SelOutcome::Done(hs) => {
                let n = txt_to_mtext(s, &hs)?.len();
                s.echo(format!("{n} object(s) converted to MTEXT."));
                Ok(Step::Done)
            }
        }
    }
}

/// SCALETEXT: scale the height of the selected TEXT/MTEXT objects. `factor` multiplies each
/// object's own height (AutoCAD's default); `height` sets them all to one height.
fn run_scaletext(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    if hs.is_empty() {
        return Err(bad("scaletext", "nothing selected"));
    }
    let height = p.get("height").and_then(Value::as_f64).filter(|h| h.is_finite() && *h > 0.0);
    let factor = p.get("factor").and_then(Value::as_f64).filter(|f| f.is_finite() && *f > 0.0);
    if height.is_none() && factor.is_none() {
        return Err(bad("scaletext", "give `height` (absolute) or `factor` (relative)"));
    }
    let d = s.doc_mut()?;
    let mut n = 0;
    for h in &hs {
        let mut counted = false;
        if d.modify_entity(*h, |e| match &mut e.kind {
            EntityKind::Text(t) => {
                t.height = height.unwrap_or(t.height * factor.unwrap_or(1.0));
                counted = true;
            }
            EntityKind::MText(t) => {
                t.height = height.unwrap_or(t.height * factor.unwrap_or(1.0));
                counted = true;
            }
            _ => {}
        })
        .is_ok()
            && counted
        {
            n += 1;
        }
    }
    s.echo(format!("{n} text object(s) rescaled."));
    Ok(json!({ "scaled": n }))
}

#[derive(Default)]
struct ScaleTextM {
    sel: SelectPhase,
    picked: Vec<Handle>,
}

impl Interactive for ScaleTextM {
    fn name(&self) -> &'static str {
        "SCALETEXT"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        if self.sel.done { Prompt::new("Specify new height or scale factor", Accept::NUMBER).default("1.0") } else { self.sel.prompt() }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if !self.sel.done {
            match self.sel.feed(s, &i)? {
                super::machines::SelOutcome::More => return Ok(Step::Continue),
                super::machines::SelOutcome::Empty => return Ok(Step::Cancel),
                super::machines::SelOutcome::Done(hs) => {
                    self.picked = hs;
                    return Ok(Step::Continue);
                }
            }
        }
        let text = match &i {
            Input::Text(t) => t.trim().to_string(),
            Input::Enter => "1.0".to_string(),
            _ => return Ok(Step::Continue),
        };
        let Ok(x) = text.parse::<f64>() else {
            s.echo("A number is required.");
            return Ok(Step::Continue);
        };
        if !(x.is_finite() && x > 0.0) {
            s.echo("The height must be positive.");
            return Ok(Step::Continue);
        }
        let p = json!({ "handles": self.picked.iter().map(|h| h.hex()).collect::<Vec<_>>(), "factor": x });
        run_scaletext(s, &p)?;
        Ok(Step::Done)
    }
}

/// SETBYLAYER: put the selected objects' colour, linetype, lineweight and transparency back to
/// ByLayer, as AutoCAD's does (blocks are not trawled into: this build has no block definition
/// editor for them to be updated in).
fn run_setbylayer(s: &mut Session, p: &Value) -> Result<Value> {
    let hs = targets(s, p)?;
    if hs.is_empty() {
        return Err(bad("setbylayer", "nothing selected"));
    }
    let d = s.doc_mut()?;
    let mut n = 0;
    for h in &hs {
        if d.modify_entity(*h, |e| {
            e.common.color = cadcraft_color::Color::ByLayer;
            e.common.linetype = "ByLayer".to_string();
            e.common.lineweight = cadcraft_doc::Lineweight::ByLayer;
            e.common.transparency = cadcraft_doc::Transparency::ByLayer;
            e.common.plot_style = "ByLayer".to_string();
        })
        .is_ok()
        {
            n += 1;
        }
    }
    s.echo(format!("{n} object(s) set to ByLayer."));
    Ok(json!({ "changed": n }))
}

#[derive(Default)]
struct SetByLayerM {
    sel: SelectPhase,
}

impl Interactive for SetByLayerM {
    fn name(&self) -> &'static str {
        "SETBYLAYER"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            let hs = self.sel.picked.clone();
            run_setbylayer(s, &json!({ "handles": hs.iter().map(|h| h.hex()).collect::<Vec<_>>() }))?;
            return Ok(Step::Done);
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        self.sel.prompt()
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match self.sel.feed(s, &i)? {
            super::machines::SelOutcome::More => Ok(Step::Continue),
            super::machines::SelOutcome::Empty => Ok(Step::Cancel),
            super::machines::SelOutcome::Done(hs) => {
                run_setbylayer(s, &json!({ "handles": hs.iter().map(|h| h.hex()).collect::<Vec<_>>() }))?;
                Ok(Step::Done)
            }
        }
    }
}

/// ABOUT: what this build is.
fn run_about(s: &mut Session, _p: &Value) -> Result<Value> {
    let n = command_specs().len();
    s.echo(format!("CADCraft {} — AutoCAD 2021 compatibility build", env!("CARGO_PKG_VERSION")));
    s.echo(format!("{n} commands registered. Type COMPAT for the AutoCAD 2021 parity report."));
    Ok(json!({ "product": "CADCraft", "version": env!("CARGO_PKG_VERSION"), "commands": n }))
}

/// COMPAT: measure this build against the AutoCAD 2021 command set.
fn run_compat(s: &mut Session, p: &Value) -> Result<Value> {
    let mut missing: Vec<(String, &'static str)> = Vec::new();
    let mut by_area: Vec<(String, usize, usize)> = Vec::new();
    let mut hit = 0;
    for (name, area) in AUTOCAD_2021_COMMANDS {
        let ok = implements(name);
        if ok {
            hit += 1;
        } else {
            missing.push(((*name).to_string(), area));
        }
        match by_area.iter_mut().find(|(a, ..)| a == area) {
            Some(e) => {
                e.1 += 1;
                e.2 += usize::from(ok);
            }
            None => by_area.push(((*area).to_string(), 1, usize::from(ok))),
        }
    }
    let total = AUTOCAD_2021_COMMANDS.len();
    let pct = if total == 0 { 0.0 } else { 100.0 * hit as f64 / total as f64 };
    let pct = (pct * 10.0).round() / 10.0;
    let aliases_mapped = AUTOCAD_2021_ALIASES.iter().filter(|(_, c)| find_command(c).is_some()).count();
    let aliases = AUTOCAD_2021_ALIASES.len();
    let detail = bool_or(p, "detail", false);

    s.echo(format!("AutoCAD 2021 commands: {hit} / {total} ({pct:.1}%)"));
    s.echo(format!("AutoCAD 2021 default aliases: {aliases_mapped} / {aliases}"));
    for (area, n, covered) in &by_area {
        s.echo(format!("  {area:<12} {covered}/{n}"));
    }
    if detail {
        for (name, area) in &missing {
            s.echo(format!("  missing: {name} ({area})"));
        }
    } else {
        s.echo("COMPAT {detail: true} lists every missing command.");
    }
    Ok(json!({
        "reference": total,
        "implemented": hit,
        "missing": missing.len(),
        "percent": pct,
        "aliases": { "reference": aliases, "mapped": aliases_mapped },
        "byArea": by_area.iter().map(|(a, n, c)| json!({"area": a, "commands": n, "implemented": c})).collect::<Vec<_>>(),
        "missingCommands": missing.iter().map(|(n, a)| json!({"command": n, "area": a})).collect::<Vec<_>>(),
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("solid", "2D Solid", run_solid)
            .menu(&["Draw", "Solid"])
            .alias(&["so"])
            .params("{p1, p2, p3, p4?} (three points make a triangle)")
            .interactive(|_| Ok(Box::new(SolidM::default()))),
        CommandSpec::new("oops", "Oops", run_oops).menu(&["Modify", "Oops"]).params("(restores what the last ERASE removed)"),
        CommandSpec::new("txt2mtxt", "Convert Text to MText", run_txt2mtxt)
            .menu(&["Express", "Text", "Convert Text to MText"])
            .params("{handles?} (default: the selection)")
            .interactive(|_| Ok(Box::new(Txt2MTextM::default()))),
        CommandSpec::new("scaletext", "Scale Text", run_scaletext)
            .menu(&["Annotate", "Scale Text"])
            .params("{handles?, height? | factor?}")
            .interactive(|_| Ok(Box::new(ScaleTextM::default()))),
        CommandSpec::new("setbylayer", "Set ByLayer", run_setbylayer)
            .menu(&["Format", "Set ByLayer"])
            .params("{handles?} (default: the selection)")
            .interactive(|_| Ok(Box::new(SetByLayerM::default()))),
        CommandSpec::new("about", "About", run_about).menu(&["Help", "About"]).enabled(always).noundo(),
        CommandSpec::new("compat", "AutoCAD 2021 Compatibility Report", run_compat)
            .menu(&["Help", "Compatibility Report"])
            .params("{detail?: bool} → {reference, implemented, missing, percent, aliases, byArea, missingCommands}")
            .enabled(always)
            .noundo(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_alias_points_at_a_real_command() {
        for (a, c) in AUTOCAD_2021_ALIASES {
            assert!(find_command(c).is_some(), "alias {a} points at unknown command {c}");
        }
    }

    #[test]
    fn default_aliases_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for (a, _) in AUTOCAD_2021_ALIASES {
            assert!(seen.insert(*a), "alias {a} is listed twice");
        }
    }

    #[test]
    fn reference_commands_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for (c, _) in AUTOCAD_2021_COMMANDS {
            assert!(seen.insert(*c), "reference command {c} is listed twice");
        }
    }

    #[test]
    fn aliases_resolve_to_their_command() {
        for (a, c) in AUTOCAD_2021_ALIASES {
            let id = resolve_alias(a);
            assert_eq!(id, *c, "alias {a}");
            assert!(find_command(&id).is_some(), "alias {a} resolves to unknown command {id}");
            // Upper case too: the command line is case-insensitive.
            assert_eq!(resolve_alias(&a.to_ascii_uppercase()), *c, "alias {a} in upper case");
        }
    }

    #[test]
    fn a_command_name_beats_an_alias() {
        // `line` is a command, so the alias table cannot shadow it.
        assert_eq!(resolve_alias("line"), "line");
        assert_eq!(resolve_alias("L"), "line");
    }

    #[test]
    fn the_report_measures_the_registry() {
        let mut s = Session::new();
        let r = run_compat(&mut s, &json!({})).unwrap();
        let total = r["reference"].as_u64().unwrap() as usize;
        let done = r["implemented"].as_u64().unwrap() as usize;
        let missing = r["missing"].as_u64().unwrap() as usize;
        assert_eq!(total, AUTOCAD_2021_COMMANDS.len());
        assert_eq!(done + missing, total);
        // A floor, not a target: it exists so a change that loses commands fails here. Raise it
        // when coverage grows; never lower it.
        assert!(done >= 242, "coverage regressed: {done}/{total}");
    }

    #[test]
    fn solid_builds_a_quad_in_autocad_order() {
        let mut s = Session::new();
        s.execute("solid", &json!({"p1": [0, 0], "p2": [1, 0], "p3": [1, 1], "p4": [0, 1]})).unwrap();
        let EntityKind::Solid(so) = &s.doc().unwrap().model.last().unwrap().kind else { panic!("not a solid") };
        // AutoCAD's Z order: p1, p2, p4, p3.
        assert!(so.corners[2].xy().near(Vec2::new(0.0, 1.0), 1e-9));
        assert!(so.corners[3].xy().near(Vec2::new(1.0, 1.0), 1e-9));
    }

    #[test]
    fn three_points_make_a_triangle() {
        let mut s = Session::new();
        s.execute("solid", &json!({"p1": [0, 0], "p2": [1, 0], "p3": [0, 1]})).unwrap();
        let EntityKind::Solid(so) = &s.doc().unwrap().model.last().unwrap().kind else { panic!("not a solid") };
        assert!(so.corners[2].xy().near(so.corners[3].xy(), 1e-12));
    }

    #[test]
    fn txt2mtxt_keeps_the_geometry() {
        let mut s = Session::new();
        let h = s.execute("text", &json!({"at": [1, 2], "height": 0.5, "text": "A{B}\\C"})).unwrap()["handle"].as_str().unwrap().to_string();
        let r = s.execute("txt2mtxt", &json!({"handles": [h]})).unwrap();
        assert_eq!(r["converted"], 1);
        let e = s.doc().unwrap().model.last().unwrap();
        let EntityKind::MText(m) = &e.kind else { panic!("not mtext") };
        assert!(m.insert.xy().near(Vec2::new(1.0, 2.0), 1e-9));
        assert!((m.height - 0.5).abs() < 1e-9);
        // The characters survive as literal text, escaped for MTEXT.
        assert_eq!(m.contents, "A\\{B\\}\\\\C");
    }

    #[test]
    fn scaletext_scales_by_factor_and_by_height() {
        let mut s = Session::new();
        let h = s.execute("text", &json!({"at": [0, 0], "height": 2.0, "text": "x"})).unwrap()["handle"].as_str().unwrap().to_string();
        s.execute("scaletext", &json!({"handles": [h], "factor": 2.5})).unwrap();
        let EntityKind::Text(t) = &s.doc().unwrap().entity(cadcraft_doc::Handle::parse_hex(&h).unwrap()).unwrap().kind else { panic!() };
        assert!((t.height - 5.0).abs() < 1e-9);
        s.execute("scaletext", &json!({"handles": [h], "height": 0.25})).unwrap();
        let EntityKind::Text(t) = &s.doc().unwrap().entity(cadcraft_doc::Handle::parse_hex(&h).unwrap()).unwrap().kind else { panic!() };
        assert!((t.height - 0.25).abs() < 1e-9);
    }

    #[test]
    fn oops_restores_after_erase() {
        let mut s = Session::new();
        let h = s.execute("line", &json!({"points": [[0, 0], [1, 1]]})).unwrap()["handles"][0].as_str().unwrap().to_string();
        s.execute("erase", &json!({"handles": [h]})).unwrap();
        assert!(s.doc().unwrap().model.is_empty());
        let r = s.execute("oops", &json!({})).unwrap();
        assert_eq!(r["restored"], 1);
        assert_eq!(s.doc().unwrap().model.len(), 1);
        // Nothing left to restore the second time.
        assert_eq!(s.execute("oops", &json!({})).unwrap()["restored"], 0);
    }

    #[test]
    fn setbylayer_resets_object_overrides() {
        let mut s = Session::new();
        let h = s.execute("line", &json!({"points": [[0, 0], [1, 1]]})).unwrap()["handles"][0].as_str().unwrap().to_string();
        s.execute("properties.set", &json!({"handles": [h], "color": 1, "linetype": "ByLayer", "lineweight": 50, "ltscale": 2.0})).unwrap();
        s.execute("setbylayer", &json!({"handles": [h]})).unwrap();
        let e = s.doc().unwrap().entity(cadcraft_doc::Handle::parse_hex(&h).unwrap()).unwrap().clone();
        assert!(matches!(e.common.color, cadcraft_color::Color::ByLayer));
        assert!(matches!(e.common.lineweight, cadcraft_doc::Lineweight::ByLayer));
    }
}
