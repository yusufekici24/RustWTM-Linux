//! "While this app runs": a power plan, and the computer kept awake.
//!
//! `[[while_running]]` entries name apps by image-name wildcard. An entry
//! is active while one of its apps runs and for `hold_secs` after the last
//! one exits, so a short gap (an app restarting itself, a helper between
//! two runs) does not switch the plan back and forth. One power plan can be
//! active at a time: game mode's request first, then the entries in the
//! order of the file.

use serde::{Deserialize, Serialize};

use crate::glob::glob_match;

const DEFAULT_HOLD_SECS: u64 = 5;
const MOST_HOLD_SECS: u64 = 3600;

fn default_hold() -> u64 {
    DEFAULT_HOLD_SECS
}

/// What an entry keeps awake while its apps run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepAwake {
    /// The computer does not go to sleep by itself.
    System,
    /// Nor does the screen turn off (held by the RustWTM window: Windows
    /// refuses it from the service).
    Display,
}

/// One `[[while_running]]` entry.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WhileRunning {
    pub name: String,
    /// Image-name wildcards.
    pub apps: Vec<String>,
    /// A power plan, by GUID or by the name Windows shows.
    #[serde(default)]
    pub power_plan: Option<String>,
    #[serde(default)]
    pub keep_awake: Option<KeepAwake>,
    /// How long the entry stays active after its last app exits.
    #[serde(default = "default_hold")]
    pub hold_secs: u64,
}

impl WhileRunning {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("while_running: name must not be empty".into());
        }
        let me = format!("while_running '{}'", self.name);
        if self.apps.is_empty() || self.apps.iter().any(|a| a.trim().is_empty()) {
            return Err(format!("{me}: apps needs at least one non-empty pattern"));
        }
        if self
            .power_plan
            .as_deref()
            .is_some_and(|p| p.trim().is_empty())
        {
            return Err(format!("{me}: power_plan must not be empty"));
        }
        if self.power_plan.is_none() && self.keep_awake.is_none() {
            return Err(format!("{me}: needs power_plan or keep_awake"));
        }
        if self.hold_secs > MOST_HOLD_SECS {
            return Err(format!("{me}: hold_secs must be at most {MOST_HOLD_SECS}"));
        }
        Ok(())
    }

    fn matches(&self, image: &str) -> bool {
        self.apps.iter().any(|a| glob_match(a, image))
    }

    fn table(&self) -> toml_edit::Table {
        let mut t = toml_edit::Table::new();
        t.insert("name", toml_edit::value(self.name.clone()));
        let apps: toml_edit::Array = self.apps.iter().map(String::as_str).collect();
        t.insert("apps", toml_edit::value(apps));
        if let Some(plan) = &self.power_plan {
            t.insert("power_plan", toml_edit::value(plan.clone()));
        }
        if let Some(awake) = self.keep_awake {
            let name = match awake {
                KeepAwake::System => "system",
                KeepAwake::Display => "display",
            };
            t.insert("keep_awake", toml_edit::value(name));
        }
        if self.hold_secs != DEFAULT_HOLD_SECS {
            t.insert("hold_secs", toml_edit::value(self.hold_secs as i64));
        }
        t
    }
}

pub(crate) fn validate_all(entries: &[WhileRunning]) -> Result<(), String> {
    for (i, e) in entries.iter().enumerate() {
        e.validate()?;
        if entries[..i].iter().any(|o| o.name == e.name) {
            return Err(format!("while_running: two entries are named '{}'", e.name));
        }
    }
    Ok(())
}

/// An entry active now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveEntry {
    pub name: String,
    /// Its apps running now, by image name; empty while it is held after
    /// they exited.
    pub apps: Vec<String>,
    pub power_plan: Option<String>,
    pub keep_awake: Option<KeepAwake>,
}

/// Follows which entries are active.
#[derive(Debug, Default)]
pub struct AppPower {
    entries: Vec<WhileRunning>,
    /// Per entry, when one of its apps was last seen (seconds).
    last_seen: Vec<Option<u64>>,
}

impl AppPower {
    pub fn new(entries: Vec<WhileRunning>) -> Self {
        let mut me = Self::default();
        me.set_config(entries);
        me
    }

    /// New entries; one that keeps its name keeps its hold.
    pub fn set_config(&mut self, entries: Vec<WhileRunning>) {
        self.last_seen = entries
            .iter()
            .map(|e| {
                self.entries
                    .iter()
                    .position(|o| o.name == e.name)
                    .and_then(|i| self.last_seen[i])
            })
            .collect();
        self.entries = entries;
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entries active at `now` (seconds), given the image names of the
    /// processes running, in the order of the configuration.
    pub fn tick<'a>(
        &mut self,
        now: u64,
        images: impl IntoIterator<Item = &'a str>,
    ) -> Vec<ActiveEntry> {
        let mut running: Vec<Vec<String>> = vec![Vec::new(); self.entries.len()];
        for image in images {
            for (e, apps) in self.entries.iter().zip(&mut running) {
                if e.matches(image) && !apps.iter().any(|a| a.eq_ignore_ascii_case(image)) {
                    apps.push(image.to_owned());
                }
            }
        }
        let mut out = Vec::new();
        for ((e, seen), mut apps) in self.entries.iter().zip(&mut self.last_seen).zip(running) {
            if !apps.is_empty() {
                *seen = Some(now);
            }
            let held = seen.is_some_and(|t| now.saturating_sub(t) <= e.hold_secs);
            if !held {
                *seen = None;
                continue;
            }
            apps.sort();
            out.push(ActiveEntry {
                name: e.name.clone(),
                apps,
                power_plan: e.power_plan.clone(),
                keep_awake: e.keep_awake,
            });
        }
        out
    }
}

/// Who wants a power plan, and which.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRequest {
    /// An entry's name, or `None` for game mode.
    pub entry: Option<String>,
    /// A GUID or a plan name, as configured.
    pub plan: String,
}

/// The plan to use: game mode's (`game`, a GUID) first, then the first
/// active entry with a plan.
pub fn choose_plan(game: Option<&str>, active: &[ActiveEntry]) -> Option<PlanRequest> {
    if let Some(plan) = game {
        return Some(PlanRequest {
            entry: None,
            plan: plan.to_owned(),
        });
    }
    active.iter().find_map(|a| {
        a.power_plan.as_ref().map(|plan| PlanRequest {
            entry: Some(a.name.clone()),
            plan: plan.clone(),
        })
    })
}

/// The strongest keep-awake among the active entries, with the entries that
/// ask for it.
pub fn keep_awake(active: &[ActiveEntry]) -> Option<(KeepAwake, Vec<String>)> {
    let most = active.iter().filter_map(|a| a.keep_awake).max()?;
    let who = active
        .iter()
        .filter(|a| a.keep_awake.is_some())
        .map(|a| a.name.clone())
        .collect();
    Some((most, who))
}

/// The GUID of the plan `wanted` names (a GUID, with or without braces, or a
/// plan name, ignoring case) among `plans` (`(GUID, name)`).
pub fn resolve_plan(wanted: &str, plans: &[(String, String)]) -> Option<String> {
    let wanted = wanted.trim().trim_start_matches('{').trim_end_matches('}');
    plans
        .iter()
        .find(|(guid, _)| guid.eq_ignore_ascii_case(wanted))
        .or_else(|| {
            plans
                .iter()
                .find(|(_, name)| name.trim().eq_ignore_ascii_case(wanted))
        })
        .map(|(guid, _)| guid.clone())
}

/// Saves `entry` into a configuration text, in place of the entry named
/// `replacing` (or its own name), else at the end. Comments and formatting
/// elsewhere are kept; the result must load.
pub fn set_while_running(
    text: &str,
    entry: &WhileRunning,
    replacing: Option<&str>,
) -> Result<String, String> {
    const KEY: &str = "while_running";
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    let tables = doc
        .entry(KEY)
        .or_insert_with(|| toml_edit::Item::ArrayOfTables(Default::default()))
        .as_array_of_tables_mut()
        .ok_or_else(|| format!("`{KEY}` must be an array of tables ([[{KEY}]])"))?;
    let name_of = |t: &toml_edit::Table| {
        t.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned()
    };
    let old = replacing.unwrap_or(&entry.name);
    if replacing.is_some_and(|r| r != entry.name) && tables.iter().any(|t| name_of(t) == entry.name)
    {
        return Err(format!("another {KEY} entry is named '{}'", entry.name));
    }
    let found = tables.iter().position(|t| name_of(t) == old);
    match found {
        Some(i) => *tables.get_mut(i).expect("found above") = entry.table(),
        None => tables.push(entry.table()),
    }
    let text = doc.to_string();
    crate::Config::from_toml(&text).map_err(|e| e.to_string())?;
    Ok(text)
}

/// Removes the entry named `name`; unchanged text if there is none.
pub fn remove_while_running(text: &str, name: &str) -> Result<String, String> {
    const KEY: &str = "while_running";
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    if doc.get(KEY).is_some_and(|i| !i.is_array_of_tables()) {
        return Err(format!("`{KEY}` must be an array of tables ([[{KEY}]])"));
    }
    if let Some(tables) = doc.get_mut(KEY).and_then(|i| i.as_array_of_tables_mut()) {
        tables.retain(|t| t.get("name").and_then(|v| v.as_str()) != Some(name));
        if tables.is_empty() {
            doc.remove(KEY);
        }
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    const HIGH: &str = "8C5E7FDA-E8BF-4A96-9A85-A6E23A8C635C";

    fn plans() -> Vec<(String, String)> {
        [
            ("381B4222-F694-41F0-9685-FF5BB260DF2E", "Balanced"),
            (HIGH, "High performance"),
            ("A1841308-3541-4FAB-BC81-F71556F20B4A", "Power saver"),
        ]
        .map(|(g, n)| (g.to_owned(), n.to_owned()))
        .to_vec()
    }

    fn config(text: &str) -> Vec<WhileRunning> {
        Config::from_toml(text).unwrap().while_running
    }

    const TWO: &str = r#"
[[while_running]]
name = "Render"
apps = ["blender.exe", "HandBrake*.exe"]
power_plan = "High performance"
keep_awake = "system"

[[while_running]]
name = "Movie"
apps = ["vlc.exe"]
keep_awake = "display"
hold_secs = 0
"#;

    #[test]
    fn entries_parse_and_validate() {
        let e = config(TWO);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].power_plan.as_deref(), Some("High performance"));
        assert_eq!(e[0].keep_awake, Some(KeepAwake::System));
        assert_eq!(e[0].hold_secs, 5);
        assert_eq!(e[1].power_plan, None);
        for (bad, why) in [
            (
                "[[while_running]]\nname = \"\"\napps = [\"a.exe\"]\nkeep_awake = \"system\"",
                "name",
            ),
            (
                "[[while_running]]\nname = \"A\"\napps = []\nkeep_awake = \"system\"",
                "apps",
            ),
            (
                "[[while_running]]\nname = \"A\"\napps = [\"a.exe\"]",
                "power_plan or keep_awake",
            ),
            (
                "[[while_running]]\nname = \"A\"\napps = [\"a.exe\"]\npower_plan = \" \"",
                "power_plan",
            ),
            (
                "[[while_running]]\nname = \"A\"\napps = [\"a.exe\"]\nkeep_awake = \"system\"\nhold_secs = 99999",
                "hold_secs",
            ),
            (
                "[[while_running]]\nname = \"A\"\napps = [\"a.exe\"]\nkeep_awake = \"system\"\n[[while_running]]\nname = \"A\"\napps = [\"b.exe\"]\nkeep_awake = \"system\"",
                "two entries",
            ),
            (
                "[[while_running]]\nname = \"A\"\napps = [\"a.exe\"]\nkeep_awake = \"always\"",
                "keep_awake",
            ),
        ] {
            let err = Config::from_toml(bad).unwrap_err().to_string();
            assert!(err.contains(why), "{bad}: {err}");
        }
    }

    #[test]
    fn an_entry_is_active_while_its_apps_run_and_for_its_hold_after() {
        let mut p = AppPower::new(config(TWO));
        assert!(p.tick(0, ["explorer.exe"]).is_empty());
        let a = p.tick(
            1,
            ["Blender.exe", "HandBrakeCLI.exe", "blender.exe", "vlc.exe"],
        );
        assert_eq!(
            a.iter()
                .map(|e| (e.name.as_str(), e.apps.clone()))
                .collect::<Vec<_>>(),
            [
                (
                    "Render",
                    vec!["Blender.exe".to_owned(), "HandBrakeCLI.exe".to_owned()]
                ),
                ("Movie", vec!["vlc.exe".to_owned()]),
            ]
        );
        // All gone: Render holds 5 s with no apps, Movie (hold 0) ends.
        let a = p.tick(2, ["explorer.exe"]);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].name, "Render");
        assert!(a[0].apps.is_empty());
        assert_eq!(p.tick(6, []).len(), 1);
        assert!(p.tick(7, []).is_empty());
        // Back after the hold ended: active again at once.
        assert_eq!(p.tick(8, ["blender.exe"]).len(), 1);
    }

    #[test]
    fn a_reloaded_entry_keeps_its_hold() {
        let mut p = AppPower::new(config(TWO));
        p.tick(1, ["blender.exe"]);
        let mut changed = config(TWO);
        changed[0].keep_awake = None;
        changed.reverse();
        p.set_config(changed);
        let a = p.tick(3, []);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].name, "Render");
        assert_eq!(a[0].keep_awake, None);
    }

    #[test]
    fn game_mode_wins_then_the_first_entry_with_a_plan() {
        let mut p = AppPower::new(config(TWO));
        let a = p.tick(1, ["vlc.exe", "blender.exe"]);
        assert_eq!(
            choose_plan(None, &a),
            Some(PlanRequest {
                entry: Some("Render".into()),
                plan: "High performance".into()
            })
        );
        assert_eq!(choose_plan(Some(HIGH), &a).unwrap().entry, None);
        assert_eq!(choose_plan(None, &a[1..]), None);
        assert_eq!(
            keep_awake(&a),
            Some((KeepAwake::Display, vec!["Render".into(), "Movie".into()]))
        );
        assert_eq!(keep_awake(&[]), None);
    }

    #[test]
    fn plans_resolve_by_guid_or_name() {
        let plans = plans();
        for wanted in [
            HIGH,
            "{8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c}",
            "high performance",
            " High performance ",
        ] {
            assert_eq!(
                resolve_plan(wanted, &plans).as_deref(),
                Some(HIGH),
                "{wanted}"
            );
        }
        assert_eq!(resolve_plan("Ultimate Performance", &plans), None);
    }

    #[test]
    fn entries_are_saved_and_removed_keeping_the_rest() {
        let base = "# mine\ndry_run = true\n";
        let mut e = config(TWO).remove(0);
        let text = set_while_running(base, &e, None).unwrap();
        assert!(text.starts_with("# mine\n"), "{text}");
        assert_eq!(config(&text), [e.clone()]);
        e.name = "Renders".into();
        e.hold_secs = 30;
        let text = set_while_running(&text, &e, Some("Render")).unwrap();
        assert_eq!(config(&text), [e.clone()]);
        assert!(text.contains("hold_secs = 30"), "{text}");
        let mut other = e.clone();
        other.name = "Other".into();
        let text = set_while_running(&text, &other, None).unwrap();
        assert!(set_while_running(&text, &other, Some("Renders")).is_err());
        let text = remove_while_running(&text, "Renders").unwrap();
        assert_eq!(config(&text), [other]);
        let text = remove_while_running(&text, "Other").unwrap();
        assert!(!text.contains("while_running"), "{text}");
        let mut empty = e;
        empty.keep_awake = None;
        empty.power_plan = None;
        assert!(set_while_running(base, &empty, None).is_err());
    }
}
