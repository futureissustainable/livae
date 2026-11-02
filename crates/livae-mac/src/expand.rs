//! Text expansion on the Mac. A global key monitor (it needs the Accessibility permission and never sees password
//! fields) feeds livae-core's `Expander`; an expansion erases the trigger and types the snippet as keyboard events,
//! so the clipboard and its history stay untouched.

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use livae_core::snippets::{Expander, Snippet};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSPasteboard, NSPasteboardTypeString, NSWorkspace};
use objc2_core_graphics::{CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation};

/// Marks the events Livae types, so its own monitor skips them ("LIVA").
const OURS: i64 = 0x4C49_5641;

struct State {
    monitor: Retained<AnyObject>,
    expander: Expander,
    snippets: Vec<Snippet>,
    /// The app the typing went to; another app means a fresh start.
    pid: i32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Start or stop listening to match the settings (called at start and on every settings change).
pub fn apply() {
    let s = crate::config::get();
    // A test copy types only when a check asks for it (LIVAE_TEST_EXPANSION), into its own throwaway window.
    let allowed = !crate::app::test_mode() || std::env::var_os("LIVAE_TEST_EXPANSION").is_some();
    let on = s.expansion && allowed && crate::access::trusted();
    STATE.with(|cell| {
        let mut slot = cell.borrow_mut();
        match (on, slot.as_mut()) {
            (true, Some(st)) => st.snippets = s.snippets.clone(),
            (true, None) => {
                let handler = RcBlock::new(|e: NonNull<NSEvent>| on_event(unsafe { e.as_ref() }));
                let mask = NSEventMask::KeyDown | NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown;
                if let Some(monitor) = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &handler) {
                    *slot = Some(State { monitor, expander: Expander::default(), snippets: s.snippets.clone(), pid: 0 });
                }
            }
            (false, Some(_)) => {
                if let Some(st) = slot.take() {
                    unsafe { NSEvent::removeMonitor(&st.monitor) };
                }
            }
            (false, None) => {}
        }
    });
}

fn clipboard_text() -> Option<String> {
    NSPasteboard::generalPasteboard().stringForType(unsafe { NSPasteboardTypeString }).map(|s| s.to_string())
}

fn on_event(ev: &NSEvent) {
    if ev.CGEvent().is_some_and(|cg| CGEvent::integer_value_field(Some(&cg), CGEventField::EventSourceUserData) == OURS) {
        return;
    }
    let pid = NSWorkspace::sharedWorkspace().frontmostApplication().map_or(0, |a| a.processIdentifier());
    let expansion = STATE.with(|cell| {
        let mut slot = cell.borrow_mut();
        let st = slot.as_mut()?;
        if pid != st.pid || ev.r#type() != NSEventType::KeyDown {
            st.pid = pid;
            st.expander.reset();
            if ev.r#type() != NSEventType::KeyDown {
                return None;
            }
        }
        let flags = ev.modifierFlags();
        if flags.intersects(NSEventModifierFlags::Command | NSEventModifierFlags::Control) {
            st.expander.reset();
            return None;
        }
        match ev.keyCode() {
            51 => {
                st.expander.backspace();
                None
            }
            // Return, Tab, Esc, arrows, Home, End, Page Up and Down, Forward Delete: the caret moved.
            36 | 48 | 53 | 76 | 115 | 116 | 117 | 119 | 121 | 123..=126 => {
                st.expander.reset();
                None
            }
            _ => {
                let typed = ev.characters().map(|c| c.to_string()).unwrap_or_default();
                let mut found = None;
                for c in typed.chars() {
                    found = st.expander.typed(c, &st.snippets, clipboard_text).or(found);
                }
                found
            }
        }
    });
    if let Some(x) = expansion {
        // The monitor sees a copy of the key; let the app take the key itself before erasing it.
        let after = dispatch2::DispatchTime::NOW.time(30_000_000);
        let _ = dispatch2::DispatchQueue::main().after(after, move || type_text(&x.text, x.erase, x.left));
    }
}

fn post(source: Option<&CGEventSource>, code: u16, text: Option<&[u16]>) {
    post_as(source, code, text, OURS);
}

fn post_as(source: Option<&CGEventSource>, code: u16, text: Option<&[u16]>, mark: i64) {
    for down in [true, false] {
        let e = CGEvent::new_keyboard_event(source, code, down);
        CGEvent::set_flags(e.as_deref(), CGEventFlags(0));
        CGEvent::set_integer_value_field(e.as_deref(), CGEventField::EventSourceUserData, mark);
        if let Some(t) = text {
            unsafe { CGEvent::keyboard_set_unicode_string(e.as_deref(), t.len() as _, t.as_ptr()) };
        }
        CGEvent::post(CGEventTapLocation::HIDEventTap, e.as_deref());
    }
}

/// Erase `erase` characters, type `text` (in pieces of up to 20 UTF-16 units, the most one event carries), then move
/// the caret `left` characters back.
pub fn type_text(text: &str, erase: usize, left: usize) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let src = source.as_deref();
    for _ in 0..erase {
        post(src, 51, None);
    }
    let mut piece: Vec<u16> = Vec::new();
    let flush = |piece: &mut Vec<u16>| {
        if !piece.is_empty() {
            post(src, 0, Some(piece));
            piece.clear();
        }
    };
    for c in text.chars() {
        match c {
            '\n' => {
                flush(&mut piece);
                post(src, 36, None);
            }
            '\t' => {
                flush(&mut piece);
                post(src, 48, None);
            }
            _ => {
                let mut buf = [0u16; 2];
                let units = c.encode_utf16(&mut buf);
                if piece.len() + units.len() > 20 {
                    flush(&mut piece);
                }
                piece.extend_from_slice(units);
            }
        }
    }
    flush(&mut piece);
    for _ in 0..left {
        post(src, 123, None);
    }
}

/// A snippet chosen in the launcher: typed into the app in front once the launcher has gone.
pub fn type_snippet(text: &str) {
    let (text, left) = livae_core::snippets::render(text, livae_core::journal::now(), clipboard_text);
    let after = dispatch2::DispatchTime::NOW.time(80_000_000);
    let _ = dispatch2::DispatchQueue::main().after(after, move || type_text(&text, 0, left));
}

/// `--type-keys` in a test copy: type `text` one key at a time like a person (unmarked, so the monitor reads it).
pub fn type_as_person(text: &str) {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    for c in text.chars() {
        let mut buf = [0u16; 2];
        post_as(source.as_deref(), 0, Some(c.encode_utf16(&mut buf)), 0);
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
}
