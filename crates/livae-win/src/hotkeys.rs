//! Global shortcuts for the panels and commands, read from the settings ("Hyper" = Ctrl+Alt+Shift). A shortcut another app holds is
//! retried every 5 s and shown in the tray, never silently lost.

use std::cell::RefCell;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use livae_core::commands::{self, Command};
use livae_core::settings::{Platform, Settings};
use livae_core::shortcut::{self, Key, HYPER_WINDOWS};

#[derive(Clone, Copy)]
pub enum Target {
    Launcher,
    Clipboard,
    Command(Command),
}

struct Hotkeys {
    manager: GlobalHotKeyManager,
    active: Vec<(HotKey, Target)>,
    pending: Vec<(HotKey, Target)>,
}

thread_local! {
    static HOTKEYS: RefCell<Option<Hotkeys>> = const { RefCell::new(None) };
}

fn hotkey(text: &str) -> Option<HotKey> {
    let s = shortcut::parse(text, HYPER_WINDOWS)?;
    let mut mods = Modifiers::empty();
    mods.set(Modifiers::CONTROL, s.mods.ctrl);
    mods.set(Modifiers::ALT, s.mods.alt);
    mods.set(Modifiers::SHIFT, s.mods.shift);
    mods.set(Modifiers::META, s.mods.meta);
    let code = match s.key {
        Key::Space => Code::Space,
        Key::Left => Code::ArrowLeft,
        Key::Right => Code::ArrowRight,
        Key::Up => Code::ArrowUp,
        Key::Down => Code::ArrowDown,
        Key::Return => Code::Enter,
        Key::Letter(c) => format!("Key{c}").parse().unwrap_or(Code::Space),
        Key::Digit(d) => format!("Digit{d}").parse().unwrap_or(Code::Space),
    };
    Some(HotKey::new(Some(mods), code))
}

/// The panel or command a registered hotkey runs.
pub fn target(id: u32) -> Option<Target> {
    HOTKEYS.with(|h| h.borrow().as_ref().and_then(|h| h.active.iter().find(|(key, _)| key.id() == id).map(|(_, target)| *target)))
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
    if crate::config::test_mode() { return; }
    let launcher = hotkey(&s.launcher).unwrap_or_else(|| hotkey("Hyper+Space").expect("fallback shortcut"));
    let clipboard = hotkey(&s.clipboard).unwrap_or_else(|| hotkey("Hyper+V").expect("fallback shortcut"));
    HOTKEYS.with(|h| {
        if let Some(h) = h.borrow_mut().as_mut() {
            for (hk, _) in h.active.drain(..) {
                let _ = h.manager.unregister(hk);
            }
            h.pending = vec![(launcher, Target::Launcher), (clipboard, Target::Clipboard)];
            for (id, text) in &s.hotkeys {
                if let (Some(command), Some(key)) = (commands::by_id(id, Platform::Windows), hotkey(text)) {
                    h.pending.push((key, Target::Command(command)));
                }
            }
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
        for (hk, target) in h.pending.drain(..) {
            if !h.active.iter().any(|(active, _)| active.id() == hk.id()) && h.manager.register(hk).is_ok() {
                h.active.push((hk, target));
            } else {
                still.push((hk, target));
            }
        }
        h.pending = still;
        !h.pending.is_empty()
    })
}

pub fn taken() -> bool {
    HOTKEYS.with(|h| h.borrow().as_ref().is_some_and(|h| !h.pending.is_empty()))
}
