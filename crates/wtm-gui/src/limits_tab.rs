//! Limits tab: per-application CPU / memory budgets kept in the service's
//! configuration, with their live usage.

use crate::i18n::LocalDecimal;
use eframe::egui::{self, RichText, Ui};
// Shown text: written with the language's decimal sign.
use crate::i18n::bytes as format_bytes;
use wtm_policy::{AppLimit, Config, LimitScope, remove_app_limit, set_app_limit};

use crate::backend::{Data, Mode, Panel, UiCommand};
use crate::processes::budget_label;
use crate::sort::{Sort, header, num};

use crate::theme::{BAD, GOOD, WARN, right};

#[derive(Debug)]
pub struct LimitsView {
    name: String,
    cpu: bool,
    cores: f32,
    memory: bool,
    gb: f32,
    scope: LimitScope,
    /// The entry being edited, replaced on save.
    editing: Option<(String, LimitScope)>,
    /// The limit sent to the service; the form is kept until the
    /// configuration loaded afterwards has it, so a failed save loses nothing.
    saving: Option<AppLimit>,
    error: Option<String>,
}

impl Default for LimitsView {
    fn default() -> Self {
        Self {
            name: String::new(),
            cpu: true,
            cores: 2.0,
            memory: false,
            gb: 8.0,
            scope: LimitScope::Group,
            editing: None,
            saving: None,
            error: None,
        }
    }
}

impl LimitsView {
    /// Starts a new limit for `app` (from the process context menu).
    pub fn prefill(&mut self, app: &str) {
        *self = Self {
            name: app.to_owned(),
            ..Self::default()
        };
    }

    /// Starts a new CPU limit for `app` with a suggested budget.
    pub fn prefill_cores(&mut self, app: &str, cores: f32) {
        self.prefill(app);
        self.cores = cores;
    }

    fn edit(&mut self, l: &AppLimit) {
        *self = Self {
            name: l.name.clone(),
            cpu: l.cpu_cores.is_some(),
            cores: l.cpu_cores.unwrap_or(2.0),
            memory: l.memory_gb.is_some(),
            gb: l.memory_gb.unwrap_or(8.0),
            scope: l.scope,
            editing: Some((l.name.clone(), l.scope)),
            saving: None,
            error: None,
        };
    }

    fn limit(&self) -> AppLimit {
        AppLimit {
            name: self.name.trim().to_owned(),
            cpu_cores: self.cpu.then_some(self.cores),
            memory_gb: self.memory.then_some(self.gb),
            scope: self.scope,
        }
    }

    /// The configuration text with the form's limit saved into it.
    fn apply(&self, text: &str) -> Result<String, String> {
        let text = match &self.editing {
            Some((name, scope)) => {
                // Matched as the service matches apps: by name in any case.
                let limits = Config::from_toml(text).map_err(|e| e.to_string())?.limits;
                let same_app = |a: &str, b: &str| a.to_lowercase() == b.to_lowercase();
                let has = |n: &str, s: LimitScope| {
                    limits.iter().any(|l| l.scope == s && same_app(&l.name, n))
                };
                if !has(name, *scope) {
                    return Err(crate::i18n::tr(
                        "This limit was changed or removed meanwhile; open it again to change it.",
                    )
                    .into());
                }
                let target = self.limit();
                let same = target.scope == *scope && same_app(&target.name, name);
                if !same && has(&target.name, target.scope) {
                    return Err(crate::i18n::format(
                        "{0} already has a limit of this kind; change that one instead.",
                        &[target.name],
                    ));
                }
                remove_app_limit(text, name, *scope)?
            }
            None => text.to_owned(),
        };
        set_app_limit(&text, &self.limit())
    }
}

/// Image names of running processes with their total CPU and memory, the
/// busiest first.
fn running_apps(data: &Data) -> Vec<(String, f32, u64)> {
    let mut apps: std::collections::HashMap<String, (String, f32, u64)> =
        std::collections::HashMap::new();
    for p in data.snapshot.iter().flat_map(|s| &s.processes) {
        if p.pid == 0 || wtm_core::protection_reason(p.pid, &p.name).is_some() {
            continue;
        }
        let e = apps
            .entry(p.name.to_lowercase())
            .or_insert_with(|| (p.name.clone(), 0.0, 0));
        e.1 += p.cpu_percent;
        e.2 += p.private_bytes;
    }
    let mut out: Vec<_> = apps.into_values().collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.2.cmp(&a.2)));
    out
}

fn scope_label(scope: LimitScope) -> &'static str {
    match scope {
        LimitScope::Group => crate::i18n::tr("all processes together"),
        LimitScope::Each => crate::i18n::tr("each instance separately"),
    }
}

pub fn limits_tab(ui: &mut Ui, data: &Data, view: &mut LimitsView) -> Vec<UiCommand> {
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
                    &[format!("{}", e)],
                ),
            );
            return commands;
        }
    };
    if view
        .saving
        .as_ref()
        .is_some_and(|s| config.limits.contains(s))
    {
        *view = LimitsView::default();
    }
    let admin = matches!(data.mode, Mode::Service { admin: true, .. });
    let cpus = data
        .snapshot
        .as_ref()
        .map_or(1, |s| s.system.logical_cpus.max(1));
    let usage = data.status.as_ref().map_or(&[][..], |s| &s.app_limits);

    ui.label(crate::i18n::tr(
        "Every process of an app, and everything it starts, shares the app's budget. \
         Programs that reach the memory budget get out-of-memory errors.",
    ));
    if config.dry_run {
        crate::theme::colored(
            ui,
            WARN,
            crate::i18n::tr(
                "Monitoring only: limits are only previewed in the event log. Switch to Enforcing in Settings.",
            ),
        );
    }
    if !admin {
        ui.weak(crate::i18n::tr(
            "Run RustWTM as administrator to change limits.",
        ));
    }
    ui.add_space(0.0);

    struct Row<'a> {
        limit: &'a AppLimit,
        instances: usize,
        processes: usize,
        outside: usize,
        cpu: f32,
        mem: u64,
        /// Fullest budget (instance) as a share of its limit, 0..=1.
        cpu_fill: Option<f32>,
        mem_fill: Option<f32>,
    }
    let mut rows: Vec<Row<'_>> = config
        .limits
        .iter()
        .map(|l| {
            let groups: Vec<_> = usage.iter().filter(|u| u.group.is_for(l)).collect();
            Row {
                limit: l,
                instances: groups.len(),
                processes: groups.iter().map(|g| g.processes).sum(),
                outside: groups.iter().map(|g| g.outside).sum(),
                cpu: groups.iter().map(|g| g.cpu_percent).sum(),
                mem: groups.iter().map(|g| g.memory_bytes).sum(),
                cpu_fill: l.cpu_percent(cpus).filter(|&c| c > 0.0).map(|limit| {
                    groups
                        .iter()
                        .map(|g| g.cpu_percent / limit)
                        .fold(0.0, f32::max)
                }),
                mem_fill: l.memory_bytes().filter(|&m| m > 0).map(|limit| {
                    groups
                        .iter()
                        .map(|g| g.memory_bytes as f32 / limit as f32)
                        .fold(0.0, f32::max)
                }),
            }
        })
        .collect();
    let mut sort = Sort::load(ui, "app_limits", Sort::new(0, false));
    sort.apply(&mut rows, |a, b, col| match col {
        0 => crate::sort::text(&a.limit.name, &b.limit.name),
        1 => num(
            a.limit.cpu_cores.unwrap_or(0.0).into(),
            b.limit.cpu_cores.unwrap_or(0.0).into(),
        )
        .then(num(
            a.limit.memory_gb.unwrap_or(0.0).into(),
            b.limit.memory_gb.unwrap_or(0.0).into(),
        )),
        2 => crate::sort::text(scope_label(a.limit.scope), scope_label(b.limit.scope)),
        3 => a.processes.cmp(&b.processes),
        4 => num(a.cpu.into(), b.cpu.into()),
        _ => a.mem.cmp(&b.mem),
    });
    if !config.limits.is_empty() {
        crate::theme::panel(ui, |ui| {
            ui.spacing_mut().scroll = crate::theme::solid_scroll();
            egui::ScrollArea::horizontal()
                .id_salt("app_limits_grid_columns")
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_min_width(1020.0);
egui::Grid::new("app_limits_grid")
        .num_columns(7)
        .striped(true)
        .show(ui, |ui| {
            for (col, (h, numeric)) in [
                (crate::i18n::tr("App"), false),
                (crate::i18n::tr("Budget"), true),
                (crate::i18n::tr("Applies to"), false),
                (crate::i18n::tr("Processes"), true),
                (crate::i18n::tr("CPU now"), true),
                (crate::i18n::tr("Memory now"), true),
            ]
            .into_iter()
            .enumerate()
            {
                header(ui, h, col, numeric, &mut sort);
            }
            ui.label("");
            crate::theme::end_row(ui);
            for r in &rows {
                let l = r.limit;
                let response = ui.label(&l.name).interact(egui::Sense::click());
                crate::context_actions::menu(&response, ("app-limit", &l.name, l.scope as u8), |ui| {
                    crate::context_actions::copy(ui, crate::i18n::tr("Name"), || l.name.clone());
                    crate::context_actions::copy(ui, crate::i18n::tr("Budget"), || budget_label(l));
                    if admin && crate::context_actions::action(ui, crate::i18n::tr("Edit")) { view.edit(l); }
                    if admin && crate::context_actions::action(ui, crate::i18n::tr("Remove")) {
                        match remove_app_limit(text, &l.name, l.scope) {
                            Ok(new) => commands.push(UiCommand::save_config(text, new)),
                            Err(e) => view.error = Some(e),
                        }
                    }
                });
                let mut budget = budget_label(l);
                if let Some(p) = l.cpu_percent(cpus) {
                    budget.push_str(&{
                        crate::i18n::format(" ({0}% of machine)", &[crate::i18n::decimal(format!("{:.1}", p))])
                    });
                }
                right(ui, budget);
                ui.label(scope_label(l.scope));
                let count = match l.scope {
                    LimitScope::Each if r.instances > 0 => {
                        let __wtm_i18n_0 = &(r.processes);
                        let __wtm_i18n_1 = &(r.instances);
                        crate::i18n::format(
                            "{0} in {1} instances",
                            &[format!("{}", __wtm_i18n_0), format!("{}", __wtm_i18n_1)],
                        )
                    }
                    _ => r.processes.to_string(),
                };
                if r.outside > 0 {
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.label({
                                let __wtm_i18n_0 = &(r.outside);
                                crate::i18n::format(
                                    "{0} ({1} outside)",
                                    &[count.to_string(), format!("{}", __wtm_i18n_0)],
                                )
                            })
                            .on_hover_text(crate::i18n::tr(
                                "These processes run in a job object of their own (a sandbox or a build \
                                 tool) and Windows does not let them share the budget. They join it once \
                                 the app is restarted.",
                            ));
                        },
                    );
                } else {
                    right(ui, count);
                }
                let cores = {
                    let __wtm_i18n_0 = &((r.cpu / 100.0 * cpus as f32).max(0.0) + 0.0);
                    crate::i18n::format("{0} cores", &[crate::i18n::decimal(format!("{:.2}", __wtm_i18n_0))])
                };
                usage_cell(ui, cores, r.cpu_fill);
                usage_cell(ui, format_bytes(r.mem as f64), r.mem_fill);
                ui.horizontal(|ui| {
                    if crate::theme::icon_button(ui, admin, crate::theme::icon::EDIT, crate::i18n::tr("Edit"))
.clicked()
                    {
                        view.edit(l);
                    }
                    if crate::theme::icon_button(ui, admin, crate::theme::icon::DELETE, crate::i18n::tr("Remove"))
.clicked()
                    {
                            match remove_app_limit(text, &l.name, l.scope) {
                                Ok(new) => commands.push(UiCommand::save_config(text, new)),
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
    }
    sort.store(ui, "app_limits");
    if config.limits.is_empty() {
        ui.weak(crate::i18n::tr("No app limits yet. Add one below, or right-click a process and choose \"Limit this app…\"."));
    }

    ui.add_space(0.0);
    use crate::theme::{icon, option, option_group, option_labelled, option_named_by_control};
    option_group(
        ui,
        if view.editing.is_some() {
            crate::i18n::tr("Change limit")
        } else {
            crate::i18n::tr("Add limit")
        },
    );
    ui.scope(|ui| {
        option_labelled(ui, icon::LIST, crate::i18n::tr("Program"), "", |ui, id| {
            ui.add_sized(
                [240.0, ui.spacing().interact_size.y],
                egui::TextEdit::singleline(&mut view.name)
                    .hint_text(crate::i18n::tr("e.g. node.exe or chrome*.exe")),
            )
            .labelled_by(id);
            egui::ComboBox::from_label(crate::i18n::tr("or a running app"))
                .selected_text(crate::i18n::tr("Pick…"))
                .height(400.0)
                .show_ui(ui, |ui| {
                    for (name, cpu, mem) in running_apps(data) {
                        let label = crate::i18n::format(
                            "{0}  ({1}% CPU, {2})",
                            &[
                                name.to_string(),
                                crate::i18n::decimal(format!("{:.1}", cpu)),
                                format_bytes(mem as f64),
                            ],
                        );
                        if ui.selectable_label(view.name == name, label).clicked() {
                            view.name = name;
                        }
                    }
                });
        });
        option_named_by_control(ui, icon::SPEED, "CPU", "", |ui| {
            ui.add_enabled(
                view.cpu,
                egui::DragValue::new(&mut view.cores)
                    .local_decimal()
                    .range(0.1..=cpus as f32)
                    .clamp_existing_to_range(false)
                    .speed(0.05)
                    .max_decimals(2)
                    .suffix(crate::i18n::tr(" cores")),
            );
            ui.add_space(8.0);
            ui.label(crate::i18n::tr(if view.cpu { "On" } else { "Off" }));
            crate::theme::switch(ui, &mut view.cpu, "CPU");
        });
        let memory = crate::i18n::tr("Memory");
        option_named_by_control(ui, icon::MEMORY, memory, "", |ui| {
            ui.add_enabled(
                view.memory,
                egui::DragValue::new(&mut view.gb)
                    .local_decimal()
                    .range(0.1..=4096.0)
                    .clamp_existing_to_range(false)
                    .speed(0.1)
                    .max_decimals(2)
                    .suffix(" GiB"),
            );
            ui.add_space(8.0);
            ui.label(crate::i18n::tr(if view.memory { "On" } else { "Off" }));
            crate::theme::switch(ui, &mut view.memory, memory);
        });
        option(ui, icon::PEOPLE, crate::i18n::tr("Applies to"), "", |ui| {
            crate::theme::choice(
                ui,
                &mut view.scope,
                LimitScope::Group,
                crate::i18n::tr("All processes together"),
            )
            .on_hover_text(crate::i18n::tr(
                "One shared budget for every process of the app and their children.",
            ));
            crate::theme::choice(
                ui,
                &mut view.scope,
                LimitScope::Each,
                crate::i18n::tr("Each instance separately"),
            )
            .on_hover_text(crate::i18n::tr(
                "Every top-level instance gets its own budget, shared with its children.",
            ));
        });
        ui.add_space(4.0);
        let candidate = view.limit();
        let valid = !candidate.name.is_empty()
            && (candidate.cpu_cores.is_some() || candidate.memory_gb.is_some());
        ui.horizontal_wrapped(|ui| {
            if crate::theme::primary_action(ui, admin && valid, crate::i18n::tr("Save limit"))
                .clicked()
            {
                match view.apply(text) {
                    Ok(new) => {
                        commands.push(UiCommand::save_config(text, new));
                        view.saving = Some(candidate);
                        view.error = None;
                    }
                    Err(e) => view.error = Some(e),
                }
            }
            if view.editing.is_some() && ui.button(crate::i18n::tr("Cancel")).clicked() {
                *view = LimitsView::default();
            }
        });
        if let Some(e) = &view.error {
            crate::theme::colored(ui, BAD, e);
        }
    });
    commands
}

/// Current use, as a bar against the budget when there is one. Red from 95%
/// (memory allocations start failing), orange from 75%.
fn usage_cell(ui: &mut Ui, text: String, fill: Option<f32>) {
    let Some(fill) = fill else {
        right(ui, text);
        return;
    };
    let color = if fill >= 0.95 {
        BAD
    } else if fill >= 0.75 {
        WARN
    } else {
        GOOD
    };
    ui.add(
        egui::ProgressBar::new(fill.clamp(0.0, 1.0))
            .corner_radius(0.0)
            .desired_width(150.0)
            .fill(color.gamma_multiply(0.8))
            .text(RichText::new(format!(
                "{text} ({})",
                crate::i18n::percent(format!("{:.0}", (fill * 100.0).max(0.0) + 0.0))
            ))),
    )
    .on_hover_text(crate::i18n::tr(
        "Use of the budget; for one budget per instance, the fullest instance",
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_round_trips_and_replaces_the_edited_entry() {
        let text =
            "dry_run = false\n[[limit]]\nname = \"zen.exe\"\nmemory_gb = 8.0\nscope = \"each\"\n";
        let config = Config::from_toml(text).unwrap();
        let mut v = LimitsView::default();
        v.edit(&config.limits[0]);
        assert_eq!(v.limit(), config.limits[0]);
        // Renaming replaces the old entry instead of adding a second one.
        v.name = "zen-browser.exe".into();
        v.scope = LimitScope::Group;
        v.cpu = true;
        let new = Config::from_toml(&v.apply(text).unwrap()).unwrap();
        assert_eq!(new.limits.len(), 1);
        assert_eq!(new.limits[0].name, "zen-browser.exe");
        assert_eq!(new.limits[0].cpu_cores, Some(2.0));

        v.prefill("node.exe");
        assert_eq!(v.limit().name, "node.exe");
        assert!(v.editing.is_none());
        let two = Config::from_toml(&v.apply(text).unwrap()).unwrap();
        assert_eq!(two.limits.len(), 2);
        assert!(v.apply("x = [").is_err());
    }

    #[test]
    fn a_budget_typed_with_a_decimal_comma_is_understood() {
        use egui_kittest::{Harness, kittest::Queryable};
        // With a sample, so the field reaches more than one core.
        let data = Data {
            config: Some(Ok(("config.toml".into(), String::new()))),
            mode: crate::backend::Mode::Service {
                admin: true,
                verified: true,
            },
            ..crate::ui_tests::data()
        };
        let mut view = LimitsView::default();
        view.prefill("a.exe");
        view.cpu = true;
        let mut h = Harness::builder()
            .allow_missing_glyphs()
            .with_size([1400.0, 900.0])
            .build_ui_state(
                move |ui, s: &mut (LimitsView, Vec<UiCommand>)| {
                    let _turkish = crate::i18n::scope(crate::i18n::Language::Turkish);
                    let commands = limits_tab(ui, &data, &mut s.0);
                    s.1.extend(commands);
                },
                (view, Vec::new()),
            );
        h.run();
        {
            // Shown with a comma, as Turkish writes it.
            use egui_kittest::kittest::NodeT;
            let field = h
                .get_all_by_role(egui::accesskit::Role::SpinButton)
                .next()
                .unwrap();
            assert_eq!(
                field.accesskit_node().value().as_deref(),
                Some("2,00 çekirdek")
            );
        }
        let field = h
            .get_all_by_role(egui::accesskit::Role::SpinButton)
            .next()
            .unwrap();
        field.click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.get_all_by_role(egui::accesskit::Role::SpinButton)
            .next()
            .unwrap()
            .type_text("2,5");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().0.cores, 2.5, "\"2,5\" is two and a half cores");
    }

    #[test]
    fn showing_a_limit_does_not_change_its_budget() {
        use egui_kittest::{Harness, kittest::Queryable};
        // No sample yet, so the processor count is not known, and a hand-set
        // memory budget below the field's lower end.
        let text =
            "[[limit]]\nname = \"a.exe\"\ncpu_cores = 2.0\nmemory_gb = 0.05\nscope = \"group\"\n";
        let data = Data {
            config: Some(Ok(("config.toml".into(), text.into()))),
            mode: crate::backend::Mode::Service {
                admin: true,
                verified: true,
            },
            ..Data::default()
        };
        let mut h = Harness::builder()
            .allow_missing_glyphs()
            .with_size([1400.0, 900.0])
            .build_ui_state(
                move |ui, s: &mut (LimitsView, Vec<UiCommand>)| {
                    let commands = limits_tab(ui, &data, &mut s.0);
                    s.1.extend(commands);
                },
                (LimitsView::default(), Vec::new()),
            );
        h.run();
        h.get_by_label("Edit").click();
        h.run_steps(3);
        h.get_by_label("Save limit").click();
        h.run();
        let saved = h
            .state()
            .1
            .iter()
            .find_map(|c| match c {
                UiCommand::SaveConfig { toml: t, .. } => Some(t.clone()),
                _ => None,
            })
            .expect("saved");
        let limit = &Config::from_toml(&saved).unwrap().limits[0];
        assert_eq!(
            limit.cpu_cores,
            Some(2.0),
            "not cut to the 1 core assumed before a sample"
        );
        assert_eq!(limit.memory_gb, Some(0.05));
    }

    #[test]
    fn an_edit_never_overwrites_another_limit_or_revives_a_removed_one() {
        let text = "[[limit]]\nname = \"a.exe\"\nmemory_gb = 4.0\nscope = \"group\"\n\
                    [[limit]]\nname = \"b.exe\"\nmemory_gb = 8.0\nscope = \"group\"\n";
        let config = Config::from_toml(text).unwrap();
        // Renaming a.exe's limit to b.exe, which has one of its own.
        let mut v = LimitsView::default();
        v.edit(&config.limits[0]);
        v.name = "B.exe".into();
        assert!(v.apply(text).is_err(), "b.exe's 8 GB limit would be lost");

        // a.exe's limit was removed elsewhere while its form was open.
        let mut v = LimitsView::default();
        v.edit(&config.limits[0]);
        v.gb = 6.0;
        let without_a = "[[limit]]\nname = \"b.exe\"\nmemory_gb = 8.0\nscope = \"group\"\n";
        assert!(
            v.apply(without_a).is_err(),
            "a removed limit is not brought back"
        );

        // Changing only the case or the budget is an ordinary edit.
        let mut v = LimitsView::default();
        v.edit(&config.limits[0]);
        v.name = "A.EXE".into();
        v.gb = 6.0;
        let new = Config::from_toml(&v.apply(text).unwrap()).unwrap();
        assert_eq!(new.limits.len(), 2);
    }
}
