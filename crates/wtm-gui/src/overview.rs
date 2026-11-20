//! Overview tab: the machine at a glance, one-click protection, the busiest
//! apps with quick actions, and what the service did recently.

use std::collections::{HashMap, VecDeque};

use eframe::egui::{self, Color32, RichText, Ui};
use wtm_core::ProcessInfo;
use wtm_core::control::{EcoQos, ProcessRef};
// Shown text: written with the language's decimal sign.
use crate::i18n::bytes as format_bytes;
use wtm_ipc::Command;
use wtm_policy::{Config, Preset, set_preset};

use crate::backend::{Data, Mode, UiCommand};
use crate::overview_layout::Card;
use crate::processes::Intent;

use crate::theme::{BAD, GOOD, WARN, load_color};

/// Half the machine is alarming; one full core is worth noticing. Less is
/// ordinary text.
fn app_color(cpu: f32, logical_cpus: u32) -> Option<Color32> {
    if cpu >= 50.0 {
        Some(BAD)
    } else if cpu >= 100.0 / logical_cpus.max(1) as f32 {
        Some(WARN)
    } else {
        None
    }
}

/// Side by side from this width, otherwise one below the other.
const TWO_CARDS: f32 = 1100.0;

/// A resource card opens the corresponding performance section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resource {
    Cpu,
    Memory,
    Gpu,
    Disk,
    Network,
}

/// One resource at a glance: a ring for loads that have a ceiling, or a
/// big value for rates, the parts behind it, and the last minute as a
/// sparkline.
struct Stat<'a> {
    resource: Resource,
    title: &'static str,
    accent: Color32,
    /// 0..=100 for loads; `None` for rates.
    ring: Option<f32>,
    value: String,
    detail: String,
    band: Band<'a>,
    series: Vec<(&'a VecDeque<f32>, Color32)>,
    /// Top of the sparkline; `None` scales to the largest value shown.
    max: Option<f32>,
}

/// What a card shows between its reading and its history: what the one
/// number is made of.
enum Band<'a> {
    None,
    /// One cell per logical processor, 0..=100.
    Cores(&'a [f32]),
    /// Parts of a whole, left to right: amount, colour (transparent shows
    /// the empty track) and what the part is, for the tooltip.
    Stack(Vec<(f64, Color32, String)>),
    /// Up to three named meters, 0..=100.
    Meters(Vec<(String, f32)>),
}

impl Stat<'_> {
    /// The reading the card shows: the ring's percentage, or the value.
    fn current(&self) -> String {
        self.ring.map_or_else(
            || self.value.clone(),
            |p| crate::i18n::percent(format!("{p:.0}")),
        )
    }
}

const STAT_HEIGHT: f32 = 188.0;
/// Ring or value block at the top of every card, so all cards line up.
const STAT_HEADER: f32 = 80.0;
/// The parts strip between the reading and the sparkline.
const STAT_BAND: f32 = 22.0;
/// Seconds of history in a card's sparkline.
const STAT_SECONDS: usize = 60;
const RING: f32 = 54.0;

/// `a` moved towards `b` by `t` (0..=1), channel by channel.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

/// A rounded rectangle filled from `top` at its top edge to `bottom` at
/// three quarters of its height and below: the tinted glow of a card.
fn gradient_fill(rect: egui::Rect, radius: f32, top: Color32, bottom: Color32) -> egui::Shape {
    let shade = |y: f32| {
        let t = ((y - rect.top()) / (rect.height() * 0.75).max(1.0)).clamp(0.0, 1.0);
        mix(top, bottom, t)
    };
    let r = radius
        .min(rect.width() / 2.0)
        .min(rect.height() / 2.0)
        .max(0.0);
    let mut mesh = egui::epaint::Mesh::default();
    let c = rect.center();
    mesh.colored_vertex(c, shade(c.y));
    // The outline is convex, so a fan from the centre fills it.
    let corners = [
        (egui::pos2(rect.right() - r, rect.top() + r), -90.0_f32),
        (egui::pos2(rect.right() - r, rect.bottom() - r), 0.0),
        (egui::pos2(rect.left() + r, rect.bottom() - r), 90.0),
        (egui::pos2(rect.left() + r, rect.top() + r), 180.0),
    ];
    for (center, start) in corners {
        for i in 0..=6 {
            let a = (start + 90.0 * i as f32 / 6.0).to_radians();
            let p = center + r * egui::vec2(a.cos(), a.sin());
            mesh.colored_vertex(p, shade(p.y));
        }
    }
    let n = mesh.vertices.len() as u32 - 1;
    for i in 1..=n {
        mesh.add_triangle(0, i, if i == n { 1 } else { i + 1 });
    }
    egui::Shape::mesh(mesh)
}

/// How strongly a card is tinted with its colour at the top.
fn glow_strength(ui: &Ui) -> f32 {
    if ui.visuals().dark_mode { 0.11 } else { 0.07 }
}

/// Draws `band` in `rect`; returns what the part under `pointer` is.
fn paint_band(
    ui: &Ui,
    rect: egui::Rect,
    band: &Band<'_>,
    accent: Color32,
    pointer: Option<egui::Pos2>,
) -> Option<String> {
    if !(rect.width() > 4.0 && rect.height() > 4.0) {
        return None;
    }
    let painter = ui.painter_at(rect.expand(1.0));
    let track = crate::theme::track_color(ui);
    let fill_of = |load: f32| {
        if load >= 70.0 {
            load_color(load)
        } else {
            accent
        }
    };
    match band {
        Band::None => None,
        Band::Cores(cores) => {
            let n = cores.len();
            if n == 0 {
                return None;
            }
            let gap = if n > 48 { 1.0 } else { 2.0 };
            let per_row = (((rect.width() + gap) / (6.0 + gap)).floor() as usize).max(1);
            let rows = n.div_ceil(per_row).clamp(1, 4);
            let cols = n.div_ceil(rows);
            let w = (rect.width() - gap * (cols - 1) as f32) / cols as f32;
            // Cells no taller than a bar, centred in the strip.
            let h = ((rect.height() - gap * (rows - 1) as f32) / rows as f32).min(10.0);
            if w < 1.0 || h < 1.0 {
                return None;
            }
            let top = rect.center().y - (h * rows as f32 + gap * (rows - 1) as f32) / 2.0;
            let mut hovered = None;
            for (i, &load) in cores.iter().enumerate() {
                let (row, col) = (i / cols, i % cols);
                let cell = egui::Rect::from_min_size(
                    egui::pos2(
                        rect.left() + col as f32 * (w + gap),
                        top + row as f32 * (h + gap),
                    ),
                    egui::vec2(w, h),
                );
                let load = if load.is_finite() {
                    load.clamp(0.0, 100.0)
                } else {
                    0.0
                };
                let color = if load >= 70.0 {
                    load_color(load)
                } else {
                    mix(track, accent, 0.18 + 0.82 * load / 70.0)
                };
                painter.rect_filled(cell, (w.min(h) / 3.0).min(2.0), color);
                if pointer.is_some_and(|p| cell.expand(gap / 2.0).contains(p)) {
                    hovered = Some(format!(
                        "{}: {}",
                        crate::i18n::format("CPU {0}", &[i.to_string()]),
                        crate::i18n::percent(format!("{load:.0}"))
                    ));
                }
            }
            hovered
        }
        Band::Stack(parts) => {
            let total: f64 = parts.iter().map(|p| p.0.max(0.0)).sum();
            let bar = egui::Rect::from_center_size(
                rect.center(),
                egui::vec2(rect.width(), 10.0_f32.min(rect.height())),
            );
            let radius = bar.height() / 2.0;
            painter.rect_filled(bar, radius, track);
            if total <= 0.0 {
                return None;
            }
            let mut x = bar.left();
            for (i, (amount, color, _)) in parts.iter().enumerate() {
                let w = (amount.max(0.0) / total) as f32 * bar.width();
                let gap = if i + 1 < parts.len() { 1.0 } else { 0.0 };
                if *color != Color32::TRANSPARENT && w > gap {
                    // The whole rounded bar clipped to the part: only the
                    // ends of the bar are rounded.
                    let part = egui::Rect::from_x_y_ranges(x..=x + w - gap, bar.y_range());
                    painter
                        .with_clip_rect(part)
                        .rect_filled(bar, radius, *color);
                }
                x += w;
            }
            pointer.filter(|p| rect.contains(*p)).map(|_| {
                parts
                    .iter()
                    .filter(|p| !p.2.is_empty())
                    .map(|p| p.2.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        Band::Meters(meters) => {
            let gap = 12.0;
            // As many as leave each name room to be read.
            let fit = (((rect.width() + gap) / (84.0 + gap)).floor() as usize).max(1);
            let shown = &meters[..meters.len().min(3).min(fit)];
            if shown.is_empty() {
                return None;
            }
            let w = (rect.width() - gap * (shown.len() - 1) as f32) / shown.len() as f32;
            if w < 8.0 {
                return None;
            }
            let font = egui::FontId::proportional(11.0);
            let weak = ui.visuals().weak_text_color();
            let strong = ui.visuals().text_color();
            let mut hovered = None;
            for (i, (name, load)) in shown.iter().enumerate() {
                let load = if load.is_finite() {
                    load.clamp(0.0, 100.0)
                } else {
                    0.0
                };
                let col = egui::Rect::from_min_size(
                    egui::pos2(rect.left() + i as f32 * (w + gap), rect.top()),
                    egui::vec2(w, rect.height()),
                );
                let value = crate::i18n::percent(format!("{load:.0}"));
                let value_galley = painter.layout_no_wrap(value.clone(), font.clone(), strong);
                let value_width = value_galley.size().x;
                painter.galley(
                    egui::pos2(col.right() - value_width, col.top()),
                    value_galley,
                    strong,
                );
                let name_clip = egui::Rect::from_x_y_ranges(
                    col.left()..=(col.right() - value_width - 4.0).max(col.left()),
                    col.y_range(),
                );
                painter.with_clip_rect(name_clip).text(
                    col.left_top(),
                    egui::Align2::LEFT_TOP,
                    name,
                    font.clone(),
                    weak,
                );
                let bar = egui::Rect::from_min_max(
                    egui::pos2(col.left(), col.bottom() - 4.0),
                    col.right_bottom(),
                );
                painter.rect_filled(bar, 2.0, track);
                let filled = egui::Rect::from_min_size(
                    bar.min,
                    egui::vec2(bar.width() * load / 100.0, bar.height()),
                );
                if filled.width() > 0.5 {
                    painter.rect_filled(filled, 2.0, fill_of(load));
                }
                if pointer.is_some_and(|p| col.contains(p)) {
                    hovered = Some(format!("{name}: {value}"));
                }
            }
            hovered
        }
    }
}

fn stat_card(ui: &mut Ui, s: &Stat<'_>) -> egui::Response {
    // Reserved before the frame, so the glow sits under its outline.
    let glow = ui.painter().add(egui::Shape::Noop);
    let pointer = ui.ctx().pointer_hover_pos();
    let mut band_hover = None;
    let frame = egui::Frame::new()
        .stroke(egui::Stroke::new(
            1.0,
            ui.visuals()
                .widgets
                .noninteractive
                .bg_stroke
                .color
                .gamma_multiply(0.75),
        ))
        .corner_radius(crate::theme::SURFACE_RADIUS)
        .inner_margin(crate::theme::CONTENT_PADDING)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // A short window keeps more of the page in view: the sparkline
            // gives up height first.
            let height = if ui.ctx().content_rect().height() < 640.0 {
                STAT_HEIGHT - 24.0
            } else {
                STAT_HEIGHT
            };
            ui.set_height(height - 24.0);
            let header = egui::vec2(ui.available_width(), STAT_HEADER);
            ui.allocate_ui_with_layout(
                header,
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_height(STAT_HEADER);
                    let meter_width = if s.ring.is_some() { RING } else { 0.0 };
                    ui.allocate_ui_with_layout(
                        egui::vec2((header.x - meter_width - 8.0).max(0.0), STAT_HEADER),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.spacing_mut().item_spacing.y = 3.0;
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                let (dot, _) = ui.allocate_exact_size(
                                    egui::vec2(8.0, 14.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().circle_filled(dot.center(), 3.5, s.accent);
                                ui.label(
                                    RichText::new(s.title)
                                        .size(13.0)
                                        .strong()
                                        .color(crate::theme::text_color(ui, s.accent)),
                                );
                            });
                            let value = s.current();
                            let color = s
                                .ring
                                .filter(|p| *p >= 70.0)
                                .map_or(ui.visuals().strong_text_color(), |p| {
                                    crate::theme::text_color(ui, load_color(p))
                                });
                            ui.label(RichText::new(value).size(30.0).color(color));
                            ui.add(
                                egui::Label::new(RichText::new(&s.detail).size(12.0).weak())
                                    .truncate(),
                            )
                            .on_hover_text(&s.detail);
                        },
                    );
                    if let Some(percent) = s.ring {
                        let color = if percent >= 70.0 {
                            load_color(percent)
                        } else {
                            s.accent
                        };
                        crate::theme::ring_gauge(ui, RING, percent, color, "");
                    }
                },
            );
            ui.add_space(2.0);
            let (band, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), STAT_BAND),
                egui::Sense::hover(),
            );
            band_hover = paint_band(ui, band, &s.band, s.accent, pointer);
            ui.add_space(4.0);
            let height = ui.available_height().max(20.0);
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), height),
                egui::Sense::hover(),
            );
            let max = s.max.unwrap_or_else(|| {
                s.series
                    .iter()
                    .flat_map(|(v, _)| v.iter().rev().take(STAT_SECONDS).copied())
                    .fold(1024.0, f32::max)
            });
            let grid = ui.visuals().weak_text_color().gamma_multiply(0.10);
            for fraction in [0.0, 0.5, 1.0] {
                let y = rect.bottom() - 2.5 - (rect.height() - 5.0) * fraction;
                ui.painter().line_segment(
                    [
                        egui::pos2(rect.left() + 2.5, y),
                        egui::pos2(rect.right() - 2.5, y),
                    ],
                    egui::Stroke::new(0.5, grid),
                );
            }
            for (values, color) in &s.series {
                let tail: VecDeque<f32> = values
                    .iter()
                    .skip(values.len().saturating_sub(STAT_SECONDS))
                    .copied()
                    .collect();
                crate::theme::sparkline(ui.painter(), rect, &tail, max, STAT_SECONDS, *color);
            }
        });
    let fill = crate::theme::card_fill(ui);
    ui.painter().set(
        glow,
        gradient_fill(
            frame.response.rect,
            f32::from(crate::theme::SURFACE_RADIUS.nw),
            mix(fill, s.accent, glow_strength(ui)),
            fill,
        ),
    );
    // A distinct id keeps the frame's hover-only response from relinquishing
    // the button's keyboard focus at the beginning of the next frame.
    let response = ui.interact(
        frame.response.rect,
        frame.response.id.with("open"),
        egui::Sense::click(),
    );
    let value = s.current();
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::Role::Button,
            ui.is_enabled(),
            format!("{}: {value} · {}", s.title, s.detail),
        )
    });
    let color = if response.hovered() || response.has_focus() {
        ui.painter().rect_stroke(
            response.rect,
            crate::theme::SURFACE_RADIUS,
            egui::Stroke::new(1.5, s.accent),
            egui::StrokeKind::Inside,
        );
        crate::theme::text_color(ui, s.accent)
    } else {
        ui.visuals().weak_text_color()
    };
    ui.painter().text(
        response.rect.right_top() + egui::vec2(-10.0, 9.0),
        egui::Align2::RIGHT_TOP,
        "↗",
        egui::FontId::proportional(13.0),
        color,
    );
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    match band_hover {
        Some(part) => response.on_hover_text(part),
        None => response.on_hover_text(crate::i18n::tr("View resource details")),
    }
}

/// The cards in as many equal columns as fit, wrapping to further rows.
fn stat_row(ui: &mut Ui, stats: &[Stat<'_>], intents: &mut Vec<Intent>) {
    let width = ui.available_width();
    let per_row = if width >= 1000.0 {
        stats.len()
    } else if width >= 640.0 {
        3
    } else if width >= 320.0 {
        2
    } else {
        // Native startup/measurement can offer zero width. Multiple columns
        // would subtract their gutters and pass a negative width to egui.
        1
    };
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        for chunk in stats.chunks(per_row.max(1)) {
            ui.columns(chunk.len(), |cols| {
                for (col, s) in cols.iter_mut().zip(chunk) {
                    // Keyed by resource, so focus and menus follow a moved card.
                    let response = col.push_id(s.resource, |ui| stat_card(ui, s)).inner;
                    let mut open = response.clicked();
                    crate::context_actions::menu(
                        &response,
                        ("overview-resource", response.id),
                        |ui| {
                            crate::context_actions::copy(ui, crate::i18n::tr("Current"), || {
                                s.current()
                            });
                            open |= crate::context_actions::action(ui, crate::i18n::tr("Details"));
                        },
                    );
                    if open {
                        intents.push(Intent::ShowResource(s.resource));
                    }
                }
            });
        }
    });
}

fn rate(bytes_per_sec: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

/// The parts strips of the cards other than the CPU's.
struct Bands {
    memory: Band<'static>,
    gpu: Band<'static>,
    disk: Band<'static>,
    network: Band<'static>,
}

impl Bands {
    fn of(data: &Data, s: &wtm_core::SystemStats) -> Self {
        use crate::theme::{DISK_READ, DISK_WRITE, MEMORY, NET_RX, NET_TX};
        let part = |label: &str, bytes: u64| format!("{label}: {}", format_bytes(bytes as f64));
        // In use, modified, standby and free pages, as the Memory page
        // draws them; without the page lists, what is in use of the total.
        let memory = match data
            .memory_composition
            .as_ref()
            .and_then(|c| Some((c.total, c.modified?, c.standby?, c.free?)).filter(|p| p.0 > 0))
        {
            Some((total, modified, standby, free)) => {
                let in_use = total
                    .saturating_sub(modified)
                    .saturating_sub(standby)
                    .saturating_sub(free);
                Band::Stack(vec![
                    (
                        in_use as f64,
                        MEMORY,
                        part(crate::i18n::tr("In use"), in_use),
                    ),
                    (
                        modified as f64,
                        MEMORY.gamma_multiply(0.65),
                        part(crate::i18n::tr("Modified"), modified),
                    ),
                    (
                        standby as f64,
                        MEMORY.gamma_multiply(0.35),
                        part(crate::i18n::tr("Standby"), standby),
                    ),
                    (
                        free as f64,
                        Color32::TRANSPARENT,
                        part(crate::i18n::tr("Free"), free),
                    ),
                ])
            }
            None if s.memory_total > 0 => Band::Stack(vec![
                (
                    s.memory_used() as f64,
                    MEMORY,
                    part(crate::i18n::tr("In use"), s.memory_used()),
                ),
                (
                    s.memory_available as f64,
                    Color32::TRANSPARENT,
                    String::new(),
                ),
            ]),
            None => Band::None,
        };
        let mut engines: Vec<(String, f32)> = s
            .gpu_engines
            .iter()
            .filter(|(_, load)| load.is_finite())
            .cloned()
            .collect();
        engines.sort_by(|a, b| b.1.total_cmp(&a.1));
        engines.truncate(3);
        let gpu = if engines.is_empty() {
            Band::None
        } else {
            Band::Meters(engines)
        };
        // A disk is named by its drive letters, or by its number.
        let disks: Vec<(String, f32)> = s
            .disks
            .iter()
            .map(|d| {
                let letters = d.name.split_once(' ').map_or("", |(_, l)| l.trim());
                let name = if letters.is_empty() {
                    crate::i18n::format("Disk {0}", &[d.name.trim().to_owned()])
                } else {
                    letters.to_owned()
                };
                (name, d.active_percent)
            })
            .collect();
        let split = |a: f64, ca: Color32, la: String, b: f64, cb: Color32, lb: String| {
            if a + b > 0.0 {
                Band::Stack(vec![(a, ca, la), (b, cb, lb)])
            } else {
                Band::None
            }
        };
        let disk = if disks.is_empty() {
            split(
                s.io_read_rate,
                DISK_READ,
                format!("{}: {}", crate::i18n::tr("Read"), rate(s.io_read_rate)),
                s.io_write_rate,
                DISK_WRITE,
                format!("{}: {}", crate::i18n::tr("Write"), rate(s.io_write_rate)),
            )
        } else {
            Band::Meters(disks)
        };
        // How full each adapter's link is; without link speeds, the share of
        // the traffic each way.
        let adapters: Vec<(String, f32)> = s
            .net_adapters
            .iter()
            .filter(|a| a.link_speed > 0)
            .map(|a| {
                let used = (a.rx_rate + a.tx_rate) * 8.0 / a.link_speed as f64 * 100.0;
                (a.name.clone(), used as f32)
            })
            .collect();
        let network = if adapters.is_empty() {
            split(
                s.net_rx_rate,
                NET_RX,
                format!("↓ {}", rate(s.net_rx_rate)),
                s.net_tx_rate,
                NET_TX,
                format!("↑ {}", rate(s.net_tx_rate)),
            )
        } else {
            Band::Meters(adapters)
        };
        Self {
            memory,
            gpu,
            disk,
            network,
        }
    }
}

/// A full-width surface tinted with `color`, marked on its left edge.
fn tinted<R>(ui: &mut Ui, color: Color32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let strength = if ui.visuals().dark_mode { 0.15 } else { 0.11 };
    let out = egui::Frame::new()
        .fill(mix(crate::theme::card_fill(ui), color, strength))
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.35)))
        .corner_radius(crate::theme::SURFACE_RADIUS)
        .inner_margin(egui::Margin {
            left: 16,
            right: 12,
            top: 8,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        });
    let rect = out.response.rect;
    let mark = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 5.0, rect.top() + 9.0),
        egui::pos2(rect.left() + 8.0, rect.bottom() - 9.0),
    );
    if mark.height() > 0.0 {
        ui.painter().rect_filled(mark, 1.5, color);
    }
    out.inner
}

/// A filled disc with a white glyph in a soft halo.
fn disc(ui: &mut Ui, color: Color32, glyph: &str, diameter: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(diameter + 8.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.circle_filled(
        rect.center(),
        diameter / 2.0 + 4.0,
        color.gamma_multiply(0.22),
    );
    painter.circle_filled(rect.center(), diameter / 2.0, color);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(diameter * 0.55),
        Color32::WHITE,
    );
}

/// One of the machine's vital counts beside the verdict.
struct Vital {
    label: &'static str,
    value: String,
    color: Option<Color32>,
    hint: Option<String>,
}

const VITAL_GAP: f32 = 8.0;

fn vital_galleys(
    ui: &Ui,
    v: &Vital,
) -> (std::sync::Arc<egui::Galley>, std::sync::Arc<egui::Galley>) {
    let painter = ui.painter();
    let weak = ui.visuals().weak_text_color();
    let strong = v.color.map_or(ui.visuals().strong_text_color(), |c| {
        crate::theme::text_color(ui, c)
    });
    (
        painter.layout_no_wrap(v.label.to_owned(), egui::FontId::proportional(11.0), weak),
        painter.layout_no_wrap(v.value.clone(), egui::FontId::proportional(15.0), strong),
    )
}

fn vital_width(ui: &Ui, v: &Vital) -> f32 {
    let (label, value) = vital_galleys(ui, v);
    label.size().x.max(value.size().x) + 22.0
}

/// A label over a value in an inset chip.
fn vital(ui: &mut Ui, v: &Vital) {
    let (label, value) = vital_galleys(ui, v);
    let size = egui::vec2(label.size().x.max(value.size().x) + 22.0, 38.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let fill = if ui.visuals().dark_mode {
        Color32::from_black_alpha(70)
    } else {
        Color32::from_white_alpha(210)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 6.0, fill);
    painter.galley(
        rect.left_top() + egui::vec2(11.0, 3.0),
        label,
        Color32::PLACEHOLDER,
    );
    painter.galley(
        rect.left_top() + egui::vec2(11.0, 16.0),
        value,
        Color32::PLACEHOLDER,
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Label, true, format!("{}: {}", v.label, v.value))
    });
    if let Some(hint) = &v.hint {
        response.on_hover_text(hint);
    }
}

/// The verdict as the page's headline, with the machine's vital counts
/// beside it, or below it on a narrow window.
fn hero(ui: &mut Ui, color: Color32, text: &str, vitals: &[Vital]) {
    tinted(ui, color, |ui| {
        let glyph = if color == GOOD { "✔" } else { "!" };
        let chips: f32 = vitals.iter().map(|v| vital_width(ui, v) + VITAL_GAP).sum();
        let disc_width = 40.0 + ui.spacing().item_spacing.x;
        let one_row = ui.available_width() - disc_width - chips >= 280.0;
        ui.horizontal(|ui| {
            disc(ui, color, glyph, 30.0);
            let width = if one_row {
                ui.available_width() - chips
            } else {
                ui.available_width()
            };
            ui.allocate_ui_with_layout(
                egui::vec2(width.max(0.0), 38.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(text)
                                .size(17.0)
                                .color(crate::theme::text_color(ui, color)),
                        )
                        .wrap(),
                    );
                },
            );
            if one_row {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = VITAL_GAP;
                    for v in vitals.iter().rev() {
                        vital(ui, v);
                    }
                });
            }
        });
        if !one_row && !vitals.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(VITAL_GAP, VITAL_GAP);
                for v in vitals {
                    vital(ui, v);
                }
            });
        }
    });
}

/// Processes, threads, handles and the hottest sensor while it is fresh.
fn vitals(data: &Data, s: &wtm_core::SystemStats) -> Vec<Vital> {
    let count = |label: &'static str, n: u64| Vital {
        label,
        value: n.to_string(),
        color: None,
        hint: None,
    };
    let mut out = vec![
        count(crate::i18n::tr("Processes"), s.process_count as u64),
        count(crate::i18n::tr("Threads"), s.thread_count),
        count(crate::i18n::tr("Handles"), s.handle_count),
    ];
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let hottest = data
        .hardware
        .fresh(now)
        .then(|| {
            data.hardware
                .sensors
                .iter()
                .filter(|s| s.kind == wtm_core::hardware::SensorKind::Temperature)
                // An unconnected sensor often reads 0, -128, 127 or 128 °C:
                // only readings a running part can have.
                .filter_map(|s| Some((s, s.value.filter(|v| (1.0..=120.0).contains(v))?)))
                .max_by(|a, b| a.1.total_cmp(&b.1))
        })
        .flatten();
    if let Some((sensor, celsius)) = hottest {
        out.push(Vital {
            label: crate::i18n::tr("Temperature"),
            value: format!("{} °C", crate::i18n::decimal(format!("{celsius:.0}"))),
            color: (celsius >= 70.0).then_some(if celsius >= 85.0 { BAD } else { WARN }),
            hint: Some(format!("{} · {}", sensor.device, sensor.name)),
        });
    }
    out
}

/// A tinted notice with a glyph: what the service does right now.
fn notice(ui: &mut Ui, color: Color32, glyph: &str, text: &str) {
    tinted(ui, color, |ui| {
        ui.horizontal(|ui| {
            disc(ui, color, glyph, 22.0);
            ui.add(
                egui::Label::new(
                    RichText::new(text)
                        .size(15.0)
                        .color(crate::theme::text_color(ui, color)),
                )
                .wrap(),
            );
        });
    });
}

/// The game in front and what game mode does for it.
pub(crate) fn game_text(game: &wtm_ipc::GameStatus) -> String {
    crate::i18n::format(
        "Game mode: {0} is in front; {1} background apps calmed until it ends",
        &[game.name.clone(), game.calmed.to_string()],
    )
}

/// What the rescue core keeps free while the machine is overloaded.
pub(crate) fn rescue_text(rescue: &wtm_ipc::RescueStatus) -> String {
    let cpus: Vec<_> = rescue.cpus.iter().map(u8::to_string).collect();
    crate::i18n::format(
        "Rescue core active: CPU {0} kept free for RustWTM and Windows, {1} apps moved off it",
        &[cpus.join(", "), rescue.moved.to_string()],
    )
}

/// One app: all processes with the same image name.
pub(crate) struct App<'a, P = ProcessRef> {
    pub name: &'a str,
    pub cpu: f32,
    pub memory: u64,
    pub instances: Vec<P>,
}

pub(crate) fn busiest_apps(data: &Data, n: usize) -> Vec<App<'_>> {
    busiest(&app_totals(data), n)
}

/// The first `n` of `totals` with a reference to each of their processes.
fn busiest<'a>(totals: &[App<'a, &'a ProcessInfo>], n: usize) -> Vec<App<'a>> {
    totals
        .iter()
        .take(n)
        .map(|a| App {
            name: a.name,
            cpu: a.cpu,
            memory: a.memory,
            instances: a.instances.iter().map(|p| ProcessRef::from(*p)).collect(),
        })
        .collect()
}

/// Every app, busiest first. Its processes are borrowed: only the few apps
/// shown need their names copied into references.
fn app_totals(data: &Data) -> Vec<App<'_, &ProcessInfo>> {
    let mut apps: HashMap<String, App<'_, &ProcessInfo>> = HashMap::new();
    for p in data.snapshot.iter().flat_map(|s| &s.processes) {
        if p.pid == 0 || wtm_core::protection_reason(p.pid, &p.name).is_some() {
            continue;
        }
        let a = apps.entry(p.name.to_lowercase()).or_insert_with(|| App {
            name: &p.name,
            cpu: 0.0,
            memory: 0,
            instances: Vec::new(),
        });
        a.cpu += p.cpu_percent;
        a.memory += p.private_working_set;
        a.instances.push(p);
    }
    let mut out: Vec<_> = apps.into_values().collect();
    out.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.memory.cmp(&a.memory)));
    out
}

/// One sentence on how the machine is doing and who is responsible.
#[cfg(test)]
fn verdict(data: &Data) -> Option<(Color32, String)> {
    let swarms = data
        .snapshot
        .as_ref()
        .map(|s| wtm_policy::find_swarms(&s.processes, &data.swarm))
        .unwrap_or_default();
    verdict_of(data, &app_totals(data), &swarms)
}

/// `verdict` (a test helper) from the apps and swarms the page already found.
fn verdict_of(
    data: &Data,
    apps: &[App<'_, &ProcessInfo>],
    swarms: &[wtm_policy::Swarm<'_>],
) -> Option<(Color32, String)> {
    let s = &data.snapshot.as_ref()?.system;
    let mem = s.memory_used() as f32 / s.memory_total.max(1) as f32 * 100.0;
    let top_cpu = apps.first();
    let top_mem = apps.iter().max_by_key(|a| a.memory);
    let cpu_blame = || {
        top_cpu.map_or(String::new(), |a| {
            let __wtm_i18n_0 = &(a.name);
            let __wtm_i18n_1 = &(a.cpu);
            crate::i18n::format(
                ": {0} uses {1}% of it",
                &[__wtm_i18n_0.to_string(), format!("{:.0}", __wtm_i18n_1)],
            )
        })
    };
    let mem_blame = || {
        top_mem.map_or(String::new(), |a| {
            let __wtm_i18n_0 = &(a.name);
            let __wtm_i18n_1 = &(format_bytes(a.memory as f64));
            crate::i18n::format(
                ": {0} uses {1} of it",
                &[__wtm_i18n_0.to_string(), __wtm_i18n_1.to_string()],
            )
        })
    };
    Some(if s.cpu_percent >= 90.0 {
        (BAD, {
            let __wtm_i18n_0 = &(cpu_blame());
            crate::i18n::format("CPU is maxed out{0}", std::slice::from_ref(__wtm_i18n_0))
        })
    } else if mem >= 90.0 {
        (BAD, {
            let __wtm_i18n_0 = &(mem_blame());
            crate::i18n::format(
                "Memory is almost full{0}",
                std::slice::from_ref(__wtm_i18n_0),
            )
        })
    } else if s.cpu_percent >= 70.0 {
        (WARN, {
            let __wtm_i18n_0 = &(cpu_blame());
            crate::i18n::format("CPU is busy{0}", std::slice::from_ref(__wtm_i18n_0))
        })
    } else if mem >= 80.0 {
        (WARN, {
            let __wtm_i18n_0 = &(mem_blame());
            crate::i18n::format(
                "Memory is getting full{0}",
                std::slice::from_ref(__wtm_i18n_0),
            )
        })
    } else {
        match swarms.len() {
            0 => (
                GOOD,
                crate::i18n::tr("Everything runs smoothly.").to_owned(),
            ),
            1 => (WARN, {
                let __wtm_i18n_0 = &(swarms[0].members.len());
                let __wtm_i18n_1 = &(swarms[0].name);
                crate::i18n::format(
                    "{0} copies of {1} are running; see below.",
                    &[format!("{}", __wtm_i18n_0), __wtm_i18n_1.to_string()],
                )
            }),
            n => (WARN, {
                crate::i18n::format(
                    "{0} programs run hundreds of copies each; see below.",
                    &[format!("{}", n)],
                )
            }),
        }
    })
}

/// Swarm copies running this long count as stuck when the service does not
/// set its own limit.
const STUCK_MINS: u64 = 10;

/// Hundreds of copies of one program: who started them and one click to end
/// them all, or only those nothing waits for any more.
/// A strip per program whose window does not respond, with a way to find out why.
fn not_responding(ui: &mut Ui, data: &Data, procs: &[ProcessInfo], intents: &mut Vec<Intent>) {
    let mut hung: Vec<_> = data.hung.iter().collect();
    hung.sort();
    for (pid, title) in hung.into_iter().take(3) {
        let Some(p) = procs.iter().find(|p| p.pid == *pid) else {
            continue;
        };
        ui.add_space(0.0);
        tinted(ui, BAD, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(crate::i18n::format(
                        "⚠  {0} is not responding (\"{1}\")",
                        &[p.name.clone(), title.clone()],
                    ))
                    .size(15.0)
                    .color(crate::theme::text_color(ui, BAD)),
                );
                if ui
                    .button(crate::i18n::tr("Why?"))
                    .on_hover_text(crate::i18n::tr("Show it and analyze its wait chain"))
                    .clicked()
                {
                    let target = ProcessRef::from(p);
                    intents.push(Intent::Reveal(target.clone()));
                    intents.push(Intent::WaitChain(target));
                }
            });
        });
    }
}

fn swarms(
    ui: &mut Ui,
    data: &Data,
    procs: &[ProcessInfo],
    swarms: &[wtm_policy::Swarm<'_>],
    intents: &mut Vec<Intent>,
) {
    if swarms.is_empty() {
        return;
    }
    let by_pid: HashMap<u32, &ProcessInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    for s in swarms {
        ui.add_space(0.0);
        let title = crate::i18n::format(
            "⚠ {0} copies of {1} are running",
            &[s.members.len().to_string(), s.name.to_string()],
        );
        tinted(ui, WARN, |ui| {
            ui.label(
                RichText::new(&title)
                    .size(15.0)
                    .color(crate::theme::text_color(ui, WARN)),
            );
            ui.label({ let __wtm_i18n_0 = &(s.parents_text()); let __wtm_i18n_1 = &(format_bytes(s.memory as f64)); let __wtm_i18n_2 = &(s.cpu_percent); crate::i18n::format("Started by {0}. Together {1} of memory and {2}% CPU. Usually a tool that keeps starting helpers and never waits for them.", &[__wtm_i18n_0.to_string(), __wtm_i18n_1.to_string(), crate::i18n::decimal(format!("{:.1}", __wtm_i18n_2))]) });
            ui.horizontal_wrapped(|ui| {
                let all: Vec<ProcessRef> = s.members.iter().map(|p| ProcessRef::from(*p)).collect();
                if ui
                    .button(
                        RichText::new({
                            let __wtm_i18n_0 = &(all.len());
                            crate::i18n::format("End all {0}", &[format!("{}", __wtm_i18n_0)])
                        })
                        .color(crate::theme::text_color(ui, BAD)),
                    )
                    .clicked()
                {
                    intents.push(Intent::ConfirmMany(all, Command::Kill));
                }
                let orphans: Vec<ProcessRef> = s
                    .members
                    .iter()
                    .filter(|p| wtm_policy::live_parent(p, &by_pid).is_none())
                    .map(|p| ProcessRef::from(*p))
                    .collect();
                if !orphans.is_empty()
                    && ui
                        .button({
                            let __wtm_i18n_0 = &(orphans.len());
                            crate::i18n::format(
                                "End the {0} whose parent exited",
                                &[format!("{}", __wtm_i18n_0)],
                            )
                        })
                        .on_hover_text(crate::i18n::tr("Nothing waits for these any more"))
                        .clicked()
                {
                    intents.push(Intent::ConfirmMany(orphans, Command::Kill));
                }
                let mins = match data.swarm.end_after_mins {
                    0 => STUCK_MINS,
                    m => m,
                };
                let stuck: Vec<ProcessRef> = s
                    .older_than(
                        std::time::Duration::from_secs(mins * 60),
                        std::time::SystemTime::now(),
                    )
                    .into_iter()
                    .map(ProcessRef::from)
                    .collect();
                if !stuck.is_empty()
                    && stuck.len() < s.members.len()
                    && ui
                        .button({
                            let __wtm_i18n_0 = &(stuck.len());
                            crate::i18n::format(
                                "End the {0} running over {1} min",
                                &[format!("{}", __wtm_i18n_0), format!("{}", mins)],
                            )
                        })
                        .on_hover_text(crate::i18n::tr(
                            "Stuck helpers, with the console hosts they started",
                        ))
                        .clicked()
                {
                    intents.push(Intent::ConfirmMany(stuck, Command::KillTree));
                }
                if ui
                    .button(crate::i18n::tr("Add to kill list…"))
                    .on_hover_text(crate::i18n::tr(
                        "End them automatically or with the panic button",
                    ))
                    .clicked()
                {
                    intents.push(Intent::AddToKillList(s.name.clone()));
                }
                if ui.button(crate::i18n::tr("Show")).clicked() {
                    intents.push(Intent::Reveal(ProcessRef::from(s.members[0])));
                }
            });
        });
    }
}

/// Returns commands for the backend and intents for the app. `layout` says
/// which cards show and in what order; warnings always show.
pub fn overview(
    ui: &mut Ui,
    data: &Data,
    layout: &crate::overview_layout::Layout,
) -> (Vec<UiCommand>, Vec<Intent>) {
    let mut commands = Vec::new();
    let mut intents = Vec::new();
    // Found once per frame: the verdict, the swarm cards and the table share them.
    let apps = app_totals(data);

    if let Some(snap) = &data.snapshot {
        let swarm_list = wtm_policy::find_swarms(&snap.processes, &data.swarm);
        let s = &snap.system;
        let mem = if s.memory_total > 0 {
            s.memory_used() as f32 / s.memory_total as f32 * 100.0
        } else {
            0.0
        };
        let h = &data.history;
        use crate::theme::{CPU, DISK_READ, DISK_WRITE, GPU, MEMORY, NET_RX, NET_TX};
        let Bands {
            memory,
            gpu,
            disk,
            network,
        } = Bands::of(data, s);
        let mut stats = vec![
            Stat {
                resource: Resource::Cpu,
                title: "CPU",
                accent: CPU,
                ring: Some(s.cpu_percent),
                value: String::new(),
                detail: {
                    let __wtm_i18n_0 = &(s.logical_cpus);
                    crate::i18n::format("{0} logical CPUs", &[format!("{}", __wtm_i18n_0)])
                },
                band: Band::Cores(&s.core_percent),
                series: vec![(&h.cpu, CPU)],
                max: Some(100.0),
            },
            Stat {
                resource: Resource::Memory,
                title: crate::i18n::tr("Memory"),
                accent: MEMORY,
                ring: Some(mem),
                value: String::new(),
                detail: {
                    let __wtm_i18n_0 = &(format_bytes(s.memory_used() as f64));
                    let __wtm_i18n_1 = &(format_bytes(s.memory_total as f64));
                    crate::i18n::format(
                        "{0} of {1}",
                        &[__wtm_i18n_0.to_string(), __wtm_i18n_1.to_string()],
                    )
                },
                band: memory,
                series: vec![(&h.memory, MEMORY)],
                max: Some(100.0),
            },
            Stat {
                resource: Resource::Gpu,
                title: "GPU",
                accent: GPU,
                ring: Some(s.gpu_percent),
                value: String::new(),
                detail: crate::i18n::tr("busiest engine").into(),
                band: gpu,
                series: vec![(&h.gpu, GPU)],
                max: Some(100.0),
            },
            Stat {
                resource: Resource::Disk,
                // Every process's reads and writes, network and pipes too,
                // as the Performance page of the same name.
                title: crate::i18n::tr("Disk & I/O"),
                accent: DISK_READ,
                ring: None,
                value: rate(s.io_read_rate + s.io_write_rate),
                detail: {
                    let __wtm_i18n_0 = &(rate(s.io_read_rate));
                    let __wtm_i18n_1 = &(rate(s.io_write_rate));
                    crate::i18n::format(
                        "read {0} · write {1}",
                        &[__wtm_i18n_0.to_string(), __wtm_i18n_1.to_string()],
                    )
                },
                band: disk,
                series: vec![(&h.io_read, DISK_READ), (&h.io_write, DISK_WRITE)],
                max: None,
            },
            Stat {
                resource: Resource::Network,
                title: crate::i18n::tr("Network"),
                accent: NET_RX,
                ring: None,
                value: rate(s.net_rx_rate + s.net_tx_rate),
                detail: format!("↓ {} · ↑ {}", rate(s.net_rx_rate), rate(s.net_tx_rate)),
                band: network,
                series: vec![(&h.net_rx, NET_RX), (&h.net_tx, NET_TX)],
                max: None,
            },
        ];
        let shown: Vec<Stat<'_>> = layout
            .resources()
            .into_iter()
            .filter_map(|card| {
                let at = stats
                    .iter()
                    .position(|s| Some(s.resource) == card.resource())?;
                Some(stats.swap_remove(at))
            })
            .collect();
        if !shown.is_empty() {
            stat_row(ui, &shown, &mut intents);
        }
        if let Some((color, text)) = verdict_of(data, &apps, &swarm_list) {
            ui.add_space(0.0);
            hero(ui, color, &text, &vitals(data, s));
        }
        if let Some(rescue) = data.status.as_ref().and_then(|s| s.rescue_core.as_ref()) {
            ui.add_space(0.0);
            notice(ui, crate::theme::CPU, "⛨", &rescue_text(rescue));
        }
        if let Some(game) = data.status.as_ref().and_then(|s| s.game.as_ref()) {
            ui.add_space(0.0);
            notice(ui, crate::theme::GPU, "▶", &game_text(game));
        }
        not_responding(ui, data, &snap.processes, &mut intents);
        swarms(ui, data, &snap.processes, &swarm_list, &mut intents);
    } else {
        crate::theme::loading(ui, crate::i18n::tr("Waiting for monitoring data…"));
    }
    ui.add_space(0.0);
    let mut section = |ui: &mut Ui, card: Card| {
        ui.push_id(card, |ui| match card {
            Card::Containers => {
                if crate::containers::overview(ui, &data.docker) {
                    intents.push(Intent::ShowContainers);
                }
            }
            Card::Protection => {
                crate::theme::card(ui, crate::i18n::tr("Automatic protection"), |ui| {
                    protection(ui, data, &mut commands);
                });
            }
            Card::Busiest => {
                crate::theme::card(ui, crate::i18n::tr("Busiest apps right now"), |ui| {
                    busiest_table(ui, data, &apps, &mut intents);
                });
            }
            Card::Recent => {
                crate::theme::card(ui, crate::i18n::tr("Recent automatic actions"), |ui| {
                    recent_actions(ui, data);
                });
            }
            Card::Hardware => {
                ui.collapsing(crate::i18n::tr("Hardware details"), crate::inventory::show);
            }
            _ => {}
        });
    };
    let sections = layout.sections();
    let mut i = 0;
    while i < sections.len() {
        let card = sections[i];
        // The two tables sit side by side on a wide window when next to each other.
        let pair = sections.get(i + 1).copied().filter(|next| {
            matches!(
                (card, *next),
                (Card::Busiest, Card::Recent) | (Card::Recent, Card::Busiest)
            )
        });
        match pair {
            Some(next) if ui.available_width() >= TWO_CARDS => {
                ui.columns(2, |cols| {
                    section(&mut cols[0], card);
                    section(&mut cols[1], next);
                });
                i += 2;
            }
            _ => {
                section(ui, card);
                i += 1;
            }
        }
    }
    if layout.everything_hidden() {
        ui.weak(crate::i18n::tr("Every Overview card is hidden."));
    }
    if ui
        .small_button(crate::i18n::tr("Customize Overview…"))
        .on_hover_text(crate::i18n::tr("Choose the cards and their order"))
        .clicked()
    {
        intents.push(Intent::CustomizeOverview);
    }
    (commands, intents)
}

fn busiest_table(
    ui: &mut Ui,
    data: &Data,
    totals: &[App<'_, &ProcessInfo>],
    intents: &mut Vec<Intent>,
) {
    let apps = busiest(totals, 8);
    // Bars rank the apps shown: each against the largest among them.
    let top_cpu = apps.iter().map(|a| a.cpu).fold(0.0, f32::max).max(1.0);
    let top_memory = apps.iter().map(|a| a.memory).max().unwrap_or(0).max(1);
    egui::Grid::new("overview_apps")
        .num_columns(5)
        .striped(true)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            ui.strong(crate::i18n::tr("App"));
            for h in ["CPU", crate::i18n::tr("Memory"), crate::i18n::tr("Processes")] {
                crate::theme::right(ui, RichText::new(h).strong());
            }
            crate::theme::end_row(ui);
            for a in &apps {
                ui.horizontal(|ui| {
                    let first = &a.instances[0];
                    let icon = data
                        .image_paths
                        .get(&(first.pid, first.create_time))
                        .and_then(|path| Some((path.as_str(), data.icons.get(path)?.as_deref()?)));
                    crate::processes::app_icon(ui, icon);
                    let response = ui.label(a.name).interact(egui::Sense::CLICK);
                    crate::context_actions::menu(&response, ("overview-app", a.name, first.pid, first.create_time), |ui| {
                        crate::context_actions::copy(ui, crate::i18n::tr("Name"), || a.name.to_owned());
                        if crate::context_actions::action(ui, crate::i18n::tr("Show in Processes")) {
                            intents.push(Intent::Reveal(first.clone()));
                        }
                        if crate::context_actions::action(ui, crate::i18n::tr("Limit…")) {
                            intents.push(Intent::LimitApp(a.name.to_owned()));
                        }
                    });
                });
                let cpus = data.snapshot.as_ref().map_or(1, |s| s.system.logical_cpus);
                let mut cpu = RichText::new(crate::i18n::percent(format!("{:.1}", a.cpu)));
                let mut cpu_bar = crate::theme::CPU;
                if let Some(color) = app_color(a.cpu, cpus) {
                    cpu = cpu.color(crate::theme::text_color(ui, color));
                    cpu_bar = color;
                }
                bar_cell(ui, cpu, a.cpu / top_cpu, cpu_bar);
                bar_cell(
                    ui,
                    RichText::new(format_bytes(a.memory as f64)),
                    (a.memory as f64 / top_memory as f64) as f32,
                    crate::theme::MEMORY,
                );
                crate::theme::right(ui, a.instances.len().to_string());
                ui.horizontal(|ui| {
                    if ui.small_button(crate::i18n::tr("Details")).clicked() {
                        intents.push(Intent::Reveal(a.instances[0].clone()));
                    }
                    ui.menu_button("⋯", |ui| {
                        if ui
                            .button(crate::i18n::tr("Efficiency mode"))
                            .on_hover_text(crate::i18n::tr(
                                "Run every process of this app in efficiency mode (EcoQoS).",
                            ))
                            .clicked()
                        {
                            for t in &a.instances {
                                intents
                                    .push(Intent::Command(t.clone(), Command::EcoQos(EcoQos::On)));
                            }
                        }
                        if ui
                            .button(crate::i18n::tr("Free memory"))
                            .on_hover_text(crate::i18n::tr(
                                "Move this app's memory to the standby list; it comes back as needed.",
                            ))
                            .clicked()
                        {
                            for t in &a.instances {
                                intents.push(Intent::Command(t.clone(), Command::TrimMemory));
                            }
                        }
                        if ui
                            .button(crate::i18n::tr("Limit…"))
                            .on_hover_text(crate::i18n::tr(
                                "Give this app a permanent CPU / memory budget.",
                            ))
                            .clicked()
                        {
                            intents.push(Intent::LimitApp(a.name.to_owned()));
                        }
                    })
                    .response
                    .on_hover_text(crate::i18n::tr("More actions"));
                    crate::theme::fill_row(ui);
                });
                crate::theme::end_row(ui);
            }
        });
}

/// A number with a short bar of `fraction` (0..=1) before it, right
/// aligned in a table cell.
pub(crate) fn bar_cell(ui: &mut Ui, text: RichText, fraction: f32, color: Color32) {
    let bar = |ui: &mut Ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(44.0, 6.0), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 3.0, crate::theme::track_color(ui));
        let fraction = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if fraction > 0.0 {
            let filled = egui::Rect::from_min_size(
                rect.min,
                egui::vec2((rect.width() * fraction).max(rect.height()), rect.height()),
            );
            painter.rect_filled(filled, 3.0, color);
        }
    };
    if ui.is_sizing_pass() {
        ui.horizontal(|ui| {
            bar(ui);
            ui.label(text);
        });
        return;
    }
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.label(text);
        bar(ui);
    });
}

fn recent_actions(ui: &mut Ui, data: &Data) {
    let recent: Vec<_> = data
        .events
        .iter()
        .rev()
        .filter(|e| e.notify)
        .take(8)
        .collect();
    if recent.is_empty() {
        ui.weak(crate::i18n::tr(
            "Nothing yet. RustWTM steps in when the machine is overloaded or a rule matches.",
        ));
    }
    // A timeline: a dot per action in the colour of its level, joined by a
    // line from the newest down.
    let n = recent.len();
    let gap = ui.spacing().item_spacing.y;
    let line = ui.visuals().weak_text_color().gamma_multiply(0.45);
    for (i, e) in recent.into_iter().enumerate() {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 20.0), egui::Sense::hover());
            let c = rect.center();
            let painter = ui.painter();
            if i > 0 {
                painter.line_segment(
                    [egui::pos2(c.x, rect.top() - gap), c],
                    egui::Stroke::new(1.5, line),
                );
            }
            if i + 1 < n {
                painter.line_segment(
                    [c, egui::pos2(c.x, rect.bottom() + gap)],
                    egui::Stroke::new(1.5, line),
                );
            }
            let color = match e.level {
                wtm_ipc::EventLevel::Info => crate::theme::ACCENT,
                wtm_ipc::EventLevel::Warn => WARN,
                wtm_ipc::EventLevel::Error => BAD,
            };
            painter.circle_filled(c, 6.0, crate::theme::card_fill(ui));
            painter.circle_filled(c, 4.0, color);
            ui.label(RichText::new(clock(e.unix_ms)).weak());
            ui.add(egui::Label::new(crate::i18n::message(&e.message).into_owned()).truncate());
        });
    }
}

/// `HH:MM:SS` in local time.
fn clock(unix_ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(unix_ms as i64)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}
fn protection(ui: &mut Ui, data: &Data, commands: &mut Vec<UiCommand>) {
    if !matches!(data.mode, Mode::Service { .. }) {
        let error = data.service_error.as_deref().unwrap_or_default();
        let problem = crate::backend::service_problem(data.service_error.as_deref());
        if problem == crate::backend::ServiceProblem::Older {
            crate::theme::colored(ui, WARN, {
                let __wtm_i18n_0 = &(env!("CARGO_PKG_VERSION"));
                crate::i18n::format(
                    "The installed RustWTM service is a different version ({0}), so nothing is limited automatically. Update it with the installer of version {1}.",
                    &[error.to_string(), __wtm_i18n_0.to_string()],
                )
            });
            ui.hyperlink_to(
                crate::i18n::tr("Download the latest release"),
                format!("{}/releases/latest", crate::about::REPO),
            );
        } else if problem == crate::backend::ServiceProblem::Newer {
            crate::theme::colored(
                ui,
                WARN,
                crate::i18n::format(
                    "The installed RustWTM service is newer than this window ({0}). Restart RustWTM to use it; if this window stays older, update it with the latest installer.",
                    &[error.to_string()],
                ),
            );
            ui.hyperlink_to(
                crate::i18n::tr("Download the latest release"),
                format!("{}/releases/latest", crate::about::REPO),
            );
        } else if problem == crate::backend::ServiceProblem::Impostor {
            crate::theme::colored(
                ui,
                WARN,
                crate::i18n::tr(
                    "Another program answers in place of the RustWTM service, so RustWTM does not use it and nothing is limited automatically.",
                ),
            );
        } else {
            crate::theme::colored(
                ui,
                WARN,
                crate::i18n::tr(
                    "The RustWTM service is not running, so nothing is limited automatically.",
                ),
            );
            ui.label(crate::i18n::tr(
                "Install and start it from an elevated prompt:",
            ));
            ui.code("wtm-service install\nwtm-service start");
        }
        return;
    }
    if data
        .status
        .as_ref()
        .is_some_and(|s| s.config_error.is_some())
    {
        crate::theme::colored(
            ui,
            BAD,
            crate::i18n::tr(
                "The service could not load config.toml and keeps its previous or default settings; see Settings.",
            ),
        );
    }
    let admin = matches!(data.mode, Mode::Service { admin: true, .. });
    let Some(Ok((_, text))) = &data.config else {
        crate::theme::loading(ui, crate::i18n::tr("Loading the service settings…"));
        return;
    };
    let Ok(config) = Config::from_toml(text) else {
        ui.colored_label(
            crate::theme::text_color(ui, BAD),
            crate::i18n::tr("The configuration is invalid; fix it in Settings."),
        );
        return;
    };

    ui.horizontal(|ui| {
        if config.dry_run {
            pill(
                ui,
                WARN,
                crate::i18n::tr("● Monitoring only — nothing is limited"),
            );
            let turn_on = ui.add_enabled(
                admin,
                egui::Button::new(RichText::new(crate::i18n::tr("Turn protection on")).strong()),
            );
            if turn_on.clicked()
                && let Some(new) = crate::settings::set_dry_run(text, false)
            {
                commands.push(UiCommand::save_config(text, new));
            }
        } else {
            pill(ui, GOOD, crate::i18n::tr("● Protection is on"));
            if ui
                .add_enabled(
                    admin,
                    egui::Button::new(crate::i18n::tr("Switch to monitoring only")),
                )
                .clicked()
                && let Some(new) = crate::settings::set_dry_run(text, true)
            {
                commands.push(UiCommand::save_config(text, new));
            }
        }
        if !admin {
            ui.weak(crate::i18n::tr("Changing it needs administrator rights."));
            // An elevated window would only restart into the same state.
            if !data.elevated
                && ui
                    .small_button(crate::i18n::tr("Restart as administrator"))
                    .clicked()
            {
                commands.push(UiCommand::RestartElevated);
            }
        }
    });

    let current = Preset::of(&config.governor);
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("CPU protection level:"));
        // The levels as one segmented control.
        egui::Frame::new()
            .fill(crate::theme::tile_fill(ui))
            .corner_radius(crate::theme::SURFACE_RADIUS)
            .inner_margin(2)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for p in Preset::ALL {
                        let button = egui::Button::selectable(
                            current == Some(p),
                            crate::i18n::tr(p.label()),
                        )
                        .min_size(egui::vec2(64.0, 0.0));
                        if ui
                            .add_enabled(admin, button)
                            .on_hover_text(crate::i18n::tr(p.description()))
                            .clicked()
                            && current != Some(p)
                            && let Ok(new) = set_preset(text, p)
                        {
                            commands.push(UiCommand::save_config(text, new));
                        }
                    }
                });
            });
        if current.is_none() {
            ui.weak(crate::i18n::tr("Custom (edited in Settings)"));
        }
    });
    if let Some(p) = current {
        ui.weak(crate::i18n::tr(p.description()));
    }

    if let Some(status) = &data.status {
        let limited = status.managed.len();
        let mut facts = vec![crate::i18n::format(
            if limited == 1 {
                "{0} process limited right now"
            } else {
                "{0} processes limited right now"
            },
            &[limited.to_string()],
        )];
        if !config.limits.is_empty() {
            let processes: usize = status.app_limits.iter().map(|g| g.processes).sum();
            facts.push({
                let __wtm_i18n_0 = &(config.limits.len());
                crate::i18n::format(
                    "{0} app limits covering {1} processes",
                    &[format!("{}", __wtm_i18n_0), format!("{}", processes)],
                )
            });
        }
        if config.governor.protect_foreground
            && let Some(app) = &status.foreground
        {
            facts.push({
                crate::i18n::format(
                    "{0} is in use and never throttled",
                    std::slice::from_ref(app),
                )
            });
        }
        if config.memory_guard.enabled {
            facts.push({
                let __wtm_i18n_0 = &(config.memory_guard.high_percent);
                crate::i18n::format(
                    "memory guard above {0}% RAM",
                    &[format!("{:.0}", __wtm_i18n_0)],
                )
            });
        }
        if !config.block.is_empty() {
            facts.push({
                let __wtm_i18n_0 = &(config.block.len());
                crate::i18n::format("{0} programs blocked", &[format!("{}", __wtm_i18n_0)])
            });
        }
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            for fact in &facts {
                egui::Frame::new()
                    .fill(crate::theme::tile_fill(ui))
                    .corner_radius(10)
                    .inner_margin(egui::Margin::symmetric(9, 3))
                    .show(ui, |ui| ui.label(RichText::new(fact).size(13.0)));
            }
        });
    }
}

/// The protection state as a tinted pill.
fn pill(ui: &mut Ui, color: Color32, text: &str) {
    egui::Frame::new()
        .fill(color.gamma_multiply(if ui.visuals().dark_mode { 0.16 } else { 0.12 }))
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.45)))
        .corner_radius(14)
        .inner_margin(egui::Margin::symmetric(12, 3))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .size(15.0)
                    .color(crate::theme::text_color(ui, color)),
            );
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_cards_handle_the_zero_width_startup_pass() {
        let stats = [Resource::Cpu, Resource::Memory].map(|resource| super::Stat {
            resource,
            title: "Resource",
            accent: crate::theme::CPU,
            ring: Some(0.0),
            value: "0%".into(),
            detail: String::new(),
            band: super::Band::None,
            series: Vec::new(),
            max: Some(100.0),
        });
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .build_ui(|ui| {
                let rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(0.0, 500.0));
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    assert_eq!(ui.available_width(), 0.0);
                    super::stat_row(ui, &stats, &mut Vec::new());
                });
            });
        h.run_steps(2);
    }

    #[test]
    fn copy_current_copies_the_reading_the_card_shows() {
        use egui_kittest::kittest::Queryable;
        let stats = [super::Stat {
            resource: Resource::Cpu,
            title: "CPU",
            accent: crate::theme::CPU,
            ring: Some(42.0),
            value: String::new(),
            detail: String::new(),
            band: super::Band::None,
            series: Vec::new(),
            max: Some(100.0),
        }];
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .with_size([400.0, 400.0])
            .build_ui(|ui| super::stat_row(ui, &stats, &mut Vec::new()));
        h.run_steps(2);
        h.get_by_label_contains("CPU: 42%").click_secondary();
        h.run_steps(2);
        h.get_by_label("Copy Current").click();
        h.run_steps(1);
        let copied: Vec<_> = h
            .output()
            .platform_output
            .commands
            .iter()
            .filter_map(|c| match c {
                egui::OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(copied, ["42%"]);
    }

    #[test]
    fn protection_button_saves_quoted_or_defaulted_mode_keys() {
        use egui_kittest::kittest::Queryable;
        for text in ["\"dry_run\" = true\n", "[governor]\nenabled = false\n"] {
            let mut d = crate::ui_tests::data();
            d.mode = crate::backend::Mode::Service {
                admin: true,
                verified: true,
            };
            d.config = Some(Ok(("config.toml".into(), text.into())));
            let mut h = egui_kittest::Harness::builder()
                .allow_missing_glyphs()
                .with_size([1200.0, 1400.0])
                .build_ui_state(
                    |ui, s: &mut (Data, Vec<UiCommand>)| {
                        s.1.extend(overview(ui, &s.0, &Default::default()).0)
                    },
                    (d, Vec::new()),
                );
            h.run();
            h.get_by_label("Turn protection on").click();
            h.run();
            let saved = h
                .state()
                .1
                .iter()
                .find_map(|command| match command {
                    UiCommand::SaveConfig { toml: text, .. } => Some(text),
                    _ => None,
                })
                .expect("mode switch sends a config save");
            assert!(!wtm_policy::Config::from_toml(saved).unwrap().dry_run);
        }
    }

    #[test]
    fn an_arranged_overview_fits_every_width() {
        use crate::overview_layout::{Card, Layout};
        use egui_kittest::kittest::Queryable;
        let mut layout = Layout {
            hidden: vec![Card::Memory, Card::Gpu],
            ..Layout::default()
        };
        // The recent actions after the hardware details: no longer next to
        // the busiest apps, so never beside them.
        layout.order.retain(|c| *c != Card::Recent);
        layout.order.push(Card::Recent);
        for width in [0.0_f32, 300.0, 700.0, 1300.0] {
            let mut h = egui_kittest::Harness::builder()
                .allow_missing_glyphs()
                .with_size([width.max(1.0), 1600.0])
                .build_ui_state(
                    |ui, (d, l): &mut (Data, Layout)| {
                        overview(ui, d, l);
                    },
                    (crate::ui_tests::data(), layout.clone()),
                );
            h.run();
            if width == 0.0 {
                continue;
            }
            for shown in ["CPU:", "Disk & I/O:", "Network:"] {
                assert!(
                    h.query_by_label_contains(shown).is_some(),
                    "{shown} at {width}"
                );
            }
            for hidden in ["Memory:", "GPU:"] {
                assert!(
                    h.query_by_label_contains(hidden).is_none(),
                    "{hidden} at {width}"
                );
            }
            let busiest = h.get_by_label("Busiest apps right now").rect();
            let hardware = h.get_by_label("Hardware details").rect();
            let recent = h.get_by_label("Recent automatic actions").rect();
            assert!(
                busiest.max.y < hardware.min.y && hardware.max.y < recent.min.y,
                "{width}: {busiest:?} {hardware:?} {recent:?}"
            );
        }
        // Next to each other, the two tables share a wide window.
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .with_size([1300.0, 1600.0])
            .build_ui_state(
                |ui, d: &mut Data| {
                    overview(ui, d, &Layout::default());
                },
                crate::ui_tests::data(),
            );
        h.run();
        let busiest = h.get_by_label("Busiest apps right now").rect();
        let recent = h.get_by_label("Recent automatic actions").rect();
        assert!(busiest.max.x <= recent.min.x, "{busiest:?} {recent:?}");
    }

    #[test]
    fn a_swarm_is_shown_with_ways_to_end_it() {
        use egui_kittest::kittest::Queryable;
        let mut d = crate::ui_tests::data();
        {
            let s = std::sync::Arc::make_mut(d.snapshot.as_mut().unwrap());
            // 160 probes: 100 started by alpha.exe (100), 60 whose parent exited.
            for i in 0..160 {
                let parent = if i < 100 { 100 } else { 9999 };
                let mut p = crate::ui_tests::proc(10_000 + i, parent, "powershell.exe", 0.1);
                p.create_time = i64::MAX - 1;
                s.processes.push(p);
            }
        }
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .with_size([1100.0, 1200.0])
            .build_ui_state(
                |ui, (d, out): &mut (Data, Vec<Intent>)| {
                    out.extend(overview(ui, d, &Default::default()).1)
                },
                (d, Vec::new()),
            );
        h.run();
        assert!(
            h.query_by_label("⚠ 160 copies of powershell.exe are running")
                .is_some()
        );
        assert!(
            h.query_by_label_contains("Started by alpha.exe (100), exited parent (60)")
                .is_some()
        );
        h.get_by_label("End the 60 whose parent exited").click();
        h.run();
        h.get_by_label("End all 160").click();
        h.run();
        let counts: Vec<usize> = h
            .state()
            .1
            .iter()
            .filter_map(|i| match i {
                Intent::ConfirmMany(t, Command::Kill) => Some(t.len()),
                _ => None,
            })
            .collect();
        assert_eq!(counts, [60, 160]);

        // Below the threshold nothing is shown.
        h.state_mut().0.swarm.min_count = 500;
        h.run();
        assert!(
            h.query_by_label_contains("copies of powershell.exe")
                .is_none()
        );
    }

    #[test]
    fn each_card_shows_what_its_reading_is_made_of() {
        let _language = crate::i18n::scope(crate::i18n::Language::English);
        let mut d = crate::ui_tests::data();
        let mut s = d.snapshot.as_ref().unwrap().system.clone();
        let tooltips = |band: &Band<'_>| match band {
            Band::Stack(parts) => parts.iter().map(|p| p.2.clone()).collect::<Vec<_>>(),
            Band::Meters(meters) => meters.iter().map(|m| m.0.clone()).collect(),
            _ => Vec::new(),
        };

        // Without the page lists: what is in use of the total.
        d.memory_composition = None;
        let b = Bands::of(&d, &s);
        assert_eq!(tooltips(&b.memory), ["In use: 8.0 GiB", ""]);
        // With them: in use, modified, standby and free, as the Memory page.
        d.memory_composition = Some(wtm_sys::memory::Composition {
            total: 16 << 30,
            available: 8 << 30,
            installed: None,
            paged_pool: 0,
            nonpaged_pool: 0,
            modified: Some(1 << 30),
            standby: Some(4 << 30),
            free: Some(3 << 30),
        });
        let b = Bands::of(&d, &s);
        assert_eq!(
            tooltips(&b.memory),
            [
                "In use: 8.0 GiB",
                "Modified: 1.0 GiB",
                "Standby: 4.0 GiB",
                "Free: 3.0 GiB"
            ]
        );

        // Disks by drive letter, or by number when they have none.
        s.disks = ["0 C: D:", "1"]
            .map(|name| wtm_core::DiskUsage {
                name: name.into(),
                ..Default::default()
            })
            .into();
        assert_eq!(tooltips(&Bands::of(&d, &s).disk), ["C: D:", "Disk 1"]);
        // No disk counters: the reads and writes of all processes.
        s.disks.clear();
        s.io_read_rate = 2048.0;
        s.io_write_rate = 0.0;
        assert_eq!(
            tooltips(&Bands::of(&d, &s).disk),
            ["Read: 2.0 KiB/s", "Write: 0 B/s"]
        );

        // How full each link is; an adapter without a link speed is left
        // out, and with none left the traffic each way is shown.
        s.net_adapters = vec![
            wtm_core::NetAdapterUsage {
                name: "Ethernet".into(),
                link_speed: 1_000_000,
                rx_rate: 62_500.0,
                tx_rate: 0.0,
                ..Default::default()
            },
            wtm_core::NetAdapterUsage {
                name: "Unknown".into(),
                ..Default::default()
            },
        ];
        match Bands::of(&d, &s).network {
            Band::Meters(m) => assert_eq!(m, [("Ethernet".to_owned(), 50.0)]),
            _ => panic!("adapters as meters"),
        }
        s.net_adapters.clear();
        s.net_rx_rate = 0.0;
        s.net_tx_rate = 0.0;
        assert!(matches!(Bands::of(&d, &s).network, Band::None));

        // The busiest engines first, three at most.
        s.gpu_engines = [
            ("Copy", 1.0),
            ("3D", 40.0),
            ("Compute", 5.0),
            ("Video", 2.0),
        ]
        .map(|(n, l)| (n.to_owned(), l))
        .into();
        assert_eq!(tooltips(&Bands::of(&d, &s).gpu), ["3D", "Compute", "Video"]);
    }

    #[test]
    fn a_band_names_the_part_under_the_pointer_and_survives_no_room() {
        let _language = crate::i18n::scope(crate::i18n::Language::English);
        let cores = [95.0, 10.0, f32::NAN, 0.0];
        let mut hovered = Vec::new();
        let mut h = egui_kittest::Harness::builder()
            .allow_missing_glyphs()
            .build_ui(|ui| {
                let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 22.0));
                // Four cells of 23.5 points with 2 between them.
                for x in [5.0, 30.0, 55.0] {
                    hovered.push(paint_band(
                        ui,
                        rect,
                        &Band::Cores(&cores),
                        crate::theme::CPU,
                        Some(egui::pos2(x, 11.0)),
                    ));
                }
                for empty in [
                    egui::Rect::ZERO,
                    egui::Rect::from_min_size(rect.min, egui::vec2(-5.0, 22.0)),
                ] {
                    for band in [
                        Band::Cores(&cores),
                        Band::Stack(vec![(1.0, crate::theme::MEMORY, "x".into())]),
                        Band::Meters(vec![("3D".into(), 50.0)]),
                    ] {
                        assert!(
                            paint_band(ui, empty, &band, crate::theme::CPU, Some(rect.center()))
                                .is_none()
                        );
                    }
                }
            });
        h.run_steps(1);
        drop(h);
        assert_eq!(
            hovered[..3],
            [
                Some("CPU 0: 95%".to_owned()),
                Some("CPU 1: 10%".to_owned()),
                Some("CPU 2: 0%".to_owned()),
            ]
        );
    }

    #[test]
    fn colors_and_times() {
        assert_eq!(clock(0).len(), 8);
        assert_eq!(app_color(60.0, 8), Some(BAD));
        assert_eq!(app_color(20.0, 8), Some(WARN));
        assert_eq!(app_color(5.0, 8), None);
    }

    #[test]
    fn the_verdict_names_the_resource_and_the_app_behind_it() {
        let mut d = crate::ui_tests::data();
        let set = |d: &mut Data, cpu: f32, available: u64| {
            let s = std::sync::Arc::make_mut(d.snapshot.as_mut().unwrap());
            s.system.cpu_percent = cpu;
            s.system.memory_available = available;
        };
        let gb = 1u64 << 30;
        // 16 GB in total.
        let cases = [
            (42.0, 8 * gb, GOOD, "Everything runs smoothly."),
            (
                95.0,
                8 * gb,
                BAD,
                "CPU is maxed out: alpha.exe uses 20% of it",
            ),
            (75.0, 8 * gb, WARN, "CPU is busy: alpha.exe uses 20% of it"),
            (10.0, gb, BAD, "Memory is almost full: "),
            (10.0, 3 * gb, WARN, "Memory is getting full: "),
        ];
        for (cpu, available, color, text) in cases {
            set(&mut d, cpu, available);
            let (c, t) = verdict(&d).unwrap();
            assert_eq!(c, color, "{t}");
            assert!(t.starts_with(text), "{t}");
        }
        d.snapshot = None;
        assert!(verdict(&d).is_none());
    }
}
