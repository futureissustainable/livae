//! Livae's settings, kept in `~/Library/Application Support/Livae/settings.toml` and edited in the Settings window.

use std::cell::RefCell;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Breaks and the journal have the same table on the PC; their types live in the core.
pub use livae_core::settings::{Breaks, Habit, Journal};

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Launcher shortcut, e.g. "Opt+Space" or "Hyper+Space" (Hyper = Cmd+Ctrl+Opt+Shift).
    pub launcher: String,
    /// Clipboard shortcut.
    pub clipboard: String,
    /// Glass darkness: 0 clear … 1 black gradient.
    pub glass: f64,
    /// Paste the chosen clipboard item into the app in front (needs Accessibility).
    pub paste: bool,
    /// Clipboard items kept (pinned ones are always kept).
    pub history: usize,
    /// Folders the file search looks in; "~" is the home folder.
    pub file_search: Vec<String>,
    /// Show the most used apps on the empty launcher (off unless turned on).
    pub recent_apps: bool,
    /// Break reminders (the Stretchly Zen schedule).
    pub breaks: Breaks,
    /// The evening journal (the Vesper prompt).
    pub journal: Journal,
    /// Livae for Zen's sites, waits and pass length (the browser add-on reads them from Livae).
    pub gate: Gate,
    /// Apps whose copies the clipboard never records (bundle ids, e.g. "com.apple.keychainaccess").
    pub ignore_apps: Vec<String>,
}

/// A pause before a site: `seconds` the first time each day, `step` more for every pass that day.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct GateSite {
    pub id: String,
    pub label: String,
    pub domains: Vec<String>,
    pub seconds: u32,
    pub step: u32,
}

/// Defaults are Livae for Zen 1.3.4 (now Livae for Zen)'s config.js (31.10.2026).
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Gate {
    pub pass_minutes: u32,
    pub sites: Vec<GateSite>,
}

impl Default for Gate {
    fn default() -> Self {
        let site = |id: &str, label: &str, domains: &[&str], seconds, step| GateSite {
            id: id.into(),
            label: label.into(),
            domains: domains.iter().map(|d| d.to_string()).collect(),
            seconds,
            step,
        };
        Self {
            pass_minutes: 15,
            sites: vec![
                site("x", "X", &["twitter.com", "x.com"], 20, 5),
                site("reddit", "Reddit", &["reddit.com"], 20, 5),
                site("youtube", "YouTube", &["youtube.com"], 5, 0),
                site("instagram", "Instagram", &["instagram.com"], 5, 0),
            ],
        }
    }
}

impl Default for Settings {
    /// The shared Mac defaults from livae-core, plus Livae for Zen's sites (Mac only).
    fn default() -> Self {
        let c = livae_core::settings::Settings::defaults_for(livae_core::settings::Platform::Mac);
        Self {
            launcher: c.launcher,
            clipboard: c.clipboard,
            glass: c.glass,
            paste: c.paste,
            history: c.history,
            file_search: c.file_search,
            recent_apps: c.recent_apps,
            breaks: c.breaks,
            journal: c.journal,
            gate: Gate::default(),
            ignore_apps: c.ignore_apps,
        }
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

pub fn support_dir() -> PathBuf {
    // A `--test` copy may run on a fixture folder (LIVAE_DATA) so checks never touch real history.
    let fixture = std::env::var_os("LIVAE_DATA").filter(|_| crate::app::test_mode()).map(PathBuf::from);
    let dir = fixture.unwrap_or_else(|| home().join("Library/Application Support/Livae"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn file() -> PathBuf {
    support_dir().join("settings.toml")
}

impl Settings {
    pub fn roots(&self) -> Vec<PathBuf> {
        let roots: Vec<PathBuf> = self
            .file_search
            .iter()
            .map(|p| if let Some(rest) = p.strip_prefix('~') { home().join(rest.trim_start_matches('/')) } else { PathBuf::from(p) })
            .filter(|p| p.is_dir())
            .collect();
        if roots.is_empty() { vec![home()] } else { roots }
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Settings>> = const { RefCell::new(None) };
}

pub fn get() -> Settings {
    CURRENT.with(|c| {
        c.borrow_mut()
            .get_or_insert_with(|| std::fs::read_to_string(file()).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default())
            .clone()
    })
}

/// Change the settings, save them, and let every part of the app pick them up.
pub fn update(f: impl FnOnce(&mut Settings)) {
    let mut s = get();
    let before = s.clone();
    f(&mut s);
    if s == before {
        return;
    }
    if crate::app::test_mode() {
        CURRENT.with(|c| *c.borrow_mut() = Some(s.clone()));
        crate::app::settings_changed(&before, &s);
        return;
    }
    if let Ok(text) = toml::to_string_pretty(&s) {
        let _ = livae_core::write_atomic(&file(), format!("# Livae settings. Change them in Livae → Settings.\n\n{text}").as_bytes());
    }
    CURRENT.with(|c| *c.borrow_mut() = Some(s.clone()));
    crate::app::settings_changed(&before, &s);
}
