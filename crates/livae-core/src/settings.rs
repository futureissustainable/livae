//! Livae's settings, the same `settings.toml` fields on the Mac and the PC. Only the defaults differ.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Mac,
    Windows,
}

impl Platform {
    pub const fn current() -> Self {
        if cfg!(windows) { Platform::Windows } else { Platform::Mac }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Launcher shortcut, e.g. "Opt+Space" or "Hyper+Space" (Hyper = Cmd+Ctrl+Opt+Shift on the Mac, Ctrl+Alt+Shift
    /// on the PC, through a key remapper).
    pub launcher: String,
    /// Clipboard shortcut.
    pub clipboard: String,
    /// Glass darkness: 0 clear … 1 black gradient.
    pub glass: f64,
    /// Paste the chosen clipboard item into the app in front.
    pub paste: bool,
    /// Clipboard items kept (pinned ones are always kept).
    pub history: usize,
    /// Folders the file search looks in; "~" is the home folder.
    pub file_search: Vec<String>,
    /// Show the most used apps on the empty launcher (off unless turned on).
    pub recent_apps: bool,
    /// Rows the search shows at once; the clipboard panel is the same size (see `rows_shown`).
    pub results: u32,
    /// Apps whose copies the clipboard never records: bundle ids on the Mac, program names on the PC ("KeePass").
    pub ignore_apps: Vec<String>,
    /// Break reminders (the Stretchly Zen schedule).
    pub breaks: Breaks,
    /// The evening journal (the Vesper prompt).
    pub journal: Journal,
    /// Currency for money answers when none is named ("100 usd"); empty: the system's region decides.
    pub currency: String,
    /// Text expansion: typed triggers become their snippet in any app (off until turned on).
    pub expansion: bool,
    pub snippets: Vec<crate::snippets::Snippet>,
    pub quicklinks: Vec<crate::quicklinks::Quicklink>,
    /// Shortcuts for launcher commands by id: "left-half" = "Hyper+Left", "lock-screen" = "Ctrl+Opt+L".
    pub hotkeys: BTreeMap<String, String>,
}

impl Settings {
    pub fn defaults_for(platform: Platform) -> Self {
        let list = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (launcher, clipboard, file_search, ignore_apps) = match platform {
            // Public defaults on the Mac: Opt+Space as in Alfred and Raycast, Shift+Cmd+C as in Maccy.
            Platform::Mac => ("Opt+Space", "Shift+Cmd+C", list(&["~"]), list(&["com.apple.keychainaccess", "com.apple.Passwords"])),
            // The PC: Alt+Space as in PowerToys Run, Win+Shift+V beside Windows' own Win+V; Desktop, Documents and
            // Downloads are always searched.
            Platform::Windows => ("Alt+Space", "Win+Shift+V", Vec::new(), list(&["KeePass", "KeePassXC", "1Password", "Bitwarden"])),
        };
        Self {
            launcher: launcher.into(),
            clipboard: clipboard.into(),
            glass: 1.0,
            paste: true,
            history: 500,
            file_search,
            recent_apps: false,
            results: 9,
            ignore_apps,
            breaks: Breaks::default(),
            journal: Journal::defaults_for(platform),
            currency: String::new(),
            expansion: false,
            snippets: crate::snippets::defaults(),
            quicklinks: crate::quicklinks::defaults(),
            hotkeys: BTreeMap::new(),
        }
    }
}

/// Off until turned on in Settings. Then: 20 s mini breaks every 20 min that cannot be skipped, long breaks off,
/// weekends off, the timer resets after 5 min away (Stretchly's rhythm).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Breaks {
    pub enabled: bool,
    pub mini_every_min: u32,
    pub mini_seconds: u32,
    /// A mini break cannot be skipped (Stretchly "strict mode").
    pub mini_strict: bool,
    pub long_enabled: bool,
    /// A long break replaces every Nth mini break.
    pub long_every: u32,
    pub long_minutes: u32,
    /// Long breaks can be put off once, by this many minutes.
    pub long_postpone_min: u32,
    pub weekends_off: bool,
    /// Away this long (no input) counts as a break and restarts the timer.
    pub natural_reset_min: u32,
    /// Seconds of warning before a break.
    pub notice_seconds: u32,
    /// Minutes "Delay" in the warning moves the coming break.
    pub delay_min: u32,
}

impl Default for Breaks {
    fn default() -> Self {
        Self {
            enabled: false,
            mini_every_min: 20,
            mini_seconds: 20,
            mini_strict: true,
            long_enabled: false,
            long_every: 2,
            long_minutes: 5,
            long_postpone_min: 5,
            weekends_off: true,
            natural_reset_min: 5,
            notice_seconds: 10,
            delay_min: 30,
        }
    }
}

impl Breaks {
    /// The scheduler's rules, in seconds. A break interval, a break and a long break last at least a minute, a
    /// second and a minute.
    pub fn rules(&self) -> crate::breaks::Rules {
        crate::breaks::Rules {
            enabled: self.enabled,
            every: self.mini_every_min.max(1) as f64 * 60.0,
            mini_seconds: self.mini_seconds.max(1) as f64,
            mini_strict: self.mini_strict,
            long_enabled: self.long_enabled,
            long_every: self.long_every,
            long_seconds: self.long_minutes.max(1) as f64 * 60.0,
            long_postpone: self.long_postpone_min as f64 * 60.0,
            weekends_off: self.weekends_off,
            natural_reset: self.natural_reset_min as f64 * 60.0,
            notice: self.notice_seconds as f64,
            delay: self.delay_min.max(1) as f64 * 60.0,
        }
    }
}

/// The evening prompt (Vesper's rhythm): ask at 20:00, snooze 15 min, ask again every 3 min.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Journal {
    pub enabled: bool,
    /// "HH:MM", local time.
    pub at: String,
    pub snooze_min: u32,
    pub nag_min: u32,
    /// The Markdown file the day is written to; "~" is the home folder.
    pub file: String,
    /// The daily Yes/No check-ins, in the order shown.
    pub habits: Vec<Habit>,
}

/// One daily check-in. `key` is its line name in the journal file ("Exercise: Yes"), so Vesper's entries keep
/// counting; a new habit's key is its label, and renaming changes only the label.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Habit {
    pub label: String,
    pub key: String,
}

impl Journal {
    pub fn defaults_for(platform: Platform) -> Self {
        // Off until turned on in Settings; no habits until "+" adds them.
        let file = match platform {
            Platform::Mac => "~/Documents/Journal.md",
            Platform::Windows => "~\\Documents\\Journal.md",
        };
        Self { enabled: false, at: "20:00".into(), snooze_min: 15, nag_min: 3, file: file.into(), habits: Vec::new() }
    }

    /// When the prompt asks, and how long a snooze and a nag last.
    pub fn schedule(&self) -> crate::journal::Schedule {
        crate::journal::Schedule::new(&self.at, self.snooze_min, self.nag_min)
    }
}

impl Default for Journal {
    fn default() -> Self {
        Self::defaults_for(Platform::current())
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::defaults_for(Platform::current())
    }
}

/// The most rows a panel shows at once (and the row views each one keeps).
pub const RESULTS_MAX: usize = 15;

/// `results` as rows on screen: 4 to RESULTS_MAX.
pub fn rows_shown(results: u32) -> usize {
    (results as usize).clamp(4, RESULTS_MAX)
}

/// A byte-order mark (older Notepad, Windows PowerShell) is not part of the settings.
pub fn parse(text: &str) -> Option<Settings> {
    toml::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// The saved settings. A missing or unreadable file gives the defaults; an unreadable file is left as it is.
pub fn load(path: &Path) -> Settings {
    std::fs::read_to_string(path).ok().and_then(|t| parse(&t)).unwrap_or_default()
}

/// Save with a comment header on top, swapped in atomically.
pub fn save(path: &Path, settings: &Settings, header: &str) -> io::Result<()> {
    let body = toml::to_string_pretty(settings).map_err(io::Error::other)?;
    crate::write_atomic(path, format!("{header}{body}").as_bytes())
}

/// A `file_search` entry as a folder: "~", "~/Projects" and "~\Projects" start at the home folder.
pub fn expand(entry: &str, home: &Path) -> PathBuf {
    match entry.trim().strip_prefix('~') {
        Some(rest) => home.join(rest.trim_start_matches(['/', '\\'])),
        None => PathBuf::from(entry.trim()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_per_system_and_partial_files() {
        let win = Settings::defaults_for(Platform::Windows);
        assert_eq!((win.launcher.as_str(), win.clipboard.as_str()), ("Alt+Space", "Win+Shift+V"));
        assert_eq!(Settings::defaults_for(Platform::Mac).launcher, "Opt+Space");
        // Missing fields take the defaults; unknown fields are ignored.
        let s = parse("glass = 0.5\nrecent_apps = true\nfuture = 1\n").unwrap();
        assert_eq!((s.glass, s.recent_apps, s.history), (0.5, true, 500));
        assert!(parse("glass = \"dark\"").is_none());
        assert_eq!(parse("\u{feff}paste = false").map(|s| s.paste), Some(false));
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        assert_eq!(load(&path), Settings::default());
        let mut s = Settings::defaults_for(Platform::Windows);
        s.file_search = vec!["~\\Projects".into(), "D:\\Work".into()];
        save(&path, &s, "# Livae settings.\n\n").unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("# Livae settings."));
        assert_eq!(load(&path), s);
        std::fs::write(&path, "launcher = [").unwrap();
        assert_eq!(load(&path), Settings::default());
    }

    #[test]
    fn breaks_and_journal_tables() {
        // The settings' defaults are the scheduler's, switched off until chosen; zero lengths still give a minute,
        // a second, a minute.
        assert_eq!(Breaks::default().rules(), crate::breaks::Rules { enabled: false, ..crate::breaks::Rules::default() });
        let zero = Breaks { mini_every_min: 0, mini_seconds: 0, long_minutes: 0, ..Breaks::default() }.rules();
        assert_eq!((zero.every, zero.mini_seconds, zero.long_seconds), (60.0, 1.0, 60.0));
        assert_eq!(Settings::defaults_for(Platform::Windows).journal.file, "~\\Documents\\Journal.md");
        assert_eq!(Settings::defaults_for(Platform::Mac).journal.file, "~/Documents/Journal.md");
        assert!(!Journal::default().enabled && Journal::default().habits.is_empty());
        // A file from before these tables keeps their defaults; a partial table fills in the rest.
        let s = parse("glass = 0.5\n[breaks]\nmini_every_min = 90\n[journal]\nat = \"21:30\"\n").unwrap();
        assert_eq!((s.breaks.rules().every, s.breaks.mini_seconds, s.journal.schedule().at.to_string()), (5400.0, 20, "21:30:00".into()));
        assert_eq!(s.journal.habits, Journal::default().habits);
    }

    #[test]
    fn snippets_quicklinks_and_hotkeys_tables() {
        let s = parse("expansion = true\n[[snippets]]\ntrigger = \";sig\"\ntext = \"Best, M\"\n[hotkeys]\nleft-half = \"Hyper+Left\"\n").unwrap();
        assert!(s.expansion);
        assert_eq!(s.snippets, vec![crate::snippets::Snippet { trigger: ";sig".into(), text: "Best, M".into() }]);
        assert_eq!(s.hotkeys.get("left-half").map(String::as_str), Some("Hyper+Left"));
        // Untouched lists keep their defaults; an emptied one stays empty.
        assert_eq!(s.quicklinks, crate::quicklinks::defaults());
        assert!(parse("quicklinks = []").unwrap().quicklinks.is_empty());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        save(&path, &s, "").unwrap();
        assert_eq!(load(&path), s);
    }

    #[test]
    fn home_folder_entries() {
        let home = Path::new("/home/m");
        assert_eq!(expand("~", home), home.join(""));
        assert_eq!(expand("~/Projects", home), home.join("Projects"));
        assert_eq!(expand("~\\Projects", home), home.join("Projects"));
        assert_eq!(expand(" /data ", home), PathBuf::from("/data"));
    }
}
