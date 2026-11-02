//! A results list that answers the mouse: hovering moves the selection, a click opens the row, and a launcher row
//! can be dragged out (an app as a shortcut, a file as Finder drags it; the Trash deletes either).
//! Rows are laid out by their owner (launcher or clipboard); this view only turns points into callbacks.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession, NSDraggingSource, NSEvent, NSMenu, NSMenuItem,
    NSResponder, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWorkspace,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSDictionary, NSError, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL};

/// Which panel owns the list.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Launcher,
    Clipboard,
}

/// A row dragged out of the panel.
pub struct Drag {
    pub path: PathBuf,
    /// An app: Finder and the Desktop get a shortcut (an alias), never the app itself.
    pub app: bool,
}

pub struct Ivars {
    owner: Cell<Owner>,
    /// Scrolled points not yet a whole row (trackpads send small, smooth steps).
    scrolled: Cell<f64>,
    /// Where the button went down, until it comes up (a click) or the row is dragged away.
    press: Cell<Option<NSPoint>>,
    dragging: RefCell<Option<Drag>>,
}

/// Points the pointer moves with the button down before a press becomes a drag.
const DRAG_START: f64 = 4.0;

/// A row's height for scrolling: one row per this many points of scroll.
const ROW: f64 = 44.0;

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
            self.ivars().press.set(Some(self.local(event)));
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            if let Some(p) = self.ivars().press.take() {
                dispatch(self.ivars().owner.get(), p, true);
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some(from) = self.ivars().press.get() else { return };
            let p = self.local(event);
            if self.ivars().owner.get() != Owner::Launcher || (p.x - from.x).hypot(p.y - from.y) < DRAG_START {
                return;
            }
            let mut drag = None;
            crate::launcher::with(|l| drag = l.drag(from.y));
            // A row that cannot be dragged still opens when the button comes up.
            let Some(drag) = drag else { return };
            self.ivars().press.set(None);
            let path = NSString::from_str(&drag.path.to_string_lossy());
            let url = NSURL::fileURLWithPath(&path);
            let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), ProtocolObject::from_ref(&*url));
            let icon = NSWorkspace::sharedWorkspace().iconForFile(&path);
            let size = 40.0;
            unsafe { item.setDraggingFrame_contents(NSRect::new(NSPoint::new(p.x - size / 2.0, p.y - size / 2.0), NSSize::new(size, size)), Some(&icon)) };
            *self.ivars().dragging.borrow_mut() = Some(drag);
            let session = self.beginDraggingSessionWithItems_event_source(&NSArray::from_retained_slice(&[item]), event, ProtocolObject::from_ref(self));
            // The panel steps aside so the Desktop and the Dock under it can take the drop; a cancelled drag has
            // nowhere to fly back to.
            session.setAnimatesToStartingPositionsOnCancelOrFail(false);
            crate::launcher::with(|l| l.hide());
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

        /// The mouse wheel or a trackpad moves the list by whole rows.
        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            // A wheel sends lines, a trackpad points; either way, one row per ROW points. Up shows earlier rows.
            let dy = if event.hasPreciseScrollingDeltas() { event.scrollingDeltaY() } else { event.scrollingDeltaY() * ROW };
            let total = self.ivars().scrolled.get() + dy;
            let rows = (total / ROW).trunc();
            self.ivars().scrolled.set(total - rows * ROW);
            if rows != 0.0 {
                let by = -rows as isize;
                match self.ivars().owner.get() {
                    Owner::Launcher => crate::launcher::with(|l| l.scroll(by)),
                    Owner::Clipboard => crate::clipboard::with(|c| c.scroll(by)),
                }
            }
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

    unsafe impl NSObjectProtocol for List {}

    unsafe impl NSDraggingSource for List {
        /// Outside Livae: an app only as a shortcut, a file as Finder drags it; either can go to the Trash.
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_mask(&self, _session: &NSDraggingSession, context: NSDraggingContext) -> NSDragOperation {
            match &*self.ivars().dragging.borrow() {
                Some(d) if context == NSDraggingContext::OutsideApplication => mask(d.app),
                _ => NSDragOperation::None,
            }
        }

        /// The Dock's Trash only answers Delete; moving the item there is the source's job.
        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn ended(&self, _session: &NSDraggingSession, at: NSPoint, operation: NSDragOperation) {
            let Some(drag) = self.ivars().dragging.take() else { return };
            if operation == NSDragOperation::Delete {
                trash(&drag.path, at);
            }
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
        let this = Self::alloc(mtm).set_ivars(Ivars {
            owner: Cell::new(owner),
            scrolled: Cell::new(0.0),
            press: Cell::new(None),
            dragging: RefCell::new(None),
        });
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

/// What a drag out of Livae allows: an app becomes a shortcut wherever it lands (Finder makes an alias, never moves
/// or copies the app); a file moves, copies or becomes an alias by Finder's own rules and modifier keys. Both can go
/// to the Trash.
fn mask(app: bool) -> NSDragOperation {
    if app {
        NSDragOperation::Link | NSDragOperation::Delete
    } else {
        NSDragOperation::Copy | NSDragOperation::Link | NSDragOperation::Generic | NSDragOperation::Move | NSDragOperation::Delete
    }
}

/// Move `path` to the Trash as Finder does (Put Back works); a refusal (a system app) shows where it was dropped.
fn trash(path: &std::path::Path, at: NSPoint) {
    let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let done = block2::RcBlock::new(move |_moved: NonNull<NSDictionary<NSURL, NSURL>>, error: *mut NSError| {
        if crate::app::test_mode() {
            crate::system::log_test(&format!("trash {name} {}", if error.is_null() { "done" } else { "refused" }));
        }
        if !error.is_null() {
            crate::hud::show(&format!("{name} can't go to the Trash"), at);
        }
    });
    NSWorkspace::sharedWorkspace().recycleURLs_completionHandler(&NSArray::from_retained_slice(&[url]), Some(&done));
}

