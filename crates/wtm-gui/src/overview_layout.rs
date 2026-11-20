//! Which Overview cards show and in what order, and the editor that arranges
//! them. Warnings (the verdict, programs not responding, swarms, the rescue
//! core and game mode) are not cards: they always show.

use crate::overview::Resource;
use eframe::egui;

/// A card the user can hide or move. Saved by name; a name from a newer
/// version reads as `Unknown` and is dropped, so it never discards the other
/// window preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Card {
    Cpu,
    Memory,
    Gpu,
    Disk,
    Network,
    Containers,
    Protection,
    Busiest,
    Recent,
    Hardware,
    #[serde(other)]
    Unknown,
}

/// The cards in the top row, in their default order.
pub const RESOURCES: [Card; 5] = [
    Card::Cpu,
    Card::Memory,
    Card::Gpu,
    Card::Disk,
    Card::Network,
];
/// The cards below the warnings, in their default order.
pub const SECTIONS: [Card; 5] = [
    Card::Containers,
    Card::Protection,
    Card::Busiest,
    Card::Recent,
    Card::Hardware,
];

impl Card {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Memory => crate::i18n::tr("Memory"),
            Self::Gpu => "GPU",
            Self::Disk => crate::i18n::tr("Disk & I/O"),
            Self::Network => crate::i18n::tr("Network"),
            Self::Containers => "Docker",
            Self::Protection => crate::i18n::tr("Automatic protection"),
            Self::Busiest => crate::i18n::tr("Busiest apps right now"),
            Self::Recent => crate::i18n::tr("Recent automatic actions"),
            Self::Hardware => crate::i18n::tr("Hardware details"),
            Self::Unknown => "",
        }
    }

    /// The resource a top-row card shows.
    pub fn resource(self) -> Option<Resource> {
        match self {
            Self::Cpu => Some(Resource::Cpu),
            Self::Memory => Some(Resource::Memory),
            Self::Gpu => Some(Resource::Gpu),
            Self::Disk => Some(Resource::Disk),
            Self::Network => Some(Resource::Network),
            _ => None,
        }
    }

    fn is_resource(self) -> bool {
        self.resource().is_some()
    }
}

/// What the window remembers of the Overview.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Layout {
    /// Every card in the user's order; a top-row card only moves among the
    /// top row and a section among the sections.
    pub order: Vec<Card>,
    /// Hidden cards, rather than shown ones, so a card added by a later
    /// version shows until the user hides it.
    pub hidden: Vec<Card>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            order: RESOURCES.iter().chain(&SECTIONS).copied().collect(),
            hidden: Vec::new(),
        }
    }
}

impl Layout {
    /// Keeps each known card once and adds the ones missing (new in this
    /// version) at the end of their group.
    pub fn normalize(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.order
            .retain(|c| *c != Card::Unknown && seen.insert(*c));
        for card in RESOURCES.iter().chain(&SECTIONS) {
            if seen.insert(*card) {
                self.order.push(*card);
            }
        }
        let mut seen = std::collections::HashSet::new();
        self.hidden
            .retain(|c| *c != Card::Unknown && seen.insert(*c));
    }

    pub fn shows(&self, card: Card) -> bool {
        !self.hidden.contains(&card)
    }

    /// The top-row cards shown, in order.
    pub fn resources(&self) -> Vec<Card> {
        self.group(true).filter(|c| self.shows(*c)).collect()
    }

    /// The sections shown, in order.
    pub fn sections(&self) -> Vec<Card> {
        self.group(false).filter(|c| self.shows(*c)).collect()
    }

    pub fn everything_hidden(&self) -> bool {
        self.resources().is_empty() && self.sections().is_empty()
    }

    fn group(&self, resources: bool) -> impl Iterator<Item = Card> + '_ {
        self.order
            .iter()
            .copied()
            .filter(move |c| *c != Card::Unknown && c.is_resource() == resources)
    }

    fn set_shown(&mut self, card: Card, shown: bool) {
        self.hidden.retain(|c| *c != card);
        if !shown {
            self.hidden.push(card);
        }
    }

    /// Moves a card one place within its group, hidden cards included.
    fn move_card(&mut self, card: Card, up: bool) {
        let group: Vec<Card> = self.group(card.is_resource()).collect();
        let Some(i) = group.iter().position(|c| *c == card) else {
            return;
        };
        let j = if up { i.checked_sub(1) } else { Some(i + 1) };
        let Some(other) = j.and_then(|j| group.get(j)) else {
            return;
        };
        let a = self.order.iter().position(|c| *c == card);
        let b = self.order.iter().position(|c| c == other);
        if let (Some(a), Some(b)) = (a, b) {
            self.order.swap(a, b);
        }
    }
}

/// The layout being arranged; the Overview changes on Apply only.
pub struct Editor {
    draft: Layout,
}

impl Editor {
    pub fn new(layout: &Layout) -> Self {
        let mut draft = layout.clone();
        draft.normalize();
        Self { draft }
    }
}

/// Returns the new layout on Apply. Cancel, Escape or a click outside
/// closes it unchanged.
pub fn show_editor(ctx: &egui::Context, editor: &mut Option<Editor>) -> Option<Layout> {
    let active = editor.as_mut()?;
    let mut apply = false;
    let mut cancel = false;
    let width = (ctx.content_rect().width() - 72.0).clamp(280.0, 460.0);
    // The list scrolls when the window is too short for it; the heading and
    // the buttons stay in view.
    let list_height = (ctx.content_rect().height() - 220.0).max(100.0);
    let modal = egui::Modal::new(egui::Id::unique("overview-layout-editor"))
        .frame(crate::theme::dialog_frame(ctx))
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().scroll = crate::theme::solid_scroll();
            ui.heading(crate::i18n::tr("Customize Overview"));
            ui.weak(crate::i18n::tr(
                "Warnings, such as programs not responding, always show.",
            ));
            let mut change = None;
            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .min_scrolled_height(list_height)
                .max_height(list_height)
                .show(ui, |ui| {
                    for (title, resources) in [
                        (crate::i18n::tr("Resource cards"), true),
                        (crate::i18n::tr("Sections"), false),
                    ] {
                        ui.add_space(4.0);
                        ui.strong(title);
                        let group: Vec<Card> = active.draft.group(resources).collect();
                        for (i, card) in group.iter().enumerate() {
                            ui.push_id(card, |ui| {
                                if let Some(c) = row(ui, &active.draft, *card, i, group.len()) {
                                    change = Some(c);
                                }
                            });
                        }
                    }
                });
            match change {
                Some(Change::Show(card, shown)) => active.draft.set_shown(card, shown),
                Some(Change::Move(card, up)) => active.draft.move_card(card, up),
                None => {}
            }
            crate::theme::divider(ui);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button(crate::i18n::tr("Restore default layout"))
                    .clicked()
                {
                    active.draft = Layout::default();
                }
                apply = ui.button(crate::i18n::tr("Apply")).clicked();
                cancel = ui.button(crate::i18n::tr("Cancel")).clicked();
            });
        });
    if apply {
        return editor.take().map(|e| e.draft);
    }
    if cancel || modal.should_close() {
        *editor = None;
    }
    None
}

enum Change {
    Show(Card, bool),
    Move(Card, bool),
}

/// A card's check box and its up and down buttons, named for screen readers.
fn row(ui: &mut egui::Ui, draft: &Layout, card: Card, i: usize, len: usize) -> Option<Change> {
    let mut change = None;
    ui.horizontal(|ui| {
        let label = card.label();
        let mut shown = draft.shows(card);
        let name_width = (ui.available_width() - 68.0).max(40.0);
        ui.allocate_ui(egui::vec2(name_width, ui.spacing().interact_size.y), |ui| {
            ui.set_width(name_width);
            if ui.checkbox(&mut shown, label).changed() {
                change = Some(Change::Show(card, shown));
            }
        });
        for (up, enabled, text, arrow) in [
            (true, i > 0, "Move {0} up", "↑"),
            (false, i + 1 < len, "Move {0} down", "↓"),
        ] {
            let name = crate::i18n::format(text, &[label.into()]);
            let response = ui
                .add_enabled(enabled, egui::Button::new(arrow))
                .on_hover_text(&name);
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, response.enabled(), &name)
            });
            if response.clicked() {
                change = Some(Change::Move(card, up));
            }
        }
    });
    change
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_shows_every_card_in_the_old_order() {
        let layout = Layout::default();
        assert_eq!(layout.resources(), RESOURCES);
        assert_eq!(layout.sections(), SECTIONS);
        assert!(!layout.everything_hidden());
    }

    #[test]
    fn unknown_and_repeated_cards_are_dropped_and_missing_ones_added() {
        let mut layout = Layout {
            order: vec![
                Card::Network,
                Card::Unknown,
                Card::Network,
                Card::Recent,
                Card::Cpu,
            ],
            hidden: vec![Card::Unknown, Card::Gpu, Card::Gpu],
        };
        layout.normalize();
        assert_eq!(
            layout.resources(),
            [Card::Network, Card::Cpu, Card::Memory, Card::Disk]
        );
        assert_eq!(
            layout.sections(),
            [
                Card::Recent,
                Card::Containers,
                Card::Protection,
                Card::Busiest,
                Card::Hardware
            ]
        );
        assert_eq!(layout.hidden, [Card::Gpu]);
    }

    #[test]
    fn cards_move_within_their_own_group_only() {
        let mut layout = Layout::default();
        // The first section does not move up into the top row.
        layout.move_card(Card::Containers, true);
        assert_eq!(layout, Layout::default());
        // The last top-row card does not move down into the sections.
        layout.move_card(Card::Network, false);
        assert_eq!(layout, Layout::default());
        layout.move_card(Card::Network, true);
        assert_eq!(
            layout.resources(),
            [
                Card::Cpu,
                Card::Memory,
                Card::Gpu,
                Card::Network,
                Card::Disk
            ]
        );
        // A hidden card keeps its place and can still be moved.
        layout.set_shown(Card::Busiest, false);
        layout.move_card(Card::Recent, true);
        layout.set_shown(Card::Busiest, true);
        assert_eq!(
            layout.sections(),
            [
                Card::Containers,
                Card::Protection,
                Card::Recent,
                Card::Busiest,
                Card::Hardware
            ]
        );
    }
}
