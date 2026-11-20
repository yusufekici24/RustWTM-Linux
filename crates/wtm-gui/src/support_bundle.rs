//! The support bundle: one text file to send with a problem report. The
//! user reads all of it before saving, and what is saved is exactly what
//! was shown. Secrets and the account name are always hidden; process
//! names and paths only unless the user includes them. Configuration
//! files, command lines, environment variables, window titles and
//! container logs are never part of it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use eframe::egui::{self, RichText, Ui};
use wtm_ipc::{Event, EventLevel, Status};

/// Events with a warning or an error, newest last.
pub const EVENTS_KEPT: usize = 100;
/// Warning and error lines kept from the service log.
pub const LOG_LINES_KEPT: usize = 200;
/// How much of the newest service log file is read, from its end.
pub const LOG_BYTES_READ: u64 = 128 * 1024;

/// What the bundle is made from.
pub struct Input<'a> {
    /// The privacy-safe report About copies (`diagnostics::Facts::report`).
    pub diagnostics: String,
    /// The engine as `diagnostics` names it ("service", "local", …).
    pub engine: &'a str,
    pub gui_elevated: bool,
    /// Why the service cannot be reached or used, if it cannot.
    pub service_error: Option<&'a str>,
    pub status: Option<&'a Status>,
    /// The event log as the window keeps it, oldest first.
    pub events: Vec<&'a Event>,
    /// The process names this window knows of, hidden like `.exe` names.
    pub names: Vec<&'a str>,
    pub log: LogTail,
    /// When the bundle was made, local time.
    pub created: String,
}

/// The end of the newest service log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogTail {
    /// Its warning and error lines, oldest first.
    Lines(Vec<String>),
    /// No log file in the folder.
    Missing,
    /// The folder or file could not be read from this window.
    Unreadable(String),
}

/// Reads the warnings and errors at the end of the newest
/// `wtm-service.log.*` in `dir`: at most [`LOG_BYTES_READ`] bytes, keeping
/// the last [`LOG_LINES_KEPT`] lines.
pub fn service_log_tail(dir: &Path) -> LogTail {
    let newest = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("wtm-service.log")
            })
            // Daily files are named by date, so the newest sorts last.
            .max_by_key(std::fs::DirEntry::file_name),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LogTail::Missing,
        Err(e) => return LogTail::Unreadable(e.to_string()),
    };
    let Some(newest) = newest else {
        return LogTail::Missing;
    };
    match read_end(&newest.path()) {
        Ok(text) => {
            let mut lines: Vec<String> = text
                .lines()
                .filter(|l| l.contains(" WARN ") || l.contains(" ERROR "))
                .map(str::to_owned)
                .collect();
            let skip = lines.len().saturating_sub(LOG_LINES_KEPT);
            lines.drain(..skip);
            LogTail::Lines(lines)
        }
        Err(e) => LogTail::Unreadable(e.to_string()),
    }
}

/// The last [`LOG_BYTES_READ`] bytes of `path`, starting at a whole line.
fn read_end(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(LOG_BYTES_READ);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.take(LOG_BYTES_READ).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(if start > 0 {
        // The first line was cut by the seek.
        text.split_once('\n')
            .map_or(String::new(), |(_, rest)| rest.to_owned())
    } else {
        text
    })
}

/// `%ProgramData%\RustWTM\logs`, where the service writes its log.
pub fn service_logs_dir() -> PathBuf {
    if cfg!(test) {
        // Tests never read this PC's real log.
        return std::env::temp_dir().join("rustwtm-test-no-service-logs");
    }
    std::env::var_os("ProgramData")
        .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from)
        .join("RustWTM")
        .join("logs")
}

/// The input for this window, with the service log read now.
pub fn from_window<'a>(data: &'a crate::backend::Data, ctx: &egui::Context) -> Input<'a> {
    let mut names: Vec<&str> = data
        .snapshot
        .as_deref()
        .map(|s| s.processes.iter().map(|p| p.name.as_str()).collect())
        .unwrap_or_default();
    names.extend(data.events.iter().filter_map(|e| e.name.as_deref()));
    names.sort_unstable();
    names.dedup();
    Input {
        diagnostics: crate::diagnostics::for_window(data, ctx),
        engine: crate::diagnostics::engine(data.mode),
        gui_elevated: data.elevated,
        service_error: data.service_error.as_deref(),
        status: data.status.as_ref(),
        events: data.events.iter().collect(),
        names,
        log: service_log_tail(&service_logs_dir()),
        created: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
    }
}

/// What the user chose to include.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// Process names and paths outside the user profile, shown as written.
    pub names_and_paths: bool,
}

/// Who is hidden whatever was chosen: the account name and the profile
/// folder, which can differ from it and can contain spaces.
pub struct Account {
    pub redactor: wtm_ai::Redactor,
    /// `%USERPROFILE%`, e.g. `C:\Users\John Smith`.
    pub profile: Option<String>,
}

impl Account {
    pub fn current() -> Self {
        Self {
            redactor: wtm_ai::Redactor::for_current_user(),
            profile: std::env::var("USERPROFILE").ok(),
        }
    }
}

/// Hides what the bundle must not carry.
struct Redaction<'a> {
    account: Account,
    names: &'a [&'a str],
    keep_names_and_paths: bool,
}

impl Redaction<'_> {
    fn apply(&self, text: &str) -> String {
        let mut text = secret_urls(text);
        if let Some(profile) = self.account.profile.as_deref().filter(|p| p.len() > 3) {
            text = replace_ignore_case(&text, profile, "%USERPROFILE%");
        }
        // Secrets in flags and headers, user-profile paths and the account
        // name, whatever was chosen, before paths are cut at their spaces.
        text = self.account.redactor.redact(&text);
        if !self.keep_names_and_paths {
            text = command_patterns(&text);
            text = paths(&text);
            text = exe_names(&text);
            for name in self.names.iter().filter(|n| n.chars().count() >= 3) {
                // Anywhere, not only as a word: a name glued to other text
                // is still hidden, at the cost of some log targets.
                text = replace_ignore_case(&text, name, "<process>");
            }
        }
        text
    }
}

/// Hides the command-line text of a kill-list entry, as its label quotes it:
/// `node.exe running "vitest"`.
fn command_patterns(text: &str) -> String {
    const MARK: &str = " running \"";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(MARK) {
        let (head, tail) = rest.split_at(at + MARK.len());
        out.push_str(head);
        match tail.find('"') {
            Some(end) => {
                out.push_str("<pattern>");
                rest = &tail[end..];
            }
            None => {
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Masks a Telegram bot token in a URL path (`/bot123:abc/`) and the
/// values of `key=`, `token=` and similar query parameters.
fn secret_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("/bot") {
        let (head, tail) = rest.split_at(at + "/bot".len());
        out.push_str(head);
        let end = tail
            .find(|c: char| c == '/' || c == '?' || c.is_whitespace() || c == '"')
            .unwrap_or(tail.len());
        let token = &tail[..end];
        if token.contains(':') && token.starts_with(|c: char| c.is_ascii_digit()) {
            out.push_str("<token>");
        } else {
            out.push_str(token);
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    let mut text = out;
    for key in [
        "key=",
        "token=",
        "api_key=",
        "apikey=",
        "access_token=",
        "secret=",
    ] {
        let mut out = String::with_capacity(text.len());
        let mut rest = text.as_str();
        while let Some(at) = find_ignore_case(rest, key) {
            // Only a parameter: after `?` or `&`.
            let before = rest[..at].chars().next_back();
            let (head, tail) = rest.split_at(at + key.len());
            out.push_str(head);
            let end = tail
                .find(|c: char| c == '&' || c == '#' || c.is_whitespace() || c == '"')
                .unwrap_or(tail.len());
            if matches!(before, Some('?' | '&')) && end > 0 {
                out.push_str("<secret>");
            } else {
                out.push_str(&tail[..end]);
            }
            rest = &tail[end..];
        }
        out.push_str(rest);
        text = out;
    }
    text
}

/// Replaces absolute paths (`D:\…`, `\\server\…`) with `<path>`. A quoted
/// path ends at its quote. An unquoted one goes on across a space while the
/// next word still looks like part of it (see [`continues_path`]), and stops
/// at the first word that does not.
fn paths(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let drive = i + 2 < chars.len()
            && chars[i].is_ascii_alphabetic()
            && chars[i + 1] == ':'
            && matches!(chars[i + 2], '\\' | '/')
            && (i == 0 || !chars[i - 1].is_alphanumeric());
        let unc = i + 2 < chars.len()
            && chars[i] == '\\'
            && chars[i + 1] == '\\'
            && !chars[i + 2].is_whitespace();
        // `%USERPROFILE%\Documents\…`, as the profile was written above.
        let variable = chars[i] == '%'
            && (i == 0 || !chars[i - 1].is_alphanumeric())
            && chars[i + 1..]
                .iter()
                .position(|&c| c == '%')
                .is_some_and(|end| {
                    let name = &chars[i + 1..i + 1 + end];
                    !name.is_empty()
                        && name.iter().all(|c| c.is_ascii_alphanumeric() || *c == '_')
                        && matches!(chars.get(i + 2 + end), Some('\\' | '/'))
                });
        if drive || unc || variable {
            let quoted = i > 0 && chars[i - 1] == '"';
            let ends = |c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | ')');
            let mut j = i;
            if quoted {
                while j < chars.len() && chars[j] != '"' {
                    j += 1;
                }
            } else {
                loop {
                    while j < chars.len() && !ends(chars[j]) {
                        j += 1;
                    }
                    // A space inside the path: the next word goes on with it.
                    let mut k = j;
                    while k < chars.len() && chars[k] == ' ' {
                        k += 1;
                    }
                    let mut m = k;
                    while m < chars.len() && !ends(chars[m]) {
                        m += 1;
                    }
                    if j < chars.len() && chars[j] == ' ' && m > k && continues_path(&chars[k..m]) {
                        j = m;
                    } else {
                        break;
                    }
                }
            }
            out.push_str("<path>");
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Whether a word after a space inside a path still belongs to it: it has
/// a `\` or `/` (`C:\Users\John Smith\x`), or ends in a file extension
/// (`C:\x\my notes.txt`). A version number after a path is hidden with it.
fn continues_path(word: &[char]) -> bool {
    if word.iter().any(|c| matches!(c, '\\' | '/')) {
        return true;
    }
    let Some(dot) = word.iter().rposition(|&c| c == '.') else {
        return false;
    };
    let extension = &word[dot + 1..];
    dot > 0
        && (1..=5).contains(&extension.len())
        && extension.iter().all(char::is_ascii_alphanumeric)
}

/// Replaces every word ending in `.exe` with `<process>`.
fn exe_names(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if word.len() > 4 && word.to_ascii_lowercase().ends_with(".exe") {
            out.push_str("<process>");
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+') {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    let lower = haystack.to_ascii_lowercase();
    lower.find(&needle.to_ascii_lowercase())
}

fn replace_ignore_case(text: &str, needle: &str, with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = find_ignore_case(rest, needle) {
        out.push_str(&rest[..at]);
        out.push_str(with);
        rest = &rest[at + needle.len()..];
    }
    out.push_str(rest);
    out
}

/// The bundle's text. Numbers use the point whatever the window's language:
/// it is read by whoever answers the report.
pub fn build(input: &Input<'_>, options: Options) -> String {
    build_with(input, options, Account::current())
}

fn build_with(input: &Input<'_>, options: Options, account: Account) -> String {
    let r = Redaction {
        account,
        names: &input.names,
        keep_names_and_paths: options.names_and_paths,
    };
    let mut s = String::new();
    let mut line = |text: &str| {
        s.push_str(text);
        s.push('\n');
    };
    line("RustWTM support bundle");
    line(&format!("Created: {}", input.created));
    line(if options.names_and_paths {
        "Process names and paths: included"
    } else {
        "Process names and paths: hidden"
    });
    line("");
    line("== Diagnostics ==");
    for l in input.diagnostics.lines() {
        line(&r.apply(l));
    }
    line("");
    line("== Connection ==");
    line(&format!("Engine: {}", input.engine));
    line(&format!("Protocol: {}", wtm_ipc::PROTOCOL_VERSION));
    line(&format!(
        "This window: {}",
        if input.gui_elevated {
            "administrator"
        } else {
            "standard user"
        }
    ));
    line(&format!(
        "Service problem: {}",
        input.service_error.map_or("none".into(), |e| r.apply(e))
    ));
    line("");
    line("== Service status ==");
    match input.status {
        None => line("Not available"),
        Some(st) => {
            let yes = |b: bool| if b { "yes" } else { "no" };
            line(&format!("Dry run: {}", yes(st.dry_run)));
            line(&format!(
                "Governor: {}",
                if st.governor_enabled { "on" } else { "off" }
            ));
            line(&format!(
                "Governor thresholds: {:.0}% high, {:.0}% recovered",
                st.high_cpu_percent, st.recover_cpu_percent
            ));
            line(&format!("Rules: {}", st.rules));
            line(&format!("Processes held by limits: {}", st.managed.len()));
            line(&format!("App limits: {}", st.app_limits.len()));
            line(&format!(
                "Foreground app protected: {}",
                yes(st.foreground.is_some())
            ));
            line(&match &st.rescue_core {
                Some(c) => format!(
                    "Rescue core: engaged, {} processors kept, {} processes moved",
                    c.cpus.len(),
                    c.moved
                ),
                None => "Rescue core: not engaged".into(),
            });
            line(&match &st.game {
                Some(g) => format!("Game mode: active, {} processes calmed", g.calmed),
                None => "Game mode: not active".into(),
            });
            // The parser's first line names the place; the lines after it
            // quote the file.
            line(&format!(
                "Configuration: {}",
                st.config_error
                    .as_deref()
                    .map_or("loaded".into(), |e| format!(
                        "not loaded: {}",
                        r.apply(e.lines().next().unwrap_or_default())
                    ))
            ));
        }
    }
    line("");
    let problems: Vec<&&Event> = input
        .events
        .iter()
        .filter(|e| e.level != EventLevel::Info)
        .collect();
    let shown = &problems[problems.len().saturating_sub(EVENTS_KEPT)..];
    line(&format!(
        "== Recent warnings and errors ({} of {}) ==",
        shown.len(),
        problems.len()
    ));
    if shown.is_empty() {
        line("None");
    }
    for e in shown {
        let when = chrono::DateTime::from_timestamp_millis(e.unix_ms as i64).map_or_else(
            || "?".into(),
            |t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            },
        );
        let level = if e.level == EventLevel::Error {
            "ERROR"
        } else {
            "WARN"
        };
        line(&format!("{when} {level} {}", r.apply(&e.message)));
    }
    line("");
    line("== Service log (warnings and errors, newest file) ==");
    match &input.log {
        LogTail::Lines(lines) if lines.is_empty() => line("None"),
        LogTail::Lines(lines) => {
            for l in lines {
                line(&r.apply(l));
            }
        }
        LogTail::Missing => line("No log file"),
        LogTail::Unreadable(e) => line(&format!("Not readable from this window: {}", r.apply(e))),
    }
    line("");
    line("== Not included ==");
    line(
        "config.toml, telegram.toml and ai.toml; secrets and keys; command lines; \
         environment variables; window titles; container logs; the account name.",
    );
    s
}

/// Asks where to save; `None` when the user cancels.
pub type AskPath = fn(owner: isize, suggested: &str) -> Result<Option<PathBuf>, String>;
/// Starts the save off the UI thread.
pub type Spawn = fn(Box<dyn FnOnce() + Send>) -> std::io::Result<()>;

fn ask_path(owner: isize, suggested: &str) -> Result<Option<PathBuf>, String> {
    wtm_sys::file_dialog::save_as(
        owner,
        crate::i18n::tr("Save support bundle"),
        suggested,
        "txt",
        &[wtm_sys::file_dialog::FileType {
            name: crate::i18n::tr("Text file"),
            pattern: "*.txt",
        }],
    )
    .map_err(|e| e.to_string())
}

fn spawn(job: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("support-bundle".into())
        .spawn(job)
        .map(drop)
}

/// How a save ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Saved {
    Cancelled,
    To(PathBuf),
    Failed(String),
}

/// The open bundle window.
pub struct View {
    pub options: Options,
    /// The text shown, and saved or copied as it is; empty until built.
    text: String,
    built_with: Option<Options>,
    saving: Option<Arc<Mutex<Option<Saved>>>>,
    outcome: Option<Saved>,
    ask: AskPath,
    spawn: Spawn,
}

impl Default for View {
    fn default() -> Self {
        Self {
            options: Options::default(),
            text: String::new(),
            built_with: None,
            saving: None,
            outcome: None,
            ask: ask_path,
            spawn,
        }
    }
}

impl View {
    /// Whether the text must be made (again) before showing.
    pub fn needs_text(&self) -> bool {
        self.built_with != Some(self.options)
    }

    pub fn set_text(&mut self, text: String) {
        self.text = text;
        self.built_with = Some(self.options);
    }

    #[cfg(test)]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[cfg(test)]
    fn with_seams(ask: AskPath, spawn: Spawn) -> Self {
        Self {
            ask,
            spawn,
            ..Self::default()
        }
    }

    fn start_save(&mut self, ctx: &egui::Context, owner: isize) {
        let slot = Arc::new(Mutex::new(None));
        let (out, ctx2, text, ask) = (slot.clone(), ctx.clone(), self.text.clone(), self.ask);
        let suggested = format!(
            "RustWTM-support-{}.txt",
            chrono::Local::now().format("%Y%m%d-%H%M")
        );
        let job = Box::new(move || {
            let saved = match ask(owner, &suggested) {
                Ok(None) => Saved::Cancelled,
                Ok(Some(path)) => match std::fs::write(&path, text.as_bytes()) {
                    Ok(()) => Saved::To(path),
                    Err(e) => Saved::Failed(e.to_string()),
                },
                Err(e) => Saved::Failed(e),
            };
            *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(saved);
            ctx2.request_repaint();
        });
        match (self.spawn)(job) {
            Ok(()) => {
                self.outcome = None;
                self.saving = Some(slot);
            }
            Err(e) => self.outcome = Some(Saved::Failed(e.to_string())),
        }
    }

    /// Picks up a finished save.
    fn poll(&mut self) {
        let done = self
            .saving
            .as_ref()
            .and_then(|slot| slot.lock().unwrap_or_else(PoisonError::into_inner).take());
        if let Some(saved) = done {
            self.saving = None;
            self.outcome = Some(saved);
        }
    }
}

/// Shows the window; returns `false` once it is closed. `owner` is the
/// main window's handle, for the Save As dialog.
pub fn window(ctx: &egui::Context, view: &mut View, owner: isize) -> bool {
    view.poll();
    let mut close = false;
    let screen = ctx.content_rect().size();
    // Fits a compact window: the text scrolls inside, the buttons stay.
    let width = (screen.x - 64.0).clamp(300.0, 680.0);
    let text_height = (screen.y * 0.5).clamp(120.0, 420.0);
    let modal = egui::Modal::new(egui::Id::unique("support_bundle"))
        .frame(crate::theme::dialog_frame(ctx))
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.heading(crate::i18n::tr("Support bundle"));
            ui.label(crate::i18n::tr(
                "Everything below is what will be saved. Secrets and your account name are always hidden; process names and paths are hidden unless you include them.",
            ));
            if ui
                .checkbox(
                    &mut view.options.names_and_paths,
                    crate::i18n::tr("Include process names and paths"),
                )
                .changed()
            {
                // The text is made again before the next frame.
                ui.ctx().request_repaint();
            }
            egui::Frame::group(ui.style()).show(ui, |ui| {
                egui::ScrollArea::both()
                    .id_salt("support_bundle_text")
                    .auto_shrink([false, true])
                    .min_scrolled_height(text_height)
                    .max_height(text_height)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&view.text).monospace().small())
                                .selectable(true)
                                .extend(),
                        );
                    });
            });
            outcome(ui, view);
            ui.horizontal(|ui| {
                let saving = view.saving.is_some();
                if ui
                    .add_enabled(
                        !saving && !view.text.is_empty() && !view.needs_text(),
                        egui::Button::new(crate::i18n::tr("Save…")),
                    )
                    .clicked()
                {
                    view.start_save(ui.ctx(), owner);
                }
                if ui.button(crate::i18n::tr("Copy")).clicked() {
                    ui.ctx().copy_text(view.text.clone());
                }
                if ui.button(crate::i18n::tr("Close")).clicked() {
                    close = true;
                }
                if saving {
                    ui.spinner();
                }
            });
        });
    // Esc and a click beside the dialog close it too, but not while the
    // Save As dialog is open: its answer comes back here.
    !(close || modal.should_close() && view.saving.is_none())
}

fn outcome(ui: &mut Ui, view: &View) {
    match &view.outcome {
        Some(Saved::To(path)) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(crate::i18n::format(
                    "Saved to {0}",
                    &[path.display().to_string()],
                ));
                if ui.link(crate::i18n::tr("Show in folder")).clicked() {
                    let _ = crate::backend::reveal(path);
                }
            });
        }
        Some(Saved::Failed(e)) => {
            crate::theme::colored(
                ui,
                crate::theme::BAD,
                crate::i18n::format(
                    "Could not save the support bundle: {0}",
                    std::slice::from_ref(e),
                ),
            );
        }
        Some(Saved::Cancelled) | None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};

    fn event(level: EventLevel, name: Option<&str>, message: &str) -> Event {
        Event {
            id: 1,
            unix_ms: 1_763_000_000_000,
            level,
            pid: Some(42),
            name: name.map(str::to_owned),
            message: message.into(),
            notify: false,
        }
    }

    fn account() -> Account {
        Account {
            redactor: wtm_ai::Redactor {
                user_name: Some("kaanuser".into()),
            },
            profile: Some(r"C:\Users\Kaan Ersoy".into()),
        }
    }

    /// A unique, empty folder under the temp directory.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rustwtm-bundle-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const PLANTED: &[&str] = &[
        "AAH-secretToken",
        "AIzaSECRET123",
        "sk-abcdef123456",
        "kaanuser",
        "Kaan Ersoy",
        "Ersoy",
    ];
    const NAMES_AND_PATHS: &[&str] = &[
        "clientapp.exe",
        r"D:\Clients\Acme\plan.txt",
        "secretproc.exe",
        "vmmem",
        r"\\fileserver\share",
        "secret-app.exe",
        "Acme Corp",
        r"Corp\",
        "budget.xlsx",
        "vitest",
        "Documents",
        "notes.txt",
    ];

    fn planted_bundle(options: Options) -> String {
        let status = Status {
            config_error: Some(
                "TOML parse error at line 3, column 9\n  |\n3 | name = \"secret-app.exe\"\n".into(),
            ),
            ..Default::default()
        };
        let events = [
            event(
                EventLevel::Warn,
                None,
                "Telegram failed: https://api.telegram.org/bot123456:AAH-secretToken/getUpdates",
            ),
            event(
                EventLevel::Error,
                None,
                "provider error https://example.test/v1/models?alt=json&key=AIzaSECRET123",
            ),
            event(
                EventLevel::Warn,
                None,
                "header Authorization: Bearer sk-abcdef123456",
            ),
            event(
                EventLevel::Warn,
                Some("clientapp.exe"),
                "ended clientapp.exe (pid 42) using 95% CPU",
            ),
            event(EventLevel::Warn, Some("vmmem"), "vmmem holds 8 GiB"),
            event(
                EventLevel::Warn,
                None,
                r"cannot read D:\Clients\Acme Corp\budget.xlsx now",
            ),
            event(
                EventLevel::Warn,
                None,
                "KillList: Terminate (kill list 'node.exe running \"vitest\"': 95% CPU for 60 s)",
            ),
            event(EventLevel::Info, None, "started"),
        ];
        let input = Input {
            diagnostics: "RustWTM diagnostic information\nVersion: 0.35.0".into(),
            engine: "service",
            gui_elevated: false,
            service_error: Some(r"pipe \\fileserver\share\x denied"),
            status: Some(&status),
            events: events.iter().collect(),
            names: vec!["vmmem"],
            log: LogTail::Lines(vec![
                r"2026-11-17T10:00:00Z  WARN wtm_service: could not open D:\Clients\Acme\plan.txt for secretproc.exe".into(),
                r"2026-11-17T10:00:01Z ERROR wtm_service: C:\Users\kaanuser\AppData\Local\x failed".into(),
                r"2026-11-17T10:00:02Z  WARN wtm_service: C:\Users\Kaan Ersoy\Documents\notes.txt locked".into(),
            ]),
            created: "2026-11-17 10:00".into(),
        };
        build_with(&input, options, account())
    }

    #[test]
    fn nothing_personal_reaches_the_bundle() {
        let text = planted_bundle(Options::default());
        for planted in PLANTED.iter().chain(NAMES_AND_PATHS) {
            assert!(!text.contains(planted), "{planted} in:\n{text}");
        }
        // What is useful stays: where the configuration failed, the
        // problem itself, and that something was hidden.
        assert!(
            text.contains("not loaded: TOML parse error at line 3, column 9"),
            "{text}"
        );
        assert!(
            text.contains("ended <process> (pid 42) using 95% CPU"),
            "{text}"
        );
        assert!(
            text.contains("could not open <path> for <process>"),
            "{text}"
        );
        assert!(text.contains("cannot read <path> now"), "{text}");
        assert!(text.contains("<process> running \"<pattern>\""), "{text}");
        assert!(text.contains("Process names and paths: hidden"));
        assert!(!text.contains("started"), "info events are left out");
    }

    #[test]
    fn paths_go_on_across_spaces_while_they_still_look_like_paths() {
        assert_eq!(
            paths(r"open C:\Program Files\App\a b.txt failed"),
            "open <path> failed"
        );
        assert_eq!(paths(r#"open "C:\x y\z" now"#), r#"open "<path>" now"#);
        assert_eq!(paths(r"at D:\data, then"), "at <path>, then");
        assert_eq!(
            paths(r"saved C:\Users\Kaan Ersoy\my notes.txt and left"),
            "saved <path> and left"
        );
        assert_eq!(
            paths(r"%USERPROFILE%\My Files\a.txt locked, 100% sure"),
            "<path> locked, 100% sure"
        );
        // Not a path: no drive, or a letter inside a word.
        for same in ["ratio 3:2 and https://x.test/a", "abc:\\ no"] {
            assert_eq!(paths(same), same);
        }
    }

    #[test]
    fn tokens_in_urls_are_masked_and_nothing_else() {
        assert_eq!(
            secret_urls("GET https://x.test/a?alt=json&token=plainvalue42 failed"),
            "GET https://x.test/a?alt=json&token=<secret> failed"
        );
        assert_eq!(
            secret_urls("https://x.test/?Key=abc&x=1"),
            "https://x.test/?Key=<secret>&x=1"
        );
        assert_eq!(
            secret_urls("https://api.telegram.org/bot42:plainvalue/getMe"),
            "https://api.telegram.org/bot<token>/getMe"
        );
        // Not a parameter, not a token.
        for same in ["monkey=1 donkey=2", "see /bots/list and /bot/x", "token=1"] {
            assert_eq!(secret_urls(same), same);
        }
    }

    #[test]
    fn names_and_paths_come_back_when_chosen_and_secrets_never_do() {
        let text = planted_bundle(Options {
            names_and_paths: true,
        });
        for planted in PLANTED {
            assert!(!text.contains(planted), "{planted} in:\n{text}");
        }
        for wanted in [
            "clientapp.exe",
            r"D:\Clients\Acme\plan.txt",
            r"D:\Clients\Acme Corp\budget.xlsx",
            "secretproc.exe",
            "vmmem",
            "node.exe running \"vitest\"",
            r"%USERPROFILE%\Documents\notes.txt",
        ] {
            assert!(text.contains(wanted), "{wanted} missing:\n{text}");
        }
        assert!(text.contains("Process names and paths: included"));
    }

    #[test]
    fn only_the_newest_warnings_are_kept() {
        let events: Vec<Event> = (0..EVENTS_KEPT + 30)
            .map(|i| event(EventLevel::Warn, None, &format!("problem {i}")))
            .collect();
        let input = Input {
            diagnostics: String::new(),
            engine: "local",
            gui_elevated: true,
            service_error: None,
            status: None,
            events: events.iter().collect(),
            names: Vec::new(),
            log: LogTail::Unreadable("Access is denied. (os error 5)".into()),
            created: String::new(),
        };
        let text = build_with(&input, Options::default(), account());
        assert!(text.contains(&format!("({EVENTS_KEPT} of {})", EVENTS_KEPT + 30)));
        assert!(!text.contains("problem 29\n") && text.contains("problem 30\n"));
        assert!(text.contains(&format!("problem {}\n", EVENTS_KEPT + 29)));
        // An unreadable log is said, not left out.
        assert!(text.contains("Not readable from this window: Access is denied."));
        assert!(text.contains("== Service status ==\nNot available"));
    }

    #[test]
    fn numbers_keep_the_point_in_every_language() {
        let _turkish = crate::i18n::scope(crate::i18n::Language::Turkish);
        let diagnostics = crate::diagnostics::Facts {
            memory: 8 << 30,
            scale: 1.25,
            ..Default::default()
        }
        .report();
        let status = Status {
            high_cpu_percent: 85.0,
            recover_cpu_percent: 60.0,
            ..Default::default()
        };
        let input = Input {
            diagnostics,
            engine: "service",
            gui_elevated: false,
            service_error: None,
            status: Some(&status),
            events: Vec::new(),
            names: Vec::new(),
            log: LogTail::Missing,
            created: String::new(),
        };
        let text = build_with(&input, Options::default(), account());
        assert!(text.contains("8.0 GiB"), "{text}");
        assert!(text.contains("Interface scale: 125%"), "{text}");
        assert!(text.contains("85% high, 60% recovered"), "{text}");
    }

    #[test]
    fn the_log_is_read_from_the_end_of_the_newest_file() {
        let dir = scratch("log");
        assert_eq!(service_log_tail(&dir.join("absent")), LogTail::Missing);
        assert_eq!(service_log_tail(&dir), LogTail::Missing);
        std::fs::write(dir.join("notes.txt"), " WARN not a log\n").unwrap();
        std::fs::write(
            dir.join("wtm-service.log.2026-11-16"),
            "2026-11-16T01:00:00Z  WARN older day\n",
        )
        .unwrap();
        // More than is read: the start is skipped, the cut line dropped.
        let mut newest = String::new();
        for i in 0..3000 {
            newest.push_str(&format!(
                "2026-11-17T01:00:00Z  INFO wtm_service: line {i}\n"
            ));
            newest.push_str(&format!(
                "2026-11-17T01:00:00Z  WARN wtm_service: warning {i}\n"
            ));
        }
        assert!(newest.len() as u64 > LOG_BYTES_READ);
        std::fs::write(dir.join("wtm-service.log.2026-11-17"), &newest).unwrap();
        let LogTail::Lines(lines) = service_log_tail(&dir) else {
            panic!("lines expected");
        };
        assert_eq!(lines.len(), LOG_LINES_KEPT);
        assert!(
            lines
                .iter()
                .all(|l| l.starts_with("2026-11-17T01:00:00Z  WARN"))
        );
        assert!(lines.last().unwrap().ends_with("warning 2999"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn harness(view: View) -> Harness<'static, View> {
        Harness::builder()
            .allow_missing_glyphs()
            .with_size([900.0, 700.0])
            .build_ui_state(
                |ui, view: &mut View| {
                    let ctx = ui.ctx().clone();
                    if view.needs_text() {
                        view.set_text(format!(
                            "bundle text, names {}",
                            view.options.names_and_paths
                        ));
                    }
                    window(&ctx, view, 0);
                },
                view,
            )
    }

    fn inline(job: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
        job();
        Ok(())
    }

    fn save_target() -> PathBuf {
        std::env::temp_dir().join(format!("rustwtm-bundle-save-{}.txt", std::process::id()))
    }

    #[test]
    fn what_is_saved_is_what_was_shown() {
        fn ask(_: isize, suggested: &str) -> Result<Option<PathBuf>, String> {
            assert!(suggested.starts_with("RustWTM-support-") && suggested.ends_with(".txt"));
            Ok(Some(save_target()))
        }
        let mut h = harness(View::with_seams(ask, inline));
        h.run();
        assert_eq!(h.state().text(), "bundle text, names false");
        h.get_by_label("Include process names and paths").click();
        h.run();
        assert_eq!(h.state().text(), "bundle text, names true");
        h.get_by_label("Save…").click();
        h.run();
        let saved = std::fs::read_to_string(save_target()).unwrap();
        assert_eq!(saved, "bundle text, names true");
        assert!(h.query_by_label_contains("Saved to").is_some());
        assert!(h.query_by_label("Show in folder").is_some());
        let _ = std::fs::remove_file(save_target());
    }

    #[test]
    fn a_cancelled_or_failed_save_writes_nothing() {
        fn cancel(_: isize, _: &str) -> Result<Option<PathBuf>, String> {
            Ok(None)
        }
        let mut h = harness(View::with_seams(cancel, inline));
        h.run();
        h.get_by_label("Save…").click();
        h.run();
        assert!(h.query_by_label_contains("Saved to").is_none());
        assert!(h.query_by_label_contains("Could not save").is_none());

        // A place that cannot be written: said, nothing half-written.
        fn missing_folder(_: isize, _: &str) -> Result<Option<PathBuf>, String> {
            Ok(Some(
                std::env::temp_dir()
                    .join("rustwtm-no-such-folder-for-bundle")
                    .join("bundle.txt"),
            ))
        }
        let mut h = harness(View::with_seams(missing_folder, inline));
        h.run();
        h.get_by_label("Save…").click();
        h.run();
        assert!(
            h.query_by_label_contains("Could not save the support bundle:")
                .is_some()
        );
        assert!(h.query_by_label_contains("Saved to").is_none());

        fn unreachable(_: isize, _: &str) -> Result<Option<PathBuf>, String> {
            panic!("no dialog without a thread")
        }
        fn no_thread(_: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
            Err(std::io::Error::other("no thread available"))
        }
        let mut h = harness(View::with_seams(unreachable, no_thread));
        h.run();
        h.get_by_label("Save…").click();
        h.run();
        assert!(
            h.query_by_label_contains("Could not save the support bundle: no thread available")
                .is_some()
        );
        // Save is offered again.
        assert!(!h.get_by_label("Save…").accesskit_node().is_disabled());
    }
}
