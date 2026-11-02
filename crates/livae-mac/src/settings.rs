//! The Settings window: shortcuts (recorded by pressing them), glass, clipboard behaviour, file-search folders,
//! open at login. Every change is saved at once and applied live.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, MainThreadOnly};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSApplication, NSButton, NSColor, NSControlStateValueOff,
    NSControlStateValueOn, NSEvent, NSEventMask, NSEventModifierFlags, NSLineBreakMode, NSOpenPanel, NSPopUpButton, NSSlider, NSSwitch,
    NSTextAlignment, NSTextField, NSView, NSWindow, NSWindowTitleVisibility, NSScrollView, NSScreen,
};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSWindowDelegate};
use objc2_foundation::{ns_string, MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use crate::config;
use crate::glass::Glass;
use livae_core::tokens;

const W: f64 = 560.0;
const PAD: f64 = 24.0;
const ROW: f64 = 44.0;
/// Room for the window's traffic lights above the title.
const TITLEBAR: f64 = 44.0;
/// Space a section header takes; larger than a row gap so sections read as groups.
const SECTION: f64 = 40.0;
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
    private_status: Retained<NSTextField>,
    private_allow: Retained<NSButton>,
    login: Retained<NSSwitch>,
    expansion: Retained<NSSwitch>,
    expansion_status: Retained<NSTextField>,
    outputs: Retained<NSPopUpButton>,
    inputs: Retained<NSPopUpButton>,
    eq: Retained<NSSwitch>,
    eq_status: Retained<NSTextField>,
    eq_allow: Retained<NSButton>,
    /// The scroll view when the content is taller than the screen; every open starts at the top.
    scroll: Option<Retained<NSScrollView>>,
    _target: Retained<Target>,
    _monitor: Option<Retained<AnyObject>>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static RECORDING: Cell<Option<Recording>> = const { Cell::new(None) };
    /// The device names behind the Output and Microphone menus' items, by tag.
    static CHOICES: RefCell<(Vec<String>, Vec<String>)> = const { RefCell::new((Vec::new(), Vec::new())) };
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
            (ui.launcher.clone(), ui.clipboard.clone(), ui.paste_status.clone(), ui.allow.clone(), ui.folders.clone(), ui.login.clone(), ui.private_status.clone(), ui.private_allow.clone(), ui.home_only.clone(), ui.ignored.clone(), ui.breaks_status.clone(), ui.journal_status.clone(), ui.ignore_reset.clone(), ui.history_count.clone())
        }) else {
            return;
        };
        let (launcher, clipboard, paste_status, allow, folders, login, private_status, private_allow, home_only, ignored, breaks_status, journal_status, ignore_reset, history_count) = ui;
        ignore_reset.setHidden(s.ignore_apps == config::Settings::default().ignore_apps);
        let (items, pinned) = crate::clipboard::counts();
        history_count.setStringValue(&NSString::from_str(&format!("{items} items, {pinned} pinned")));
        breaks_status.setStringValue(&NSString::from_str(&crate::breaks::status_line()));
        journal_status.setStringValue(&NSString::from_str(&crate::journal::status_line()));
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
        private_status.setStringValue(&NSString::from_str(if open {
            "Desktop, Documents and Downloads included"
        } else {
            "Desktop, Documents and Downloads need your OK"
        }));
        private_allow.setHidden(open);
        launcher.setTitle(&NSString::from_str(&crate::ui::shortcut_label(&s.launcher)));
        clipboard.setTitle(&NSString::from_str(&crate::ui::shortcut_label(&s.clipboard)));
        let trusted = crate::access::trusted();
        let status = if !s.paste {
            "Off: the item is copied, you press ⌘V"
        } else if trusted {
            "On: pastes straight into the app in front"
        } else {
            "Needs Accessibility permission to paste"
        };
        paste_status.setStringValue(&NSString::from_str(status));
        allow.setHidden(trusted || !s.paste);
        let names: Vec<String> = s.file_search.iter().map(|f| if f == "~" { "Home folder".to_string() } else { f.clone() }).collect();
        folders.setStringValue(&NSString::from_str(&names.join(", ")));
        login.setState(if crate::access::open_at_login() { NSControlStateValueOn } else { NSControlStateValueOff });
        if let Some((sw, status)) = u.borrow().as_ref().map(|ui| (ui.expansion.clone(), ui.expansion_status.clone())) {
            sw.setState(if s.expansion { NSControlStateValueOn } else { NSControlStateValueOff });
            let first = s.snippets.first().map(|x| format!(", e.g. {}", x.trigger)).unwrap_or_default();
            let line = if !s.expansion {
                "Snippets, quicklinks and command shortcuts are in the file".to_string()
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
            eq_status.setStringValue(&NSString::from_str(&status.text()));
            eq_allow.setHidden(status != crate::sound::Status::NeedsPermission);
        }
    });
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
            if let Some(scroll) = &ui.scroll
                && let Some(doc) = scroll.documentView() {
                    // A test copy can open it at the end, to check the last sections.
                    let end = crate::app::test_mode() && std::env::var_os("LIVAE_TEST_SETTINGS_END").is_some();
                    let top = if end { 0.0 } else { doc.frame().size.height - scroll.contentSize().height };
                    scroll.contentView().scrollToPoint(NSPoint::new(0.0, top.max(0.0)));
                    scroll.reflectScrolledClipView(&scroll.contentView());
                }
            crate::ui::present(&ui.window);
            crate::motion::show(&ui.window, crate::motion::Kind::Bloom);
        }
    });
    if !crate::app::offscreen() {
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    }
}

fn build(mtm: MainThreadMarker) {
    let s = config::get();
    let this = Target::alloc(mtm).set_ivars(());
    let target: Retained<Target> = unsafe { msg_send![super(this), init] };
    let tgt = Some(&*target as &AnyObject);
    // Title, eight sections, the rows below them, the version line.
    let breaks_rows = 4.0 * ROW + (ROW - 16.0);
    let journal_rows = 2.0 * ROW + (ROW - 16.0);
    let general_rows = ROW + ROW + (ROW - 12.0);
    let sound_rows = 3.0 * ROW + (ROW - 12.0);
    let rows = 2.0 * ROW + 2.0 * ROW + (ROW + (ROW - 12.0) + ROW + ROW + ROW + (ROW - 16.0)) + (ROW + (ROW - 16.0) + (ROW - 12.0) + ROW) + breaks_rows + journal_rows + sound_rows + general_rows;
    // The title starts below the traffic lights (TITLEBAR).
    let h = TITLEBAR + 28.0 + 8.0 * SECTION + rows + 16.0 + 18.0 + PAD;
    // Taller than the screen: the same content in a scroll view, the window as tall as fits.
    let fits = NSScreen::mainScreen(mtm).map(|m| m.visibleFrame().size.height - 60.0).unwrap_or(h).min(h);
    let window = crate::ui::titled_window(mtm, NSSize::new(W, fits));
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
    let doc = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, h)));
    let mut scroll_view = None;
    if fits < h {
        // The scroll view stops below the title bar, so rows never pass under the traffic lights; the document
        // drops its own title-bar padding (nothing is laid out there).
        doc.setFrameSize(NSSize::new(W, h - TITLEBAR));
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, fits - TITLEBAR)));
        scroll.setAutomaticallyAdjustsContentInsets(false);
        scroll.setDrawsBackground(false);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        // No autoresizing: the glass view starts at 100 x 100 and grows on the first layout, which would stretch
        // the scroll view past the top of the window. The window never resizes, so a fixed frame is exact.
        scroll.setDocumentView(Some(&doc));
        glass.content.addSubview(&scroll);
        scroll_view = Some(scroll);
    } else {
        glass.content.addSubview(&doc);
    }
    let v: &NSView = &doc;

    let mut y = h - TITLEBAR - 28.0;
    let title = text(mtm, "Settings", 20.0, tokens::color::dark::TEXT);
    title.setFont(Some(&crate::font::sans(20.0, unsafe { objc2_app_kit::NSFontWeightMedium })));
    title.setFrame(NSRect::new(NSPoint::new(PAD, y), NSSize::new(200.0, 26.0)));
    v.addSubview(&title);
    let version = text(mtm, concat!("Livae ", env!("CARGO_PKG_VERSION")), 14.0, tokens::color::dark::TEXT_SECONDARY);
    version.setFrame(NSRect::new(NSPoint::new(PAD, PAD), NSSize::new(200.0, 18.0)));
    v.addSubview(&version);

    let section = |name: &str, y: &mut f64| {
        *y -= SECTION;
        let t = text(mtm, name, 14.0, tokens::color::dark::TEXT);
        t.setFont(Some(&crate::font::sans(14.0, unsafe { objc2_app_kit::NSFontWeightMedium })));
        t.setFrame(NSRect::new(NSPoint::new(PAD, *y + 4.0), NSSize::new(300.0, 18.0)));
        v.addSubview(&t);
    };
    let label_at = |s: &str, y: f64| {
        let t = text(mtm, s, 16.0, tokens::color::dark::TEXT);
        t.setFrame(NSRect::new(NSPoint::new(PAD, y + 11.0), NSSize::new(260.0, 22.0)));
        v.addSubview(&t);
    };
    let right = |view: &NSView, width: f64, y: f64, height: f64| {
        view.setFrame(NSRect::new(NSPoint::new(W - PAD - width, y + (ROW - height) / 2.0), NSSize::new(width, height)));
        v.addSubview(view);
    };

    section("Shortcuts", &mut y);
    y -= ROW;
    label_at("Search", y);
    let launcher = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(&s.launcher), tgt, Some(sel!(recordLauncher:)), mtm) };
    right(&launcher, 180.0, y, 32.0);
    y -= ROW;
    label_at("Clipboard", y);
    let clipboard = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(&s.clipboard), tgt, Some(sel!(recordClipboard:)), mtm) };
    right(&clipboard, 180.0, y, 32.0);

    section("Look", &mut y);
    y -= ROW;
    label_at("Glass", y);
    let slider = unsafe { NSSlider::sliderWithValue_minValue_maxValue_target_action(s.glass, 0.0, 1.0, tgt, Some(sel!(glassChanged:)), mtm) };
    slider.setContinuous(true);
    right(&slider, 220.0, y, 24.0);
    let clear = text(mtm, "Clear", 14.0, tokens::color::dark::TEXT_SECONDARY);
    clear.setAlignment(NSTextAlignment::Right);
    clear.setFrame(NSRect::new(NSPoint::new(W - PAD - 220.0 - 52.0, y + 14.0), NSSize::new(44.0, 16.0)));
    v.addSubview(&clear);
    let slider_frame = slider.frame();
    slider.setFrame(NSRect::new(NSPoint::new(W - PAD - 220.0, slider_frame.origin.y), NSSize::new(180.0, slider_frame.size.height)));
    let dark = text(mtm, "Dark", 14.0, tokens::color::dark::TEXT_SECONDARY);
    dark.setFrame(NSRect::new(NSPoint::new(W - PAD - 32.0, y + 14.0), NSSize::new(36.0, 16.0)));
    v.addSubview(&dark);
    y -= ROW;
    // How many results the search shows at once; the clipboard panel is the same size.
    label_at("Results shown", y);
    let results = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    for n in RESULTS {
        results.addItemWithTitle(&NSString::from_str(&n.to_string()));
    }
    results.selectItemAtIndex(RESULTS.iter().position(|&n| n == s.results).unwrap_or(4) as isize);
    unsafe {
        results.setTarget(tgt);
        results.setAction(Some(sel!(resultsChanged:)));
    }
    right(&results, 130.0, y, 28.0);

    section("Clipboard", &mut y);
    y -= ROW;
    label_at("Paste into the app in front", y);
    let paste = NSSwitch::new(mtm);
    paste.setState(if s.paste { NSControlStateValueOn } else { NSControlStateValueOff });
    unsafe {
        paste.setTarget(tgt);
        paste.setAction(Some(sel!(pasteToggled:)));
    }
    right(&paste, 40.0, y, 22.0);
    y -= ROW - 12.0;
    let paste_status = text(mtm, "", 14.0, tokens::color::dark::TEXT_SECONDARY);
    paste_status.setFrame(NSRect::new(NSPoint::new(PAD, y + 14.0), NSSize::new(320.0, 18.0)));
    v.addSubview(&paste_status);
    let allow = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Allow…"), tgt, Some(sel!(allowAccessibility:)), mtm) };
    right(&allow, 100.0, y, 28.0);
    y -= ROW;
    label_at("History size", y);
    let keep = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    for n in HISTORY {
        keep.addItemWithTitle(&NSString::from_str(&format!("{n} items")));
    }
    keep.selectItemAtIndex(HISTORY.iter().position(|&n| n == s.history).unwrap_or(1) as isize);
    unsafe {
        keep.setTarget(tgt);
        keep.setAction(Some(sel!(historyChanged:)));
    }
    right(&keep, 130.0, y, 28.0);
    let clear_btn = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Clear unpinned items…"), tgt, Some(sel!(clearHistory:)), mtm) };
    y -= ROW;
    right(&clear_btn, 170.0, y, 28.0);
    let history_count = text(mtm, "", 14.0, tokens::color::dark::TEXT_SECONDARY);
    history_count.setFrame(NSRect::new(NSPoint::new(PAD, y + 14.0), NSSize::new(260.0, 18.0)));
    v.addSubview(&history_count);
    y -= ROW;
    label_at("Never record from", y);
    let ignore_add = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Add…"), tgt, Some(sel!(addIgnored:)), mtm) };
    right(&ignore_add, 80.0, y, 28.0);
    let ignore_reset = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Reset"), tgt, Some(sel!(resetIgnored:)), mtm) };
    ignore_reset.setFrame(NSRect::new(NSPoint::new(W - PAD - 80.0 - 8.0 - 80.0, y + 8.0), NSSize::new(80.0, 28.0)));
    v.addSubview(&ignore_reset);
    y -= ROW - 16.0;
    let ignored = text(mtm, "", 14.0, tokens::color::dark::TEXT_SECONDARY);
    ignored.setFrame(NSRect::new(NSPoint::new(PAD, y + 14.0), NSSize::new(W - 2.0 * PAD, 18.0)));
    v.addSubview(&ignored);

    section("Search", &mut y);
    y -= ROW;
    label_at("Search in", y);
    let add = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Add…"), tgt, Some(sel!(addFolder:)), mtm) };
    right(&add, 80.0, y, 28.0);
    let reset = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Use Home only"), tgt, Some(sel!(resetFolders:)), mtm) };
    reset.setFrame(NSRect::new(NSPoint::new(W - PAD - 80.0 - 8.0 - 130.0, y + 8.0), NSSize::new(130.0, 28.0)));
    v.addSubview(&reset);
    y -= ROW - 16.0;
    let folders = text(mtm, "", 14.0, tokens::color::dark::TEXT_SECONDARY);
    folders.setFrame(NSRect::new(NSPoint::new(PAD, y + 14.0), NSSize::new(W - 2.0 * PAD, 18.0)));
    v.addSubview(&folders);
    y -= ROW - 12.0;
    let private_status = text(mtm, "", 14.0, tokens::color::dark::TEXT_SECONDARY);
    private_status.setFrame(NSRect::new(NSPoint::new(PAD, y + 14.0), NSSize::new(340.0, 18.0)));
    v.addSubview(&private_status);
    let private_allow = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Allow…"), tgt, Some(sel!(allowFolders:)), mtm) };
    right(&private_allow, 100.0, y, 28.0);
    y -= ROW;
    label_at("Show most used apps", y);
    let recent = NSSwitch::new(mtm);
    recent.setState(if s.recent_apps { NSControlStateValueOn } else { NSControlStateValueOff });
    unsafe {
        recent.setTarget(tgt);
        recent.setAction(Some(sel!(recentToggled:)));
    }
    right(&recent, 40.0, y, 22.0);

    section("Breaks", &mut y);
    y -= ROW;
    label_at("Break reminders", y);
    let breaks_on = NSSwitch::new(mtm);
    breaks_on.setState(if s.breaks.enabled { NSControlStateValueOn } else { NSControlStateValueOff });
    unsafe {
        breaks_on.setTarget(tgt);
        breaks_on.setAction(Some(sel!(breaksToggled:)));
    }
    right(&breaks_on, 40.0, y, 22.0);
    y -= ROW - 16.0;
    let breaks_status = text(mtm, "", 14.0, tokens::color::dark::TEXT_SECONDARY);
    breaks_status.setFrame(NSRect::new(NSPoint::new(PAD, y + 14.0), NSSize::new(W - 2.0 * PAD, 18.0)));
    v.addSubview(&breaks_status);
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
    y -= ROW;
    label_at("Every", y);
    let every = popup(BREAK_EVERY.iter().map(|m| format!("{m} min")).collect(), BREAK_EVERY.iter().position(|&m| m == s.breaks.mini_every_min).unwrap_or(1), sel!(breakEveryChanged:));
    right(&every, 130.0, y, 28.0);
    y -= ROW;
    label_at("Length", y);
    let length_label = |sec: u32| if sec < 60 { format!("{sec} s") } else { format!("{} min", sec / 60) };
    let length = popup(BREAK_SECONDS.iter().map(|&sec| length_label(sec)).collect(), BREAK_SECONDS.iter().position(|&x| x == s.breaks.mini_seconds).unwrap_or(0), sel!(breakLengthChanged:));
    right(&length, 130.0, y, 28.0);
    y -= ROW;
    label_at("Weekends off", y);
    let weekends = NSSwitch::new(mtm);
    weekends.setState(if s.breaks.weekends_off { NSControlStateValueOn } else { NSControlStateValueOff });
    unsafe {
        weekends.setTarget(tgt);
        weekends.setAction(Some(sel!(weekendsToggled:)));
    }
    right(&weekends, 40.0, y, 22.0);

    section("Journal", &mut y);
    y -= ROW;
    label_at("Evening journal", y);
    let journal_on = NSSwitch::new(mtm);
    journal_on.setState(if s.journal.enabled { NSControlStateValueOn } else { NSControlStateValueOff });
    unsafe {
        journal_on.setTarget(tgt);
        journal_on.setAction(Some(sel!(journalToggled:)));
    }
    right(&journal_on, 40.0, y, 22.0);
    y -= ROW - 16.0;
    let journal_status = text(mtm, "", 14.0, tokens::color::dark::TEXT_SECONDARY);
    journal_status.setFrame(NSRect::new(NSPoint::new(PAD, y + 14.0), NSSize::new(W - 2.0 * PAD, 18.0)));
    v.addSubview(&journal_status);
    y -= ROW;
    label_at("Ask at", y);
    let at = popup(JOURNAL_AT.iter().map(|t| t.to_string()).collect(), JOURNAL_AT.iter().position(|&t| t == s.journal.at).unwrap_or(2), sel!(journalAtChanged:));
    right(&at, 130.0, y, 28.0);

    // The output and microphone kept in use, and the equaliser on the output.
    section("Sound", &mut y);
    let pulldown = |action: objc2::runtime::Sel| {
        let p = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, true);
        unsafe {
            p.setTarget(tgt);
            p.setAction(Some(action));
        }
        p
    };
    y -= ROW;
    label_at("Output", y);
    let outputs = pulldown(sel!(outputPicked:));
    right(&outputs, 260.0, y, 28.0);
    y -= ROW;
    label_at("Microphone", y);
    let inputs = pulldown(sel!(inputPicked:));
    right(&inputs, 260.0, y, 28.0);
    y -= ROW;
    label_at("Equaliser", y);
    let eq = NSSwitch::new(mtm);
    unsafe {
        eq.setTarget(tgt);
        eq.setAction(Some(sel!(eqToggled:)));
    }
    right(&eq, 40.0, y, 22.0);
    y -= ROW - 12.0;
    let eq_status = text(mtm, "", 14.0, tokens::color::dark::TEXT_SECONDARY);
    eq_status.setFrame(NSRect::new(NSPoint::new(PAD, y + 14.0), NSSize::new(W - 2.0 * PAD - 220.0, 18.0)));
    v.addSubview(&eq_status);
    let eq_allow = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Allow…"), tgt, Some(sel!(allowAudio:)), mtm) };
    eq_allow.setFrame(NSRect::new(NSPoint::new(W - PAD - 210.0, y + (ROW - 28.0) / 2.0), NSSize::new(100.0, 28.0)));
    v.addSubview(&eq_allow);
    let eq_edit = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Edit…"), tgt, Some(sel!(editEq:)), mtm) };
    right(&eq_edit, 100.0, y, 28.0);

    section("General", &mut y);
    y -= ROW;
    label_at("Open at login", y);
    let login = NSSwitch::new(mtm);
    unsafe {
        login.setTarget(tgt);
        login.setAction(Some(sel!(loginToggled:)));
    }
    right(&login, 40.0, y, 22.0);
    y -= ROW;
    label_at("Text expansion", y);
    let expansion = NSSwitch::new(mtm);
    expansion.setState(if s.expansion { NSControlStateValueOn } else { NSControlStateValueOff });
    unsafe {
        expansion.setTarget(tgt);
        expansion.setAction(Some(sel!(expansionToggled:)));
    }
    right(&expansion, 40.0, y, 22.0);
    y -= ROW - 12.0;
    let expansion_status = text(mtm, "", 14.0, tokens::color::dark::TEXT_SECONDARY);
    expansion_status.setFrame(NSRect::new(NSPoint::new(PAD, y + 14.0), NSSize::new(W - 2.0 * PAD - 110.0, 18.0)));
    v.addSubview(&expansion_status);
    let edit = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Edit file…"), tgt, Some(sel!(editFile:)), mtm) };
    right(&edit, 100.0, y, 28.0);

    // Shortcut recording: the next key press in this window becomes the shortcut; Esc cancels.
    let handler = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        let ev = unsafe { event.as_ref() };
        let ours = UI.with(|u| u.borrow().as_ref().is_some_and(|ui| ev.window(MainThreadMarker::new().unwrap()).is_some_and(|w| std::ptr::eq(&*w, &*ui.window))));
        if RECORDING.with(|r| r.get()).is_none() || !ours {
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
        *u.borrow_mut() = Some(Ui { window, glass, launcher, clipboard, paste_status, allow, folders, home_only: reset, ignored, ignore_reset, history_count, breaks_status, journal_status, private_status, private_allow, login, expansion, expansion_status, outputs, inputs, eq, eq_status, eq_allow, scroll: scroll_view, _target: target, _monitor: monitor })
    });
}
