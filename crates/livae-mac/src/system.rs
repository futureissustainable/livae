//! System commands from the launcher or a hotkey: lock, sleep, restart, empty the Trash, dark mode, quit the app in
//! front. A `--test` copy only writes the command's id to `actions.log` in its data folder.

use livae_core::commands::System;
use objc2_app_kit::NSRunningApplication;
use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation};

/// A test copy records what would have run, so checks never lock or restart the owner's Mac.
pub fn log_test(id: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(crate::config::support_dir().join("actions.log")) {
        let _ = writeln!(f, "{id}");
    }
}

/// AppleScript off the main thread (System Events and Finder may ask for permission the first time).
fn script(source: &'static str) {
    std::thread::spawn(move || {
        let _ = std::process::Command::new("/usr/bin/osascript").args(["-e", source]).status();
    });
}

fn run_tool(path: &'static str, arg: &'static str) {
    std::thread::spawn(move || {
        let _ = std::process::Command::new(path).arg(arg).status();
    });
}

/// ⌃⌘Q, the Mac's own Lock Screen shortcut (needs the Accessibility permission Livae already asks for).
fn lock() {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    for down in [true, false] {
        let event = CGEvent::new_keyboard_event(source.as_deref(), 12, down);
        CGEvent::set_flags(event.as_deref(), CGEventFlags::MaskCommand | CGEventFlags::MaskControl);
        CGEvent::post(CGEventTapLocation::HIDEventTap, event.as_deref());
    }
}

/// `front` is the app that was in front when the launcher opened (or when the hotkey was pressed).
pub fn run(action: System, id: &str, front: Option<&NSRunningApplication>) {
    if crate::app::test_mode() {
        log_test(id);
        return;
    }
    // Never quit Livae itself through "the app in front".
    let front = front.filter(|a| a.processIdentifier() != std::process::id() as i32);
    match action {
        System::LockScreen => lock(),
        System::Sleep => run_tool("/usr/bin/pmset", "sleepnow"),
        System::DisplaysOff => run_tool("/usr/bin/pmset", "displaysleepnow"),
        System::Restart => script("tell application \"System Events\" to restart"),
        System::ShutDown => script("tell application \"System Events\" to shut down"),
        System::LogOut => script("tell application \"System Events\" to log out"),
        System::EmptyTrash => script("tell application \"Finder\" to empty trash"),
        System::EjectDisks => script("tell application \"Finder\" to eject (every disk whose ejectable is true)"),
        System::DarkMode => script("tell application \"System Events\" to tell appearance preferences to set dark mode to not dark mode"),
        System::QuitApp => {
            if let Some(app) = front {
                app.terminate();
            }
        }
        System::ForceQuitApp => {
            if let Some(app) = front {
                app.forceTerminate();
            }
        }
    }
}
