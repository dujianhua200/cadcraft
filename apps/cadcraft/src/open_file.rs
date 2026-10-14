//! Files the desktop hands the app, beyond the command line.
//!
//! Every desktop passes a document on the command line, and [`crate::parse_startup_args`]
//! picks those up. macOS adds its own handoff: a double-click in Finder, an "Open With"
//! choice, or a file dropped on the app's icon arrives as an open-documents Apple Event,
//! never as an argument. winit 0.30 owns the `NSApplication` delegate and does not forward
//! that event (its delegate answers only launch and terminate), and eframe 0.36 does not
//! surface it either, so with no handler AppKit itself answered the event with
//! "cannot open files in the Drawing format" (#158). [`DesktopFiles`] answers it instead:
//! on macOS it installs the Apple Event handler through `fmv_macos_events` (registered
//! from the will-finish-launching notification, the last moment a registration survives
//! AppKit installing its own handler at launch) and queues each event for the app's next
//! frame, where [`crate::apply_desktop_events`] opens the files through the same
//! `open_path` File ▸ Open uses. The registration guard lives in this struct: dropping it
//! unregisters the handler, so it is kept until the event loop returns.
//!
//! No `unsafe` here: the Objective-C work lives in `fmv_macos_events`, whose public API is
//! safe Rust. This module only moves paths and quit requests across to the app.

use std::path::PathBuf;

/// One desktop handoff: a file to open, or a request to quit (Cmd+Q, Dock ▸ Quit).
///
/// On other desktops the variants are never constructed (only the command line hands files
/// over), which is expected, not dead code.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopEvent {
    /// Open this file, as File ▸ Open would.
    Open(PathBuf),
    /// Quit the app, as closing the window would.
    Quit,
}

/// The desktop open-file source: installed once in `main`, drained every frame.
#[derive(Default)]
pub struct DesktopFiles {
    /// Held for its `Drop`: dropping it unregisters the handler. Never read by design.
    #[cfg(target_os = "macos")]
    #[allow(dead_code)]
    registration: Option<fmv_macos_events::Registration>,
    #[cfg(target_os = "macos")]
    inbox: fmv_macos_events::Inbox,
}

impl DesktopFiles {
    /// Install the desktop open-file handler. The returned value must live until the event
    /// loop returns: dropping it unregisters the handler. On other desktops this is a
    /// no-op (the command line is the only handoff).
    pub fn install() -> Self {
        #[cfg(target_os = "macos")]
        {
            let (registration, inbox) = fmv_macos_events::Registration::install();
            Self { registration: Some(registration), inbox }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self::default()
        }
    }

    /// Wake the UI when a desktop event arrives, so a frame drains it even if the app was
    /// idle. Called once with the frame context, before the event loop starts.
    pub fn set_wake(&self, wake: impl Fn() + 'static) {
        #[cfg(target_os = "macos")]
        self.inbox.set_wake(wake);
        #[cfg(not(target_os = "macos"))]
        drop(wake);
    }

    /// The desktop events since the last call, in arrival order.
    pub fn drain(&self) -> Vec<DesktopEvent> {
        #[cfg(target_os = "macos")]
        {
            let mut out = Vec::new();
            for event in self.inbox.drain() {
                match event {
                    fmv_macos_events::Event::Open(paths) => out.extend(paths.into_iter().map(DesktopEvent::Open)),
                    fmv_macos_events::Event::Quit => out.push(DesktopEvent::Quit),
                }
            }
            out
        }
        #[cfg(not(target_os = "macos"))]
        {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_source_has_no_events() {
        assert!(DesktopFiles::default().drain().is_empty());
    }

    /// `install` is only called from `main`, on the main thread: on macOS it would panic
    /// on a test thread (the handler needs the main thread), so this runs elsewhere.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn install_and_drain_are_safe_without_desktop_events() {
        let files = DesktopFiles::install();
        files.set_wake(|| {});
        assert!(files.drain().is_empty());
    }
}
