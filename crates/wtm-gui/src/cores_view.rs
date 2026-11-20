//! Cores page: the processors as tiles, where the service runs the
//! processes it placed and why, and the `[[pin]]`, `[[separate]]` and
//! `[[reserve_cores]]` entries with an editor that previews an entry on the
//! processes running now before it is saved.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use eframe::egui::{self, Color32, RichText, Ui};
use wtm_core::ProcessInfo;
use wtm_core::cpu::Topology;
use wtm_ipc::PlacementStatus;
use wtm_policy::{
    Config, CoreSpec, Entry, EntryKind, Inputs, Matcher, Pin, PlacementConfig, Placer,
    ReserveConfig, ReserveCores, Separate, Tier, Why, processor_numbers, remove_entry, set_entry,
};

use crate::backend::{Data, Mode, Panel, UiCommand};
use crate::theme::{BAD, WARN};

/// Placed processes listed at most; with the rescue core engaged nearly
/// every process is placed.
const MOST_ROWS: usize = 200;

/// Holder colours for the core tiles, in order of appearance.
const PALETTE: [Color32; 6] = [
    crate::theme::CPU,
    crate::theme::MEMORY,
    crate::theme::GPU,
    crate::theme::NET_RX,
    crate::theme::NET_TX,
    crate::theme::DISK_WRITE,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoresMode {
    Count,
    Tier,
    List,
}

#[derive(Debug)]
pub struct CoresView {
    kind: EntryKind,
    name: String,
    /// The program of a pin, the owner of reserved cores, or the apps of a
    /// separation group (comma-separated).
    apps: String,
    mode: CoresMode,
    count: u16,
    tier: Tier,
    list: String,
    weights: String,
    strict: bool,
    tree: bool,
    owner_only: bool,
    /// The entry being edited (its kind and name), replaced on save.
    editing: Option<(EntryKind, String)>,
    /// The entry sent to the service; the form is kept until the
    /// configuration loaded afterwards has it.
    saving: Option<Entry>,
    error: Option<String>,
}

impl Default for CoresView {
    fn default() -> Self {
        Self {
            kind: EntryKind::Pin,
            name: String::new(),
            apps: String::new(),
            mode: CoresMode::Tier,
            count: 2,
            tier: Tier::Efficient,
            list: String::new(),
            weights: String::new(),
            strict: false,
            tree: true,
            owner_only: true,
            editing: None,
            saving: None,
            error: None,
        }
    }
}

fn cores_of(view: &mut CoresView, spec: &CoreSpec) {
    match spec {
        CoreSpec::Count(n) => {
            view.mode = CoresMode::Count;
            view.count = *n;
        }
        CoreSpec::Tier(t) => {
            view.mode = CoresMode::Tier;
            view.tier = *t;
        }
        CoreSpec::List(_) => {
            view.mode = CoresMode::List;
            view.list = spec.to_string();
        }
    }
}

impl CoresView {
    fn edit(&mut self, entry: &Entry) {
        *self = Self {
            kind: entry.kind(),
            name: entry.name().to_owned(),
            editing: Some((entry.kind(), entry.name().to_owned())),
            ..Self::default()
        };
        match entry {
            Entry::Pin(p) => {
                self.apps = p.matcher.name.clone().unwrap_or_default();
                cores_of(self, &p.cores);
                (self.strict, self.tree) = (p.strict, p.tree);
            }
            Entry::Separate(s) => {
                self.apps = s.apps.join(", ");
                self.weights = s
                    .weights
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                (self.strict, self.tree) = (s.strict, s.tree);
            }
            Entry::Reserve(r) => {
                self.apps = r.app.clone();
                cores_of(self, &r.cores);
                (self.strict, self.tree, self.owner_only) = (r.strict, r.tree, r.owner_only);
            }
        }
    }

    fn cores(&self) -> Result<CoreSpec, String> {
        match self.mode {
            CoresMode::Count => Ok(CoreSpec::Count(self.count.max(1))),
            CoresMode::Tier => Ok(CoreSpec::Tier(self.tier)),
            CoresMode::List => self.list.parse(),
        }
    }

    /// The entry the form describes, or why it is not one yet.
    fn entry(&self, hold_secs: u64) -> Result<Entry, String> {
        let name = self.name.trim().to_owned();
        if name.is_empty() {
            return Err(crate::i18n::tr("Give the entry a name.").into());
        }
        let apps: Vec<String> = self
            .apps
            .split(',')
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_owned)
            .collect();
        let one = || {
            apps.first()
                .cloned()
                .ok_or_else(|| crate::i18n::tr("Name the program, e.g. game.exe.").to_owned())
        };
        Ok(match self.kind {
            EntryKind::Pin => Entry::Pin(Pin {
                name,
                matcher: Matcher {
                    name: Some(one()?),
                    ..Matcher::default()
                },
                cores: self.cores()?,
                strict: self.strict,
                tree: self.tree,
            }),
            EntryKind::Separate => {
                let weights = self
                    .weights
                    .split(',')
                    .map(str::trim)
                    .filter(|w| !w.is_empty())
                    .map(|w| {
                        w.parse::<u32>().map_err(|_| {
                            crate::i18n::tr("Weights are whole numbers, one per app.").to_owned()
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Entry::Separate(Separate {
                    name,
                    apps,
                    weights,
                    strict: self.strict,
                    tree: self.tree,
                    hold_secs,
                })
            }
            EntryKind::Reserve => Entry::Reserve(ReserveCores {
                name,
                app: one()?,
                cores: self.cores()?,
                owner_only: self.owner_only,
                strict: self.strict,
                tree: self.tree,
                hold_secs,
            }),
        })
    }

    /// The configuration text with the form's entry saved into it.
    fn apply(&self, text: &str, entry: &Entry) -> Result<String, String> {
        let replacing = match &self.editing {
            Some((kind, name)) if *kind == entry.kind() => Some(name.as_str()),
            // The kind changed: the old entry goes, the new one is added.
            Some((kind, name)) => {
                let text = remove_entry(text, *kind, name)?;
                return set_entry(&text, entry, None);
            }
            None => None,
        };
        set_entry(text, entry, replacing)
    }
}

fn kind_label(kind: EntryKind) -> &'static str {
    crate::i18n::tr(match kind {
        EntryKind::Pin => "Keep on cores",
        EntryKind::Separate => "Keep apart",
        EntryKind::Reserve => "Reserve cores",
    })
}

fn tier_label(tier: Tier) -> &'static str {
    crate::i18n::tr(match tier {
        Tier::Performance => "Performance cores",
        Tier::Efficient => "Efficient cores",
        Tier::LowPower => "Low-power cores",
    })
}

fn spec_label(spec: &CoreSpec) -> String {
    match spec {
        CoreSpec::Count(n) => crate::i18n::format_count(
            "the fastest core",
            "the {0} fastest cores",
            usize::from(*n),
            &[n.to_string()],
        ),
        CoreSpec::Tier(t) => tier_label(*t).to_owned(),
        CoreSpec::List(_) => crate::i18n::format("CPUs {0}", &[spec.to_string()]),
    }
}

/// Every entry of a configuration, in the order the plan applies them.
fn entries(config: &Config) -> Vec<Entry> {
    let mut out: Vec<Entry> = config
        .reserve_cores
        .iter()
        .cloned()
        .map(Entry::Reserve)
        .collect();
    out.extend(config.separate.iter().cloned().map(Entry::Separate));
    out.extend(config.pins.iter().cloned().map(Entry::Pin));
    out
}

fn options_label(entry: &Entry) -> String {
    let (strict, tree, owner_only, weights) = match entry {
        Entry::Pin(p) => (p.strict, p.tree, true, &[][..]),
        Entry::Separate(s) => (s.strict, s.tree, true, &s.weights[..]),
        Entry::Reserve(r) => (r.strict, r.tree, r.owner_only, &[][..]),
    };
    let mut parts = Vec::new();
    if !weights.is_empty() {
        let list: Vec<String> = weights.iter().map(u32::to_string).collect();
        parts.push(crate::i18n::format("shares {0}", &[list.join(":")]));
    }
    if strict {
        parts.push(crate::i18n::tr("hard affinity").to_owned());
    }
    if !tree {
        parts.push(crate::i18n::tr("not the programs it starts").to_owned());
    }
    if !owner_only {
        parts.push(crate::i18n::tr("the app may use every core").to_owned());
    }
    parts.join(", ")
}

/// Who holds each processor (CPU Set id) now, with a colour per holder.
fn holders(placement: &PlacementStatus) -> HashMap<u32, (String, Color32)> {
    let mut out = HashMap::new();
    let mut next = 0usize;
    let mut colour = || {
        let c = PALETTE[next % PALETTE.len()];
        next += 1;
        c
    };
    for r in &placement.reservations {
        let c = colour();
        for id in &r.cpus {
            out.insert(*id, (r.entry.clone(), c));
        }
    }
    for s in &placement.splits {
        for (app, cpus) in &s.parts {
            let c = colour();
            for id in cpus {
                out.entry(*id).or_insert_with(|| (app.clone(), c));
            }
        }
    }
    out
}

/// The processors as tiles, one per physical core: its tier, processor
/// numbers, load and who holds it.
fn core_map(
    ui: &mut Ui,
    topology: &Topology,
    placement: Option<&PlacementStatus>,
    rescue: &[u8],
    load: &[f32],
) {
    let holders = placement.map(holders).unwrap_or_default();
    let mut order: Vec<_> = topology.cpus.iter().collect();
    order.sort_by_key(|c| (c.group, c.index));
    let number: HashMap<u32, usize> = order.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
    // Physical cores in processor order.
    let mut cores: Vec<(u16, u8, Vec<&wtm_core::cpu::LogicalCpu>)> = Vec::new();
    for c in &order {
        match cores
            .iter_mut()
            .find(|(g, k, _)| *g == c.group && *k == c.core)
        {
            Some((_, _, cpus)) => cpus.push(c),
            None => cores.push((c.group, c.core, vec![c])),
        }
    }
    // Frames cannot wrap by themselves: as many tiles per row as fit.
    const TILE: f32 = 96.0 + 2.0 * 8.0 + 3.0;
    const GAP: f32 = 6.0;
    let per_row = (((ui.available_width() + GAP) / (TILE + GAP)).floor() as usize).max(1);
    ui.spacing_mut().item_spacing.y = GAP;
    for row in cores.chunks(per_row) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(GAP, GAP);
            for (_, _, cpus) in row {
                let ids: Vec<u32> = cpus.iter().map(|c| c.id).collect();
                let numbers = processor_numbers(topology, &ids);
                let busy = {
                    let values: Vec<f32> = ids
                        .iter()
                        .filter_map(|id| load.get(number[id]).copied())
                        .collect();
                    (!values.is_empty()).then(|| values.iter().sum::<f32>() / values.len() as f32)
                };
                let in_rescue = cpus
                    .iter()
                    .any(|c| c.group == 0 && rescue.contains(&c.index));
                let reserved = cpus.iter().all(|c| c.reserved_elsewhere);
                let (holder, colour) = if in_rescue {
                    (
                        crate::i18n::tr("Rescue core").to_owned(),
                        crate::theme::ACCENT,
                    )
                } else if reserved {
                    (
                        crate::i18n::tr("Reserved by Windows").to_owned(),
                        ui.visuals().weak_text_color(),
                    )
                } else {
                    match ids.iter().find_map(|id| holders.get(id)) {
                        Some((name, c)) => (name.clone(), *c),
                        None => (
                            crate::i18n::tr("Shared").to_owned(),
                            ui.visuals().widgets.noninteractive.bg_stroke.color,
                        ),
                    }
                };
                let tier = topology.tier(cpus[0].efficiency_class);
                egui::Frame::new()
                    .fill(crate::theme::card_fill(ui))
                    .stroke(egui::Stroke::new(1.5, colour))
                    .corner_radius(6)
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.set_width(96.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.horizontal(|ui| {
                                if tier != "Uniform" {
                                    ui.label(RichText::new(&tier).strong());
                                }
                                ui.label(crate::i18n::format(
                                    "CPU {0}",
                                    std::slice::from_ref(&numbers),
                                ));
                            });
                            let fraction = busy.unwrap_or(0.0) / 100.0;
                            let bar = egui::ProgressBar::new(fraction.clamp(0.0, 1.0))
                                .desired_height(4.0)
                                .fill(crate::theme::CPU);
                            ui.add(bar);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&holder)
                                        .small()
                                        .color(crate::theme::text_color(ui, colour)),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&holder);
                        });
                    });
            }
        });
    }
}

/// What `entry` would do to the processes running now: placed processes
/// by app with their processors, and how many others it keeps off cores.
fn preview(
    config: &Config,
    entry: &Entry,
    replacing: Option<&(EntryKind, String)>,
    topology: &Topology,
    processes: &[ProcessInfo],
) -> Vec<String> {
    let mut placement: PlacementConfig = config.placement();
    if let Some((kind, name)) = replacing {
        match kind {
            EntryKind::Pin => placement.pins.retain(|p| &p.name != name),
            EntryKind::Separate => placement.separate.retain(|s| &s.name != name),
            EntryKind::Reserve => placement.reserve.retain(|r| &r.name != name),
        }
    }
    match entry.clone() {
        Entry::Pin(p) => placement.pins.push(p),
        Entry::Separate(s) => placement.separate.push(Separate { hold_secs: 0, ..s }),
        Entry::Reserve(r) => placement.reserve.push(ReserveCores { hold_secs: 0, ..r }),
    }
    // Without waiting: the preview shows what holds once the apps have run
    // for a moment.
    for s in &mut placement.separate {
        s.hold_secs = 0;
    }
    for r in &mut placement.reserve {
        r.hold_secs = 0;
    }
    let untouchable = |p: &ProcessInfo| ReserveConfig::untouchable(p.pid, &p.name);
    let keeps_all = |p: &ProcessInfo| config.reserve.exempts(&p.name);
    let own = HashSet::new();
    let inputs = Inputs {
        now: Instant::now(),
        topology,
        processes,
        own_choice: &own,
        rescue: None,
        untouchable: &untouchable,
        keeps_all: &keeps_all,
    };
    let plan = Placer::new(placement).plan(&inputs, &mut |_, _| None);
    let name = entry.name();
    let by_name: HashMap<(u32, i64), &str> = processes
        .iter()
        .map(|p| ((p.pid, p.create_time), p.name.as_str()))
        .collect();
    // App -> (processors, processes); and the processes kept off cores.
    let mut apps: Vec<(String, String, usize)> = Vec::new();
    let mut kept_off = 0usize;
    let mut keys: Vec<_> = plan.placements.keys().collect();
    keys.sort_unstable();
    for key in keys {
        let p = &plan.placements[key];
        let ours = |w: &Why| match w {
            Why::Pinned(e) | Why::Separated(e) | Why::Reserved(e) => e == name,
            _ => false,
        };
        if p.why.iter().any(ours) {
            let app = by_name.get(key).copied().unwrap_or("?").to_owned();
            let cpus = processor_numbers(topology, &p.cpus);
            match apps.iter_mut().find(|(a, c, _)| *a == app && *c == cpus) {
                Some((_, _, n)) => *n += 1,
                None => apps.push((app, cpus, 1)),
            }
        } else if p
            .why
            .iter()
            .any(|w| matches!(w, Why::KeptOff(e) if e == name))
        {
            kept_off += 1;
        }
    }
    let mut lines: Vec<String> = apps
        .into_iter()
        .map(|(app, cpus, n)| {
            crate::i18n::format_count(
                "{0} on CPUs {1}",
                "{0} ({2} processes) on CPUs {1}",
                n,
                &[app, cpus, n.to_string()],
            )
        })
        .collect();
    if kept_off > 0 {
        lines.push(crate::i18n::format_count(
            "{0} other process kept off these cores",
            "{0} other processes kept off these cores",
            kept_off,
            &[kept_off.to_string()],
        ));
    }
    lines.extend(
        plan.notes
            .iter()
            .filter(|n| n.contains(&format!("'{name}'")))
            .map(|n| crate::i18n::message(n).into_owned()),
    );
    if lines.is_empty() {
        lines.push(crate::i18n::tr("Nothing running now matches this entry.").to_owned());
    }
    lines
}

pub fn cores_tab(ui: &mut Ui, data: &Data, view: &mut CoresView) -> Vec<UiCommand> {
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
    let all = entries(&config);
    if view.saving.as_ref().is_some_and(|s| all.contains(s)) {
        *view = CoresView::default();
    }
    let admin = matches!(data.mode, Mode::Service { admin: true, .. });
    let placement = data.status.as_ref().and_then(|s| s.placement.as_ref());
    let topology = placement
        .map(|p| &p.topology)
        .or(data.cpu_topology.as_ref());

    ui.label(crate::i18n::tr(
        "Keep an app on chosen cores, keep apps apart while they run together, or reserve \
         cores for one app while it runs. The service applies this with Windows CPU Sets, \
         after the rescue core.",
    ));
    if config.dry_run {
        crate::theme::colored(
            ui,
            WARN,
            crate::i18n::tr(
                "Monitoring only: core placement is only previewed in the event log. Switch to Enforcing in Settings.",
            ),
        );
    }
    if !admin {
        ui.weak(crate::i18n::tr(
            "Run RustWTM as administrator to change core placement.",
        ));
    }

    crate::theme::section(ui, crate::i18n::tr("Processors"));
    match topology {
        Some(topology) if !topology.cpus.is_empty() => {
            let rescue = data
                .status
                .as_ref()
                .and_then(|s| s.rescue_core.as_ref())
                .map_or(&[][..], |r| &r.cpus[..]);
            let load = data
                .snapshot
                .as_ref()
                .map_or(&[][..], |s| &s.system.core_percent[..]);
            core_map(ui, topology, placement, rescue, load);
        }
        _ => {
            ui.weak(crate::i18n::tr("The processor layout could not be read."));
        }
    }

    crate::theme::section(ui, crate::i18n::tr("Placed now"));
    match placement {
        None => {
            ui.weak(crate::i18n::tr(
                "The service does not report core placement. Start the RustWTM service to see it.",
            ));
        }
        Some(p) => placed(ui, p),
    }

    crate::theme::section(ui, crate::i18n::tr("Entries"));
    entry_table(ui, &all, text, admin, view, &mut commands);

    editor(
        ui,
        data,
        &config,
        text,
        admin,
        topology,
        view,
        &mut commands,
    );
    commands
}

/// The plan in force: what cannot be met, the processes placed and why,
/// and those left alone.
fn placed(ui: &mut Ui, p: &PlacementStatus) {
    let numbers = |ids: &[u32]| processor_numbers(&p.topology, ids);
    for note in &p.notes {
        crate::theme::colored(
            ui,
            WARN,
            crate::i18n::format("Not met: {0}", &[crate::i18n::message(note).into_owned()]),
        );
    }
    if p.placed.is_empty() {
        ui.weak(crate::i18n::tr(
            "No process is placed: every process may use every processor.",
        ));
    } else {
        crate::theme::table_panel(ui, "core_placed", 640.0, |ui| {
            egui::Grid::new("core_placed_grid")
                .num_columns(5)
                .striped(true)
                .show(ui, |ui| {
                    for h in ["PID", "Name", "CPUs", "How", "Why"] {
                        ui.label(RichText::new(crate::i18n::tr(h)).strong());
                    }
                    crate::theme::end_row(ui);
                    for row in p.placed.iter().take(MOST_ROWS) {
                        crate::theme::right(ui, row.target.pid.to_string());
                        ui.label(&row.target.name);
                        ui.label(numbers(&row.cpus));
                        ui.label(crate::i18n::tr(if row.strict {
                            "Hard affinity"
                        } else {
                            "CPU Sets"
                        }));
                        let why: Vec<String> = row
                            .why
                            .iter()
                            .map(|w| crate::i18n::message(w).into_owned())
                            .collect();
                        ui.label(why.join(", "));
                        crate::theme::end_row(ui);
                    }
                });
        });
        if p.placed.len() > MOST_ROWS {
            ui.weak(crate::i18n::format(
                "…and {0} more",
                &[(p.placed.len() - MOST_ROWS).to_string()],
            ));
        }
    }
    for (target, why) in &p.left_alone {
        ui.weak(crate::i18n::format(
            "{0} ({1}) left alone: {2}",
            &[
                target.name.clone(),
                target.pid.to_string(),
                crate::i18n::message(why).into_owned(),
            ],
        ));
    }
}

fn entry_table(
    ui: &mut Ui,
    all: &[Entry],
    text: &str,
    admin: bool,
    view: &mut CoresView,
    commands: &mut Vec<UiCommand>,
) {
    if all.is_empty() {
        ui.weak(crate::i18n::tr(
            "No entries yet. Add one below: every app keeps every core until then.",
        ));
        return;
    }
    crate::theme::table_panel(ui, "core_entries", 760.0, |ui| {
        egui::Grid::new("core_entries_grid")
            .num_columns(6)
            .striped(true)
            .show(ui, |ui| {
                // The buttons first: a long translation that widens the
                // table on a narrow window cannot push them out of view.
                ui.label("");
                for h in ["Kind", "Name", "Apps", "Cores", "Options"] {
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
                            view.edit(entry);
                        }
                        if crate::theme::icon_button(
                            ui,
                            admin,
                            crate::theme::icon::DELETE,
                            crate::i18n::tr("Remove"),
                        )
                        .clicked()
                        {
                            match remove_entry(text, entry.kind(), entry.name()) {
                                Ok(new) => commands.push(UiCommand::save_config(text, new)),
                                Err(e) => view.error = Some(e),
                            }
                        }
                    });
                    ui.label(kind_label(entry.kind()));
                    ui.label(entry.name());
                    let (apps, cores) = match entry {
                        Entry::Pin(p) => (
                            p.matcher
                                .name
                                .clone()
                                .or(p.matcher.path.clone())
                                .unwrap_or_default(),
                            spec_label(&p.cores),
                        ),
                        Entry::Separate(s) => (
                            s.apps.join(", "),
                            crate::i18n::tr("split while they run together").to_owned(),
                        ),
                        Entry::Reserve(r) => (r.app.clone(), spec_label(&r.cores)),
                    };
                    ui.label(apps);
                    ui.label(cores);
                    ui.horizontal(|ui| {
                        ui.label(options_label(entry));
                        crate::theme::fill_row(ui);
                    });
                    crate::theme::end_row(ui);
                }
            });
    });
}

#[allow(clippy::too_many_arguments)]
fn editor(
    ui: &mut Ui,
    data: &Data,
    config: &Config,
    text: &str,
    admin: bool,
    topology: Option<&Topology>,
    view: &mut CoresView,
    commands: &mut Vec<UiCommand>,
) {
    use crate::theme::{icon, option, option_group, option_labelled, option_named_by_control};
    option_group(
        ui,
        if view.editing.is_some() {
            crate::i18n::tr("Change entry")
        } else {
            crate::i18n::tr("Add entry")
        },
    );
    // Kept from the entry being edited; new entries wait the default.
    let hold_secs = match &view.editing {
        Some((EntryKind::Separate, name)) => config
            .separate
            .iter()
            .find(|s| &s.name == name)
            .map_or(5, |s| s.hold_secs),
        Some((EntryKind::Reserve, name)) => config
            .reserve_cores
            .iter()
            .find(|r| &r.name == name)
            .map_or(5, |r| r.hold_secs),
        _ => 5,
    };
    ui.scope(|ui| {
        option(ui, icon::LIST, crate::i18n::tr("Kind"), "", |ui| {
            for kind in [EntryKind::Pin, EntryKind::Separate, EntryKind::Reserve] {
                crate::theme::choice(ui, &mut view.kind, kind, kind_label(kind));
            }
        });
        let hint = crate::i18n::tr(match view.kind {
            EntryKind::Pin => "Runs the app on the cores you choose.",
            EntryKind::Separate => {
                "While two or more of these apps run, each gets cores of its own; the first gets the fastest."
            }
            EntryKind::Reserve => {
                "While the app runs, these cores are its own: everything else is kept off them."
            }
        });
        ui.weak(hint);
        option_labelled(ui, icon::EDIT, crate::i18n::tr("Name"), "", |ui, id| {
            ui.add_sized(
                [240.0, ui.spacing().interact_size.y],
                egui::TextEdit::singleline(&mut view.name)
                    .hint_text(crate::i18n::tr("e.g. Game and stream")),
            )
            .labelled_by(id);
        });
        let (apps_title, apps_hint) = match view.kind {
            EntryKind::Separate => ("Apps", "e.g. game.exe, obs64.exe"),
            EntryKind::Reserve => ("Program", "e.g. reaper.exe"),
            EntryKind::Pin => ("Program", "e.g. msbuild.exe or cl*.exe"),
        };
        option_labelled(ui, icon::LIST, crate::i18n::tr(apps_title), "", |ui, id| {
            ui.add_sized(
                [320.0, ui.spacing().interact_size.y],
                egui::TextEdit::singleline(&mut view.apps).hint_text(crate::i18n::tr(apps_hint)),
            )
            .labelled_by(id);
        });
        if view.kind == EntryKind::Separate {
            option_labelled(ui, icon::CHART, crate::i18n::tr("Shares"), "", |ui, id| {
                ui.add_sized(
                    [120.0, ui.spacing().interact_size.y],
                    egui::TextEdit::singleline(&mut view.weights)
                        .hint_text(crate::i18n::tr("e.g. 3, 1")),
                )
                .labelled_by(id);
                ui.weak(crate::i18n::tr("Equal when empty."));
            });
        } else {
            option(ui, icon::SPEED, crate::i18n::tr("Cores"), "", |ui| {
                crate::theme::choice(
                    ui,
                    &mut view.mode,
                    CoresMode::Tier,
                    crate::i18n::tr("By type"),
                );
                crate::theme::choice(
                    ui,
                    &mut view.mode,
                    CoresMode::Count,
                    crate::i18n::tr("The fastest"),
                );
                crate::theme::choice(
                    ui,
                    &mut view.mode,
                    CoresMode::List,
                    crate::i18n::tr("By number"),
                );
            });
            // The choice for the way picked, on a row of its own so a
            // compact window does not cut it off.
            match view.mode {
                CoresMode::Tier => {
                    option(ui, icon::SPEED, crate::i18n::tr("By type"), "", |ui| {
                        for tier in [Tier::Performance, Tier::Efficient, Tier::LowPower] {
                            crate::theme::choice(ui, &mut view.tier, tier, tier_label(tier));
                        }
                    });
                }
                CoresMode::Count => {
                    let max = topology.map_or(64, |t| t.cpus.len().clamp(1, 1024) as u16);
                    option_labelled(ui, icon::SPEED, crate::i18n::tr("The fastest"), "", |ui, id| {
                        ui.add(
                            egui::DragValue::new(&mut view.count)
                                .range(1..=max)
                                .suffix(crate::i18n::tr(" cores")),
                        )
                        .labelled_by(id);
                    });
                }
                CoresMode::List => {
                    option_labelled(ui, icon::SPEED, crate::i18n::tr("By number"), "", |ui, id| {
                        ui.add_sized(
                            [140.0, ui.spacing().interact_size.y],
                            egui::TextEdit::singleline(&mut view.list)
                                .hint_text(crate::i18n::tr("e.g. 0-3,8")),
                        )
                        .labelled_by(id);
                    });
                }
            }
        }
        let tree = crate::i18n::tr("Include the programs it starts");
        option_named_by_control(
            ui,
            icon::LINK,
            tree,
            crate::i18n::tr(
                "Windows does not pass core choices on to child processes; RustWTM places them as well.",
            ),
            |ui| crate::theme::switch(ui, &mut view.tree, tree),
        );
        if view.kind == EntryKind::Reserve {
            let only = crate::i18n::tr("The app runs only on its reserved cores");
            option_named_by_control(ui, icon::ADMIN, only, "", |ui| {
                crate::theme::switch(ui, &mut view.owner_only, only)
            });
        }
        let strict = crate::i18n::tr("Hard affinity");
        option_named_by_control(
            ui,
            icon::KEY,
            strict,
            crate::i18n::tr(
                "Binding and inherited by child processes, but limited to one processor group; overrides the app's own choice.",
            ),
            |ui| crate::theme::switch(ui, &mut view.strict, strict),
        );
        let entry = view.entry(hold_secs);
        if let (Ok(entry), Some(topology), Some(snapshot)) =
            (&entry, topology, data.snapshot.as_ref())
        {
            ui.add_space(4.0);
            ui.label(RichText::new(crate::i18n::tr("With the apps running now")).strong());
            for line in preview(
                config,
                entry,
                view.editing.as_ref(),
                topology,
                &snapshot.processes,
            ) {
                ui.label(line);
            }
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if crate::theme::primary_action(ui, admin && entry.is_ok(), crate::i18n::tr("Save entry"))
                .clicked()
                && let Ok(entry) = &entry
            {
                match view.apply(text, entry) {
                    Ok(new) => {
                        commands.push(UiCommand::save_config(text, new));
                        view.saving = Some(entry.clone());
                        view.error = None;
                    }
                    Err(e) => view.error = Some(e),
                }
            }
            if view.editing.is_some() && ui.button(crate::i18n::tr("Cancel")).clicked() {
                *view = CoresView::default();
            }
        });
        let shown = view.error.clone().or_else(|| {
            // Saying what is missing only once something was typed.
            (!view.name.is_empty() || !view.apps.is_empty())
                .then(|| entry.err())
                .flatten()
        });
        if let Some(e) = shown {
            crate::theme::colored(ui, BAD, e);
        }
    });
}

/// Sample entries for reviews: a split, reserved cores and a pin.
#[cfg(test)]
pub(crate) const REVIEW_ENTRIES: &str = r#"
[[separate]]
name = "Game and stream"
apps = ["game.exe", "obs64.exe"]
weights = [3, 1]

[[reserve_cores]]
name = "Audio"
app = "reaper.exe"
cores = 1

[[pin]]
name = "Builds on E-cores"
match = { name = "msbuild.exe" }
cores = "efficient"
"#;

/// A plan as the service reports it for [`REVIEW_ENTRIES`] on the hybrid
/// test topology (CPU Set ids 0-7: two P-cores, four E-cores, two LP-E).
#[cfg(test)]
pub(crate) fn fixture() -> PlacementStatus {
    use wtm_core::control::ProcessRef;
    use wtm_ipc::PlacedProcess;
    let target = |pid: u32, name: &str| ProcessRef {
        pid,
        create_time: i64::from(pid),
        name: name.into(),
    };
    let placed = |pid: u32, name: &str, cpus: &[u32], why: &str| PlacedProcess {
        target: target(pid, name),
        cpus: cpus.to_vec(),
        strict: false,
        why: vec![why.into()],
    };
    PlacementStatus {
        topology: crate::ui_tests::hybrid_topology(),
        placed: vec![
            placed(200, "reaper.exe", &[0], "reserve cores 'Audio'"),
            placed(
                210,
                "game.exe",
                &[1, 2, 3, 4],
                "keep apart 'Game and stream'",
            ),
            placed(211, "obs64.exe", &[5], "keep apart 'Game and stream'"),
            placed(
                220,
                "msbuild.exe",
                &[2, 3, 4, 5],
                "keep on cores 'Builds on E-cores'",
            ),
        ],
        splits: vec![wtm_policy::Split {
            entry: "Game and stream".into(),
            parts: vec![
                ("game.exe".into(), vec![1, 2, 3, 4]),
                ("obs64.exe".into(), vec![5]),
            ],
        }],
        reservations: vec![wtm_policy::Reservation {
            entry: "Audio".into(),
            cpus: vec![0],
        }],
        notes: Vec::new(),
        left_alone: vec![(target(230, "vlc.exe"), "chose its own processors".into())],
        dry_run: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Mode;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    struct Page {
        data: Data,
        view: CoresView,
        sent: Vec<UiCommand>,
    }

    fn page(config: &str) -> Harness<'static, Page> {
        page_sized(config, [1200.0, 1400.0])
    }

    fn page_sized(config: &str, size: [f32; 2]) -> Harness<'static, Page> {
        let mut data = crate::ui_tests::data();
        data.mode = Mode::Service {
            admin: true,
            verified: true,
        };
        data.config = Some(Ok(("config.toml".into(), config.to_owned())));
        data.cpu_topology = Some(crate::ui_tests::hybrid_topology());
        if let Some(status) = data.status.as_mut() {
            status.placement = Some(fixture());
        }
        let mut snap = data.snapshot.as_deref().unwrap().clone();
        snap.processes = [(200, "reaper.exe"), (210, "game.exe"), (211, "obs64.exe")]
            .into_iter()
            .map(|(pid, name)| crate::ui_tests::proc(pid, 0, name, 1.0))
            .collect();
        data.snapshot = Some(std::sync::Arc::new(snap));
        let state = Page {
            data,
            view: CoresView::default(),
            sent: Vec::new(),
        };
        Harness::builder()
            .allow_missing_glyphs()
            .with_size(size)
            .build_ui_state(
                |ui, page: &mut Page| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let sent = cores_tab(ui, &page.data, &mut page.view);
                        page.sent.extend(sent);
                    });
                },
                state,
            )
    }

    /// The configuration saved last, loaded back as the service does.
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
    fn entry_buttons_stay_in_view_on_a_narrow_page_in_every_language() {
        // About the page's width in a compact window beside the navigation.
        for language in crate::i18n::Language::TRANSLATED {
            let _language = crate::i18n::scope(language);
            let mut h = page_sized(REVIEW_ENTRIES, [700.0, 2400.0]);
            h.run_steps(2);
            let edits: Vec<_> = h
                .query_all_by_label(crate::i18n::tr("Edit"))
                .map(|n| n.rect())
                .collect();
            println!("EXPECTED: {language:?} 3 edit buttons inside 700 px; ACTUAL: {edits:?}");
            assert_eq!(edits.len(), 3, "{language:?}");
            for rect in edits {
                assert!(rect.max.x <= 700.0, "{language:?}: {rect:?}");
            }
        }
    }

    #[test]
    fn shows_the_cores_the_plan_and_the_entries() {
        let mut h = page(&format!("dry_run = false\n{REVIEW_ENTRIES}"));
        h.run_steps(2);
        // Tiles: who holds each core.
        for holder in ["Audio", "game.exe", "obs64.exe", "Shared"] {
            assert!(h.query_all_by_label(holder).next().is_some(), "{holder}");
        }
        assert!(h.query_by_label("CPU 0").is_some());
        // The plan, with the entries that placed each process.
        assert_eq!(
            h.query_all_by_label("keep apart 'Game and stream'").count(),
            2,
            "the game and the stream"
        );
        assert!(h.query_by_label("1-4").is_some());
        assert!(
            h.query_by_label("vlc.exe (230) left alone: chose its own processors")
                .is_some()
        );
        // The entries, in the order the plan applies them.
        for (kind, cores) in [
            ("Reserve cores", "the fastest core"),
            ("Keep apart", "split while they run together"),
            ("Keep on cores", "Efficient cores"),
        ] {
            // Also among the form's choices.
            assert!(h.query_all_by_label(kind).next().is_some(), "{kind}");
            assert!(h.query_all_by_label(cores).next().is_some(), "{cores}");
        }
        assert!(h.query_by_label("shares 3:1").is_some());
        assert!(h.query_by_label_contains("Monitoring only:").is_none());
    }

    #[test]
    fn entries_are_added_previewed_edited_and_removed() {
        let mut h = page("dry_run = true\n");
        h.run_steps(2);
        assert!(h.query_by_label_contains("Monitoring only:").is_some());
        assert!(h.query_by_label_contains("No entries yet").is_some());

        // A new separation group, previewed on the apps running now.
        h.get_by_label("Keep apart").click();
        h.run_steps(2);
        type_into(&mut h, "Name", "Game and stream");
        type_into(&mut h, "Apps", "game.exe, obs64.exe");
        // Eight cores, four each: the fastest go to the first app.
        assert!(h.query_by_label("game.exe on CPUs 0-3").is_some());
        assert!(h.query_by_label("obs64.exe on CPUs 4-7").is_some());
        h.get_by_label("Save entry").click();
        h.run_steps(2);
        let c = saved(&mut h);
        assert_eq!(c.separate.len(), 1);
        assert_eq!(c.separate[0].apps, ["game.exe", "obs64.exe"]);
        // Loaded with the change: the form starts over.
        assert!(h.query_by_label("Add entry").is_some());

        // Changed into reserved cores for the game.
        h.get_by_label("Edit").click();
        h.run_steps(2);
        assert!(h.query_by_label("Change entry").is_some());
        h.get_by_label("Reserve cores").click();
        h.run_steps(2);
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Program")
            .click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Program")
            .type_text("game.exe");
        h.run_steps(2);
        h.get_by_label("The fastest").click();
        h.run_steps(2);
        h.get_by_label("Save entry").click();
        h.run_steps(2);
        let c = saved(&mut h);
        assert!(c.separate.is_empty());
        assert_eq!(c.reserve_cores.len(), 1);
        assert_eq!(c.reserve_cores[0].app, "game.exe");
        assert_eq!(c.reserve_cores[0].cores, CoreSpec::Count(2));

        h.get_by_label("Remove").click();
        h.run_steps(2);
        let c = saved(&mut h);
        assert!(c.reserve_cores.is_empty() && c.separate.is_empty());
    }

    #[test]
    fn an_incomplete_entry_says_what_is_missing_and_is_not_saved() {
        let mut h = page("dry_run = true\n");
        h.run_steps(2);
        type_into(&mut h, "Program", "game.exe");
        assert!(h.query_by_label("Give the entry a name.").is_some());
        h.get_by_label("Save entry").click();
        h.run_steps(2);
        assert!(h.state().sent.is_empty());
    }
}
