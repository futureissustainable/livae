//! Command-line flags, the data folder (`%APPDATA%\Livae`, or `LIVAE_DATA` for a `--test` copy) and the settings
//! file, read again whenever it changes on disk so an edit applies within seconds, without a restart.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::SystemTime;

use livae_core::settings::{self, Settings};
use windows::Win32::UI::Shell::{FOLDERID_Profile, FOLDERID_RoamingAppData};

fn args() -> &'static [String] {
    static ARGS: OnceLock<Vec<String>> = OnceLock::new();
    ARGS.get_or_init(|| std::env::args().collect())
}

pub fn flag(name: &str) -> bool {
    args().iter().any(|a| a == name)
}

/// The word after `name` ("--query note" gives "note").
pub fn value(name: &str) -> Option<String> {
    let a = args();
    a.iter().position(|x| x == name).and_then(|i| a.get(i + 1)).cloned()
}

/// `--test`: a throwaway copy for checks; no shortcuts, no tray icon, no clipboard recording or saving, no
/// single-instance lock (it runs next to the real Livae).
pub fn test_mode() -> bool {
    flag("--test")
}

/// `--test --offscreen`: windows open far outside every monitor and never take the focus.
pub fn offscreen() -> bool {
    test_mode() && flag("--offscreen")
}

/// A `--test` copy's fixture folder (`LIVAE_DATA`), so checks never touch real history.
pub fn fixture() -> Option<PathBuf> {
    std::env::var_os("LIVAE_DATA").filter(|_| test_mode()).map(PathBuf::from)
}

pub fn home() -> PathBuf {
    std::env::var_os("USERPROFILE").map(PathBuf::from).or_else(|| crate::shell::known_folder(&FOLDERID_Profile)).unwrap_or_default()
}

pub fn data_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = fixture().unwrap_or_else(|| {
            let roaming = std::env::var_os("APPDATA").map(PathBuf::from).or_else(|| crate::shell::known_folder(&FOLDERID_RoamingAppData));
            roaming.unwrap_or_else(home).join("Livae")
        });
        if !test_mode() {
            let _ = std::fs::create_dir_all(dir.join("clips"));
        }
        dir
    })
    .clone()
}

pub fn settings_path() -> PathBuf {
    data_dir().join("settings.toml")
}

pub const HEADER: &str = "# Livae settings. Save the file and Livae applies it within a few seconds.\n\
# Shortcuts: Alt+Space, Win+Shift+V, Ctrl+Alt+K ... Hyper+Space works too (Hyper = Ctrl+Alt+Shift, e.g. Caps Lock remapped with AutoHotkey).\n\
# glass: 0 clear to 1 dark. paste: paste the chosen clipboard item into the app in front.\n\
# file_search: extra folders to search besides Desktop, Documents and Downloads (\"~\" is your user folder).\n\
# ignore_apps: programs whose copies are never recorded (the name without .exe).\n\
# [breaks]: break reminders, off until enabled (minutes, seconds; strict breaks cannot be skipped).\n\
# [journal]: the evening note; at = \"HH:MM\", file = the Markdown file it is written to (\"~\" is your user folder).\n\n";

struct Current {
    settings: Settings,
    stamp: Option<SystemTime>,
}

thread_local! {
    static CURRENT: RefCell<Option<Current>> = const { RefCell::new(None) };
}

fn stamp() -> Option<SystemTime> {
    std::fs::metadata(settings_path()).and_then(|m| m.modified()).ok()
}

fn read() -> Current {
    Current { settings: settings::load(&settings_path()), stamp: stamp() }
}

/// First start: write the defaults so there is a file to edit.
pub fn ensure_file() {
    if !test_mode() && !settings_path().exists() {
        let _ = settings::save(&settings_path(), &Settings::default(), HEADER);
    }
}

pub fn get() -> Settings {
    CURRENT.with(|c| c.borrow_mut().get_or_insert_with(read).settings.clone())
}

/// Change the settings and save them (a `--test` copy keeps the change in memory only).
pub fn update(f: impl FnOnce(&mut Settings)) {
    let mut s = get();
    f(&mut s);
    if !test_mode() {
        let _ = settings::save(&settings_path(), &s, HEADER);
    }
    CURRENT.with(|c| *c.borrow_mut() = Some(Current { settings: s, stamp: stamp() }));
}

/// Read the file again if it changed on disk. Returns (before, after) when the settings differ.
pub fn reload() -> Option<(Settings, Settings)> {
    let now = stamp();
    CURRENT.with(|c| {
        let mut c = c.borrow_mut();
        let cur = c.get_or_insert_with(read);
        if cur.stamp == now {
            return None;
        }
        cur.stamp = now;
        // A half-edited file that does not parse keeps the settings in use; the next good save applies.
        let fresh = match std::fs::read_to_string(settings_path()) {
            Ok(text) => settings::parse(&text)?,
            Err(_) => Settings::default(),
        };
        if fresh == cur.settings {
            return None;
        }
        Some((std::mem::replace(&mut cur.settings, fresh.clone()), fresh))
    })
}

/// Folders the file search walks: Desktop, Documents and Downloads plus the `file_search` entries, without
/// missing folders, repeats, or folders inside another one. A fixture copy uses only its settings' folders.
pub fn search_roots(s: &Settings) -> Vec<PathBuf> {
    let mut roots = if fixture().is_some() { Vec::new() } else { crate::shell::user_folders() };
    roots.extend(s.file_search.iter().map(|e| settings::expand(e, &home())));
    roots.retain(|p| p.is_dir());
    let key = |p: &PathBuf| p.to_string_lossy().to_lowercase().trim_end_matches(['\\', '/']).to_string();
    let keys: Vec<String> = roots.iter().map(key).collect();
    roots
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            let k = &keys[*i];
            !keys.iter().enumerate().any(|(j, o)| (j < *i && o == k) || (o != k && k.starts_with(&format!("{o}\\"))))
        })
        .map(|(_, p)| p.clone())
        .collect()
}
