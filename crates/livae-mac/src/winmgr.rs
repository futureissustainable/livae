//! Window placement through Accessibility: the focused window of the app in front is moved and sized to a half,
//! third, quarter, the centre or the next screen (geometry in livae-core). A `--test` copy only logs the command.

use std::cell::RefCell;
use std::ptr::NonNull;

use livae_core::window::{self, Action, Rect};
use objc2_app_kit::NSScreen;
use objc2_application_services::{AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{CFEqual, CFRetained, CFString, CGPoint, CGSize};
use objc2_foundation::MainThreadMarker;

thread_local! {
    /// Where each window was before Livae first moved it, for Restore (the newest 50).
    static BEFORE: RefCell<Vec<(CFRetained<AXUIElement>, Rect)>> = const { RefCell::new(Vec::new()) };
}

fn frame(w: &AXUIElement) -> Option<Rect> {
    let read = |name: &str, kind: AXValueType, out: NonNull<std::ffi::c_void>| {
        let v = crate::access::attribute(w, name)?.downcast::<AXValue>().ok()?;
        unsafe { v.value(kind, out) }.then_some(())
    };
    let mut p = CGPoint::new(0.0, 0.0);
    let mut s = CGSize::new(0.0, 0.0);
    read("AXPosition", AXValueType::CGPoint, NonNull::from(&mut p).cast())?;
    read("AXSize", AXValueType::CGSize, NonNull::from(&mut s).cast())?;
    Some(Rect::new(p.x, p.y, s.width, s.height))
}

fn set(w: &AXUIElement, name: &str, kind: AXValueType, value: NonNull<std::ffi::c_void>) {
    if let Some(v) = unsafe { AXValue::new(kind, value) } {
        unsafe { w.set_attribute_value(&CFString::from_str(name), &v) };
    }
}

/// Size, then position, then size again: a window that could not grow before moving gets its size on the new spot.
fn apply(w: &AXUIElement, r: Rect) {
    let mut p = CGPoint::new(r.x, r.y);
    let mut s = CGSize::new(r.w, r.h);
    set(w, "AXSize", AXValueType::CGSize, NonNull::from(&mut s).cast());
    set(w, "AXPosition", AXValueType::CGPoint, NonNull::from(&mut p).cast());
    set(w, "AXSize", AXValueType::CGSize, NonNull::from(&mut s).cast());
}

/// Each screen's usable area (without the menu bar and the Dock), top-left origin like Accessibility.
fn areas(mtm: MainThreadMarker) -> Vec<Rect> {
    let primary = crate::ui::primary_height(mtm);
    NSScreen::screens(mtm)
        .iter()
        .map(|s| {
            let v = s.visibleFrame();
            Rect::new(v.origin.x, primary - v.origin.y - v.size.height, v.size.width, v.size.height)
        })
        .collect()
}

fn same(a: &AXUIElement, b: &AXUIElement) -> bool {
    CFEqual(Some(a), Some(b))
}

/// Place the focused window of the app with process id `pid`.
pub fn run(action: Action, id: &str, pid: Option<i32>) {
    // A test copy only logs, unless a check points it at its own throwaway window (LIVAE_TEST_WINDOWS, `--pid`).
    if crate::app::test_mode() && std::env::var_os("LIVAE_TEST_WINDOWS").is_none() {
        crate::system::log_test(id);
        return;
    }
    if !crate::access::trusted() {
        // Never a permission prompt from a test copy.
        if !crate::app::test_mode() {
            crate::access::request();
        }
        return;
    }
    let (Some(pid), Some(mtm)) = (pid.filter(|&p| p != std::process::id() as i32), MainThreadMarker::new()) else { return };
    let app = unsafe { AXUIElement::new_application(pid) };
    unsafe { app.set_messaging_timeout(0.3) };
    // The focused window; an app in the background may only report its main window.
    let window = |name| crate::access::attribute(&app, name).and_then(|w| w.downcast::<AXUIElement>().ok());
    let Some(win) = window("AXFocusedWindow").or_else(|| window("AXMainWindow")) else { return };
    let Some(now) = frame(&win) else { return };
    let screens = areas(mtm);
    let target = match action {
        Action::Restore => BEFORE.with(|b| {
            let mut b = b.borrow_mut();
            let at = b.iter().position(|(w, _)| same(w, &win))?;
            Some(b.remove(at).1)
        }),
        Action::NextDisplay => window::next_display(now, &screens),
        _ => window::screen_of(now, &screens).and_then(|i| window::place(action, screens[i], now)),
    };
    let Some(target) = target else { return };
    if action != Action::Restore {
        BEFORE.with(|b| {
            let mut b = b.borrow_mut();
            if !b.iter().any(|(w, _)| same(w, &win)) {
                b.push((win.clone(), now));
                if b.len() > 50 {
                    b.remove(0);
                }
            }
        });
    }
    apply(&win, target);
}
