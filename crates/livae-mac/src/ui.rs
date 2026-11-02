//! Small AppKit helpers shared by the launcher, the clipboard and Settings.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSResponder, NSWindow, NSWindowStyleMask,
    NSBox, NSBoxType, NSColor, NSFocusRingType, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSImage, NSImageView,
    NSLineBreakMode, NSScreen, NSTextField, NSTitlePosition,
};
use objc2_foundation::{ns_string, MainThreadMarker, NSAttributedString, NSDictionary, NSObject, NSPoint, NSRect, NSSize, NSString};

use crate::panel::Controller;
use livae_core::tokens;

pub const TEXT: [u8; 3] = tokens::color::dark::TEXT;
pub const SECONDARY: [u8; 3] = tokens::color::dark::TEXT_SECONDARY;
pub const TERTIARY: [u8; 3] = tokens::color::dark::TEXT_TERTIARY;

pub fn rgb(c: [u8; 3]) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(c[0] as f64 / 255.0, c[1] as f64 / 255.0, c[2] as f64 / 255.0, 1.0)
}

pub fn white(alpha: f64) -> Retained<NSColor> {
    NSColor::colorWithWhite_alpha(1.0, alpha)
}

pub fn label(mtm: MainThreadMarker, size: f64, color: [u8; 3]) -> Retained<NSTextField> {
    let t = NSTextField::labelWithString(ns_string!(""), mtm);
    t.setFont(Some(&crate::font::regular(size)));
    t.setTextColor(Some(&rgb(color)));
    t.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    t
}

pub fn symbol(name: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)
}

pub fn placeholder(text: &str, font: &NSFont) -> Retained<NSAttributedString> {
    let color = rgb(TERTIARY);
    let keys = unsafe { [NSForegroundColorAttributeName, NSFontAttributeName] };
    let attrs = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &[&*color as &AnyObject, font as &AnyObject]);
    unsafe { NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &NSString::from_str(text), Some(&attrs)) }
}

/// Selection band: a surface change, nothing else.
pub fn band(mtm: MainThreadMarker, radius: f64) -> Retained<NSBox> {
    let b = NSBox::new(mtm);
    b.setBoxType(NSBoxType::Custom);
    b.setTitlePosition(NSTitlePosition::NoTitle);
    b.setBorderWidth(0.0);
    b.setCornerRadius(radius);
    b.setFillColor(&white(0.12));
    b
}

/// The search capsule's icon and field (`size` pt text, a type-scale step: 24 launcher, 20 clipboard), wired to `controller`.
pub fn search_field(mtm: MainThreadMarker, controller: &Controller, hint: &str, width: f64, size: f64, height: f64) -> (Retained<NSImageView>, Retained<NSTextField>, Retained<NSFont>) {
    let icon = NSImageView::new(mtm);
    if let Some(img) = symbol("magnifyingglass") {
        icon.setImage(Some(&img));
    }
    icon.setContentTintColor(Some(&rgb(SECONDARY)));
    icon.setFrame(NSRect::new(NSPoint::new(20.0, (height - 22.0) / 2.0), NSSize::new(22.0, 22.0)));
    let field = NSTextField::textFieldWithString(ns_string!(""), mtm);
    field.setBezeled(false);
    field.setDrawsBackground(false);
    field.setFocusRingType(NSFocusRingType::None);
    let font = crate::font::regular(size);
    field.setFont(Some(&font));
    field.setTextColor(Some(&rgb(TEXT)));
    field.setPlaceholderAttributedString(Some(&placeholder(hint, &font)));
    unsafe { field.setDelegate(Some(ProtocolObject::from_ref(controller))) };
    let h = size + 8.0;
    field.setFrame(NSRect::new(NSPoint::new(56.0, (height - h) / 2.0), NSSize::new(width - 56.0 - 20.0, h)));
    (icon, field, font)
}

/// `/Users/me/x` → `~/x`; anything outside the home folder unchanged.
pub fn tilde(path: &str) -> String {
    let home = crate::config::home().to_string_lossy().to_string();
    match path.strip_prefix(&home) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// `v` limited to [lo, hi]; never panics when the range is empty (no screen, or a screen narrower than the panel).
pub fn fit(v: f64, lo: f64, hi: f64) -> f64 {
    v.min(hi).max(lo)
}

/// Far outside every screen, for `--offscreen` test copies.
pub const OFFSCREEN: f64 = -30000.0;

define_class!(
    /// A titled window that an `--offscreen` test copy may keep outside every screen (AppKit otherwise pulls
    /// titled windows back on screen whenever they are shown or moved).
    #[unsafe(super(NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeTitledWindow"]
    struct TitledWindow;

    impl TitledWindow {
        #[unsafe(method(constrainFrameRect:toScreen:))]
        fn constrain(&self, frame: NSRect, screen: Option<&NSScreen>) -> NSRect {
            if crate::app::offscreen() {
                return frame;
            }
            unsafe { msg_send![super(self), constrainFrameRect: frame, toScreen: screen] }
        }
    }
);

/// A titled, closable window with its content under the title bar (Settings, Welcome).
pub fn titled_window(mtm: MainThreadMarker, size: NSSize) -> Retained<NSWindow> {
    let this = TitledWindow::alloc(mtm).set_ivars(());
    let window: Retained<TitledWindow> = unsafe {
        msg_send![super(this), initWithContentRect: NSRect::new(NSPoint::new(0.0, 0.0), size),
            styleMask: NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::FullSizeContentView,
            backing: NSBackingStoreType::Buffered, defer: false]
    };
    Retained::into_super(window)
}

/// Show a window and give it the keyboard; an `--offscreen` test copy shows it outside every screen instead,
/// without the focus.
pub fn present(window: &objc2_app_kit::NSWindow) {
    if crate::app::offscreen() {
        // Order first, then move: ordering a titled window front pulls it back onto a screen. Both land in
        // the same frame, so nothing flashes.
        window.orderFrontRegardless();
        window.setFrameOrigin(NSPoint::new(OFFSCREEN, OFFSCREEN));
    } else {
        window.makeKeyAndOrderFront(None);
    }
}

/// Visible frame of the screen that contains `p` (else the main screen).
pub fn visible_frame_at(mtm: MainThreadMarker, p: NSPoint) -> NSRect {
    let screens = NSScreen::screens(mtm);
    let screen = screens.iter().find(|s| {
        let f = s.frame();
        p.x >= f.origin.x && p.x <= f.origin.x + f.size.width && p.y >= f.origin.y && p.y <= f.origin.y + f.size.height
    });
    screen.or_else(|| NSScreen::mainScreen(mtm)).map(|s| s.visibleFrame()).unwrap_or(NSRect::ZERO)
}

/// Height of the primary screen (the one with the menu bar), for flipping Accessibility coordinates.
pub fn primary_height(mtm: MainThreadMarker) -> f64 {
    NSScreen::screens(mtm).iter().next().map(|s| s.frame().size.height).unwrap_or(0.0)
}

/// "Shift+Cmd+C" → "⇧⌘C", "Opt+Space" → "⌥ Space", "Hyper+V" → "Hyper V" (macOS symbol order ⌃⌥⇧⌘).
pub fn shortcut_label(s: &str) -> String {
    let parts: Vec<String> = s.split('+').map(|p| p.trim().to_lowercase()).collect();
    let key = parts.last().cloned().unwrap_or_default();
    let has = |n: &[&str]| parts[..parts.len().saturating_sub(1)].iter().any(|p| n.contains(&p.as_str()));
    let key = if key == "space" { "Space".to_string() } else { key.to_uppercase() };
    if has(&["hyper"]) {
        return format!("Hyper {key}");
    }
    let mut out = String::new();
    for (names, sym) in [(&["ctrl", "control"][..], "⌃"), (&["opt", "option", "alt"][..], "⌥"), (&["shift"][..], "⇧"), (&["cmd", "command"][..], "⌘")] {
        if has(names) {
            out.push_str(sym);
        }
    }
    if key.len() > 1 { format!("{out} {key}") } else { format!("{out}{key}") }
}
