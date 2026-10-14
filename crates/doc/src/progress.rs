//! Progress and cancellation for opening a drawing.
//!
//! Opening a large file (a 20 MB DWG can expand to ~100 MB of DXF and tens of thousands of
//! entities) takes seconds to tens of seconds. The reader reports which stage it is in so the
//! UI can show it and a headless caller can poll it, and it stops early when cancelled: a
//! cancelled open never produces a half-built drawing.
//!
//! The types live here (the lowest layer every reader depends on) so the file readers, the
//! engine and the UI all share one definition instead of each inventing a token.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// A stage of opening a drawing file, in order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OpenStage {
    /// The DWG bytes are being decoded to DXF (a no-op for a DXF file, which is already text).
    #[default]
    DwgDecode,
    /// The DXF text is being parsed into group codes.
    DxfParse,
    /// Group codes are being built into the drawing model (entities, blocks, tables).
    Build,
    /// The drawing is fully built.
    Done,
}

impl OpenStage {
    /// The stage's label for the opening card and for headless progress reports.
    pub fn label(self) -> &'static str {
        match self {
            OpenStage::DwgDecode => "正在解码 DWG",
            OpenStage::DxfParse => "正在解析 DXF",
            OpenStage::Build => "正在构建图形",
            OpenStage::Done => "已完成",
        }
    }
}

/// Shared progress of one open: the current stage (polled by the UI or a headless caller) and
/// the cancellation flag. Cloned handles share the same state.
#[derive(Default)]
pub struct OpenProgress {
    stage: Mutex<OpenStage>,
    cancelled: AtomicBool,
    /// An observer the reader calls on every stage change (the UI thread forwards it, or a
    /// headless caller records it); a failing observer never stops the read.
    on_stage: Mutex<Option<Box<dyn FnMut(OpenStage) + Send>>>,
}

impl std::fmt::Debug for OpenProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenProgress").field("stage", &self.stage()).field("cancelled", &self.is_cancelled()).finish_non_exhaustive()
    }
}

impl OpenProgress {
    /// A fresh progress tracker, at [`OpenStage::DwgDecode`] and not cancelled.
    pub fn new() -> Self {
        OpenProgress::default()
    }

    /// The current stage.
    pub fn stage(&self) -> OpenStage {
        self.stage.lock().map(|g| *g).unwrap_or_default()
    }

    /// Move to `stage` and tell the observer, if one is installed.
    pub fn set_stage(&self, stage: OpenStage) {
        if let Ok(mut g) = self.stage.lock() {
            *g = stage;
        }
        if let Ok(mut obs) = self.on_stage.lock()
            && let Some(f) = obs.as_mut()
        {
            f(stage);
        }
    }

    /// Install the stage-change observer, replacing any previous one.
    pub fn set_observer(&self, f: impl FnMut(OpenStage) + Send + 'static) {
        if let Ok(mut obs) = self.on_stage.lock() {
            *obs = Some(Box::new(f));
        }
    }

    /// Ask the reader to stop. The read returns a cancellation error; nothing half-built is
    /// handed back.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Whether cancellation was asked for.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_and_cancel_roundtrip() {
        let p = OpenProgress::new();
        assert_eq!(p.stage(), OpenStage::DwgDecode);
        assert!(!p.is_cancelled());
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        p.set_observer(move |s| {
            if let Ok(mut v) = seen2.lock() {
                v.push(s);
            }
        });
        p.set_stage(OpenStage::Build);
        assert_eq!(p.stage(), OpenStage::Build);
        assert_eq!(seen.lock().map(|v| v.clone()).unwrap_or_default(), vec![OpenStage::Build]);
        p.cancel();
        assert!(p.is_cancelled());
    }
}
