//! The two global shortcuts, read from the settings ("Hyper" = Ctrl+Alt+Shift). A shortcut another app holds is
//! retried every 5 s and shown in the tray, never silently lost.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use livae_core::settings::Settings;
use livae_core::shortcut::{self, Key, HYPER_WINDOWS};

static LAUNCHER_ID: AtomicU32 = AtomicU32::new(0);
static CLIPBOARD_ID: AtomicU32 = AtomicU32::new(0);

pub enum Target {
    Launcher,
    Clipboard,
}

struct Hotkeys {
    manager: GlobalHotKeyManager,
    active: Vec<HotKey>,
    pending: Vec<HotKey>,
}

thread_local! {
    static HOTKEYS: RefCell<Option<Hotkeys>> = const { RefCell::new(None) };
}

fn hotkey(text: &str, fallback: &str) -> HotKey {
    let s = shortcut::parse(text, HYPER_WINDOWS).or_else(|| shortcut::parse(fallback, HYPER_WINDOWS)).expect("fallback shortcut");
    let mut mods = Modifiers::empty();
    mods.set(Modifiers::CONTROL, s.mods.ctrl);
    mods.set(Modifiers::ALT, s.mods.alt);
    mods.set(Modifiers::SHIFT, s.mods.shift);
    mods.set(Modifiers::META, s.mods.meta);
    let code = match s.key {
        Key::Space => Code::Space,
        Key::Letter(c) => format!("Key{c}").parse().unwrap_or(Code::Space),
        Key::Digit(d) => format!("Digit{d}").parse().unwrap_or(Code::Space),
    };
    HotKey::new(Some(mods), code)
}

/// Which panel a hotkey id opens.
pub fn target(id: u32) -> Option<Target> {
    if id == LAUNCHER_ID.load(Ordering::SeqCst) {
        Some(Target::Launcher)
    } else if id == CLIPBOARD_ID.load(Ordering::SeqCst) {
        Some(Target::Clipboard)
    } else {
        None
    }
}

/// Start listening. The press arrives in global-hotkey's own window on this thread; it is posted to our main
/// window so it runs outside any other message. (Its release events come from another thread and are ignored.)
pub fn install() {
    let Ok(manager) = GlobalHotKeyManager::new() else { return };
    GlobalHotKeyEvent::set_event_handler(Some(|e: GlobalHotKeyEvent| {
        if e.state == HotKeyState::Pressed {
            crate::app::post_hotkey(e.id);
        }
    }));
    HOTKEYS.with(|h| *h.borrow_mut() = Some(Hotkeys { manager, active: Vec::new(), pending: Vec::new() }));
}

/// Register the settings' shortcuts, replacing the old ones.
pub fn apply(s: &Settings) {
    let launcher = hotkey(&s.launcher, "Hyper+Space");
    let clipboard = hotkey(&s.clipboard, "Hyper+V");
    LAUNCHER_ID.store(launcher.id(), Ordering::SeqCst);
    CLIPBOARD_ID.store(clipboard.id(), Ordering::SeqCst);
    HOTKEYS.with(|h| {
        if let Some(h) = h.borrow_mut().as_mut() {
            for hk in h.active.drain(..) {
                let _ = h.manager.unregister(hk);
            }
            h.pending = vec![launcher, clipboard];
        }
    });
    retry();
}

/// Try the shortcuts still held by another app. True when one is still taken.
pub fn retry() -> bool {
    HOTKEYS.with(|h| {
        let mut slot = h.borrow_mut();
        let Some(h) = slot.as_mut() else { return false };
        let mut still = Vec::new();
        for hk in h.pending.drain(..) {
            if h.manager.register(hk).is_ok() {
                h.active.push(hk);
            } else {
                still.push(hk);
            }
        }
        h.pending = still;
        !h.pending.is_empty()
    })
}

pub fn taken() -> bool {
    HOTKEYS.with(|h| h.borrow().as_ref().is_some_and(|h| !h.pending.is_empty()))
}
