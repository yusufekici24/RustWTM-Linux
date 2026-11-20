//! The Hardware specifications page as a spec sheet: a banner with the
//! PC's name and headline numbers, then one illustrated card per part (a
//! drawn chip with a core map, memory sticks per slot, drives scaled by
//! capacity, graphics cards and the board). The full field list stays
//! under "All details" for copying.
//!
//! Everything here comes from the same read-only Windows inventory as the
//! tables; nothing is live, and nothing is guessed: a part Windows does not
//! describe is left out rather than drawn empty.

use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, Stroke, Ui, Vec2, pos2, vec2};

use crate::inventory::{Item, field, memory_metadata};
use crate::theme;

/// Storage drawn in green, the board in the accent.
const STORAGE: Color32 = theme::DISK_READ;
const BOARD: Color32 = theme::ACCENT;

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Spec {
    pub maker: String,
    pub model: String,
    pub system_type: String,
    pub cpus: Vec<Cpu>,
    pub modules: Vec<Dimm>,
    pub drives: Vec<Drive>,
    pub gpus: Vec<Gpu>,
    pub board: Option<Board>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Cpu {
    pub name: String,
    pub cores: Option<u32>,
    pub threads: Option<u32>,
    pub mhz: Option<u32>,
    pub l2: Option<u64>,
    pub l3: Option<u64>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Dimm {
    pub maker: String,
    pub part: String,
    pub bytes: Option<u64>,
    pub mhz: Option<u32>,
    pub kind: Option<&'static str>,
    pub form: Option<&'static str>,
    pub slot: String,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Drive {
    pub index: Option<u32>,
    pub model: String,
    pub bytes: Option<u64>,
    pub interface: String,
    pub firmware: String,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Gpu {
    pub name: String,
    pub maker: String,
    pub driver: String,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Board {
    pub maker: String,
    pub product: String,
    pub version: String,
    pub bios: String,
    pub bios_date: String,
}

fn text(item: &Item, key: &str) -> String {
    field(item, key).unwrap_or_default().to_owned()
}

fn number<T: std::str::FromStr + PartialEq + Default>(item: &Item, key: &str) -> Option<T> {
    field(item, key)
        .and_then(|v| v.parse::<T>().ok())
        .filter(|v| *v != T::default())
}

impl Spec {
    pub(crate) fn from_items(items: &[Item]) -> Self {
        let mut spec = Spec::default();
        let mut board = Board::default();
        let mut has_board = false;
        for item in items {
            match item.kind.as_str() {
                "System" => {
                    spec.maker = text(item, "Manufacturer");
                    spec.model = text(item, "Model");
                    spec.system_type = text(item, "SystemType");
                }
                "CPU" => {
                    let name = text(item, "Name");
                    if !name.is_empty() {
                        spec.cpus.push(Cpu {
                            name,
                            cores: number(item, "NumberOfCores"),
                            threads: number(item, "NumberOfLogicalProcessors"),
                            mhz: number(item, "MaxClockSpeed"),
                            // Reported in KiB.
                            l2: number::<u64>(item, "L2CacheSize").map(|k| k * 1024),
                            l3: number::<u64>(item, "L3CacheSize").map(|k| k * 1024),
                        });
                    }
                }
                "RAM" => spec.modules.push(Dimm {
                    maker: text(item, "Manufacturer"),
                    part: text(item, "PartNumber"),
                    bytes: number(item, "Capacity"),
                    mhz: number(item, "ConfiguredClockSpeed"),
                    kind: field(item, "SMBIOSMemoryType")
                        .and_then(|v| memory_metadata("SMBIOSMemoryType", v)),
                    form: field(item, "FormFactor").and_then(|v| memory_metadata("FormFactor", v)),
                    slot: text(item, "DeviceLocator"),
                }),
                "Disk" => {
                    let model = text(item, "Model");
                    if !model.is_empty() {
                        spec.drives.push(Drive {
                            index: field(item, "Index").and_then(|v| v.parse().ok()),
                            model,
                            bytes: number(item, "Size"),
                            interface: text(item, "InterfaceType"),
                            firmware: text(item, "FirmwareRevision"),
                        });
                    }
                }
                "GPU" => {
                    let name = text(item, "Name");
                    if !name.is_empty() {
                        spec.gpus.push(Gpu {
                            name,
                            maker: text(item, "AdapterCompatibility"),
                            driver: text(item, "DriverVersion"),
                        });
                    }
                }
                "Motherboard" => {
                    board.maker = text(item, "Manufacturer");
                    board.product = text(item, "Product");
                    board.version = text(item, "Version");
                    has_board |= !board.product.is_empty() || !board.maker.is_empty();
                }
                "BIOS" => {
                    board.bios = text(item, "SMBIOSBIOSVersion");
                    board.bios_date = text(item, "ReleaseDate");
                    has_board |= !board.bios.is_empty();
                }
                _ => {}
            }
        }
        spec.modules
            .retain(|m| m.bytes.is_some() || !m.part.is_empty());
        spec.drives.sort_by_key(|d| d.index.unwrap_or(u32::MAX));
        spec.board = has_board.then_some(board);
        spec
    }

    /// "Maker Model", without repeating a maker the model already names.
    pub(crate) fn title(&self) -> String {
        if self.maker.is_empty()
            || self
                .model
                .to_lowercase()
                .contains(&self.maker.to_lowercase())
        {
            self.model.clone()
        } else if self.model.is_empty() {
            self.maker.clone()
        } else {
            format!("{} {}", self.maker, self.model)
        }
    }

    pub(crate) fn memory_total(&self) -> Option<u64> {
        let total: u64 = self.modules.iter().filter_map(|m| m.bytes).sum();
        (total > 0).then_some(total)
    }

    pub(crate) fn storage_total(&self) -> Option<u64> {
        let total: u64 = self.drives.iter().filter_map(|d| d.bytes).sum();
        (total > 0).then_some(total)
    }

    pub(crate) fn cores(&self) -> Option<u32> {
        let n: u32 = self.cpus.iter().filter_map(|c| c.cores).sum();
        (n > 0).then_some(n)
    }

    pub(crate) fn threads(&self) -> Option<u32> {
        let n: u32 = self.cpus.iter().filter_map(|c| c.threads).sum();
        (n > 0).then_some(n)
    }
}

fn bytes(n: u64) -> String {
    crate::i18n::bytes(n as f64)
}

fn clock(mhz: u32) -> String {
    if mhz >= 1000 {
        crate::i18n::decimal(format!("{:.1} GHz", f64::from(mhz) / 1000.0))
    } else {
        format!("{mhz} MHz")
    }
}

/// A part's card, drawn into whichever column it is given.
type Card<'a> = Box<dyn FnOnce(&mut Ui) + 'a>;

/// The whole showcase; the caller adds the detail tables below it.
pub(crate) fn show(ui: &mut Ui, items: &[Item]) {
    let spec = Spec::from_items(items);
    hero(ui, &spec);
    ui.add_space(4.0);
    let wide = ui.available_width() >= 900.0;
    let mut cards: Vec<Card<'_>> = Vec::new();
    for cpu in &spec.cpus {
        cards.push(Box::new(move |ui| cpu_card(ui, cpu)));
    }
    if !spec.modules.is_empty() {
        cards.push(Box::new(|ui| memory_card(ui, &spec)));
    }
    if !spec.drives.is_empty() {
        cards.push(Box::new(|ui| storage_card(ui, &spec)));
    }
    if !spec.gpus.is_empty() {
        cards.push(Box::new(|ui| graphics_card(ui, &spec.gpus)));
    }
    if let Some(board) = &spec.board {
        let system_type = spec.system_type.as_str();
        cards.push(Box::new(move |ui| board_card(ui, board, system_type)));
    }
    if wide {
        // Each card goes to the shorter column, so neither runs on alone.
        ui.columns(2, |cols| {
            for card in cards {
                let i = usize::from(cols[1].min_rect().height() < cols[0].min_rect().height());
                card(&mut cols[i]);
            }
        });
    } else {
        for card in cards {
            card(ui);
        }
    }
}

/// The banner: the PC's name and its headline numbers.
fn hero(ui: &mut Ui, spec: &Spec) {
    let dark = ui.visuals().dark_mode;
    let background = ui.painter().add(egui::Shape::Noop);
    let inner = egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(20, 16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(crate::i18n::tr("This PC"))
                    .small()
                    .strong()
                    .color(theme::text_color(ui, theme::ACCENT)),
            );
            let title = spec.title();
            let title = if title.is_empty() {
                crate::i18n::tr("This PC").to_owned()
            } else {
                title
            };
            ui.label(RichText::new(title).size(26.0).strong());
            let subtitle: Vec<String> = [
                spec.system_type.clone(),
                spec.cpus
                    .first()
                    .map(|c| c.name.clone())
                    .unwrap_or_default(),
            ]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
            if !subtitle.is_empty() {
                ui.weak(subtitle.join("  ·  "));
            }
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 28.0;
                if let Some(cores) = spec.cores() {
                    big_stat(ui, &cores.to_string(), crate::i18n::tr("Cores"), theme::CPU);
                }
                if let Some(threads) = spec.threads() {
                    big_stat(
                        ui,
                        &threads.to_string(),
                        crate::i18n::tr("Threads"),
                        theme::CPU,
                    );
                }
                if let Some(total) = spec.memory_total() {
                    big_stat(ui, &bytes(total), crate::i18n::tr("Memory"), theme::MEMORY);
                }
                if let Some(total) = spec.storage_total() {
                    big_stat(ui, &bytes(total), crate::i18n::tr("Storage"), STORAGE);
                }
                if !spec.gpus.is_empty() {
                    big_stat(
                        ui,
                        &spec.gpus.len().to_string(),
                        crate::i18n::tr("Graphics"),
                        theme::GPU,
                    );
                }
            });
        });
    let rect = inner.response.rect;
    let base = theme::tile_fill(ui);
    let mut shapes = vec![egui::Shape::rect_filled(rect, theme::SURFACE_RADIUS, base)];
    // A soft glow (stacked faint discs) behind a large chip, on the right
    // where the banner has room; all of it inside the banner.
    let radius = (rect.height() * 0.5 - 6.0).max(0.0);
    let center = pos2(rect.right() - radius - 24.0, rect.center().y);
    if rect.width() >= 720.0 && radius > 40.0 {
        let glow = |color: Color32, center: Pos2, radius: f32, shapes: &mut Vec<egui::Shape>| {
            const STEPS: usize = 28;
            let alpha = if dark { 4 } else { 3 };
            for k in 0..STEPS {
                let r = radius * (1.0 - k as f32 / STEPS as f32);
                shapes.push(egui::Shape::circle_filled(
                    center,
                    r,
                    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha),
                ));
            }
        };
        glow(
            theme::MEMORY,
            center - vec2(radius * 0.55, 0.0),
            radius * 0.8,
            &mut shapes,
        );
        glow(theme::CPU, center, radius, &mut shapes);
        let mut chip = Vec::new();
        chip_shapes(
            &mut chip,
            Rect::from_center_size(center, Vec2::splat(radius * 1.25)),
            theme::text_color(ui, theme::CPU),
        );
        shapes.extend(chip);
    }
    // An accent edge along the top.
    shapes.push(egui::Shape::rect_filled(
        Rect::from_min_size(rect.min + vec2(14.0, 0.0), vec2(rect.width() - 28.0, 3.0)),
        1.5,
        theme::ACCENT,
    ));
    shapes.push(egui::Shape::rect_stroke(
        rect,
        theme::SURFACE_RADIUS,
        Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        egui::StrokeKind::Inside,
    ));
    ui.painter().set(background, egui::Shape::Vec(shapes));
}

fn big_stat(ui: &mut Ui, value: &str, caption: &str, color: Color32) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.label(
            RichText::new(value)
                .size(30.0)
                .strong()
                .color(theme::text_color(ui, color)),
        );
        ui.weak(caption);
    });
}

/// A part's card: its drawing on the left, its facts on the right.
fn part_card(
    ui: &mut Ui,
    title: &str,
    color: Color32,
    icon: fn(&egui::Painter, Rect, Color32),
    add: impl FnOnce(&mut Ui),
) {
    theme::panel(ui, |ui| {
        ui.horizontal_top(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(64.0), egui::Sense::hover());
            icon(ui.painter(), rect, theme::text_color(ui, color));
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(title)
                        .small()
                        .strong()
                        .color(theme::text_color(ui, color)),
                );
                add(ui);
            });
        });
    });
}

/// A small rounded label for a secondary fact.
fn pill(ui: &mut Ui, text: impl Into<String>) {
    // Outlined too: on a light card the fill alone barely shows.
    egui::Frame::new()
        .fill(theme::chip_fill(ui.visuals().dark_mode))
        .stroke(Stroke::new(
            1.0,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
        .corner_radius(9)
        .inner_margin(egui::Margin::symmetric(7, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text.into()).small());
        });
}

fn pills(ui: &mut Ui, facts: &[String]) {
    let facts: Vec<&String> = facts.iter().filter(|f| !f.is_empty()).collect();
    if facts.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        for f in facts {
            pill(ui, f.as_str());
        }
    });
}

fn mid_stat(ui: &mut Ui, value: &str, caption: &str, color: Color32) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.label(
            RichText::new(value)
                .size(20.0)
                .strong()
                .color(theme::text_color(ui, color)),
        );
        ui.weak(RichText::new(caption).small());
    });
}

fn cpu_card(ui: &mut Ui, cpu: &Cpu) {
    part_card(
        ui,
        crate::i18n::tr("Processor"),
        theme::CPU,
        chip_icon,
        |ui| {
            ui.label(RichText::new(&cpu.name).strong().size(15.0));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 20.0;
                if let Some(cores) = cpu.cores {
                    mid_stat(ui, &cores.to_string(), crate::i18n::tr("Cores"), theme::CPU);
                }
                if let Some(threads) = cpu.threads {
                    mid_stat(
                        ui,
                        &threads.to_string(),
                        crate::i18n::tr("Threads"),
                        theme::CPU,
                    );
                }
                if let Some(mhz) = cpu.mhz {
                    mid_stat(ui, &clock(mhz), crate::i18n::tr("Max clock"), theme::CPU);
                }
            });
            if let Some(cores) = cpu.cores {
                core_map(ui, cores, cpu.threads.unwrap_or(cores));
            }
            cache_bars(ui, cpu);
        },
    );
}

/// One square per core, with a dot per thread it runs.
fn core_map(ui: &mut Ui, cores: u32, threads: u32) {
    const MOST: u32 = 128;
    let shown = cores.min(MOST);
    let per_core = threads_per_core(cores, threads);
    let cell = 15.0;
    let gap = 3.0;
    let width = ui.available_width().max(cell);
    let per_row = ((width + gap) / (cell + gap)).floor().max(1.0) as u32;
    let rows = shown.div_ceil(per_row);
    let height = rows as f32 * (cell + gap) - gap;
    let (rect, response) =
        ui.allocate_exact_size(vec2(width, height.max(cell)), egui::Sense::hover());
    let color = theme::text_color(ui, theme::CPU);
    let painter = ui.painter();
    for i in 0..shown {
        let (row, col) = (i / per_row, i % per_row);
        let min = rect.min + vec2(col as f32 * (cell + gap), row as f32 * (cell + gap));
        let square = Rect::from_min_size(min, Vec2::splat(cell));
        painter.rect_filled(square, 3.0, color.gamma_multiply(0.22));
        painter.rect_stroke(
            square,
            3.0,
            Stroke::new(1.0, color.gamma_multiply(0.7)),
            egui::StrokeKind::Inside,
        );
        for t in 0..per_core {
            let x = square.left() + cell * (t as f32 + 1.0) / (per_core as f32 + 1.0);
            painter.circle_filled(pos2(x, square.center().y), 1.8, color);
        }
    }
    response.on_hover_text(crate::i18n::format(
        "{0} cores, {1} threads",
        &[cores.to_string(), threads.to_string()],
    ));
}

fn cache_bars(ui: &mut Ui, cpu: &Cpu) {
    let caches: Vec<(&str, u64)> = [
        (crate::i18n::tr("L2 cache"), cpu.l2),
        (crate::i18n::tr("L3 cache"), cpu.l3),
    ]
    .into_iter()
    .filter_map(|(label, size)| size.map(|s| (label, s)))
    .collect();
    let Some(largest) = caches.iter().map(|(_, s)| *s).max() else {
        return;
    };
    let color = theme::text_color(ui, theme::CPU);
    for (label, size) in caches {
        ui.horizontal(|ui| {
            ui.add_sized(
                [70.0, 16.0],
                egui::Label::new(RichText::new(label).small().weak()),
            );
            let width = (ui.available_width() - 70.0).max(40.0);
            let (rect, _) = ui.allocate_exact_size(vec2(width, 8.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 4.0, theme::track_color(ui));
            let share = (size as f32 / largest as f32).clamp(0.04, 1.0);
            ui.painter().rect_filled(
                Rect::from_min_size(rect.min, vec2(rect.width() * share, rect.height())),
                4.0,
                color,
            );
            ui.label(RichText::new(bytes(size)).small());
        });
    }
}

fn memory_card(ui: &mut Ui, spec: &Spec) {
    part_card(
        ui,
        crate::i18n::tr("Memory"),
        theme::MEMORY,
        dimm_icon,
        |ui| {
            let kinds: Vec<&str> = spec.modules.iter().filter_map(|m| m.kind).collect();
            let speed = spec.modules.iter().filter_map(|m| m.mhz).max();
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 20.0;
                if let Some(total) = spec.memory_total() {
                    mid_stat(
                        ui,
                        &bytes(total),
                        crate::i18n::tr("Capacity"),
                        theme::MEMORY,
                    );
                }
                if let Some(kind) = kinds.first() {
                    mid_stat(ui, kind, crate::i18n::tr("Memory type"), theme::MEMORY);
                }
                if let Some(mhz) = speed {
                    mid_stat(
                        ui,
                        &format!("{mhz} MHz"),
                        crate::i18n::tr("Memory speed"),
                        theme::MEMORY,
                    );
                }
            });
            // One stick per module, labelled with its size and slot.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                for m in &spec.modules {
                    memory_stick(ui, m);
                }
            });
        },
    );
}

fn memory_stick(ui: &mut Ui, m: &Dimm) {
    let color = theme::text_color(ui, theme::MEMORY);
    let (rect, response) = ui.allocate_exact_size(vec2(132.0, 56.0), egui::Sense::hover());
    let painter = ui.painter();
    let board = Rect::from_min_size(rect.min, vec2(rect.width(), 40.0));
    painter.rect_filled(board, 3.0, color.gamma_multiply(0.22));
    painter.rect_stroke(
        board,
        3.0,
        Stroke::new(1.0, color),
        egui::StrokeKind::Inside,
    );
    // Memory chips along the top of the stick.
    for i in 0..4 {
        let x = board.left() + 8.0 + i as f32 * 30.0;
        painter.rect_filled(
            Rect::from_min_size(pos2(x, board.top() + 4.0), vec2(22.0, 9.0)),
            1.5,
            color.gamma_multiply(0.75),
        );
    }
    let size = m.bytes.map(bytes).unwrap_or_default();
    painter.text(
        pos2(board.center().x, board.top() + 22.0),
        egui::Align2::CENTER_CENTER,
        &size,
        FontId::proportional(11.0),
        ui.visuals().strong_text_color(),
    );
    // Gold contacts along the bottom edge, broken by the key notch.
    let gold = Color32::from_rgb(214, 170, 72);
    let notch = board.center().x + 12.0;
    let mut x = board.left() + 6.0;
    while x < board.right() - 6.0 {
        if (x - notch).abs() > 3.0 {
            painter.line_segment(
                [pos2(x, board.bottom() - 6.0), pos2(x, board.bottom() - 1.0)],
                Stroke::new(1.5, gold),
            );
        }
        x += 4.0;
    }
    // Laptops name slots like "Controller0-ChannelA-DIMM0": cut to the
    // stick's width, whole in the hover below.
    let font = FontId::proportional(10.0);
    let slot = elide(painter, &m.slot, &font, rect.width() - 4.0);
    painter.text(
        pos2(rect.center().x, rect.bottom() - 7.0),
        egui::Align2::CENTER_CENTER,
        &slot,
        font,
        ui.visuals().weak_text_color(),
    );
    let details: Vec<String> = [
        m.slot.clone(),
        format!("{} {}", m.maker, m.part).trim().to_owned(),
        m.kind.unwrap_or_default().to_owned(),
        m.form.unwrap_or_default().to_owned(),
        m.mhz.map(|v| format!("{v} MHz")).unwrap_or_default(),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect();
    if !details.is_empty() {
        response.on_hover_text(details.join(" · "));
    }
}

/// Thread dots per core square. Only when every core runs the same number
/// of threads: a hybrid processor (16 cores, 22 threads) mixes them, and
/// which core runs how many is not reported, so it gets none.
fn threads_per_core(cores: u32, threads: u32) -> u32 {
    if cores > 0 && threads.is_multiple_of(cores) {
        (threads / cores).clamp(1, 4)
    } else {
        0
    }
}

/// `text` cut with an ellipsis to fit `width` in `font`.
fn elide(painter: &egui::Painter, text: &str, font: &FontId, width: f32) -> String {
    let fits = |s: &str| {
        painter
            .layout_no_wrap(s.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x
            <= width
    };
    if fits(text) {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    for keep in (1..chars.len()).rev() {
        let cut: String = chars[..keep].iter().chain(['…'].iter()).collect();
        if fits(&cut) {
            return cut;
        }
    }
    "…".to_owned()
}

fn storage_card(ui: &mut Ui, spec: &Spec) {
    part_card(ui, crate::i18n::tr("Storage"), STORAGE, drive_icon, |ui| {
        let largest = spec
            .drives
            .iter()
            .filter_map(|d| d.bytes)
            .max()
            .unwrap_or(0);
        let color = theme::text_color(ui, STORAGE);
        for d in &spec.drives {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&d.model).strong());
                if let Some(size) = d.bytes {
                    theme::right(ui, RichText::new(bytes(size)).strong().color(color));
                }
            });
            // Drives side by side by size.
            if let Some(size) = d.bytes.filter(|_| largest > 0) {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 6.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 3.0, theme::track_color(ui));
                let share = (size as f32 / largest as f32).clamp(0.03, 1.0);
                ui.painter().rect_filled(
                    Rect::from_min_size(rect.min, vec2(rect.width() * share, rect.height())),
                    3.0,
                    color,
                );
            }
            pills(
                ui,
                &[
                    d.index
                        .map(|i| crate::i18n::format("Disk {0}", &[i.to_string()]))
                        .unwrap_or_default(),
                    d.interface.clone(),
                    firmware(&d.firmware),
                ],
            );
        }
    });
}

fn firmware(version: &str) -> String {
    if version.is_empty() {
        String::new()
    } else {
        crate::i18n::format("Firmware {0}", &[version.to_owned()])
    }
}

fn graphics_card(ui: &mut Ui, gpus: &[Gpu]) {
    part_card(
        ui,
        crate::i18n::tr("Graphics"),
        theme::GPU,
        gpu_icon,
        |ui| {
            for g in gpus {
                ui.label(RichText::new(&g.name).strong().size(15.0));
                let driver = if g.driver.is_empty() {
                    String::new()
                } else {
                    crate::i18n::format("Driver {0}", std::slice::from_ref(&g.driver))
                };
                let maker = if g.name.to_lowercase().contains(&g.maker.to_lowercase()) {
                    String::new()
                } else {
                    g.maker.clone()
                };
                pills(ui, &[maker, driver]);
            }
        },
    );
}

fn board_card(ui: &mut Ui, board: &Board, system_type: &str) {
    part_card(
        ui,
        crate::i18n::tr("Motherboard and firmware"),
        BOARD,
        board_icon,
        |ui| {
            let name = format!("{} {}", board.maker, board.product)
                .trim()
                .to_owned();
            if !name.is_empty() {
                ui.label(RichText::new(name).strong().size(15.0));
            }
            let bios = if board.bios.is_empty() {
                String::new()
            } else {
                crate::i18n::format("BIOS {0}", std::slice::from_ref(&board.bios))
            };
            pills(
                ui,
                &[
                    board.version.clone(),
                    bios,
                    board.bios_date.clone(),
                    system_type.to_owned(),
                ],
            );
        },
    );
}

// Drawings: simple line art in the part's colour, 64 × 64.

fn chip_icon(p: &egui::Painter, rect: Rect, color: Color32) {
    let mut shapes = Vec::new();
    chip_shapes(&mut shapes, rect, color);
    p.extend(shapes);
}

/// A processor package with pins on every side, scaled to `rect`.
fn chip_shapes(shapes: &mut Vec<egui::Shape>, rect: Rect, color: Color32) {
    let unit = rect.width() / 64.0;
    let body = rect.shrink(14.0 * unit);
    let pin = 7.0 * unit;
    let stroke = Stroke::new(2.0 * unit, color.gamma_multiply(0.8));
    let pins = 5;
    for i in 0..pins {
        let f = i as f32 / (pins - 1) as f32;
        let t = body.left() + 4.0 * unit + f * (body.width() - 8.0 * unit);
        let s = body.top() + 4.0 * unit + f * (body.height() - 8.0 * unit);
        for segment in [
            [pos2(t, body.top() - pin), pos2(t, body.top())],
            [pos2(t, body.bottom()), pos2(t, body.bottom() + pin)],
            [pos2(body.left() - pin, s), pos2(body.left(), s)],
            [pos2(body.right(), s), pos2(body.right() + pin, s)],
        ] {
            shapes.push(egui::Shape::line_segment(segment, stroke));
        }
    }
    let radius = 4.0 * unit;
    shapes.push(egui::Shape::rect_filled(
        body,
        radius,
        color.gamma_multiply(0.25),
    ));
    shapes.push(egui::Shape::rect_stroke(
        body,
        radius,
        Stroke::new(1.5 * unit, color),
        egui::StrokeKind::Inside,
    ));
    shapes.push(egui::Shape::rect_filled(
        body.shrink(9.0 * unit),
        2.0 * unit,
        color.gamma_multiply(0.85),
    ));
}

fn dimm_icon(p: &egui::Painter, rect: Rect, color: Color32) {
    for (i, y) in [rect.top() + 12.0, rect.top() + 34.0]
        .into_iter()
        .enumerate()
    {
        let stick = Rect::from_min_size(
            pos2(rect.left() + 2.0 + i as f32 * 4.0, y),
            vec2(56.0, 18.0),
        );
        p.rect_filled(stick, 2.0, color.gamma_multiply(0.25));
        p.rect_stroke(
            stick,
            2.0,
            Stroke::new(1.5, color),
            egui::StrokeKind::Inside,
        );
        for c in 0..4 {
            p.rect_filled(
                Rect::from_min_size(
                    pos2(stick.left() + 4.0 + c as f32 * 13.0, stick.top() + 3.0),
                    vec2(9.0, 7.0),
                ),
                1.0,
                color.gamma_multiply(0.85),
            );
        }
    }
}

fn drive_icon(p: &egui::Painter, rect: Rect, color: Color32) {
    // An M.2 drive: controller, two flash packages and the key notch.
    let stick = Rect::from_center_size(rect.center(), vec2(60.0, 22.0));
    p.rect_filled(stick, 3.0, color.gamma_multiply(0.25));
    p.rect_stroke(
        stick,
        3.0,
        Stroke::new(1.5, color),
        egui::StrokeKind::Inside,
    );
    p.rect_filled(
        Rect::from_min_size(stick.min + vec2(5.0, 5.0), vec2(10.0, 12.0)),
        1.5,
        color.gamma_multiply(0.6),
    );
    for i in 0..2 {
        p.rect_filled(
            Rect::from_min_size(
                stick.min + vec2(19.0 + i as f32 * 16.0, 4.0),
                vec2(13.0, 14.0),
            ),
            1.5,
            color.gamma_multiply(0.85),
        );
    }
    p.circle_stroke(
        pos2(stick.right() - 4.0, stick.center().y),
        2.5,
        Stroke::new(1.2, color),
    );
}

fn gpu_icon(p: &egui::Painter, rect: Rect, color: Color32) {
    let card = Rect::from_center_size(rect.center() + vec2(2.0, 0.0), vec2(56.0, 30.0));
    p.rect_filled(card, 4.0, color.gamma_multiply(0.22));
    p.rect_stroke(card, 4.0, Stroke::new(1.5, color), egui::StrokeKind::Inside);
    // The bracket and the two fans.
    p.line_segment(
        [
            pos2(card.left() - 4.0, card.top() - 2.0),
            pos2(card.left() - 4.0, card.bottom() + 6.0),
        ],
        Stroke::new(2.0, color),
    );
    for i in 0..2 {
        let c = pos2(card.left() + 15.0 + i as f32 * 26.0, card.center().y);
        p.circle_stroke(c, 10.0, Stroke::new(1.5, color));
        for blade in 0..3 {
            let a = blade as f32 * std::f32::consts::TAU / 3.0 + 0.4;
            p.line_segment(
                [c, c + vec2(a.cos(), a.sin()) * 8.0],
                Stroke::new(1.5, color.gamma_multiply(0.8)),
            );
        }
        p.circle_filled(c, 2.5, color);
    }
    p.line_segment(
        [
            pos2(card.left() + 8.0, card.bottom() + 3.0),
            pos2(card.right() - 10.0, card.bottom() + 3.0),
        ],
        Stroke::new(2.0, Color32::from_rgb(214, 170, 72)),
    );
}

fn board_icon(p: &egui::Painter, rect: Rect, color: Color32) {
    let board = rect.shrink(6.0);
    p.rect_filled(board, 3.0, color.gamma_multiply(0.18));
    p.rect_stroke(
        board,
        3.0,
        Stroke::new(1.5, color),
        egui::StrokeKind::Inside,
    );
    // A socket, memory slots and traces.
    let socket = Rect::from_min_size(board.min + vec2(8.0, 8.0), vec2(18.0, 18.0));
    p.rect_stroke(
        socket,
        2.0,
        Stroke::new(1.5, color),
        egui::StrokeKind::Inside,
    );
    for i in 0..3 {
        let x = board.left() + 32.0 + i as f32 * 5.0;
        p.line_segment(
            [pos2(x, board.top() + 6.0), pos2(x, board.top() + 28.0)],
            Stroke::new(2.0, color.gamma_multiply(0.8)),
        );
    }
    let trace = Stroke::new(1.0, color.gamma_multiply(0.6));
    for i in 0..3 {
        let y = board.top() + 34.0 + i as f32 * 5.0;
        p.line_segment(
            [pos2(board.left() + 6.0, y), pos2(board.right() - 14.0, y)],
            trace,
        );
    }
    p.rect_filled(
        Rect::from_min_size(
            pos2(board.right() - 12.0, board.bottom() - 14.0),
            vec2(8.0, 8.0),
        ),
        1.0,
        color.gamma_multiply(0.85),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(json: &[u8]) -> Vec<Item> {
        crate::inventory::parse(json).unwrap()
    }

    #[test]
    fn a_spec_is_built_from_the_inventory_without_guessing() {
        let spec = Spec::from_items(&items(br#"[
            {"kind":"System","fields":["Manufacturer: Contoso","Model: Contoso Studio 14","SystemType: x64-based PC"]},
            {"kind":"CPU","fields":["Name: Example 8-core","NumberOfCores: 8","NumberOfLogicalProcessors: 16","MaxClockSpeed: 4700","L2CacheSize: 8192","L3CacheSize: 0"]},
            {"kind":"RAM","fields":["Capacity: 17179869184","SMBIOSMemoryType: 34","DeviceLocator: A1","ConfiguredClockSpeed: 5600"]},
            {"kind":"RAM","fields":["Capacity: 17179869184","SMBIOSMemoryType: 34","DeviceLocator: B1"]},
            {"kind":"RAM","fields":[]},
            {"kind":"Disk","fields":["Index: 1","Model: Second","Size: 1000"]},
            {"kind":"Disk","fields":["Index: 0","Model: First","Size: 3000"]},
            {"kind":"BIOS","fields":["SMBIOSBIOSVersion: 1.2"]}
        ]"#));
        // The maker is not repeated when the model names it.
        assert_eq!(spec.title(), "Contoso Studio 14");
        assert_eq!((spec.cores(), spec.threads()), (Some(8), Some(16)));
        assert_eq!(spec.cpus[0].l2, Some(8 << 20));
        // A reported 0 is no size at all.
        assert_eq!(spec.cpus[0].l3, None);
        assert_eq!(spec.modules.len(), 2, "an empty module is left out");
        assert_eq!(spec.memory_total(), Some(32 << 30));
        assert_eq!(spec.modules[0].kind, Some("DDR5"));
        assert_eq!(spec.drives[0].model, "First", "drives in disk order");
        assert_eq!(spec.storage_total(), Some(4000));
        assert!(spec.gpus.is_empty());
        let board = spec.board.unwrap();
        assert_eq!((board.bios.as_str(), board.product.as_str()), ("1.2", ""));
        assert_eq!(clock(4700), "4.7 GHz");
        assert_eq!(clock(800), "800 MHz");
    }

    #[test]
    fn the_page_shows_the_spec_sheet_and_keeps_every_field_one_click_away() {
        use egui_kittest::kittest::Queryable;
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .with_size([1300.0, 1200.0])
            .build_ui(|ui| {
                egui::ScrollArea::vertical().show(ui, crate::inventory::specifications);
            });
        crate::inventory::set_test_inventory(
            &h.ctx,
            Some(Ok(br#"[
                {"kind":"System","fields":["Manufacturer: Contoso","Model: Studio","SystemType: x64-based PC"]},
                {"kind":"CPU","fields":["Name: Example 8-core","NumberOfCores: 8","NumberOfLogicalProcessors: 16","MaxClockSpeed: 4700","L3CacheSize: 32768"]},
                {"kind":"RAM","fields":["Capacity: 17179869184","SMBIOSMemoryType: 34","DeviceLocator: DIMM A1","ConfiguredClockSpeed: 5600"]},
                {"kind":"RAM","fields":["Capacity: 17179869184","SMBIOSMemoryType: 34","DeviceLocator: DIMM B1"]},
                {"kind":"Disk","fields":["Index: 0","Model: Fast SSD","Size: 2199023255552","InterfaceType: SCSI","FirmwareRevision: 2.0"]},
                {"kind":"GPU","fields":["Name: Example graphics","AdapterCompatibility: Example","DriverVersion: 32.1"]},
                {"kind":"Motherboard","fields":["Manufacturer: Boards","Product: Pro"]},
                {"kind":"BIOS","fields":["SMBIOSBIOSVersion: 3.10"]}
            ]"#)),
        );
        h.run();
        for text in [
            "Contoso Studio",
            "This PC",
            "32.0 GiB",
            "2.0 TiB",
            "4.7 GHz",
            "DDR5",
            "5600 MHz",
            "Fast SSD",
            "Firmware 2.0",
            "Driver 32.1",
            "BIOS 3.10",
            "Boards Pro",
        ] {
            assert!(
                h.query_all_by_label_contains(text).next().is_some(),
                "{text}"
            );
        }
        // The tables are folded away until asked for.
        assert!(h.query_by_label("Logical processors").is_none());
        h.get_by_label("All details").click();
        h.run();
        assert!(h.query_by_label("Logical processors").is_some());
    }

    #[test]
    fn hybrid_processors_get_no_guessed_threads() {
        assert_eq!(threads_per_core(8, 16), 2);
        assert_eq!(threads_per_core(8, 8), 1);
        assert_eq!(threads_per_core(16, 22), 0);
        assert_eq!(threads_per_core(0, 4), 0);
    }

    #[test]
    fn long_slot_names_are_cut_to_the_stick() {
        let mut h = egui_kittest::Harness::builder().build_ui(|ui| {
            let painter = ui.painter();
            let font = FontId::proportional(10.0);
            assert_eq!(elide(painter, "DIMM A1", &font, 128.0), "DIMM A1");
            let cut = elide(
                painter,
                "Controller0-ChannelA-DIMM0-Bank7-Rank1",
                &font,
                128.0,
            );
            assert!(cut.ends_with('…') && cut.chars().count() < 38, "{cut}");
            let width = painter
                .layout_no_wrap(cut, font.clone(), Color32::WHITE)
                .size()
                .x;
            assert!(width <= 128.0, "{width}");
        });
        h.run();
    }

    #[test]
    fn an_empty_inventory_names_nothing() {
        let spec = Spec::from_items(&[]);
        assert_eq!(spec.title(), "");
        assert_eq!(
            (spec.cores(), spec.memory_total(), spec.storage_total()),
            (None, None, None)
        );
        assert!(spec.board.is_none());
    }
}
