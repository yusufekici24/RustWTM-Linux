//! The process inspector's Handles panel: the files, registry keys and
//! other objects a process has open, as Process Explorer's handle view
//! lists them. Read once on request with this window's own rights, off the
//! UI thread, where the rows are also sorted and counted.

use eframe::egui::{self, RichText, Ui};
use wtm_core::control::{HandleInfo, HandleReport, ProcessRef};

use crate::backend::Data;
use crate::processes::Intent;
use crate::theme::BAD;

/// A read handle list, sorted and counted for drawing.
#[derive(Debug, Clone, Default)]
pub struct HandleList {
    pub total: usize,
    /// By type, then name; unnamed handles after the named ones of their
    /// type.
    pub rows: Vec<HandleInfo>,
    /// Each row's type and name in lower case, for the filter: made once
    /// here rather than for every row in every frame.
    pub keys: Vec<String>,
    /// Handle types with their counts, most common first.
    pub kinds: Vec<(String, usize)>,
    pub can_name: bool,
    pub names_incomplete: bool,
    pub too_many: bool,
}

impl From<HandleReport> for HandleList {
    fn from(report: HandleReport) -> Self {
        let too_many = report.too_many();
        let mut rows = report.handles;
        rows.sort_by_cached_key(|h| {
            (
                h.kind.to_lowercase(),
                h.name.is_none(),
                h.name.as_deref().map(str::to_lowercase),
                h.value,
            )
        });
        let mut kinds: Vec<(String, usize)> = Vec::new();
        for h in &rows {
            match kinds.last_mut() {
                Some((kind, n)) if *kind == h.kind => *n += 1,
                _ => kinds.push((h.kind.clone(), 1)),
            }
        }
        kinds.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let keys = rows
            .iter()
            .map(|h| {
                format!("{}\n{}", h.kind, h.name.as_deref().unwrap_or_default()).to_lowercase()
            })
            .collect();
        Self {
            total: report.total,
            rows,
            keys,
            kinds,
            can_name: report.can_name,
            names_incomplete: report.names_incomplete,
            too_many,
        }
    }
}

/// Rows drawn at most; a busy process holds thousands of handles.
const SHOWN_HANDLES: usize = 500;

/// Draws the handles of `sel` once they have been asked for.
pub fn show(ui: &mut Ui, data: &Data, sel: &ProcessRef, intents: &mut Vec<Intent>) -> Option<()> {
    let (_, read) = data.handles.as_ref().filter(|(r, _)| r == sel)?;
    ui.horizontal_wrapped(|ui| {
        ui.strong(crate::i18n::tr("Handles"));
        if let Some(Ok(list)) = read {
            ui.weak(crate::i18n::format(
                "{0} handles",
                &[list.total.to_string()],
            ));
        }
        if ui.small_button(crate::i18n::tr("Hide")).clicked() {
            intents.push(Intent::Handles(None));
        }
    });
    match read {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(crate::i18n::tr("Reading handles…"));
            });
        }
        Some(Err(e)) => {
            crate::theme::colored(ui, BAD, e);
        }
        Some(Ok(list)) => list_view(ui, sel, list),
    }
    Some(())
}

fn list_view(ui: &mut Ui, sel: &ProcessRef, list: &HandleList) {
    if list.too_many {
        crate::theme::colored(
            ui,
            BAD,
            crate::i18n::format(
                "It holds {0} handles, too many to list.",
                &[list.total.to_string()],
            ),
        );
        return;
    }
    if !list.can_name {
        ui.weak(crate::i18n::tr(
            "Names need administrator rights for this process.",
        ));
    } else if list.names_incomplete {
        ui.weak(crate::i18n::tr(
            "Some names were not read: too many handles, or a file did not answer in time.",
        ));
    }
    ui.horizontal_wrapped(|ui| {
        for (kind, n) in list.kinds.iter().take(8) {
            ui.weak(RichText::new(format!("{kind} {n}")).small());
        }
    });

    // Per process: a filter typed for one process does not hide the next
    // one's handles.
    let filter_id = egui::Id::unique(("handles_filter", sel.pid, sel.create_time));
    let unnamed_id = egui::Id::unique("handles_unnamed");
    let mut filter: String = ui.data(|d| d.get_temp(filter_id)).unwrap_or_default();
    // As in Process Explorer, only named handles unless asked; without
    // names, every handle.
    let mut unnamed: bool = ui.data(|d| d.get_temp(unnamed_id)).unwrap_or(false);
    ui.horizontal_wrapped(|ui| {
        let label = crate::i18n::tr("Filter by type or name");
        let edit = ui.add(
            egui::TextEdit::singleline(&mut filter)
                .char_limit(256)
                .hint_text(label)
                .desired_width(180.0),
        );
        edit.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, label));
        if list.can_name {
            ui.checkbox(&mut unnamed, crate::i18n::tr("Show unnamed handles"));
        }
    });
    ui.data_mut(|d| {
        d.insert_temp(filter_id, filter.clone());
        d.insert_temp(unnamed_id, unnamed);
    });
    let unnamed = unnamed || !list.can_name;

    let needle = filter.trim().to_lowercase();
    let matching: Vec<&HandleInfo> = list
        .rows
        .iter()
        .zip(&list.keys)
        .filter(|(h, key)| {
            (unnamed || h.name.is_some()) && (needle.is_empty() || key.contains(&needle))
        })
        .map(|(h, _)| h)
        .collect();
    if matching.is_empty() && !needle.is_empty() {
        ui.weak(crate::i18n::tr("Nothing matches the filter."));
        return;
    }
    if matching.len() > SHOWN_HANDLES {
        ui.weak(crate::i18n::format(
            "Showing the first {0} of {1}.",
            &[SHOWN_HANDLES.to_string(), matching.len().to_string()],
        ));
    }
    egui::ScrollArea::horizontal()
        .id_salt("handles_scroll")
        .show(ui, |ui| {
            egui::Grid::new("handles_grid")
                .num_columns(3)
                .striped(true)
                .spacing(egui::vec2(12.0, 2.0))
                .show(ui, |ui| {
                    for h in matching.into_iter().take(SHOWN_HANDLES) {
                        // The name last: paths are long and scroll sideways.
                        ui.label(RichText::new(&h.kind).small());
                        ui.label(
                            RichText::new(format!("0x{:X}", h.value))
                                .small()
                                .monospace(),
                        )
                        .on_hover_text(crate::i18n::format(
                            "Access: {0}",
                            &[format!("0x{:08X}", h.access)],
                        ));
                        match &h.name {
                            Some(name) => {
                                let shown = ui.label(RichText::new(name).small().monospace());
                                crate::context_actions::copy_text(
                                    &shown,
                                    ("process-handle", h.value),
                                    crate::i18n::tr("Name"),
                                    name,
                                );
                            }
                            None => {
                                ui.weak("—");
                            }
                        }
                        ui.end_row();
                    }
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(kind: &str, name: Option<&str>, value: u64) -> HandleInfo {
        HandleInfo {
            value,
            kind: kind.into(),
            name: name.map(Into::into),
            access: 0,
        }
    }

    #[test]
    fn a_filter_belongs_to_its_process() {
        use egui_kittest::{Harness, kittest::Queryable};
        let list = HandleList::from(HandleReport {
            total: 2,
            handles: vec![handle("Event", None, 0x18), handle("File", None, 0x1C)],
            can_name: false,
            names_incomplete: false,
        });
        let process = |pid| ProcessRef {
            pid,
            create_time: 1,
            name: "a.exe".into(),
        };
        let mut h = Harness::builder()
            .allow_missing_glyphs()
            .with_size([600.0, 500.0])
            .build_ui_state(
                move |ui, sel: &mut ProcessRef| list_view(ui, sel, &list),
                process(100),
            );
        h.run_steps(2);
        h.get_by_label("Filter by type or name").click();
        h.run_steps(1);
        h.get_by_label("Filter by type or name").type_text("zzz");
        h.run_steps(2);
        assert!(h.query_by_label("Nothing matches the filter.").is_some());
        // Another process: its handles are not hidden by that filter.
        *h.state_mut() = process(200);
        h.run_steps(2);
        assert!(h.query_by_label("0x18").is_some() && h.query_by_label("0x1C").is_some());
    }

    #[test]
    fn rows_sort_by_type_then_name_and_types_are_counted() {
        let list = HandleList::from(HandleReport {
            total: 5,
            handles: vec![
                handle("Key", Some(r"HKLM\b"), 8),
                handle("Event", None, 4),
                handle("Key", None, 12),
                handle("Key", Some(r"HKLM\A"), 16),
                handle("File", Some(r"C:\x"), 20),
            ],
            can_name: true,
            names_incomplete: false,
        });
        let order: Vec<u64> = list.rows.iter().map(|h| h.value).collect();
        assert_eq!(order, [4, 20, 16, 8, 12]);
        assert_eq!(
            list.kinds,
            [("Key".into(), 3), ("Event".into(), 1), ("File".into(), 1)]
        );
        assert!(!list.too_many);
    }
}
