//! Offline presentation catalog. Process names, paths and configuration values
//! never go through this lookup. Formatted values are inserted in one pass.
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Language {
    #[default]
    System,
    English,
    Turkish,
    German,
    French,
    Spanish,
    // Variant names are stored in the registry: append, never rename.
    ChineseSimplified,
    ChineseTraditional,
    Hindi,
    Bengali,
    Portuguese,
    Russian,
    Indonesian,
    Japanese,
    Marathi,
    Vietnamese,
    Telugu,
    Swahili,
    Filipino,
    Tamil,
    Korean,
    Italian,
    Gujarati,
    Kannada,
    Polish,
    Ukrainian,
    Dutch,
    Punjabi,
}

/// Languages by catalog index; English (0) is the source text.
const BY_INDEX: [Language; 27] = [
    Language::English,
    Language::Turkish,
    Language::German,
    Language::French,
    Language::Spanish,
    Language::ChineseSimplified,
    Language::ChineseTraditional,
    Language::Hindi,
    Language::Bengali,
    Language::Portuguese,
    Language::Russian,
    Language::Indonesian,
    Language::Japanese,
    Language::Marathi,
    Language::Vietnamese,
    Language::Telugu,
    Language::Swahili,
    Language::Filipino,
    Language::Tamil,
    Language::Korean,
    Language::Italian,
    Language::Gujarati,
    Language::Kannada,
    Language::Polish,
    Language::Ukrainian,
    Language::Dutch,
    Language::Punjabi,
];
/// Languages from index 5 on keep one `locales/<tag>.json` file each,
/// in `BY_INDEX` order.
const LOCALE_FILES: [(&str, &str); 22] = [
    ("zh-Hans", include_str!("../locales/zh-Hans.json")),
    ("zh-Hant", include_str!("../locales/zh-Hant.json")),
    ("hi", include_str!("../locales/hi.json")),
    ("bn", include_str!("../locales/bn.json")),
    ("pt-BR", include_str!("../locales/pt-BR.json")),
    ("ru", include_str!("../locales/ru.json")),
    ("id", include_str!("../locales/id.json")),
    ("ja", include_str!("../locales/ja.json")),
    ("mr", include_str!("../locales/mr.json")),
    ("vi", include_str!("../locales/vi.json")),
    ("te", include_str!("../locales/te.json")),
    ("sw", include_str!("../locales/sw.json")),
    ("fil", include_str!("../locales/fil.json")),
    ("ta", include_str!("../locales/ta.json")),
    ("ko", include_str!("../locales/ko.json")),
    ("it", include_str!("../locales/it.json")),
    ("gu", include_str!("../locales/gu.json")),
    ("kn", include_str!("../locales/kn.json")),
    ("pl", include_str!("../locales/pl.json")),
    ("uk", include_str!("../locales/uk.json")),
    ("nl", include_str!("../locales/nl.json")),
    ("pa", include_str!("../locales/pa.json")),
];
const FIRST_LOCALE_FILE: usize = 5;

impl Language {
    /// Picker order: Latin-script names alphabetically, then other scripts.
    pub const ALL: [Self; 28] = [
        Self::System,
        Self::Indonesian,
        Self::German,
        Self::English,
        Self::Spanish,
        Self::Filipino,
        Self::French,
        Self::Italian,
        Self::Swahili,
        Self::Dutch,
        Self::Polish,
        Self::Portuguese,
        Self::Vietnamese,
        Self::Turkish,
        Self::Russian,
        Self::Ukrainian,
        Self::Marathi,
        Self::Hindi,
        Self::Bengali,
        Self::Punjabi,
        Self::Gujarati,
        Self::Tamil,
        Self::Telugu,
        Self::Kannada,
        Self::ChineseSimplified,
        Self::ChineseTraditional,
        Self::Japanese,
        Self::Korean,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::System => tr("Windows language"),
            Self::English => "English",
            Self::Turkish => "Türkçe",
            Self::German => "Deutsch",
            Self::French => "Français",
            Self::Spanish => "Español",
            Self::ChineseSimplified => "简体中文",
            Self::ChineseTraditional => "繁體中文",
            Self::Hindi => "हिन्दी",
            Self::Bengali => "বাংলা",
            Self::Portuguese => "Português (Brasil)",
            Self::Russian => "Русский",
            Self::Indonesian => "Bahasa Indonesia",
            Self::Japanese => "日本語",
            Self::Marathi => "मराठी",
            Self::Vietnamese => "Tiếng Việt",
            Self::Telugu => "తెలుగు",
            Self::Swahili => "Kiswahili",
            Self::Filipino => "Filipino",
            Self::Tamil => "தமிழ்",
            Self::Korean => "한국어",
            Self::Italian => "Italiano",
            Self::Gujarati => "ગુજરાતી",
            Self::Kannada => "ಕನ್ನಡ",
            Self::Polish => "Polski",
            Self::Ukrainian => "Українська",
            Self::Dutch => "Nederlands",
            Self::Punjabi => "ਪੰਜਾਬੀ",
        }
    }
    fn index(self) -> u8 {
        if self == Self::System {
            return system_language().index();
        }
        BY_INDEX
            .iter()
            .position(|&l| l == self)
            .expect("every language has a catalog index") as u8
    }
    /// Every language with a catalog, English first.
    #[cfg(test)]
    pub const TRANSLATED: [Self; 27] = BY_INDEX;
    /// The language text is shown in now, with the Windows choice resolved.
    pub fn current() -> Self {
        BY_INDEX[index()]
    }
    /// Whether the language writes a decimal comma (12,5) rather than a
    /// point (12.5).
    pub fn decimal_comma(self) -> bool {
        matches!(
            self,
            Self::Turkish
                | Self::German
                | Self::French
                | Self::Spanish
                | Self::Italian
                | Self::Dutch
                | Self::Polish
                | Self::Portuguese
                | Self::Russian
                | Self::Ukrainian
                | Self::Indonesian
                | Self::Vietnamese
        )
    }
    /// Short language tag, e.g. for naming review captures and in
    /// diagnostic reports.
    pub fn code(self) -> &'static str {
        match self.index() as usize {
            0 => "en",
            1 => "tr",
            2 => "de",
            3 => "fr",
            4 => "es",
            i => LOCALE_FILES[i - FIRST_LOCALE_FILE].0,
        }
    }
    pub fn from_tag(tag: &str) -> Self {
        let tag = tag.to_ascii_lowercase();
        let mut parts = tag.split(['-', '_']);
        let primary = parts.next().unwrap_or("");
        let mut rest = parts;
        match primary {
            "tr" => Self::Turkish,
            "de" => Self::German,
            "fr" => Self::French,
            "es" => Self::Spanish,
            "zh" if rest.any(|p| matches!(p, "hant" | "tw" | "hk" | "mo")) => {
                Self::ChineseTraditional
            }
            "zh" => Self::ChineseSimplified,
            "hi" => Self::Hindi,
            "bn" => Self::Bengali,
            "pt" => Self::Portuguese,
            "ru" => Self::Russian,
            // "in" is the withdrawn code that older systems still report.
            "id" | "in" => Self::Indonesian,
            "ja" => Self::Japanese,
            "mr" => Self::Marathi,
            "vi" => Self::Vietnamese,
            "te" => Self::Telugu,
            "sw" => Self::Swahili,
            "fil" | "tl" => Self::Filipino,
            "ta" => Self::Tamil,
            "ko" => Self::Korean,
            "it" => Self::Italian,
            "gu" => Self::Gujarati,
            "kn" => Self::Kannada,
            "pl" => Self::Polish,
            "uk" => Self::Ukrainian,
            "nl" => Self::Dutch,
            // The catalog is Gurmukhi; Shahmukhi (Pakistan) is right-to-left.
            "pa" if rest.any(|p| matches!(p, "arab" | "pk")) => Self::English,
            "pa" => Self::Punjabi,
            _ => Self::English,
        }
    }
}

#[cfg(windows)]
const REGISTRY: &str = r"Software\RustWTM";
static ACTIVE: AtomicU8 = AtomicU8::new(0);
thread_local! { static LOCAL: Cell<Option<u8>> = const { Cell::new(None) }; }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Translations {
    tr: String,
    de: String,
    fr: String,
    es: String,
}
impl Translations {
    fn texts(&self) -> [&str; 4] {
        [&self.tr, &self.de, &self.fr, &self.es]
    }
}
type Catalog = BTreeMap<String, Translations>;
/// Every English key with its Turkish, German, French and Spanish text.
fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../locales/messages.json"))
            .expect("validated embedded translation catalog")
    })
}
type LocaleCatalog = HashMap<String, String>;
/// A per-language file, parsed on first use so only the languages
/// actually shown take memory.
fn locale_catalog(index: usize) -> Option<&'static LocaleCatalog> {
    static LOADED: [OnceLock<LocaleCatalog>; LOCALE_FILES.len()] =
        [const { OnceLock::new() }; LOCALE_FILES.len()];
    let slot = index.checked_sub(FIRST_LOCALE_FILE)?;
    let (tag, json) = LOCALE_FILES.get(slot)?;
    Some(LOADED[slot].get_or_init(|| {
        serde_json::from_str(json)
            .unwrap_or_else(|e| panic!("validated embedded locale {tag}: {e}"))
    }))
}
fn system_language() -> Language {
    Language::from_tag(&wtm_core::system_ui_language())
}
/// `~/.config/rustwtm/language`: the chosen language as JSON.
#[cfg(target_os = "linux")]
fn preference_file() -> Option<std::path::PathBuf> {
    wtm_sys::compat::user_config_dir().map(|d| d.join("language"))
}

#[cfg(target_os = "linux")]
pub fn preference() -> Language {
    preference_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(s.trim()).ok())
        .unwrap_or_default()
}

#[cfg(target_os = "linux")]
pub fn save(language: Language) -> std::io::Result<()> {
    let path =
        preference_file().ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &path,
        serde_json::to_string(&language).expect("language serialization"),
    )?;
    activate(language);
    Ok(())
}

#[cfg(windows)]
pub fn preference() -> Language {
    let key = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(REGISTRY)
        .ok();
    read_preference(key.as_ref())
}
#[cfg(windows)]
fn read_preference(key: Option<&winreg::RegKey>) -> Language {
    key.and_then(|k| k.get_value::<String, _>("Language").ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
pub fn initialize() {
    activate(preference());
}
pub fn activate(language: Language) {
    ACTIVE.store(language.index(), Ordering::Relaxed);
}
#[cfg(windows)]
pub fn save(language: Language) -> std::io::Result<()> {
    let (key, _) =
        winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).create_subkey(REGISTRY)?;
    write_preference(&key, language)?;
    activate(language);
    Ok(())
}
#[cfg(windows)]
fn write_preference(key: &winreg::RegKey, language: Language) -> std::io::Result<()> {
    key.set_value(
        "Language",
        &serde_json::to_string(&language).expect("language serialization"),
    )
}
/// Render/test scopes restore their previous language even when unwinding.
#[must_use]
pub struct Scope(Option<u8>, std::marker::PhantomData<std::rc::Rc<()>>);
pub fn scope(language: Language) -> Scope {
    Scope(
        LOCAL.with(|c| c.replace(Some(language.index()))),
        std::marker::PhantomData,
    )
}
impl Drop for Scope {
    fn drop(&mut self) {
        LOCAL.with(|c| c.set(self.0));
    }
}
fn index() -> usize {
    LOCAL
        .with(|c| c.get())
        .unwrap_or_else(|| ACTIVE.load(Ordering::Relaxed)) as usize
}
pub fn tr(english: &str) -> &str {
    let i = index();
    if i == 0 {
        return english;
    }
    let text = match locale_catalog(i) {
        Some(locale) => locale.get(english).map(String::as_str),
        None => catalog()
            .get(english)
            .and_then(|v| v.texts().get(i - 1).copied()),
    };
    text.filter(|s| !s.is_empty()).unwrap_or(english)
}

pub fn debug(value: impl std::fmt::Debug) -> String {
    tr(&format!("{value:?}")).to_owned()
}

/// Old service logs remain unchanged. Recognize complete known message templates
/// only at the presentation boundary; unknown/native diagnostics remain verbatim.
pub fn message(english: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    use std::cell::RefCell;
    use std::collections::HashMap;
    /// Messages kept per thread; the screen shows far fewer at a time.
    const KEPT: usize = 1024;
    // Matching a message against every template takes long enough to show
    // when a frame does it for each visible event, so results are kept for
    // the current language: `None` when the text stays as it is.
    thread_local! {
        static SEEN: RefCell<(usize, HashMap<String, Option<String>>)> =
            RefCell::new((0, HashMap::new()));
    }
    let language = index();
    if language == 0 {
        // The service names priority classes as Windows' API does.
        if english.contains("BelowNormal") || english.contains("AboveNormal") {
            return Cow::Owned(
                english
                    .replace("BelowNormal", "below normal")
                    .replace("AboveNormal", "above normal"),
            );
        }
        return Cow::Borrowed(english);
    }
    let known = SEEN.with(|seen| {
        let seen = seen.borrow();
        (seen.0 == language)
            .then(|| seen.1.get(english).cloned())
            .flatten()
    });
    if let Some(known) = known {
        return known.map_or(Cow::Borrowed(english), Cow::Owned);
    }
    let text = message_at_depth(english, 0);
    SEEN.with(|seen| {
        let mut seen = seen.borrow_mut();
        if seen.0 != language || seen.1.len() >= KEPT {
            *seen = (language, HashMap::new());
        }
        let translated = match &text {
            Cow::Borrowed(t) if std::ptr::eq(*t, english) => None,
            t => Some(t.clone().into_owned()),
        };
        seen.1.insert(english.to_owned(), translated);
    });
    text
}
/// A number with one decimal point and digits on both sides: "12.5",
/// "-0.25"; not "1.2.3", "a.txt" or "5".
fn is_plain_decimal(value: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    value
        .strip_prefix('-')
        .unwrap_or(value)
        .split_once('.')
        .is_some_and(|(whole, part)| digits(whole) && digits(part))
}

fn message_at_depth(english: &str, depth: usize) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    if index() == 0 {
        return Cow::Borrowed(english);
    }
    let exact = tr(english);
    if exact != english {
        return Cow::Borrowed(exact);
    }
    if english.len() > 16384 {
        return Cow::Borrowed(english);
    }
    static PATTERNS: OnceLock<Vec<&'static str>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        let mut keys: Vec<_> = catalog()
            .keys()
            .filter(|k| {
                k.contains("{0}")
                    && (k.chars().filter(char::is_ascii_alphabetic).count() >= 4
                        || k.starts_with("Rule:"))
            })
            .map(String::as_str)
            .collect();
        keys.sort_by_key(|k| {
            std::cmp::Reverse((k.chars().filter(char::is_ascii_alphabetic).count(), k.len()))
        });
        keys
    });
    for key in patterns {
        if let Some(mut values) = captures(key, english) {
            // These fields are actions/reasons, never program names or paths.
            if depth < 2
                && ["Governor:", "Rule:", "MemoryGuard:", "Insight:", "dry-run:"]
                    .iter()
                    .any(|p| key.starts_with(p))
            {
                for value in &mut values {
                    *value = message_at_depth(value, depth + 1).into_owned();
                }
            }
            if ["priority {0}", "I/O priority {0}", "memory priority {0}"].contains(key) {
                values[0] = tr(&values[0]).to_owned();
            }
            // The service writes numbers with a point: "12.5" is "12,5"
            // in Turkish. Versions and addresses have more than one.
            for value in &mut values {
                if is_plain_decimal(value) {
                    *value = decimal(std::mem::take(value));
                }
            }
            return Cow::Owned(format(key, &values));
        }
    }
    Cow::Borrowed(english)
}

fn captures(template: &str, text: &str) -> Option<Vec<String>> {
    let mut rest = text;
    let mut pattern = template;
    let mut values = Vec::new();
    while let Some(open) = pattern.find('{') {
        rest = rest.strip_prefix(&pattern[..open])?;
        let tail = &pattern[open + 1..];
        let close = tail.find('}')?;
        let index = tail[..close].parse::<usize>().ok()?;
        if index != values.len() {
            return None;
        }
        pattern = &tail[close + 1..];
        let literal_end = pattern.find('{').unwrap_or(pattern.len());
        let delimiter = &pattern[..literal_end];
        // Adjacent placeholders cannot be recovered unambiguously from old logs.
        if delimiter.is_empty() && !pattern.is_empty() {
            return None;
        }
        let end = if pattern.is_empty() {
            rest.len()
        } else if literal_end == pattern.len() {
            rest.len()
                .checked_sub(delimiter.len())
                .filter(|_| rest.ends_with(delimiter))?
        } else {
            rest.find(delimiter)?
        };
        values.push(rest[..end].to_owned());
        rest = &rest[end..];
    }
    (rest == pattern).then_some(values)
}
/// `format` with the singular template when `n` is 1 ("1 startup app"),
/// the plural one otherwise. Languages without the distinction translate
/// both alike.
pub fn format_count(one: &str, many: &str, n: usize, values: &[String]) -> String {
    format(if n == 1 { one } else { many }, values)
}

/// A percentage in the window's language: "42%", or "%42" in Turkish,
/// which puts the sign first. `number` is already formatted ("42", "12.5").
pub fn percent(number: impl Into<String>) -> String {
    format("{0}%", &[decimal(number)])
}

/// `text`, a number formatted with a decimal point, as the language shown
/// writes it: 12,5 in Turkish or German. Only a point between two digits
/// changes. Shown text only: config, logs, IPC and copied diagnostics keep
/// the point.
pub fn decimal(text: impl Into<String>) -> String {
    let text = text.into();
    if !Language::current().decimal_comma() || !text.contains('.') {
        return text;
    }
    let chars: Vec<char> = text.chars().collect();
    chars
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let digit = |j: Option<usize>| {
                j.and_then(|j| chars.get(j))
                    .is_some_and(char::is_ascii_digit)
            };
            if c == '.' && digit(i.checked_sub(1)) && digit(Some(i + 1)) {
                ','
            } else {
                c
            }
        })
        .collect()
}

/// A byte count or rate (`wtm_core::format_bytes`) as the language shown
/// writes it: "8,0 GiB" in Turkish.
pub fn bytes(n: f64) -> String {
    decimal(wtm_core::format_bytes(n))
}

/// A length of time (`wtm_core::format_duration`) in the language's units:
/// "1m 15s" in English, "1 dk 15 sn" in Turkish, "21 Std." in German. A
/// zero part is left out ("1 dk"). Two days or more are written in days and
/// hours ("6 gün 23 sa"); a single day stays in hours ("30 sa"), as the
/// catalog's day unit is a plural.
pub fn duration(d: std::time::Duration) -> String {
    if index() == 0 {
        return wtm_core::format_duration(d);
    }
    let s = d.as_secs();
    let seconds = |n: u64| format!("{n}{}", tr(" s"));
    let minutes = |n: u64| format("{0} min", &[n.to_string()]);
    let (whole, rest) = match s {
        0..60 => return seconds(s),
        60..3600 => (
            minutes(s / 60),
            (!s.is_multiple_of(60)).then(|| seconds(s % 60)),
        ),
        3600..172_800 => (
            hours(s / 3600),
            (s % 3600 / 60 > 0).then(|| minutes(s % 3600 / 60)),
        ),
        _ => (
            format!("{}{}", s / 86_400, tr(" days")),
            (s % 86_400 / 3600 > 0).then(|| hours(s % 86_400 / 3600)),
        ),
    };
    match rest {
        Some(rest) => format!("{whole} {rest}"),
        None => whole,
    }
}

/// A number of hours: "24 h", "24 sa", "24 Std.".
pub fn hours(n: u64) -> String {
    format("{0} h", &[n.to_string()])
}

/// A period's button: "24 h", "7 days" or "30 days" in the language shown.
pub fn period(hours_back: u32) -> String {
    match hours_back {
        168 => tr("7 days").to_owned(),
        720 => tr("30 days").to_owned(),
        n => hours(u64::from(n)),
    }
}

/// The text before and after a percentage, from the same "{0}%" template as
/// [`percent`]: Turkish writes %42, so its sign goes before the number.
pub fn percent_affixes() -> (String, String) {
    let text = tr("{0}%");
    match text.split_once("{0}") {
        Some((before, after)) => (before.to_owned(), after.to_owned()),
        None => (String::new(), "%".to_owned()),
    }
}

/// A number typed in a field: "2,5" or "2.5".
pub fn parse_decimal(text: &str) -> Option<f64> {
    text.trim().replace(',', ".").parse().ok()
}

/// Shows a slider's or number field's decimals with the language's sign,
/// and accepts both signs when one is typed.
pub trait LocalDecimal {
    fn local_decimal(self) -> Self;
}

impl LocalDecimal for eframe::egui::Slider<'_> {
    fn local_decimal(self) -> Self {
        self.custom_formatter(|v, decimals| {
            decimal(eframe::egui::emath::format_with_decimals_in_range(
                v, decimals,
            ))
        })
        .custom_parser(parse_decimal)
    }
}

impl LocalDecimal for eframe::egui::DragValue<'_> {
    fn local_decimal(self) -> Self {
        self.custom_formatter(|v, decimals| {
            decimal(eframe::egui::emath::format_with_decimals_in_range(
                v, decimals,
            ))
        })
        .custom_parser(parse_decimal)
    }
}

/// Puts the percent sign where the language writes it on a slider or a
/// number field.
pub trait PercentAffix {
    fn percent_affix(self) -> Self;
}

impl PercentAffix for eframe::egui::Slider<'_> {
    fn percent_affix(self) -> Self {
        let (before, after) = percent_affixes();
        self.prefix(before).suffix(after)
    }
}

impl PercentAffix for eframe::egui::DragValue<'_> {
    fn percent_affix(self) -> Self {
        let (before, after) = percent_affixes();
        self.prefix(before).suffix(after)
    }
}

pub fn format(english: &str, values: &[String]) -> String {
    let text = tr(english);
    let mut result =
        String::with_capacity(text.len() + values.iter().map(String::len).sum::<usize>());
    let mut rest = text;
    while let Some(i) = rest.find(['{', '}']) {
        result.push_str(&rest[..i]);
        rest = &rest[i..];
        if rest.starts_with("{{") || rest.starts_with("}}") {
            result.push(rest.chars().next().unwrap());
            rest = &rest[2..];
        } else if rest.starts_with('{')
            && let Some(end) = rest.find('}')
            && let Ok(n) = rest[1..end].parse::<usize>()
            && let Some(v) = values.get(n)
        {
            result.push_str(v);
            rest = &rest[end + 1..];
            // A value ending in an abbreviation ("1 Min.") at the end of a
            // sentence takes one full stop, not two.
            if v.ends_with('.')
                && !v.ends_with("..")
                && rest.starts_with('.')
                && !rest.starts_with("..")
            {
                rest = &rest[1..];
            }
        } else {
            result.push(rest.chars().next().unwrap());
            rest = &rest[1..];
        }
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(text: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            if rest[open..].starts_with("{{") {
                rest = &rest[open + 2..];
                continue;
            }
            let tail = &rest[open + 1..];
            let end = tail.find('}').expect("unclosed placeholder");
            assert!(
                tail[..end].parse::<usize>().is_ok(),
                "invalid placeholder in {text}"
            );
            out.push(&tail[..end]);
            rest = &tail[end + 1..];
        }
        out.sort_unstable();
        out
    }
    #[test]
    fn every_translation_preserves_all_fields() {
        assert!(!catalog().is_empty());
        for (key, value) in catalog() {
            let expected = placeholders(key);
            for translated in value.texts() {
                assert!(!translated.trim().is_empty(), "empty translation: {key}");
                assert_eq!(
                    placeholders(translated),
                    expected,
                    "field mismatch: {key} -> {translated}"
                );
            }
        }
    }
    #[test]
    fn every_locale_file_translates_exactly_the_catalog_keys() {
        let edges = |text: &str| {
            let start = text.len() - text.trim_start().len();
            let end = text.trim_end().len();
            (text[..start].to_owned(), text[end..].to_owned())
        };
        for (slot, (tag, _)) in LOCALE_FILES.iter().enumerate() {
            let index = FIRST_LOCALE_FILE + slot;
            assert_eq!(BY_INDEX[index].index() as usize, index, "{tag}");
            let locale = locale_catalog(index).unwrap();
            let missing: Vec<_> = catalog()
                .keys()
                .filter(|k| !locale.contains_key(*k))
                .collect();
            assert!(missing.is_empty(), "{tag} is missing {missing:?}");
            let extra: Vec<_> = locale
                .keys()
                .filter(|k| !catalog().contains_key(*k))
                .collect();
            assert!(extra.is_empty(), "{tag} has unknown keys {extra:?}");
            for (key, translated) in locale {
                assert!(
                    !translated.trim().is_empty(),
                    "{tag}: empty translation: {key}"
                );
                assert_eq!(
                    placeholders(translated),
                    placeholders(key),
                    "{tag}: field mismatch: {key} -> {translated}"
                );
                assert_eq!(edges(translated), edges(key), "{tag}: spacing: {key:?}");
            }
        }
        assert_eq!(BY_INDEX.len(), Language::ALL.len() - 1);
        for language in BY_INDEX {
            assert!(
                Language::ALL.contains(&language),
                "{language:?} not offered"
            );
        }
    }
    #[test]
    fn every_language_translates_and_reports_itself_as_current() {
        for language in BY_INDEX {
            let _scope = scope(language);
            assert_eq!(Language::current(), language);
            if language != Language::English {
                assert_ne!(tr("Settings"), "Settings", "{language:?}");
            }
            assert_eq!(tr("an unknown future message"), "an unknown future message");
        }
        let _scope = scope(Language::Russian);
        assert_eq!(tr("Settings"), "Параметры");
        assert_eq!(
            format("User: {0}", &["{1} çğİıöşü.exe".into()]),
            tr("User: {0}").replace("{0}", "{1} çğİıöşü.exe")
        );
    }
    #[test]
    fn lookup_switches_in_all_five_languages_and_falls_back() {
        for (language, text) in [
            (Language::English, "Settings"),
            (Language::Turkish, "Ayarlar"),
            (Language::German, "Einstellungen"),
            (Language::French, "Paramètres"),
            (Language::Spanish, "Configuración"),
        ] {
            let _scope = scope(language);
            assert_eq!(tr("Settings"), text);
            assert_eq!(tr("an unknown future message"), "an unknown future message");
            assert_eq!(
                format("User: {0}", &["{1} çğİıöşü.exe".into()]),
                format!(
                    "{}{1}",
                    match language {
                        Language::English => "User: ",
                        Language::Turkish => "Kullanıcı: ",
                        Language::German => "Benutzer: ",
                        Language::French => "Utilisateur : ",
                        Language::Spanish => "Usuario: ",
                        _ => unreachable!(),
                    },
                    "{1} çğİıöşü.exe"
                )
            );
        }
    }
    #[test]
    fn service_numbers_take_the_language_decimal_sign() {
        let _scope = scope(Language::German);
        assert_eq!(
            message("average CPU 12.5% (p95 20.0%) over 125 min"),
            "durchschnittlich 12,5% CPU (p95 20,0%) über 125 Min."
        );
        assert!(is_plain_decimal("12.5") && is_plain_decimal("-0.25"));
        for value in ["1.2.3", "a.txt", "5", ".5", "5.", "192.168.0.1"] {
            assert!(!is_plain_decimal(value), "{value}");
        }
        drop(_scope);
        let _scope = scope(Language::English);
        assert_eq!(
            message("average CPU 12.5% (p95 20.0%) over 125 min"),
            "average CPU 12.5% (p95 20.0%) over 125 min"
        );
    }

    #[test]
    fn app_power_texts_and_priority_actions_translate() {
        let _scope = scope(Language::Turkish);
        for (english, kept) in [
            ("I/O priority very low", None),
            ("memory priority below normal", None),
            ("power plan High performance for 'Render'", Some("Render")),
            ("power plan Balanced again", Some("Balanced")),
            (
                "RustWTM: 'Render' keeps the computer awake",
                Some("'Render'"),
            ),
        ] {
            let shown = message(english);
            assert_ne!(shown, english);
            // The level words are translated inside the action.
            assert!(
                !shown.contains("very low") && !shown.contains("below normal"),
                "{shown}"
            );
            if let Some(kept) = kept {
                assert!(shown.contains(kept), "{shown}");
            }
        }
    }

    #[test]
    fn core_placement_reasons_translate() {
        let _scope = scope(Language::Turkish);
        for reason in [
            "keep on cores 'Builds'",
            "keep apart 'Game and stream'",
            "reserve cores 'Audio'",
            "kept off the cores reserved by 'Audio'",
            "rescue core",
            "keep apart 'Game and stream': cores split between game.exe, obs64.exe",
            "reserve cores 'Audio' (dry run): CPUs 0-1 kept for its app",
            "keep on cores 'Far': cores 40-41 name none of this machine's usable processors",
        ] {
            let shown = message(reason);
            assert_ne!(shown, reason, "{reason}");
            for name in ["Builds", "Game and stream", "Audio", "game.exe, obs64.exe"] {
                if reason.contains(name) {
                    assert!(shown.contains(name), "{shown}");
                }
            }
        }
    }

    #[test]
    fn english_names_priority_classes_in_words() {
        let _scope = scope(Language::English);
        assert_eq!(
            message("Governor: priority BelowNormal (system CPU 98%)"),
            "Governor: priority below normal (system CPU 98%)"
        );
        assert_eq!(
            message("AboveNormal, efficiency mode"),
            "above normal, efficiency mode"
        );
        assert_eq!(message("Idle"), "Idle");
    }

    #[test]
    fn durations_and_periods_use_the_language_units() {
        let d = std::time::Duration::from_secs;
        for (language, expected) in [
            (
                Language::English,
                [
                    "45s", "1m 15s", "1m 00s", "21h 00m", "6d 23h", "24 h", "7 days",
                ],
            ),
            (
                Language::Turkish,
                [
                    "45 sn",
                    "1 dk 15 sn",
                    "1 dk",
                    "21 sa",
                    "6 gün 23 sa",
                    "24 sa",
                    "7 gün",
                ],
            ),
            (
                Language::German,
                [
                    "45 s",
                    "1 Min. 15 s",
                    "1 Min.",
                    "21 Std.",
                    "6 Tage 23 Std.",
                    "24 Std.",
                    "7 Tage",
                ],
            ),
        ] {
            let _scope = scope(language);
            let shown = [
                duration(d(45)),
                duration(d(75)),
                duration(d(60)),
                duration(d(21 * 3600)),
                duration(d(6 * 86400 + 23 * 3600)),
                period(24),
                period(168),
            ];
            assert_eq!(shown, expected, "{language:?}");
        }
        let _scope = scope(Language::Turkish);
        assert_eq!(duration(d(3600 + 5 * 60 + 30)), "1 sa 5 dk");
        drop(_scope);
        let _scope = scope(Language::German);
        assert_eq!(
            format(
                "On: CPU above {0} or memory above {1} for {2}.",
                &["80 %".into(), "90 %".into(), duration(d(60))]
            ),
            "Ein: CPU über 80 % oder Speicher über 90 % für 1 Min."
        );
        assert_eq!(format("{0}.", &["Wait...".into()]), "Wait....");
    }
    #[test]
    fn scoped_languages_restore_after_unwinding_and_do_not_cross_threads() {
        let _scope = scope(Language::English);
        let _ = std::panic::catch_unwind(|| {
            let _s = scope(Language::Turkish);
            assert_eq!(tr("Settings"), "Ayarlar");
            panic!("scope test");
        });
        assert_eq!(tr("Settings"), "Settings");
        std::thread::scope(|s| {
            s.spawn(|| {
                let _s = scope(Language::French);
                assert_eq!(tr("Settings"), "Paramètres");
            })
            .join()
            .unwrap();
        });
        assert_eq!(tr("Settings"), "Settings");
    }
    #[test]
    fn region_tags_and_unknown_locales_resolve() {
        for (tag, language) in [
            ("tr-TR", Language::Turkish),
            ("FR_ca", Language::French),
            ("de-DE", Language::German),
            ("es-MX", Language::Spanish),
            ("en-GB", Language::English),
            ("ja-JP", Language::Japanese),
            ("zh-CN", Language::ChineseSimplified),
            ("zh-Hans-SG", Language::ChineseSimplified),
            ("zh-TW", Language::ChineseTraditional),
            ("zh_HK", Language::ChineseTraditional),
            ("zh-Hant", Language::ChineseTraditional),
            ("pt-PT", Language::Portuguese),
            ("ru-RU", Language::Russian),
            ("uk-UA", Language::Ukrainian),
            ("in-ID", Language::Indonesian),
            ("tl-PH", Language::Filipino),
            ("fil-PH", Language::Filipino),
            ("ko-KR", Language::Korean),
            ("hi-IN", Language::Hindi),
            ("pa-IN", Language::Punjabi),
            ("pa-Guru-IN", Language::Punjabi),
            ("pa-Arab-PK", Language::English),
            ("ar-SA", Language::English),
            ("he-IL", Language::English),
            ("", Language::English),
        ] {
            assert_eq!(Language::from_tag(tag), language);
        }
    }
    #[cfg(windows)]
    #[test]
    fn persisted_language_round_trips_and_invalid_preferences_use_windows() {
        use winreg::{RegKey, enums::HKEY_CURRENT_USER};
        let root = RegKey::predef(HKEY_CURRENT_USER);
        let path = format!(r"Software\RustWTM-tests-language-{}", std::process::id());
        let (key, _) = root.create_subkey(&path).unwrap();
        for language in Language::ALL {
            write_preference(&key, language).unwrap();
            assert_eq!(read_preference(Some(&key)), language);
        }
        let readonly = root.open_subkey(&path).unwrap();
        assert!(write_preference(&readonly, Language::Turkish).is_err());
        key.set_value("Language", &"broken JSON").unwrap();
        assert_eq!(read_preference(Some(&key)), Language::System);
        drop(readonly);
        drop(key);
        root.delete_subkey(&path).unwrap();
        assert_eq!(read_preference(None), Language::System);
    }
    #[test]
    fn known_service_messages_translate_without_altering_unknown_diagnostics() {
        let _scope = scope(Language::Turkish);
        assert_eq!(
            message("blocked from starting: çğİıöşü-{0}.exe"),
            "Başlaması engellendi: çğİıöşü-{0}.exe"
        );
        assert_eq!(
            message("0x80004005: native driver details"),
            "0x80004005: native driver details"
        );
        assert_eq!(
            captures("prefix {0} suffix", "prefix a suffix suffix"),
            Some(vec!["a suffix".into()])
        );
        assert_eq!(captures("{0}{1}", "ambiguous"), None);
        assert_eq!(
            message("Governor: priority BelowNormal (system CPU 98%, process at 40.0% (step 1/3))"),
            "Kaynak yöneticisi: öncelik Normalin altında (sistem CPU %98, işlem %40,0 (adım 1/3))"
        );
        assert_eq!(
            message("Rule: kill (rule 'Idle')"),
            "Kural: sonlandır (kural 'Idle')"
        );
        // A provider's error stays as it came.
        assert_eq!(
            message("Cooling CPU Fan: control failed: access denied"),
            "Soğutma CPU Fan: kontrol başarısız: access denied"
        );
        assert_eq!(
            message("Sensors: readings resumed"),
            "Sensörler: okuma yeniden başladı"
        );
    }
    #[test]
    fn percentages_follow_the_language() {
        {
            let _language = scope(Language::English);
            assert_eq!(percent("42"), "42%");
        }
        {
            // German writes a decimal comma.
            let _language = scope(Language::German);
            assert_eq!(percent("12.5"), "12,5%");
        }
        // Turkish puts the sign first, on sliders too.
        let _language = scope(Language::Turkish);
        assert_eq!(percent("42"), "%42");
        assert_eq!(percent_affixes(), ("%".to_owned(), String::new()));
    }

    #[test]
    fn decimals_follow_the_language() {
        {
            let _language = scope(Language::English);
            assert_eq!(bytes(8.0 * 1024.0 * 1024.0 * 1024.0), "8.0 GiB");
            assert_eq!(decimal("12.5"), "12.5");
            assert_eq!(percent("12.5"), "12.5%");
        }
        {
            let _language = scope(Language::German);
            assert_eq!(bytes(8.0 * 1024.0 * 1024.0 * 1024.0), "8,0 GiB");
            assert_eq!(percent("12.5"), "12,5%");
        }
        let _language = scope(Language::Turkish);
        assert_eq!(bytes(1536.0), "1,5 KiB");
        assert_eq!(decimal("2.25"), "2,25");
        assert_eq!(percent("12.5"), "%12,5");
        assert_eq!(percent("42"), "%42");
        // Only a point between digits: a sentence's full stop stays.
        assert_eq!(decimal("1.5 s."), "1,5 s.");
        assert_eq!(decimal("Done."), "Done.");
    }

    #[test]
    fn slider_percent_sign_follows_the_language() {
        let _language = scope(Language::English);
        assert_eq!(percent_affixes(), (String::new(), "%".to_owned()));
    }

    #[test]
    fn literal_ui_lookups_have_catalog_entries() {
        let sources = [
            include_str!("context_actions.rs"),
            include_str!("ai_assistant.rs"),
            include_str!("disk_details.rs"),
            include_str!("numeric_tray.rs"),
            include_str!("devices.rs"),
            include_str!("packages.rs"),
            include_str!("incidents.rs"),
            include_str!("file_locks.rs"),
            include_str!("rules.rs"),
            include_str!("experience.rs"),
            include_str!("onboarding.rs"),
            include_str!("app.rs"),
            include_str!("charts.rs"),
            include_str!("performance_view.rs"),
            include_str!("about.rs"),
            include_str!("alert.rs"),
            include_str!("disks_tab.rs"),
            include_str!("containers.rs"),
            include_str!("container_alerts.rs"),
            include_str!("container_history.rs"),
            include_str!("device_history.rs"),
            include_str!("event_rows.rs"),
            include_str!("backend.rs"),
            include_str!("kill_tab.rs"),
            include_str!("limits_tab.rs"),
            include_str!("cores_view.rs"),
            include_str!("app_power_view.rs"),
            include_str!("overview.rs"),
            include_str!("overview_layout.rs"),
            include_str!("support_bundle.rs"),
            include_str!("ports_tab.rs"),
            include_str!("power_controls.rs"),
            include_str!("processes.rs"),
            include_str!("memory_view.rs"),
            include_str!("diagnostics.rs"),
            include_str!("marks.rs"),
            include_str!("release_notes.rs"),
            include_str!("process_columns.rs"),
            include_str!("resource_strip.rs"),
            include_str!("workspace_inspector.rs"),
            include_str!("sensors.rs"),
            include_str!("settings.rs"),
            include_str!("preferences.rs"),
            include_str!("inventory.rs"),
            include_str!("sort.rs"),
            include_str!("telegram_settings.rs"),
            include_str!("theme.rs"),
            include_str!("tray.rs"),
            include_str!("views.rs"),
        ];
        for source in sources {
            let source = source.split("\n#[cfg(test)]\nmod tests").next().unwrap();
            for marker in ["i18n::tr(", "i18n::format("] {
                for tail in source.split(marker).skip(1) {
                    let literal = tail.trim_start();
                    if !literal.starts_with('"') {
                        continue;
                    }
                    let mut escaped = false;
                    let end = literal[1..]
                        .char_indices()
                        .find_map(|(i, c)| {
                            if escaped {
                                escaped = false;
                                None
                            } else if c == '\\' {
                                escaped = true;
                                None
                            } else if c == '"' {
                                Some(i + 2)
                            } else {
                                None
                            }
                        })
                        .unwrap();
                    let mut raw = literal[..end].replace("\r\n", "\n");
                    while let Some(start) = raw.find("\\\n") {
                        let after = &raw[start + 2..];
                        let end = start + 2 + after.len() - after.trim_start().len();
                        raw.replace_range(start..end, "");
                    }
                    let key = serde_json::from_str::<String>(&raw).unwrap();
                    assert!(catalog().contains_key(&key), "missing locale entry: {key}");
                }
            }
        }
    }
}
