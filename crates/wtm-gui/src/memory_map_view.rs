//! The process inspector's Memory map panel: a process's address space by
//! allocation, as VMMap and System Informer's Memory tab show it, with its
//! heaps, thread stacks, PEB and TEBs named. Read once on request and off
//! the UI thread, where the totals, order and filter keys are also made;
//! of the memory itself only pointers and heap signatures are read.

use eframe::egui::{self, RichText, Ui};
use wtm_core::control::{
    Allocation, MemoryKind, MemoryMap, ProcessRef, Usage, is_executable, protection_code,
};
// Shown text: written with the language's decimal sign.
use crate::i18n::bytes as format_bytes;

use crate::backend::Data;
use crate::processes::Intent;
use crate::theme::{BAD, WARN};

/// Committed and reserved bytes of one kind of memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KindTotal {
    pub kind: MemoryKind,
    pub committed: u64,
    pub reserved: u64,
    pub allocations: usize,
}

/// A read memory map, totalled and sorted for drawing.
#[derive(Debug, Clone, Default)]
pub struct MemoryList {
    /// Image, mapped file, shareable and private memory, in that order;
    /// kinds the process has none of are left out.
    pub kinds: Vec<KindTotal>,
    /// Most committed first.
    pub rows: Vec<Allocation>,
    /// Each row's kind, address, name and usage in lower case, for the
    /// filter.
    pub keys: Vec<String>,
    /// Private allocations with executable committed memory, and their
    /// committed bytes.
    pub private_executable: (usize, u64),
    /// Committed bytes of the allocations named as heaps and as stacks.
    pub heaps: u64,
    pub stacks: u64,
    pub incomplete: bool,
    pub usage_unread: bool,
}

impl From<MemoryMap> for MemoryList {
    fn from(map: MemoryMap) -> Self {
        let mut kinds: Vec<KindTotal> = [
            MemoryKind::Image,
            MemoryKind::Mapped,
            MemoryKind::Shareable,
            MemoryKind::Private,
        ]
        .into_iter()
        .map(|kind| KindTotal {
            kind,
            ..KindTotal::default()
        })
        .collect();
        let mut private_executable = (0, 0);
        let (mut heaps, mut stacks) = (0, 0);
        for a in &map.allocations {
            if a.usage.iter().any(|u| matches!(u, Usage::Heap { .. })) {
                heaps += a.committed;
            }
            if a.usage.iter().any(|u| matches!(u, Usage::Stack { .. })) {
                stacks += a.committed;
            }
            if let Some(t) = kinds.iter_mut().find(|t| t.kind == a.kind) {
                t.committed += a.committed;
                t.reserved += a.reserved;
                t.allocations += 1;
            }
            if a.kind == MemoryKind::Private && a.executable {
                private_executable.0 += 1;
                private_executable.1 += a.committed;
            }
        }
        kinds.retain(|t| t.allocations > 0);
        let mut rows = map.allocations;
        rows.sort_by(|a, b| {
            b.committed
                .cmp(&a.committed)
                .then(b.reserved.cmp(&a.reserved))
                .then(a.base.cmp(&b.base))
        });
        let keys = rows
            .iter()
            .map(|a| {
                format!(
                    "{}\n0x{:x}\n{}\n{}",
                    kind_label(a.kind),
                    a.base,
                    a.name.as_deref().unwrap_or_default(),
                    usage_text(a).unwrap_or_default()
                )
                .to_lowercase()
            })
            .collect();
        Self {
            kinds,
            rows,
            keys,
            private_executable,
            heaps,
            stacks,
            incomplete: map.incomplete,
            usage_unread: map.usage_unread,
        }
    }
}

/// What the process keeps in `a`, in words: "Default heap", "Stack of
/// thread 5120", "PEB, 12 TEBs"; `None` when nothing is known.
pub fn usage_text(a: &Allocation) -> Option<String> {
    if a.usage.is_empty() {
        return None;
    }
    let bits = |wow64: bool, text: String| {
        if wow64 {
            crate::i18n::format("{0} (32-bit)", &[text])
        } else {
            text
        }
    };
    let mut parts = Vec::new();
    for u in &a.usage {
        if let Usage::Peb { wow64 } = *u {
            parts.push(bits(wow64, crate::i18n::tr("PEB").to_owned()));
        }
    }
    // One allocation holds the TEBs of many threads.
    let tebs: Vec<(u32, bool)> = a
        .usage
        .iter()
        .filter_map(|u| match *u {
            Usage::Teb { tid, wow64 } => Some((tid, wow64)),
            _ => None,
        })
        .collect();
    match tebs.as_slice() {
        [] => {}
        [(tid, wow64)] => parts.push(bits(
            *wow64,
            crate::i18n::format("TEB of thread {0}", &[tid.to_string()]),
        )),
        many => parts.push(crate::i18n::format("{0} TEBs", &[many.len().to_string()])),
    }
    for u in &a.usage {
        match *u {
            Usage::Stack { tid, wow64 } => parts.push(bits(
                wow64,
                crate::i18n::format("Stack of thread {0}", &[tid.to_string()]),
            )),
            Usage::Heap { default, wow64 } => parts.push(bits(
                wow64,
                crate::i18n::tr(if default { "Default heap" } else { "Heap" }).to_owned(),
            )),
            Usage::Peb { .. } | Usage::Teb { .. } => {}
        }
    }
    Some(parts.join(", "))
}

/// Rows drawn at most; a large process has tens of thousands.
const SHOWN_ALLOCATIONS: usize = 500;

pub fn kind_label(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Image => crate::i18n::tr("Image"),
        MemoryKind::Mapped => crate::i18n::tr("Mapped file"),
        MemoryKind::Shareable => crate::i18n::tr("Shareable"),
        MemoryKind::Private => crate::i18n::tr("Private"),
    }
}

/// Draws the memory map of `sel` once it has been asked for.
pub fn show(ui: &mut Ui, data: &Data, sel: &ProcessRef, intents: &mut Vec<Intent>) -> Option<()> {
    let (_, read) = data.memory_map.as_ref().filter(|(r, _)| r == sel)?;
    ui.horizontal_wrapped(|ui| {
        ui.strong(crate::i18n::tr("Memory map"));
        if ui.small_button(crate::i18n::tr("Hide")).clicked() {
            intents.push(Intent::MemoryMap(None));
        }
    });
    match read {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(crate::i18n::tr("Reading the memory map…"));
            });
        }
        Some(Err(e)) => {
            crate::theme::colored(ui, BAD, e);
        }
        Some(Ok(list)) => list_view(ui, sel, list),
    }
    Some(())
}

fn bytes(n: u64) -> String {
    format_bytes(n as f64)
}

fn list_view(ui: &mut Ui, sel: &ProcessRef, list: &MemoryList) {
    if list.incomplete {
        ui.weak(crate::i18n::tr(
            "The map is partial: the process has very many regions, or reading took too long.",
        ));
    }
    egui::Grid::new("memory_map_totals")
        .num_columns(4)
        .striped(true)
        .spacing(egui::vec2(12.0, 2.0))
        .show(ui, |ui| {
            ui.weak(crate::i18n::tr("Type"));
            ui.weak(crate::i18n::tr("Committed"));
            ui.weak(crate::i18n::tr("Reserved"));
            ui.weak(crate::i18n::tr("Allocations"));
            ui.end_row();
            for t in &list.kinds {
                ui.label(kind_label(t.kind));
                ui.label(bytes(t.committed));
                ui.weak(bytes(t.reserved));
                ui.weak(t.allocations.to_string());
                ui.end_row();
            }
        });
    let (count, committed) = list.private_executable;
    if count > 0 {
        ui.label(crate::i18n::format(
            "Private executable memory: {0} in {1} allocations",
            &[bytes(committed), count.to_string()],
        ))
        .on_hover_text(crate::i18n::tr(
            "Executable memory not backed by a file. JIT compilers (.NET, browsers, Java) create it, and so does code injected into the process.",
        ));
    }
    if list.heaps > 0 || list.stacks > 0 {
        ui.label(crate::i18n::format(
            "Heaps: {0} · Thread stacks: {1}",
            &[bytes(list.heaps), bytes(list.stacks)],
        ))
        .on_hover_text(crate::i18n::tr(
            "Committed in the allocations named below. Large blocks a heap allocates on their own are not counted.",
        ));
    }
    if list.usage_unread {
        ui.weak(crate::i18n::tr(
            "Heaps and stacks are not named: the process does not allow reading its memory.",
        ));
    }

    // Per process, as the Handles filter.
    let filter_id = egui::Id::unique(("memory_map_filter", sel.pid, sel.create_time));
    let mut filter: String = ui.data(|d| d.get_temp(filter_id)).unwrap_or_default();
    let label = crate::i18n::tr("Filter by type or name");
    let edit = ui.add(
        egui::TextEdit::singleline(&mut filter)
            .char_limit(256)
            .hint_text(label)
            .desired_width(180.0),
    );
    edit.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, label));
    ui.data_mut(|d| d.insert_temp(filter_id, filter.clone()));

    let needle = filter.trim().to_lowercase();
    let matching: Vec<&Allocation> = list
        .rows
        .iter()
        .zip(&list.keys)
        .filter(|(_, key)| needle.is_empty() || key.contains(&needle))
        .map(|(a, _)| a)
        .collect();
    if matching.is_empty() && !needle.is_empty() {
        ui.weak(crate::i18n::tr("Nothing matches the filter."));
        return;
    }
    if matching.len() > SHOWN_ALLOCATIONS {
        ui.weak(crate::i18n::format(
            "Showing the first {0} of {1}.",
            &[SHOWN_ALLOCATIONS.to_string(), matching.len().to_string()],
        ));
    }
    egui::ScrollArea::horizontal()
        .id_salt("memory_map_scroll")
        .show(ui, |ui| {
            egui::Grid::new("memory_map_grid")
                .num_columns(4)
                .striped(true)
                .spacing(egui::vec2(12.0, 2.0))
                .show(ui, |ui| {
                    // Narrow enough for the inspector: the reserved size and
                    // full path are on hover, and the kind shows where there
                    // is no file.
                    ui.weak(crate::i18n::tr("Address"));
                    ui.weak(crate::i18n::tr("Committed"));
                    ui.weak(crate::i18n::tr("Protection"));
                    ui.weak(crate::i18n::tr("Name"));
                    ui.end_row();
                    for a in matching.into_iter().take(SHOWN_ALLOCATIONS) {
                        ui.label(RichText::new(format!("0x{:X}", a.base)).small().monospace());
                        ui.label(RichText::new(bytes(a.committed)).small())
                            .on_hover_text(format!(
                                "{}: {}",
                                crate::i18n::tr("Reserved"),
                                bytes(a.reserved)
                            ));
                        let code = RichText::new(protection_text(a)).small().monospace();
                        if a.kind == MemoryKind::Private && a.executable {
                            ui.label(code.color(crate::theme::text_color(ui, WARN)))
                                .on_hover_text(crate::i18n::tr(
                                    "Executable memory not backed by a file. JIT compilers (.NET, browsers, Java) create it, and so does code injected into the process.",
                                ));
                        } else {
                            ui.label(code);
                        }
                        match &a.name {
                            Some(name) => {
                                let file = name.rsplit(['\\', '/']).next().unwrap_or(name);
                                let shown = ui
                                    .label(RichText::new(file).small())
                                    .on_hover_text(format!("{}: {name}", kind_label(a.kind)));
                                crate::context_actions::copy_text(
                                    &shown,
                                    ("process-memory", a.base),
                                    crate::i18n::tr("Name"),
                                    name,
                                );
                            }
                            None => match usage_text(a) {
                                Some(text) => {
                                    ui.label(RichText::new(&text).small()).on_hover_text(
                                        format!("{}: {text}", kind_label(a.kind)),
                                    );
                                }
                                None => {
                                    ui.weak(RichText::new(kind_label(a.kind)).small());
                                }
                            },
                        }
                        ui.end_row();
                    }
                });
        });
}

/// The protection it was reserved with, and "+X" when some committed part
/// has since become executable (as JIT compilers make their code).
fn protection_text(a: &Allocation) -> String {
    let code = protection_code(a.protection);
    if a.executable && !is_executable(a.protection) {
        format!("{code} +X").trim_start().to_owned()
    } else {
        code.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protection_shows_execution_granted_later() {
        let a = |protection, executable| Allocation {
            protection,
            executable,
            ..Allocation::default()
        };
        assert_eq!(protection_text(&a(0x04, true)), "RW +X");
        assert_eq!(protection_text(&a(0x20, true)), "RX");
        assert_eq!(protection_text(&a(0x04, false)), "RW");
        assert_eq!(protection_text(&a(0, true)), "+X");
    }

    fn allocation(kind: MemoryKind, base: u64, committed: u64, executable: bool) -> Allocation {
        Allocation {
            base,
            kind,
            reserved: committed * 2,
            committed,
            regions: 1,
            protection: 0x04,
            executable,
            name: None,
            usage: Vec::new(),
        }
    }

    #[test]
    fn totals_by_kind_and_most_committed_first() {
        let list = MemoryList::from(MemoryMap {
            allocations: vec![
                allocation(MemoryKind::Private, 0x1000, 10, true),
                allocation(MemoryKind::Image, 0x2000, 30, true),
                allocation(MemoryKind::Private, 0x3000, 20, false),
            ],
            ..MemoryMap::default()
        });
        let order: Vec<u64> = list.rows.iter().map(|a| a.base).collect();
        assert_eq!(order, [0x2000, 0x3000, 0x1000]);
        let kinds: Vec<_> = list
            .kinds
            .iter()
            .map(|t| (t.kind, t.committed, t.reserved, t.allocations))
            .collect();
        assert_eq!(
            kinds,
            [
                (MemoryKind::Image, 30, 60, 1),
                (MemoryKind::Private, 30, 60, 2)
            ]
        );
        // An executable image is ordinary; only private memory counts.
        assert_eq!(list.private_executable, (1, 10));
        assert!(list.keys[0].contains("0x2000"));
        assert_eq!((list.heaps, list.stacks), (0, 0));
    }

    fn used(committed: u64, usage: Vec<Usage>) -> Allocation {
        Allocation {
            usage,
            ..allocation(MemoryKind::Private, committed, committed, false)
        }
    }

    #[test]
    fn usage_reads_as_words_with_tebs_grouped() {
        let teb = |tid| Usage::Teb { tid, wow64: false };
        let text = |usage| usage_text(&used(1, usage));
        assert_eq!(text(vec![]), None);
        assert_eq!(
            text(vec![Usage::Peb { wow64: false }, teb(4), teb(8), teb(12)]).as_deref(),
            Some("PEB, 3 TEBs")
        );
        assert_eq!(
            text(vec![Usage::Teb {
                tid: 4,
                wow64: true
            }])
            .as_deref(),
            Some("TEB of thread 4 (32-bit)")
        );
        assert_eq!(
            text(vec![Usage::Stack {
                tid: 5120,
                wow64: false
            }])
            .as_deref(),
            Some("Stack of thread 5120")
        );
        assert_eq!(
            text(vec![
                Usage::Heap {
                    default: true,
                    wow64: false
                },
                Usage::Heap {
                    default: false,
                    wow64: true
                }
            ])
            .as_deref(),
            Some("Default heap, Heap (32-bit)")
        );
    }

    #[test]
    fn heaps_and_stacks_are_totalled_and_filterable() {
        let stack = Usage::Stack {
            tid: 9,
            wow64: false,
        };
        let heap = Usage::Heap {
            default: false,
            wow64: false,
        };
        let list = MemoryList::from(MemoryMap {
            allocations: vec![
                used(0x100, vec![stack]),
                used(0x200, vec![heap]),
                used(0x400, vec![]),
            ],
            usage_unread: false,
            ..MemoryMap::default()
        });
        assert_eq!((list.heaps, list.stacks), (0x200, 0x100));
        assert!(list.keys.iter().any(|k| k.contains("stack of thread 9")));
        let unread = MemoryList::from(MemoryMap {
            usage_unread: true,
            ..MemoryMap::default()
        });
        assert!(unread.usage_unread);
    }
}
