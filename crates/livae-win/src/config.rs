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
    gate: Gate,
    gate_text: String,
}

thread_local! {
    static CURRENT: RefCell<Option<Current>> = const { RefCell::new(None) };
}

fn stamp() -> Option<SystemTime> {
    std::fs::metadata(settings_path()).and_then(|m| m.modified()).ok()
}

fn read() -> Current {
    let text = std::fs::read_to_string(settings_path()).unwrap_or_default();
    let (settings, gate, gate_text) = parse(&text).unwrap_or_default();
    Current { settings, gate, gate_text, stamp: stamp() }
}

/// First start: write the defaults so there is a file to edit.
pub fn ensure_file() {
    if !test_mode() && !settings_path().exists() {
        let _ = save(&Settings::default(), &Gate::default().toml());
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
        let gate_text = CURRENT.with(|c| c.borrow().as_ref().map(|c| c.gate_text.clone())).unwrap_or_default();
        let _ = save(&s, &gate_text);
    }
    CURRENT.with(|c| {
        let mut c = c.borrow_mut();
        let cur = c.get_or_insert_with(read);
        cur.settings = s;
        cur.stamp = stamp();
    });
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
        let (fresh, gate, gate_text) = match std::fs::read_to_string(settings_path()) {
            Ok(text) => parse(&text)?,
            Err(_) => (Settings::default(), Gate::default(), String::new()),
        };
        cur.gate = gate;
        cur.gate_text = gate_text;
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

/// Gate stays local until the shared settings own it; Cargo and core are outside this port's scope.
#[derive(Clone, Debug, PartialEq)]
pub struct GateSite {
    pub id: String,
    pub label: String,
    pub domains: Vec<String>,
    pub seconds: u32,
    pub step: u32,
}

#[derive(Clone, Debug, PartialEq)]
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

impl Gate {
    fn toml(&self) -> String {
        let mut text = format!("[gate]\npass_minutes = {}\n", self.pass_minutes);
        for s in &self.sites {
            // JSON basic strings are also TOML basic strings for these built-in values.
            let domains = s.domains.iter().map(|d| crate::gate::quote(d)).collect::<Vec<_>>().join(", ");
            text.push_str(&format!(
                "\n[[gate.sites]]\nid = {}\nlabel = {}\ndomains = [{domains}]\nseconds = {}\nstep = {}\n",
                crate::gate::quote(&s.id),
                crate::gate::quote(&s.label),
                s.seconds,
                s.step
            ));
        }
        text
    }
}

pub fn gate() -> Gate {
    CURRENT.with(|c| c.borrow_mut().get_or_insert_with(read).gate.clone())
}

/// Keep Gate intact when an existing journal control saves shared settings. Stage the shared serializer's
/// output before replacing the real file, so a failed write cannot discard Gate.
fn save(s: &Settings, gate: &str) -> std::io::Result<()> {
    let path = settings_path();
    let stage = path.with_extension("toml.livae-stage");
    let result = (|| {
        settings::save(&stage, s, HEADER)?;
        let mut text = std::fs::read_to_string(&stage)?;
        text.push('\n');
        text.push_str(gate);
        livae_core::write_atomic(&path, text.as_bytes())
    })();
    let _ = std::fs::remove_file(stage);
    result
}

/// Complete TOML statements, retaining multiline strings, arrays and comments. The core parser validates
/// syntax; this scanner only identifies the Gate tables and assignments for projection onto shared types.
fn statements(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let (mut start, mut i, mut depth) = (0, 0, 0i32);
    let (mut quote, mut triple, mut comment) = (0u8, false, false);
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if comment {
            if c == b'\n' {
                comment = false;
            }
        } else if quote != 0 {
            if quote == b'"' && c == b'\\' {
                i += 2;
                continue;
            }
            if c == quote {
                if triple {
                    if b.get(i..i + 3) == Some(&[quote; 3]) {
                        quote = 0;
                        i += 3;
                        continue;
                    }
                } else {
                    quote = 0;
                }
            }
        } else {
            match c {
                b'#' => comment = true,
                b'"' | b'\'' => {
                    quote = c;
                    triple = b.get(i..i + 3) == Some(&[c; 3]);
                    if triple {
                        i += 3;
                        continue;
                    }
                }
                b'[' | b'{' => depth += 1,
                b']' | b'}' => depth -= 1,
                _ => {}
            }
        }
        if c == b'\n' && quote == 0 && depth == 0 {
            out.push(&text[start..=i]);
            start = i + 1;
        }
        i += 1;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

fn parse(text: &str) -> Option<(Settings, Gate, String)> {
    let settings = settings::parse(text)?;
    let mut gate = Gate::default();
    let mut raw = String::new();
    let (mut table, mut site) = (String::new(), Vec::new());
    let mut sites: Vec<Vec<&str>> = Vec::new();
    let mut pass = None;
    let mut saw_site = false;
    for statement in statements(text.trim_start_matches('\u{feff}')) {
        let t = statement.trim();
        if t.starts_with('[') {
            let header = t.split('#').next()?.trim();
            table = header.chars().filter(|c| !c.is_whitespace() && *c != '"' && *c != '\'').collect();
            if table == "[[gate.sites]]" {
                if saw_site {
                    sites.push(std::mem::take(&mut site));
                }
                saw_site = true;
            }
        }
        if table == "[gate]" || table == "[[gate.sites]]" {
            raw.push_str(statement);
            if table == "[gate]" {
                if let Some((key, value)) = t.split_once('=')
                    && key.trim().trim_matches(['"', '\'']) == "pass_minutes" {
                        pass = Some(u32::try_from(settings::parse(&format!("history = {value}"))?.history).ok()?);
                    }
            } else if !t.starts_with('[') {
                site.push(statement);
            }
        }
    }
    if saw_site {
        sites.push(site);
    }
    if let Some(n) = pass {
        gate.pass_minutes = n;
    }
    if !sites.is_empty() {
        let mut parsed = Vec::new();
        for fields in sites {
            let mut projected = String::new();
            let mut step = None;
            let (mut id, mut label, mut domains, mut seconds) = (false, false, false, false);
            for field in fields {
                let Some((key, value)) = field.trim().split_once('=') else {
                    continue;
                };
                let key = key.trim().trim_matches(['"', '\'']);
                let to = match key {
                    "id" => {
                        id = true;
                        "launcher"
                    }
                    "label" => {
                        label = true;
                        "clipboard"
                    }
                    "domains" => {
                        domains = true;
                        "file_search"
                    }
                    "seconds" => {
                        seconds = true;
                        "history"
                    }
                    "step" => {
                        step = Some(u32::try_from(settings::parse(&format!("history = {value}"))?.history).ok()?);
                        continue;
                    }
                    _ => continue,
                };
                projected.push_str(&format!("{to} = {value}\n"));
            }
            if !(id && label && domains && seconds) {
                return None;
            }
            let s = settings::parse(&projected)?;
            parsed.push(GateSite { id: s.launcher, label: s.clipboard, domains: s.file_search, seconds: u32::try_from(s.history).ok()?, step: step? });
        }
        gate.sites = parsed;
    } else if raw.lines().any(|l| l.trim().starts_with("sites")) {
        // The Mac serializes an explicitly empty list as sites = [].
        let projected = raw.replace("[gate]", "").replace("sites", "file_search");
        if settings::parse(&projected)?.file_search.is_empty() {
            gate.sites.clear();
        }
    }
    Some((settings, gate, raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_gate_tables_preserve_custom_sites_and_multiline_values() {
        let text = "\u{feff}glass = 0.7\n[gate]\npass_minutes = 27\n\n[[gate.sites]]\nid = 'reading'\nlabel = '''Read\n[not.a.table]\nmore'''\ndomains = [\n 'example.com', # comment\n 'example.org',\n]\nseconds = 31\nstep = 7\n\n[journal]\nenabled = false\n";
        let (settings, gate, raw) = parse(text).unwrap();
        assert_eq!(settings.glass, 0.7);
        assert_eq!(gate.pass_minutes, 27);
        assert_eq!(gate.sites.len(), 1);
        assert_eq!(
            gate.sites[0],
            GateSite {
                id: "reading".into(),
                label: "Read\n[not.a.table]\nmore".into(),
                domains: vec!["example.com".into(), "example.org".into()],
                seconds: 31,
                step: 7,
            }
        );
        assert_eq!(parse(&raw).unwrap().1, gate);
        assert!(!raw.contains("[journal]"));
        assert!(parse("[gate]\npass_minutes = -1\n").is_none());
        assert!(parse("[gate]\npass_minutes = 4294967296\n").is_none());
        assert!(parse("[[gate.sites]]\nid = 'missing-fields'\n").is_none());
        assert!(parse("[[gate.sites]]\n").is_none());
        assert!(parse("[gate]\nsites = []\n").unwrap().1.sites.is_empty());
        let defaults = Gate::default();
        assert_eq!(parse(&defaults.toml()).unwrap().1, defaults);
    }
}
