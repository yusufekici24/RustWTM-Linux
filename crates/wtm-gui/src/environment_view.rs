//! The process inspector's Environment panel: a process's environment
//! variables, as Process Explorer's Environment tab lists them. Read once on
//! request with this window's own rights, never through the service, and
//! never written anywhere: variables often hold secrets.

use eframe::egui::{self, RichText, Ui};
use wtm_core::control::ProcessRef;

use crate::backend::Data;
use crate::processes::Intent;
use crate::theme::BAD;

/// Draws the variables of `sel` once they have been asked for; those of
/// another process are not shown.
pub fn show(ui: &mut Ui, data: &Data, sel: &ProcessRef, intents: &mut Vec<Intent>) -> Option<()> {
    let (_, read) = data.environment.as_ref().filter(|(r, _)| r == sel)?;
    ui.horizontal_wrapped(|ui| {
        ui.strong(crate::i18n::tr("Environment"));
        if let Some(Ok(vars)) = read {
            ui.weak(crate::i18n::format(
                "{0} variables",
                &[vars.0.len().to_string()],
            ));
            if ui.small_button(crate::i18n::tr("Copy all")).clicked() {
                ui.ctx().copy_text(as_text(&sorted(&vars.0)));
            }
        }
        if ui.small_button(crate::i18n::tr("Hide")).clicked() {
            intents.push(Intent::Environment(None));
        }
    });
    match read {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(crate::i18n::tr("Reading environment variables…"));
            });
        }
        Some(Err(e)) => {
            crate::theme::colored(ui, BAD, e);
        }
        Some(Ok(vars)) => list(ui, sel, &vars.0),
    }
    Some(())
}

/// By name, ignoring case.
fn sorted(vars: &[(String, String)]) -> Vec<&(String, String)> {
    let mut sorted: Vec<&(String, String)> = vars.iter().collect();
    sorted.sort_by_cached_key(|(name, _)| name.to_lowercase());
    sorted
}

fn list(ui: &mut Ui, sel: &ProcessRef, vars: &[(String, String)]) {
    let sorted = sorted(vars);
    // Drawn in full, as the inspector scrolls; only sideways does it scroll.
    egui::ScrollArea::horizontal()
        .id_salt("environment_scroll")
        .show(ui, |ui| {
            egui::Grid::new("environment_grid")
                .num_columns(2)
                .striped(true)
                .spacing(egui::vec2(12.0, 2.0))
                .show(ui, |ui| {
                    for (name, value) in sorted {
                        ui.label(RichText::new(name).small().strong());
                        let shown = ui.label(RichText::new(shortened(value)).small().monospace());
                        let shown = if value.chars().count() > SHOWN_CHARS {
                            shown.on_hover_text(RichText::new(value).small().monospace())
                        } else {
                            shown
                        };
                        crate::context_actions::copy_text(
                            &shown,
                            ("process-environment", sel.pid, sel.create_time, name),
                            name,
                            value,
                        );
                        ui.end_row();
                    }
                });
        });
}

/// Characters of a value shown in the list; the rest is on hover.
const SHOWN_CHARS: usize = 60;

fn shortened(value: &str) -> std::borrow::Cow<'_, str> {
    if value.chars().count() <= SHOWN_CHARS {
        return value.into();
    }
    let head: String = value.chars().take(SHOWN_CHARS - 1).collect();
    format!("{head}…").into()
}

/// `NAME=value` lines, as `set` prints them.
fn as_text(vars: &[&(String, String)]) -> String {
    vars.iter()
        .map(|(name, value)| format!("{name}={value}\n"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_values_are_cut_and_copied_whole() {
        assert_eq!(shortened("short"), "short");
        let path = "C:\\Windows;".repeat(10);
        let cut = shortened(&path);
        assert_eq!(cut.chars().count(), SHOWN_CHARS);
        assert!(cut.ends_with('…'));
        let a = ("A".to_owned(), "1".to_owned());
        let p = ("PATH".to_owned(), path.clone());
        assert_eq!(as_text(&[&a, &p]), format!("A=1\nPATH={path}\n"));
    }
}
