//! A results list that answers the mouse: hovering moves the selection, a click opens the row.
//! Rows are laid out by their owner (launcher or clipboard); this view only turns points into callbacks.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{NSEvent, NSMenu, NSMenuItem, NSResponder, NSTrackingArea, NSTrackingAreaOptions, NSView};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSString};

/// Which panel owns the list.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Launcher,
    Clipboard,
}

pub struct Ivars {
    owner: Cell<Owner>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeList"]
    #[ivars = Ivars]
    pub struct List;

    impl List {
        // A click in a non-key or non-activating panel still counts on the first press.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let p = self.local(event);
            dispatch(self.ivars().owner.get(), p, true);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            let p = self.local(event);
            // Ask the owner first and let its borrow end: the menu runs its own event loop, and its action
            // needs the owner again.
            let mut items = Vec::new();
            match self.ivars().owner.get() {
                Owner::Launcher => crate::launcher::with(|l| items = l.menu_items(p.y)),
                Owner::Clipboard => crate::clipboard::with(|c| items = c.menu_items(p.y)),
            }
            if items.is_empty() {
                return;
            }
            let mtm = MainThreadMarker::new().unwrap();
            let target = MENU_TARGET.with(|t| t.get_or_init(|| MenuTarget::new(mtm)).clone());
            let menu = NSMenu::new(mtm);
            for (title, tag) in items {
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), Some(sel!(rowAction:)), &NSString::from_str(""))
                };
                unsafe { item.setTarget(Some(&target)) };
                item.setTag(tag);
                menu.addItem(&item);
            }
            NSMenu::popUpContextMenu_withEvent_forView(&menu, event, self);
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let p = self.local(event);
            dispatch(self.ivars().owner.get(), p, false);
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            for area in self.trackingAreas().iter() {
                self.removeTrackingArea(&area);
            }
            let options = NSTrackingAreaOptions::MouseMoved | NSTrackingAreaOptions::ActiveAlways | NSTrackingAreaOptions::InVisibleRect;
            let area = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(NSTrackingArea::alloc(), NSRect::ZERO, options, Some(self), None)
            };
            self.addTrackingArea(&area);
            let _: () = unsafe { msg_send![super(self), updateTrackingAreas] };
        }
    }
);

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeRowMenuTarget"]
    struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        /// Tags 1-9 belong to the clipboard, 11-19 to the launcher.
        #[unsafe(method(rowAction:))]
        fn row_action(&self, sender: Option<&AnyObject>) {
            let tag = sender.and_then(|s| s.downcast_ref::<NSMenuItem>()).map(|i| i.tag()).unwrap_or(0);
            match tag {
                1..=9 => crate::clipboard::with(|c| c.menu_action(tag)),
                11..=19 => crate::launcher::with(|l| l.menu_action(tag)),
                _ => {}
            }
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

thread_local! {
    static MENU_TARGET: std::cell::OnceCell<Retained<MenuTarget>> = const { std::cell::OnceCell::new() };
}

impl List {
    pub fn new(mtm: MainThreadMarker, owner: Owner) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { owner: Cell::new(owner) });
        unsafe { msg_send![super(this), init] }
    }

    fn local(&self, event: &NSEvent) -> NSPoint {
        self.convertPoint_fromView(event.locationInWindow(), None)
    }
}

fn dispatch(owner: Owner, p: NSPoint, click: bool) {
    match owner {
        Owner::Launcher => crate::launcher::with(|l| l.pointer(p.y, click)),
        Owner::Clipboard => crate::clipboard::with(|c| c.pointer(p.y, click)),
    }
}
