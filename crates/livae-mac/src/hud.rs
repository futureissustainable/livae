//! A short confirmation ("Copied", "Copied. Press ⌘V to paste") in a small glass capsule that fades by itself.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSBackingStoreType, NSColor, NSPanel, NSStatusWindowLevel,
    NSTextAlignment, NSTextField, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

use crate::glass::Glass;
use crate::ui::{self, TEXT};

const H: f64 = 40.0;

struct Hud {
    panel: Retained<NSPanel>,
    glass: Glass,
    label: Retained<NSTextField>,
    shown: u64,
}

thread_local! {
    static HUD: RefCell<Option<Hud>> = const { RefCell::new(None) };
}

fn build(mtm: MainThreadMarker) -> Hud {
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(200.0, H));
    let panel = {
        NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(false);
    panel.setIgnoresMouseEvents(true);
    panel.setLevel(NSStatusWindowLevel);
    panel.setCollectionBehavior(NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary);
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setAppearance(NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }).as_deref());
    let glass = Glass::new(mtm, H / 2.0, 1.0);
    let label = ui::label(mtm, 14.0, TEXT);
    label.setFont(Some(&crate::font::sans(14.0, unsafe { objc2_app_kit::NSFontWeightMedium })));
    label.setAlignment(NSTextAlignment::Center);
    glass.content.addSubview(&label);
    panel.setContentView(Some(&glass.root));
    Hud { panel, glass, label, shown: 0 }
}

/// Show `text` centred on `at` (screen point, AppKit coordinates) for about 1.4 s.
pub fn show(text: &str, at: NSPoint) {
    let mtm = MainThreadMarker::new().unwrap();
    let ticket = HUD.with(|h| {
        let mut slot = h.borrow_mut();
        let hud = slot.get_or_insert_with(|| build(mtm));
        hud.label.setStringValue(&NSString::from_str(text));
        hud.label.sizeToFit();
        let w = hud.label.frame().size.width + 40.0;
        hud.label.setFrame(NSRect::new(NSPoint::new(20.0, (H - 18.0) / 2.0), NSSize::new(w - 40.0, 18.0)));
        hud.panel.setFrame_display(NSRect::new(NSPoint::new(at.x - w / 2.0, at.y - H / 2.0), NSSize::new(w, H)), true);
        hud.glass.root.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, H)));
        let was = crate::motion::shown(&hud.panel);
        hud.panel.orderFrontRegardless();
        if !was {
            crate::motion::show(&hud.panel, crate::motion::Kind::Bloom);
        }
        // A confirmation never takes a click from the app below.
        hud.panel.setIgnoresMouseEvents(true);
        hud.shown += 1;
        hud.shown
    });
    let after = dispatch2::DispatchTime::NOW.time(1_400_000_000);
    let _ = dispatch2::DispatchQueue::main().after(after, move || {
        HUD.with(|h| {
            if let Some(hud) = h.borrow().as_ref()
                && hud.shown == ticket {
                    crate::motion::hide(&hud.panel, crate::motion::Kind::Bloom);
                }
        });
    });
}
