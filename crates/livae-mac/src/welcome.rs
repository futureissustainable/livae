//! First run: what the two shortcuts are, the one permission paste needs, open at login. One primary action.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, MainThreadOnly};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSApplication, NSButton, NSColor,
    NSImage, NSImageView, NSTextAlignment, NSTextField, NSWindow, NSWindowTitleVisibility,
};
use objc2_foundation::{ns_string, MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use crate::config;
use crate::glass::Glass;
use crate::ui::{self, SECONDARY, TEXT};

const W: f64 = 480.0;
const H: f64 = 484.0;
const PAD: f64 = 32.0;

struct Ui {
    window: Retained<NSWindow>,
    allow: Retained<NSButton>,
    allowed: Retained<NSTextField>,
    folders: Retained<NSButton>,
    folders_ok: Retained<NSTextField>,
    _glass: Glass,
    _target: Retained<Target>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeWelcomeTarget"]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        #[unsafe(method(allow:))]
        fn allow(&self, _s: Option<&AnyObject>) {
            crate::access::request();
        }

        #[unsafe(method(folders:))]
        fn folders(&self, _s: Option<&AnyObject>) {
            crate::access::allow_folders(refresh);
        }

        #[unsafe(method(settings:))]
        fn settings(&self, _s: Option<&AnyObject>) {
            close();
            crate::settings::show();
        }

        #[unsafe(method(start:))]
        fn start(&self, _s: Option<&AnyObject>) {
            close();
            crate::launcher::toggle();
        }
    }
);

fn close() {
    UI.with(|u| {
        if let Some(ui) = u.borrow().as_ref() {
            crate::motion::hide(&ui.window, crate::motion::SHRINK);
        }
    });
}

/// Called every few seconds while open, so "Allow…" turns into "Allowed" after the user grants it.
pub fn refresh() {
    UI.with(|u| {
        if let Some(ui) = u.borrow().as_ref() {
            let ok = crate::access::trusted();
            ui.allow.setHidden(ok);
            ui.allowed.setHidden(!ok);
            let open = crate::access::folders_allowed();
            ui.folders.setHidden(open);
            ui.folders_ok.setHidden(!open);
        }
    });
}

fn line(mtm: MainThreadMarker, v: &objc2_app_kit::NSView, y: f64, title: &str, body: &str) {
    let t = ui::label(mtm, 16.0, TEXT);
    t.setFont(Some(&crate::font::sans(16.0, unsafe { objc2_app_kit::NSFontWeightMedium })));
    t.setStringValue(&NSString::from_str(title));
    t.setFrame(NSRect::new(NSPoint::new(PAD, y + 22.0), NSSize::new(250.0, 22.0)));
    v.addSubview(&t);
    let b = ui::label(mtm, 14.0, SECONDARY);
    b.setStringValue(&NSString::from_str(body));
    b.setFrame(NSRect::new(NSPoint::new(PAD, y + 2.0), NSSize::new(W - 2.0 * PAD - 130.0, 18.0)));
    v.addSubview(&b);
}

fn chip(mtm: MainThreadMarker, v: &objc2_app_kit::NSView, y: f64, text: &str) {
    // Same style as the row's own subtitle: the shortcut is a fact about the row, not a control.
    let c = ui::label(mtm, 14.0, SECONDARY);
    c.setStringValue(&NSString::from_str(text));
    c.setAlignment(NSTextAlignment::Right);
    c.setFrame(NSRect::new(NSPoint::new(W - PAD - 150.0, y + 12.0), NSSize::new(150.0, 20.0)));
    v.addSubview(&c);
}

pub fn show() {
    let mtm = MainThreadMarker::new().unwrap();
    if UI.with(|u| u.borrow().is_none()) {
        build(mtm);
    }
    refresh();
    UI.with(|u| {
        if let Some(ui) = u.borrow().as_ref() {
            ui.window.center();
            crate::ui::present(&ui.window);
            crate::motion::show(&ui.window, crate::motion::RISE);
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
    let window = crate::ui::titled_window(mtm, NSSize::new(W, H));
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(ns_string!("Welcome to Livae"));
    window.setTitlebarAppearsTransparent(true);
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setMovableByWindowBackground(true);
    window.setAppearance(NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }).as_deref());
    let glass = Glass::new(mtm, 0.0, s.glass.max(0.85));
    window.setContentView(Some(&glass.root));
    let v = &glass.content;

    let icon = NSImageView::new(mtm);
    if let Some(img) = NSImage::imageNamed(ns_string!("Livae")) {
        icon.setImage(Some(&img));
    }
    icon.setFrame(NSRect::new(NSPoint::new(PAD - 6.0, H - 104.0), NSSize::new(64.0, 64.0)));
    v.addSubview(&icon);
    let title = ui::label(mtm, 28.0, TEXT);
    title.setFont(Some(&crate::font::sans(28.0, unsafe { objc2_app_kit::NSFontWeightSemibold })));
    title.setStringValue(ns_string!("Livae"));
    // Centred on the icon; the rows below say what Livae does, so no tagline.
    title.setFrame(NSRect::new(NSPoint::new(PAD + 70.0, H - 89.0), NSSize::new(250.0, 34.0)));
    v.addSubview(&title);

    let mut y = H - 176.0;
    line(mtm, v, y, "Search", "Apps, files, maths and units");
    chip(mtm, v, y, &ui::shortcut_label(&s.launcher));
    y -= 64.0;
    line(mtm, v, y, "Clipboard", "Everything you copied, at your cursor");
    chip(mtm, v, y, &ui::shortcut_label(&s.clipboard));
    y -= 64.0;
    line(mtm, v, y, "Paste into apps", "macOS asks for Accessibility once");
    let allow = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Allow…"), tgt, Some(sel!(allow:)), mtm) };
    allow.setFrame(NSRect::new(NSPoint::new(W - PAD - 100.0, y + 8.0), NSSize::new(100.0, 28.0)));
    v.addSubview(&allow);
    let allowed = ui::label(mtm, 14.0, SECONDARY);
    allowed.setStringValue(ns_string!("Allowed"));
    allowed.setAlignment(NSTextAlignment::Right);
    allowed.setFrame(NSRect::new(NSPoint::new(W - PAD - 100.0, y + 12.0), NSSize::new(100.0, 20.0)));
    v.addSubview(&allowed);

    y -= 64.0;
    line(mtm, v, y, "Search your files", "Desktop, Documents and Downloads");
    let folders = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Allow…"), tgt, Some(sel!(folders:)), mtm) };
    folders.setFrame(NSRect::new(NSPoint::new(W - PAD - 100.0, y + 8.0), NSSize::new(100.0, 28.0)));
    v.addSubview(&folders);
    let folders_ok = ui::label(mtm, 14.0, SECONDARY);
    folders_ok.setStringValue(ns_string!("Allowed"));
    folders_ok.setAlignment(NSTextAlignment::Right);
    folders_ok.setFrame(NSRect::new(NSPoint::new(W - PAD - 100.0, y + 12.0), NSSize::new(100.0, 20.0)));
    v.addSubview(&folders_ok);

    let change = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Change shortcuts…"), tgt, Some(sel!(settings:)), mtm) };
    change.setFrame(NSRect::new(NSPoint::new(PAD - 6.0, 28.0), NSSize::new(170.0, 32.0)));
    v.addSubview(&change);
    let start = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Start"), tgt, Some(sel!(start:)), mtm) };
    start.setKeyEquivalent(ns_string!("\r"));
    start.setFrame(NSRect::new(NSPoint::new(W - PAD - 110.0, 28.0), NSSize::new(110.0, 32.0)));
    v.addSubview(&start);

    UI.with(|u| *u.borrow_mut() = Some(Ui { window, allow, allowed, folders, folders_ok, _glass: glass, _target: target }));
}
