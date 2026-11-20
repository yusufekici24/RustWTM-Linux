//! The process inspector's Security panel: the account, groups and
//! privileges a process's token holds, as Process Explorer's Security tab
//! lists them. Read once on request, off the UI thread.

use eframe::egui::{self, RichText, Ui};
use wtm_core::control::{ProcessRef, TokenGroup, TokenPrivilege, TokenReport};

use crate::backend::Data;
use crate::processes::Intent;
use crate::theme::{BAD, GOOD};

/// Draws the token of `sel` once it has been asked for; a report about
/// another process is not shown.
pub fn show(ui: &mut Ui, data: &Data, sel: &ProcessRef, intents: &mut Vec<Intent>) -> Option<()> {
    let (_, report) = data.security.as_ref().filter(|(r, _)| r == sel)?;
    ui.horizontal(|ui| {
        ui.strong(crate::i18n::tr("Security"));
        if ui.small_button(crate::i18n::tr("Hide")).clicked() {
            intents.push(Intent::Security(None));
        }
    });
    match report {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(crate::i18n::tr("Reading the security token…"));
            });
        }
        Some(Err(e)) => {
            crate::theme::colored(ui, BAD, e);
        }
        Some(Ok(t)) => report_view(ui, t),
    }
    Some(())
}

fn report_view(ui: &mut Ui, t: &TokenReport) {
    // The inspector already names the user; the SID is what it adds.
    ui.horizontal_wrapped(|ui| {
        let sid = ui.label(RichText::new(&t.user.sid).small().monospace());
        if let Some(name) = &t.user.name {
            sid.on_hover_text(name);
        }
        if ui.small_button(crate::i18n::tr("Copy SID")).clicked() {
            ui.ctx().copy_text(t.user.sid.clone());
        }
    });
    ui.weak(crate::i18n::format("Session {0}", &[t.session.to_string()]));
    if t.app_container {
        ui.weak(crate::i18n::tr("Runs in an app container"));
    }

    let mut groups: Vec<&TokenGroup> = t.groups.iter().collect();
    groups.sort_by_cached_key(|g| display_name(g).to_lowercase());
    let mut privileges: Vec<&TokenPrivilege> = t.privileges.iter().collect();
    privileges.sort_by_key(|p| (!p.enabled(), p.name.to_lowercase()));

    // Drawn in full: the inspector scrolls, and a nested list would shrink
    // to whatever height is left in it. Only sideways do they scroll, since
    // an unnamed capability SID is some ninety characters long.
    ui.label(crate::i18n::format(
        "Groups ({0})",
        &[groups.len().to_string()],
    ));
    egui::ScrollArea::horizontal()
        .id_salt("security_groups_scroll")
        .show(ui, |ui| groups_grid(ui, groups));
    ui.label(crate::i18n::format(
        "Privileges ({0})",
        &[privileges.len().to_string()],
    ));
    egui::ScrollArea::horizontal()
        .id_salt("security_privileges_scroll")
        .show(ui, |ui| privileges_grid(ui, privileges));
}

fn groups_grid(ui: &mut Ui, groups: Vec<&TokenGroup>) {
    egui::Grid::new("security_groups")
        .num_columns(2)
        .striped(true)
        .spacing(egui::vec2(12.0, 2.0))
        .show(ui, |ui| {
            for g in groups {
                // Names are shown whole; only a bare SID is cut.
                let text = match &g.account.name {
                    Some(name) => name.as_str().into(),
                    None => shortened(&g.account.sid),
                };
                let name = RichText::new(text).small();
                let name = if g.enabled() || g.integrity() {
                    ui.label(name)
                } else {
                    ui.label(name.weak())
                };
                name.on_hover_text(RichText::new(&g.account.sid).monospace());
                ui.label(RichText::new(group_flags(g)).small().weak());
                ui.end_row();
            }
        });
}

fn privileges_grid(ui: &mut Ui, privileges: Vec<&TokenPrivilege>) {
    egui::Grid::new("security_privileges")
        .num_columns(2)
        .striped(true)
        .spacing(egui::vec2(12.0, 2.0))
        .show(ui, |ui| {
            for p in privileges {
                let name = ui.label(RichText::new(&p.name).small().monospace());
                if let Some(d) = &p.description {
                    name.on_hover_text(d);
                }
                if p.enabled() {
                    crate::theme::colored(ui, GOOD, crate::i18n::tr("Enabled"));
                } else {
                    ui.weak(crate::i18n::tr("Disabled"));
                }
                ui.end_row();
            }
        });
}

/// The account name, or the SID when Windows has no name for it.
fn display_name(g: &TokenGroup) -> &str {
    g.account.name.as_deref().unwrap_or(&g.account.sid)
}

/// A long SID (an app capability's runs to ninety characters) cut in the
/// middle; the full SID is on hover.
fn shortened(name: &str) -> std::borrow::Cow<'_, str> {
    const MOST: usize = 40;
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= MOST {
        return name.into();
    }
    let head: String = chars[..24].iter().collect();
    let tail: String = chars[chars.len() - 12..].iter().collect();
    format!("{head}…{tail}").into()
}

/// How the token uses a group, most telling first: whether it grants
/// access at all, then its other roles.
pub(crate) fn group_flags(g: &TokenGroup) -> String {
    let mut out: Vec<&str> = Vec::new();
    // The integrity label is never "enabled": it sets a level instead.
    if g.deny_only() {
        out.push(crate::i18n::tr("Deny only"));
    } else if !g.enabled() && !g.integrity() {
        out.push(crate::i18n::tr("Disabled"));
    }
    if g.integrity() {
        out.push(crate::i18n::tr("Integrity"));
    }
    if g.logon_id() {
        out.push(crate::i18n::tr("Logon session"));
    }
    if g.owner() {
        out.push(crate::i18n::tr("Owner"));
    }
    if g.resource() {
        out.push(crate::i18n::tr("Domain-local"));
    }
    if g.mandatory() {
        out.push(crate::i18n::tr("Mandatory"));
    }
    out.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_unnamed_sids_are_cut_in_the_middle() {
        assert_eq!(shortened("S-1-5-32-544"), "S-1-5-32-544");
        let capability = "S-1-15-3-1024-1065365936-1281604716-3511738428-1654721687-432734479";
        // The authority and first value, then the last twelve characters.
        assert_eq!(
            shortened(capability),
            "S-1-15-3-1024-1065365936…87-432734479"
        );
        assert_eq!(shortened(capability).chars().count(), 37);
    }
}
