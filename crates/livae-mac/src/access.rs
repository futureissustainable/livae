//! System access: the Accessibility permission (to paste into the app in front and to find the text cursor),
//! the protected folders file search needs, posting ⌘V, and open at login.

use std::ptr::NonNull;

use objc2_application_services::{AXError, AXIsProcessTrusted, AXIsProcessTrustedWithOptions, AXUIElement, AXValue, AXValueType, kAXTrustedCheckOptionPrompt};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

pub fn trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Shows macOS's own prompt, which leads to Privacy & Security → Accessibility.
pub fn request() {
    let key: &CFString = unsafe { kAXTrustedCheckOptionPrompt };
    let dict = CFDictionary::<CFString, CFBoolean>::from_slices(&[key], &[CFBoolean::new(true)]);
    unsafe { AXIsProcessTrustedWithOptions(Some(dict.as_opaque())) };
}

/// Folders macOS hides from apps (Spotlight results included) until the user allows each one.
fn protected() -> [std::path::PathBuf; 3] {
    let home = crate::config::home();
    [home.join("Desktop"), home.join("Documents"), home.join("Downloads")]
}

fn folders_marker() -> std::path::PathBuf {
    crate::config::support_dir().join(".folders-asked")
}

/// Whether the user has been asked for the protected folders (asking again shows nothing new).
pub fn folders_asked() -> bool {
    folders_marker().exists()
}

/// True when every protected folder can be read. Only call after asking: on an undecided folder this prompts.
pub fn folders_allowed() -> bool {
    folders_asked() && protected().iter().all(|d| !d.is_dir() || std::fs::read_dir(d).is_ok())
}

/// Ask for the folders the first time; afterwards macOS no longer prompts, so open Files and Folders in Settings.
pub fn allow_folders(done: fn()) {
    if folders_asked() {
        let url = objc2_foundation::NSURL::URLWithString(objc2_foundation::ns_string!(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders"
        ));
        if let Some(url) = url {
            objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&url);
        }
    } else {
        request_folders(done);
    }
}

/// Ask macOS for Desktop, Documents and Downloads, one system prompt each, off the main thread (each prompt
/// blocks until answered); `done` runs on the main thread afterwards.
pub fn request_folders(done: fn()) {
    std::thread::spawn(move || {
        for d in protected() {
            let _ = std::fs::read_dir(d);
        }
        // Marked only once every prompt is answered, so no main-thread check ever lands on an open prompt.
        let _ = std::fs::write(folders_marker(), "");
        dispatch2::DispatchQueue::main().exec_async(done);
    });
}

fn attribute(element: &AXUIElement, name: &str) -> Option<CFRetained<CFType>> {
    let mut out: *const CFType = std::ptr::null();
    let err = unsafe { element.copy_attribute_value(&CFString::from_str(name), NonNull::from(&mut out)) };
    if err != AXError::Success || out.is_null() {
        return None;
    }
    Some(unsafe { CFRetained::from_raw(NonNull::new_unchecked(out as *mut CFType)) })
}

/// Screen rectangle of the text cursor in the focused app, in AppKit coordinates (origin bottom-left).
/// None when there is no text cursor, the app does not report it, or Livae lacks the permission.
pub fn caret_rect(primary_screen_height: f64) -> Option<CGRect> {
    if !trusted() {
        return None;
    }
    let system = unsafe { AXUIElement::new_system_wide() };
    // Process-wide for Accessibility calls; the hotkey path makes up to three, so a hung app costs at most 0.3 s.
    unsafe { system.set_messaging_timeout(0.1) };
    let focused = attribute(&system, "AXFocusedUIElement")?.downcast::<AXUIElement>().ok()?;
    let range = attribute(&focused, "AXSelectedTextRange")?;
    let mut out: *const CFType = std::ptr::null();
    let err = unsafe { focused.copy_parameterized_attribute_value(&CFString::from_str("AXBoundsForRange"), &range, NonNull::from(&mut out)) };
    if err != AXError::Success || out.is_null() {
        return None;
    }
    let value = unsafe { CFRetained::from_raw(NonNull::new_unchecked(out as *mut CFType)) }.downcast::<AXValue>().ok()?;
    let mut rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0));
    if !unsafe { value.value(AXValueType::CGRect, NonNull::from(&mut rect).cast()) } {
        return None;
    }
    if rect.size.height <= 0.0 && rect.origin.x == 0.0 && rect.origin.y == 0.0 {
        return None;
    }
    // Accessibility uses a top-left origin on the primary screen.
    rect.origin.y = primary_screen_height - rect.origin.y - rect.size.height;
    Some(rect)
}

/// ⌘V into whatever app is in front (needs the Accessibility permission).
pub fn post_paste() {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    for down in [true, false] {
        let event = CGEvent::new_keyboard_event(source.as_deref(), 9, down);
        CGEvent::set_flags(event.as_deref(), CGEventFlags::MaskCommand);
        CGEvent::post(CGEventTapLocation::HIDEventTap, event.as_deref());
    }
}

pub fn open_at_login() -> bool {
    unsafe { SMAppService::mainAppService().status() == SMAppServiceStatus::Enabled }
}

/// True when macOS accepted the change (it refuses for a copy that is not an installed app bundle).
pub fn set_open_at_login(on: bool) -> bool {
    let service = unsafe { SMAppService::mainAppService() };
    unsafe { if on { service.registerAndReturnError() } else { service.unregisterAndReturnError() } }.is_ok()
}
