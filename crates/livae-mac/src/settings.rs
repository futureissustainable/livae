//! The Settings window: a sidebar of seven panes (General, Search, Clipboard, Breaks, Journal, Sound, Expansion),
//! each one column of rows: the label, its help or status under it, its controls on one right edge. Every pane
//! fits without scrolling. Every change is saved at once and applied live.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSApplication, NSButton, NSColor, NSControlStateValueOff,
    NSControlStateValueOn, NSEvent, NSEventMask, NSEventModifierFlags, NSLineBreakMode, NSOpenPanel, NSPopUpButton, NSSlider, NSSwitch,
    NSAccessibility, NSResponder, NSTextField, NSView, NSWindow, NSWindowTitleVisibility,
};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSWindowDelegate};
use objc2_foundation::{ns_string, MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use crate::config;
use crate::glass::Glass;
use livae_core::tokens;

const W: f64 = 720.0;
const H: f64 = 480.0;
const PAD: f64 = 24.0;
const ROW: f64 = 44.0;
/// A row with its help line under the label.
const ROW_HELP: f64 = 62.0;
/// Between groups of rows: more than inside a group, so space shows what belongs together.
const GROUP: f64 = 24.0;
/// Room for the window's traffic lights above the sidebar and the title.
const TITLEBAR: f64 = 44.0;
const SIDEBAR: f64 = 196.0;
/// The content column: from after the sidebar to the window's edge, less the padding.
const CX: f64 = SIDEBAR + PAD;
const CW: f64 = W - CX - PAD;
/// Every control ends on this edge; a second button sits one fixed button width further in. Every value control
/// (a shortcut, a choice, a slider) has one width.
const BUTTON_W: f64 = 104.0;
const VALUE_W: f64 = 220.0;
/// The panes: id, title, SF Symbol.
const PANES: [(&str, &str, &str); 7] = [
    ("general", "General", "gearshape"),
    ("search", "Search", "magnifyingglass"),
    ("clipboard", "Clipboard", "doc.on.clipboard"),
    ("breaks", "Breaks", "cup.and.saucer"),
    ("journal", "Journal", "book"),
    ("sound", "Sound", "speaker.wave.2"),
    ("expansion", "Text Expansion", "character.cursor.ibeam"),
];
const HISTORY: [usize; 4] = [100, 500, 1000, 5000];
const RESULTS: [u32; 7] = [5, 6, 7, 8, 9, 10, 12];
const BREAK_EVERY: [u32; 5] = [15, 20, 30, 45, 60];
const BREAK_SECONDS: [u32; 4] = [20, 30, 60, 120];
const JOURNAL_AT: [&str; 6] = ["18:00", "19:00", "20:00", "21:00", "22:00", "23:00"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Recording {
    Launcher,
    Clipboard,
}

struct Ui {
    window: Retained<NSWindow>,
    glass: Glass,
    launcher: Retained<NSButton>,
    clipboard: Retained<NSButton>,
    paste_status: Retained<NSTextField>,
    allow: Retained<NSButton>,
    folders: Retained<NSTextField>,
    home_only: Retained<NSButton>,
    ignored: Retained<NSTextField>,
    ignore_reset: Retained<NSButton>,
    history_count: Retained<NSTextField>,
    breaks_status: Retained<NSTextField>,
    journal_status: Retained<NSTextField>,
    private_allow: Retained<NSButton>,
    login: Retained<NSSwitch>,
    expansion: Retained<NSSwitch>,
    expansion_status: Retained<NSTextField>,
    outputs: Retained<NSPopUpButton>,
    inputs: Retained<NSPopUpButton>,
    eq: Retained<NSSwitch>,
    eq_status: Retained<NSTextField>,
    eq_allow: Retained<NSButton>,
    /// Each pane's view and its sidebar row; the selection fill behind the chosen one.
    panes: Vec<(Retained<NSView>, Retained<SidebarRow>)>,
    selection: Retained<NSView>,
    _target: Retained<Target>,
    _monitor: Option<Retained<AnyObject>>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static RECORDING: Cell<Option<Recording>> = const { Cell::new(None) };
    /// The pane on show, by index into `PANES`.
    static PANE: Cell<usize> = const { Cell::new(0) };
    /// The device names behind the Output and Microphone menus' items, by tag.
    static CHOICES: RefCell<(Vec<String>, Vec<String>)> = const { RefCell::new((Vec::new(), Vec::new())) };
}

define_class!(
    /// One pane in the sidebar: its symbol in a fixed column, its name on one left edge; a click shows the pane.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeSettingsSidebarRow"]
    #[ivars = Cell<usize>]
    struct SidebarRow;

    impl SidebarRow {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            show_pane(PANES[self.ivars().get()].0);
        }
    }
);

impl SidebarRow {
    fn new(mtm: MainThreadMarker, index: usize, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Cell::new(index));
        let row: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        let (_, title, symbol) = PANES[index];
        let icon = objc2_app_kit::NSImageView::new(mtm);
        icon.setImage(crate::ui::symbol(symbol).as_deref());
        icon.setContentTintColor(Some(&rgb(tokens::color::dark::TEXT)));
        icon.setFrame(NSRect::new(NSPoint::new(8.0, (frame.size.height - 20.0) / 2.0), NSSize::new(24.0, 20.0)));
        row.addSubview(&icon);
        let label = text(mtm, title, 16.0, tokens::color::dark::TEXT);
        label.setFrame(NSRect::new(NSPoint::new(40.0, (frame.size.height - 22.0) / 2.0), NSSize::new(frame.size.width - 48.0, 22.0)));
        row.addSubview(&label);
        row.setAccessibilityRole(Some(unsafe { objc2_app_kit::NSAccessibilityButtonRole }));
        row.setAccessibilityLabel(Some(&NSString::from_str(title)));
        row
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeSettingsTarget"]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl NSWindowDelegate for Target {
        /// Leaving the window ends a recording, so the shortcuts never stay switched off.
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _n: &NSNotification) {
            finish_recording(None);
        }

        /// Closing fades the window out, then hands the focus back to the app the user came from, so the next paste
        /// lands there.
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, sender: &NSWindow) -> bool {
            finish_recording(None);
            crate::motion::hide_then(sender, crate::motion::Kind::Bloom, || {
                if let Some(mtm) = MainThreadMarker::new() {
                    NSApplication::sharedApplication(mtm).hide(None);
                }
            });
            false
        }
    }

    impl Target {
        #[unsafe(method(recordLauncher:))]
        fn record_launcher(&self, _s: Option<&AnyObject>) {
            start_recording(Recording::Launcher);
        }

        #[unsafe(method(recordClipboard:))]
        fn record_clipboard(&self, _s: Option<&AnyObject>) {
            start_recording(Recording::Clipboard);
        }

        #[unsafe(method(glassChanged:))]
        fn glass_changed(&self, sender: Option<&AnyObject>) {
            if let Some(slider) = sender.and_then(|s| s.downcast_ref::<NSSlider>()) {
                let v = (slider.doubleValue() * 100.0).round() / 100.0;
                config::update(|s| s.glass = v);
            }
        }

        #[unsafe(method(pasteToggled:))]
        fn paste_toggled(&self, sender: Option<&AnyObject>) {
            if let Some(sw) = sender.and_then(|s| s.downcast_ref::<NSSwitch>()) {
                let on = sw.state() == NSControlStateValueOn;
                config::update(|s| s.paste = on);
                refresh();
            }
        }

        #[unsafe(method(allowAccessibility:))]
        fn allow_accessibility(&self, _s: Option<&AnyObject>) {
            crate::access::request();
        }

        #[unsafe(method(resultsChanged:))]
        fn results_changed(&self, sender: Option<&AnyObject>) {
            if let Some(p) = sender.and_then(|s| s.downcast_ref::<NSPopUpButton>()) {
                let i = p.indexOfSelectedItem().clamp(0, RESULTS.len() as isize - 1) as usize;
                config::update(|s| s.results = RESULTS[i]);
            }
        }

        #[unsafe(method(historyChanged:))]
        fn history_changed(&self, sender: Option<&AnyObject>) {
            if let Some(p) = sender.and_then(|s| s.downcast_ref::<NSPopUpButton>()) {
                let i = p.indexOfSelectedItem().clamp(0, HISTORY.len() as isize - 1) as usize;
                config::update(|s| s.history = HISTORY[i]);
            }
        }

        #[unsafe(method(breaksToggled:))]
        fn breaks_toggled(&self, sender: Option<&AnyObject>) {
            if let Some(sw) = sender.and_then(|s| s.downcast_ref::<NSSwitch>()) {
                let on = sw.state() == NSControlStateValueOn;
                config::update(|s| s.breaks.enabled = on);
                refresh();
            }
        }

        #[unsafe(method(breakEveryChanged:))]
        fn break_every_changed(&self, sender: Option<&AnyObject>) {
            if let Some(p) = sender.and_then(|s| s.downcast_ref::<NSPopUpButton>()) {
                let i = p.indexOfSelectedItem().clamp(0, BREAK_EVERY.len() as isize - 1) as usize;
                config::update(|s| s.breaks.mini_every_min = BREAK_EVERY[i]);
                refresh();
            }
        }

        #[unsafe(method(breakLengthChanged:))]
        fn break_length_changed(&self, sender: Option<&AnyObject>) {
            if let Some(p) = sender.and_then(|s| s.downcast_ref::<NSPopUpButton>()) {
                let i = p.indexOfSelectedItem().clamp(0, BREAK_SECONDS.len() as isize - 1) as usize;
                config::update(|s| s.breaks.mini_seconds = BREAK_SECONDS[i]);
            }
        }

        #[unsafe(method(weekendsToggled:))]
        fn weekends_toggled(&self, sender: Option<&AnyObject>) {
            if let Some(sw) = sender.and_then(|s| s.downcast_ref::<NSSwitch>()) {
                let on = sw.state() == NSControlStateValueOn;
                config::update(|s| s.breaks.weekends_off = on);
                refresh();
            }
        }

        #[unsafe(method(journalToggled:))]
        fn journal_toggled(&self, sender: Option<&AnyObject>) {
            if let Some(sw) = sender.and_then(|s| s.downcast_ref::<NSSwitch>()) {
                let on = sw.state() == NSControlStateValueOn;
                config::update(|s| s.journal.enabled = on);
                refresh();
            }
        }

        #[unsafe(method(journalAtChanged:))]
        fn journal_at_changed(&self, sender: Option<&AnyObject>) {
            if let Some(p) = sender.and_then(|s| s.downcast_ref::<NSPopUpButton>()) {
                let i = p.indexOfSelectedItem().clamp(0, JOURNAL_AT.len() as isize - 1) as usize;
                config::update(|s| s.journal.at = JOURNAL_AT[i].to_string());
                refresh();
            }
        }

        #[unsafe(method(clearHistory:))]
        fn clear_history(&self, _s: Option<&AnyObject>) {
            let mtm = MainThreadMarker::new().unwrap();
            let alert = NSAlert::new(mtm);
            alert.setAlertStyle(NSAlertStyle::Warning);
            alert.setMessageText(ns_string!("Clear unpinned items?"));
            alert.setInformativeText(ns_string!("Pinned items stay. This cannot be undone."));
            alert.addButtonWithTitle(ns_string!("Clear"));
            alert.addButtonWithTitle(ns_string!("Cancel"));
            if alert.runModal() == NSAlertFirstButtonReturn {
                crate::clipboard::clear_unpinned();
            }
        }

        #[unsafe(method(addIgnored:))]
        fn add_ignored(&self, _s: Option<&AnyObject>) {
            let mtm = MainThreadMarker::new().unwrap();
            let panel = NSOpenPanel::openPanel(mtm);
            panel.setCanChooseFiles(true);
            panel.setCanChooseDirectories(false);
            panel.setAllowsMultipleSelection(true);
            panel.setDirectoryURL(Some(&objc2_foundation::NSURL::fileURLWithPath(ns_string!("/Applications"))));
            if panel.runModal() == 1 {
                let ids: Vec<String> = panel
                    .URLs()
                    .iter()
                    .filter_map(|u| objc2_foundation::NSBundle::bundleWithURL(&u).and_then(|b| b.bundleIdentifier()))
                    .map(|b| b.to_string())
                    .collect();
                config::update(|s| {
                    for id in ids {
                        if !s.ignore_apps.contains(&id) {
                            s.ignore_apps.push(id);
                        }
                    }
                });
                refresh();
            }
        }

        #[unsafe(method(resetIgnored:))]
        fn reset_ignored(&self, _s: Option<&AnyObject>) {
            config::update(|s| s.ignore_apps = config::Settings::default().ignore_apps);
            refresh();
        }

        #[unsafe(method(addFolder:))]
        fn add_folder(&self, _s: Option<&AnyObject>) {
            let mtm = MainThreadMarker::new().unwrap();
            let panel = NSOpenPanel::openPanel(mtm);
            panel.setCanChooseDirectories(true);
            panel.setCanChooseFiles(false);
            if panel.runModal() == 1 {
                let picked: Vec<String> = panel.URLs().iter().filter_map(|u| u.path()).map(|p| crate::ui::tilde(&p.to_string())).collect();
                config::update(|s| {
                    for p in picked {
                        if !s.file_search.contains(&p) {
                            s.file_search.push(p);
                        }
                    }
                });
                refresh();
            }
        }

        #[unsafe(method(recentToggled:))]
        fn recent_toggled(&self, sender: Option<&AnyObject>) {
            if let Some(sw) = sender.and_then(|s| s.downcast_ref::<NSSwitch>()) {
                let on = sw.state() == NSControlStateValueOn;
                config::update(|s| s.recent_apps = on);
            }
        }

        #[unsafe(method(allowFolders:))]
        fn allow_folders(&self, _s: Option<&AnyObject>) {
            crate::access::allow_folders(refresh);
        }

        #[unsafe(method(resetFolders:))]
        fn reset_folders(&self, _s: Option<&AnyObject>) {
            config::update(|s| s.file_search = vec!["~".into()]);
            refresh();
        }

        #[unsafe(method(expansionToggled:))]
        fn expansion_toggled(&self, sender: Option<&AnyObject>) {
            if let Some(sw) = sender.and_then(|s| s.downcast_ref::<NSSwitch>()) {
                let on = sw.state() == NSControlStateValueOn;
                if on && !crate::access::trusted() {
                    crate::access::request();
                }
                config::update(|s| s.expansion = on);
                refresh();
            }
        }

        /// Snippets, quicklinks and command shortcuts are edited in the file; a saved edit applies within seconds.
        #[unsafe(method(editFile:))]
        fn edit_file(&self, _s: Option<&AnyObject>) {
            let url = objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(&config::file().to_string_lossy()));
            let ws = objc2_app_kit::NSWorkspace::sharedWorkspace();
            if let Some(editor) = ws.URLForApplicationWithBundleIdentifier(ns_string!("com.apple.TextEdit")) {
                ws.openURLs_withApplicationAtURL_configuration_completionHandler(
                    &objc2_foundation::NSArray::from_retained_slice(&[url]),
                    &editor,
                    &objc2_app_kit::NSWorkspaceOpenConfiguration::configuration(),
                    None,
                );
            } else {
                ws.openURL(&url);
            }
        }

        #[unsafe(method(outputPicked:))]
        fn output_picked(&self, sender: Option<&AnyObject>) {
            pick_device(sender, true);
        }

        #[unsafe(method(inputPicked:))]
        fn input_picked(&self, sender: Option<&AnyObject>) {
            pick_device(sender, false);
        }

        #[unsafe(method(eqToggled:))]
        fn eq_toggled(&self, sender: Option<&AnyObject>) {
            if let Some(sw) = sender.and_then(|s| s.downcast_ref::<NSSwitch>()) {
                let on = sw.state() == NSControlStateValueOn;
                config::update(|s| s.sound.eq = on);
            }
        }

        #[unsafe(method(editEq:))]
        fn edit_eq(&self, _s: Option<&AnyObject>) {
            crate::equalizer::show();
        }

        /// Screen & System Audio Recording, where macOS lets Livae hear what apps play.
        #[unsafe(method(allowAudio:))]
        fn allow_audio(&self, _s: Option<&AnyObject>) {
            if let Some(url) = objc2_foundation::NSURL::URLWithString(ns_string!("x-apple.systempreferences:com.apple.preference.security?Privacy_AudioCapture")) {
                objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&url);
            }
        }

        #[unsafe(method(openWelcome:))]
        fn open_welcome(&self, _s: Option<&AnyObject>) {
            crate::welcome::show();
        }

        #[unsafe(method(loginToggled:))]
        fn login_toggled(&self, sender: Option<&AnyObject>) {
            if let Some(sw) = sender.and_then(|s| s.downcast_ref::<NSSwitch>()) {
                let _ = crate::access::set_open_at_login(sw.state() == NSControlStateValueOn);
                refresh();
            }
        }
    }
);

fn rgb(c: [u8; 3]) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(c[0] as f64 / 255.0, c[1] as f64 / 255.0, c[2] as f64 / 255.0, 1.0)
}

fn text(mtm: MainThreadMarker, s: &str, size: f64, color: [u8; 3]) -> Retained<NSTextField> {
    let t = NSTextField::labelWithString(&NSString::from_str(s), mtm);
    t.setFont(Some(&crate::font::regular(size)));
    t.setTextColor(Some(&rgb(color)));
    t.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
    t
}

/// Shortcut text from a key event: "Hyper+V", "Cmd+Shift+L", … None when the key is not supported yet.
fn shortcut_from(event: &NSEvent) -> Option<String> {
    let f = event.modifierFlags();
    let (cmd, ctrl, opt, shift) = (
        f.contains(NSEventModifierFlags::Command),
        f.contains(NSEventModifierFlags::Control),
        f.contains(NSEventModifierFlags::Option),
        f.contains(NSEventModifierFlags::Shift),
    );
    if !(cmd || ctrl || opt) {
        return None; // a shortcut needs Cmd, Ctrl or Opt
    }
    let key = if event.keyCode() == 49 {
        "Space".to_string()
    } else {
        let c = event.charactersIgnoringModifiers()?.to_string().to_uppercase();
        if c.len() == 1 && c.chars().all(|c| c.is_ascii_alphanumeric()) { c } else { return None }
    };
    let mods = if cmd && ctrl && opt && shift {
        "Hyper".to_string()
    } else {
        [(cmd, "Cmd"), (ctrl, "Ctrl"), (opt, "Opt"), (shift, "Shift")].iter().filter(|(on, _)| *on).map(|(_, n)| *n).collect::<Vec<_>>().join("+")
    };
    Some(format!("{mods}+{key}"))
}

fn start_recording(which: Recording) {
    RECORDING.with(|r| r.set(Some(which)));
    crate::app::suspend_shortcuts();
    UI.with(|u| {
        if let Some(ui) = u.borrow().as_ref() {
            let b = if which == Recording::Launcher { &ui.launcher } else { &ui.clipboard };
            b.setTitle(ns_string!("Press new shortcut…"));
            b.setToolTip(Some(ns_string!("Esc cancels")));
        }
    });
}

fn finish_recording(shortcut: Option<String>) {
    let Some(which) = RECORDING.with(|r| r.take()) else { return };
    if let Some(s) = shortcut {
        let other = { let c = config::get(); if which == Recording::Launcher { c.clipboard } else { c.launcher } };
        if s.eq_ignore_ascii_case(&other) {
            let what = if which == Recording::Launcher { "the clipboard" } else { "search" };
            let mouse = NSEvent::mouseLocation();
            crate::hud::show(&format!("{} already opens {what}", crate::ui::shortcut_label(&s)), NSPoint::new(mouse.x, mouse.y + 40.0));
        } else {
            config::update(|c| if which == Recording::Launcher { c.launcher = s } else { c.clipboard = s });
        }
    }
    crate::app::resume_shortcuts();
    refresh();
}

/// Bring every control in line with the saved settings and the system state.
/// The equaliser's state or the devices changed: show it.
pub fn sound_changed() {
    refresh();
    crate::equalizer::reload();
}

/// An Output or Microphone menu item: add the device to the end of the order, or take it out; the last item
/// clears the order (macOS chooses again).
fn pick_device(sender: Option<&AnyObject>, output: bool) {
    let Some(item) = sender.and_then(|s| s.downcast_ref::<NSPopUpButton>()).and_then(|p| p.selectedItem()) else { return };
    let tag = item.tag();
    let name = CHOICES.with(|c| {
        let c = c.borrow();
        let list = if output { &c.0 } else { &c.1 };
        usize::try_from(tag).ok().and_then(|i| list.get(i).cloned())
    });
    config::update(|s| {
        let order = if output { &mut s.sound.outputs } else { &mut s.sound.inputs };
        match name {
            None => order.clear(),
            Some(n) => match order.iter().position(|o| *o == n) {
                Some(i) => {
                    order.remove(i);
                }
                None => order.push(n),
            },
        }
    });
}

/// Fill a pull-down with the devices: chosen ones first in their order (numbered), then the rest connected.
fn fill_choices(menu: &NSPopUpButton, order: &[String], connected: &[String]) -> Vec<String> {
    let mut names: Vec<String> = order.to_vec();
    names.extend(connected.iter().filter(|c| !order.contains(c)).cloned());
    menu.removeAllItems();
    let title = match order {
        [] => "macOS chooses".to_string(),
        [one] => one.clone(),
        [first, rest @ ..] => format!("{first}, then {}", rest.join(", then ")),
    };
    menu.addItemWithTitle(&NSString::from_str(&title));
    for (i, n) in names.iter().enumerate() {
        let shown = match (order.iter().position(|o| o == n), connected.contains(n)) {
            (Some(at), true) => format!("{}. {n}", at + 1),
            (Some(at), false) => format!("{}. {n} (not connected)", at + 1),
            (None, _) => n.clone(),
        };
        menu.addItemWithTitle(&NSString::from_str(&shown));
        if let Some(item) = menu.lastItem() {
            item.setTag(i as isize);
            item.setState(if order.contains(n) { NSControlStateValueOn } else { NSControlStateValueOff });
        }
    }
    if !order.is_empty() {
        if let Some(m) = menu.menu() {
            m.addItem(&objc2_app_kit::NSMenuItem::separatorItem(MainThreadMarker::new().unwrap()));
        }
        menu.addItemWithTitle(ns_string!("Let macOS Choose"));
        if let Some(item) = menu.lastItem() {
            item.setTag(-1);
        }
    }
    names
}

pub fn refresh() {
    let s = config::get();
    UI.with(|u| {
        let Some(ui) = u.borrow().as_ref().map(|ui| {
            (ui.launcher.clone(), ui.clipboard.clone(), ui.paste_status.clone(), ui.allow.clone(), ui.folders.clone(), ui.login.clone(), ui.private_allow.clone(), ui.home_only.clone(), ui.ignored.clone(), ui.breaks_status.clone(), ui.journal_status.clone(), ui.ignore_reset.clone(), ui.history_count.clone())
        }) else {
            return;
        };
        let (launcher, clipboard, paste_status, allow, folders, login, private_allow, home_only, ignored, breaks_status, journal_status, ignore_reset, history_count) = ui;
        ignore_reset.setHidden(s.ignore_apps == config::Settings::default().ignore_apps);
        let (items, pinned) = crate::clipboard::counts();
        history_count.setStringValue(&NSString::from_str(&format!("{items} items, {pinned} pinned")));
        breaks_status.setStringValue(&NSString::from_str(&crate::breaks::status_line()));
        // The time has its own row below: here, only whether today's note is done.
        let journal_line = if !s.journal.enabled {
            "Off".to_string()
        } else if crate::journal::written_today() {
            "Today's note is written".to_string()
        } else {
            "One short note each evening".to_string()
        };
        journal_status.setStringValue(&NSString::from_str(&journal_line));
        // App names for the ignored bundle ids; an id with no installed app shows as itself.
        let names: Vec<String> = s
            .ignore_apps
            .iter()
            .map(|id| {
                objc2_app_kit::NSWorkspace::sharedWorkspace()
                    .URLForApplicationWithBundleIdentifier(&NSString::from_str(id))
                    .and_then(|u| u.URLByDeletingPathExtension())
                    .and_then(|u| u.lastPathComponent())
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| id.clone())
            })
            .collect();
        ignored.setStringValue(&NSString::from_str(&if names.is_empty() { "Nothing".to_string() } else { names.join(", ") }));
        home_only.setHidden(s.file_search == ["~"]);
        let open = crate::access::folders_allowed();
        private_allow.setHidden(open);
        launcher.setTitle(&NSString::from_str(&crate::ui::shortcut_label(&s.launcher)));
        clipboard.setTitle(&NSString::from_str(&crate::ui::shortcut_label(&s.clipboard)));
        let trusted = crate::access::trusted();
        let status = if !s.paste {
            "Off: the item is copied, you press ⌘V"
        } else if trusted {
            "Choosing an item types it where you were writing"
        } else {
            "Needs Accessibility permission to paste"
        };
        paste_status.setStringValue(&NSString::from_str(status));
        allow.setHidden(trusted || !s.paste);
        let names: Vec<String> = s.file_search.iter().map(|f| if f == "~" { "Home folder".to_string() } else { f.clone() }).collect();
        let private = if open { ", with Desktop, Documents and Downloads" } else { ". Desktop, Documents and Downloads need your OK" };
        folders.setStringValue(&NSString::from_str(&format!("{}{private}", names.join(", "))));
        login.setState(if crate::access::open_at_login() { NSControlStateValueOn } else { NSControlStateValueOff });
        if let Some((sw, status)) = u.borrow().as_ref().map(|ui| (ui.expansion.clone(), ui.expansion_status.clone())) {
            sw.setState(if s.expansion { NSControlStateValueOn } else { NSControlStateValueOff });
            let first = s.snippets.first().map(|x| format!(", e.g. {}", x.trigger)).unwrap_or_default();
            let line = if !s.expansion {
                format!("Off: triggers like {} stay as typed", s.snippets.first().map(|x| x.trigger.as_str()).unwrap_or(":date"))
            } else if crate::access::trusted() {
                format!("On: {} snippets{first}", s.snippets.len())
            } else {
                "Needs Accessibility permission".to_string()
            };
            status.setStringValue(&NSString::from_str(&line));
        }
        if let Some((outputs, inputs, eq, eq_status, eq_allow)) = u.borrow().as_ref().map(|ui| (ui.outputs.clone(), ui.inputs.clone(), ui.eq.clone(), ui.eq_status.clone(), ui.eq_allow.clone())) {
            let devices = crate::sound::devices();
            let names = |out: bool| devices.iter().filter(|d| if out { d.output } else { d.input }).map(|d| d.name.clone()).collect::<Vec<_>>();
            let o = fill_choices(&outputs, &s.sound.outputs, &names(true));
            let i = fill_choices(&inputs, &s.sound.inputs, &names(false));
            CHOICES.with(|c| *c.borrow_mut() = (o, i));
            eq.setState(if s.sound.eq { NSControlStateValueOn } else { NSControlStateValueOff });
            let status = crate::sound::status();
            let line = if status == crate::sound::Status::Off { "A 10-band EQ, filters and a boost for each output".to_string() } else { status.text() };
            eq_status.setStringValue(&NSString::from_str(&line));
            eq_allow.setHidden(status != crate::sound::Status::NeedsPermission);
        }
    });
}

/// The window's panes by id, for `--design-dump`.
pub fn panes() -> Vec<&'static str> {
    PANES.iter().map(|p| p.0).collect()
}

/// Show one pane (by id) and mark it in the sidebar; builds the window if needed.
pub fn show_pane(pane: &str) {
    if UI.with(|u| u.borrow().is_none()) {
        build(MainThreadMarker::new().unwrap());
        refresh();
    }
    let chosen = PANES.iter().position(|p| p.0 == pane).unwrap_or(0);
    PANE.with(|p| p.set(chosen));
    UI.with(|u| {
        if let Some(ui) = u.borrow().as_ref() {
            for (i, (view, button)) in ui.panes.iter().enumerate() {
                view.setHidden(i != chosen);
                if i == chosen {
                    ui.selection.setFrame(button.frame());
                }
            }
        }
    });
}

pub fn window() -> Option<Retained<NSWindow>> {
    UI.with(|u| u.borrow().as_ref().map(|ui| ui.window.clone()))
}

pub fn set_glass(darkness: f64) {
    UI.with(|u| {
        if let Some(ui) = u.borrow().as_ref() {
            ui.glass.set_darkness(darkness.max(0.85));
        }
    });
}

pub fn show() {
    let mtm = MainThreadMarker::new().unwrap();
    let exists = UI.with(|u| u.borrow().is_some());
    if !exists {
        build(mtm);
    }
    refresh();
    UI.with(|u| {
        if let Some(ui) = u.borrow().as_ref() {
            ui.window.center();
            crate::ui::present(&ui.window);
            crate::motion::show(&ui.window, crate::motion::Kind::Bloom);
        }
    });
    if !crate::app::offscreen() {
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    }
}

/// Lays out one pane top-down: rows of a label, its controls on the right edge, and an optional help line under
/// the label at the column's full width.
struct Pane<'a> {
    mtm: MainThreadMarker,
    view: &'a NSView,
    top: f64,
}

impl Pane<'_> {
    fn place(&self, v: &NSView, x: f64, top: f64, w: f64, h: f64) {
        v.setFrame(NSRect::new(NSPoint::new(x, H - top - h), NSSize::new(w, h)));
        self.view.addSubview(v);
    }

    fn title(&mut self, title: &str) {
        let t = text(self.mtm, title, 20.0, tokens::color::dark::TEXT);
        t.setFont(Some(&crate::font::sans(20.0, unsafe { objc2_app_kit::NSFontWeightMedium })));
        self.place(&t, CX, self.top, CW, 28.0);
        self.top += 28.0 + 16.0;
    }

    /// A row: `controls` (view, width, height) from the right edge inwards, `help` under the label.
    fn row(&mut self, label: &str, controls: &[(&NSView, f64, f64)], help: Option<&NSTextField>) {
        let mut right = CX + CW;
        for (c, w, h) in controls {
            self.place(c, right - w, self.top + (ROW - h) / 2.0, *w, *h);
            right -= w + 8.0;
        }
        let l = text(self.mtm, label, 16.0, tokens::color::dark::TEXT);
        l.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        self.place(&l, CX, self.top + 11.0, right - CX - 8.0, 22.0);
        match help {
            Some(h) => {
                h.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
                self.place(h, CX, self.top + 38.0, CW, 18.0);
                self.top += ROW_HELP;
            }
            None => self.top += ROW,
        }
    }

    fn group(&mut self) {
        self.top += GROUP;
    }
}

fn build(mtm: MainThreadMarker) {
    let s = config::get();
    let this = Target::alloc(mtm).set_ivars(());
    let target: Retained<Target> = unsafe { msg_send![super(this), init] };
    let tgt = Some(&*target as &AnyObject);
    let window = crate::ui::titled_window(mtm, NSSize::new(W, H));
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(ns_string!("Livae Settings"));
    window.setTitlebarAppearsTransparent(true);
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setAppearance(NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }).as_deref());
    window.setMovableByWindowBackground(true);
    let glass = Glass::new(mtm, 0.0, s.glass.max(0.85));
    window.setContentView(Some(&glass.root));
    let root: &NSView = &glass.content;

    // The sidebar: one row per pane, its symbol and name; the chosen one sits on a quiet fill. Arrow keys and
    // Cmd+1 to 7 move between panes too.
    let selection = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
    selection.setWantsLayer(true);
    if let Some(layer) = selection.layer() {
        layer.setBackgroundColor(Some(&crate::ui::white(0.1).CGColor()));
        layer.setCornerRadius(tokens::radius::CONTROL);
    }
    root.addSubview(&selection);
    let mut buttons = Vec::new();
    for i in 0..PANES.len() {
        let frame = NSRect::new(NSPoint::new(12.0, H - TITLEBAR - 8.0 - (i as f64 + 1.0) * 36.0 + 4.0), NSSize::new(SIDEBAR - 24.0, 32.0));
        let row = SidebarRow::new(mtm, i, frame);
        root.addSubview(&row);
        buttons.push(row);
    }

    let switch = |on: bool, action: objc2::runtime::Sel| {
        let sw = NSSwitch::new(mtm);
        sw.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
        unsafe {
            sw.setTarget(tgt);
            sw.setAction(Some(action));
        }
        sw
    };
    let button = |title: &str, action: objc2::runtime::Sel| unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), tgt, Some(action), mtm) };
    let popup = |items: Vec<String>, selected: usize, action: objc2::runtime::Sel| {
        let p = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
        for t in items {
            p.addItemWithTitle(&NSString::from_str(&t));
        }
        p.selectItemAtIndex(selected as isize);
        unsafe {
            p.setTarget(tgt);
            p.setAction(Some(action));
        }
        p
    };
    let pulldown = |action: objc2::runtime::Sel| {
        let p = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, true);
        unsafe {
            p.setTarget(tgt);
            p.setAction(Some(action));
        }
        p
    };
    let help = || text(mtm, "", 14.0, tokens::color::dark::TEXT_SECONDARY);
    let pane_view = || {
        let v = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, H)));
        root.addSubview(&v);
        v
    };
    let mut views = Vec::new();

    // General: the two shortcuts, login, the glass, and Livae itself.
    let v = pane_view();
    let mut p = Pane { mtm, view: &v, top: TITLEBAR - 8.0 };
    p.title("General");
    let launcher = button(&s.launcher, sel!(recordLauncher:));
    let clipboard = button(&s.clipboard, sel!(recordClipboard:));
    p.row("Search shortcut", &[(&launcher, VALUE_W, 28.0)], None);
    p.row("Clipboard shortcut", &[(&clipboard, VALUE_W, 28.0)], None);
    p.group();
    let login = switch(false, sel!(loginToggled:));
    p.row("Open at login", &[(&login, 40.0, 22.0)], None);
    let slider = unsafe { NSSlider::sliderWithValue_minValue_maxValue_target_action(s.glass, 0.0, 1.0, tgt, Some(sel!(glassChanged:)), mtm) };
    slider.setContinuous(true);
    let glass_help = help();
    glass_help.setStringValue(ns_string!("Darker reads better over bright windows"));
    p.row("Glass", &[(&slider, VALUE_W, 24.0)], Some(&glass_help));
    p.group();
    let welcome = button("Welcome…", sel!(openWelcome:));
    p.row(concat!("Livae ", env!("CARGO_PKG_VERSION")), &[(&welcome, BUTTON_W, 28.0)], None);
    views.push(v);

    // Search: how many rows, where files are looked for, and what the empty search shows.
    let v = pane_view();
    let mut p = Pane { mtm, view: &v, top: TITLEBAR - 8.0 };
    p.title("Search");
    let results = popup(RESULTS.iter().map(|n| n.to_string()).collect(), RESULTS.iter().position(|&n| n == s.results).unwrap_or(4), sel!(resultsChanged:));
    let results_help = help();
    results_help.setStringValue(ns_string!("The clipboard shows the same number"));
    p.row("Results shown", &[(&results, VALUE_W, 28.0)], Some(&results_help));
    let recent = switch(s.recent_apps, sel!(recentToggled:));
    p.row("Show most used apps", &[(&recent, 40.0, 22.0)], None);
    p.group();
    let add = button("Add…", sel!(addFolder:));
    let reset = button("Home Only", sel!(resetFolders:));
    // Where it looks, with Desktop, Documents and Downloads in the same line (macOS asks for those apart).
    let folders = help();
    let private_allow = button("Allow…", sel!(allowFolders:));
    p.row("Search in", &[(&add, BUTTON_W, 28.0), (&reset, BUTTON_W, 28.0), (&private_allow, BUTTON_W, 28.0)], Some(&folders));
    views.push(v);

    // Clipboard: pasting, how much is kept, what is never recorded.
    let v = pane_view();
    let mut p = Pane { mtm, view: &v, top: TITLEBAR - 8.0 };
    p.title("Clipboard");
    let paste = switch(s.paste, sel!(pasteToggled:));
    let allow = button("Allow…", sel!(allowAccessibility:));
    let paste_status = help();
    p.row("Paste into the app in front", &[(&paste, 40.0, 22.0), (&allow, BUTTON_W, 28.0)], Some(&paste_status));
    p.group();
    let keep = popup(HISTORY.iter().map(|n| format!("{n} items")).collect(), HISTORY.iter().position(|&n| n == s.history).unwrap_or(1), sel!(historyChanged:));
    p.row("Keep", &[(&keep, VALUE_W, 28.0)], None);
    let clear_btn = button("Clear…", sel!(clearHistory:));
    let history_count = help();
    p.row("History", &[(&clear_btn, BUTTON_W, 28.0)], Some(&history_count));
    p.group();
    let ignore_add = button("Add…", sel!(addIgnored:));
    let ignore_reset = button("Reset", sel!(resetIgnored:));
    let ignored = help();
    p.row("Never record from", &[(&ignore_add, BUTTON_W, 28.0), (&ignore_reset, BUTTON_W, 28.0)], Some(&ignored));
    views.push(v);

    // Breaks.
    let v = pane_view();
    let mut p = Pane { mtm, view: &v, top: TITLEBAR - 8.0 };
    p.title("Breaks");
    let breaks_on = switch(s.breaks.enabled, sel!(breaksToggled:));
    let breaks_status = help();
    p.row("Break reminders", &[(&breaks_on, 40.0, 22.0)], Some(&breaks_status));
    p.group();
    let every = popup(BREAK_EVERY.iter().map(|m| format!("{m} min")).collect(), BREAK_EVERY.iter().position(|&m| m == s.breaks.mini_every_min).unwrap_or(1), sel!(breakEveryChanged:));
    p.row("Every", &[(&every, VALUE_W, 28.0)], None);
    let length_label = |sec: u32| if sec < 60 { format!("{sec} s") } else { format!("{} min", sec / 60) };
    let length = popup(BREAK_SECONDS.iter().map(|&sec| length_label(sec)).collect(), BREAK_SECONDS.iter().position(|&x| x == s.breaks.mini_seconds).unwrap_or(0), sel!(breakLengthChanged:));
    p.row("Length", &[(&length, VALUE_W, 28.0)], None);
    let weekends = switch(s.breaks.weekends_off, sel!(weekendsToggled:));
    p.row("Weekends off", &[(&weekends, 40.0, 22.0)], None);
    views.push(v);

    // Journal.
    let v = pane_view();
    let mut p = Pane { mtm, view: &v, top: TITLEBAR - 8.0 };
    p.title("Journal");
    let journal_on = switch(s.journal.enabled, sel!(journalToggled:));
    let journal_status = help();
    p.row("Evening journal", &[(&journal_on, 40.0, 22.0)], Some(&journal_status));
    p.group();
    let at = popup(JOURNAL_AT.iter().map(|t| t.to_string()).collect(), JOURNAL_AT.iter().position(|&t| t == s.journal.at).unwrap_or(2), sel!(journalAtChanged:));
    p.row("Ask at", &[(&at, VALUE_W, 28.0)], None);
    views.push(v);

    // Sound: the devices kept in use, and the equaliser.
    let v = pane_view();
    let mut p = Pane { mtm, view: &v, top: TITLEBAR - 8.0 };
    p.title("Sound");
    let outputs = pulldown(sel!(outputPicked:));
    let outputs_help = help();
    outputs_help.setStringValue(ns_string!("The first one connected is always used"));
    p.row("Output", &[(&outputs, VALUE_W, 28.0)], Some(&outputs_help));
    let inputs = pulldown(sel!(inputPicked:));
    p.row("Microphone", &[(&inputs, VALUE_W, 28.0)], None);
    p.group();
    let eq = switch(s.sound.eq, sel!(eqToggled:));
    let eq_edit = button("Edit…", sel!(editEq:));
    let eq_allow = button("Allow…", sel!(allowAudio:));
    let eq_status = help();
    p.row("Equaliser", &[(&eq, 40.0, 22.0), (&eq_edit, BUTTON_W, 28.0), (&eq_allow, BUTTON_W, 28.0)], Some(&eq_status));
    views.push(v);

    // Text expansion: on or off, and the file that holds the snippets, quicklinks and command shortcuts.
    let v = pane_view();
    let mut p = Pane { mtm, view: &v, top: TITLEBAR - 8.0 };
    p.title("Text Expansion");
    let expansion = switch(s.expansion, sel!(expansionToggled:));
    let expansion_status = help();
    p.row("Expand snippets as you type", &[(&expansion, 40.0, 22.0)], Some(&expansion_status));
    p.group();
    let edit = button("Edit…", sel!(editFile:));
    let edit_help = help();
    edit_help.setStringValue(ns_string!("Snippets, quicklinks and command shortcuts"));
    p.row("Settings file", &[(&edit, BUTTON_W, 28.0)], Some(&edit_help));
    views.push(v);

    // Shortcut recording: the next key press in this window becomes the shortcut; Esc cancels.
    let handler = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        let ev = unsafe { event.as_ref() };
        let ours = UI.with(|u| u.borrow().as_ref().is_some_and(|ui| ev.window(MainThreadMarker::new().unwrap()).is_some_and(|w| std::ptr::eq(&*w, &*ui.window))));
        if !ours {
            return event.as_ptr();
        }
        if RECORDING.with(|r| r.get()).is_none() {
            // Up and down move between panes (unless a text box has the keyboard), Cmd+1 to 7 jump to one.
            let typing = ev.window(MainThreadMarker::new().unwrap()).and_then(|w| w.firstResponder()).is_some_and(|r| r.isKindOfClass(<objc2_app_kit::NSTextView as objc2::ClassType>::class()));
            let now = PANE.with(|p| p.get());
            let cmd = ev.modifierFlags().contains(NSEventModifierFlags::Command);
            let digit = ev.charactersIgnoringModifiers().map(|c| c.to_string()).and_then(|c| c.parse::<usize>().ok());
            let next = match ev.keyCode() {
                125 if !typing && !cmd => Some((now + 1).min(PANES.len() - 1)),
                126 if !typing && !cmd => Some(now.saturating_sub(1)),
                _ if cmd => digit.filter(|d| (1..=PANES.len()).contains(d)).map(|d| d - 1),
                _ => None,
            };
            if let Some(i) = next {
                show_pane(PANES[i].0);
                return std::ptr::null_mut();
            }
            return event.as_ptr();
        }
        if ev.keyCode() == 53 {
            finish_recording(None);
            return std::ptr::null_mut();
        }
        match shortcut_from(ev) {
            Some(s) => {
                finish_recording(Some(s));
                std::ptr::null_mut()
            }
            None => std::ptr::null_mut(),
        }
    });
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler) };

    window.setDelegate(Some(ProtocolObject::from_ref(&*target)));
    UI.with(|u| {
        *u.borrow_mut() = Some(Ui {
            window, glass, launcher, clipboard, paste_status, allow, folders, home_only: reset, ignored, ignore_reset, history_count,
            breaks_status, journal_status, private_allow, login, expansion, expansion_status, outputs, inputs, eq,
            eq_status, eq_allow, panes: views.into_iter().zip(buttons).collect(), selection, _target: target, _monitor: monitor,
        })
    });
    show_pane("general");
}
