//! Kill list tab and the panic button: processes that may be ended, either
//! automatically when they keep eating CPU or all at once on demand.

use crate::i18n::LocalDecimal;
use crate::i18n::PercentAffix;
use eframe::egui::{self, RichText, Ui};
use wtm_core::control::ProcessRef;
use wtm_ipc::Command;
use wtm_policy::{KillEntry, remove_kill_entry, set_kill_entry};

use crate::backend::{Data, Mode, Panel, UiCommand};
use crate::processes::Intent;

use crate::theme::{BAD, WARN, right};

/// `KillEntry::label` in the window's language. The English label stays
/// the key that running processes are matched by.
fn shown_label(e: &KillEntry) -> String {
    match &e.cmdline {
        Some(c) => crate::i18n::format("{0} running \"{1}\"", &[e.name.clone(), c.clone()]),
        None => e.name.clone(),
    }
}

/// `KillEntry::describe` in the window's language.
fn shown_when(e: &KillEntry) -> String {
    let Some(cpu) = e.cpu_above else {
        return crate::i18n::tr("only with the panic button").to_owned();
    };
    let (secs, cpu) = (e.for_secs.to_string(), format!("{cpu:.0}"));
    match e.system_cpu_above {
        Some(sys) => crate::i18n::format(
            "automatically after {0} s at {1}% CPU or more while the PC is above {2}%",
            &[secs, cpu, format!("{sys:.0}")],
        ),
        None => crate::i18n::format(
            "automatically after {0} s at {1}% CPU or more",
            &[secs, cpu],
        ),
    }
}

/// A process the panic button would end.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub process: ProcessRef,
    pub cpu: f32,
    /// The kill-list entry that matched, as a label.
    pub entry: String,
    /// Started by a matching process.
    pub child: bool,
}

/// What the panic button ends right now.
pub fn targets(data: &Data) -> Vec<Target> {
    let Some(snap) = &data.snapshot else {
        return Vec::new();
    };
    let lines = &data.command_lines;
    wtm_policy::panic_targets(&data.kill_list, &snap.processes, &mut |p| {
        lines.get(&(p.pid, p.create_time)).cloned()
    })
    .into_iter()
    .map(|t| Target {
        process: ProcessRef::from(t.process),
        cpu: t.process.cpu_percent,
        entry: data.kill_list[t.entry].label(),
        child: t.child,
    })
    .collect()
}

/// The panic confirmation: lists what will be ended. One click ends it all.
pub fn panic_dialog(ctx: &egui::Context, data: &Data, open: &mut bool, intents: &mut Vec<Intent>) {
    if !*open {
        return;
    }
    let targets = targets(data);
    let modal = egui::Modal::new(egui::Id::unique("panic"))
        .frame(crate::theme::dialog_frame(ctx))
        .show(ctx, |ui| {
        ui.set_width(620.0);
        ui.label(RichText::new(crate::i18n::tr("🚨 Panic: end the kill list")).size(22.0).color(crate::theme::text_color(ui, BAD)));
        if targets.is_empty() {
            ui.label(crate::i18n::tr("Nothing on the kill list is running right now."));
            if data.kill_list.is_empty() {
                ui.weak(crate::i18n::tr("Add programs in the Kill list tab, or right-click a process → Add to kill list…."));
            }
            if ui.button(crate::i18n::tr("Close")).clicked() {
                *open = false;
            }
            return;
        }
        ui.label({ let __wtm_i18n_0 = &(targets.len()); crate::i18n::format("These {0} processes will be ended immediately. Unsaved work in them is lost.", &[format!("{}", __wtm_i18n_0)]) });
        ui.add_space(0.0);
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            egui::Grid::new("panic_targets")
                .num_columns(4)
                .striped(true)
                .show(ui, |ui| {
                    ui.strong(crate::i18n::tr("Process"));
                    for h in ["PID", "CPU"] {
                        right(ui, RichText::new(h).strong());
                    }
                    ui.strong(crate::i18n::tr("Kill-list entry"));
                    crate::theme::end_row(ui);
                    for t in &targets {
                        let name = if t.child {
                            format!("  └ {}", t.process.name)
                        } else {
                            t.process.name.clone()
                        };
                        ui.label(name);
                        right(ui, t.process.pid.to_string());
                        right(ui, crate::i18n::percent(format!("{:.1}", t.cpu)));
                        ui.weak(&t.entry);
                        crate::theme::end_row(ui);
                    }
                });
        });
        ui.add_space(0.0);
        ui.horizontal(|ui| {
            if ui
                .button(
                    RichText::new({ let __wtm_i18n_0 = &(targets.len()); crate::i18n::format("End {0} processes now", &[format!("{}", __wtm_i18n_0)]) })
                        .strong()
                        .color(crate::theme::text_color(ui, BAD)),
                )
                .clicked()
            {
                intents.extend(
                    targets
                        .iter()
                        .map(|t| Intent::Command(t.process.clone(), Command::Kill)),
                );
                *open = false;
            }
            if ui.button(crate::i18n::tr("Cancel")).clicked() {
                *open = false;
            }
        });
    });
    if modal.should_close() {
        *open = false;
    }
}

/// The add / change form.
#[derive(Debug, Clone, PartialEq)]
pub struct KillView {
    name: String,
    cmdline: String,
    automatic: bool,
    cpu: f32,
    seconds: u64,
    busy_pc: bool,
    system_cpu: f32,
    tree: bool,
    /// The entry being changed, as it was: found again by value on save,
    /// since the list may change meanwhile (a removal, a reload).
    editing: Option<KillEntry>,
    /// The entry sent to the service; the form is kept until the list
    /// loaded afterwards has it, so a failed save loses nothing.
    saving: Option<KillEntry>,
    error: Option<String>,
}

impl Default for KillView {
    fn default() -> Self {
        Self {
            name: String::new(),
            cmdline: String::new(),
            automatic: true,
            cpu: 50.0,
            seconds: 60,
            busy_pc: false,
            system_cpu: 85.0,
            tree: true,
            editing: None,
            saving: None,
            error: None,
        }
    }
}

impl KillView {
    /// A new entry for `name` (from a process's context menu).
    pub fn prefill(&mut self, name: &str, cmdline: &str) {
        *self = Self {
            name: name.to_owned(),
            cmdline: cmdline.to_owned(),
            ..Self::default()
        };
    }

    fn edit(&mut self, e: &KillEntry) {
        *self = Self {
            name: e.name.clone(),
            cmdline: e.cmdline.clone().unwrap_or_default(),
            automatic: e.cpu_above.is_some(),
            cpu: e.cpu_above.unwrap_or(50.0),
            seconds: e.for_secs,
            busy_pc: e.system_cpu_above.is_some(),
            system_cpu: e.system_cpu_above.unwrap_or(85.0),
            tree: e.tree,
            editing: Some(e.clone()),
            saving: None,
            error: None,
        };
    }

    /// The configuration `text` with the form's entry added, or put in place
    /// of the edited one wherever it is now in `list`; `None` (with the
    /// error shown) when that entry is gone or the entry is invalid.
    fn save(&mut self, text: &str, list: &[KillEntry]) -> Option<String> {
        let index = match &self.editing {
            None => None,
            Some(original) => match list.iter().position(|e| e == original) {
                Some(i) => Some(i),
                None => {
                    self.error = Some(
                        crate::i18n::tr(
                            "This entry was changed or removed meanwhile; open it again to change it.",
                        )
                        .into(),
                    );
                    return None;
                }
            },
        };
        let entry = self.entry();
        match set_kill_entry(text, &entry, index) {
            Ok(new) => {
                // Compared with the list loaded after the save, so as the
                // file will give it back.
                self.saving = Some(entry.as_stored());
                self.error = None;
                Some(new)
            }
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }

    fn entry(&self) -> KillEntry {
        let cmdline = self.cmdline.trim();
        KillEntry {
            name: self.name.trim().to_owned(),
            cmdline: (!cmdline.is_empty()).then(|| cmdline.to_owned()),
            cpu_above: self.automatic.then_some(self.cpu),
            for_secs: self.seconds,
            system_cpu_above: (self.automatic && self.busy_pc).then_some(self.system_cpu),
            tree: self.tree,
        }
    }
}

pub fn kill_tab(ui: &mut Ui, data: &Data, view: &mut KillView, panic: &mut bool) -> Vec<UiCommand> {
    let mut commands = Vec::new();
    ui.label(crate::i18n::tr(
        "Programs that may be ended. With a CPU threshold the service ends them by itself \
         once they keep using that much and do not recover; the panic button ends everything \
         listed here at once.",
    ));
    ui.add_space(0.0);
    let running = targets(data);
    let panic_label = {
        let __wtm_i18n_0 = &(running.len());
        crate::i18n::format(
            "🚨 Panic: end {0} processes now",
            &[format!("{}", __wtm_i18n_0)],
        )
    };
    // Red only when it can be pressed: faded, red text was pale pink on
    // white in the light theme.
    let mut panic_text = RichText::new(panic_label).strong();
    if !running.is_empty() {
        panic_text = panic_text.color(crate::theme::text_color(ui, BAD));
    }
    if ui
        .add_enabled(!running.is_empty(), egui::Button::new(panic_text))
        .on_hover_text(crate::i18n::tr(
            "Shows what will be ended first (Ctrl+Shift+K)",
        ))
        .clicked()
    {
        *panic = true;
    }
    ui.add_space(0.0);

    let text = match &data.config {
        None => {
            crate::theme::loading(ui, crate::i18n::tr("Loading the service settings…"));
            return commands;
        }
        Some(Err(e)) => {
            crate::theme::colored(ui, BAD, e);
            if ui.button(crate::i18n::tr("Retry")).clicked() {
                commands.push(UiCommand::Load(Panel::Config));
            }
            return commands;
        }
        Some(Ok((_, text))) => text,
    };
    // The backend keeps an empty list when the file does not parse; that is
    // not the same as an empty kill list, as Limits says too.
    if let Err(e) = wtm_policy::Config::from_toml(text) {
        crate::theme::colored(
            ui,
            BAD,
            crate::i18n::format(
                "The configuration is invalid ({0}); fix it in Settings.",
                &[format!("{e}")],
            ),
        );
        return commands;
    }
    if view
        .saving
        .as_ref()
        .is_some_and(|s| data.kill_list.contains(s))
    {
        *view = KillView::default();
    }
    let admin = matches!(data.mode, Mode::Service { admin: true, .. });
    if data.status.as_ref().is_some_and(|s| s.dry_run) {
        crate::theme::colored(
            ui,
            WARN,
            crate::i18n::tr(
                "Monitoring only: automatic ends are only logged. The panic button always works.",
            ),
        );
    }
    if !admin {
        ui.horizontal(|ui| {
            ui.weak(crate::i18n::tr(
                "Run RustWTM as administrator to change the kill list.",
            ));
            if !data.elevated
                && ui
                    .small_button(crate::i18n::tr("Restart as administrator"))
                    .clicked()
            {
                commands.push(UiCommand::RestartElevated);
            }
        });
    }

    if !data.kill_list.is_empty() {
        crate::theme::panel(ui, |ui| {
            ui.spacing_mut().scroll = crate::theme::solid_scroll();
            let width = ui.available_width();
            egui::ScrollArea::horizontal()
                .id_salt("kill_list_columns")
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    // As wide as the panel; it scrolls only when the rows need more.
                    ui.set_min_width(width);
                    egui::Grid::new("kill_list")
                        .num_columns(5)
                        .striped(true)
                        .show(ui, |ui| {
                            for h in [
                                crate::i18n::tr("Program"),
                                crate::i18n::tr("Ended"),
                                crate::i18n::tr("Its children"),
                            ] {
                                ui.strong(h);
                            }
                            right(ui, RichText::new(crate::i18n::tr("Running now")).strong());
                            crate::theme::end_row(ui);
                            for (i, e) in data.kill_list.iter().enumerate() {
                                let label = e.label();
                                let response =
                                    ui.label(shown_label(e)).interact(egui::Sense::click());
                                crate::context_actions::menu(
                                    &response,
                                    ("kill-list", &label, e.describe(), e.tree),
                                    |ui| {
                                        crate::context_actions::copy(
                                            ui,
                                            crate::i18n::tr("Program"),
                                            || label.clone(),
                                        );
                                        if admin
                                            && crate::context_actions::action(
                                                ui,
                                                crate::i18n::tr("Edit"),
                                            )
                                        {
                                            view.edit(e);
                                        }
                                        if admin
                                            && crate::context_actions::action(
                                                ui,
                                                crate::i18n::tr("Remove"),
                                            )
                                        {
                                            match remove_kill_entry(text, i) {
                                                Ok(new) => {
                                                    commands.push(UiCommand::save_config(text, new))
                                                }
                                                Err(e) => view.error = Some(e),
                                            }
                                        }
                                    },
                                );
                                ui.label(shown_when(e));
                                ui.label(crate::i18n::tr(if e.tree { "yes" } else { "no" }));
                                let label = e.label();
                                let n = running.iter().filter(|t| t.entry == label).count();
                                right(ui, n.to_string());
                                ui.horizontal(|ui| {
                                    if crate::theme::icon_button(
                                        ui,
                                        admin,
                                        crate::theme::icon::EDIT,
                                        crate::i18n::tr("Edit"),
                                    )
                                    .clicked()
                                    {
                                        view.edit(e);
                                    }
                                    if crate::theme::icon_button(
                                        ui,
                                        admin,
                                        crate::theme::icon::DELETE,
                                        crate::i18n::tr("Remove"),
                                    )
                                    .clicked()
                                    {
                                        match remove_kill_entry(text, i) {
                                            Ok(new) => {
                                                commands.push(UiCommand::save_config(text, new))
                                            }
                                            Err(e) => view.error = Some(e),
                                        }
                                    }
                                    crate::theme::fill_row(ui);
                                });
                                crate::theme::end_row(ui);
                            }
                        });
                });
        });
    } else {
        ui.weak(crate::i18n::tr("The kill list is empty. Add a program below, or right-click a process → Add to kill list…"));
    }

    ui.add_space(0.0);
    use crate::theme::{
        icon, option_group, option_labelled, option_named_by_control, option_switch,
    };
    option_group(
        ui,
        if view.editing.is_some() {
            crate::i18n::tr("Change entry")
        } else {
            crate::i18n::tr("Add to the kill list")
        },
    );
    ui.scope(|ui| {
        option_labelled(ui, icon::LIST, crate::i18n::tr("Program"), "", |ui, id| {
            ui.add_sized(
                [280.0, ui.spacing().interact_size.y],
                egui::TextEdit::singleline(&mut view.name)
                    .hint_text(crate::i18n::tr("e.g. node.exe or python*.exe")),
            )
            .labelled_by(id);
        });
        option_labelled(
            ui,
            icon::FILTER,
            crate::i18n::tr("Command line contains"),
            "",
            |ui, id| {
                ui.add_sized(
                    [280.0, ui.spacing().interact_size.y],
                    egui::TextEdit::singleline(&mut view.cmdline).hint_text(crate::i18n::tr(
                        "optional, e.g. vitest (only those node.exe)",
                    )),
                )
                .labelled_by(id);
            },
        );
        // A share of the whole machine: one busy core is little on big PCs.
        let cpus = data
            .snapshot
            .as_ref()
            .map_or(1, |s| s.system.logical_cpus.max(1));
        let share = crate::i18n::format(
            "{0}% of this PC = {1} of its {2} logical processors (one fully busy core is {3}%)",
            &[
                format!("{:.0}", view.cpu),
                crate::i18n::decimal(format!("{:.1}", view.cpu / 100.0 * cpus as f32)),
                format!("{}", cpus),
                crate::i18n::decimal(format!("{:.1}", 100.0 / cpus as f32)),
            ],
        );
        let automatic = crate::i18n::tr("End it automatically at");
        // "50% CPU", or "%50 CPU" where the sign goes first.
        let (before, after) = crate::i18n::percent_affixes();
        option_named_by_control(ui, icon::SPEED, automatic, &share, |ui| {
            ui.add_enabled(
                view.automatic,
                egui::DragValue::new(&mut view.cpu)
                    .local_decimal()
                    .range(1.0..=100.0)
                    .clamp_existing_to_range(false)
                    .prefix(before)
                    .suffix(format!("{after} CPU")),
            );
            ui.label(crate::i18n::tr("for"));
            ui.add_enabled(
                view.automatic,
                egui::DragValue::new(&mut view.seconds)
                    .range(1..=3600)
                    .clamp_existing_to_range(false)
                    .suffix(crate::i18n::tr(" s")),
            );
            ui.add_space(8.0);
            ui.label(crate::i18n::tr(if view.automatic { "On" } else { "Off" }));
            crate::theme::switch(ui, &mut view.automatic, automatic);
        });
        ui.add_enabled_ui(view.automatic, |ui| {
            let busy = crate::i18n::tr("only while the PC is above");
            option_named_by_control(ui, icon::CHART, busy, "", |ui| {
                ui.add_enabled(
                    view.busy_pc,
                    egui::DragValue::new(&mut view.system_cpu)
                        .local_decimal()
                        .range(1.0..=100.0)
                        .clamp_existing_to_range(false)
                        .percent_affix(),
                );
                ui.add_space(8.0);
                ui.label(crate::i18n::tr(if view.busy_pc { "On" } else { "Off" }));
                crate::theme::switch(ui, &mut view.busy_pc, busy);
            });
        });
        option_switch(
            ui,
            icon::PEOPLE,
            crate::i18n::tr("Also end the processes it started"),
            "",
            &mut view.tree,
        );

        let entry = view.entry();
        if !entry.name.is_empty()
            && let Some(snap) = &data.snapshot
        {
            let lines = &data.command_lines;
            let matching: Vec<String> = snap
                .processes
                .iter()
                .filter(|p| entry.matches(p, &mut |p| lines.get(&(p.pid, p.create_time)).cloned()))
                .map(|p| format!("{} ({})", p.name, p.pid))
                .collect();
            ui.weak(if matching.is_empty() {
                crate::i18n::tr("Matches no running process right now.").to_owned()
            } else {
                {
                    let __wtm_i18n_0 = &(matching.len());
                    let __wtm_i18n_1 = &(matching.join(", "));
                    crate::i18n::format(
                        "Matches {0} running now: {1}",
                        &[format!("{}", __wtm_i18n_0), __wtm_i18n_1.to_string()],
                    )
                }
            });
        }
        ui.horizontal_wrapped(|ui| {
            let label = if view.editing.is_some() {
                crate::i18n::tr("Save entry")
            } else {
                crate::i18n::tr("Add entry")
            };
            if crate::theme::primary_action(ui, admin, label).clicked()
                && let Some(new) = view.save(text, &data.kill_list)
            {
                commands.push(UiCommand::save_config(text, new));
            }
            if view.editing.is_some() && ui.button(crate::i18n::tr("Cancel")).clicked() {
                *view = KillView::default();
            }
        });
        if let Some(e) = &view.error {
            crate::theme::colored(ui, BAD, e);
        }
    });
    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shown_text_matches_the_policy_s_words_and_is_translated() {
        let entries = [
            KillEntry {
                name: "python.exe".into(),
                ..KillEntry::default()
            },
            KillEntry {
                name: "node.exe".into(),
                cmdline: Some("vitest".into()),
                cpu_above: Some(50.0),
                for_secs: 60,
                ..KillEntry::default()
            },
            KillEntry {
                name: "node.exe".into(),
                cpu_above: Some(50.0),
                for_secs: 60,
                system_cpu_above: Some(85.0),
                ..KillEntry::default()
            },
        ];
        {
            let _language = crate::i18n::scope(crate::i18n::Language::English);
            for e in &entries {
                assert_eq!(shown_label(e), e.label());
                assert_eq!(shown_when(e), e.describe());
            }
        }
        let _language = crate::i18n::scope(crate::i18n::Language::Turkish);
        assert_eq!(shown_label(&entries[1]), "\"vitest\" çalıştıran node.exe");
        assert_eq!(shown_when(&entries[0]), "yalnızca acil düğmesiyle");
        assert_eq!(
            shown_when(&entries[2]),
            "bilgisayar %85 üzerindeyken, %50 veya üzeri CPU'da 60 sn sonra otomatik"
        );
    }

    fn entry(name: &str) -> KillEntry {
        KillEntry {
            name: name.into(),
            cmdline: None,
            cpu_above: Some(50.0),
            for_secs: 60,
            system_cpu_above: None,
            tree: true,
        }
    }

    fn config(entries: &[KillEntry]) -> String {
        entries.iter().fold(String::new(), |text, e| {
            set_kill_entry(&text, e, None).unwrap()
        })
    }

    fn names(text: &str) -> Vec<String> {
        wtm_policy::Config::from_toml(text)
            .unwrap()
            .kill
            .into_iter()
            .map(|e| e.name)
            .collect()
    }

    #[test]
    fn the_edited_entry_is_found_by_value_wherever_it_moved() {
        let list = [entry("a.exe"), entry("b.exe"), entry("c.exe")];
        let mut view = KillView::default();
        view.edit(&list[1]);
        view.name = "b2.exe".into();
        // a.exe was removed meanwhile: b.exe is now first.
        let now = [entry("b.exe"), entry("c.exe")];
        let new = view.save(&config(&now), &now).unwrap();
        assert_eq!(names(&new), ["b2.exe", "c.exe"], "c.exe is not overwritten");

        // The entry itself is gone: nothing is saved, nothing is lost.
        let mut view = KillView::default();
        view.edit(&list[1]);
        let gone = [entry("a.exe"), entry("c.exe")];
        assert!(view.save(&config(&gone), &gone).is_none());
        assert!(view.error.is_some());
        assert_eq!(view.name, "b.exe", "the form is kept");
    }

    #[test]
    fn the_form_stays_until_the_saved_entry_is_loaded() {
        let mut view = KillView {
            name: "new.exe".into(),
            ..KillView::default()
        };
        assert!(view.save(&config(&[]), &[]).is_some());
        assert_eq!(view.saving, Some(entry("new.exe")));
        assert_eq!(view.name, "new.exe", "kept while the save is under way");
    }

    #[test]
    fn an_invalid_config_is_not_shown_as_an_empty_list() {
        use egui_kittest::{Harness, kittest::Queryable};
        // A typo in config.toml: the kill list cannot be read.
        let data = Data {
            config: Some(Ok((
                "config.toml".into(),
                "[[kill]]\nname = \"a.exe\"\ntree = maybe\n".into(),
            ))),
            ..Data::default()
        };
        let mut view = KillView::default();
        let mut h = Harness::builder()
            .allow_missing_glyphs()
            .with_size([1000.0, 800.0])
            .build_ui(move |ui| {
                let mut panic = false;
                let _ = kill_tab(ui, &data, &mut view, &mut panic);
            });
        h.run_steps(2);
        assert!(
            h.query_by_label_contains("The kill list is empty")
                .is_none(),
            "the entries are still in the file"
        );
        assert!(
            h.query_by_label_contains("The configuration is invalid")
                .is_some()
        );
        assert!(h.query_by_label("Add entry").is_none(), "nothing to add to");
    }

    #[test]
    fn a_panic_only_entry_is_recognised_once_saved() {
        // Automatic ending off, the wait left at something other than the
        // default: config.toml keeps no wait for such an entry.
        let mut view = KillView {
            name: "tool.exe".into(),
            automatic: false,
            seconds: 30,
            ..KillView::default()
        };
        let new = view.save(&config(&[]), &[]).unwrap();
        let reloaded = wtm_policy::Config::from_toml(&new).unwrap().kill;
        assert!(
            view.saving.as_ref().is_some_and(|s| reloaded.contains(s)),
            "the form must see its entry in the reloaded list, or Add entry adds it again: {:?} vs {reloaded:?}",
            view.saving
        );
    }
}
