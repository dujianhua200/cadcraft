//! CADCraft desktop app.
//!
//! Usage: `cadcraft [--control <port>] [--sample] [files…]` (`cadcraft --help` lists the rest; see [`launch_args`])
//!
//! `--control <port>` (or `CADCRAFT_CONTROL_PORT`) starts a localhost JSON-lines control server:
//! `{"id":1,"method":"cmdline.input","params":{"text":"circle 0,0 5"}}` → `{"id":1,"ok":true,…}`.
//! See `cadcraft_ui_egui::control` for the methods.
//!
//! `CADCRAFT_VSYNC=1` presents with vsync on Linux/BSD, where the default is the low-latency
//! present mode (`CADCRAFT_VSYNC=0` selects that elsewhere); see `cadcraft_ui_egui::gpu::surface_config`.
//!
//! `log` records go to standard error and `<settings dir>/logs/cadcraft.log` (`RUST_LOG` sets the
//! levels); see [`logging`]. The `CADCRAFT_*` variables are read once, at startup; see [`env`].
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod control_server;
mod desktop_theme;
mod env;
#[cfg(any(target_os = "windows", test))]
mod graphics;
mod launch_args;
mod logging;
#[cfg(target_os = "macos")]
mod native_menu;
mod open_file;
mod spacemouse;
mod system_animations;
mod workspace;

use cadcraft_engine::Session;
use cadcraft_ui_egui::switch::Switch;
use cadcraft_ui_egui::{CadApp, Services, i18n};

struct App {
    cad: CadApp,
    spacemouse: spacemouse::SpaceMouse,
    /// The desktop's light/dark choice where winit reports none (Linux, the BSDs).
    desktop_theme: desktop_theme::DesktopTheme,
    /// Whether the OS shows animations, for the System choice of View ▸ Accessibility ▸ Animation Effects.
    system_animations: system_animations::SystemAnimations,
    workspace: workspace::Workspace,
    workspace_error: Option<String>,
    #[cfg(target_os = "macos")]
    native_menu: Option<native_menu::NativeMenu>,
    /// The desktop open-file source (Finder double-click, "Open With", Dock drop on macOS);
    /// kept alive until the event loop returns, so its handler stays registered.
    desktop_files: open_file::DesktopFiles,
    /// `CADCRAFT_NO_NATIVE_MENU`: the menus stay inside the window.
    #[cfg(target_os = "macos")]
    menus_in_window: bool,
    /// The `movable` flag last set on the main window.
    #[cfg(target_os = "macos")]
    movable: Option<bool>,
}

impl App {
    /// `ctx` is `None` only for the final attempt at exit.
    fn persist_workspace(&mut self, ctx: Option<&egui::Context>) {
        let result = match ctx {
            None => self.workspace.save_on_exit(&self.cad.ui),
            Some(ctx) => self.workspace.save_if_changed(&self.cad.ui, std::time::Instant::now()).map(|wait| {
                if let Some(wait) = wait {
                    ctx.request_repaint_after(wait);
                }
            }),
        };
        if let Err(error) = result {
            if self.workspace_error.as_ref() != Some(&error) {
                eprintln!("CADCraft workspace: {error}");
                self.cad.set_status(format!("Workspace could not be saved: {error}"));
                self.workspace_error = Some(error);
            }
        } else {
            self.workspace_error = None;
        }
    }
}

/// View ▸ Theme ▸ System resolves against `raw.system_theme`, which winit leaves empty on Linux
/// and the BSDs: the desktop's answer, where it gives one, stands in for it.
fn follow_desktop(raw: &mut egui::RawInput, desktop: Option<egui::Theme>) {
    if desktop.is_some() {
        raw.system_theme = desktop;
    }
}

impl eframe::App for App {
    /// eframe logic callback: syncs the OS animation setting and runs `CadApp::logic`; when the pass index is 0 (the first pass or a hidden tick) it also installs/polls the macOS native menu, polls the spacemouse and persists the workspace; a quit request closes the viewport.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // A desktop handoff (Finder double-click, "Open With", Dock drop on macOS) opens
        // through the same `open_path` File ▸ Open uses; a desktop quit request closes.
        let desktop = self.desktop_files.drain();
        apply_desktop_events(&mut self.cad, desktop);
        self.cad.system_animations = self.system_animations.current();
        // Consume control/open/screenshot queues and time-based autosave, including hidden ticks.
        self.cad.guarded_logic(ctx);
        // Host frame work runs here, once per frame, and so also on a hidden logic-only tick (which
        // never reaches `App::ui`): native menus stay current, the spacemouse still navigates and a
        // workspace changed while hidden is persisted. `current_pass_index()` is zero on the first
        // pass and on every hidden tick, and non-zero on later passes of a multi-pass frame.
        if ctx.current_pass_index() == 0 {
            #[cfg(target_os = "macos")]
            {
                if self.native_menu.is_none() && !self.menus_in_window {
                    self.native_menu = Some(native_menu::NativeMenu::install(&mut self.cad, ctx));
                }
                if let Some(m) = &mut self.native_menu {
                    m.poll(&mut self.cad, ctx);
                }
            }
            self.spacemouse.poll(&mut self.cad, ctx);
            // After `cad.logic`, so a control command's workspace change is saved in the same frame.
            self.persist_workspace(Some(ctx));
        }
        if std::mem::take(&mut self.cad.quit_requested) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
    /// Apply the desktop-theme fallback, forward raw input to `CadApp`, and capture Tab at the command line.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        follow_desktop(raw, self.desktop_theme.current());
        self.cad.raw_input_hook(ctx, raw);
        cadcraft_ui_egui::cmdline::capture_tab(ctx, raw);
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.cad.guarded_ui(ui);
        #[cfg(target_os = "macos")]
        craft_window::title_bar::sync_movable(ui.ctx(), &mut self.movable);
    }
    /// A normal exit: the workspace is saved, and this run's autosave files are not needed (a
    /// crash leaves them for recovery).
    fn on_exit(&mut self) {
        self.persist_workspace(None);
        self.cad.cancel_screenshots();
        self.cad.shutdown_autosave();
    }
    /// Preferences (the interface theme) survive restarts; eframe saves the window state itself
    /// (`persist_window`); egui state is not kept.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string(cadcraft_ui_egui::options::PROFILE_KEY, self.cad.profile_json());
        storage.set_string(cadcraft_ui_egui::PREFS_KEY, self.cad.prefs_json());
        storage.set_string(cadcraft_ui_egui::autosave::PREFS_KEY, cadcraft_ui_egui::autosave::prefs_json());
    }
    fn persist_egui_memory(&self) -> bool {
        false
    }
}

/// File format hooks for the engine (it stays I/O-agnostic).
pub fn install_io() {
    cadcraft_engine::cmd::file::set_io(cadcraft_engine::cmd::file::IoHooks {
        read_progress: Some(|b, name, p| cadcraft_io::read_with_progress(b, name, p).map_err(|e| e.to_string())),
        read: |b, name| cadcraft_io::read(b, name).map_err(|e| e.to_string()),
        write: |d, name| cadcraft_io::write(d, name).map_err(|e| e.to_string()),
        plot: Some(|d, space, opts| cadcraft_io::plotter::plot(d, space, opts).map_err(|e| e.to_string())),
    });
    cadcraft_engine::cmd::file::set_image_decoder(|b| cadcraft_io::raster::decode(b).map_err(|e| e.to_string()));
    cadcraft_engine::cmd::file::set_write_as(|d, name, o| {
        cadcraft_io::write_with(d, name, &cadcraft_io::SaveOptions { version: o.version, binary: o.binary }).map_err(|e| e.to_string())
    });
    // AutoLISP's entget, entmake and entmod see entities as DXF groups.
    cadcraft_engine::cmd::file::set_entity_codec(cadcraft_engine::cmd::file::EntityCodec {
        groups: cadcraft_io::entity_groups,
        entity: cadcraft_io::entity_from_groups,
    });
}

fn services() -> Services {
    Services {
        pick_open: Some(Box::new(|| {
            rfd::FileDialog::new()
                .add_filter(i18n::t("Drawings (DWG, DXF)"), &["dwg", "dxf", "dwt"])
                .add_filter(i18n::t("SVG drawings (SVG, SVGZ)"), &["svg", "svgz"])
                .add_filter(i18n::t("All files"), &["*"])
                .pick_file()
                .map(|p| p.to_string_lossy().to_string())
        })),
        pick_save: Some(Box::new(|name: &str| {
            filters_for_save(name)
                .iter()
                .fold(rfd::FileDialog::new().set_file_name(name), |d, (label, ext)| d.add_filter(i18n::t(label), &[*ext]))
                .save_file()
                .map(|p| p.to_string_lossy().to_string())
        })),
        // APPLOAD: AutoLISP routines only, starting where the last one was loaded from.
        pick_lisp: Some(Box::new(|dir: &str| {
            let dialog = rfd::FileDialog::new().add_filter(i18n::t("AutoLISP (LSP)"), &["lsp"]).add_filter(i18n::t("All files"), &["*"]);
            let dialog = if dir.is_empty() { dialog } else { dialog.set_directory(dir) };
            dialog.pick_file().map(|p| p.to_string_lossy().to_string())
        })),
        no_file_system: false,
        pick_pgp: Some(Box::new(|dir: Option<&str>| {
            let dialog = rfd::FileDialog::new().add_filter(i18n::t("Alias files (PGP)"), &["pgp"]);
            let dialog = match dir {
                Some(dir) => dialog.set_directory(dir),
                None => dialog,
            };
            dialog.pick_file().map(|p| p.to_string_lossy().to_string())
        })),
    }
}

/// File types for the Save dialog, the suggested name's type first: Windows adds the first
/// filter's extension, and macOS accepts only listed extensions, replacing any other with the
/// first one. DWG must be listed for a `.dwg` name to survive, and a drawing opened from DWG
/// then defaults to DWG; other names keep DXF first.
fn filters_for_save(name: &str) -> [(&'static str, &'static str); 6] {
    let mut filters =
        [("Drawing (DXF)", "dxf"), ("Drawing (DWG)", "dwg"), ("Drawing Template (DWT)", "dwt"), ("SVG", "svg"), ("PNG", "png"), ("PDF", "pdf")];
    let lower = name.to_ascii_lowercase();
    filters.sort_by_key(|(_, ext)| !lower.ends_with(&format!(".{ext}")));
    filters
}

/// Whether eframe's storage file (`app.ron`, a map of keys to strings) holds a saved main window
/// state. An unreadable or malformed file counts as none, as it does for eframe.
fn saved_window(app_ron: &str) -> bool {
    ron::from_str::<std::collections::HashMap<String, String>>(app_ron).is_ok_and(|kv| kv.contains_key(WINDOW_KEY))
}

/// The window state from the last session is saved and will be restored (#2595).
fn restores_window() -> bool {
    eframe::storage_dir(APP_ID).and_then(|dir| std::fs::read_to_string(dir.join("app.ron")).ok()).is_some_and(|ron| saved_window(&ron))
}

/// The main window: its size, title bar, placement and rendering needs. `restoring`: a window state
/// saved by the last session is restored instead of opening centred.
fn window_options(restoring: bool) -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("CADCraft")
            .with_inner_size([1600.0, 1000.0])
            .with_min_inner_size([900.0, 560.0])
            .with_drag_and_drop(true)
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
            .with_app_id(APP_ID),
        // The size, position and maximized state are kept between sessions (#2595); eframe keeps a
        // restored window on the monitors that are connected now.
        persist_window: true,
        // The first launch opens centred on its screen, not cascaded from the top-left corner
        // (#1129); later launches go back where the window was (eframe would centre over that).
        // Wayland compositors place windows themselves.
        centered: !restoring,
        // CADCraft installs its own capped font chain (`theme::install_fonts`); never run
        // system-font discovery as a hidden fallback.
        system_font_fallback: false,
        // A stencil buffer clips the contents of clipped layout viewports on the GPU.
        stencil_buffer: STENCIL_BITS,
        ..Default::default()
    }
}

const APP_ID: &str = "ai.storyteller.cadcraft";

/// The key eframe saves the main window state under in its storage.
const WINDOW_KEY: &str = "window";

/// Stencil bits of the window's render pass (see `NativeOptions::stencil_buffer`).
const STENCIL_BITS: u8 = 8;

#[cfg(target_os = "macos")]
const APP_ICON_PNG: &[u8] = include_bytes!("../../../assets/app-icon/cadcraft-macos-512.png");
#[cfg(not(target_os = "macos"))]
const APP_ICON_PNG: &[u8] = include_bytes!("../../../assets/app-icon/cadcraft-256.png");

fn app_icon() -> Option<egui::IconData> {
    eframe::icon_data::from_png_bytes(APP_ICON_PNG).map_err(|e| log::warn!("app icon: {e}")).ok()
}

fn version_string() -> String {
    format!("cadcraft {}", cadcraft_engine::build_version())
}

/// A desktop handoff from [`open_file::DesktopFiles::drain`]: open each file through the
/// same `open_path` File ▸ Open uses, and quit on a desktop quit request (Cmd+Q, Dock ▸ Quit).
fn apply_desktop_events(cad: &mut CadApp, events: Vec<open_file::DesktopEvent>) {
    for event in events {
        match event {
            open_file::DesktopEvent::Open(path) => cad.open_path(&path),
            open_file::DesktopEvent::Quit => cad.quit_requested = true,
        }
    }
}

/// The control port from `--control <port>` or `CADCRAFT_CONTROL_PORT` (`source`); a value that is
/// not a port is logged and ignored instead of silently starting no control channel.
// Fork note: upstream replaced this hand-rolled parser with `launch_args`
// (clap; a bad flag is a usage error, exit 2). The parser and its unit tests
// are kept for reference under `cfg(test)` only — `main` does not call them.
#[cfg(test)]
fn control_port_from(source: &str, value: Option<String>) -> Option<u16> {
    let value = value?;
    let port = value.trim().parse().ok();
    if port.is_none() {
        log::warn!("{source}: {value:?} is not a port number; the control channel is off");
    }
    port
}

/// `--control` followed by `value`: a bare `--control` (last argument) keeps the port already
/// read from `CADCRAFT_CONTROL_PORT` (`current`) instead of turning the control channel off.
#[cfg(test)]
fn control_flag(current: Option<u16>, value: Option<String>) -> Option<u16> {
    if value.is_some() {
        return control_port_from("--control", value);
    }
    match current {
        Some(port) => log::warn!("--control: no port given; using CADCRAFT_CONTROL_PORT ({port})"),
        None => log::warn!("--control: no port given; the control channel is off"),
    }
    current
}

/// The command-line arguments `main` acts on: `--control <port>`, `--sample`, files to
/// open, and `--version` / `-V` (which prints and exits before any log file is opened).
#[cfg(test)]
struct StartupArgs {
    control_port: Option<u16>,
    files: Vec<String>,
    sample: bool,
    version: bool,
}

/// Split `args` (already without the program name) into [`StartupArgs`]; `env_port` is the
/// port from `CADCRAFT_CONTROL_PORT`, which a bare `--control` keeps. Anything that is not
/// a recognised flag is a file to open, so `cadcraft drawing.dwg` and `cadcraft --bogus`
/// both reach `open_path` (the latter fails there, like a bad File ▸ Open).
#[cfg(test)]
fn parse_startup_args(mut args: impl Iterator<Item = String>, env_port: Option<u16>) -> StartupArgs {
    let mut control_port = env_port;
    let mut files = Vec::new();
    let mut sample = false;
    let mut version = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--control" => control_port = control_flag(control_port, args.next()),
            "--sample" => sample = true,
            "--version" | "-V" => version = true,
            _ => files.push(a),
        }
    }
    StartupArgs { control_port, files, sample, version }
}
/// Parse CLI options (`--control`, `--sample`, files, `--version`), install logging and I/O hooks, build the services and app, and run eframe.
fn main() -> eframe::Result {
    // A copy of this program started to read or write a DWG file does only that.
    if let Some(code) = cadcraft_io::dwg_worker::serve_if_asked() {
        std::process::exit(code);
    }
    cadcraft_io::dwg_worker::use_own_worker();
    // First, so every start-up record is captured; see `logging`.
    let logger = logging::install();
    // FORK (dujianhua200/cadcraft): upstream's self-update helper is disabled. It
    // downloads releases from storytold/cadcraft and would replace this fork build
    // (and all fork features) with an upstream one. This fork updates through its
    // own GitHub Releases instead; see docs/upstream-sync.md. The upstream call was:
    // `craft_update::self_update::finish(&cadcraft_ui_egui::UPDATE_APP, …)`.
    // `--help` and `--version` print to standard output and exit 0. A usage error (an unknown or
    // repeated flag, a missing or bad value, a bad CADCRAFT_CONTROL_PORT) names the argument on
    // standard error and exits 2 before the window opens.
    let launch = launch_args::parse(launch_args::command(), std::env::args_os().skip(1)).unwrap_or_else(|e| e.exit());
    let vars = env::Env::read();
    let (control_port, sample, files) = (launch.control, launch.sample, launch.files);
    // The log file lives under the settings directory; opened after the arguments, so `--version`
    // leaves no file behind. Records logged until now are written to it first.
    if let Some(logger) = logger {
        match vars.config_dir.as_ref().map(|d| logger.attach_dir(&d.join(logging::LOG_DIR))) {
            Some(Ok(path)) => log::info!("{} ({} {}), log file {}", version_string(), std::env::consts::OS, std::env::consts::ARCH, path.display()),
            Some(Err(e)) => log::warn!("no log file: {e}"),
            None => {
                logger.stderr_only();
                log::warn!("no log file: no settings directory (an empty CADCRAFT_CONFIG_DIR turns it off)");
            }
        }
    }
    install_io();
    cadcraft_engine::settings_dir::set(vars.config_dir.clone());
    if let Some(dir) = &vars.config_dir {
        cadcraft_ui_egui::fault::install_report_hook(dir.join(cadcraft_ui_egui::fault::REPORT_DIR));
    }
    if let Some(name) = &vars.font_alt {
        cadcraft_engine::fonts::ttf::set_font_alt(name);
    }
    if let Some(list) = &vars.font_fallback {
        cadcraft_engine::fonts::ttf::set_fallback_fonts(list);
    }
    let surface = cadcraft_ui_egui::gpu::surface_config(vars.vsync, cfg!(all(unix, not(target_os = "macos"))));
    let mut options = eframe::NativeOptions {
        // The crosshair is drawn by the app: present without a queue on Linux/BSD, where X11
        // swapchains hold two frames back under vsync (`CADCRAFT_VSYNC=1` restores vsync).
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration::default().with_surface_config(surface),
        ..window_options(restores_window())
    };
    if let Some(icon) = app_icon() {
        options.viewport = options.viewport.with_icon(icon);
    }
    // Before eframe creates the wgpu instance: default Windows to DirectX 12 with FXC (see graphics.rs).
    #[cfg(target_os = "windows")]
    graphics::configure(&mut options, eframe::wgpu::Backends::from_env(), eframe::wgpu::Dx12Compiler::from_env());
    // Command previews (COPY, MOVE...) of large selections are tessellated by egui, whose index
    // buffer outgrew wgpu's default 256 MiB cap; the validation error is fatal. Ask for what the
    // adapter actually supports.
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup {
        let base = std::sync::Arc::clone(&create.device_descriptor);
        create.device_descriptor = std::sync::Arc::new(move |adapter| {
            let mut d = base(adapter);
            d.required_limits.max_buffer_size = d.required_limits.max_buffer_size.max(adapter.limits().max_buffer_size);
            d
        });
    }
    // winit has no file drag-and-drop on Wayland (only on X11), so dropping files from the file
    // manager showed a "no" cursor. Run through XWayland when it's there; CADCRAFT_WAYLAND=1
    // keeps the native Wayland backend.
    #[cfg(all(unix, not(target_os = "macos")))]
    if std::env::var_os("DISPLAY").is_some() && vars.wayland != Switch::On {
        options.event_loop_builder = Some(Box::new(|b| {
            use winit::platform::x11::EventLoopBuilderExtX11;
            b.with_x11();
        }));
    }
    // The desktop open-file handler (Finder double-click, "Open With", Dock drop on macOS):
    // installed before the event loop starts, so its will-finish-launching registration
    // survives AppKit's own handler installation; the guard lives in the app below.
    let desktop_files = open_file::DesktopFiles::install();
    // macOS: Quit CADCraft (Cmd+Q), the Dock's Quit and logout end the app through AppKit's
    // terminate, which would skip the unsaved-changes prompt (#2157). winit turns each into a
    // close request for the window instead, so it asks first, as the close button does.
    #[cfg(target_os = "macos")]
    {
        options.event_loop_builder = Some(Box::new(|b| {
            use winit::platform::macos::EventLoopBuilderExtMacOS;
            b.with_close_requested_on_terminate(true);
        }));
    }
    eframe::run_native(
        "CADCraft",
        options,
        Box::new(move |cc| {
            // A desktop handoff wakes a frame, so the open is not stuck behind an idle UI.
            {
                let ctx = cc.egui_ctx.clone();
                desktop_files.set_wake(move || ctx.request_repaint());
            }
            let mut app = CadApp::new(Session::empty(), services());
            let (workspace, restored, workspace_error) = workspace::Workspace::load(vars.config_dir.as_deref());
            if let Some(ui) = restored {
                app.ui = ui;
            }
            app.set_system_cursor_default(vars.system_cursor == Switch::On);
            if let Some(error) = &workspace_error {
                eprintln!("CADCraft workspace: {error}");
                app.set_status(error);
            }
            app.system_languages = vars.locale.map(|s| vec![s]).unwrap_or_else(|| sys_locale::get_locales().collect());
            // The user profile (Options): drafting settings, selection sizes, drawing-area colours.
            if let Some(profile) = cc.storage.and_then(|s| s.get_string(cadcraft_ui_egui::options::PROFILE_KEY)) {
                app.load_profile(&profile);
            }
            if let Some(prefs) = cc.storage.and_then(|s| s.get_string(cadcraft_ui_egui::PREFS_KEY)) {
                app.load_prefs(&prefs);
            }
            if let Some(prefs) = cc.storage.and_then(|s| s.get_string(cadcraft_ui_egui::autosave::PREFS_KEY)) {
                cadcraft_ui_egui::autosave::load_prefs(&prefs);
            }
            // Autosave files (SAVEFILEPATH) default to `<settings dir>/autosave`.
            if let Some(dir) = &vars.config_dir {
                app.enable_autosave(&dir.join("autosave"));
                app.enable_update_check();
                // Startup LISP (acad.lsp, acaddoc.lsp) is read from `<settings>/support`; the
                // folder is made so it is there to put them in.
                let support = dir.join("support");
                if let Err(e) = std::fs::create_dir_all(&support) {
                    log::warn!("no LISP support folder {}: {e}", support.display());
                }
                app.session.set_lisp_support_dir(Some(support));
            }
            app.integrated_titlebar = cfg!(target_os = "macos");
            if surface.present_mode == eframe::wgpu::PresentMode::AutoNoVsync {
                app.frame_cap = Some(cadcraft_ui_egui::gpu::FrameCap::new(240));
            }
            if let Some(rs) = &cc.wgpu_render_state {
                app.set_wgpu(rs, eframe::egui_wgpu::depth_format_from_bits(0, STENCIL_BITS));
            }
            if let Some(port) = control_port {
                let rx = control_server::start(port, cc.egui_ctx.clone());
                app = app.with_control(rx);
            }
            if sample {
                let _ = app.run("ui.sample", serde_json::json!({}));
            }
            for f in &files {
                app.open_path(f);
            }
            if app.session.docs.is_empty() {
                // In the units the profile asks for (MEASUREINIT, restored above).
                let metric = app.session.settings.measureinit;
                app.session.new_drawing(metric);
            }
            let ctx = cc.egui_ctx.clone();
            let desktop_theme = desktop_theme::DesktopTheme::start(move || ctx.request_repaint());
            let ctx = cc.egui_ctx.clone();
            let system_animations = system_animations::SystemAnimations::start(move || ctx.request_repaint());
            Ok(Box::new(App {
                desktop_theme,
                system_animations,
                cad: app,
                spacemouse: spacemouse::SpaceMouse::start(cc.egui_ctx.clone()),
                workspace,
                workspace_error,
                desktop_files,
                #[cfg(target_os = "macos")]
                native_menu: None,
                #[cfg(target_os = "macos")]
                menus_in_window: vars.no_native_menu == Switch::On,
                #[cfg(target_os = "macos")]
                movable: None,
            }))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        CadApp, Services, Session, apply_desktop_events, filters_for_save, follow_desktop, install_io, open_file::DesktopEvent, parse_startup_args,
        saved_window, window_options,
    };

    #[test]
    fn save_dialog_offers_dwg_with_the_drawing_format_first() {
        let first = |name: &str| filters_for_save(name)[0].1;
        assert_eq!(first("Waco Winnelson Floorplan.dwg"), "dwg");
        assert_eq!(first("FLOORPLAN.DWG"), "dwg");
        // New drawings and DXF files keep DXF first.
        assert_eq!(first("Drawing1.dxf"), "dxf");
        assert_eq!(first("Drawing1"), "dxf");
        assert_eq!(first("plan.pdf"), "pdf");
        assert_eq!(first("office.dwt"), "dwt");
        for name in ["a.dwg", "b.dxf", "c"] {
            let exts: Vec<_> = filters_for_save(name).iter().map(|(_, e)| *e).collect();
            assert!(exts.contains(&"dwg") && exts.contains(&"dxf"), "{name}");
        }
    }

    /// #1129: the first launch opens centred, not cascaded from the top-left corner.
    #[test]
    fn the_first_launch_opens_centred() {
        let o = window_options(false);
        assert!(o.centered, "centred on launch");
        assert!(o.persist_window, "the window state is saved for the next launch");
    }

    /// #2595: a saved window state (size, position, maximized) is restored, not centred over.
    #[test]
    fn a_saved_window_reopens_where_it_was() {
        let o = window_options(true);
        assert!(!o.centered, "centring would move a restored window");
        assert!(o.persist_window);
    }

    /// Only a readable `app.ron` with a window entry counts as a saved window state.
    #[test]
    fn a_saved_window_is_read_from_app_ron() {
        assert!(saved_window(r#"{"cadcraft.prefs": "{}", "window": "(maximized: true)"}"#));
        assert!(!saved_window(r#"{"cadcraft.prefs": "{\"window\":1}"}"#), "a value mentioning it is not the key");
        assert!(!saved_window(""));
        assert!(!saved_window("{\"window\": "), "truncated");
        assert!(!saved_window("not ron at all"));
    }

    /// View ▸ Theme ▸ System on Linux: winit reports no theme, so the desktop's answer stands in;
    /// without one (or where winit answers and the desktop doesn't) the input is left alone.
    #[test]
    fn the_desktop_answers_for_the_system_theme() {
        let mut raw = egui::RawInput::default();
        follow_desktop(&mut raw, Some(egui::Theme::Light));
        assert_eq!(raw.system_theme, Some(egui::Theme::Light));
        let mut raw = egui::RawInput { system_theme: Some(egui::Theme::Dark), ..Default::default() };
        follow_desktop(&mut raw, None);
        assert_eq!(raw.system_theme, Some(egui::Theme::Dark));
        let mut raw = egui::RawInput::default();
        follow_desktop(&mut raw, None);
        assert_eq!(raw.system_theme, None);
    }

    #[test]
    fn startup_args_split_flags_from_files() {
        let args = parse_startup_args(["--sample", "a.dwg", "--control", "7979", "b.dxf"].into_iter().map(String::from), None);
        assert_eq!(args.files, ["a.dwg", "b.dxf"]);
        assert!(args.sample);
        assert_eq!(args.control_port, Some(7979));
        assert!(!args.version);
    }

    #[test]
    fn startup_args_version_and_bare_control() {
        let args = parse_startup_args(["-V"].into_iter().map(String::from), Some(47321));
        assert!(args.version);
        assert_eq!(args.control_port, Some(47321));
        // A bare `--control` keeps the environment port; a bad one turns the channel off.
        let args = parse_startup_args(["--control"].into_iter().map(String::from), Some(47321));
        assert_eq!(args.control_port, Some(47321));
        let args = parse_startup_args(["--control", "abc"].into_iter().map(String::from), Some(47321));
        assert_eq!(args.control_port, None);
        // An unrecognised flag is a file to open, not an error.
        let args = parse_startup_args(["--bogus"].into_iter().map(String::from), None);
        assert_eq!(args.files, ["--bogus"]);
    }

    #[test]
    fn desktop_quit_event_requests_quit() {
        let mut app = CadApp::new(Session::new(), Services::default());
        apply_desktop_events(&mut app, vec![DesktopEvent::Quit]);
        assert!(app.quit_requested);
    }

    /// A desktop Open event reaches the same `open_path` File ▸ Open uses: the drawing
    /// arrives in the session once its background read finishes.
    #[test]
    fn desktop_open_event_opens_a_drawing() {
        // The reader hooks are installed by `main`; without them the open fails.
        install_io();
        let dir = std::env::temp_dir().join(format!("cadcraft-desktop-open-{}", std::process::id()));
        assert!(std::fs::create_dir_all(&dir).is_ok());
        let path = dir.join("empty.dxf");
        let dxf = "0\nSECTION\n2\nHEADER\n0\nENDSEC\n0\nSECTION\n2\nTABLES\n0\nENDSEC\n0\nSECTION\n2\nBLOCKS\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n0\nENDSEC\n0\nEOF\n";
        assert!(std::fs::write(&path, dxf).is_ok());
        let mut app = CadApp::new(Session::empty(), Services::default());
        let before = app.session.docs.len();
        apply_desktop_events(&mut app, vec![DesktopEvent::Open(path.clone())]);
        let start = std::time::Instant::now();
        while app.session.docs.len() == before && start.elapsed() < std::time::Duration::from_secs(10) {
            app.finish_opening();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        app.finish_opening();
        assert_eq!(app.session.docs.len(), before + 1);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
