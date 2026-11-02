//! App start: menu-bar item, the two global shortcuts (from Settings, retried until free), the click-outside
//! monitor, and live reaction to settings changes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};

use block2::RcBlock;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, AllocAnyThread, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSEvent, NSEventMask, NSImage, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar,
    NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{ns_string, MainThreadMarker, NSData, NSNotification, NSObject, NSObjectProtocol, NSSize, NSString, NSTimer};

use crate::config::{self, Settings};


static LAUNCHER_ID: AtomicU32 = AtomicU32::new(0);
static CLIPBOARD_ID: AtomicU32 = AtomicU32::new(0);

/// `--test`: a throwaway copy for checks; no shortcuts, no clipboard recording or saving, no login item.
pub fn test_mode() -> bool {
    std::env::args().any(|a| a == "--test")
}

/// `--test --offscreen`: windows open far outside every screen and never take the focus, so checks can read
/// them while the owner keeps working.
pub fn offscreen() -> bool {
    test_mode() && std::env::args().any(|a| a == "--offscreen")
}

/// "Hyper+Space", "Cmd+Shift+L", "Ctrl+Opt+Left" (Hyper = the four modifiers Caps Lock sends through Karabiner).
fn hotkey(text: &str) -> Option<HotKey> {
    use livae_core::shortcut::{self, Key};
    let s = shortcut::parse(text, shortcut::HYPER_MAC)?;
    let mut mods = Modifiers::empty();
    mods.set(Modifiers::CONTROL, s.mods.ctrl);
    mods.set(Modifiers::ALT, s.mods.alt);
    mods.set(Modifiers::SHIFT, s.mods.shift);
    mods.set(Modifiers::META, s.mods.meta);
    let code = match s.key {
        Key::Space => Code::Space,
        Key::Letter(c) => format!("Key{c}").parse().ok()?,
        Key::Digit(d) => format!("Digit{d}").parse().ok()?,
        Key::Left => Code::ArrowLeft,
        Key::Right => Code::ArrowRight,
        Key::Up => Code::ArrowUp,
        Key::Down => Code::ArrowDown,
        Key::Return => Code::Enter,
    };
    Some(HotKey::new(Some(mods), code))
}

struct Shortcuts {
    manager: GlobalHotKeyManager,
    active: Vec<HotKey>,
    /// Shortcuts another app held when we asked; retried every few seconds instead of silently lost.
    pending: Vec<HotKey>,
    suspended: bool,
    status: Retained<NSStatusItem>,
    search: Retained<NSMenuItem>,
    clip: Retained<NSMenuItem>,
    taken: Retained<NSMenuItem>,
}

/// Show `shortcut` ("Hyper+V", "Opt+Space") at the right edge of a menu item, the way macOS menus do.
fn show_shortcut(item: &NSMenuItem, shortcut: &str) {
    use objc2_app_kit::NSEventModifierFlags as M;
    let mut mask = M::empty();
    let mut key = String::new();
    for part in shortcut.split('+').map(|p| p.trim().to_lowercase()) {
        match part.as_str() {
            "hyper" => mask |= M::Command | M::Control | M::Option | M::Shift,
            "cmd" | "command" => mask |= M::Command,
            "ctrl" | "control" => mask |= M::Control,
            "opt" | "option" | "alt" => mask |= M::Option,
            "shift" => mask |= M::Shift,
            "space" => key = " ".into(),
            k => key = k.to_string(),
        }
    }
    item.setKeyEquivalent(&NSString::from_str(&key));
    item.setKeyEquivalentModifierMask(mask);
}

thread_local! {
    /// The disabled first line of the Breaks submenu ("Next break in 12 min"), refreshed when the menu opens.
    static BREAKS_STATUS: RefCell<Option<Retained<NSMenuItem>>> = const { RefCell::new(None) };
    static JOURNAL_STATUS: RefCell<Option<Retained<NSMenuItem>>> = const { RefCell::new(None) };
    static SHORTCUTS: RefCell<Option<Shortcuts>> = const { RefCell::new(None) };
    static KEEP: RefCell<Vec<Retained<AnyObject>>> = const { RefCell::new(Vec::new()) };
    /// Hotkey id to the launcher command it runs (`[hotkeys]` in the settings).
    static COMMANDS: RefCell<HashMap<u32, &'static livae_core::commands::Def>> = RefCell::new(HashMap::new());
}

fn apply_shortcuts() {
    let s = config::get();
    // Anything unreadable falls back to the defaults: Opt+Space and Shift+Cmd+C.
    let launcher = hotkey(&s.launcher).unwrap_or(HotKey::new(Some(Modifiers::ALT), Code::Space));
    let clipboard = hotkey(&s.clipboard).unwrap_or(HotKey::new(Some(Modifiers::META | Modifiers::SHIFT), Code::KeyC));
    LAUNCHER_ID.store(launcher.id(), Ordering::SeqCst);
    CLIPBOARD_ID.store(clipboard.id(), Ordering::SeqCst);
    let platform = livae_core::settings::Platform::Mac;
    let commands: Vec<(HotKey, &'static livae_core::commands::Def)> = s
        .hotkeys
        .iter()
        .filter_map(|(id, text)| Some((hotkey(text)?, livae_core::commands::all(platform).find(|d| d.id.eq_ignore_ascii_case(id.trim()))?)))
        .collect();
    COMMANDS.with(|c| *c.borrow_mut() = commands.iter().map(|(hk, d)| (hk.id(), *d)).collect());
    SHORTCUTS.with(|cell| {
        if let Some(s) = cell.borrow_mut().as_mut() {
            for hk in s.active.drain(..) {
                let _ = s.manager.unregister(hk);
            }
            s.pending = [launcher, clipboard].into_iter().chain(commands.iter().map(|(hk, _)| *hk)).collect();
            s.suspended = false;
        }
    });
    claim_pending();
}

pub fn suspend_shortcuts() {
    SHORTCUTS.with(|cell| {
        if let Some(s) = cell.borrow_mut().as_mut() {
            for hk in s.active.drain(..) {
                let _ = s.manager.unregister(hk);
            }
            s.pending.clear();
            s.suspended = true;
        }
    });
}

pub fn resume_shortcuts() {
    apply_shortcuts();
}

fn claim_pending() {
    SHORTCUTS.with(|cell| {
        let mut slot = cell.borrow_mut();
        let Some(s) = slot.as_mut() else { return };
        if s.suspended {
            return;
        }
        let mut still = Vec::new();
        for hk in s.pending.drain(..) {
            if s.manager.register(hk).is_ok() {
                s.active.push(hk);
            } else {
                still.push(hk);
            }
        }
        s.pending = still;
        let settings = config::get();
        show_shortcut(&s.search, &settings.launcher);
        show_shortcut(&s.clip, &settings.clipboard);
        s.taken.setHidden(s.pending.is_empty());
        let paused = crate::clipboard::paused();
        // Livae's grid of days, today lit; all nine squares once today's journal is written; ! a shortcut is taken, ⏸ clipboard paused.
        let done = crate::journal::state() == crate::journal::JournalState::Done;
        let title = if !s.pending.is_empty() { "!" } else if paused { " ⏸\u{fe0e}" } else { "" };
        if let Some(button) = s.status.button(MainThreadMarker::new().unwrap()) {
            button.setImage(Some(&mark_image(done)));
            button.setTitle(&NSString::from_str(title));
            let tip = if s.pending.is_empty() { "Livae" } else { "Livae: another app holds one of your shortcuts; retrying" };
            button.setToolTip(Some(&NSString::from_str(tip)));
        }
    });
}

/// Called by `config::update` after the settings file is saved.
pub fn settings_changed(before: &Settings, after: &Settings) {
    if before.launcher != after.launcher || before.clipboard != after.clipboard || before.hotkeys != after.hotkeys {
        apply_shortcuts();
    }
    if before.expansion != after.expansion || before.snippets != after.snippets {
        crate::expand::apply();
    }
    if before.glass != after.glass {
        crate::launcher::set_glass(after.glass);
        crate::clipboard::set_glass(after.glass);
        crate::settings::set_glass(after.glass);
        crate::journal::set_glass(after.glass);
        crate::year::set_glass(after.glass);
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeAppDelegate"]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSMenuDelegate for AppDelegate {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &NSMenu) {
            BREAKS_STATUS.with(|b| {
                if let Some(item) = b.borrow().as_ref() {
                    item.setTitle(&NSString::from_str(&crate::breaks::status_line()));
                }
            });
            JOURNAL_STATUS.with(|j| {
                if let Some(item) = j.borrow().as_ref() {
                    item.setTitle(&NSString::from_str(&crate::journal::status_line()));
                }
            });
        }
    }

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            start(MainThreadMarker::new().unwrap(), self);
        }
    }

    impl AppDelegate {
        #[unsafe(method(openLauncher:))]
        fn open_launcher(&self, _s: Option<&AnyObject>) {
            crate::launcher::toggle();
        }

        #[unsafe(method(openClipboard:))]
        fn open_clipboard(&self, _s: Option<&AnyObject>) {
            crate::clipboard::toggle();
        }

        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _s: Option<&AnyObject>) {
            crate::settings::show();
        }

        #[unsafe(method(openYear:))]
        fn open_year(&self, _s: Option<&AnyObject>) {
            crate::year::show();
        }

        #[unsafe(method(openJournal:))]
        fn open_journal(&self, _s: Option<&AnyObject>) {
            crate::journal::show();
        }

        #[unsafe(method(breakNow:))]
        fn break_now(&self, _s: Option<&AnyObject>) {
            crate::breaks::start_now();
        }

        #[unsafe(method(breakSkip:))]
        fn break_skip(&self, _s: Option<&AnyObject>) {
            crate::breaks::skip_to_next_break();
        }

        #[unsafe(method(breakPause1h:))]
        fn break_pause_1h(&self, _s: Option<&AnyObject>) {
            crate::breaks::pause_for(60);
        }

        #[unsafe(method(breakPauseMorning:))]
        fn break_pause_morning(&self, _s: Option<&AnyObject>) {
            crate::breaks::pause_until_morning();
        }

        #[unsafe(method(breakResume:))]
        fn break_resume(&self, _s: Option<&AnyObject>) {
            crate::breaks::resume();
        }

        #[unsafe(method(togglePause:))]
        fn toggle_pause(&self, sender: Option<&AnyObject>) {
            let paused = crate::clipboard::toggle_paused();
            if let Some(item) = sender.and_then(|s| s.downcast_ref::<NSMenuItem>()) {
                item.setState(if paused { objc2_app_kit::NSControlStateValueOn } else { objc2_app_kit::NSControlStateValueOff });
            }
            claim_pending(); // refreshes the menu-bar symbol
        }

        #[unsafe(method(openWelcome:))]
        fn open_welcome(&self, _s: Option<&AnyObject>) {
            crate::welcome::show();
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

fn menu_item(mtm: MainThreadMarker, title: &str, action: objc2::runtime::Sel, key: &str, target: &AppDelegate) -> Retained<NSMenuItem> {
    let item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), Some(action), &NSString::from_str(key)) };
    unsafe { item.setTarget(Some(target)) };
    item
}

/// The menu-bar mark (a template image, so it follows the menu bar's light or dark look), made once per state.
fn mark_image(done: bool) -> Retained<NSImage> {
    thread_local! {
        static MARKS: RefCell<[Option<Retained<NSImage>>; 2]> = const { RefCell::new([None, None]) };
    }
    MARKS.with(|m| {
        m.borrow_mut()[done as usize]
            .get_or_insert_with(|| {
                let bytes: &[u8] = if done { include_bytes!("menubar-done.png") } else { include_bytes!("menubar.png") };
                let img = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(bytes)).expect("menu-bar mark");
                img.setSize(NSSize::new(18.0, 18.0));
                img.setTemplate(true);
                img
            })
            .clone()
    })
}

fn start(mtm: MainThreadMarker, delegate: &AppDelegate) {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let status = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    let menu = NSMenu::new(mtm);
    let search = menu_item(mtm, "Search", sel!(openLauncher:), "", delegate);
    let clip = menu_item(mtm, "Clipboard", sel!(openClipboard:), "", delegate);
    menu.addItem(&search);
    menu.addItem(&clip);
    let taken = menu_item(mtm, "A shortcut is in use by another app…", sel!(openSettings:), "", delegate);
    taken.setHidden(true);
    menu.addItem(&taken);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&menu_item(mtm, "Journal…", sel!(openJournal:), "", delegate));
    menu.addItem(&menu_item(mtm, "Year…", sel!(openYear:), "", delegate));
    let journal_status = NSMenuItem::new(mtm);
    journal_status.setEnabled(false);
    menu.addItem(&journal_status);
    JOURNAL_STATUS.with(|j| *j.borrow_mut() = Some(journal_status));
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&menu_item(mtm, "Pause Clipboard History", sel!(togglePause:), "", delegate));
    // Breaks: what is next, and the few things one does with it.
    let breaks_item = NSMenuItem::new(mtm);
    breaks_item.setTitle(ns_string!("Breaks"));
    let breaks_menu = NSMenu::new(mtm);
    let breaks_status = NSMenuItem::new(mtm);
    breaks_status.setEnabled(false);
    breaks_menu.addItem(&breaks_status);
    breaks_menu.addItem(&NSMenuItem::separatorItem(mtm));
    breaks_menu.addItem(&menu_item(mtm, "Take a Break Now", sel!(breakNow:), "", delegate));
    breaks_menu.addItem(&menu_item(mtm, "Skip to Next Break", sel!(breakSkip:), "", delegate));
    breaks_menu.addItem(&menu_item(mtm, "Pause for 1 Hour", sel!(breakPause1h:), "", delegate));
    breaks_menu.addItem(&menu_item(mtm, "Pause Until Tomorrow", sel!(breakPauseMorning:), "", delegate));
    breaks_menu.addItem(&menu_item(mtm, "Resume", sel!(breakResume:), "", delegate));
    breaks_item.setSubmenu(Some(&breaks_menu));
    menu.addItem(&breaks_item);
    BREAKS_STATUS.with(|b| *b.borrow_mut() = Some(breaks_status));
    menu.setDelegate(Some(ProtocolObject::from_ref(delegate)));
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&menu_item(mtm, "Settings…", sel!(openSettings:), ",", delegate));
    menu.addItem(&menu_item(mtm, "Welcome", sel!(openWelcome:), "", delegate));
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let quit = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("Quit Livae"), Some(sel!(terminate:)), ns_string!("q")) };
    menu.addItem(&quit);
    status.setMenu(Some(&menu));

    crate::launcher::install(mtm);
    crate::clipboard::install(mtm);
    crate::breaks::install(mtm);
    crate::journal::install(mtm);
    crate::journal::on_change(claim_pending);
    if !test_mode() {
        crate::gate::install_manifest();
    }

    // First run: open at login, as a menu-bar utility should.
    let marker = config::support_dir().join(".first-run-done");
    let first_run = !marker.exists() && !test_mode();
    if first_run {
        // Once, whether or not macOS accepts the login item (it can be turned on in Settings), so Welcome shows once.
        crate::access::set_open_at_login(true);
        let _ = std::fs::write(&marker, "");
    }

    // Global shortcuts. Events arrive on the main thread (Carbon, application event target).
    let manager = GlobalHotKeyManager::new().expect("hotkey manager");
    GlobalHotKeyEvent::set_event_handler(Some(|e: GlobalHotKeyEvent| {
        if e.state != HotKeyState::Pressed || MainThreadMarker::new().is_none() {
            return;
        }
        if e.id == LAUNCHER_ID.load(Ordering::SeqCst) {
            crate::launcher::toggle();
        } else if e.id == CLIPBOARD_ID.load(Ordering::SeqCst) {
            crate::clipboard::toggle();
        } else if let Some(def) = COMMANDS.with(|c| c.borrow().get(&e.id).copied()) {
            let front = objc2_app_kit::NSWorkspace::sharedWorkspace().frontmostApplication();
            crate::launcher::run_command(def, front.as_deref());
        }
    }));
    SHORTCUTS.with(|cell| {
        *cell.borrow_mut() = Some(Shortcuts { manager, active: Vec::new(), pending: Vec::new(), suspended: test_mode(), status, search, clip, taken })
    });
    if test_mode() {
        // A test copy is the same bundle, so while it runs, opening Livae.app only activates it (02.11.2026): it quits
        // itself after 15 minutes and can never stand in for the everyday copy.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(15 * 60));
            std::process::exit(0)
        });
    } else {
        apply_shortcuts();
    }
    livae_core::currency::load_from(&config::support_dir(), !test_mode());
    crate::expand::apply();
    let retry = RcBlock::new(|_t: NonNull<NSTimer>| {
        config::reload();
        // Accessibility granted since the last tick: text expansion can start.
        crate::expand::apply();
        claim_pending();
        crate::welcome::refresh();
        crate::settings::refresh();
    });
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(5.0, true, &retry) };

    // Close an open panel on any click in another app (not for an offscreen check copy: the owner's clicks are not for it).
    let click = RcBlock::new(|_e: NonNull<NSEvent>| {
        crate::launcher::with(|l| l.hide());
        crate::clipboard::with(|c| c.hide());
    });
    let monitor = if offscreen() { None } else { NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
        NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown,
        &click,
    ) };
    KEEP.with(|k| {
        let mut k = k.borrow_mut();
        k.push(Retained::into_super(Retained::into_super(timer)));
        if let Some(m) = monitor {
            k.push(m);
        }
    });

    // Test aids: `--show`, `--clipboard`, `--settings` open a panel; `--query text` fills the launcher field.
    let args: Vec<String> = std::env::args().collect();
    if first_run || args.iter().any(|a| a == "--welcome") {
        crate::welcome::show();
    }
    if args.iter().any(|a| a == "--show") {
        crate::launcher::toggle();
    }
    if args.iter().any(|a| a == "--clipboard") {
        crate::clipboard::show();
    }
    if args.iter().any(|a| a == "--settings") {
        crate::settings::show();
    }
    if args.iter().any(|a| a == "--year") {
        crate::year::show_test(&args);
    }
    if let Some(i) = args.iter().position(|a| a == "--journal") {
        crate::journal::show_test(args.get(i + 1).filter(|a| !a.starts_with("--")).map(std::path::PathBuf::from));
    }
    if let Some(kind) = args.iter().position(|a| a == "--break").and_then(|i| args.get(i + 1)) {
        crate::breaks::show_test_break(kind == "long");
    }
    if let Some(q) = args.iter().position(|a| a == "--query").and_then(|i| args.get(i + 1)) {
        let q = q.clone();
        crate::launcher::with(|l| l.set_query(&q));
    }
    // `--dump file`: the launcher's rows once its search settles, for checks over SSH.
    if let Some(path) = args.iter().position(|a| a == "--dump").and_then(|i| args.get(i + 1)).cloned() {
        let after = dispatch2::DispatchTime::NOW.time(1_500_000_000);
        let _ = dispatch2::DispatchQueue::main().after(after, move || {
            let _ = std::fs::write(&path, crate::launcher::dump());
        });
    }
    // `--run id,id`: run launcher commands in turn, the way their hotkeys would (a test copy only logs them).
    if let Some(ids) = args.iter().position(|a| a == "--run").and_then(|i| args.get(i + 1)) {
        let pid = args.iter().position(|a| a == "--pid").and_then(|i| args.get(i + 1)).and_then(|p| p.parse().ok());
        for id in ids.split(',') {
            let Some(def) = livae_core::commands::all(livae_core::settings::Platform::Mac).find(|d| d.id == id) else { continue };
            match def.command {
                livae_core::commands::Command::Window(w) => crate::winmgr::run(w, def.id, pid),
                _ => crate::launcher::run_command(def, None),
            }
        }
    }
}

pub fn run() {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    let delegate = AppDelegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
