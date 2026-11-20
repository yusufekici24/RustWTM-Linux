//! "Find handle or DLL": a window that searches every process for a file,
//! key or other object it has open, or a DLL it has loaded, as Process
//! Explorer's Find does. The search runs on a worker thread, one process
//! at a time, and its hits arrive here process by process.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use eframe::egui::{self, RichText};
use wtm_core::control::ProcessRef;
use wtm_core::find::{FindHit, FindProgress, HitKind, SHORTEST_QUERY};

use crate::backend::Data;
use crate::theme::BAD;

/// A search in progress or finished, kept in [`Data`].
#[derive(Debug, Default)]
pub struct FindState {
    /// Each search gets the next number; a late worker of an earlier one
    /// adds nothing.
    pub generation: u64,
    pub query: String,
    pub hits: Vec<FindHit>,
    pub progress: FindProgress,
    /// Set to stop the search between processes.
    pub cancel: Arc<AtomicBool>,
}

/// The window's own state.
#[derive(Debug, Default)]
pub struct FindWindow {
    pub open: bool,
    pub query: String,
    /// Said under the results, such as that a clicked process has exited.
    pub note: Option<&'static str>,
}

/// What the window asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindAction {
    Search(String),
    Stop,
    /// Show this process in the inspector, if it is still running.
    Select(ProcessRef),
}

/// Hits drawn at most; the search keeps up to its own cap.
const SHOWN_HITS: usize = 500;

pub fn window(ctx: &egui::Context, w: &mut FindWindow, data: &Data) -> Option<FindAction> {
    if !w.open {
        return None;
    }
    let mut action = None;
    let state = data.find.as_ref();
    let running = state.is_some_and(|s| !s.progress.done);
    let modal = egui::Modal::new(egui::Id::unique("find_handle_or_dll"))
        .frame(crate::theme::dialog_frame(ctx))
        .show(ctx, |ui| {
        ui.set_width(ui.available_width().min(720.0));
        ui.heading(crate::i18n::tr("Find handle or DLL"));
        ui.weak(crate::i18n::tr(
            "Processes with a file, key or other object open, or a DLL loaded, whose name contains the text.",
        ));
        ui.horizontal(|ui| {
            let label = crate::i18n::tr("Text in a file, key, object or DLL name");
            let edit = ui.add(
                egui::TextEdit::singleline(&mut w.query)
                    .char_limit(256)
                    .hint_text(label)
                    .desired_width(320.0),
            );
            edit.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, label));
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let long_enough = w.query.trim().chars().count() >= SHORTEST_QUERY;
            if running {
                if ui.button(crate::i18n::tr("Stop")).clicked() {
                    action = Some(FindAction::Stop);
                }
            } else if ui
                .add_enabled(long_enough, egui::Button::new(crate::i18n::tr("Search")))
                .on_disabled_hover_text(crate::i18n::tr("Type 3 or more characters."))
                .clicked()
                || (enter && long_enough)
            {
                w.note = None;
                action = Some(FindAction::Search(w.query.trim().to_owned()));
            }
        });
        if let Some(s) = state {
            progress(ui, s, running);
            if let Some(clicked) = results(ui, &s.hits) {
                action = Some(FindAction::Select(clicked));
            }
        }
        if let Some(note) = w.note {
            crate::theme::colored(ui, BAD, crate::i18n::tr(note));
        }
        ui.separator();
        if ui.button(crate::i18n::tr("Close")).clicked() {
            w.open = false;
        }
    });
    if modal.should_close() {
        w.open = false;
    }
    action
}

fn progress(ui: &mut egui::Ui, s: &FindState, running: bool) {
    let p = &s.progress;
    ui.horizontal_wrapped(|ui| {
        if running {
            ui.spinner();
        }
        // The box may since hold other text.
        ui.strong(crate::i18n::format(
            "Results for \"{0}\"",
            std::slice::from_ref(&s.query),
        ));
        ui.label(crate::i18n::format(
            "Searched {0} of {1} processes",
            &[p.searched.to_string(), p.total.to_string()],
        ));
        ui.label(crate::i18n::format("{0} hits", &[p.hits.to_string()]));
    });
    // What was not searched, so that no hits never claims too much.
    if p.unreadable > 0 {
        ui.weak(crate::i18n::format(
            "{0} could not be opened; run as administrator to search them too.",
            &[p.unreadable.to_string()],
        ));
    }
    if p.incomplete > 0 {
        ui.weak(crate::i18n::format(
            "Names were not fully read in {0} processes.",
            &[p.incomplete.to_string()],
        ));
    }
    if p.capped {
        ui.weak(crate::i18n::format(
            "Stopped at {0} hits.",
            &[p.hits.to_string()],
        ));
    }
    if p.timed_out {
        ui.weak(crate::i18n::tr("Stopped after a minute."));
    }
    // Only a search that reached every process may say nothing matched.
    if p.done && p.hits == 0 && p.total > 0 && p.searched == p.total {
        ui.label(crate::i18n::tr("No process has a matching handle or DLL."));
    }
}

/// The hits; returns the process of a clicked one.
fn results(ui: &mut egui::Ui, hits: &[FindHit]) -> Option<ProcessRef> {
    if hits.is_empty() {
        return None;
    }
    if hits.len() > SHOWN_HITS {
        ui.weak(crate::i18n::format(
            "Showing the first {0} of {1}.",
            &[SHOWN_HITS.to_string(), hits.len().to_string()],
        ));
    }
    let mut clicked = None;
    // The window has a fixed height, so the list scrolls inside it.
    egui::ScrollArea::both()
        .id_salt("find_results")
        .max_height(360.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            egui::Grid::new("find_grid")
                .num_columns(3)
                .striped(true)
                .spacing(egui::vec2(12.0, 2.0))
                .show(ui, |ui| {
                    ui.weak(crate::i18n::tr("Process"));
                    ui.weak(crate::i18n::tr("Type"));
                    ui.weak(crate::i18n::tr("Name"));
                    ui.end_row();
                    for hit in hits.iter().take(SHOWN_HITS) {
                        let label = format!("{} ({})", hit.process.name, hit.process.pid);
                        if ui
                            .link(RichText::new(label).small())
                            .on_hover_text(crate::i18n::tr("Show in the inspector"))
                            .clicked()
                        {
                            clicked = Some(hit.process.clone());
                        }
                        let kind = match &hit.kind {
                            HitKind::Dll => "DLL",
                            HitKind::Handle(kind) => kind.as_str(),
                        };
                        ui.label(RichText::new(kind).small());
                        let name = ui.label(RichText::new(&hit.name).small().monospace());
                        crate::context_actions::copy_text(
                            &name,
                            ("find-hit", hit.process.pid, &hit.name),
                            crate::i18n::tr("Name"),
                            &hit.name,
                        );
                        ui.end_row();
                    }
                });
        });
    clicked
}

/// Whether `target` (by ID and start time) is in the current snapshot.
pub fn still_running(data: &Data, target: &ProcessRef) -> bool {
    data.snapshot.as_ref().is_some_and(|s| {
        s.processes
            .iter()
            .any(|p| p.pid == target.pid && p.create_time == target.create_time)
    })
}
