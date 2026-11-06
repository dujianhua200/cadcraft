//! The drawing area: grid, entities, selection highlighting, grips, rubber bands, object-snap
//! markers, crosshair cursor, ViewCube, UCS icon and mouse handling.

use cadcraft_color::Rgb;
use cadcraft_doc::Handle;
use cadcraft_engine::Input;
use cadcraft_engine::snap::{self, SnapHit};
use cadcraft_geom::{Bounds2, Mat3, Vec2};
use cadcraft_render::{DisplayList, Kind};
use egui::{Color32, Pos2, Rect, Sense, Shape, Stroke, pos2, vec2};

use crate::CadApp;
use crate::theme::Tokens;

mod cursor;
mod cycle;
mod draw;
mod dyn_boxes;
mod grid;
mod grips;
mod list;
mod pick;
mod viewport;
mod widgets;

use cursor::{draw_crosshair, draw_snap_marker, draw_tracking, effective_point, override_cursor};
use cycle::{CycleList, cycle_badge, cycle_click, cycle_list, over_canvas_menu, shift_space_cycle};
pub(crate) use draw::{Look, draw_highlight, draw_list};
use draw::{dashed_polygon, dashed_rect, draw_drag_gpu, draw_highlight_gpu, draw_list_gpu, list_mesh_key, selection_area_fill};
use dyn_boxes::dynamic_input;
pub(crate) use dyn_boxes::length_units;
use grid::draw_grid;
use grips::{click_grip, grip_at, grip_menu, grip_options, grip_rect};
use list::{OnlyList, PrimIndex, ensure_list, rebuilt_preview};
#[cfg(test)]
use list::{preview_options, prims_of};
use pick::{hatch_at, wheel_zoom_points};
#[cfg(test)]
use viewport::viewport_highlight_tolerance;
use viewport::{VpContent, VpHighlight, draw_vp_content, ensure_vp_content};
use widgets::{draw_ucs_icon, draw_viewcube, tile_rects, tile_xf, viewport_label};

/// World ↔ screen transform for the canvas rect.
#[derive(Clone, Copy, Debug)]
pub struct Xf {
    pub rect: Rect,
    pub center: Vec2,
    /// Pixels (points) per drawing unit.
    pub scale: f64,
    /// The view twist (radians counterclockwise, `cadcraft_engine::View::twist`): the drawing
    /// appears turned by it about `center`. 0 for paper space.
    pub twist: f64,
}

impl Xf {
    /// A world offset from `center` in the screen's frame (y up): turned by the twist.
    fn turn(&self, v: Vec2) -> Vec2 {
        if self.twist == 0.0 { v } else { v.rotate(self.twist) }
    }
    pub fn to_screen(&self, p: Vec2) -> Pos2 {
        let c = self.rect.center();
        let q = self.turn(p - self.center);
        pos2(c.x + (q.x * self.scale) as f32, c.y - (q.y * self.scale) as f32)
    }
    pub fn to_world(&self, p: Pos2) -> Vec2 {
        let c = self.rect.center();
        self.center + self.screen_delta_to_world(p - c)
    }
    /// The world offset a screen offset `d` (points, y down) covers: a pan moves the view by it.
    pub fn screen_delta_to_world(&self, d: egui::Vec2) -> Vec2 {
        let q = Vec2::new(f64::from(d.x) / self.scale, -f64::from(d.y) / self.scale);
        if self.twist == 0.0 { q } else { q.rotate(-self.twist) }
    }
    /// The world box holding everything on screen (in a twisted view, the box of the rect's
    /// four corners).
    pub fn world_bounds(&self) -> Bounds2 {
        let r = self.rect;
        Bounds2::from_points([r.left_bottom(), r.right_top(), r.left_top(), r.right_bottom()].map(|p| self.to_world(p)))
    }
    /// The GPU transform ([`crate::gpu::CanvasCallback::xform`]) that draws vertex data built at
    /// `origin`, moved by `m` (a drag; [`Mat3::IDENTITY`] for the drawing), in this view: the
    /// shader takes `L·p + e + (origin − center)` to the screen frame, which must be
    /// `R(m(origin + p) − center)` with `R` the twist, so `L = R·m` and
    /// `e = R(m(origin) − center) − (origin − center)`.
    pub(crate) fn gpu_xform(&self, origin: Vec2, m: Mat3) -> [f32; 6] {
        let (a, b) = (self.turn(Vec2::new(m.a, m.b)), self.turn(Vec2::new(m.c, m.d)));
        let e = self.turn(m.apply(origin) - self.center) - (origin - self.center);
        [a.x as f32, a.y as f32, b.x as f32, b.y as f32, e.x as f32, e.y as f32]
    }
}

/// What a display list was built from: drawing uid, revision, tessellation band, hatch level of
/// detail band, lineweight display, document and active viewport view (see [`ensure_list`]).
type ListKey = (u64, u64, i32, i32, bool, bool, usize, u64);

/// Whether two list keys are for the same drawing content (they may differ only in the view).
fn same_content(a: &ListKey, b: &ListKey) -> bool {
    (a.0, a.1, a.4, a.5, a.6, a.7) == (b.0, b.1, b.4, b.5, b.6, b.7)
}

#[derive(Default)]
pub struct CanvasState {
    pub list: Option<DisplayList>,
    /// The font set the list was drawn with ([`cadcraft_fonts::ttf::generation`] and
    /// [`cadcraft_fonts::shx::generation`]): a new font folder redraws the text.
    font_generation: u64,
    key: ListKey,
    /// The area `list` was built for (hatch patterns exist only inside it); a view leaving it
    /// rebuilds the list.
    window: Option<Bounds2>,
    /// A rebuild for a new view running on a worker thread (see [`rebuild`]); `list` is drawn
    /// meanwhile.
    job: Option<rebuild::Job>,
    /// Bumped whenever `list` is rebuilt (identifies it for [`PrimIndex`]).
    list_gen: u64,
    prim_index: PrimIndex,
    /// Set when the app runs on wgpu: entities are drawn by [`crate::gpu`]; otherwise on the CPU.
    pub gpu: Option<crate::gpu::GpuTarget>,
    /// The wgpu device, for background rebuilds to create their buffers (see [`rebuild`]).
    pub gpu_device: Option<egui_wgpu::wgpu::Device>,
    /// The mesh last handed to the GPU (see [`crate::gpu::CanvasCallback::key`]).
    mesh_key: Option<u64>,
    mesh_slot: crate::gpu::MeshSlot,
    mesh_origin: Vec2,
    /// The dragged objects of a MOVE/COPY/ROTATE/SCALE preview, uploaded once per drag (see
    /// [`draw_drag_gpu`]).
    drag_key: Option<u64>,
    drag_slot: crate::gpu::MeshSlot,
    drag_origin: Vec2,
    /// The selection highlight on the GPU (see [`draw_highlight_gpu`]).
    hl_key: Option<u64>,
    hl_slot: crate::gpu::MeshSlot,
    hl_origin: Vec2,
    /// Selected infinite lines (clipped and drawn on the CPU each frame).
    hl_infinite: Vec<usize>,
    /// The selection and hover highlights inside the active MSPACE viewport (see [`VpHighlight`]).
    vp_sel: VpHighlight,
    vp_hover: VpHighlight,
    /// Model space seen through the active MSPACE viewport (see [`VpContent`]).
    vp: VpContent,
    /// What a running command shows in place of the drawing (see [`OnlyList`]).
    only: OnlyList,
    /// Indices of infinite-line primitives in `list` (drawn on the CPU in GPU mode).
    infinite: Vec<usize>,
    /// Milliseconds spent converting the display list to vertex data (last rebuild).
    pub mesh_ms: f64,
    pub rect: Option<Rect>,
    pub xf: Option<Xf>,
    pub hover: Option<Handle>,
    pub snap: Option<SnapHit>,
    /// Effective cursor point after snaps/ortho/polar.
    pub cursor: Option<Vec2>,
    /// The polar or object snap tracking path(s) the cursor is on.
    pub track: Option<snap::tracking::Track>,
    pan_last: Option<Pos2>,
    /// True when the latest scroll came from a trackpad (or other precise-scrolling device):
    /// scrolling then pans and pinch zooms. A notched mouse wheel clears it so the wheel zooms.
    scroll_pans: bool,
    /// Hot grips (object, grip index): the ones Shift+clicked while idle, then the ones the
    /// running GRIP edit works on (the engine's grip modes take them from there).
    pub hot_grips: Vec<(Handle, usize)>,
    /// Grip dimensional input: the value fields of the grip being stretched.
    pub grip_dyn: crate::grip_dyn::GripDyn,
    /// A GRIP edit was running last frame (its grips go cold when it ends).
    grip_editing: bool,
    /// The grip whose menu (Stretch, Add Vertex, Lengthen…) a right-click opened.
    grip_menu: Option<(Handle, usize)>,
    /// Dynamic block grips of the selection (cached), the one being dragged, and the list grip
    /// whose choices are open (and whether it opened this frame).
    pub(crate) dyn_grips: crate::dyngrips::Cache,
    pub(crate) dyn_hot: Option<(Handle, i32, cadcraft_engine::doc::dynblock::GripPart)>,
    pub(crate) dyn_menu: Option<(Handle, cadcraft_engine::doc::dynblock::DynGrip)>,
    pub(crate) dyn_menu_fresh: bool,
    /// Zoom to extents once the canvas size is known (after opening a drawing).
    pub zoom_pending: bool,
    pub build_ms: f64,
    pub draw_ms: f64,
    hover_at: Option<Pos2>,
    /// Constraint glyphs for the parametric overlay (cached per drawing revision).
    pub param: crate::parametric::Cache,
    /// Basemap tile textures + failed-tile back-off (see [`crate::tiles`]).
    pub(crate) tilemap: crate::tiles::TileCache,
    /// A selection window opened by a press-and-drag, to close where the button is let go.
    drag_window: bool,
    /// A freehand TRIM/EXTEND drag path being collected (world points): a press that
    /// never really moves stays a click, decided by the path's on-screen length.
    drag_trim: Option<Vec<Vec2>>,
    /// The right-click that opened the shortcut menu held Shift or Ctrl: it is the object snap
    /// menu ([`crate::context_menu`]).
    pub snap_menu: bool,
    /// Tab was pressed over a snap marker: show the next candidate ([`SnapCycle`]).
    pub snap_tab: bool,
    pub snap_cycle: Option<SnapCycle>,
    /// The selection cycling list a click over overlapping objects opened (SELECTIONCYCLING 2).
    cycle: Option<CycleList>,
    /// Selection cycling's badge shows beside the cursor, and where it was last worked out.
    cycle_badge: bool,
    cycle_badge_at: Option<Pos2>,
    /// Shift+Space stepping the highlight through the objects under the cursor (selection
    /// cycling's temporary override): where it started, the objects, which one is highlighted.
    hover_cycle: Option<(Pos2, Vec<Handle>, usize)>,
    /// What the cycling list and the Shift+Space highlight were made for (see [`CycleKey`]).
    cycle_key: Option<CycleKey>,
}

/// The command, prompt, drawing (and its revision) and view that the objects under the cursor
/// were listed for: when any of them changes, the list and the Shift+Space highlight are stale.
type CycleKey = (Option<String>, Option<String>, u64, u64, Vec2, f64);

impl CanvasState {
    /// The active MSPACE viewport's contents when they are drawn apart from the layout's list
    /// (see [`VpContent`]): where they are on paper at the viewport's current view, and how many
    /// times they have been built.
    pub fn viewport_content(&self) -> Option<(Bounds2, u64)> {
        self.vp.key.and(self.vp.paper_bounds()).map(|b| (b, self.vp.builds))
    }

    /// The rows of the open selection cycling list (screen rectangle, object), top to bottom.
    pub fn cycle_rows(&self) -> Vec<(Rect, Handle)> {
        self.cycle.as_ref().map(|c| c.rows.iter().copied().zip(c.items.iter().map(|(h, _, _)| *h)).collect()).unwrap_or_default()
    }

    /// Whether a rebuild for the current view is still running in the background (the canvas
    /// shows the previous list until it lands).
    pub fn building(&self) -> bool {
        self.job.is_some()
    }

    /// Put a finished background build in place, with its GPU vertex data when it has some.
    fn install(&mut self, key: ListKey, window: Option<Bounds2>, built: rebuild::Built) {
        self.list_gen = self.list_gen.wrapping_add(1);
        if let Some(m) = built.mesh {
            if let Ok(mut slot) = self.mesh_slot.lock() {
                *slot = Some(m.data);
            }
            self.mesh_key = Some(list_mesh_key(&key, self.list_gen, m.origin, m.bg, m.lwdisplay));
            self.mesh_origin = m.origin;
            self.mesh_ms = m.ms;
            self.infinite = m.infinite;
        }
        self.list = Some(built.list);
        self.key = key;
        self.window = window;
        self.build_ms = built.build_ms;
    }
}

/// Tab cycling through the candidate snaps under the cursor: where it started and which
/// candidate shows. Moving the cursor out of the aperture ends it.
#[derive(Clone, Copy, Debug)]
pub struct SnapCycle {
    anchor: Vec2,
    index: usize,
}

#[derive(Clone, Copy)]
struct PointerInput {
    hover_pos: Option<Pos2>,
    scroll: egui::Vec2,
    mods: egui::Modifiers,
    middle_down: bool,
    pressed_primary: bool,
    pressed_secondary: bool,
    dbl_middle: bool,
    dbl_primary: bool,
}

struct Navigation<'a> {
    rect: Rect,
    tiles: &'a [(Rect, cadcraft_engine::ModelTile)],
    response: &'a egui::Response,
    xf: Xf,
    pointer: PointerInput,
}

fn navigate_canvas(app: &mut CadApp, ui: &mut egui::Ui, nav: &Navigation<'_>) -> Option<(bool, bool)> {
    let p = nav.pointer;
    let inside = p.hover_pos.is_some_and(|at| nav.rect.contains(at) && ui.ctx().layer_id_at(at) == Some(ui.layer_id()) && nav.response.hovered());
    let on_menu = p.hover_pos.is_some_and(|at| over_canvas_menu(app, at));
    app.session.ctrl_down = p.mods.ctrl;
    ui.input(|i| {
        for event in &i.raw.events {
            if let egui::Event::MouseWheel { unit, .. } = event {
                app.canvas.scroll_pans = cfg!(target_os = "macos") && *unit == egui::MouseWheelUnit::Point;
            }
        }
    });
    handle_scroll_zoom(app, ui, nav, inside);
    handle_button_navigation(app, nav, inside);
    if nav.response.dragged_by(egui::PointerButton::Primary) && app.session.realtime_zoom_active() {
        let _ = app.session.realtime_zoom(f64::from(nav.response.drag_delta().y));
    }
    if p.pressed_primary
        && let Some(at) = p.hover_pos
        && !nav.rect.contains(at)
        && let Some(i) = nav.tiles.iter().position(|(r, _)| r.contains(at))
    {
        if let Err(e) = app.session.click_tile(i) {
            app.session.echo(e.to_string());
        }
        ui.ctx().request_repaint();
        return None;
    }
    Some((inside, on_menu))
}

fn handle_scroll_zoom(app: &mut CadApp, ui: &mut egui::Ui, nav: &Navigation<'_>, inside: bool) {
    let p = nav.pointer;
    if inside
        && app.canvas.scroll_pans
        && p.scroll != egui::Vec2::ZERO
        && let Ok(st) = app.session.state_mut()
    {
        let view = st.view();
        st.set_view_quiet(cadcraft_engine::View {
            center: view.center - nav.xf.screen_delta_to_world(p.scroll),
            height: view.height,
            twist: view.twist,
        });
    }
    let opts = ui.ctx().options(|o| o.input_options);
    let wheel = ui.input(|i| wheel_zoom_points(&i.raw.events, &opts, nav.rect.height()));
    if inside
        && !app.canvas.scroll_pans
        && wheel != 0.0
        && let Some(at) = p.hover_pos
    {
        let factor = app.session.wheel_zoom_factor(f64::from(wheel));
        let about = nav.xf.to_world(at);
        let _ = app.session.zoom_about(factor, about);
    }
    let pinch = ui.input(|i| crate::animation::zoom_delta(i, &opts, app.shows_animations()));
    if inside
        && (pinch - 1.0).abs() > 1e-4
        && let Some(at) = p.hover_pos
    {
        let _ = app.session.zoom_about(f64::from(pinch), nav.xf.to_world(at));
    }
}

fn handle_button_navigation(app: &mut CadApp, nav: &Navigation<'_>, inside: bool) {
    let p = nav.pointer;
    let middle_pressed = nav.response.ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Middle));
    let focused = nav.response.ctx.input(|i| i.focused);
    if p.middle_down
        && focused
        && (app.canvas.pan_last.is_some() || (middle_pressed && inside))
        && let Some(at) = p.hover_pos
    {
        if let Some(last) = app.canvas.pan_last {
            let delta = at - last;
            if let Ok(st) = app.session.state_mut() {
                let view = st.view();
                st.set_view_quiet(cadcraft_engine::View {
                    center: view.center - nav.xf.screen_delta_to_world(delta),
                    height: view.height,
                    twist: view.twist,
                });
            }
        }
        app.canvas.pan_last = Some(at);
    } else {
        app.canvas.pan_last = None;
    }
    if p.dbl_middle && inside {
        let _ = app.session.zoom_extents();
    }
}

struct CanvasPaint<'a> {
    app: &'a mut CadApp,
    ui: &'a mut egui::Ui,
    painter: &'a egui::Painter,
    tokens: Tokens,
    full: Rect,
    tiles: &'a [(Rect, cadcraft_engine::ModelTile)],
    rect: Rect,
    xf: Xf,
    xf_paper: Xf,
    view: cadcraft_engine::View,
    active_vp: Option<cadcraft_doc::Viewport>,
    active_clip: Option<cadcraft_render::clip::PolyClip>,
    inside: bool,
    on_menu: bool,
    hover_pos: Option<Pos2>,
    raw_world: Option<Vec2>,
}

fn paint_content_highlights(frame: &mut CanvasPaint<'_>, bg: Rgb, sel: &[Handle], preview_look: Look, selection_look: Option<Look>) {
    let app = &mut *frame.app;
    let painter = frame.painter;
    let xf_paper = frame.xf_paper;
    let rect = frame.rect;
    let xf = frame.xf;
    let t = frame.tokens;
    let ui = &mut *frame.ui;
    let active_vp = &frame.active_vp;
    let active_clip = &frame.active_clip;
    let scale = frame.xf.scale;
    // The active viewport's contents, drawn apart from the layout's list, under its own objects.
    // `masked`: its outline is in the stencil buffer this frame.
    let mut masked = false;
    if let Some(only) = app.session.display_only() {
        // A command that shows only some objects while it runs (DVIEW's preview set): those,
        // in the view being changed (inside the active viewport, clipped to it).
        let clip = active_vp.as_ref().map_or(rect, |vp| {
            Rect::from_two_pos(
                xf_paper.to_screen(Vec2::new(vp.center.x - vp.width / 2.0, vp.center.y + vp.height / 2.0)),
                xf_paper.to_screen(Vec2::new(vp.center.x + vp.width / 2.0, vp.center.y - vp.height / 2.0)),
            )
            .intersect(rect)
        });
        if let Ok(st) = app.session.state() {
            // A clipped viewport's outline, in the model units the preview is built in.
            let outline =
                active_vp.as_ref().zip(active_clip.as_ref()).map(|(vp, c)| c.polygon().iter().map(|q| vp.paper_to_model(*q)).collect::<Vec<_>>());
            app.canvas.only.update(st, &only, scale, outline.as_deref());
        }
        draw_list(&painter.with_clip_rect(clip), &xf, &app.canvas.only.list, bg, app.session.settings.lwdisplay);
    } else {
        masked = draw_active_viewport(app, &xf_paper, painter, rect, ui.ctx(), bg);
        draw_basemap(app, painter, &xf_paper, rect, ui.ctx());
        if app.canvas.gpu.is_some() {
            draw_list_gpu(&mut app.canvas, painter, &xf_paper, bg, app.session.settings.lwdisplay);
        } else if let Some(list) = &app.canvas.list {
            draw_list(painter, &xf_paper, list, bg, app.session.settings.lwdisplay);
        }
    }
    if let Some(vp) = &active_vp {
        // The active viewport: a heavy border, highlights in model units clipped to it.
        let r = Rect::from_two_pos(
            xf_paper.to_screen(Vec2::new(vp.center.x - vp.width / 2.0, vp.center.y + vp.height / 2.0)),
            xf_paper.to_screen(Vec2::new(vp.center.x + vp.width / 2.0, vp.center.y - vp.height / 2.0)),
        );
        let heavy = Stroke::new(3.0, Color32::from_rgb(0x2a, 0x2a, 0x2a));
        match &active_clip {
            Some(c) => {
                let mut pts: Vec<egui::Pos2> = c.polygon().iter().map(|q| xf_paper.to_screen(*q)).collect();
                pts.extend(pts.first().copied());
                painter.add(egui::Shape::line(pts, heavy));
            }
            None => {
                painter.rect_stroke(r, 0.0, heavy, egui::StrokeKind::Middle);
            }
        }
        if let Ok(st) = app.session.state() {
            // A clipped viewport's outline bounds (its outline is masked when `masked`).
            let bounds = active_clip.as_ref().map_or(r, |c| {
                let b = c.bounds();
                Rect::from_two_pos(xf_paper.to_screen(Vec2::new(b.min.x, b.max.y)), xf_paper.to_screen(Vec2::new(b.max.x, b.min.y)))
            });
            let clipped = painter.with_clip_rect(bounds.intersect(rect));
            let c = &mut app.canvas;
            if let Some(h) = c.hover
                && !sel.contains(&h)
            {
                c.vp_hover.update(st, &[h], scale);
                let list = &c.vp_hover.list;
                draw_highlight(&clipped, &xf, list, &(0..list.prims.len()).collect::<Vec<_>>(), t.hover, preview_look);
            }
            if let Some(look) = selection_look
                && !sel.is_empty()
            {
                c.vp_sel.update(st, sel, scale);
                let list = &c.vp_sel.list;
                let all = || (0..list.prims.len()).collect::<Vec<_>>();
                if c.gpu.is_some() {
                    // Shares the GPU highlight layer with the layout's own selection; the high bit
                    // keeps this list's generations apart from the layout list's.
                    let list_id = c.vp_sel.list_gen | 1 << 63;
                    let stencil = if masked { crate::gpu::Stencil::Inside } else { crate::gpu::Stencil::Off };
                    let hl = (&mut c.hl_key, &c.hl_slot, &mut c.hl_origin, &mut c.hl_infinite);
                    draw_highlight_gpu(&clipped, &xf, list, list_id, all, sel, t.selection, look, hl, stencil);
                    draw_highlight(&clipped, &xf, list, &c.hl_infinite, t.selection, look);
                } else {
                    draw_highlight(&clipped, &xf, list, &all(), t.selection, look);
                }
            }
        }
    } else if let CanvasState { list: Some(list), list_gen, prim_index, hover, gpu, hl_key, hl_slot, hl_origin, hl_infinite, .. } = &mut app.canvas {
        if let Some(h) = *hover
            && !sel.contains(&h)
        {
            draw_highlight(painter, &xf, list, &prim_index.prims(*list_gen, list, &[h]), t.hover, preview_look);
        }
        if let Some(look) = selection_look {
            if gpu.is_some() && !sel.is_empty() {
                let list_id = *list_gen;
                let hl = (hl_key, &*hl_slot, hl_origin, &mut *hl_infinite);
                draw_highlight_gpu(
                    painter,
                    &xf,
                    list,
                    list_id,
                    || prim_index.prims(list_id, list, sel),
                    sel,
                    t.selection,
                    look,
                    hl,
                    crate::gpu::Stencil::Off,
                );
                draw_highlight(painter, &xf, list, hl_infinite, t.selection, look);
            } else {
                draw_highlight(painter, &xf, list, &prim_index.prims(*list_gen, list, sel), t.selection, look);
            }
        }
    }
}

fn paint_pointer_overlay(
    frame: &mut CanvasPaint<'_>,
    paper_rect: Option<Rect>,
    surround: Color32,
    selecting: bool,
    sheet: Option<cadcraft_render::Sheet>,
) {
    let app = &mut *frame.app;
    let ui = &mut *frame.ui;
    let painter = frame.painter;
    let t = frame.tokens;
    let rect = frame.rect;
    let full = frame.full;
    let tiles = frame.tiles;
    let xf = frame.xf;
    let inside = frame.inside;
    let on_menu = frame.on_menu;
    let hover_pos = frame.hover_pos;
    let view = frame.view;
    // Acquired tracking points and the polar / tracking paths under the cursor.
    if app.canvas.cursor.is_some() {
        draw_tracking(painter, &xf, &app.session.tracking.acquired, app.canvas.track.as_ref(), view.height * 4.0);
    }
    if let (Some(hit), Some(_)) = (app.canvas.snap, app.canvas.cursor) {
        draw_snap_marker(painter, xf.to_screen(hit.point), &hit);
    }
    draw_tiles(app, ui, tiles);
    // UCS icon, ViewCube and viewport label.
    if app.ui.show_ucs_icon {
        draw_ucs_icon(painter, rect);
        crate::viewcube::ucs_icon(app, ui, rect);
    }
    if app.ui.show_viewcube && sheet.is_none() {
        draw_viewcube(app, ui, rect);
    }
    viewport_label(painter, rect);
    // Typing and Escape reach the command line even when its overlay is hidden.
    crate::cmdline::keyboard(app, ui.ctx());
    // Command line overlay.
    if app.ui.show_command_line {
        crate::cmdline::show(app, ui, full);
    }
    // Crosshair cursor.
    if inside && !on_menu {
        // Hide the OS cursor under the drawn crosshair, unless assistive tech needs it (#39).
        ui.ctx().set_cursor_icon(if app.ui.system_cursor { egui::CursorIcon::Crosshair } else { egui::CursorIcon::None });
        if let Some(hp) = hover_pos {
            // Use the painted sheet even in MSPACE, where the editing transform follows a
            // model viewport. The whole cursor takes one colour from its centre.
            let ink = paper_rect.map_or(t.crosshair, |paper| crate::theme::crosshair_color(if paper.contains(hp) { t.paper } else { surround }));
            draw_crosshair(app, painter, rect, hp, selecting, ink);
            if app.canvas.cycle_badge && app.canvas.cycle.is_none() {
                cycle_badge(painter, hp);
            }
        }
    }
    if inside
        && app.session.settings.dynmode
        && let Some(hp) = hover_pos
    {
        // A stretched grip shows its dimensions instead of the pointer boxes; a prompt asking for
        // a distance shows its one field and keeps the prompt and its options.
        let fields = crate::grip_dyn::draw(app, painter);
        if !fields || app.session.current_prompt().is_some_and(|p| p.distance) {
            dynamic_input(app, painter, hp, fields);
        }
    }
    // Over the crosshair, so the row under the pointer reads clearly.
    cycle_list(app, painter);
    // No continuous repaint while hovering: input events repaint by themselves. Repainting every
    // frame kept a vsync swapchain's queue full (every frame waited behind queued ones) and would
    // spin the CPU with the low-latency present mode (`gpu::surface_config`).
}
struct DoubleClickContext<'a> {
    inside: bool,
    double: bool,
    hover: Option<Pos2>,
    xf: Xf,
    xf_paper: Xf,
    scale: f64,
    active_viewport: &'a Option<cadcraft_doc::Viewport>,
    active_clip: &'a Option<cadcraft_render::clip::PolyClip>,
}

fn handle_double_click(app: &mut CadApp, ctx: DoubleClickContext<'_>) -> Option<bool> {
    let DoubleClickContext { inside, double, hover, xf, xf_paper, scale, active_viewport, active_clip } = ctx;
    if inside
        && double
        && !app.session.prompting()
        && matches!(app.session.layout_space(), cadcraft_doc::Space::Paper(_))
        && let Some(hp) = hover
    {
        let pp = xf_paper.to_world(hp);
        let in_rect = |v: &cadcraft_doc::Viewport| (pp.x - v.center.x).abs() <= v.width / 2.0 && (pp.y - v.center.y).abs() <= v.height / 2.0;
        let in_active = active_viewport.as_ref().is_some_and(|v| active_clip.as_ref().map_or(in_rect(v), |c| c.contains(pp)));
        if !in_active {
            app.canvas.hot_grips.clear();
            app.session.pending_window = None;
            app.canvas.drag_window = false;
            if app.run("mspace", serde_json::json!({"at": [pp.x, pp.y]})).is_ok() {
                // First double-click inside a layout viewport only enters model space;
                // say so (in the user's language) instead of silently doing nothing.
                app.session.echo(crate::i18n::t("Entered model space (MSPACE). Double-click the object again to edit it."));
            } else if active_viewport.is_some() {
                let _ = app.run("pspace", serde_json::json!({}));
            }
            app.session.pending_window = None;
            app.canvas.drag_window = false;
            app.canvas.list = None;
            return None;
        }
    }
    // Double-click text to edit it, as in AutoCAD: TEXTEDIT with that object already picked.
    if inside
        && double
        && app.session.running.is_none()
        && let Some(hp) = hover
        && edit_text_at(app, xf.to_world(hp))
    {
        return None;
    }

    let mut hatch_dialog = false;
    // Double-click a hatch (no command running) to edit it in the Hatch Edit dialog. The click is
    // used up: it must not also start a grip edit on the hatch's centre grip below.
    if inside
        && double
        && app.session.running.is_none()
        && let Some(hp) = hover
        && let Some(h) = hatch_at(app, xf.to_world(hp), scale)
    {
        app.session.set_selection(vec![h]);
        crate::hatchdlg::open_edit(app);
        hatch_dialog = true;
    }

    Some(hatch_dialog)
}

fn update_canvas_cursor(
    app: &mut CadApp,
    ui: &egui::Ui,
    xf: &Xf,
    inside: bool,
    hover: Option<Pos2>,
) -> (Option<Vec2>, Option<crate::dyninput::Frame>) {
    let raw_world = hover.filter(|_| inside).map(|p| xf.to_world(p));
    let dyn_frame = crate::cmdline::dyn_frame(app);
    if let Some(w) = raw_world {
        let mut eff = effective_point(app, w, xf, ui.ctx());
        // A locked Dynamic Input value (`40,` or `40<` typed) holds the cursor and overrides snaps.
        if let Some(f) = dyn_frame
            && let Some(e) = crate::dyninput::parse(&app.cmd.buffer, &f)
            && e.first_locked().is_some()
        {
            eff = crate::dyninput::constrain(&e, &f, eff, app.cmd.second_locked());
            app.canvas.snap = None;
            app.canvas.track = None;
        }
        // Grip dimensional input: typed lengths and angles hold the grip and override snaps.
        let grip_held = crate::grip_dyn::constrain(app, eff);
        if let Some(q) = grip_held {
            eff = q;
            app.canvas.snap = None;
            app.canvas.track = None;
        }
        app.canvas.cursor = Some(eff);
        app.session.cursor = eff;
        app.session.cursor_deferred = app.canvas.snap.and_then(|h| h.deferred);
        // Typed or menu snap overrides (END, NON…) and Tab cycling; a locked Dynamic Input value wins.
        if dyn_frame.and_then(|f| crate::dyninput::parse(&app.cmd.buffer, &f)).is_none_or(|e| e.first_locked().is_none()) {
            override_cursor(app, w, xf);
        }
        // Typed grip dimensions win over snap overrides too.
        if let Some(q) = grip_held {
            app.canvas.cursor = Some(q);
            app.session.cursor = q;
            app.canvas.snap = None;
            app.session.cursor_deferred = None;
        }
    } else {
        app.canvas.cursor = None;
        app.canvas.snap = None;
        app.session.cursor_deferred = None;
    }

    (raw_world, dyn_frame)
}

fn update_hover(app: &mut CadApp, ui: &egui::Ui, inside: bool, hover_pos: Option<Pos2>, raw_world: Option<Vec2>, scale: f64) {
    // The cursor picks objects (pick box, rollover highlight): "Select objects" and single-object
    // prompts, or no command with PICKFIRST on; SELECTIONPREVIEW says in which of those it shows.
    let selecting = app.session.previews_selection();
    if inside && selecting && app.session.pending_window.is_none() {
        if hover_pos != app.canvas.hover_at
            && let (Some(w), Ok(st)) = (raw_world, app.session.state())
        {
            let filter = app.session.settings.previewfilter;
            app.canvas.hover = cadcraft_engine::select::pick(&st.doc, &st.edit_space(), w, app.session.settings.pickbox.max(1.0) * 1.5 / scale)
                .filter(|h| st.doc.entity(*h).is_none_or(|e| !cadcraft_engine::select::preview_filtered(&st.doc, e, filter)));
            app.canvas.hover_at = hover_pos;
        }
    } else {
        app.canvas.hover = None;
        app.canvas.hover_at = None;
    }
    // The open selection cycling list highlights the object of the row under the pointer.
    if app.canvas.cycle.is_some() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.canvas.cycle = None;
    }
    if let Some(c) = &app.canvas.cycle {
        app.canvas.hover = hover_pos.and_then(|hp| c.row_at(hp));
        app.canvas.hover_at = None;
    }
    shift_space_cycle(app, ui, hover_pos.filter(|_| inside), raw_world);
    if inside && hover_pos != app.canvas.cycle_badge_at {
        app.canvas.cycle_badge = raw_world.is_some_and(|w| app.session.cycling_badge(w));
        app.canvas.cycle_badge_at = hover_pos;
    } else if !inside {
        app.canvas.cycle_badge = false;
        app.canvas.cycle_badge_at = None;
    }
}

struct PromptClickContext<'a> {
    rect: Rect,
    xf: Xf,
    inside: bool,
    middle_down: bool,
    primary_clicked: bool,
    response: &'a egui::Response,
}

fn collect_prompt_clicks(app: &mut CadApp, ui: &egui::Ui, context: PromptClickContext<'_>) -> bool {
    // egui drops a click whose button comes up a few pixels from where it went down (a drag to it),
    // and eframe hands a slow frame's clicks over together, which egui reports as one.
    let point_prompt = app.session.current_prompt().is_some_and(|p| cadcraft_engine::pointmod::accepts(&p))
        && app.session.pending_window.is_none()
        && !app.session.realtime_zoom_active();
    let pressed_primary = context.primary_clicked || (point_prompt && context.response.drag_stopped_by(egui::PointerButton::Primary));
    if point_prompt && context.inside && pressed_primary && !context.middle_down && app.cmd.options.is_none() && app.cmd.buffer.is_empty() {
        let releases: Vec<egui::Pos2> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, .. } => Some(*pos),
                    _ => None,
                })
                .collect()
        });
        // The last one is placed below, at the cursor, like any click.
        if let Some((_, earlier)) = releases.split_last() {
            for pos in earlier.iter().filter(|p| context.rect.contains(**p)) {
                let w = effective_point(app, context.xf.to_world(*pos), &context.xf, ui.ctx());
                let result = match app.canvas.snap {
                    Some(hit) => app.session.input_snapped(hit),
                    None => app.session.input(Input::Point(w)),
                };
                if let Err(e) = result {
                    app.session.echo(e.to_string());
                }
            }
        }
    }

    pressed_primary
}

/// Draw the drawing area and handle its input (zoom and pan, picking, the crosshair) for this
/// The active layout viewport's contents (paper space): its basemap tiles
/// first (fork, §9 — under its vector contents, clipped to the viewport, with
/// the viewport's own model-to-screen transform; a clipped viewport's outline
/// mask covers vectors only, so it gets no basemap), then the vectors
/// themselves. Returns `masked`: the outline is in the stencil buffer this
/// frame. Split out of [`show`] for the cognitive-complexity gate.
fn draw_active_viewport(app: &mut CadApp, xf_paper: &Xf, painter: &egui::Painter, rect: Rect, ctx: &egui::Context, bg: Rgb) -> bool {
    let Some(f) = ensure_vp_content(app, xf_paper, ctx) else { return false };
    if f.outline.is_none()
        && let Ok(doc) = app.session.doc()
    {
        let vp_painter = painter.with_clip_rect(f.on_screen.intersect(rect));
        let xf_vp = Xf { rect: f.on_screen, center: f.xf.to_world(f.on_screen.center()), scale: f.xf.scale, twist: f.xf.twist };
        crate::tiles::draw_tilemap(&mut app.canvas.tilemap, doc, &vp_painter, &xf_vp, ctx);
    }
    let gpu = app.canvas.gpu.is_some();
    let clipped = painter.with_clip_rect(f.on_screen.intersect(rect));
    draw_vp_content(&mut app.canvas.vp, gpu, &clipped, &f, xf_paper, bg, app.session.settings.lwdisplay);
    gpu && f.outline.is_some()
}

/// Basemap tiles under the vectors (fork, §9): model space through the canvas
/// view; paper space inside each layout viewport (each with its own view),
/// never on the sheet itself. Missing tiles stay blank until the background
/// fetcher lands them (§9.4). Split out of [`show`] for the
/// cognitive-complexity gate.
fn draw_basemap(app: &mut CadApp, painter: &egui::Painter, xf_paper: &Xf, rect: Rect, ctx: &egui::Context) {
    if let Ok(doc) = app.session.doc() {
        let sheet = app.canvas.list.as_ref().and_then(|l| l.sheet);
        if sheet.is_none() {
            crate::tiles::draw_tilemap(&mut app.canvas.tilemap, doc, painter, xf_paper, ctx);
        } else {
            draw_layout_viewport_tiles(&app.session, &mut app.canvas.tilemap, doc, painter, xf_paper, rect, ctx);
        }
    }
}

/// frame.
///
/// It also owns the cursor-visibility contract: the OS cursor is hidden only while the pointer is
/// over the drawing area *and* the canvas is the topmost layer under it, so the drawn crosshair
/// replaces the pointer over the drawing alone. A menu bar, an open menu (including its frame, not
/// just its items), a tooltip or a floating palette above the canvas keeps the normal OS cursor
/// visible (#1701), and so do the menus painted on the canvas itself (see [`over_canvas_menu`]).
pub fn show(app: &mut CadApp, ui: &mut egui::Ui) {
    let t = Tokens::get();
    let full = ui.available_rect_before_wrap();
    // Tiled model viewports (VPORTS): the current tile is the drawing area below; `draw_tiles`
    // draws the others.
    let tiles = tile_rects(app, full);
    let rect = tiles.iter().find(|(_, tile)| tile.active).map_or(full, |(r, _)| *r);
    let resp = ui.interact(full, ui.scope_id().with("canvas"), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    app.canvas.rect = Some(rect);
    app.session.viewport_px = (f64::from(rect.width()).max(1.0), f64::from(rect.height()).max(1.0));
    if app.canvas.zoom_pending {
        app.canvas.zoom_pending = false;
        let _ = app.session.zoom_extents();
    }
    let Ok(st) = app.session.state() else { return };
    let view = st.view();
    let scale = f64::from(rect.height()) / view.height.max(1e-12);
    let xf = Xf { rect, center: view.center, scale, twist: view.twist };
    app.canvas.xf = Some(xf);

    // ---------- input ----------
    let pointer = ui.input(|i| PointerInput {
        hover_pos: i.pointer.hover_pos(),
        scroll: i.smooth_scroll_delta,
        mods: i.modifiers,
        middle_down: i.pointer.middle_down(),
        pressed_primary: i.pointer.primary_clicked(),
        pressed_secondary: i.pointer.secondary_clicked(),
        dbl_middle: i.pointer.button_double_clicked(egui::PointerButton::Middle),
        dbl_primary: i.pointer.button_double_clicked(egui::PointerButton::Primary),
    });
    let navigation = Navigation { rect, tiles: &tiles, response: &resp, xf, pointer };
    let Some((inside, on_menu)) = navigate_canvas(app, ui, &navigation) else { return };
    let PointerInput { hover_pos, mods, middle_down, pressed_primary, pressed_secondary, dbl_primary, .. } = pointer;
    // Re-read the view after navigation.
    let view = app.session.state().map(|s| s.view()).unwrap_or(view);
    let scale = f64::from(rect.height()) / view.height.max(1e-12);
    let xf = Xf { rect, center: view.center, scale, twist: view.twist };
    app.canvas.xf = Some(xf);
    // The paper (display) transform; equal to `xf` except inside an MSPACE viewport.
    let (xf_paper, active_vp) = match app.session.state() {
        Ok(st) => {
            let pv = st.paper_view();
            // Paper space is never twisted.
            let xp = Xf { rect, center: pv.center, scale: f64::from(rect.height()) / pv.height.max(1e-12), twist: 0.0 };
            (if st.mspace.is_some() { xp } else { xf }, st.active_viewport().map(|(_, v)| v))
        }
        Err(_) => (xf, None),
    };
    // A clipped active viewport's outline (picks and its heavy border follow it).
    let active_clip = app.session.state().ok().and_then(|st| st.active_viewport().and_then(|(h, v)| cadcraft_render::viewport_clip(&st.doc, h, &v)));
    let Some(hatch_dialog) = handle_double_click(
        app,
        DoubleClickContext {
            inside,
            double: dbl_primary,
            hover: hover_pos,
            xf,
            xf_paper,
            scale,
            active_viewport: &active_vp,
            active_clip: &active_clip,
        },
    ) else {
        return;
    };

    let pressed_primary =
        collect_prompt_clicks(app, ui, PromptClickContext { rect, xf, inside, middle_down, primary_clicked: pressed_primary, response: &resp });

    let drag_path_done = update_drag_trim(app, ui, &resp, &xf, inside, middle_down, hover_pos);
    let (raw_world, dyn_frame) = update_canvas_cursor(app, ui, &xf, inside, hover_pos);

    // A click on the Dynamic Input options menu picks the keyword; elsewhere it closes the menu.
    let mut picked = false;
    if pressed_primary && app.cmd.options.is_some() {
        match hover_pos.and_then(|h| app.cmd.option_at(h)) {
            Some(k) => {
                crate::cmdline::pick_option(app, k);
                picked = true;
            }
            None => app.cmd.options = None,
        }
    }
    // A double-click with no command running (on a layout viewport, say) leaves no selection
    // window behind; while a command asks for points, every click still counts (#815).
    let idle_dbl = dbl_primary && app.session.running.is_none();
    if idle_dbl {
        app.session.pending_window = None;
        app.canvas.drag_window = false;
    }
    let pressed_primary = pressed_primary && !picked && !hatch_dialog && !idle_dbl && drag_path_done.is_none();
    let key: CycleKey = (
        app.session.running.as_ref().map(|r| r.id.clone()),
        app.session.current_prompt().map(|p| p.message),
        app.session.state().map_or(0, |st| st.uid),
        app.session.state().map_or(0, |st| st.revision),
        xf.center,
        xf.scale,
    );
    if app.canvas.cycle_key.as_ref() != Some(&key) {
        app.canvas.cycle = None;
        app.canvas.hover_cycle = None;
        app.canvas.cycle_key = Some(key);
    }
    let cycled = pressed_primary && !middle_down && cycle_click(app, &xf, hover_pos, raw_world.filter(|_| inside), mods.shift);
    // Clicks.
    if inside
        && pressed_primary
        && !middle_down
        && !cycled
        && let Some(p) = app.canvas.cursor
    {
        if app.session.prompting() {
            // Ctrl held while picking the point of a grip edit makes copies (the Copy option).
            if mods.ctrl && app.session.running.as_ref().is_some_and(|r| r.id == "grip") {
                let _ = app.session.input(Input::Keyword("Copy".into()));
            }
            // Clicking places the point; a value typed into the Dynamic Input boxes is used up.
            if let Some(f) = dyn_frame
                && crate::dyninput::parse(&app.cmd.buffer, &f).is_some()
            {
                app.cmd.buffer.clear();
            }
            // Preserve a snap's viewport origin for DIST, and its deferred curve where needed.
            // Shift held flips the pick for commands that advertise it (TRIM/EXTEND);
            // the point itself is the snap-adjusted cursor either way.
            let result = if mods.shift {
                app.session.input(Input::PointShift(p))
            } else {
                match app.canvas.snap {
                    Some(hit) => app.session.input_snapped(hit),
                    None => app.session.input(Input::Point(p)),
                }
            };
            if let Err(e) = result {
                app.session.echo(e.to_string());
            }
        } else if let Some(g) = crate::dyngrips::at(app, &xf, hover_pos) {
            crate::dyngrips::click(app, g);
        } else if let Some(g) = grip_at(app, &xf, hover_pos) {
            click_grip(app, g, mods.shift);
        } else if let Err(e) = app.session.idle_click(raw_world.unwrap_or(p), mods.shift) {
            app.session.echo(e.to_string());
        }
    }
    // Press-and-drag selection window (PICKDRAG = 2; click-click still works): a drag
    // that starts while objects are being selected opens a window where the button went down,
    // and letting go closes it. Drags on grips and drags while drawing are left alone.
    if !app.session.settings.pickdrag {
        app.canvas.drag_window = false;
    }
    if app.session.settings.pickdrag
        && resp.drag_started_by(egui::PointerButton::Primary)
        && !middle_down
        && app.canvas.drag_trim.is_none()
        && app.canvas.cycle.is_none()
        && app.session.pending_window.is_none()
        && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        && rect.contains(origin)
        && grip_at(app, &xf, Some(origin)).is_none()
        && crate::dyngrips::at(app, &xf, Some(origin)).is_none()
    {
        app.canvas.drag_window = app.session.begin_window(xf.to_world(origin));
    }
    if app.canvas.drag_window && resp.drag_stopped_by(egui::PointerButton::Primary) {
        app.canvas.drag_window = false;
        if app.session.pending_window.is_some()
            && let Some(end) = ui.input(|i| i.pointer.interact_pos())
        {
            let end = xf.to_world(end);
            let done = if app.session.prompting() { app.session.input(Input::Point(end)) } else { app.session.idle_click(end, mods.shift) };
            if let Err(e) = done {
                app.session.echo(e.to_string());
            }
        }
    }
    // Right-click acts as Enter while a command runs (the classic CAD default; held, it opens the
    // command menu); otherwise it opens the shortcut menu (Repeat, Clipboard, …).
    // Shift/Ctrl+right-click opens the object snap menu.
    let idle = !app.session.prompting();
    if inside && pressed_secondary {
        app.canvas.snap_menu = mods.shift || mods.ctrl;
    }
    if crate::context_menu::in_command(app, ui, inside, pressed_secondary) {
        let _ = app.session.input(Input::Enter);
    }
    // Right-click on a grip with options (polyline vertex, line end…) opens the grip menu.
    if inside && pressed_secondary && idle && !app.canvas.snap_menu {
        app.canvas.grip_menu = grip_at(app, &xf, hover_pos).filter(|g| !grip_options(app, *g).is_empty());
    }
    crate::dyngrips::menu(app, ui.ctx(), &xf);
    if idle && !app.canvas.snap_menu && app.canvas.grip_menu.is_some() {
        grip_menu(app, &resp);
    } else if idle || app.canvas.snap_menu {
        crate::context_menu::show(app, &resp);
    }

    update_hover(app, ui, inside, hover_pos, raw_world, scale);

    paint_canvas(CanvasPaint {
        app,
        ui,
        painter: &painter,
        tokens: t,
        full,
        tiles: &tiles,
        rect,
        xf,
        xf_paper,
        view,
        active_vp,
        active_clip,
        inside,
        on_menu,
        hover_pos,
        raw_world,
    });
}

fn paint_canvas(frame: CanvasPaint<'_>) {
    let CanvasPaint {
        app,
        ui,
        painter,
        tokens: t,
        full,
        tiles,
        rect,
        xf,
        xf_paper,
        view,
        active_vp,
        active_clip,
        inside,
        on_menu,
        hover_pos,
        raw_world,
    } = frame;
    let scale = xf.scale;
    let selecting = app.session.previews_selection();
    painter.rect_filled(rect, 0.0, t.canvas);
    let t0 = crate::now_ms();
    let mut views = vec![(xf_paper.world_bounds(), 1.0 / xf_paper.scale)];
    views.extend(tiles.iter().filter(|(r, tile)| !tile.active && r.width() >= 1.0 && r.height() >= 1.0).map(|(r, tile)| {
        let xf = tile_xf(*r, tile);
        (xf.world_bounds(), 1.0 / xf.scale)
    }));
    ensure_list(app, &xf_paper, &views, ui.ctx());
    let sheet = app.canvas.list.as_ref().and_then(|l| l.sheet);
    let surround = Color32::from_rgb(0x50, 0x57, 0x63);
    let (bg, paper_rect) = match sheet {
        Some(sh) => {
            // Paper space: grey surround, the sheet with a shadow, the printable area dashed.
            painter.rect_filled(rect, 0.0, surround);
            let a = xf_paper.to_screen(Vec2::new(0.0, sh.size.y));
            let b = xf_paper.to_screen(Vec2::new(sh.size.x, 0.0));
            let paper = Rect::from_two_pos(a, b);
            painter.rect_filled(paper.translate(vec2(5.0, 5.0)), 0.0, Color32::from_black_alpha(110));
            painter.rect_filled(paper, 0.0, t.paper);
            let pa = Rect::from_two_pos(
                xf_paper.to_screen(Vec2::new(sh.printable.min.x, sh.printable.max.y)),
                xf_paper.to_screen(Vec2::new(sh.printable.max.x, sh.printable.min.y)),
            );
            painter.extend(dashed_rect(painter.clip_rect(), pa, Stroke::new(1.0, Color32::from_gray(150)), 4.0, 4.0));
            (Rgb(t.paper.r(), t.paper.g(), t.paper.b()), Some(paper))
        }
        None => {
            blockedit::wash(app, painter, rect);
            draw_grid(app, painter, &xf);
            (Rgb(t.canvas.r(), t.canvas.g(), t.canvas.b()), None)
        }
    };
    let sel = app.session.selection();
    // How the object under the cursor and the selected ones are highlighted (PREVIEWEFFECT,
    // HIGHLIGHT, SELECTIONEFFECT); with HIGHLIGHT off only their grips show.
    let (preview_look, selection_look) = (Look::preview(&app.session.settings), Look::selection(&app.session.settings));
    paint_content_highlights(
        &mut CanvasPaint {
            app,
            ui,
            painter,
            tokens: t,
            full,
            tiles,
            rect,
            xf,
            xf_paper,
            view,
            active_vp: active_vp.clone(),
            active_clip: active_clip.clone(),
            inside,
            on_menu,
            hover_pos,
            raw_world,
        },
        bg,
        &sel,
        preview_look,
        selection_look,
    );
    app.canvas.draw_ms = crate::now_ms() - t0;
    compare::paint(app, ui.ctx(), painter, &xf_paper, rect);
    blockedit::paint(app, ui.ctx(), painter, &xf_paper, rect);
    // Constraint bars and dynamic dimensional constraints (model space, or inside a viewport).
    if sheet.is_none() || active_vp.is_some() {
        let clip = match &active_vp {
            Some(vp) => Rect::from_two_pos(
                xf_paper.to_screen(Vec2::new(vp.center.x - vp.width / 2.0, vp.center.y + vp.height / 2.0)),
                xf_paper.to_screen(Vec2::new(vp.center.x + vp.width / 2.0, vp.center.y - vp.height / 2.0)),
            )
            .intersect(rect),
            None => rect,
        };
        let p = painter.with_clip_rect(clip);
        crate::parametric::draw_overlay(app, ui, &p, &xf);
    }
    // Grips on selected objects (when idle or editing grips).
    let grip_edit = app.session.running.as_ref().is_some_and(|r| r.id == "grip");
    if (!grip_edit && (app.canvas.grip_editing || app.session.prompting())) || sel.is_empty() {
        app.canvas.hot_grips.clear();
    }
    if !grip_edit {
        app.canvas.dyn_hot = None;
    }
    app.canvas.grip_editing = grip_edit;
    if (!app.session.prompting() || grip_edit)
        && app.session.shows_grips(sel.len())
        && let Ok(st) = app.session.state()
    {
        let gs = app.session.settings.gripsize as f32;
        for h in sel.iter().take(200) {
            if let Some(e) = st.doc.entity(*h) {
                let grips = e.kind.grips();
                let count = grips.len();
                for (gi, g) in grips.into_iter().enumerate() {
                    let sp = xf.to_screen(g);
                    if rect.contains(sp) {
                        let r = grip_rect(&e.kind, count, gi, sp, gs);
                        let hot = app.canvas.hot_grips.contains(&(*h, gi));
                        let warm = !hot && hover_pos.is_some_and(|hp| r.expand(2.0).contains(hp));
                        painter.rect_filled(
                            r,
                            0.0,
                            if hot {
                                t.grip_hot
                            } else if warm {
                                Color32::from_rgb(0xff, 0x7f, 0x9f)
                            } else {
                                t.grip
                            },
                        );
                        painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_rgb(0x10, 0x30, 0x80)), egui::StrokeKind::Middle);
                    }
                }
            }
        }
        crate::dyngrips::draw(app, painter, &xf, rect, hover_pos);
    }
    // Rubber band preview of the active command.
    // `xf` is the space being edited: paper space, or model space inside the active viewport.
    let drag = match (app.canvas.cursor, app.canvas.gpu.is_some()) {
        (Some(c), true) => app.session.drag(c),
        _ => None,
    };
    if let Some((drag, extra)) = drag {
        if let Ok(st) = app.session.state() {
            let serial = app.session.clipboard_source.as_ref().map_or(0, |c| c.serial);
            let transparency = app.session.settings.transparency_display;
            draw_drag_gpu(&mut app.canvas, painter, &xf, &st.doc, &drag, (&app.session.clipboard, serial), bg, transparency);
            let list = rebuilt_preview(&st.doc, extra.iter(), scale);
            draw_list(painter, &xf, &list, bg, false);
        }
    } else if let (Some(c), true) = (app.canvas.cursor, app.session.prompting()) {
        let ents = app.session.preview(c);
        if !ents.is_empty()
            && let Ok(st) = app.session.state()
        {
            let list = rebuilt_preview(&st.doc, ents.iter(), scale);
            draw_list(painter, &xf, &list, bg, false);
        }
    }
    // Pending selection window.
    if let (Some(pw), Some(c)) = (app.session.pending_window, raw_world) {
        // The world box it selects (turned in a twisted view).
        let corners = [pw.corner, Vec2::new(c.x, pw.corner.y), c, Vec2::new(pw.corner.x, c.y)].map(|q| xf.to_screen(q));
        let crossing = app.session.window_is_crossing(pw.corner, c);
        if let Some(fill) = selection_area_fill(&app.session.settings, crossing) {
            painter.add(Shape::convex_polygon(corners.to_vec(), fill, Stroke::NONE));
        }
        let st = Stroke::new(1.0, if crossing { t.crossing_stroke } else { t.window_stroke });
        if crossing {
            painter.extend(dashed_polygon(painter.clip_rect(), &corners, st, 5.0, 3.0));
        } else {
            let mut pts = corners.to_vec();
            pts.push(corners[0]);
            painter.add(Shape::line(pts, st));
        }
    }
    // The freehand trim path being dragged.
    if let Some(path) = &app.canvas.drag_trim
        && path.len() > 1
    {
        let pts: Vec<Pos2> = path.iter().map(|p| xf.to_screen(*p)).collect();
        painter.add(Shape::line(pts, Stroke::new(1.5, t.accent)));
    }
    paint_pointer_overlay(
        &mut CanvasPaint {
            app,
            ui,
            painter,
            tokens: t,
            full,
            tiles,
            rect,
            xf,
            xf_paper,
            view,
            active_vp: active_vp.clone(),
            active_clip: active_clip.clone(),
            inside,
            on_menu,
            hover_pos,
            raw_world,
        },
        paper_rect,
        surround,
        selecting,
        sheet,
    );
}

/// Start TEXTEDIT on the text, mtext or attribute definition at `p` (the objects its shortcut menu
/// offers Edit Text for); false when there is none. A dimension at `p` opens its Properties
/// panel instead (a dimension has no in-place editor; TEXTEDIT's command-line prompt would
/// still take its text, but the panel is where its style and overrides live).
///
/// A double-click that lands on text the user cannot edit in place still answers: text inside
/// a block reference and text on a locked layer name the blocker on the command line instead
/// of silently doing nothing.
///
/// When no object claims the click at all, the text search below retries with its box
/// expanded: glyphs draw past the approximate pick box, so a double-click on the first or
/// last character of a label at a zoomed-in view would otherwise miss the text and do nothing.
fn edit_text_at(app: &mut CadApp, p: Vec2) -> bool {
    use cadcraft_engine::doc::EntityKind;
    if let Some(h) = cadcraft_engine::cmd::curves::pick_at(&app.session, p) {
        let Some(e) = app.session.doc().ok().and_then(|d| d.entity(h).cloned()) else {
            return false;
        };
        match &e.kind {
            EntityKind::Text(_) | EntityKind::MText(_) | EntityKind::AttDef(_) => {
                if locked_hint(app, h, &e) {
                    return true;
                }
                app.start("textedit");
                if let Err(e) = app.session.input(Input::Point(p)) {
                    app.session.echo(e.to_string());
                }
                true
            }
            EntityKind::Dimension(_) => {
                if locked_hint(app, h, &e) {
                    return true;
                }
                // A dimension has no in-place editor: select it and open its Properties
                // panel, as AutoCAD's double-click does — never silently nothing.
                app.session.set_selection(vec![h]);
                let _ = crate::docking::command(app, false, &serde_json::json!({"operation": "open", "panel": "properties"}));
                app.session.echo(crate::i18n::t("Opened the Properties panel for the dimension. Edit its text override and style there."));
                true
            }
            EntityKind::Insert(_) => {
                if nested_text_at(app, h, p) {
                    app.session.echo(crate::i18n::t("Text inside a block cannot be edited here. Enter the block editor to edit it."));
                    return true;
                }
                false
            }
            _ => false,
        }
    } else {
        // Nothing claims the click: it may sit on glyphs past a text's pick box.
        if let Some((h, centre)) = expanded_text_at(app, p) {
            let Some(e) = app.session.doc().ok().and_then(|d| d.entity(h).cloned()) else {
                return false;
            };
            if locked_hint(app, h, &e) {
                return true;
            }
            app.start("textedit");
            // The click itself is outside the box TEXTEDIT picks with: feed the box centre,
            // which is inside it, so the already-found text is the one that opens.
            if let Err(e) = app.session.input(Input::Point(centre)) {
                app.session.echo(e.to_string());
            }
            return true;
        }
        if nested_text_anywhere(app, p) {
            app.session.echo(crate::i18n::t("Text inside a block cannot be edited here. Enter the block editor to edit it."));
            return true;
        }
        false
    }
}

/// Echo the blocker when the text `e` (handle `h`) cannot be edited: its layer is locked, or
/// a reference edit in progress excludes it. True when blocked (the click is answered).
fn locked_hint(app: &mut CadApp, h: Handle, e: &cadcraft_doc::Entity) -> bool {
    if app.session.doc().ok().is_some_and(|d| d.layer(&e.common.layer).is_some_and(|l| l.locked)) {
        app.session.echo(crate::i18n::t("Text on a locked layer cannot be edited. Unlock the layer first."));
        true
    } else if cadcraft_engine::cmd::curves::is_locked(&app.session, h) {
        app.session.echo(crate::i18n::t("The object is outside the reference-edit working set and cannot be edited."));
        true
    } else {
        false
    }
}

/// How far past a text's approximate pick box its glyphs may draw, in text heights:
/// the pick box (`entity_bounds`) now measures CJK at 1.0 em/character, but an
/// installed fallback font's advance can still exceed it a little, and narrow-glyph
/// strings are overestimated the other way — so containment is always confirmed
/// against the placed bounds below, never against the pick box alone.
/// The box the text actually draws in (the renderer's placed bounds), for a text-like
/// entity. The placed bounds are exact; an AttDef draws its tag.
fn placed_text_box(d: &cadcraft_doc::Drawing, e: &cadcraft_doc::Entity) -> Option<Bounds2> {
    use cadcraft_doc::EntityKind;
    use cadcraft_engine::render::{layout_mtext_entity, place_text_entity};
    match &e.kind {
        EntityKind::Text(t) => Some(place_text_entity(d, t, &t.value).1),
        EntityKind::MText(t) => Some(layout_mtext_entity(d, t).bounds),
        EntityKind::AttDef(a) => Some(place_text_entity(d, &a.text, &a.tag).1),
        _ => None,
    }
}

/// How far to widen the pick box as a cheap pre-filter before confirming against the
/// placed bounds: real glyphs are at most ~1.5 em wide per character, so the placed box
/// can stick out by (1.5 - 0.93) em/character beyond the pick box. Only a superset test;
/// containment is always confirmed against [`placed_text_box`]. Kept as the fallback
/// even though the pick box now measures CJK at 1.0 em: an installed fallback font's
/// advance is not pinned to 1.0 em exactly.
fn prefilter_expand(e: &cadcraft_doc::Entity, ap: f64) -> f64 {
    use cadcraft_doc::EntityKind;
    match &e.kind {
        EntityKind::Text(t) => ap.max(0.6 * t.value.chars().count() as f64 * t.height * t.width_factor),
        EntityKind::MText(t) => ap.max(0.5 * t.height),
        EntityKind::AttDef(a) => ap.max(0.6 * a.tag.chars().count() as f64 * a.text.height * a.text.width_factor),
        _ => ap,
    }
}

/// The topmost text, mtext or attribute definition whose placed box (expanded by the
/// pick aperture) contains `p`, with the point TEXTEDIT should be fed (the placed-box
/// centre: the click itself may sit outside the pick box).
fn expanded_text_at(app: &CadApp, p: Vec2) -> Option<(Handle, Vec2)> {
    use cadcraft_doc::{EntityKind, entity_bounds};
    let d = app.session.doc().ok()?;
    let store = d.space(&app.session.space())?;
    let ap = cadcraft_engine::cmd::curves::pick_aperture(&app.session);
    let mut found = None;
    for e in store.iter() {
        if !matches!(e.kind, EntityKind::Text(_) | EntityKind::MText(_) | EntityKind::AttDef(_)) {
            continue;
        }
        if !d.is_visible(e) {
            continue;
        }
        let b = entity_bounds(d, e, 0);
        if b.is_empty() || !b.expand(prefilter_expand(e, ap)).contains(p) {
            continue;
        }
        let Some(pb) = placed_text_box(d, e) else { continue };
        if !pb.is_empty() && pb.expand(ap).contains(p) {
            found = Some((e.handle, pb.center()));
        }
    }
    found
}

/// Whether `p` sits on text inside the block reference `h`: a definition's TEXT/MTEXT, or one
/// of the reference's own attribute values. Picking returns the INSERT for all of those, so a
/// double-click on them would otherwise do nothing at all. Containment is tested against
/// the placed bounds, as in [`expanded_text_at`].
fn nested_text_at(app: &CadApp, h: Handle, p: Vec2) -> bool {
    use cadcraft_doc::EntityKind;
    let Ok(d) = app.session.doc() else { return false };
    let Some(e) = d.entity(h) else { return false };
    let EntityKind::Insert(ins) = &e.kind else { return false };
    let ap = cadcraft_engine::cmd::curves::pick_aperture(&app.session);
    if attrib_text_at(d, &ins.attribs, p, ap) {
        return true;
    }
    let Some(blk) = d.block(&ins.block) else { return false };
    let mut budget = 500_000usize;
    for off in insert_copies(ins) {
        let m = ins.transform(blk.base.xy()).then(off);
        let mut boxes = Vec::new();
        nested_text_boxes(d, &ins.block, m, 0, &mut budget, &mut boxes);
        for b in boxes {
            if b.expand(ap).contains(p) {
                return true;
            }
        }
    }
    false
}

/// Whether `p` sits on text inside any block reference in the current space, when picking
/// claimed no object at all (a click on a text-only block's glyphs is outside the reference's
/// own geometry). Block definitions are walked once each and their placed boxes reused for
/// every reference to them.
fn nested_text_anywhere(app: &CadApp, p: Vec2) -> bool {
    use cadcraft_doc::EntityKind;
    let Ok(d) = app.session.doc() else { return false };
    let Some(store) = d.space(&app.session.space()) else { return false };
    let ap = cadcraft_engine::cmd::curves::pick_aperture(&app.session);
    let mut memo: std::collections::HashMap<String, Vec<Bounds2>> = std::collections::HashMap::new();
    let mut budget = 500_000usize;
    for e in store.iter() {
        let EntityKind::Insert(ins) = &e.kind else { continue };
        if !d.is_visible(e) {
            continue;
        }
        if attrib_text_at(d, &ins.attribs, p, ap) {
            return true;
        }
        let boxes = memo.entry(ins.block.clone()).or_insert_with(|| {
            let mut v = Vec::new();
            nested_text_boxes(d, &ins.block, cadcraft_geom::Mat3::IDENTITY, 0, &mut budget, &mut v);
            v
        });
        let Some(blk) = d.block(&ins.block) else { continue };
        for off in insert_copies(ins) {
            let m = ins.transform(blk.base.xy()).then(off);
            for b in boxes.iter() {
                let wb = Bounds2::from_points(b.corners().map(|c| m.apply(c)));
                if wb.expand(ap).contains(p) {
                    return true;
                }
            }
        }
    }
    false
}

/// Whether `p` sits on one of the reference's attribute values (already in world
/// coordinates), tested against the placed bounds as in [`expanded_text_at`].
fn attrib_text_at(d: &cadcraft_doc::Drawing, attribs: &[cadcraft_doc::Attrib], p: Vec2, ap: f64) -> bool {
    use cadcraft_engine::render::{layout_mtext_entity, place_text_entity};
    for a in attribs {
        if a.invisible {
            continue;
        }
        let b = match a.multiline_text() {
            Some(m) => layout_mtext_entity(d, &m).bounds,
            None => place_text_entity(d, &a.text, &a.text.value).1,
        };
        if !b.is_empty() && b.expand(ap).contains(p) {
            return true;
        }
    }
    false
}

/// The per-copy offsets of an arrayed reference (one translation each, in its rotated frame),
/// capped so a hostile array cannot multiply the walk.
fn insert_copies(ins: &cadcraft_doc::Insert) -> Vec<cadcraft_geom::Mat3> {
    let cols = ins.cols.max(1) as usize;
    let rows = ins.rows.max(1) as usize;
    let total = cols.saturating_mul(rows).min(4096);
    (0..total)
        .map(|i| {
            let (c, r) = (i % cols, i / cols);
            cadcraft_geom::Mat3::translate(Vec2::new(ins.col_spacing * c as f64, ins.row_spacing * r as f64).rotate(ins.rotation))
        })
        .collect()
}

/// The placed box of every TEXT/MTEXT inside block `name`, mapped by `m` (block-to-frame).
/// AttDefs are skipped: they do not draw in a reference (its attribute values do, handled by
/// [`attrib_text_at`]). The walk is bounded by [`cadcraft_doc::MAX_BLOCK_DEPTH`] and the shared
/// `budget` of entity visits, so cyclic or hostile nesting cannot run away.
fn nested_text_boxes(d: &cadcraft_doc::Drawing, name: &str, m: cadcraft_geom::Mat3, depth: usize, budget: &mut usize, out: &mut Vec<Bounds2>) {
    use cadcraft_doc::EntityKind;
    use cadcraft_engine::render::{layout_mtext_entity, place_text_entity};
    if depth > cadcraft_doc::MAX_BLOCK_DEPTH {
        return;
    }
    let Some(blk) = d.block(name) else { return };
    for be in blk.entities.iter() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        if !be.common.visible || matches!(be.kind, EntityKind::AttDef(_)) {
            continue;
        }
        // Contents on a hidden layer do not draw (layer "0" takes the reference's layer).
        if be.common.layer != "0" && d.layer(&be.common.layer).is_some_and(|l| !l.visible()) {
            continue;
        }
        match &be.kind {
            EntityKind::Text(t) => {
                let b = place_text_entity(d, t, &t.value).1;
                if !b.is_empty() {
                    out.push(Bounds2::from_points(b.corners().map(|c| m.apply(c))));
                }
            }
            EntityKind::MText(t) => {
                let b = layout_mtext_entity(d, t).bounds;
                if !b.is_empty() {
                    out.push(Bounds2::from_points(b.corners().map(|c| m.apply(c))));
                }
            }
            EntityKind::Insert(inner) => {
                let Some(iblk) = d.block(&inner.block) else { continue };
                for off in insert_copies(inner) {
                    let im = inner.transform(iblk.base.xy()).then(off).then(m);
                    nested_text_boxes(d, &inner.block, im, depth + 1, budget, out);
                }
            }
            _ => {}
        }
    }
}

/// Basemap tiles inside the paper-space layout viewports other than the
/// active one (the active viewport's tiles are drawn with its contents, just
/// before `draw_vp_content`): each viewport gets its own transform from
/// [`crate::tiles::viewport_tile_xf`] and its own `tiles_for_view`, clipped
/// to its rectangle on screen. Clipped (`clip`) viewports get no basemap —
/// their outline mask covers vectors only. Never on the sheet itself.
fn draw_layout_viewport_tiles(
    session: &cadcraft_engine::Session,
    tilemap: &mut crate::tiles::TileCache,
    doc: &cadcraft_doc::Drawing,
    painter: &egui::Painter,
    xf_paper: &Xf,
    rect: Rect,
    ctx: &egui::Context,
) {
    let Ok(st) = session.state() else { return };
    let space = st.space.clone();
    let active = st.active_viewport().map(|(h, _)| h);
    let Some(store) = doc.space(&space) else { return };
    for e in store.iter() {
        let cadcraft_doc::EntityKind::Viewport(vp) = &e.kind else { continue };
        if !vp.on || vp.clip.is_some() || active == Some(e.handle) {
            continue;
        }
        let Some(xf) = crate::tiles::viewport_tile_xf(xf_paper, vp.center.xy(), vp.height, vp.view_center, vp.view_height) else { continue };
        let r = Rect::from_two_pos(
            xf_paper.to_screen(Vec2::new(vp.center.x - vp.width / 2.0, vp.center.y + vp.height / 2.0)),
            xf_paper.to_screen(Vec2::new(vp.center.x + vp.width / 2.0, vp.center.y - vp.height / 2.0)),
        );
        let clipped = painter.with_clip_rect(r.intersect(rect));
        crate::tiles::draw_tilemap(tilemap, doc, &clipped, &xf, ctx);
    }
}

/// The tiles other than the current one (each with its own view, clipped to it), then the
/// tile borders: thin, and heavy around the current tile.
fn draw_tiles(app: &mut CadApp, ui: &egui::Ui, tiles: &[(Rect, cadcraft_engine::ModelTile)]) {
    let t = Tokens::get();
    let bg = Rgb(t.canvas.r(), t.canvas.g(), t.canvas.b());
    for (r, tile) in tiles.iter().filter(|(r, tile)| !tile.active && r.width() >= 1.0 && r.height() >= 1.0) {
        let p = ui.painter_at(*r);
        let xf = tile_xf(*r, tile);
        p.rect_filled(*r, 0.0, t.canvas);
        draw_grid(app, &p, &xf);
        // This tile's own basemap, under its vectors: each split viewport
        // draws its own `tiles_for_view`, never the active tile's.
        if let Ok(doc) = app.session.doc() {
            crate::tiles::draw_tilemap(&mut app.canvas.tilemap, doc, &p, &xf, ui.ctx());
        }
        if let Some(list) = &app.canvas.list {
            draw_list(&p, &xf, list, bg, app.session.settings.lwdisplay);
        }
        viewport_label(&p, *r);
    }
    let p = ui.painter();
    for (r, tile) in tiles {
        let (w, c) = if tile.active { (2.0, t.accent) } else { (1.0, t.border) };
        p.rect_stroke(*r, 0.0, Stroke::new(w, c), egui::StrokeKind::Outside);
    }
}

/// Append `p` to a freehand drag path when it moved at least `min_step` world units from
/// the last point: a slow drag must not flood the fence with near-duplicates.
/// Freehand trim/extend (fork): while TRIM/EXTEND waits for picks, a primary
/// drag lays down a trim path instead of placing points or opening a window;
/// the path is fed whole on release. A press that never really moves stays a
/// click (handled as usual by the caller). Returns the finished path, if any.
/// Split out of [`show`] for the cognitive-complexity gate.
fn update_drag_trim(
    app: &mut CadApp,
    ui: &egui::Ui,
    resp: &egui::Response,
    xf: &Xf,
    inside: bool,
    middle_down: bool,
    hover_pos: Option<Pos2>,
) -> Option<Vec<Vec2>> {
    let scale = xf.scale;
    let drag_trimming = app.session.drag_trim_active() && app.cmd.options.is_none() && app.cmd.buffer.is_empty();
    if drag_trimming
        && inside
        && !middle_down
        && resp.drag_started_by(egui::PointerButton::Primary)
        && let Some(origin) = ui.input(|i| i.pointer.press_origin()).filter(|o| xf.rect.contains(*o))
    {
        app.canvas.drag_trim = Some(vec![xf.to_world(origin)]);
    }
    if let (Some(path), Some(pos)) = (app.canvas.drag_trim.as_mut(), hover_pos) {
        drag_path_push(path, xf.to_world(pos), 2.0 / scale.max(1e-12));
    }
    let mut drag_path_done: Option<Vec<Vec2>> = None;
    if app.canvas.drag_trim.is_some()
        && resp.drag_stopped_by(egui::PointerButton::Primary)
        && let Some(path) = app.canvas.drag_trim.take()
        && drag_path_is_drag(&path, scale, 8.0)
    {
        drag_path_done = Some(path);
    }
    if let Some(path) = &drag_path_done
        && let Err(e) = app.session.input(Input::DragPath(path.clone()))
    {
        app.session.echo(e.to_string());
    }
    drag_path_done
}

fn drag_path_push(path: &mut Vec<Vec2>, p: Vec2, min_step: f64) {
    if path.last().is_none_or(|l| l.dist(p) >= min_step) {
        path.push(p);
    }
}

/// Whether a collected drag path counts as a drag rather than a click: its on-screen
/// length (world length × `scale` pixels per unit) reaches `min_px`.
fn drag_path_is_drag(path: &[Vec2], scale: f64, min_px: f64) -> bool {
    path.windows(2).filter_map(|w| Some(w.first()?.dist(*w.get(1)?))).sum::<f64>() * scale >= min_px
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests;

/// The Block Editor's tint, base point and bar.
mod blockedit;
/// The COMPARE overlay.
mod compare;

/// Background rebuilds of the display list for a new view.
mod images;
mod rebuild;

#[cfg(test)]
mod hatch_pick_tests {
    use cadcraft_engine::Session;
    use cadcraft_geom::Vec2;
    use serde_json::json;

    use crate::{CadApp, Services};

    #[test]
    fn hatch_at_finds_a_hatch_under_the_cursor_only() {
        let mut app = CadApp::new(Session::new(), Services::default());
        app.session.execute("rectang", &json!({ "p1": [0, 0], "p2": [10, 10] })).unwrap();
        let hx = app.session.execute("hatch", &json!({ "points": [[5, 5]], "pattern": "SOLID" })).unwrap()["handle"].as_str().unwrap().to_string();
        app.session.execute("line", &json!({ "points": [[20, 0], [30, 0]] })).unwrap();
        assert_eq!(super::hatch_at(&app, Vec2::new(5.0, 5.0), 10.0).map(|h| h.hex()), Some(hx));
        assert_eq!(super::hatch_at(&app, Vec2::new(25.0, 0.0), 10.0), None, "a line is not a hatch");
        assert_eq!(super::hatch_at(&app, Vec2::new(60.0, 60.0), 10.0), None);
    }

    #[test]
    fn double_clicking_a_dimension_opens_its_properties_panel() {
        let mut app = CadApp::new(Session::new(), Services::default());
        let dh =
            app.session.execute("dimlinear", &json!({ "p1": [0, 0], "p2": [10, 0], "at": [5, -2] })).unwrap()["handle"].as_str().unwrap().to_string();
        let _ = app.session.zoom_extents();
        let ctx = egui::Context::default();
        let screen = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0)));
        let frame = |app: &mut CadApp, events: Vec<egui::Event>, time: f64| {
            app.logic(&ctx);
            let input = egui::RawInput { screen_rect: screen, events, time: Some(time), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| app.ui(ui));
            out.textures_delta.clear();
        };
        frame(&mut app, Vec::new(), 0.0);
        // On the dimension line, mid-span.
        let at = app.canvas.xf.unwrap().to_screen(Vec2::new(5.0, -2.0));
        let button =
            |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::default() };
        frame(&mut app, vec![egui::Event::PointerMoved(at)], 0.1);
        frame(&mut app, vec![button(true), button(false)], 0.2);
        frame(&mut app, vec![button(true), button(false)], 0.3);
        frame(&mut app, Vec::new(), 0.4);
        assert_eq!(app.session.selection().iter().map(|h| h.hex()).collect::<Vec<_>>(), vec![dh]);
        assert!(app.session.log.iter().any(|l| l.contains("属性面板")), "no properties hint in {:?}", app.session.log);
        assert!(app.session.running.is_none(), "double-click starts no command");
    }

    #[test]
    fn double_clicking_a_hatch_opens_the_hatch_edit_dialog() {
        let mut app = CadApp::new(Session::new(), Services::default());
        app.session.execute("rectang", &json!({ "p1": [0, 0], "p2": [10, 10] })).unwrap();
        let hx = app.session.execute("hatch", &json!({ "points": [[5, 5]], "pattern": "SOLID" })).unwrap()["handle"].as_str().unwrap().to_string();
        let _ = app.session.zoom_extents();
        let ctx = egui::Context::default();
        let screen = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0)));
        let frame = |app: &mut CadApp, events: Vec<egui::Event>, time: f64| {
            app.logic(&ctx);
            let input = egui::RawInput { screen_rect: screen, events, time: Some(time), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| app.ui(ui));
            out.textures_delta.clear();
        };
        frame(&mut app, Vec::new(), 0.0);
        let at = app.canvas.xf.unwrap().to_screen(Vec2::new(5.0, 5.0));
        let button =
            |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::default() };
        frame(&mut app, vec![egui::Event::PointerMoved(at)], 0.1);
        frame(&mut app, vec![button(true), button(false)], 0.2);
        frame(&mut app, vec![button(true), button(false)], 0.3);
        frame(&mut app, Vec::new(), 0.4);
        assert_eq!(app.ui.dialog.as_deref(), Some("hatch"));
        assert_eq!(app.hatch_dlg.as_ref().map(|s| s.edit.iter().map(|h| h.hex()).collect::<Vec<_>>()), Some(vec![hx]));
        // The double-click (at the hatch's centre grip) only opens the dialog: no grip edit.
        assert!(app.session.running.is_none(), "{:?}", app.session.running.as_ref().map(|r| r.machine.name()));
    }
}

#[cfg(test)]
mod drag_trim_tests {
    use cadcraft_engine::Session;
    use cadcraft_engine::doc::EntityKind;
    use cadcraft_geom::Vec2;
    use serde_json::json;

    use crate::{CadApp, Services};

    #[test]
    fn drag_path_helpers_dedupe_and_measure() {
        let mut path = vec![Vec2::new(0.0, 0.0)];
        super::drag_path_push(&mut path, Vec2::new(0.1, 0.0), 0.5);
        assert_eq!(path.len(), 1, "a step under the minimum is dropped");
        super::drag_path_push(&mut path, Vec2::new(1.0, 0.0), 0.5);
        assert_eq!(path.len(), 2);
        // 1 world unit at 10 px/unit is a 10 px drag; at 1 px/unit it is a click.
        assert!(super::drag_path_is_drag(&path, 10.0, 8.0));
        assert!(!super::drag_path_is_drag(&path, 1.0, 8.0));
        assert!(!super::drag_path_is_drag(&[Vec2::new(0.0, 0.0)], 10.0, 8.0));
    }

    fn segs(app: &CadApp) -> Vec<((i64, i64), (i64, i64))> {
        let d = app.session.doc().unwrap();
        let mut v: Vec<((i64, i64), (i64, i64))> = d
            .model
            .iter()
            .filter_map(|e| if let EntityKind::Line(l) = &e.kind { Some(((l.a.x as i64, l.a.y as i64), (l.b.x as i64, l.b.y as i64))) } else { None })
            .map(|(a, b)| if a <= b { (a, b) } else { (b, a) })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn dragging_across_lines_in_trim_trims_them() {
        let mut app = CadApp::new(Session::new(), Services::default());
        for pts in [[[0.0, -5.0], [0.0, 5.0]], [[10.0, -5.0], [10.0, 5.0]], [[-5.0, 0.0], [15.0, 0.0]], [[-5.0, 2.0], [15.0, 2.0]]] {
            app.session.execute("line", &json!({ "points": pts })).unwrap();
        }
        let _ = app.session.zoom_extents();
        app.session.cmdline("trim").unwrap();
        assert!(app.session.drag_trim_active());
        let ctx = egui::Context::default();
        let screen = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0)));
        let frame = |app: &mut CadApp, events: Vec<egui::Event>, time: f64| {
            app.logic(&ctx);
            let input = egui::RawInput { screen_rect: screen, events, time: Some(time), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| app.ui(ui));
            out.textures_delta.clear();
        };
        frame(&mut app, Vec::new(), 0.0);
        let xf = app.canvas.xf.unwrap();
        let (a, m, b) = (xf.to_screen(Vec2::new(5.0, -1.0)), xf.to_screen(Vec2::new(5.0, 1.0)), xf.to_screen(Vec2::new(5.0, 3.0)));
        let button =
            |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::default() };
        frame(&mut app, vec![egui::Event::PointerMoved(a), button(a, true)], 0.1);
        frame(&mut app, vec![egui::Event::PointerMoved(m)], 0.2);
        frame(&mut app, vec![egui::Event::PointerMoved(b), button(b, false)], 0.3);
        frame(&mut app, Vec::new(), 0.4);
        // The stroke crossed both horizontals between the two vertical edges: each is
        // trimmed there, and TRIM is still running for the next pick.
        assert!(app.session.running.is_some(), "trim ended early: {}", app.session.prompt_text());
        assert_eq!(
            segs(&app),
            vec![((-5, 0), (0, 0)), ((-5, 2), (0, 2)), ((0, -5), (0, 5)), ((10, -5), (10, 5)), ((10, 0), (15, 0)), ((10, 2), (15, 2)),]
        );
        app.session.input(cadcraft_engine::Input::Enter).unwrap();
        assert!(app.session.running.is_none());
    }
}
