//! The floating panel and field controller shared by the launcher and the clipboard.
//!
//! The panel is non-activating, so the app you were in stays in front (and receives the paste). It must still be
//! allowed to become key for typing. An accessory app has no Edit menu, so ⌘A/C/V/X/Z are routed here by hand.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSApplication, NSBackingStoreType, NSColor, NSControl,
    NSControlTextEditingDelegate, NSEvent, NSEventModifierFlags, NSFloatingWindowLevel, NSPanel, NSResponder, NSTextFieldDelegate,
    NSTextView, NSWindow, NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize};

use crate::list::Owner;

/// What the owner is told.
pub enum Event<'a> {
    TextChanged,
    /// Field command (moveUp:, insertNewline:, …). Return true when handled.
    Command(Sel),
    /// ⌘ + key (lower-case). Return true when handled.
    Key(&'a str),
    Resigned,
}

fn route(owner: Owner, event: Event) -> bool {
    let mut handled = false;
    match owner {
        Owner::Launcher => crate::launcher::with(|l| handled = l.event(event)),
        Owner::Clipboard => crate::clipboard::with(|c| handled = c.event(event)),
    }
    handled
}

pub struct Ivars {
    owner: Cell<Owner>,
}

define_class!(
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaePanel"]
    #[ivars = Ivars]
    pub struct Panel;

    impl Panel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            let flags = event.modifierFlags();
            let key = event.charactersIgnoringModifiers().map(|s| s.to_string().to_lowercase()).unwrap_or_default();
            let shift = flags.contains(NSEventModifierFlags::Shift);
            let handled: bool = if !flags.contains(NSEventModifierFlags::Command) {
                false
            } else {
                let edit = match key.as_str() {
                    "a" => Some(sel!(selectAll:)),
                    "c" => Some(sel!(copy:)),
                    "x" => Some(sel!(cut:)),
                    "v" => Some(sel!(paste:)),
                    "z" if shift => Some(sel!(redo:)),
                    "z" => Some(sel!(undo:)),
                    _ => None,
                };
                // The owner gets first say (the clipboard uses ⌘⌫ and ⌘P), then the text-editing keys.
                if route(self.ivars().owner.get(), Event::Key(&key)) {
                    true
                } else if let Some(action) = edit {
                    let app = NSApplication::sharedApplication(self.mtm());
                    unsafe { app.sendAction_to_from(action, None, Some(&**self as &AnyObject)) }
                } else {
                    false
                }
            };
            if handled {
                true
            } else {
                let sup: bool = unsafe { msg_send![super(self), performKeyEquivalent: event] };
                sup
            }
        }
    }
);

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeFieldController"]
    #[ivars = Ivars]
    pub struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSControlTextEditingDelegate for Controller {
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _notification: &NSNotification) {
            route(self.ivars().owner.get(), Event::TextChanged);
        }

        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn do_command(&self, _control: &NSControl, _text_view: &NSTextView, command: Sel) -> bool {
            route(self.ivars().owner.get(), Event::Command(command))
        }
    }

    unsafe impl NSTextFieldDelegate for Controller {}

    unsafe impl NSWindowDelegate for Controller {
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            // An offscreen test copy stays open while the owner clicks around; a real one closes.
            if crate::app::offscreen() {
                return;
            }
            route(self.ivars().owner.get(), Event::Resigned);
        }
    }
);

impl Controller {
    pub fn new(mtm: MainThreadMarker, owner: Owner) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { owner: Cell::new(owner) });
        unsafe { msg_send![super(this), init] }
    }
}

impl Panel {
    pub fn new(mtm: MainThreadMarker, owner: Owner, controller: &Controller) -> Retained<Self> {
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(600.0, 56.0));
        let this = Self::alloc(mtm).set_ivars(Ivars { owner: Cell::new(owner) });
        let panel: Retained<Self> = unsafe {
            msg_send![super(this), initWithContentRect: rect,
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered, defer: false]
        };
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        // No window shadow: it is square and lags behind resizes (the stray corners). The glass brings its own.
        panel.setHasShadow(false);
        panel.setLevel(NSFloatingWindowLevel);
        panel.setCollectionBehavior(NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary);
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setHidesOnDeactivate(false);
        // A see-through window lets clicks fall through to the app behind; take every click (31.10.2026 bug).
        panel.setIgnoresMouseEvents(false);
        panel.setAppearance(NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }).as_deref());
        panel.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(controller)));
        panel
    }
}
