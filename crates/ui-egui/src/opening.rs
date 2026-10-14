//! Opening drawings without freezing the window (#694). A file is read and decoded on a worker
//! thread (the slow part of OPEN, which needs no session); the UI shows an "Opening ..." card with
//! the stage, the elapsed time and a Cancel button, and keeps painting. The finished drawing is
//! added to the session on the UI thread. Cancelling stops the read: no half-built drawing is
//! added. The web build has no threads and opens synchronously.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use cadcraft_doc::OpenProgress;
use cadcraft_engine::cmd::file::{ReadFile, read_file_with_progress};

use crate::theme::Tokens;

type Outcome = Result<ReadFile, String>;

struct Job {
    path: String,
    started: Instant,
    progress: Arc<OpenProgress>,
    rx: Receiver<Outcome>,
}

/// The files being read in the background, oldest first.
#[derive(Default)]
pub struct Opening {
    jobs: Vec<Job>,
}

impl Opening {
    /// Start reading `path` on a worker thread.
    pub fn start(&mut self, path: &str) {
        let (tx, rx) = channel();
        let p = path.to_string();
        let progress = Arc::new(OpenProgress::new());
        let progress2 = progress.clone();
        let spawned = std::thread::Builder::new().name("cadcraft-open".into()).spawn(move || {
            let _ = tx.send(read_file_with_progress(&p, &progress2));
        });
        if spawned.is_err() {
            // No thread to read on: report it like any other failed open.
            let (tx, rx2) = channel();
            let _ = tx.send(Err(format!("open: {path}: could not start a reader thread")));
            self.jobs.push(Job { path: path.to_string(), started: Instant::now(), progress, rx: rx2 });
            return;
        }
        self.jobs.push(Job { path: path.to_string(), started: Instant::now(), progress, rx });
    }

    /// Ask every in-flight read to stop. The worker returns a cancellation error, the job is
    /// dropped by [`Opening::poll`], and no half-built drawing reaches the session.
    pub fn cancel_all(&mut self) {
        for j in &self.jobs {
            j.progress.cancel();
        }
    }

    /// Whether any file is still being read.
    pub fn busy(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// The finished reads, as (path, result). A reader that died without answering is an error.
    pub fn poll(&mut self) -> Vec<(String, Outcome)> {
        let mut done = Vec::new();
        self.jobs.retain(|j| match j.rx.try_recv() {
            Ok(r) => {
                done.push((j.path.clone(), r));
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => {
                done.push((j.path.clone(), Err(format!("open: {}: the file could not be read", j.path))));
                false
            }
        });
        done
    }

    /// The text of the card: the file, the stage, how long it has taken, and how many files
    /// are queued.
    fn label(&self) -> Option<String> {
        let first = self.jobs.first()?;
        let name = std::path::Path::new(&first.path).file_name().map_or_else(|| first.path.clone(), |n| n.to_string_lossy().to_string());
        let secs = first.started.elapsed().as_secs_f32();
        let stage = if first.progress.is_cancelled() { "正在取消".to_string() } else { first.progress.stage().label().to_string() };
        let more = self.jobs.len() - 1;
        Some(if more == 0 {
            format!("正在打开 {name}…{stage} {secs:.0} 秒")
        } else {
            format!("正在打开 {name} 及另外 {more} 个…{stage} {secs:.0} 秒")
        })
    }

    /// Paint the card in the middle of the window and keep frames coming while files load.
    /// The Cancel button asks the worker to stop; the card stays up until the worker answers.
    pub fn show(&mut self, ctx: &egui::Context) {
        let Some(label) = self.label() else { return };
        let t = Tokens::get();
        let mut cancel = false;
        egui::Area::new(egui::Id::new("cc_opening")).order(egui::Order::Foreground).anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO).show(
            ctx,
            |ui| {
                egui::Frame::NONE
                    .fill(t.panel)
                    .stroke(egui::Stroke::new(1.0, t.accent))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(18, 12))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().color(t.accent));
                            ui.label(egui::RichText::new(label).color(t.text));
                            if ui.button("取消").clicked() {
                                cancel = true;
                            }
                        });
                    });
            },
        );
        if cancel {
            self.cancel_all();
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use cadcraft_engine::Session;

    use crate::{CadApp, Services};

    /// Opening starts in the background (the UI is not blocked), shows the card, and a file that
    /// cannot be read is reported like a failed OPEN without adding a drawing.
    #[test]
    fn open_path_reads_in_the_background_and_reports_failure() {
        let mut app = CadApp::new(Session::new(), Services { pick_open: None, pick_save: None });
        let before = app.session.docs.len();
        let missing = std::env::temp_dir().join(format!("cadcraft-missing-{}.dxf", std::process::id()));
        let p = missing.to_string_lossy().to_string();
        app.open_path(&p);
        assert!(app.opening.busy() || app.status.is_some(), "the read starts in the background");
        let t = Instant::now();
        while app.opening.busy() && t.elapsed() < Duration::from_secs(10) {
            app.finish_opening();
            std::thread::sleep(Duration::from_millis(5));
        }
        app.finish_opening();
        assert!(!app.opening.busy());
        assert_eq!(app.session.docs.len(), before);
        let (msg, _) = app.status.clone().unwrap_or_default();
        assert!(msg.contains(&p), "status reports the file: {msg}");
        assert!(app.session.log.iter().any(|l| l.contains(&p)));
    }

    #[test]
    fn card_text_names_the_file_and_counts_queued_ones() {
        let mut o = super::Opening::default();
        assert!(o.label().is_none());
        o.start("/nonexistent/dir/a.dxf");
        o.start("/nonexistent/dir/b.dxf");
        let l = o.label().unwrap_or_default();
        assert!(l.starts_with("正在打开 a.dxf 及另外 1 个"), "{l}");
        assert!(l.contains("正在解码 DWG") || l.contains("正在解析 DXF") || l.contains("正在构建图形"), "{l}");
        o.cancel_all();
    }

    #[test]
    fn cancel_marks_jobs_cancelled() {
        let mut o = super::Opening::default();
        o.start("/nonexistent/dir/a.dxf");
        assert!(!o.jobs.first().map(|j| j.progress.is_cancelled()).unwrap_or(true));
        o.cancel_all();
        assert!(o.jobs.first().map(|j| j.progress.is_cancelled()).unwrap_or(false));
    }
}
