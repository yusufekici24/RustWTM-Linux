//! The App power page: `[[while_running]]` entries switch the power plan
//! and keep the computer awake while their apps run.

use eframe::egui::{self, RichText, Ui};
use wtm_policy::{Config, KeepAwake, WhileRunning, remove_while_running, set_while_running};

use crate::backend::{Data, Mode, Panel, UiCommand};
use crate::theme::{BAD, WARN};

/// What the keep-awake choice offers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Awake {
    #[default]
    Off,
    System,
    Display,
}

impl From<Option<KeepAwake>> for Awake {
    fn from(k: Option<KeepAwake>) -> Self {
        match k {
            None => Self::Off,
            Some(KeepAwake::System) => Self::System,
            Some(KeepAwake::Display) => Self::Display,
        }
    }
}

impl From<Awake> for Option<KeepAwake> {
    fn from(a: Awake) -> Self {
        match a {
            Awake::Off => None,
            Awake::System => Some(KeepAwake::System),
            Awake::Display => Some(KeepAwake::Display),
        }
    }
}

#[derive(Default)]
pub struct AppPowerView {
    name: String,
    /// Image-name wildcards, comma-separated.
    apps: String,
    /// The plan's GUID; empty: the plan is not changed.
    plan: String,
    awake: Awake,
    /// The entry being edited, replaced on save; its hold is kept.
    editing: Option<(String, u64)>,
    /// The entry sent to the service; the form is kept until the
    /// configuration loaded afterwards has it.
    saving: Option<WhileRunning>,
    error: Option<String>,
    /// This machine's plans, `(GUID, name)`; read once.
    plans: Option<Vec<(String, String)>>,
}

impl AppPowerView {
    fn plans(&mut self) -> &[(String, String)] {
        self.plans
            .get_or_insert_with(|| wtm_sys::power::plans().unwrap_or_default())
    }

    fn entry(&self) -> Result<WhileRunning, String> {
        let entry = WhileRunning {
            name: self.name.trim().to_owned(),
            apps: self
                .apps
                .split(',')
                .map(|a| a.trim().to_owned())
                .filter(|a| !a.is_empty())
                .collect(),
            power_plan: (!self.plan.is_empty()).then(|| self.plan.clone()),
            keep_awake: self.awake.into(),
            hold_secs: self.editing.as_ref().map_or(5, |(_, hold)| *hold),
        };
        if entry.name.is_empty() {
            return Err(crate::i18n::tr("Give the entry a name.").to_owned());
        }
        if entry.apps.is_empty() {
            return Err(crate::i18n::tr("Name at least one program.").to_owned());
        }
        if entry.power_plan.is_none() && entry.keep_awake.is_none() {
            return Err(
                crate::i18n::tr("Choose a power plan or keep the computer awake.").to_owned(),
            );
        }
        Ok(entry)
    }

    fn edit(&mut self, entry: &WhileRunning, plans: &[(String, String)]) {
        let plans = plans.to_vec();
        *self = Self {
            name: entry.name.clone(),
            apps: entry.apps.join(", "),
            // A plan written by name is shown, and saved, by its GUID.
            plan: entry
                .power_plan
                .as_deref()
                .map(|p| wtm_policy::resolve_plan(p, &plans).unwrap_or_else(|| p.to_owned()))
                .unwrap_or_default(),
            awake: entry.keep_awake.into(),
            editing: Some((entry.name.clone(), entry.hold_secs)),
            plans: Some(plans),
            ..Self::default()
        };
    }
}

/// The name of the plan `wanted` names on this machine, else `wanted`.
fn plan_label(wanted: &str, plans: &[(String, String)]) -> String {
    wtm_policy::resolve_plan(wanted, plans)
        .and_then(|guid| plans.iter().find(|(g, _)| *g == guid))
        .map_or_else(|| wanted.to_owned(), |(_, name)| name.clone())
}

fn awake_label(awake: Option<KeepAwake>) -> &'static str {
    match awake {
        None => crate::i18n::tr("No"),
        Some(KeepAwake::System) => crate::i18n::tr("The computer"),
        Some(KeepAwake::Display) => crate::i18n::tr("The computer and the screen"),
    }
}

pub fn app_power_tab(ui: &mut Ui, data: &Data, view: &mut AppPowerView) -> Vec<UiCommand> {
    let mut commands = Vec::new();
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
    let config = match Config::from_toml(text) {
        Ok(c) => c,
        Err(e) => {
            crate::theme::colored(
                ui,
                BAD,
                crate::i18n::format(
                    "The configuration is invalid ({0}); fix it in Settings.",
                    &[e.to_string()],
                ),
            );
            return commands;
        }
    };
    if view
        .saving
        .as_ref()
        .is_some_and(|s| config.while_running.contains(s))
    {
        let plans = view.plans.take();
        *view = AppPowerView {
            plans,
            ..AppPowerView::default()
        };
    }
    let admin = matches!(data.mode, Mode::Service { admin: true, .. });
    let plans = view.plans().to_vec();

    ui.label(crate::i18n::tr(
        "While one of an entry's programs runs, RustWTM switches to its power plan and can keep \
         the computer awake. When they exit, the previous plan comes back, unless you chose \
         another one meanwhile. Game mode's plan comes first, then the entries in this order.",
    ));
    if config.dry_run {
        crate::theme::colored(
            ui,
            WARN,
            crate::i18n::tr(
                "Monitoring only: power plans and keeping awake are only previewed in the event log. Switch to Enforcing in Settings.",
            ),
        );
    }
    if !admin {
        ui.weak(crate::i18n::tr(
            "Run RustWTM as administrator to change these entries.",
        ));
    }

    crate::theme::section(ui, crate::i18n::tr("Now"));
    now(ui, data);

    crate::theme::section(ui, crate::i18n::tr("When these programs run"));
    entry_table(
        ui,
        &config.while_running,
        &plans,
        text,
        admin,
        view,
        &mut commands,
    );

    editor(ui, text, admin, &plans, view, &mut commands);
    commands
}

fn now(ui: &mut Ui, data: &Data) {
    let Some(power) = data.status.as_ref().map(|s| &s.power) else {
        ui.weak(crate::i18n::tr(
            "Connect to the service to see what is in use.",
        ));
        return;
    };
    let mut any = false;
    if let Some(plan) = &power.plan {
        any = true;
        ui.label(match &power.plan_for {
            Some(entry) => {
                crate::i18n::format("Power plan {0} for {1}", &[plan.clone(), entry.clone()])
            }
            None => crate::i18n::format("Power plan {0} for game mode", std::slice::from_ref(plan)),
        });
    }
    if let Some((awake, who)) = &power.awake {
        any = true;
        let who = who.join(", ");
        ui.label(match awake {
            KeepAwake::System => crate::i18n::format("The computer stays awake for {0}", &[who]),
            KeepAwake::Display => {
                crate::i18n::format("The computer and the screen stay on for {0}", &[who])
            }
        });
        if *awake == KeepAwake::Display {
            ui.weak(crate::i18n::tr(
                "The screen stays on while the RustWTM window runs, also in the tray.",
            ));
        }
    }
    for entry in &power.active {
        any = true;
        let line = if entry.apps.is_empty() {
            crate::i18n::format(
                "{0}: its programs just exited",
                std::slice::from_ref(&entry.name),
            )
        } else {
            crate::i18n::format(
                "{0}: {1} running",
                &[entry.name.clone(), entry.apps.join(", ")],
            )
        };
        ui.weak(line);
    }
    if let Some(note) = &power.note {
        any = true;
        crate::theme::colored(ui, WARN, crate::i18n::message(note).as_ref());
    }
    if !any {
        ui.weak(crate::i18n::tr("Nothing is switched now."));
    }
}

fn entry_table(
    ui: &mut Ui,
    all: &[WhileRunning],
    plans: &[(String, String)],
    text: &str,
    admin: bool,
    view: &mut AppPowerView,
    commands: &mut Vec<UiCommand>,
) {
    if all.is_empty() {
        ui.weak(crate::i18n::tr(
            "No entries yet. Add one below: nothing is switched until then.",
        ));
        return;
    }
    crate::theme::table_panel(ui, "power_entries", 620.0, |ui| {
        egui::Grid::new("power_entries_grid")
            .num_columns(5)
            .striped(true)
            .show(ui, |ui| {
                // The buttons first, as on the Cores page.
                ui.label("");
                for h in ["Name", "Programs", "Power plan", "Keep awake"] {
                    ui.label(RichText::new(crate::i18n::tr(h)).strong());
                }
                crate::theme::end_row(ui);
                for entry in all {
                    ui.horizontal(|ui| {
                        if crate::theme::icon_button(
                            ui,
                            admin,
                            crate::theme::icon::EDIT,
                            crate::i18n::tr("Edit"),
                        )
                        .clicked()
                        {
                            view.edit(entry, plans);
                        }
                        if crate::theme::icon_button(
                            ui,
                            admin,
                            crate::theme::icon::DELETE,
                            crate::i18n::tr("Remove"),
                        )
                        .clicked()
                        {
                            match remove_while_running(text, &entry.name) {
                                Ok(new) => commands.push(UiCommand::save_config(text, new)),
                                Err(e) => view.error = Some(e),
                            }
                        }
                    });
                    ui.label(&entry.name);
                    ui.label(entry.apps.join(", "));
                    ui.label(entry.power_plan.as_deref().map_or_else(
                        || crate::i18n::tr("Unchanged").to_owned(),
                        |p| plan_label(p, plans),
                    ));
                    ui.horizontal(|ui| {
                        ui.label(awake_label(entry.keep_awake));
                        crate::theme::fill_row(ui);
                    });
                    crate::theme::end_row(ui);
                }
            });
    });
}

fn editor(
    ui: &mut Ui,
    text: &str,
    admin: bool,
    plans: &[(String, String)],
    view: &mut AppPowerView,
    commands: &mut Vec<UiCommand>,
) {
    use crate::theme::{icon, option, option_group, option_labelled};
    option_group(
        ui,
        if view.editing.is_some() {
            crate::i18n::tr("Change entry")
        } else {
            crate::i18n::tr("Add entry")
        },
    );
    option_labelled(ui, icon::EDIT, crate::i18n::tr("Name"), "", |ui, id| {
        ui.add_sized(
            [240.0, ui.spacing().interact_size.y],
            egui::TextEdit::singleline(&mut view.name).hint_text(crate::i18n::tr("e.g. Render")),
        )
        .labelled_by(id);
    });
    option_labelled(ui, icon::LIST, crate::i18n::tr("Programs"), "", |ui, id| {
        ui.add_sized(
            [320.0, ui.spacing().interact_size.y],
            egui::TextEdit::singleline(&mut view.apps)
                .hint_text(crate::i18n::tr("e.g. blender.exe, HandBrake*.exe")),
        )
        .labelled_by(id);
    });
    option_labelled(
        ui,
        icon::POWER,
        crate::i18n::tr("Power plan"),
        "",
        |ui, id| {
            let shown = if view.plan.is_empty() {
                crate::i18n::tr("Unchanged").to_owned()
            } else {
                plan_label(&view.plan, plans)
            };
            egui::ComboBox::from_id_salt("power_plan_choice")
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut view.plan,
                        String::new(),
                        crate::i18n::tr("Unchanged"),
                    );
                    for (guid, name) in plans {
                        ui.selectable_value(&mut view.plan, guid.clone(), name);
                    }
                })
                .response
                .labelled_by(id);
        },
    );
    option(ui, icon::POWER, crate::i18n::tr("Keep awake"), "", |ui| {
        crate::theme::choice(ui, &mut view.awake, Awake::Off, crate::i18n::tr("No"));
        crate::theme::choice(
            ui,
            &mut view.awake,
            Awake::System,
            crate::i18n::tr("The computer"),
        );
        crate::theme::choice(
            ui,
            &mut view.awake,
            Awake::Display,
            crate::i18n::tr("The computer and the screen"),
        );
    });
    if view.awake == Awake::Display {
        ui.weak(crate::i18n::tr(
            "The screen stays on while the RustWTM window runs, also in the tray.",
        ));
    }
    let entry = view.entry();
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if crate::theme::primary_action(ui, admin && entry.is_ok(), crate::i18n::tr("Save entry"))
            .clicked()
            && let Ok(entry) = &entry
        {
            let replacing = view.editing.as_ref().map(|(name, _)| name.as_str());
            match set_while_running(text, entry, replacing) {
                Ok(new) => {
                    commands.push(UiCommand::save_config(text, new));
                    view.saving = Some(entry.clone());
                    view.error = None;
                }
                Err(e) => view.error = Some(e),
            }
        }
        if view.editing.is_some() && ui.button(crate::i18n::tr("Cancel")).clicked() {
            let plans = view.plans.take();
            *view = AppPowerView {
                plans,
                ..AppPowerView::default()
            };
        }
    });
    let shown = view.error.clone().or_else(|| {
        (!view.name.is_empty() || !view.apps.is_empty())
            .then(|| entry.err())
            .flatten()
    });
    if let Some(e) = shown {
        crate::theme::colored(ui, BAD, e);
    }
}

/// Sample entries for reviews and tests, with the service enforcing (what
/// [`fixture`] reports).
#[cfg(test)]
pub(crate) const REVIEW_ENTRIES: &str = r#"
dry_run = false

[[while_running]]
name = "Render"
apps = ["blender.exe", "HandBrake*.exe"]
power_plan = "8C5E7FDA-E8BF-4A96-9A85-A6E23A8C635C"
keep_awake = "system"

[[while_running]]
name = "Movie"
apps = ["vlc.exe"]
keep_awake = "display"
"#;

/// The plans of the review machine.
#[cfg(test)]
pub(crate) fn review_plans() -> Vec<(String, String)> {
    [
        ("381B4222-F694-41F0-9685-FF5BB260DF2E", "Balanced"),
        ("8C5E7FDA-E8BF-4A96-9A85-A6E23A8C635C", "High performance"),
        ("A1841308-3541-4FAB-BC81-F71556F20B4A", "Power saver"),
    ]
    .map(|(g, n)| (g.to_owned(), n.to_owned()))
    .to_vec()
}

/// What the service reports for [`REVIEW_ENTRIES`] with Blender running.
#[cfg(test)]
pub(crate) fn fixture() -> wtm_ipc::PowerStatus {
    wtm_ipc::PowerStatus {
        active: vec![wtm_policy::ActiveEntry {
            name: "Render".into(),
            apps: vec!["blender.exe".into()],
            power_plan: Some("8C5E7FDA-E8BF-4A96-9A85-A6E23A8C635C".into()),
            keep_awake: Some(KeepAwake::System),
        }],
        plan: Some("High performance".into()),
        plan_for: Some("Render".into()),
        awake: Some((KeepAwake::System, vec!["Render".into()])),
        note: None,
    }
}

#[cfg(test)]
pub(crate) fn review_view() -> AppPowerView {
    AppPowerView {
        plans: Some(review_plans()),
        ..AppPowerView::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    struct Page {
        data: Data,
        view: AppPowerView,
        sent: Vec<UiCommand>,
    }

    fn page(config: &str, size: [f32; 2]) -> Harness<'static, Page> {
        let mut data = crate::ui_tests::data();
        data.mode = Mode::Service {
            admin: true,
            verified: true,
        };
        data.config = Some(Ok(("config.toml".into(), config.to_owned())));
        if let Some(status) = data.status.as_mut() {
            status.dry_run = Config::from_toml(config).is_ok_and(|c| c.dry_run);
            *status.power = fixture();
        }
        let state = Page {
            data,
            view: review_view(),
            sent: Vec::new(),
        };
        Harness::builder()
            .allow_missing_glyphs()
            .with_size(size)
            .build_ui_state(
                |ui, page: &mut Page| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let sent = app_power_tab(ui, &page.data, &mut page.view);
                        page.sent.extend(sent);
                    });
                },
                state,
            )
    }

    fn saved(h: &mut Harness<'_, Page>) -> Config {
        let text = h
            .state_mut()
            .sent
            .drain(..)
            .rev()
            .find_map(|c| match c {
                UiCommand::SaveConfig { toml, .. } => Some(toml),
                _ => None,
            })
            .expect("saved");
        h.state_mut().data.config = Some(Ok(("config.toml".into(), text.clone())));
        h.run_steps(2);
        Config::from_toml(&text).unwrap()
    }

    fn type_into(h: &mut Harness<'_, Page>, label: &str, text: &str) {
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, label)
            .click();
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, label)
            .type_text(text);
        h.run_steps(2);
    }

    #[test]
    fn shows_what_is_switched_and_the_entries() {
        let mut h = page(REVIEW_ENTRIES, [1200.0, 1400.0]);
        h.run_steps(2);
        for label in [
            "Power plan High performance for Render",
            "The computer stays awake for Render",
            "Render: blender.exe running",
            "blender.exe, HandBrake*.exe",
        ] {
            assert!(h.query_by_label(label).is_some(), "{label}");
        }
        // In the table for Movie, and as a choice in the form.
        assert_eq!(
            h.query_all_by_label("The computer and the screen").count(),
            2
        );
        // The plan saved by GUID is shown by its name.
        assert!(h.query_all_by_label("High performance").count() >= 1);
        assert_eq!(h.query_all_by_label("Edit").count(), 2);
    }

    #[test]
    fn monitoring_only_shows_nothing_in_force() {
        let entries = REVIEW_ENTRIES.replace("dry_run = false", "dry_run = true");
        let mut h = page(&entries, [1200.0, 1400.0]);
        // What the service reports in monitoring only: the entries active,
        // no plan switched, nothing kept awake.
        {
            let status = h.state_mut().data.status.as_mut().unwrap();
            status.power.plan = None;
            status.power.plan_for = None;
            status.power.awake = None;
        }
        h.run_steps(2);
        assert!(h.query_by_label_contains("Monitoring only:").is_some());
        assert!(h.query_by_label("Render: blender.exe running").is_some());
        assert!(h.query_by_label_contains("stays awake").is_none());
        assert!(h.query_by_label_contains("Power plan High").is_none());
    }

    #[test]
    fn adds_an_entry_with_a_plan_by_guid_and_edits_and_removes_it() {
        let mut h = page("dry_run = false\n", [1200.0, 1400.0]);
        h.run_steps(2);
        assert!(h.query_by_label_contains("No entries yet").is_some());
        type_into(&mut h, "Name", "Builds");
        type_into(&mut h, "Programs", "msbuild.exe, cl*.exe");
        h.state_mut().view.plan = "8C5E7FDA-E8BF-4A96-9A85-A6E23A8C635C".into();
        h.get_by_label("The computer").click();
        h.run_steps(2);
        h.get_by_label("Save entry").click();
        h.run_steps(2);
        let config = saved(&mut h);
        assert_eq!(
            config.while_running,
            [WhileRunning {
                name: "Builds".into(),
                apps: vec!["msbuild.exe".into(), "cl*.exe".into()],
                power_plan: Some("8C5E7FDA-E8BF-4A96-9A85-A6E23A8C635C".into()),
                keep_awake: Some(KeepAwake::System),
                hold_secs: 5,
            }]
        );
        // Edited into a screen keep-awake without a plan, under a new name.
        h.get_by_label("Edit").click();
        h.run_steps(2);
        assert!(h.query_by_label("Change entry").is_some());
        h.state_mut().view.name = "Calls".into();
        h.state_mut().view.plan.clear();
        h.get_by_label("The computer and the screen").click();
        h.run_steps(2);
        h.get_by_label("Save entry").click();
        h.run_steps(2);
        let config = saved(&mut h);
        assert_eq!(config.while_running.len(), 1);
        assert_eq!(config.while_running[0].name, "Calls");
        assert_eq!(config.while_running[0].power_plan, None);
        assert_eq!(config.while_running[0].keep_awake, Some(KeepAwake::Display));
        h.get_by_label("Remove").click();
        h.run_steps(2);
        assert!(saved(&mut h).while_running.is_empty());
    }

    #[test]
    fn an_entry_needs_a_name_programs_and_something_to_do() {
        let mut h = page("dry_run = false\n", [1200.0, 1400.0]);
        h.run_steps(2);
        type_into(&mut h, "Name", "Nothing");
        assert!(h.query_by_label("Name at least one program.").is_some());
        type_into(&mut h, "Programs", "app.exe");
        assert!(
            h.query_by_label("Choose a power plan or keep the computer awake.")
                .is_some()
        );
        assert!(h.state().sent.is_empty());
    }

    #[test]
    fn entry_buttons_stay_in_view_on_a_narrow_page_in_every_language() {
        for language in crate::i18n::Language::TRANSLATED {
            let _language = crate::i18n::scope(language);
            let mut h = page(REVIEW_ENTRIES, [700.0, 2400.0]);
            h.run_steps(2);
            let edits: Vec<_> = h
                .query_all_by_label(crate::i18n::tr("Edit"))
                .map(|n| n.rect())
                .collect();
            assert_eq!(edits.len(), 2, "{language:?}");
            for rect in edits {
                assert!(rect.max.x <= 700.0, "{language:?}: {rect:?}");
            }
        }
    }
}
