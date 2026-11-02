//! The launcher, in the macOS 27 Spotlight shape: a glass search capsule and, below it, a separate glass card with
//! the results, grouped in one `NSGlassEffectContainerView`.
//!
//! Empty query: the scopes (Applications, Files, Settings). Typing: a quicklink search ("gh livae") first, answers
//! (maths, units, money, time zones), apps ranked by match and by how often you open them, commands (system and
//! window), quicklinks and snippets, then files from Spotlight's index (searched off the main thread). Mouse: hover
//! selects, click opens. Keys: ↑↓, Enter, ⌘1-⌘9, ⌘, (Settings), Esc. A click outside closes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::AnyClass;
use objc2::{sel, MainThreadOnly};
use livae_core::answers::Answer;
use livae_core::commands::{Command, Def};
use livae_core::quicklinks::{self, Hit};
use livae_core::rows::{self, Extra};
use livae_core::search::rank_words_scored;
use livae_core::settings::Platform;
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBox, NSEvent, NSFont, NSGlassEffectContainerView, NSImage, NSImageView, NSPasteboard, NSPasteboardTypeString,
    NSRunningApplication, NSTextAlignment, NSTextField, NSView, NSWorkspace,
};
use objc2_foundation::{ns_string, MainThreadMarker, NSPoint, NSRange, NSRect, NSSize, NSString, NSURL};

use crate::apps::{self, App, Usage};
use crate::config;
use crate::glass::Glass;
use crate::list::{List, Owner};
use crate::panel::{Controller, Event, Panel};
use crate::ui::{self, SECONDARY, TERTIARY, TEXT};

const WIDTH: f64 = 720.0;
/// The search capsule: 72 pt tall with a 28 pt query (the macOS 27 Spotlight look).
const FIELD_H: f64 = 72.0;
const QUERY: f64 = livae_core::tokens::text::H3.size;
/// The query starts on the result titles' column (the field draws its text 2 pt in), with or without a scope icon.
const FIELD_X: f64 = TEXT_X - 2.0;
/// Space between the capsule and the card.
const GAP: f64 = 10.0;
const ROW_H: f64 = 44.0;
const INSET: f64 = 8.0;
const EDGE: f64 = 16.0;
/// One text column: 16 edge + 24 icon + 12 gap (the field starts at 56 in its capsule, same optical column).
const TEXT_X: f64 = 56.0;
/// The most rows on screen (Settings › Results shown picks how many); the list scrolls through up to MAX_ITEMS.
const MAX_ROWS: usize = livae_core::settings::RESULTS_MAX;
const MAX_ITEMS: usize = 40;
const CARD_RADIUS: f64 = 24.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    All,
    Apps,
    Files,
}

#[derive(Clone)]
enum Item {
    Scope { title: &'static str, scope: Scope },
    Clipboard,
    App(usize),
    File(PathBuf),
    /// Maths, units, money or the time somewhere; Enter copies it.
    Answer(Answer),
    Quick(Hit),
    Command(&'static Def),
    /// An index into the settings' snippets; Enter types it into the app in front.
    Snippet(usize),
    Settings,
    /// Nothing local fits: search the web for the query.
    Web(String),
    /// macOS still hides Desktop, Documents and Downloads from file search: ask once.
    AllowFolders,
    Searching,
    Empty,
}

struct Row {
    icon: Retained<NSImageView>,
    title: Retained<NSTextField>,
    detail: Retained<NSTextField>,
    hint: Retained<NSTextField>,
}

pub struct Launcher {
    mtm: MainThreadMarker,
    panel: Retained<Panel>,
    capsule: Glass,
    card: Glass,
    field: Retained<NSTextField>,
    field_font: Retained<NSFont>,
    search_icon: Retained<NSImageView>,
    caret: crate::caret::Caret,
    /// What Enter does, written after the query: "– Open Safari".
    hint: Retained<NSTextField>,
    rows: Vec<Row>,
    /// The selected row's band; one view that slides between rows.
    band: Retained<NSBox>,
    apps: Vec<App>,
    names: Vec<String>,
    usage: Usage,
    icons: HashMap<String, Retained<NSImage>>,
    items: Vec<Item>,
    files: Vec<PathBuf>,
    /// Bumped on every edit; a file search whose ticket is stale is dropped.
    generation: Arc<AtomicU64>,
    /// The mdfind running for the newest search (its ticket), killed as soon as the text changes.
    running: Arc<std::sync::Mutex<Option<(u64, std::process::Child)>>>,
    /// True while a file search for the current text is running.
    searching: bool,
    scope: Scope,
    selected: usize,
    /// First item on screen (the list scrolls by moving this).
    first: usize,
    /// Recently opened apps (from macOS's own records), for day-one ranking.
    recent: Arc<std::sync::Mutex<HashMap<String, u32>>>,
    top: f64,
    card_h: f64,
    /// The app in front when the launcher opened: commands act on it.
    front: Option<Retained<NSRunningApplication>>,
    /// A command that asks for a second Enter, after the first.
    armed: Option<usize>,
    _controller: Retained<Controller>,
    _list: Retained<List>,
}

thread_local! {
    static LAUNCHER: RefCell<Option<Launcher>> = const { RefCell::new(None) };
}

/// Runs `f` on the launcher. Re-entrant calls (AppKit calling back while we hold it) are skipped.
pub fn with(f: impl FnOnce(&mut Launcher)) {
    LAUNCHER.with(|cell| {
        if let Ok(mut slot) = cell.try_borrow_mut()
            && let Some(l) = slot.as_mut() {
                f(l);
            }
    });
}

pub fn toggle() {
    with(|l| if crate::motion::shown(&l.panel) { l.hide() } else { l.show() });
}

pub fn set_glass(darkness: f64) {
    // Never so clear that white text loses its ground.
    let darkness = darkness.max(0.35);
    with(|l| {
        l.capsule.set_darkness(darkness);
        l.card.set_darkness(darkness);
    });
}

/// Run a launcher command or a hotkey's command on `front` (the app that was in front).
pub fn run_command(def: &Def, front: Option<&NSRunningApplication>) {
    match def.command {
        Command::System(s) => crate::system::run(s, def.id, front),
        Command::Window(w) => crate::winmgr::run(w, def.id, front.map(|a| a.processIdentifier())),
    }
}

/// `--dump`: each row on screen as "n<TAB>title<TAB>detail".
pub fn dump() -> String {
    let mut out = String::new();
    with(|l| {
        for (n, item) in l.items.iter().enumerate().take(config::get().rows()) {
            let (title, detail) = l.texts(n, item);
            out.push_str(&format!("{}\t{title}\t{detail}\n", n + 1));
        }
    });
    out
}

pub fn install(mtm: MainThreadMarker) {
    let controller = Controller::new(mtm, Owner::Launcher);
    let panel = Panel::new(mtm, Owner::Launcher, &controller);
    let s = config::get();

    let capsule = Glass::tinted(mtm, FIELD_H / 2.0, s.glass.max(0.35), crate::glass::Tint::Capsule);
    let (search_icon, field, field_font) = ui::search_field(mtm, &controller, "Search apps and files", WIDTH, QUERY, FIELD_H);
    capsule.content.addSubview(&search_icon);
    let hint = ui::label(mtm, QUERY, TERTIARY);
    hint.setFont(Some(&field_font));
    hint.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
    capsule.content.addSubview(&hint);
    capsule.content.addSubview(&field);

    let capsule_content = capsule.content.clone();
    let card = Glass::tinted(mtm, CARD_RADIUS, s.glass.max(0.35), crate::glass::Tint::Card);
    let list = List::new(mtm, Owner::Launcher);
    // No autoresizing: layout() sets the frame; a stretchy mask on top of that shrank it twice inside the glass.
    card.content.addSubview(&list);
    let band = ui::band(mtm, CARD_RADIUS - INSET);
    band.setHidden(true);
    list.addSubview(&band);
    let rows = (0..MAX_ROWS)
        .map(|_| {
            let icon = NSImageView::new(mtm);
            let title = ui::label(mtm, 16.0, TEXT);
            let detail = ui::label(mtm, 14.0, SECONDARY);
            let hint = ui::label(mtm, 14.0, SECONDARY);
            hint.setAlignment(NSTextAlignment::Right);
            for v in [&*icon as &NSView, &title, &detail, &hint] {
                list.addSubview(v);
                v.setHidden(true);
            }
            Row { icon, title, detail, hint }
        })
        .collect();

    // Both shapes in one glass container, so they render and move as one Liquid Glass surface.
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, FIELD_H));
    let inner = NSView::initWithFrame(NSView::alloc(mtm), rect);
    inner.addSubview(&capsule.root);
    inner.addSubview(&card.root);
    let root: Retained<NSView> = if AnyClass::get(c"NSGlassEffectContainerView").is_some() {
        let c = NSGlassEffectContainerView::new(mtm);
        c.setSpacing(GAP / 2.0);
        inner.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
        c.setContentView(Some(&inner));
        Retained::into_super(c)
    } else {
        inner
    };
    panel.setContentView(Some(&root));

    let apps = apps::scan();
    let names = apps.iter().map(|a| a.name.clone()).collect();
    let launcher = Launcher {
        mtm,
        panel,
        capsule,
        card,
        field,
        field_font,
        search_icon,
        caret: crate::caret::Caret::new(mtm, &capsule_content, QUERY + 6.0),
        hint,
        rows,
        band,
        apps,
        names,
        usage: Usage::load(),
        icons: HashMap::new(),
        items: Vec::new(),
        files: Vec::new(),
        generation: Arc::new(AtomicU64::new(0)),
        running: Arc::new(std::sync::Mutex::new(None)),
        searching: false,
        scope: Scope::All,
        selected: 0,
        first: 0,
        recent: Arc::new(std::sync::Mutex::new(HashMap::new())),
        top: 0.0,
        card_h: 0.0,
        front: None,
        armed: None,
        _controller: controller,
        _list: list,
    };
    LAUNCHER.with(|cell| *cell.borrow_mut() = Some(launcher));
    // Moving the caret with the arrow keys or the mouse changes no text: follow it here.
    let moved = block2::RcBlock::new(|_n: std::ptr::NonNull<objc2_foundation::NSNotification>| {
        with(|l| {
            if crate::motion::shown(&l.panel) {
                l.paint_field();
            }
        });
        crate::clipboard::with(|c| c.caret_moved());
    });
    let center = objc2_foundation::NSNotificationCenter::defaultCenter();
    let observer = unsafe { center.addObserverForName_object_queue_usingBlock(Some(objc2_app_kit::NSTextViewDidChangeSelectionNotification), None, None, &moved) };
    std::mem::forget(observer); // lives as long as the app
    // Holding ⌘ reveals the ⌘1-⌘9 shortcuts; letting go hides them again.
    let flags = block2::RcBlock::new(|event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
        let cmd = unsafe { event.as_ref() }.modifierFlags().contains(objc2_app_kit::NSEventModifierFlags::Command);
        if CMD.with(|c| c.replace(cmd)) != cmd {
            with(|l| {
                if l.panel.isVisible() {
                    l.paint_selection(false);
                }
            });
        }
        event.as_ptr()
    });
    if let Some(m) = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(objc2_app_kit::NSEventMask::FlagsChanged, &flags) } {
        std::mem::forget(m); // lives as long as the app
    }
}

/// Folders full of files nobody opens by hand (package managers, build output); never shown as file results.
const NOISE: [&str; 9] = ["node_modules", "site-packages", "__pycache__", "target", "vendor", "Pods", "DerivedData", "bower_components", "venv"];

thread_local! {
    static PENDING_QUERY: RefCell<String> = const { RefCell::new(String::new()) };
    /// ⌘ is held: rows show their ⌘1-⌘9 shortcuts.
    static CMD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Launcher {
    pub fn show(&mut self) {
        self.front = NSWorkspace::sharedWorkspace().frontmostApplication();
        CMD.with(|c| c.set(NSEvent::modifierFlags_class().contains(objc2_app_kit::NSEventModifierFlags::Command)));
        crate::clipboard::with(|c| c.hide());
        self.rescan_apps();
        self.field.setStringValue(ns_string!(""));
        self.set_scope(Scope::All);
        self.selected = 0;
        self.first = 0;
        let vf = ui::visible_frame_at(self.mtm, NSEvent::mouseLocation());
        self.top = if crate::app::offscreen() { ui::OFFSCREEN } else { vf.origin.y + vf.size.height * 0.78 };
        let x = vf.origin.x + (vf.size.width - WIDTH) / 2.0;
        self.panel.setFrameOrigin(NSPoint::new(x, self.top - FIELD_H));
        self.query_changed();
        ui::present(&self.panel);
        crate::motion::drop_in(&self.panel, &self.parts(), crate::motion::SEARCH);
        self.panel.makeFirstResponder(Some(&self.field));
        self.paint_field();
    }

    /// Apps installed or removed since the last open, found off the main thread so the panel opens at once.
    fn rescan_apps(&self) {
        let recent = self.recent.clone();
        std::thread::spawn(move || {
            let fresh = apps::scan();
            apps::recent_scores(fresh.iter().map(|a| (a.name.clone(), a.path.clone())).collect(), recent);
            dispatch2::DispatchQueue::main().exec_async(move || {
                with(|l| {
                    let names: Vec<String> = fresh.iter().map(|a| a.name.clone()).collect();
                    let changed = names != l.names;
                    if changed {
                        l.apps = fresh;
                        l.names = names;
                    }
                    // New apps, or fresh "last opened" dates for the most used row: redraw what is open.
                    let q = l.field.stringValue().to_string();
                    let empty_with_recents = q.trim().is_empty() && l.scope == Scope::All && config::get().recent_apps;
                    if l.panel.isVisible() && l.scope != Scope::Files && (changed || empty_with_recents) {
                        l.rebuild(&q);
                    }
                })
            });
        });
    }

    /// Put `q` in the field with the caret after it.
    pub fn set_query(&mut self, q: &str) {
        self.field.setStringValue(&NSString::from_str(q));
        if let Some(editor) = self.field.currentEditor() {
            editor.setSelectedRange(NSRange::new(q.encode_utf16().count(), 0));
        }
        self.query_changed();
    }

    pub fn hide(&mut self) {
        if crate::motion::shown(&self.panel) {
            self.generation.fetch_add(1, Ordering::SeqCst);
            crate::motion::drop_out(&self.panel, &self.parts());
        }
    }

    /// Keyboard, field and focus events from the shared panel.
    pub fn event(&mut self, event: Event) -> bool {
        match event {
            Event::TextChanged => {
                self.selected = 0;
                self.query_changed();
                true
            }
            Event::Command(c) if c == sel!(moveDown:) => {
                self.move_selection(1);
                true
            }
            Event::Command(c) if c == sel!(moveUp:) => {
                self.move_selection(-1);
                true
            }
            Event::Command(c) if c == sel!(insertNewline:) => {
                self.open_index(self.selected);
                true
            }
            Event::Command(c) if c == sel!(cancelOperation:) => {
                self.hide();
                true
            }
            // Backspace on an empty query leaves the scope (Files → everything).
            Event::Command(c) if c == sel!(deleteBackward:) => self.leave_scope_if_empty(),
            Event::Command(_) => false,
            Event::Key(",") => {
                self.hide();
                crate::settings::show();
                true
            }
            // ⌘↵ shows the selected file in Finder.
            Event::Key("\r") => {
                if let Some(Item::File(p)) = self.items.get(self.selected).cloned() {
                    let url = NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy()));
                    NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&objc2_foundation::NSArray::from_retained_slice(&[url]));
                    self.hide();
                }
                true
            }
            Event::Key(k) => match k.chars().next().and_then(|c| c.to_digit(10)) {
                Some(n) if n >= 1 && k.len() == 1 => {
                    self.open_index(self.first + n as usize - 1);
                    true
                }
                _ => false,
            },
            Event::Resigned => {
                self.hide();
                true
            }
        }
    }

    /// Right click on an app or a file: open, show in Finder, copy the path.
    pub fn menu_items(&mut self, y: f64) -> Vec<(&'static str, isize)> {
        let slot = ((self.card_h - INSET - y) / ROW_H).floor();
        if slot < 0.0 {
            return Vec::new();
        }
        let i = self.first + slot as usize;
        let items = match self.items.get(i) {
            Some(Item::App(_)) => vec![("Open", 11), ("Show in Finder", 12)],
            Some(Item::File(_)) => vec![("Open", 11), ("Show in Finder", 12), ("Copy Path", 13)],
            _ => return Vec::new(),
        };
        self.selected = i;
        self.paint_selection(false);
        items
    }

    pub fn menu_action(&mut self, tag: isize) {
        let path = match self.items.get(self.selected) {
            Some(Item::App(a)) => self.apps[*a].path.clone(),
            Some(Item::File(p)) => p.clone(),
            _ => return,
        };
        match tag {
            11 => self.open_index(self.selected),
            12 => {
                let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
                NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&objc2_foundation::NSArray::from_retained_slice(&[url]));
                self.hide();
            }
            13 => {
                let pb = objc2_app_kit::NSPasteboard::generalPasteboard();
                pb.clearContents();
                pb.setString_forType(&NSString::from_str(&path.to_string_lossy()), unsafe { objc2_app_kit::NSPasteboardTypeString });
                let f = self.panel.frame();
                crate::hud::show("Path copied", NSPoint::new(f.origin.x + WIDTH / 2.0, f.origin.y + f.size.height - FIELD_H / 2.0));
            }
            _ => {}
        }
    }

    /// Mouse over the list: hover selects, click opens.
    pub fn pointer(&mut self, y: f64, click: bool) {
        let slot = ((self.card_h - INSET - y) / ROW_H).floor();
        if slot < 0.0 {
            return;
        }
        let i = self.first + slot as usize;
        if i >= self.items.len() {
            return;
        }
        if click {
            self.open_index(i);
        } else if i != self.selected {
            self.selected = i;
            self.disarm();
            self.paint_selection(true);
        }
    }

    fn set_scope(&mut self, scope: Scope) {
        self.scope = scope;
        let (text, symbol) = match scope {
            Scope::All => ("Search apps and files", None),
            Scope::Apps => ("Search applications", Some("square.grid.2x2")),
            Scope::Files => ("Search files by name", Some("folder")),
        };
        self.field.setPlaceholderAttributedString(Some(&ui::placeholder(text, &self.field_font)));
        // No icon for the plain search, as in Spotlight; a scope shows its icon before the query.
        self.search_icon.setHidden(symbol.is_none());
        if let Some(img) = symbol.and_then(ui::symbol) {
            self.search_icon.setImage(Some(&img));
        }
        let f = self.field.frame();
        self.field.setFrame(NSRect::new(NSPoint::new(FIELD_X, f.origin.y), NSSize::new(WIDTH - FIELD_X - EDGE - 8.0, f.size.height)));
    }

    /// What moves when the search comes and goes: the capsule, the card, and each row on screen.
    fn parts(&self) -> crate::motion::Parts {
        use objc2::Message;
        let rows = self.rows.iter().filter(|r| !r.title.isHidden()).map(|r| {
            [&*r.icon as &NSView, &r.title, &r.detail, &r.hint].into_iter().map(|v| v.retain()).collect()
        });
        crate::motion::Parts { capsule: self.capsule.root.clone(), card: self.card.root.clone(), rows: rows.collect() }
    }

    /// What Enter does with the selected row, for the hint after the query.
    fn enter_hint(&self) -> Option<String> {
        let item = self.items.get(self.selected)?;
        let (title, _) = self.texts(self.selected, item);
        Some(match item {
            Item::App(_) | Item::File(_) => format!("Open {title}"),
            Item::Answer(_) => "Copy".into(),
            Item::Quick(_) | Item::Command(_) | Item::Web(_) => title,
            Item::Snippet(_) => "Type it".into(),
            Item::Settings | Item::Clipboard => format!("Open {title}"),
            Item::Scope { title, .. } => format!("Show {title}"),
            Item::AllowFolders | Item::Searching | Item::Empty => return None,
        })
    }

    /// The caret at the insertion point and, after the query, what Enter does.
    fn paint_field(&self) {
        self.caret.place(&self.field);
        let q = self.field.stringValue().to_string();
        let hint = (!q.trim().is_empty()).then(|| self.enter_hint()).flatten();
        let end = crate::caret::text_end(&self.field);
        match (hint, end) {
            (Some(h), Some(x)) if WIDTH - EDGE - (x + 10.0) >= 80.0 => {
                let f = self.field.frame();
                self.hint.setStringValue(&NSString::from_str(&format!("\u{2013} {h}")));
                self.hint.setFrame(NSRect::new(NSPoint::new(x + 10.0, f.origin.y), NSSize::new(WIDTH - EDGE - (x + 10.0), f.size.height)));
                self.hint.setHidden(false);
            }
            _ => self.hint.setHidden(true),
        }
    }

    fn leave_scope_if_empty(&mut self) -> bool {
        if self.scope != Scope::All && self.field.stringValue().length() == 0 {
            self.set_scope(Scope::All);
            self.query_changed();
            return true;
        }
        false
    }

    fn query_changed(&mut self) {
        let q = self.field.stringValue().to_string();
        let ticket = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.files.clear();
        self.searching = false;
        let wants_files = self.scope == Scope::Files || (self.scope == Scope::All && q.trim().chars().count() >= 2);
        if wants_files && !q.trim().is_empty() {
            self.searching = true;
            self.search_files(q.trim().to_string(), ticket);
        }
        self.rebuild(&q);
    }

    fn rebuild(&mut self, q: &str) {
        let mut items = Vec::new();
        let q = q.trim();
        if q.is_empty() {
            match self.scope {
                Scope::All => {
                    // The apps you open most come first when turned on in Settings; the scopes follow.
                    let recent = self.recent.lock().map(|r| r.clone()).unwrap_or_default();
                    let score = |i: usize| self.usage.boost(&self.apps[i].name) + recent.get(&self.apps[i].name).copied().unwrap_or(0);
                    let mut top: Vec<usize> = (0..self.apps.len()).filter(|&i| score(i) > 0).collect();
                    top.sort_by_key(|&i| std::cmp::Reverse(score(i)));
                    if config::get().recent_apps {
                        items.extend(top.into_iter().take(5).map(Item::App));
                    }
                    items.push(Item::Scope { title: "Applications", scope: Scope::Apps });
                    items.push(Item::Scope { title: "Files", scope: Scope::Files });
                    items.push(Item::Clipboard);
                    items.push(Item::Settings);
                }
                Scope::Apps => {
                    let mut by_use: Vec<usize> = (0..self.apps.len()).collect();
                    let recent = self.recent.lock().map(|r| r.clone()).unwrap_or_default();
                    by_use.sort_by_key(|&i| std::cmp::Reverse(self.usage.boost(&self.apps[i].name) + recent.get(&self.apps[i].name).copied().unwrap_or(0)));
                    items.extend(by_use.into_iter().take(MAX_ITEMS).map(Item::App));
                }
                Scope::Files => items.push(Item::Empty),
            }
        } else {
            let s = config::get();
            let keyword = (self.scope == Scope::All).then(|| quicklinks::keyword_hit(q, &s.quicklinks)).flatten();
            let keyword_index = keyword.as_ref().map(Hit::index);
            items.extend(keyword.map(Item::Quick));
            items.extend(livae_core::answers::answers(q, &s.currency()).into_iter().map(Item::Answer));
            if self.scope != Scope::Files {
                let usage = &self.usage;
                let apps = &self.apps;
                let recent = self.recent.lock().map(|r| r.clone()).unwrap_or_default();
                let take = if self.scope == Scope::Apps { MAX_ITEMS } else { 6 };
                let boost = |i: usize| usage.boost(&apps[i].name) + recent.get(&apps[i].name).copied().unwrap_or(0);
                let named: Vec<(&str, &str)> = apps.iter().map(|a| (a.name.as_str(), a.keywords)).collect();
                // Apps, commands, quicklinks and snippets in one order by how well each matches.
                let mut ranked: Vec<(u32, Item)> = rank_words_scored(q, &named, boost).into_iter().take(take).map(|(sc, i)| (sc, Item::App(i))).collect();
                if self.scope == Scope::All {
                    let extras = rows::extras(q, Platform::Mac, &s.quicklinks, &s.snippets, keyword_index, |name| usage.boost(name));
                    ranked.extend(extras.into_iter().map(|(sc, e)| {
                        (sc, match e {
                            Extra::Command(d) => Item::Command(d),
                            Extra::Quick(h) => Item::Quick(h),
                            Extra::Snippet(i) => Item::Snippet(i),
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
            let file_cap = if self.scope == Scope::Files { MAX_ITEMS } else { 6 };
            items.extend(self.files.iter().take(file_cap).cloned().map(Item::File));
            if self.searching && self.files.is_empty() && self.scope == Scope::Files {
                items.push(Item::Searching);
            }
            let maths = items.iter().any(|i| matches!(i, Item::Answer(_)));
            if self.scope == Scope::All && !self.searching && !maths {
                items.push(Item::Web(q.to_string()));
            }
            if self.scope != Scope::Apps && !maths && !crate::access::folders_asked() {
                items.push(Item::AllowFolders); // last, so Enter never starts system prompts by surprise
            }
            if items.is_empty() {
                items.push(if self.searching { Item::Searching } else { Item::Empty });
            }
        }
        items.truncate(MAX_ITEMS);
        self.first = 0;
        self.armed = None;
        self.items = items;
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
        self.layout();
    }

    /// Spotlight's index, off the main thread, after a short pause in typing; stale answers are dropped.
    fn search_files(&self, q: String, ticket: u64) {
        let counter = self.generation.clone();
        let roots = config::get().roots();
        let running = self.running.clone();
        if let Some((_, mut old)) = running.lock().ok().and_then(|mut r| r.take()) {
            let _ = old.kill();
            let _ = old.wait();
        }
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(120));
            if counter.load(Ordering::SeqCst) != ticket {
                return;
            }
            // Files you have opened in the last year first (Spotlight's own record), then any name match.
            let clean: String = q.chars().filter(|c| !matches!(c, '"' | '*' | '\\' | '\'')).collect();
            let kinds = "(kMDItemContentTypeTree == \"public.content\" || kMDItemContentTypeTree == \"public.folder\")";
            let used = format!("kMDItemFSName == \"*{clean}*\"cd && kMDItemLastUsedDate >= $time.today(-365) && {kinds}");
            let named = format!("kMDItemFSName == \"*{clean}*\"cd && {kinds}");
            let mut queries: Vec<(bool, Vec<std::ffi::OsString>)> = Vec::new();
            for root in &roots {
                queries.push((true, vec!["-onlyin".into(), root.into(), used.clone().into()]));
            }
            for root in &roots {
                queries.push((false, vec!["-onlyin".into(), root.into(), named.clone().into()]));
            }
            let mut found: Vec<(bool, PathBuf)> = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for (recent, args) in queries {
                let mut kept = 0;
                let Ok(mut child) = std::process::Command::new("/usr/bin/mdfind")
                    .args(&args)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                else {
                    continue;
                };
                let out = child.stdout.take();
                {
                    let Ok(mut slot) = running.lock() else { return };
                    if counter.load(Ordering::SeqCst) != ticket {
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }
                    // Killing a newer search's process ends its read; ours replaces it.
                    if let Some((_, mut old)) = slot.replace((ticket, child)) {
                        let _ = old.kill();
                        let _ = old.wait();
                    }
                }
                if let Some(out) = out {
                    for line in BufReader::new(out).lines().map_while(Result::ok) {
                        let hidden = line.split('/').any(|part| part.starts_with('.') || NOISE.contains(&part)) || line.contains("/Library/");
                        if !hidden && !line.ends_with(".app") && !line.contains(".app/") && seen.insert(line.clone()) {
                            found.push((recent, PathBuf::from(line)));
                            kept += 1;
                        }
                        if kept >= 60 || counter.load(Ordering::SeqCst) != ticket {
                            break;
                        }
                    }
                }
                if let Ok(mut slot) = running.lock()
                    && slot.as_ref().is_some_and(|(t, _)| *t == ticket)
                        && let Some((_, mut c)) = slot.take() {
                            let _ = c.kill();
                            let _ = c.wait();
                        }
                if counter.load(Ordering::SeqCst) != ticket {
                    return;
                }
                if std::env::var_os("LIVAE_DEBUG").is_some() {
                    eprintln!("mdfind recent={recent} kept={kept} args={args:?}");
                }
            }
            // Opened before, then names that start with the query, then shorter paths.
            let ql = q.to_lowercase();
            found.sort_by_key(|(recent, p)| {
                let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                (!recent, !name.starts_with(&ql), !name.contains(&ql), p.components().count())
            });
            let mut found: Vec<PathBuf> = found.into_iter().map(|(_, p)| p).collect();
            found.truncate(MAX_ITEMS / 2);
            dispatch2::DispatchQueue::main().exec_async(move || with(|l| l.files_arrived(ticket, found)));
        });
    }

    fn files_arrived(&mut self, ticket: u64, files: Vec<PathBuf>) {
        if self.generation.load(Ordering::SeqCst) != ticket || !self.panel.isVisible() {
            return;
        }
        self.searching = false;
        self.files = files;
        let q = self.field.stringValue().to_string();
        self.rebuild(&q);
    }

    /// Scroll the list by `by` rows (the wheel or a trackpad); the selection stays among the rows on screen.
    pub fn scroll(&mut self, by: isize) {
        let shown = config::get().rows();
        let Some(last) = self.items.len().checked_sub(1) else { return };
        let first = (self.first as isize + by).clamp(0, self.items.len().saturating_sub(shown) as isize) as usize;
        if first == self.first {
            return;
        }
        self.first = first;
        self.selected = self.selected.clamp(first, (first + shown - 1).min(last));
        self.layout();
    }

    fn move_selection(&mut self, by: isize) {
        if self.items.is_empty() {
            return;
        }
        let n = self.items.len() as isize;
        self.selected = ((self.selected as isize + by).rem_euclid(n)) as usize;
        self.disarm();
        if self.selected < self.first {
            self.first = self.selected;
            self.layout();
        } else if self.selected >= self.first + config::get().rows() {
            self.first = self.selected + 1 - config::get().rows();
            self.layout();
        } else {
            self.paint_selection(true);
        }
    }

    fn open_index(&mut self, i: usize) {
        let Some(item) = self.items.get(i).cloned() else { return };
        match item {
            Item::Scope { scope, .. } => {
                self.set_scope(scope);
                self.field.setStringValue(ns_string!(""));
                self.selected = 0;
                self.query_changed();
                self.panel.makeFirstResponder(Some(&self.field));
            }
            Item::AllowFolders => {
                let q = self.field.stringValue().to_string();
                PENDING_QUERY.with(|p| *p.borrow_mut() = q);
                self.hide();
                // Back to the same search once the prompts are answered, now with those folders.
                crate::access::request_folders(|| {
                    let q = PENDING_QUERY.with(|p| p.take());
                    with(|l| {
                        l.show();
                        l.set_query(&q);
                    });
                });
            }
            Item::Clipboard => {
                self.hide();
                crate::clipboard::show();
            }
            Item::App(i) => {
                let app = &self.apps[i];
                self.usage.bump(&app.name);
                let url = match &app.url {
                    Some(u) => NSURL::URLWithString(&NSString::from_str(u)),
                    None => Some(NSURL::fileURLWithPath(&NSString::from_str(&app.path.to_string_lossy()))),
                };
                if let Some(url) = url {
                    NSWorkspace::sharedWorkspace().openURL(&url);
                }
                self.hide();
            }
            Item::File(p) => {
                self.usage.bump(&p.to_string_lossy());
                NSWorkspace::sharedWorkspace().openURL(&NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy())));
                self.hide();
            }
            Item::Answer(a) => {
                let v = a.copy;
                let pb = NSPasteboard::generalPasteboard();
                pb.clearContents();
                pb.setString_forType(&NSString::from_str(&v), unsafe { NSPasteboardTypeString });
                let f = self.panel.frame();
                self.hide();
                crate::hud::show(&format!("Copied {v}"), NSPoint::new(f.origin.x + WIDTH / 2.0, f.origin.y + f.size.height - FIELD_H / 2.0));
            }
            Item::Quick(Hit::Open { url, .. }) => self.open_url(&url),
            Item::Quick(Hit::Fill { text, .. }) => self.set_query(&text),
            Item::Command(def) => {
                if def.command.confirm() && self.armed != Some(i) {
                    self.armed = Some(i);
                    self.layout();
                    return;
                }
                let front = self.front.take();
                self.usage.bump(def.name);
                self.hide();
                run_command(def, front.as_deref());
            }
            Item::Snippet(n) => {
                let Some(snippet) = config::get().snippets.get(n).cloned() else { return };
                self.hide();
                if crate::app::test_mode() {
                    crate::system::log_test(&format!("snippet {}", snippet.trigger));
                } else {
                    crate::expand::type_snippet(&snippet.text);
                }
            }
            Item::Web(q) => self.open_url(&format!("https://www.google.com/search?q={}", quicklinks::encode(&q))),
            Item::Settings => {
                self.hide();
                crate::settings::show();
            }
            Item::Searching | Item::Empty => {}
        }
    }

    fn open_url(&mut self, url: &str) {
        if let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) {
            NSWorkspace::sharedWorkspace().openURL(&url);
        }
        self.hide();
    }

    fn disarm(&mut self) {
        if self.armed.take().is_some() {
            self.layout();
        }
    }

    /// A row's words: (title, detail). `i` is its index in the list.
    fn texts(&self, i: usize, item: &Item) -> (String, String) {
        match item {
            Item::Scope { title, .. } => (title.to_string(), String::new()),
            Item::Clipboard => ("Clipboard".to_string(), String::new()),
            Item::App(a) => (self.apps[*a].name.clone(), self.apps[*a].detail.to_string()),
            Item::File(p) => (
                p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
                p.parent().map(|d| crate::ui::tilde(&d.to_string_lossy())).unwrap_or_default(),
            ),
            Item::Answer(a) => (a.title.clone(), a.detail.clone()),
            Item::Quick(Hit::Open { title, .. } | Hit::Fill { title, .. }) => (title.clone(), "Quicklink".to_string()),
            Item::Command(d) => (d.name.to_string(), if self.armed == Some(i) { "Press Enter again".to_string() } else { d.command.detail().to_string() }),
            Item::Snippet(n) => {
                let s = config::get();
                let Some(x) = s.snippets.get(*n) else { return (String::new(), String::new()) };
                // As it would be typed, with the copied text shown as its placeholder.
                let (text, _) = livae_core::snippets::render(&x.text, livae_core::journal::now(), || Some("{clipboard}".into()));
                let line = text.lines().next().unwrap_or_default();
                let short: String = line.chars().take(60).collect();
                (if short.len() < line.len() { format!("{short}…") } else { short }, x.trigger.clone())
            }
            Item::Web(_) => ("Search the web".to_string(), String::new()),
            Item::Settings => ("Settings".to_string(), String::new()),
            Item::AllowFolders => ("Include Desktop, Documents and Downloads".to_string(), String::new()),
            Item::Searching => ("Searching files…".to_string(), String::new()),
            Item::Empty => (
                if self.field.stringValue().length() == 0 {
                    "Type a file name".to_string()
                } else if self.scope == Scope::Files {
                    format!("No file names match \u{201c}{}\u{201d}", self.field.stringValue())
                } else {
                    "No matches".to_string()
                },
                if self.scope == Scope::All { String::new() } else { "⌫ back to everything".to_string() },
            ),
        }
    }

    fn icon_for(&mut self, path: &str) -> Retained<NSImage> {
        if let Some(img) = self.icons.get(path) {
            return img.clone();
        }
        let img = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(path));
        self.icons.insert(path.to_string(), img.clone());
        img
    }

    /// Move the band to the selected row (sliding when `animate`) and show the ⌘ numbers while ⌘ is held.
    fn paint_selection(&self, animate: bool) {
        let cmd = CMD.with(|c| c.get());
        let mut band = None;
        for (slot, row) in self.rows.iter().enumerate() {
            let i = self.first + slot;
            let item = self.items.get(i);
            let selectable = !matches!(item, Some(Item::Empty | Item::Searching) | None);
            if i == self.selected && selectable && slot < config::get().rows().min(self.items.len().saturating_sub(self.first)) {
                let y = self.card_h - INSET - (slot as f64 + 1.0) * ROW_H;
                band = Some(NSRect::new(NSPoint::new(INSET, y), NSSize::new(WIDTH - 2.0 * INSET, ROW_H)));
            }
            let hint = if !selectable {
                String::new()
            } else if cmd {
                format!("⌘{}", slot + 1)
            } else {
                String::new()
            };
            row.hint.setStringValue(&NSString::from_str(&hint));
        }
        match band {
            Some(r) => crate::motion::slide(&self.band, r, animate),
            None => self.band.setHidden(true),
        }
        self.paint_field();
    }

    /// Size the window to the capsule plus the card (top edge fixed) and place every view.
    fn layout(&mut self) {
        let n = self.items.len().saturating_sub(self.first).min(config::get().rows());
        let card_h = if n > 0 { INSET + n as f64 * ROW_H + INSET } else { 0.0 };
        self.card_h = card_h;
        let h = FIELD_H + if n > 0 { GAP + card_h } else { 0.0 };
        let mut frame = self.panel.frame();
        frame.size = NSSize::new(WIDTH, h);
        frame.origin.y = self.top - h;
        crate::motion::place(&self.panel, frame);
        self.capsule.root.setFrame(NSRect::new(NSPoint::new(0.0, h - FIELD_H), NSSize::new(WIDTH, FIELD_H)));
        self.card.root.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, card_h)));
        self.card.root.setHidden(n == 0);
        self._list.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, card_h)));

        let _home = config::home().to_string_lossy().to_string();
        let tint = ui::rgb(SECONDARY);
        let mut file_rows: Vec<(usize, f64, f64)> = Vec::new(); // (slot, name width, y)
        for slot in 0..self.rows.len() {
            let shown = slot < n;
            {
                let row = &self.rows[slot];
                for v in [&*row.icon as &NSView, &row.title, &row.detail, &row.hint] {
                    v.setHidden(!shown);
                }
            }
            if !shown {
                continue;
            }
            let y = card_h - INSET - (slot as f64 + 1.0) * ROW_H;
            let index = self.first + slot;
            let item = self.items[index].clone();
            let (title, detail) = self.texts(index, &item);
            let symbol = match &item {
                Item::Quick(_) => Some("link"),
                Item::Command(d) => Some(if matches!(d.command, Command::Window(_)) { "macwindow" } else { "power" }),
                Item::Snippet(_) => Some("text.quote"),
                _ => None,
            };
            let (image, template): (Option<Retained<NSImage>>, bool) = match &item {
                Item::App(i) => {
                    let p = self.apps[*i].path.to_string_lossy().to_string();
                    (Some(self.icon_for(&p)), false)
                }
                Item::File(p) => (Some(self.icon_for(&p.to_string_lossy())), false),
                _ => (symbol.and_then(ui::symbol), true),
            };
            let quiet = matches!(item, Item::Empty | Item::Searching);
            let row = &self.rows[slot];
            let (isz, ix, iy) = if template { (20.0, EDGE + 6.0, y + 12.0) } else { (26.0, EDGE + 3.0, y + 9.0) };
            row.icon.setFrame(NSRect::new(NSPoint::new(ix, iy), NSSize::new(isz, isz)));
            row.icon.setHidden(image.is_none());
            row.icon.setImage(image.as_deref());
            row.icon.setContentTintColor(if template { Some(&*tint) } else { None });
            row.title.setTextColor(Some(&ui::rgb(if quiet { TERTIARY } else { TEXT })));
            let answer = matches!(item, Item::Answer(_));
            let font = if answer {
                crate::font::sans(24.0, unsafe { objc2_app_kit::NSFontWeightSemibold })
            } else {
                crate::font::regular(16.0)
            };
            row.title.setFont(Some(&font));
            row.title.setStringValue(&NSString::from_str(&title));
            row.title.sizeToFit();
            // Files: names in one column, folders aligned in a second one.
            let is_file = matches!(item, Item::File(_));
            let cap = if is_file { 420.0 } else { WIDTH - TEXT_X - EDGE - 70.0 };
            let title_w = row.title.frame().size.width.min(cap);
            let (ty, th) = if answer { (y + 7.0, 30.0) } else { (y + 11.0, 22.0) };
            row.title.setFrame(NSRect::new(NSPoint::new(TEXT_X, ty), NSSize::new(title_w, th)));
            row.title.setLineBreakMode(if is_file { objc2_app_kit::NSLineBreakMode::ByTruncatingMiddle } else { objc2_app_kit::NSLineBreakMode::ByTruncatingTail });
            let detail_x = if is_file { TEXT_X + 262.0 } else { TEXT_X + title_w + 12.0 };
            row.detail.setLineBreakMode(if is_file { objc2_app_kit::NSLineBreakMode::ByTruncatingHead } else { objc2_app_kit::NSLineBreakMode::ByTruncatingTail });
            row.detail.setFrame(NSRect::new(NSPoint::new(detail_x, y + 13.0), NSSize::new((WIDTH - EDGE - 52.0 - detail_x).max(0.0), 18.0)));
            row.detail.setStringValue(&NSString::from_str(&detail));
            row.hint.setFrame(NSRect::new(NSPoint::new(WIDTH - EDGE - 44.0, y + 13.0), NSSize::new(44.0, 18.0)));
            if is_file {
                file_rows.push((slot, title_w, y));
            }
        }
        if let Some(col) = file_rows.iter().map(|r| r.1).reduce(f64::max) {
            let detail_x = TEXT_X + col + 16.0;
            for (slot, _, y) in file_rows {
                self.rows[slot].detail.setFrame(NSRect::new(NSPoint::new(detail_x, y + 13.0), NSSize::new((WIDTH - EDGE - 52.0 - detail_x).max(0.0), 18.0)));
            }
        }
        self.paint_selection(false);
    }
}
