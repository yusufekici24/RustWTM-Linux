//! The process inspector's Windows panel: a process's top-level windows on
//! this desktop, as System Informer's Windows tab lists them, with Task
//! Manager's "Switch to". Read once on request off the UI thread; titles
//! often name documents, mails or web pages, so they are only shown, never
//! logged.

use eframe::egui::{self, RichText, Ui};
use wtm_core::control::{ProcessRef, SwitchOutcome, WindowInfo};

use crate::backend::Data;
use crate::processes::Intent;
use crate::theme::{BAD, WARN};

/// A read window list, with the outcome of the last "Switch to". `Debug`
/// gives counts only: titles stay out of logs.
#[derive(Clone, Default)]
pub struct WindowList {
    /// Visible windows first, each group in Z order (topmost first).
    pub rows: Vec<WindowInfo>,
    /// Each row's title and class in lower case, for the filter.
    pub keys: Vec<String>,
    pub visible: usize,
    pub switched: Option<SwitchOutcome>,
}

impl From<Vec<WindowInfo>> for WindowList {
    fn from(mut rows: Vec<WindowInfo>) -> Self {
        // Stable: Z order is kept within each group.
        rows.sort_by_key(|w| !w.visible);
        let keys = rows
            .iter()
            .map(|w| format!("{}\n{}", w.title, w.class).to_lowercase())
            .collect();
        Self {
            visible: rows.iter().filter(|w| w.visible).count(),
            rows,
            keys,
            switched: None,
        }
    }
}

impl std::fmt::Debug for WindowList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowList")
            .field("windows", &self.rows.len())
            .field("visible", &self.visible)
            .field("switched", &self.switched)
            .finish()
    }
}

/// Rows drawn at most; Explorer has hundreds of hidden windows.
const SHOWN_WINDOWS: usize = 500;

/// Draws the windows of `sel` once they have been asked for.
pub fn show(ui: &mut Ui, data: &Data, sel: &ProcessRef, intents: &mut Vec<Intent>) -> Option<()> {
    let (_, read) = data.windows.as_ref().filter(|(r, _)| r == sel)?;
    ui.horizontal_wrapped(|ui| {
        ui.strong(crate::i18n::tr("Windows"));
        if let Some(Ok(list)) = read {
            ui.weak(crate::i18n::format_count(
                "{0} window, {1} visible",
                "{0} windows, {1} visible",
                list.rows.len(),
                &[list.rows.len().to_string(), list.visible.to_string()],
            ));
        }
        if ui.small_button(crate::i18n::tr("Hide")).clicked() {
            intents.push(Intent::Windows(None));
        }
    });
    match read {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(crate::i18n::tr("Reading windows…"));
            });
        }
        Some(Err(e)) => {
            crate::theme::colored(ui, BAD, e);
        }
        Some(Ok(list)) => list_view(ui, sel, list, intents),
    }
    Some(())
}

fn list_view(ui: &mut Ui, sel: &ProcessRef, list: &WindowList, intents: &mut Vec<Intent>) {
    match list.switched {
        Some(SwitchOutcome::Refused) => {
            crate::theme::colored(
                ui,
                WARN,
                crate::i18n::tr("Windows kept the focus on another window."),
            );
        }
        Some(SwitchOutcome::Gone) => {
            ui.weak(crate::i18n::tr("That window has closed."));
        }
        Some(SwitchOutcome::Focused) | None => {}
    }
    if list.rows.is_empty() {
        ui.weak(crate::i18n::tr(
            "No windows on this desktop. A service or another user's program has none here.",
        ));
        return;
    }

    // Per process, as the Handles filter.
    let filter_id = egui::Id::unique(("windows_filter", sel.pid, sel.create_time));
    let hidden_id = egui::Id::unique("windows_hidden");
    let mut filter: String = ui.data(|d| d.get_temp(filter_id)).unwrap_or_default();
    let mut hidden: bool = ui.data(|d| d.get_temp(hidden_id)).unwrap_or(false);
    ui.horizontal_wrapped(|ui| {
        let label = crate::i18n::tr("Filter by title or class");
        let edit = ui.add(
            egui::TextEdit::singleline(&mut filter)
                .char_limit(256)
                .hint_text(label)
                .desired_width(180.0),
        );
        edit.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, label));
        if list.visible < list.rows.len() {
            ui.checkbox(&mut hidden, crate::i18n::tr("Show hidden windows"));
        }
    });
    ui.data_mut(|d| {
        d.insert_temp(filter_id, filter.clone());
        d.insert_temp(hidden_id, hidden);
    });

    let needle = filter.trim().to_lowercase();
    let matching: Vec<&WindowInfo> = list
        .rows
        .iter()
        .zip(&list.keys)
        .filter(|(w, key)| (hidden || w.visible) && (needle.is_empty() || key.contains(&needle)))
        .map(|(w, _)| w)
        .collect();
    if matching.is_empty() && needle.is_empty() {
        ui.weak(crate::i18n::tr("No visible windows."));
        return;
    }
    if matching.is_empty() && !needle.is_empty() {
        ui.weak(crate::i18n::tr("Nothing matches the filter."));
        return;
    }
    if matching.len() > SHOWN_WINDOWS {
        ui.weak(crate::i18n::format(
            "Showing the first {0} of {1}.",
            &[SHOWN_WINDOWS.to_string(), matching.len().to_string()],
        ));
    }
    egui::ScrollArea::horizontal()
        .id_salt("windows_scroll")
        .show(ui, |ui| {
            egui::Grid::new("windows_grid")
                .num_columns(3)
                .striped(true)
                .spacing(egui::vec2(12.0, 2.0))
                .show(ui, |ui| {
                    for w in matching.into_iter().take(SHOWN_WINDOWS) {
                        // The action first: titles are long and scroll
                        // sideways.
                        if w.visible {
                            if ui
                                .small_button(crate::i18n::tr("Switch to"))
                                .on_hover_text(crate::i18n::tr(
                                    "Restore it if minimized and bring it to the front",
                                ))
                                .clicked()
                            {
                                intents.push(Intent::SwitchToWindow(sel.clone(), w.handle));
                            }
                        } else {
                            ui.weak(RichText::new(crate::i18n::tr("Hidden")).small());
                        }
                        // Unwrapped: a long title scrolls sideways rather
                        // than folding into a narrow column.
                        ui.horizontal(|ui| {
                            if w.title.is_empty() {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(crate::i18n::tr("(no title)")).small().weak(),
                                    )
                                    .extend(),
                                );
                            } else {
                                let shown = ui.add(
                                    egui::Label::new(RichText::new(&w.title).small()).extend(),
                                );
                                crate::context_actions::copy_text(
                                    &shown,
                                    ("process-window", w.handle),
                                    crate::i18n::tr("Title"),
                                    &w.title,
                                );
                            }
                            // Hidden windows of suspended apps read as hung
                            // too; as in Task Manager, only shown ones count.
                            if w.hung && w.visible {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(crate::i18n::tr("Not responding"))
                                            .small()
                                            .color(crate::theme::text_color(ui, BAD)),
                                    )
                                    .extend(),
                                );
                            }
                        });
                        ui.add(
                            egui::Label::new(RichText::new(&w.class).small().monospace().weak())
                                .extend(),
                        )
                        .on_hover_text(crate::i18n::format("Thread {0}", &[w.tid.to_string()]));
                        ui.end_row();
                    }
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(handle: isize, title: &str, visible: bool) -> WindowInfo {
        WindowInfo {
            handle,
            tid: 7,
            title: title.into(),
            class: "Notepad".into(),
            visible,
            ..WindowInfo::default()
        }
    }

    #[test]
    fn visible_windows_come_first_in_z_order() {
        let list = WindowList::from(vec![
            window(1, "Hidden helper", false),
            window(2, "Front", true),
            window(3, "", false),
            window(4, "Back", true),
        ]);
        let order: Vec<isize> = list.rows.iter().map(|w| w.handle).collect();
        assert_eq!(order, [2, 4, 1, 3]);
        assert_eq!(list.visible, 2);
        assert_eq!(list.keys[0], "front\nnotepad");
        // Titles stay out of logs.
        let debug = format!("{list:?}");
        assert!(!debug.to_lowercase().contains("front"), "{debug}");
        assert!(debug.contains("windows: 4") && debug.contains("visible: 2"));
    }
}
