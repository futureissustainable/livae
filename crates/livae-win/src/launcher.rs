//! The launcher, in the Mac v0.2 shape: a glass search capsule and, 10 px below, a glass card with the results.
//!
//! Empty query: the scopes (Applications, Files, Clipboard, Settings), after the most used apps when that setting
//! is on. Typing: answers for maths, units, money and time zones, apps (Start menu shortcuts and
//! Store apps) ranked by match and use, files from a name index of the user's folders (built off the UI thread,
//! refreshed every 10 minutes), quicklinks, commands, snippets, and "Search the web" last. Keys: Up/Down, Enter opens, Ctrl+Enter shows the file in
//! Explorer, Ctrl+1..9 opens a row (the numbers show while Ctrl is held), Esc closes. Mouse: hover selects, a
//! click opens. It hides when it loses the focus.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::fmt::Write as _;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use livae_core::commands;
use livae_core::field::Field;
use livae_core::quicklinks::{self, Hit};
use livae_core::rows::{self, Extra};
use livae_core::search::rank_words_scored;
use livae_core::settings::{Platform, Settings};
use livae_core::usage::Usage;
use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::FOLDERID_Recent;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::glass::{self, glyph, gfx, Font, Panel, Trim, BAND, BAND_RADIUS, SECONDARY, TERTIARY, TEXT};
use crate::{app, config, shell};

const WIDTH: f32 = 720.0;
const FIELD_H: f32 = 56.0;
const ROW_H: f32 = 46.0;
const INSET: f32 = 8.0;
const EDGE: f32 = 16.0;
/// One text column: 16 edge + 26 icon + 14 gap; the field text starts at the same x in its capsule.
const TEXT_X: f32 = 56.0;
const ICON: f32 = 26.0;
/// Rows on screen; the list scrolls through up to MAX_ITEMS.
const MAX_ROWS: usize = 9;
const MAX_ITEMS: usize = 40;
/// Files kept in the name index (the newest shallow ones first, as the walk goes level by level).
const MAX_INDEX: usize = 200_000;
/// Hide the panel if it is no longer the active window (posted on deactivation, handled after it settles).
const WM_CHECK_FOCUS: u32 = WM_APP + 30;

#[derive(Clone)]
pub struct App {
    pub name: String,
    /// What ShellExecute opens: the shortcut's path, or "shell:AppsFolder\<id>" for a Store app.
    pub launch: String,
    /// The shortcut file, for Ctrl+Enter.
    pub path: Option<PathBuf>,
    /// Settings pages and system tools: arguments for `launch`, the kind shown beside the name, hidden search words,
    /// and where the icon comes from.
    pub args: Option<&'static str>,
    pub detail: &'static str,
    pub keywords: &'static str,
    pub icon: Option<String>,
}

impl App {
    fn new(name: String, launch: String, path: Option<PathBuf>) -> App {
        App { name, launch, path, args: None, detail: "", keywords: "", icon: None }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    All,
    Apps,
    Files,
}

#[derive(Clone)]
enum Item {
    Scope(&'static str, Scope),
    Clipboard,
    App(usize),
    /// A file or folder (true for a folder).
    File(PathBuf, bool),
    Answer(livae_core::answers::Answer),
    Quicklink(Hit),
    Command(&'static commands::Def),
    Snippet(livae_core::snippets::Snippet),
    Settings,
    /// Nothing local fits: search the web for the query.
    Web(String),
    Indexing,
    Empty,
}

struct Entry {
    path: Box<str>,
    /// Lower-case file name.
    name: Box<str>,
    depth: u16,
    dir: bool,
    /// Opened lately (Windows' Recent items).
    recent: bool,
}

pub struct Index {
    entries: Vec<Entry>,
    built: Instant,
}

pub struct Launcher {
    panel: Panel,
    field: Field,
    scope: Scope,
    apps: Vec<App>,
    names: Vec<String>,
    usage: Usage,
    index: Option<Arc<Index>>,
    items: Vec<Item>,
    selected: usize,
    armed: Option<usize>,
    first: usize,
    /// Ctrl is held: rows show their Ctrl+1..9 numbers.
    ctrl: bool,
    caret: bool,
    /// The first half of a character outside the BMP (an emoji), waiting for its second WM_CHAR.
    high: Option<u16>,
    mouse: POINT,
    apps_ready: bool,
    apps_scanning: bool,
    apps_scanned: Option<Instant>,
}

thread_local! {
    static LAUNCHER: RefCell<Option<Launcher>> = const { RefCell::new(None) };
}

static INDEXING: AtomicBool = AtomicBool::new(false);

/// Runs `f` on the launcher. Re-entrant calls (Windows calling back while we hold it) are skipped.
fn with<R>(f: impl FnOnce(&mut Launcher) -> R) -> Option<R> {
    LAUNCHER.with(|cell| cell.try_borrow_mut().ok().and_then(|mut slot| slot.as_mut().map(f)))
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk.0 as i32) < 0 }
}

fn usage_file() -> PathBuf {
    config::data_dir().join("usage.tsv")
}

pub fn install() {
    // Rates come from disk; a money question fetches fresh ones (never in a test copy).
    livae_core::currency::load_from(&config::data_dir(), !config::test_mode());
    let mut apps = scan_start_menu();
    add_system(&mut apps);
    let names = apps.iter().map(|a| a.name.clone()).collect();
    let l = Launcher {
        panel: Panel::new(glass::LAUNCHER_FIELD, glass::LAUNCHER_CARD, WIDTH, FIELD_H),
        field: Field::default(),
        scope: Scope::All,
        apps,
        names,
        usage: Usage::load(&usage_file()),
        index: None,
        items: Vec::new(),
        selected: 0,
        armed: None,
        first: 0,
        ctrl: false,
        caret: true,
        high: None,
        mouse: POINT::default(),
        apps_ready: false,
        apps_scanning: false,
        apps_scanned: None,
    };
    LAUNCHER.with(|c| *c.borrow_mut() = Some(l));
    with(|l| l.rescan_apps());
    refresh_index();
}

pub fn show() {
    with(|l| l.show());
    crate::clipboard::hide();
}

pub fn hide() {
    with(|l| l.hide(false));
}

pub fn toggle() {
    let open = with(|l| l.panel.visible()).unwrap_or(false);
    if open { with(|l| l.hide(true)); } else { show(); }
}

pub fn set_query(q: &str) {
    with(|l| {
        l.field.set(q);
        l.query_changed();
    });
}

/// The app the launcher was opened over.
pub fn front() -> windows::Win32::Foundation::HWND {
    with(|l| l.panel.prev).unwrap_or_default()
}

/// The caret blink; false when the launcher is closed.
pub fn blink() -> bool {
    with(|l| {
        if !l.panel.visible() {
            return false;
        }
        l.caret = !l.caret;
        l.paint_field();
        true
    })
    .unwrap_or(false)
}

/// Apps and the file index are loaded (for `--dump`).
pub fn ready() -> bool {
    with(|l| l.apps_ready && l.index.is_some()).unwrap_or(false) && !INDEXING.load(Ordering::SeqCst)
}

pub fn settings_changed(before: &Settings, after: &Settings) {
    if before.file_search != after.file_search {
        refresh_index();
    }
    with(|l| {
        if l.panel.visible() {
            l.rebuild();
        }
    });
}

/// Build the file-name index again in the background (startup, every 10 minutes, new search folders).
pub fn refresh_index() {
    if INDEXING.swap(true, Ordering::SeqCst) {
        return;
    }
    let roots = config::search_roots(&config::get());
    let fixture = config::fixture().is_some();
    std::thread::spawn(move || {
        let index = Arc::new(build_index(&roots, fixture));
        app::on_main(move || {
            INDEXING.store(false, Ordering::SeqCst);
            with(|l| {
                l.index = Some(index);
                if l.panel.visible() && !l.field.is_empty() {
                    l.rebuild();
                }
            });
        });
    });
}

/// Every folder under the roots, level by level, without hidden, system, linked or noise folders.
fn build_index(roots: &[PathBuf], fixture: bool) -> Index {
    const HIDDEN_OR_SYSTEM: u32 = 0x2 | 0x4;
    let recent = if fixture { HashSet::new() } else { recent_names() };
    let mut entries = Vec::new();
    let mut queue: VecDeque<(PathBuf, u16)> = roots.iter().map(|r| (r.clone(), 0)).collect();
    while let Some((dir, depth)) = queue.pop_front() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if entries.len() >= MAX_INDEX {
                return Index { entries, built: Instant::now() };
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue; // junctions and links: no loops, no second copies
            }
            let attrs = e.metadata().map(|m| m.file_attributes()).unwrap_or(0);
            if attrs & HIDDEN_OR_SYSTEM != 0 {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            let is_dir = ft.is_dir();
            if (is_dir && livae_core::files::skip_dir(&name)) || name.starts_with("~$") {
                continue;
            }
            let lower = name.to_lowercase();
            let path = e.path();
            entries.push(Entry { path: path.to_string_lossy().into(), recent: recent.contains(&lower), name: lower.into(), depth, dir: is_dir });
            if is_dir && depth < 12 {
                queue.push_back((path, depth + 1));
            }
        }
    }
    Index { entries, built: Instant::now() }
}

/// Names of files and folders opened lately, from Windows' Recent items ("report.pdf.lnk").
fn recent_names() -> HashSet<String> {
    let Some(dir) = shell::known_folder(&FOLDERID_Recent) else { return HashSet::new() };
    let Ok(rd) = std::fs::read_dir(dir) else { return HashSet::new() };
    rd.flatten()
        .filter_map(|e| {
            let p = e.path();
            p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")).then(|| p.file_stem().map(|s| s.to_string_lossy().to_lowercase()))?
        })
        .collect()
}

/// Start menu shortcuts (all users and this user), one per name.
fn scan_start_menu() -> Vec<App> {
    let mut roots = Vec::new();
    for var in ["ProgramData", "APPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            roots.push(PathBuf::from(base).join(r"Microsoft\Windows\Start Menu\Programs"));
        }
    }
    let mut out: Vec<App> = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = roots;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if !matches!(ext.as_str(), "lnk" | "url" | "appref-ms") {
                continue;
            }
            let Some(name) = p.file_stem().map(|s| s.to_string_lossy().to_string()) else { continue };
            if is_noise_app(&name) || !seen.insert(name.to_lowercase()) {
                continue;
            }
            out.push(App::new(name, p.to_string_lossy().to_string(), Some(p)));
        }
    }
    out
}

fn is_noise_app(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.contains("uninstall") || lower.contains("readme") || lower.starts_with("website")
}

/// Start menu shortcuts plus everything else in "All apps" (Store apps such as Settings and Calculator).
fn scan_all() -> Vec<App> {
    let mut apps = scan_start_menu();
    let mut seen: HashSet<String> = apps.iter().map(|a| a.name.to_lowercase()).collect();
    for (name, id) in shell::apps_folder() {
        if is_noise_app(&name) || !seen.insert(name.to_lowercase()) {
            continue;
        }
        apps.push(App::new(name, format!("shell:AppsFolder\\{id}"), None));
    }
    add_system(&mut apps);
    apps
}

/// Settings pages, Control Panel applets and system tools (livae-core's table), after the apps so an app with the
/// same name wins; tools this edition of Windows lacks (gpedit.msc on Home) are left out.
fn add_system(apps: &mut Vec<App>) {
    const SETTINGS_APP: &str = "shell:AppsFolder\\windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel";
    const CONTROL_PANEL: &str = "shell:AppsFolder\\Microsoft.Windows.ControlPanel";
    let sys = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("System32");
    let mut seen: HashSet<String> = apps.iter().map(|a| a.name.to_lowercase()).collect();
    for it in livae_core::system_settings::WINDOWS {
        let program = !it.target.contains(':');
        if (program && !sys.join(it.target).exists()) || !seen.insert(it.name.to_lowercase()) {
            continue;
        }
        let icon = match (program, it.target) {
            (false, t) if t.starts_with("shell:") => t.to_string(),
            (false, _) => SETTINGS_APP.to_string(),
            (true, "control.exe") => CONTROL_PANEL.to_string(),
            (true, "rundll32.exe") => sys.join("sysdm.cpl").to_string_lossy().to_string(),
            (true, t) => sys.join(t).to_string_lossy().to_string(),
        };
        apps.push(App { args: it.args, detail: it.detail, keywords: it.keywords, icon: Some(icon), ..App::new(it.name.to_string(), it.target.to_string(), None) });
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
}

/// A folder under the user folder written from "~", as on the Mac.
pub fn home_text(p: &Path) -> String {
    let home = config::home().to_string_lossy().to_string();
    let s = p.to_string_lossy().to_string();
    match (s.get(..home.len()), s.get(home.len()..)) {
        (Some(head), Some(rest)) if !home.is_empty() && head.eq_ignore_ascii_case(&home) => format!("~{rest}"),
        _ => s,
    }
}

/// The icon cache key and shell source for a file: one icon per extension except for files that carry their own.
fn file_icon(p: &Path, dir: bool) -> (String, String) {
    let source = p.to_string_lossy().to_string();
    let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
    if dir {
        ("folder".into(), source)
    } else if ext.is_empty() || matches!(ext.as_str(), "exe" | "lnk" | "url" | "ico" | "appref-ms" | "msc" | "cpl" | "scr") {
        (format!("file:{source}"), source)
    } else {
        (format!("ext:{ext}"), source)
    }
}

impl Launcher {
    fn show(&mut self) {
        self.panel.remember_front(crate::clipboard::front());
        self.field.clear();
        self.scope = Scope::All;
        self.selected = 0;
        self.first = 0;
        // The live keyboard: this thread has not seen the keys pressed while another app was in front.
        self.ctrl = unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) < 0 && GetAsyncKeyState(VK_MENU.0 as i32) >= 0 };
        // The card appearing under a still pointer is not a hover.
        self.mouse = glass::cursor();
        if self.apps_scanned.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
            self.rescan_apps();
        }
        if self.index.as_ref().is_some_and(|i| i.built.elapsed() > Duration::from_secs(180)) {
            refresh_index();
        }
        let m = glass::monitor_at(glass::cursor());
        self.panel.scale = m.scale;
        if config::offscreen() {
            (self.panel.x, self.panel.top) = glass::offscreen_origin();
        } else {
            let w = self.panel.px(WIDTH);
            self.panel.x = m.work.left + (m.work.right - m.work.left - w) / 2;
            self.panel.top = m.work.top + ((m.work.bottom - m.work.top) as f32 * 0.22) as i32;
        }
        self.rebuild();
        self.panel.show(!config::offscreen());
        self.caret = true;
        app::blink_on();
        self.paint_all();
    }

    fn hide(&mut self, restore: bool) {
        if self.panel.visible() {
            self.panel.hide(restore);
        }
    }

    /// Apps installed or removed since the last open, found off the UI thread so the panel opens at once.
    fn rescan_apps(&mut self) {
        if self.apps_scanning {
            return;
        }
        self.apps_scanning = true;
        self.apps_scanned = Some(Instant::now());
        std::thread::spawn(|| {
            let fresh = scan_all();
            app::on_main(move || {
                with(|l| {
                    l.apps_scanning = false;
                    l.apps_ready = true;
                    let names: Vec<String> = fresh.iter().map(|a| a.name.clone()).collect();
                    if names != l.names {
                        l.apps = fresh;
                        l.names = names;
                        // Rows hold indices into the old list.
                        // A hidden --test copy (a --dump over SSH) still lists its rows.
                        if l.panel.visible() || config::test_mode() { l.rebuild() } else { l.items.clear() }
                    }
                });
            });
        });
    }

    fn query_changed(&mut self) {
        self.selected = 0;
        self.caret = true;
        self.rebuild();
        self.paint_field();
    }

    fn rebuild(&mut self) {
        self.armed = None;
        let q = self.field.text().trim().to_string();
        let s = config::get();
        let mut items = Vec::new();
        let usage = &self.usage;
        let apps = &self.apps;
        let used = |i: usize| usage.boost(&apps[i].name);
        if q.is_empty() {
            match self.scope {
                Scope::All => {
                    if s.recent_apps {
                        let mut top: Vec<usize> = (0..apps.len()).filter(|&i| usage.count(&apps[i].name) > 0).collect();
                        top.sort_by_key(|&i| std::cmp::Reverse(usage.count(&apps[i].name)));
                        items.extend(top.into_iter().take(5).map(Item::App));
                    }
                    items.push(Item::Scope("Applications", Scope::Apps));
                    items.push(Item::Scope("Files", Scope::Files));
                    items.push(Item::Clipboard);
                    items.push(Item::Settings);
                }
                Scope::Apps => {
                    let mut by_use: Vec<usize> = (0..apps.len()).collect();
                    by_use.sort_by_key(|&i| std::cmp::Reverse(used(i)));
                    items.extend(by_use.into_iter().take(MAX_ITEMS).map(Item::App));
                }
                Scope::Files => items.push(Item::Empty),
            }
        } else {
            let keyword = if self.scope == Scope::All { quicklinks::keyword_hit(&q, &s.quicklinks) } else { None };
            let keyword_index = keyword.as_ref().map(Hit::index);
            items.extend(keyword.map(Item::Quicklink));
            let currency = if s.currency.is_empty() { region_currency() } else { s.currency.clone() };
            items.extend(livae_core::answers::answers(&q, &currency).into_iter().map(Item::Answer));
            if self.scope != Scope::Files {
                let take = if self.scope == Scope::Apps { MAX_ITEMS } else { 6 };
                let named: Vec<(&str, &str)> = self.apps.iter().map(|a| (a.name.as_str(), a.keywords)).collect();
                // Apps, commands, quicklinks and snippets in one order by how well each matches.
                let mut ranked: Vec<(u32, Item)> = rank_words_scored(&q, &named, used).into_iter().take(take).map(|(sc, i)| (sc, Item::App(i))).collect();
                if self.scope == Scope::All {
                    let extras = rows::extras(&q, Platform::Windows, &s.quicklinks, &s.snippets, keyword_index, |name| usage.boost(name));
                    ranked.extend(extras.into_iter().map(|(sc, e)| {
                        (sc, match e {
                            Extra::Command(d) => Item::Command(d),
                            Extra::Quick(h) => Item::Quicklink(h),
                            Extra::Snippet(i) => Item::Snippet(s.snippets[i].clone()),
                        })
                    }));
                }
                ranked.sort_by_key(|(sc, _)| std::cmp::Reverse(*sc));
                items.extend(ranked.into_iter().map(|(_, item)| item));
                if self.scope == Scope::All {
                    let ql = q.to_lowercase();
                    if "settings".starts_with(&ql) || "preferences".starts_with(&ql) {
                        items.push(Item::Settings);
                    }
                    if "clipboard".starts_with(&ql) {
                        items.push(Item::Clipboard);
                    }
                }
            }
            let wants_files = self.scope == Scope::Files || (self.scope == Scope::All && q.chars().count() >= 2);
            if wants_files {
                match &self.index {
                    Some(ix) => {
                        let cap = if self.scope == Scope::Files { MAX_ITEMS } else { 6 };
                        let boost = |i: usize| {
                            let e = &ix.entries[i];
                            usage.boost(&e.path) + if e.recent { 500 } else { 0 }
                        };
                        let hits = livae_core::files::rank(&q, ix.entries.iter().map(|e| (&*e.name, e.depth)), boost, cap);
                        items.extend(hits.into_iter().map(|i| Item::File(PathBuf::from(&*ix.entries[i].path), ix.entries[i].dir)));
                    }
                    None if self.scope == Scope::Files => items.push(Item::Indexing),
                    None => {}
                }
            }
            let maths = items.iter().any(|i| matches!(i, Item::Answer(_)));
            if self.scope == Scope::All && !maths {
                items.push(Item::Web(q.clone()));
            }
            if items.is_empty() {
                items.push(Item::Empty);
            }
        }
        items.truncate(MAX_ITEMS);
        self.items = items;
        self.first = 0;
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
        self.layout();
    }

    fn rows(&self) -> usize {
        self.items.len().saturating_sub(self.first).min(MAX_ROWS)
    }

    fn layout(&mut self) {
        let n = self.rows();
        self.panel.set_card(if n > 0 { INSET + n as f32 * ROW_H + INSET } else { 0.0 });
        self.paint_card();
    }

    fn move_selection(&mut self, by: isize) {
        self.armed = None;
        if self.items.is_empty() {
            return;
        }
        let n = self.items.len() as isize;
        self.selected = (self.selected as isize + by).rem_euclid(n) as usize;
        if self.selected < self.first {
            self.first = self.selected;
        } else if self.selected >= self.first + MAX_ROWS {
            self.first = self.selected + 1 - MAX_ROWS;
        }
        self.layout();
    }

    fn scroll(&mut self, rows: isize) {
        self.armed = None;
        let max_first = self.items.len().saturating_sub(MAX_ROWS) as isize;
        self.first = (self.first as isize + rows).clamp(0, max_first) as usize;
        self.selected = self.selected.clamp(self.first, (self.first + MAX_ROWS - 1).min(self.items.len().saturating_sub(1)));
        self.layout();
    }

    fn set_scope(&mut self, scope: Scope) {
        self.scope = scope;
        self.field.clear();
        self.query_changed();
    }

    fn open(&mut self, i: usize) {
        let Some(item) = self.items.get(i).cloned() else { return };
        if self.selected != i { self.armed = None; self.selected = i; }
        match item {
            Item::Scope(_, scope) => self.set_scope(scope),
            Item::Clipboard => {
                // Open the clipboard first (it takes the focus from us), then let it close the launcher.
                let prev = self.panel.prev.0 as isize; // a window handle is just a number across the hop
                app::on_main(move || crate::clipboard::show_for(windows::Win32::Foundation::HWND(prev as _)));
            }
            Item::App(a) => {
                let app = self.apps[a].clone();
                self.usage.bump(&app.name);
                self.save_usage();
                // Opened while we are in front, so Windows lets the app take the focus.
                match app.args {
                    Some(args) => shell::open_args(&app.launch, args),
                    None => shell::open(&app.launch),
                };
                self.hide(false);
            }
            Item::File(p, _) => {
                self.usage.bump(&p.to_string_lossy());
                self.save_usage();
                shell::open(&p.to_string_lossy());
                self.hide(false);
            }
            Item::Answer(answer) => {
                let v = answer.copy;
                crate::clipboard::put_text(&v);
                let at = POINT { x: self.panel.x + self.panel.px(WIDTH / 2.0), y: self.panel.top + self.panel.px(FIELD_H / 2.0) };
                self.hide(true);
                glass::toast(&format!("Copied {v}"), at);
            }
            Item::Quicklink(Hit::Open { url, .. }) => {
                shell::open(&url);
                self.hide(false);
            }
            Item::Quicklink(Hit::Fill { text, .. }) => {
                self.field.set(&text);
                self.query_changed();
            }
            Item::Command(definition) => {
                if definition.command.confirm() && self.armed != Some(i) {
                    self.armed = Some(i);
                    self.paint_card();
                    return;
                }
                let front = self.panel.prev;
                self.armed = None;
                self.usage.bump(definition.name);
                self.save_usage();
                self.hide(true);
                app::run_command(definition.command, front);
            }
            Item::Snippet(snippet) => {
                let front = self.panel.prev;
                self.hide(true);
                crate::expand::insert(snippet, front);
            }
            Item::Web(q) => {
                shell::open(&format!("https://www.google.com/search?q={}", quicklinks::encode(&q)));
                self.hide(false);
            }
            Item::Settings => {
                self.hide(true);
                app::open_settings();
            }
            Item::Indexing | Item::Empty => {}
        }
    }

    /// Ctrl+Enter: show the selected file (or an app's shortcut) in Explorer.
    fn reveal(&mut self) {
        let path = match self.items.get(self.selected) {
            Some(Item::File(p, _)) => Some(p.clone()),
            Some(Item::App(a)) => self.apps[*a].path.clone(),
            _ => None,
        };
        if let Some(p) = path {
            shell::reveal(&p);
            self.hide(false);
        }
    }

    fn save_usage(&self) {
        if !config::test_mode() {
            let _ = self.usage.save(&usage_file());
        }
    }

    /// A row's text: (title, detail).
    fn texts(&self, item: &Item) -> (String, String) {
        match item {
            Item::Scope(t, _) => (t.to_string(), String::new()),
            Item::Clipboard => ("Clipboard".into(), String::new()),
            Item::App(i) => (self.apps[*i].name.clone(), self.apps[*i].detail.to_string()),
            Item::File(p, _) => {
                let name = p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| p.to_string_lossy().to_string());
                (name, p.parent().map(home_text).unwrap_or_default())
            }
            Item::Answer(answer) => (answer.title.clone(), answer.detail.clone()),
            Item::Quicklink(Hit::Open { title, .. } | Hit::Fill { title, .. }) => (title.clone(), "Quicklink".into()),
            Item::Command(definition) => (definition.name.into(), if self.armed == Some(self.selected)
                && matches!(self.items.get(self.selected), Some(Item::Command(d)) if d.command == definition.command) {
                    "Press Enter again".into()
                } else { definition.command.detail().into() }),
            Item::Snippet(snippet) => {
                // As it would be typed, with the copied text shown as its placeholder.
                let (text, _) = livae_core::snippets::render(&snippet.text, livae_core::journal::now(), || Some("{clipboard}".into()));
                let line = text.lines().next().unwrap_or_default();
                let short: String = line.chars().take(60).collect();
                (if short.len() < line.len() { format!("{short}…") } else { short }, snippet.trigger.clone())
            }
            Item::Settings => ("Livae Settings".into(), String::new()),
            Item::Web(_) => ("Search the web".into(), String::new()),
            Item::Indexing => ("Indexing files…".into(), String::new()),
            Item::Empty => {
                let title = if self.field.is_empty() {
                    "Type a file name".to_string()
                } else if self.scope == Scope::Files {
                    format!("No file names match \u{201c}{}\u{201d}", self.field.text().trim())
                } else {
                    "No matches".to_string()
                };
                (title, if self.scope == Scope::All { String::new() } else { "Backspace for everything".into() })
            }
        }
    }

    fn paint_all(&mut self) {
        self.paint_field();
        self.paint_card();
    }

    fn darkness(&self) -> f32 {
        // Never so clear that white text loses its ground.
        config::get().glass.max(0.35) as f32
    }

    fn paint_field(&mut self) {
        let (scale, dark, caret) = (self.panel.scale, self.darkness(), self.caret);
        let (icon, hint) = match self.scope {
            Scope::All => (glyph::SEARCH, "Search apps and files"),
            Scope::Apps => (glyph::APPS, "Search applications"),
            Scope::Files => (glyph::FOLDER, "Search files by name"),
        };
        let field = &self.field;
        let mut at = None;
        self.panel.capsule.draw(scale, |c| {
            c.glass(dark);
            c.glyph(icon, 20.0, EDGE + 2.0, 0.0, 28.0, c.h, SECONDARY);
            let fmt = gfx().format(Font::Sans, 24.0, DWRITE_FONT_WEIGHT_NORMAL, false, false);
            at = Some(c.field(field, &fmt, TEXT_X, WIDTH - TEXT_X - EDGE, hint, caret));
        });
        if let Some(at) = at {
            self.panel.set_caret(at);
        }
    }

    fn paint_card(&mut self) {
        let n = self.rows();
        if n == 0 {
            return;
        }
        let g = gfx();
        let body = g.format(Font::Sans, 16.0, DWRITE_FONT_WEIGHT_NORMAL, false, false);
        let answer = g.format(Font::Sans, 24.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, false, false);
        let small = g.format(Font::Sans, 14.0, DWRITE_FONT_WEIGHT_NORMAL, false, false);
        let hint_fmt = g.format(Font::Sans, 14.0, DWRITE_FONT_WEIGHT_NORMAL, false, true);
        struct Row {
            y: f32,
            title: String,
            detail: String,
            icon: Option<(String, String)>,
            answer: bool,
            quiet: bool,
            file: bool,
            on: bool,
            hint: String,
        }
        let mut rows = Vec::new();
        for slot in 0..n {
            let i = self.first + slot;
            let item = &self.items[i];
            let (title, detail) = self.texts(item);
            let selectable = !matches!(item, Item::Empty | Item::Indexing);
            let icon = match item {
                Item::App(a) => {
                    let app = &self.apps[*a];
                    let source = app.icon.clone().or_else(|| app.path.as_ref().map(|p| p.to_string_lossy().to_string())).unwrap_or_else(|| app.launch.clone());
                    Some((format!("app:{}", app.launch), source))
                }
                Item::File(p, dir) => Some(file_icon(p, *dir)),
                _ => None,
            };
            rows.push(Row {
                y: INSET + slot as f32 * ROW_H,
                title,
                detail,
                icon,
                answer: matches!(item, Item::Answer(_)),
                quiet: !selectable,
                file: matches!(item, Item::File(..)),
                on: i == self.selected && selectable,
                hint: if self.ctrl && selectable { format!("Ctrl+{}", slot + 1) } else { String::new() },
            });
        }
        // Files: names in one column, folders aligned in a second one.
        let hint_w = 64.0;
        let right = WIDTH - EDGE - hint_w;
        let name_col = rows.iter().filter(|r| r.file).map(|r| g.width(&g.fit(&r.title, &body, 420.0, Trim::Middle), &body)).fold(0.0f32, f32::max);
        let (scale, dark) = (self.panel.scale, self.darkness() * 0.8);
        self.panel.card.draw(scale, |c| {
            c.glass(dark);
            for r in &rows {
                if r.on {
                    c.round(INSET, r.y, WIDTH - 2.0 * INSET, ROW_H, BAND_RADIUS, glass::white(BAND));
                }
                if let Some((key, source)) = &r.icon {
                    c.picture(key, source, EDGE + 3.0, r.y + (ROW_H - ICON) / 2.0, ICON, ICON, 0.0);
                }
                let color = if r.quiet { TERTIARY } else { TEXT };
                if r.answer {
                    let tw = g.width(&r.title, &answer).min(right - TEXT_X);
                    c.text(&r.title, &answer, TEXT_X, r.y, right - TEXT_X, ROW_H, color);
                    if !r.detail.is_empty() {
                        let dx = TEXT_X + tw + 12.0;
                        c.text(&r.detail, &small, dx, r.y, (right - dx).max(1.0), ROW_H, SECONDARY);
                    }
                } else if r.file {
                    let title = g.fit(&r.title, &body, 420.0, Trim::Middle);
                    c.text(&title, &body, TEXT_X, r.y, 424.0, ROW_H, color);
                    let dx = TEXT_X + name_col + 16.0;
                    let folder = g.fit(&r.detail, &small, (right - dx).max(0.0), Trim::Head);
                    c.text(&folder, &small, dx, r.y, (right - dx).max(1.0), ROW_H, SECONDARY);
                } else {
                    let tw = g.width(&r.title, &body).min(right - TEXT_X);
                    c.text(&r.title, &body, TEXT_X, r.y, right - TEXT_X, ROW_H, color);
                    if !r.detail.is_empty() {
                        let dx = TEXT_X + tw + 12.0;
                        c.text(&r.detail, &small, dx, r.y, (right - dx).max(1.0), ROW_H, SECONDARY);
                    }
                }
                if !r.hint.is_empty() {
                    c.text(&r.hint, &hint_fmt, WIDTH - EDGE - hint_w, r.y, hint_w, ROW_H, SECONDARY);
                }
            }
        });
    }

    fn slot_at(&self, lparam: LPARAM) -> Option<usize> {
        let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / self.panel.scale;
        let slot = ((y - INSET) / ROW_H).floor();
        if slot < 0.0 {
            return None;
        }
        let i = self.first + slot as usize;
        (slot < self.rows() as f32 && i < self.items.len()).then_some(i)
    }

    fn key(&mut self, vk: u16) -> bool {
        let ctrl = key_down(VK_CONTROL);
        let alt = key_down(VK_MENU);
        let shift = key_down(VK_SHIFT);
        // Ctrl+Alt is AltGr on many keyboards: it types characters, so it is not a shortcut.
        let cmd = ctrl && !alt;
        let vk = VIRTUAL_KEY(vk);
        match vk {
            VK_ESCAPE => self.hide(true),
            VK_RETURN if cmd => self.reveal(),
            VK_RETURN => self.open(self.selected),
            VK_DOWN => self.move_selection(1),
            VK_UP => self.move_selection(-1),
            VK_NEXT => self.move_selection(MAX_ROWS as isize),
            VK_PRIOR => self.move_selection(-(MAX_ROWS as isize)),
            VK_LEFT => self.field.left(shift, cmd),
            VK_RIGHT => self.field.right(shift, cmd),
            VK_HOME => self.field.home(shift),
            VK_END => self.field.end(shift),
            VK_BACK if self.field.is_empty() && self.scope != Scope::All => self.set_scope(Scope::All),
            VK_BACK => {
                self.field.backspace(cmd);
                self.query_changed();
            }
            VK_DELETE => {
                self.field.delete(cmd);
                self.query_changed();
            }
            VK_CONTROL => {
                if self.ctrl != !alt {
                    self.ctrl = !alt;
                    self.paint_card();
                }
            }
            _ if cmd && vk.0 == b'A' as u16 => self.field.select_all(),
            _ if cmd && vk.0 == b'C' as u16 => {
                if let Some(t) = self.field.selected_text() {
                    crate::clipboard::put_text(t);
                }
            }
            _ if cmd && vk.0 == b'X' as u16 => {
                if let Some(t) = self.field.cut() {
                    crate::clipboard::put_text(&t);
                    self.query_changed();
                }
            }
            _ if cmd && vk.0 == b'V' as u16 => {
                if let Some(t) = crate::clipboard::get_text() {
                    self.field.insert(&t);
                    self.query_changed();
                }
            }
            _ if cmd && (b'1' as u16..=b'9' as u16).contains(&vk.0) => self.open(self.first + (vk.0 - b'1' as u16) as usize),
            _ => return false,
        }
        if self.panel.visible() {
            self.caret = true;
            self.paint_field();
        }
        true
    }

    fn handle(&mut self, part: isize, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let card = part == glass::LAUNCHER_CARD;
        match msg {
            WM_PAINT | shell::WM_PICTURE => {
                if card { self.paint_card() } else { self.paint_field() }
                Some(LRESULT(0))
            }
            WM_ACTIVATE if !card && (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
                unsafe {
                    let _ = PostMessageW(Some(self.panel.capsule.hwnd), WM_CHECK_FOCUS, WPARAM(0), LPARAM(0));
                }
                None
            }
            WM_CHECK_FOCUS => {
                if self.panel.visible() && !config::offscreen() && unsafe { GetForegroundWindow() } != self.panel.capsule.hwnd {
                    self.hide(false);
                }
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                self.hide(true);
                Some(LRESULT(0))
            }
            // Holding Enter is one confirmation, even when Windows repeats the key.
            WM_KEYDOWN if wparam.0 as u16 == VK_RETURN.0 && lparam.0 & (1 << 30) != 0 => Some(LRESULT(0)),
            WM_KEYDOWN => self.key(wparam.0 as u16).then_some(LRESULT(0)),
            WM_KEYUP if wparam.0 as u16 == VK_CONTROL.0 => {
                if self.ctrl {
                    self.ctrl = false;
                    self.paint_card();
                }
                Some(LRESULT(0))
            }
            WM_CHAR => {
                let u = wparam.0 as u16;
                let text = if (0xD800..0xDC00).contains(&u) {
                    self.high = Some(u);
                    None
                } else if (0xDC00..0xE000).contains(&u) {
                    self.high.take().and_then(|h| String::from_utf16(&[h, u]).ok())
                } else {
                    char::from_u32(u as u32).map(String::from)
                };
                if let Some(t) = text.filter(|t| !t.chars().any(char::is_control)) {
                    self.field.insert(&t);
                    self.query_changed();
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE if card => {
                // Only a real move selects: rows moving under a still pointer must not steal the selection.
                let at = glass::cursor();
                if (at.x, at.y) != (self.mouse.x, self.mouse.y) {
                    self.mouse = at;
                    if let Some(i) = self.slot_at(lparam)
                        && i != self.selected && !matches!(self.items[i], Item::Empty | Item::Indexing) {
                            self.armed = None;
                            self.selected = i;
                            self.paint_card();
                        }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP if card => {
                if let Some(i) = self.slot_at(lparam) {
                    self.open(i);
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as isize;
                self.scroll(-delta.signum() * 3);
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

/// Window messages for the launcher's capsule and card.
pub fn message(part: isize, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    with(|l| l.handle(part, msg, wparam, lparam)).flatten()
}

/// `--dump`: the panel's geometry and the text of every row on screen, one per line.
pub fn dump() -> String {
    with(|l| {
        let mut s = String::new();
        let (cap, card) = l.panel.geometry();
        let _ = writeln!(s, "panel launcher");
        let _ = writeln!(s, "query {:?}", l.field.text());
        let _ = writeln!(s, "scope {:?}", l.scope);
        let _ = writeln!(s, "scale {}", l.panel.scale);
        let _ = writeln!(s, "capsule {cap}");
        let _ = writeln!(s, "card {card}");
        let _ = writeln!(s, "apps {}", l.apps.len());
        let _ = writeln!(s, "indexed {}", l.index.as_ref().map_or(0, |i| i.entries.len()));
        let _ = writeln!(s, "selected {}", l.selected + 1);
        for slot in 0..l.rows() {
            let (t, d) = l.texts(&l.items[l.first + slot]);
            let _ = writeln!(s, "{}\t{}\t{}", slot + 1, t, d);
        }
        s
    })
    .unwrap_or_default()
}

/// ISO currency from the user's region, when the settings leave it empty.
fn region_currency() -> String {
    use windows::Win32::Globalization::{GetLocaleInfoEx, LOCALE_SINTLSYMBOL};
    // LOCALE_NAME_USER_DEFAULT is the null locale name in the Windows SDK.
    let mut text = [0u16; 16];
    let n = unsafe { GetLocaleInfoEx(windows::core::PCWSTR::null(), LOCALE_SINTLSYMBOL, Some(&mut text)) };
    let code = if n > 1 { String::from_utf16_lossy(&text[..n as usize - 1]).trim().to_string() } else { String::new() };
    if code.is_empty() { "EUR".into() } else { code }
}
