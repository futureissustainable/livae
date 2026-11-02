//! The evening journal: Vesper's prompt in Livae's glass, laid out as a notebook (design D, 31.10.2026): the
//! date and the day's rating small on top, the note large in the middle, the habits and Save in the footer.
//! It asks at the hour set in Settings (checked every 30 s and on wake), snoozes, comes back to the front until
//! today is saved, and edits today's entry. Entries are written by `livae_core::journal` in Vesper's exact
//! format, so a Vesper journal stays one file. While Vesper still runs, Livae never asks on its own;
//! opening it by hand still works.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

use block2::RcBlock;
use livae_core::journal::{self as core, Draft, Entry, Mode, NaiveDate, NaiveDateTime, Schedule, Tracker, DAY_COLORS, DAY_LABELS, MIN_CHARS};
use livae_core::tokens::{self, color::dark};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadOnly, Message};
use objc2_app_kit::{
    NSAccessibility, NSAccessibilityCheckBoxRole, NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSApplication,
    NSAttributedStringNSStringDrawing, NSAutoresizingMaskOptions, NSBackingStoreType, NSBeep, NSBezierPath, NSBorderType, NSButton, NSButtonType,
    NSColor, NSControl, NSControlTextEditingDelegate, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSFloatingWindowLevel,
    NSFocusRingType, NSFont, NSFontAttributeName, NSFontWeightMedium, NSFontWeightRegular, NSForegroundColorAttributeName, NSImageView,
    NSMenu, NSMenuItem, NSMutableParagraphStyle, NSPanel, NSParagraphStyleAttributeName, NSResponder, NSRunningApplication, NSScreen,
    NSScrollView, NSTextAlignment, NSTextDelegate, NSTextField, NSTextFieldDelegate, NSTextInputClient, NSTextView, NSTextViewDelegate,
    NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindow,
    NSWindowCollectionBehavior, NSWindowDelegate, NSWindowOrderingMode, NSWindowStyleMask, NSWorkspace, NSWorkspaceDidWakeNotification,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSAttributedString, NSDictionary, NSNotification, NSNumber, NSObject, NSObjectProtocol, NSPoint, NSRange,
    NSRect, NSSize, NSString, NSTimer,
};

use crate::config::{self, Habit};
use crate::glass::Glass;
use crate::ui::{self, SECONDARY, TERTIARY, TEXT};

const W: f64 = 720.0;
const PAD_TOP: f64 = 36.0;
const PAD_X: f64 = 40.0;
const PAD_BOTTOM: f64 = 28.0;
const INNER: f64 = W - 2.0 * PAD_X;
const PANEL_RADIUS: f64 = 30.0;
/// Top row (date and rating pills), the note, a hairline, the footer.
const TOP_ROW: f64 = 28.0;
const NOTE_GAP: f64 = 26.0;
const NOTE_H: f64 = 190.0;
const NOTE_SIZE: f64 = 24.0;
/// 1.5 × 24.
const NOTE_LINE: f64 = 36.0;
const RULE_GAP: f64 = 20.0;
const FOOTER_GAP: f64 = 18.0;
const BUTTON_H: f64 = 40.0;
const SAVE_W: f64 = 96.0;
const PILL_RADIUS: f64 = 12.0;
/// Space between footer items. Habit chips wrap onto more lines (8 apart) when they do not fit beside Save.
const ITEM_GAP: f64 = 16.0;
/// Between the dismiss text, the counter and Save.
const RIGHT_GAP: f64 = 12.0;
const CHIP_H: f64 = 28.0;
const LINE_GAP: f64 = 8.0;
/// Habit chips keep this room around their content for the focus outline.
const CHIP_MARGIN: f64 = 4.0;
const BOX: f64 = 16.0;
const ADD_W: f64 = 28.0;
const EDIT_W: f64 = 180.0;
/// The hint or error line under the note; the note gives up this height while it shows.
const NOTICE_H: f64 = 24.0;
/// Black over the full-screen blur, so the desktop behind stays quiet on an OLED screen.
const BACKDROP_WASH: f64 = 0.3;
/// A due prompt that lost the keyboard comes back after this many seconds (Vesper: 25).
const REFRONT_S: i64 = 25;
/// Test runs and `--journal` write here, inside `config::support_dir()`, never in the vault.
const TEST_FILE: &str = "journal-test.md";
const ERROR_TEXT: &str = "Could not save. Check the file and retry.";

// Button tags: ratings 0 to 4; 30 dismiss, 31 save, 40 add a habit.
const DISMISS_TAG: isize = 30;
const SAVE_TAG: isize = 31;
const ADD_TAG: isize = 40;

/// For the menu bar: ✷ pending, ✦ done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalState {
    Pending,
    Done,
    Off,
}

#[derive(Default)]
struct Sched {
    tracker: Tracker,
    /// Set by `show_test`: every read and write goes to this file instead.
    test_file: Option<PathBuf>,
    /// The last look at the file: whether that day had an entry.
    done: core::DoneCache,
    listener: Option<fn()>,
}

/// The inline habit name field: adding a new habit, or renaming the one at an index.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Edit {
    Add,
    Rename(usize),
}

/// The line under the note: nothing, what Save still needs, or a failed save.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Notice {
    None,
    Hint,
    Error,
}

struct Ui {
    backdrop: Retained<NSPanel>,
    window: Retained<Prompt>,
    glass: Glass,
    date: Retained<NSTextField>,
    days: Vec<Retained<Pill>>,
    scroll: Retained<NSScrollView>,
    note: Retained<NSTextView>,
    placeholder: Retained<NSTextField>,
    rule: Retained<NSView>,
    chips: RefCell<Vec<Retained<HabitChip>>>,
    add: Retained<Pill>,
    editor: Retained<NSView>,
    field: Retained<NSTextField>,
    count: Retained<NSTextField>,
    count_w: f64,
    notice_text: Retained<NSTextField>,
    notice_icon: Retained<NSImageView>,
    dismiss: Retained<Pill>,
    save: Retained<Pill>,
    mode: Cell<Mode>,
    /// The rating, the habit answers and the entry they edit, until Save.
    draft: RefCell<Draft>,
    /// Each habit's run of Yes days up to yesterday; today's answer is added live.
    streak_base: RefCell<Vec<(String, u32)>>,
    editing: Cell<Option<Edit>>,
    notice: Cell<Notice>,
    saving: Cell<bool>,
    showing: Cell<bool>,
    /// Changes on every open and close, so late callbacks can tell they are stale.
    ticket: Cell<u64>,
    nag: RefCell<Option<Retained<NSTimer>>>,
    _monitor: Option<Retained<AnyObject>>,
}

thread_local! {
    static SCHED: RefCell<Sched> = RefCell::new(Sched::default());
    /// Built once, then only ever borrowed shared, so AppKit callbacks may re-enter safely.
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static TARGET: OnceCell<Retained<Target>> = const { OnceCell::new() };
    static TIMER: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
}

fn with_ui<R>(f: impl FnOnce(&Ui) -> R) -> Option<R> {
    UI.with(|u| u.borrow().as_ref().map(f))
}

fn habits() -> Vec<Habit> {
    config::get().journal.habits
}

fn keys(habits: &[Habit]) -> Vec<String> {
    habits.iter().map(|h| h.key.clone()).collect()
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeJournalTarget"]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl NSTextDelegate for Target {
        #[unsafe(method(textDidChange:))]
        fn text_did_change(&self, _n: &NSNotification) {
            with_ui(|u| u.refresh_state());
        }
    }

    unsafe impl NSTextViewDelegate for Target {}

    unsafe impl NSControlTextEditingDelegate for Target {
        /// The habit name field lost the keyboard without Return or Esc (a click elsewhere): keep a valid name.
        #[unsafe(method(controlTextDidEndEditing:))]
        fn control_text_did_end_editing(&self, _n: &NSNotification) {
            with_ui(|u| u.finish_edit(true, false));
        }
    }

    unsafe impl NSTextFieldDelegate for Target {}

    unsafe impl NSWindowDelegate for Target {
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _n: &NSNotification) {
            resigned();
        }
    }

    impl Target {
        #[unsafe(method(choose:))]
        fn choose(&self, sender: Option<&AnyObject>) {
            let Some(tag) = sender.and_then(|s| s.downcast_ref::<NSControl>()).map(|c| c.tag()) else { return };
            with_ui(|u| {
                if (0..DAY_LABELS.len() as isize).contains(&tag) {
                    u.draft.borrow_mut().day = Some(tag as usize);
                }
                u.refresh();
            });
        }

        #[unsafe(method(dismiss:))]
        fn dismiss_pressed(&self, _s: Option<&AnyObject>) {
            escape();
        }

        #[unsafe(method(save:))]
        fn save_pressed(&self, _s: Option<&AnyObject>) {
            save();
        }

        #[unsafe(method(addHabit:))]
        fn add_habit(&self, _s: Option<&AnyObject>) {
            with_ui(|u| u.begin_edit(Edit::Add));
        }

        #[unsafe(method(renameHabit:))]
        fn rename_habit(&self, sender: Option<&AnyObject>) {
            if let Some(i) = sender.and_then(|s| s.downcast_ref::<NSMenuItem>()).map(|m| m.tag() as usize) {
                with_ui(|u| u.begin_edit(Edit::Rename(i)));
            }
        }

        #[unsafe(method(removeHabit:))]
        fn remove_habit(&self, sender: Option<&AnyObject>) {
            // After the menu closes: removing rebuilds the chips, and the one that opened the menu is still in use.
            if let Some(i) = sender.and_then(|s| s.downcast_ref::<NSMenuItem>()).map(|m| m.tag() as usize) {
                dispatch2::DispatchQueue::main().exec_async(move || {
                    with_ui(|u| u.remove_habit(i));
                });
            }
        }

        #[unsafe(method(wake:))]
        fn wake(&self, _n: &NSNotification) {
            check();
        }
    }
);

fn target(mtm: MainThreadMarker) -> Retained<Target> {
    TARGET.with(|t| {
        t.get_or_init(|| {
            let this = Target::alloc(mtm).set_ivars(());
            unsafe { msg_send![super(this), init] }
        })
        .clone()
    })
}

define_class!(
    /// The prompt. Non-activating like the launcher, so it takes the keyboard without pulling Livae to the
    /// front, and the app you were in has it back when the prompt closes.
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeJournalPanel"]
    struct Prompt;

    impl Prompt {
        /// Never in an offscreen test copy: it must not take the owner's typing.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            !crate::app::offscreen()
        }

        /// An accessory app has no Edit menu: ⌘A/C/V/X/Z are routed to the text by hand, as panel.rs does.
        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            let flags = event.modifierFlags();
            let mut handled = false;
            if flags.contains(NSEventModifierFlags::Command) {
                let key = event.charactersIgnoringModifiers().map(|s| s.to_string().to_lowercase()).unwrap_or_default();
                let edit = match key.as_str() {
                    "a" => Some(sel!(selectAll:)),
                    "c" => Some(sel!(copy:)),
                    "x" => Some(sel!(cut:)),
                    "v" => Some(sel!(paste:)),
                    "z" if flags.contains(NSEventModifierFlags::Shift) => Some(sel!(redo:)),
                    "z" => Some(sel!(undo:)),
                    _ => None,
                };
                if let Some(action) = edit {
                    let app = NSApplication::sharedApplication(self.mtm());
                    handled = unsafe { app.sendAction_to_from(action, None, Some(&**self as &AnyObject)) };
                }
            }
            handled || unsafe { msg_send![super(self), performKeyEquivalent: event] }
        }
    }
);

pub struct PillIvars {
    ring: OnceCell<Retained<NSView>>,
    size: Cell<f64>,
}

define_class!(
    /// A choice or a button: a filled shape with a centred label, reachable with Tab and the arrow keys
    /// (without Full Keyboard Access), and a 1 px focus outline 2 px outside it.
    #[unsafe(super(NSButton, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeJournalChoice"]
    #[ivars = PillIvars]
    struct Pill;

    impl Pill {
        /// A click leaves the keyboard where it was (usually the note); only the keyboard moves the focus here.
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            !clicking(self.mtm())
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _e: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            let ok: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
            if ok {
                self.show_ring(true);
            }
            ok
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            let ok: bool = unsafe { msg_send![super(self), resignFirstResponder] };
            if ok {
                self.show_ring(false);
            }
            ok
        }
    }
);

fn clicking(mtm: MainThreadMarker) -> bool {
    let mouse = [
        NSEventType::LeftMouseDown,
        NSEventType::LeftMouseUp,
        NSEventType::LeftMouseDragged,
        NSEventType::RightMouseDown,
        NSEventType::RightMouseUp,
        NSEventType::OtherMouseDown,
        NSEventType::OtherMouseUp,
    ];
    NSApplication::sharedApplication(mtm).currentEvent().is_some_and(|e| mouse.contains(&e.r#type()))
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn outset(r: NSRect, by: f64) -> NSRect {
    rect(r.origin.x - by, r.origin.y - by, r.size.width + 2.0 * by, r.size.height + 2.0 * by)
}

impl Pill {
    fn new(mtm: MainThreadMarker, parent: &NSView, frame: NSRect, radius: f64, tag: isize, action: Sel) -> Retained<Self> {
        let ring = NSView::initWithFrame(NSView::alloc(mtm), outset(frame, 3.0));
        ring.setWantsLayer(true);
        if let Some(l) = ring.layer() {
            l.setCornerRadius(radius + 3.0);
            l.setBorderWidth(1.0);
            l.setBorderColor(Some(&ui::rgb(dark::FOCUS).CGColor()));
        }
        ring.setHidden(true);
        parent.addSubview(&ring);

        let this = Self::alloc(mtm).set_ivars(PillIvars { ring: OnceCell::new(), size: Cell::new(tokens::text::LABEL.size) });
        let b: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        let _ = b.ivars().ring.set(ring);
        b.setButtonType(NSButtonType::MomentaryChange);
        b.setBordered(false);
        b.setTag(tag);
        let t = target(mtm);
        let t: &AnyObject = &t;
        unsafe {
            b.setTarget(Some(t));
            b.setAction(Some(action));
        }
        b.setWantsLayer(true);
        if let Some(l) = b.layer() {
            l.setCornerRadius(radius);
        }
        parent.addSubview(&b);
        b
    }

    fn place(&self, frame: NSRect) {
        self.setFrame(frame);
        if let Some(r) = self.ivars().ring.get() {
            r.setFrame(outset(frame, 3.0));
        }
    }

    fn show_ring(&self, on: bool) {
        if let Some(r) = self.ivars().ring.get() {
            r.setHidden(!on);
        }
    }

    fn paint(&self, title: &str, fill: Option<&NSColor>, edge: Option<&NSColor>, text: &NSColor) {
        if let Some(l) = self.layer() {
            l.setBackgroundColor(fill.map(|c| c.CGColor()).as_deref());
            l.setBorderWidth(if edge.is_some() { 1.0 } else { 0.0 });
            if let Some(e) = edge {
                l.setBorderColor(Some(&e.CGColor()));
            }
        }
        // A momentary-change button shows its alternate title while pressed; keep the label.
        let t = styled(title, &medium(self.ivars().size.get()), text, true);
        self.setAttributedTitle(&t);
        self.setAttributedAlternateTitle(&t);
    }
}

pub struct ChipIvars {
    index: Cell<usize>,
    label: RefCell<String>,
    answer: Cell<Option<bool>>,
    streak: Cell<u32>,
    focused: Cell<bool>,
}

define_class!(
    /// A habit in the footer: a check box (dashed while unanswered, filled for Yes, a minus for No), its name,
    /// and the current streak. A click or Space moves unanswered → Yes → No → Yes; right-click renames or removes.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeJournalHabit"]
    #[ivars = ChipIvars]
    struct HabitChip;

    impl HabitChip {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            !clicking(self.mtm())
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _e: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            let ok: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
            self.ivars().focused.set(ok);
            self.setNeedsDisplay(true);
            ok
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            let ok: bool = unsafe { msg_send![super(self), resignFirstResponder] };
            if ok {
                self.ivars().focused.set(false);
                self.setNeedsDisplay(true);
            }
            ok
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if event.modifierFlags().contains(NSEventModifierFlags::Control) {
                self.context_menu(event);
            } else {
                toggle_habit(self.ivars().index.get());
            }
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            self.context_menu(event);
        }

        #[unsafe(method(accessibilityPerformPress))]
        fn accessibility_perform_press(&self) -> bool {
            toggle_habit(self.ivars().index.get());
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }
    }
);

impl HabitChip {
    fn new(mtm: MainThreadMarker, index: usize, parent: &NSView) -> Retained<Self> {
        let ivars = ChipIvars {
            index: Cell::new(index),
            label: RefCell::new(String::new()),
            answer: Cell::new(None),
            streak: Cell::new(0),
            focused: Cell::new(false),
        };
        let this = Self::alloc(mtm).set_ivars(ivars);
        let c: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        c.setAccessibilityElement(true);
        c.setAccessibilityRole(Some(unsafe { NSAccessibilityCheckBoxRole }));
        parent.addSubview(&c);
        c
    }

    fn update(&self, label: &str, answer: Option<bool>, streak: u32) {
        let iv = self.ivars();
        *iv.label.borrow_mut() = label.to_string();
        iv.answer.set(answer);
        iv.streak.set(streak);
        let spoken = if streak >= 2 { format!("{label}, {streak} days") } else { label.to_string() };
        self.setAccessibilityLabel(Some(&NSString::from_str(&spoken)));
        // Checkbox values: 1 Yes, 0 No, 2 not answered yet.
        let value = NSNumber::new_i32(match answer {
            Some(true) => 1,
            Some(false) => 0,
            None => 2,
        });
        unsafe { self.setAccessibilityValue(Some(&value)) };
        self.setNeedsDisplay(true);
    }

    fn texts(&self) -> (Retained<NSAttributedString>, Option<Retained<NSAttributedString>>) {
        let iv = self.ivars();
        let font = regular(tokens::text::LABEL.size);
        let color = ui::rgb(if iv.answer.get() == Some(true) { TEXT } else { SECONDARY });
        let label = styled(&iv.label.borrow(), &font, &color, false);
        let n = iv.streak.get();
        let streak = (n >= 2).then(|| styled(&format!("{n} days"), &font, &ui::rgb(TERTIARY), false));
        (label, streak)
    }

    /// Box, gap, name, and the streak when there is one; the frame adds the focus margin on both sides.
    fn content_width(&self) -> f64 {
        let (label, streak) = self.texts();
        BOX + 8.0 + label.size().width.ceil() + streak.map_or(0.0, |s| 6.0 + s.size().width.ceil())
    }

    fn draw(&self) {
        let iv = self.ivars();
        let b = self.bounds();
        let h = b.size.height;
        if iv.focused.get() {
            let ring = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(outset(b, -0.5), 8.0, 8.0);
            ring.setLineWidth(1.0);
            ui::rgb(dark::FOCUS).setStroke();
            ring.stroke();
        }
        let top = ((h - BOX) / 2.0).round();
        let outline = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect(CHIP_MARGIN + 0.75, top + 0.75, BOX - 1.5, BOX - 1.5), 4.25, 4.25);
        outline.setLineWidth(1.5);
        match iv.answer.get() {
            Some(true) => {
                ui::rgb(TEXT).setFill();
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect(CHIP_MARGIN, top, BOX, BOX), 5.0, 5.0).fill();
            }
            Some(false) => {
                ui::rgb(dark::CONTROL_LINE).setStroke();
                outline.stroke();
                let minus = NSBezierPath::bezierPath();
                minus.moveToPoint(NSPoint::new(CHIP_MARGIN + 4.5, top + BOX / 2.0));
                minus.lineToPoint(NSPoint::new(CHIP_MARGIN + BOX - 4.5, top + BOX / 2.0));
                minus.setLineWidth(1.5);
                ui::rgb(SECONDARY).setStroke();
                minus.stroke();
            }
            None => {
                let dash = [2.5_f64, 2.0];
                unsafe { outline.setLineDash_count_phase(dash.as_ptr(), 2, 0.0) };
                ui::rgb(dark::CONTROL_LINE).setStroke();
                outline.stroke();
            }
        }
        let (label, streak) = self.texts();
        let x = CHIP_MARGIN + BOX + 8.0;
        let size = label.size();
        label.drawAtPoint(NSPoint::new(x, ((h - size.height) / 2.0).round()));
        if let Some(s) = streak {
            let ss = s.size();
            s.drawAtPoint(NSPoint::new(x + size.width.ceil() + 6.0, ((h - ss.height) / 2.0).round()));
        }
    }

    fn context_menu(&self, event: &NSEvent) {
        let mtm = self.mtm();
        let menu = NSMenu::new(mtm);
        let t = target(mtm);
        let t: &AnyObject = &t;
        for (title, action) in [("Rename…", sel!(renameHabit:)), ("Remove", sel!(removeHabit:))] {
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), Some(action), ns_string!(""))
            };
            unsafe { item.setTarget(Some(t)) };
            item.setTag(self.ivars().index.get() as isize);
            menu.addItem(&item);
        }
        NSMenu::popUpContextMenu_withEvent_forView(&menu, event, self);
    }
}

fn toggle_habit(index: usize) {
    with_ui(|u| u.toggle(index));
}

fn medium(size: f64) -> Retained<NSFont> {
    crate::font::sans(size, unsafe { NSFontWeightMedium })
}

fn regular(size: f64) -> Retained<NSFont> {
    crate::font::regular(size)
}

fn styled(text: &str, font: &NSFont, color: &NSColor, centred: bool) -> Retained<NSAttributedString> {
    let para = NSMutableParagraphStyle::new();
    if centred {
        para.setAlignment(NSTextAlignment::Center);
    }
    let keys = unsafe { [NSForegroundColorAttributeName, NSFontAttributeName, NSParagraphStyleAttributeName] };
    let values: [&AnyObject; 3] = [color, font, &para];
    let attrs = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &values);
    unsafe { NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &NSString::from_str(text), Some(&attrs)) }
}

fn day_color(i: usize) -> Retained<NSColor> {
    let (r, g, b) = DAY_COLORS[i];
    NSColor::colorWithSRGBRed_green_blue_alpha(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0, 1.0)
}

/// The note's lines: 24 regular, 1.5 apart (the space a line lacks to 36 is added under it).
fn note_paragraph() -> Retained<NSMutableParagraphStyle> {
    let font = regular(NOTE_SIZE);
    let para = NSMutableParagraphStyle::new();
    para.setLineSpacing((NOTE_LINE - (font.ascender() - font.descender() + font.leading())).max(0.0));
    para
}

fn note_attributes() -> Retained<NSDictionary<NSString, AnyObject>> {
    let font = regular(NOTE_SIZE);
    let para = note_paragraph();
    let color = ui::rgb(TEXT);
    let keys = unsafe { [NSForegroundColorAttributeName, NSFontAttributeName, NSParagraphStyleAttributeName] };
    let values: [&AnyObject; 3] = [&color, &font, &para];
    NSDictionary::<NSString, AnyObject>::from_slices(&keys, &values)
}

fn same(a: &AnyObject, b: &AnyObject) -> bool {
    std::ptr::eq(a, b)
}

/// What a key press in the prompt does.
enum Key {
    Escape,
    Save,
    Tab(bool),
    Press(Retained<Pill>),
    Step(Retained<Pill>, isize),
    Toggle(usize),
    FinishEdit(bool),
}

impl Ui {
    fn answer(&self, key: &str) -> Option<bool> {
        self.draft.borrow().answer(key)
    }

    fn entry(&self) -> Entry {
        self.draft.borrow().entry(self.note.string().to_string())
    }

    fn set_note(&self, text: &str) {
        self.note.setString(&NSString::from_str(text));
        if let Some(storage) = unsafe { self.note.textStorage() } {
            unsafe { storage.setAttributes_range(Some(&note_attributes()), NSRange::new(0, storage.length())) };
        }
        if let Some(undo) = self.note.undoManager() {
            undo.removeAllActions();
        }
    }

    /// Fill the form for `mode`: today's entry when editing, the kept draft after a snooze, else empty.
    fn prepare(&self, mode: Mode, now: NaiveDateTime) {
        let today = now.date();
        self.mode.set(mode);
        self.saving.set(false);
        self.editing.set(None);
        self.notice.set(Notice::None);
        let keys = keys(&habits());
        let saved = (mode == Mode::Edit).then(|| core::parse(&file(), today).ok().flatten().unwrap_or_default());
        let note = self.draft.borrow_mut().prepare(saved, today, &keys);
        if let Some(note) = note {
            self.set_note(&note);
        }
        *self.streak_base.borrow_mut() = core::streaks_before(&file(), &keys, today);
        self.date.setStringValue(&NSString::from_str(&core::date_label(today)));

        let title = match mode {
            Mode::Due => format!("Snooze {} min", config::get().journal.snooze_min.max(1)),
            Mode::Early => "Close".into(),
            Mode::Edit => "Cancel".into(),
        };
        // A quiet text button; its 8 pt sides belong to the gap around it.
        let secondary = ui::rgb(SECONDARY);
        let width = styled(&title, &medium(tokens::text::LABEL.size), &secondary, true).size().width.ceil() + 16.0;
        self.dismiss.place(rect(0.0, 0.0, width, BUTTON_H));
        self.dismiss.paint(&title, None, None, &secondary);
        self.rebuild_chips();
        self.refresh();
    }

    /// One chip per habit in Settings, in its order.
    fn rebuild_chips(&self) {
        let mtm = MainThreadMarker::new().unwrap();
        let mut chips = self.chips.borrow_mut();
        for c in chips.drain(..) {
            c.removeFromSuperview();
        }
        let parent: &NSView = &self.glass.content;
        *chips = (0..habits().len()).map(|i| HabitChip::new(mtm, i, parent)).collect();
    }

    fn refresh(&self) {
        let text = ui::rgb(dark::CANVAS);
        let secondary = ui::rgb(SECONDARY);
        let rest = ui::white(0.06);
        for (i, p) in self.days.iter().enumerate() {
            let on = self.draft.borrow().day == Some(i);
            if on {
                p.paint(DAY_LABELS[i], Some(&day_color(i)), None, &text);
            } else {
                p.paint(DAY_LABELS[i], Some(&rest), None, &secondary);
            }
            p.setAccessibilitySelected(on);
        }
        self.refresh_chips();
        self.refresh_state();
    }

    /// Each chip's answer and streak: the run up to yesterday, plus today when Yes, ended by No.
    fn refresh_chips(&self) {
        let habits = habits();
        for (c, h) in self.chips.borrow().iter().zip(&habits) {
            let answer = self.answer(&h.key);
            let base = self.streak_base.borrow().iter().find(|(k, _)| *k == h.key).map_or(0, |(_, n)| *n);
            c.update(&h.label, answer, core::streak_with(base, answer));
        }
        self.layout();
    }

    /// The counter, the placeholder, the live hint and Save; runs on every keystroke.
    fn refresh_state(&self) {
        let entry = self.entry();
        let keys = keys(&habits());
        let n = entry.note_len();
        self.count.setStringValue(&NSString::from_str(&format!("{n} / {MIN_CHARS}")));
        self.count.setTextColor(Some(&ui::rgb(if n >= MIN_CHARS { TEXT } else { SECONDARY })));
        self.placeholder.setHidden(!entry.note.is_empty());
        if self.notice.get() == Notice::Hint {
            match core::missing(&entry, &keys) {
                Some(hint) => self.notice_text.setStringValue(&NSString::from_str(hint)),
                None => self.set_notice(Notice::None, None),
            }
        }
        let saving = self.saving.get();
        let ok = entry.is_complete(&keys) && !saving;
        // Always readable: an outline until the form is complete (a press says what is missing), then the
        // light primary button.
        self.save.setEnabled(!saving);
        let title = if saving { "Saving…" } else { "Save" };
        if ok {
            self.save.paint(title, Some(&ui::rgb(dark::ACTION)), None, &ui::rgb(dark::ACTION_TEXT));
        } else {
            self.save.paint(title, None, Some(&ui::white(0.16)), &ui::rgb(SECONDARY));
        }
    }

    /// The line under the note: a hint (what Save still needs) or a failed save with its mark and, as a
    /// tooltip, the path and the reason.
    fn set_notice(&self, notice: Notice, text: Option<&str>) {
        self.notice.set(notice);
        match notice {
            Notice::None => self.notice_text.setToolTip(None),
            Notice::Hint => self.notice_text.setToolTip(None),
            Notice::Error => self.notice_text.setToolTip(text.map(NSString::from_str).as_deref()),
        }
        let shown = match notice {
            Notice::Error => ERROR_TEXT,
            _ => text.unwrap_or(""),
        };
        self.notice_text.setStringValue(&NSString::from_str(shown));
        self.layout();
    }

    fn toggle(&self, index: usize) {
        let Some(h) = habits().into_iter().nth(index) else { return };
        self.draft.borrow_mut().toggle(&h.key);
        self.refresh_chips();
        self.refresh_state();
    }

    fn begin_edit(&self, edit: Edit) {
        let habits = habits();
        let text = match edit {
            Edit::Add => String::new(),
            Edit::Rename(i) => match habits.get(i) {
                Some(h) => h.label.clone(),
                None => return,
            },
        };
        self.editing.set(Some(edit));
        self.field.setStringValue(&NSString::from_str(&text));
        self.layout();
        unsafe { self.field.selectText(None) };
    }

    /// Return confirms (`strict`: an invalid name beeps and stays), Esc cancels, leaving the field keeps a
    /// valid name. Habits change in Settings at once.
    fn finish_edit(&self, confirm: bool, strict: bool) {
        let Some(edit) = self.editing.get() else { return };
        let habits = habits();
        let typed = self.field.stringValue().to_string();
        let renaming = match edit {
            Edit::Add => None,
            Edit::Rename(i) => Some(i),
        };
        let name = if confirm { core::habit_name_for(&typed, &habits, renaming) } else { None };
        if confirm && strict && name.is_none() && !typed.trim().is_empty() {
            NSBeep();
            return;
        }
        self.editing.set(None);
        match (edit, name) {
            (Edit::Add, Some(name)) => {
                config::update(|s| s.journal.habits.push(Habit { label: name.clone(), key: name.clone() }));
                self.draft.borrow_mut().add_habit(&name);
                let today = core::now().date();
                self.streak_base.borrow_mut().extend(core::streaks_before(&file(), &[name], today));
                self.rebuild_chips();
            }
            (Edit::Rename(i), Some(name)) => config::update(|s| {
                if let Some(h) = s.journal.habits.get_mut(i) {
                    h.label = name;
                }
            }),
            _ => {}
        }
        self.refresh();
        if strict {
            let note: &NSResponder = &self.note;
            self.window.makeFirstResponder(Some(note));
        }
    }

    /// Hidden from new days only: past entries keep their lines, and an entry already holding it keeps it.
    fn remove_habit(&self, index: usize) {
        let Some(h) = habits().into_iter().nth(index) else { return };
        config::update(|s| {
            if index < s.journal.habits.len() {
                s.journal.habits.remove(index);
            }
        });
        self.draft.borrow_mut().remove_habit(&h.key);
        self.rebuild_chips();
        self.refresh();
    }

    /// Place everything from the bottom up. The footer grows by a line when the habits wrap, and the panel
    /// grows with it, keeping its top edge.
    fn layout(&self) {
        let chips = self.chips.borrow();
        let editing = self.editing.get();
        // Right: the dismiss text, the counter and Save, 12 apart; the habits get everything left of them.
        // The dismiss button's 8 pt sides belong to the gaps around its text.
        let dismiss_w = self.dismiss.frame().size.width;
        let right_w = (dismiss_w - 16.0) + RIGHT_GAP + self.count_w + RIGHT_GAP + SAVE_W;
        let avail = INNER - right_w - ITEM_GAP;

        // Habits flow left to right, then the add button (or the name field).
        enum Item {
            Chip(usize, f64),
            Add,
            Editor,
        }
        let mut items: Vec<Item> = Vec::new();
        for (i, c) in chips.iter().enumerate() {
            items.push(if editing == Some(Edit::Rename(i)) { Item::Editor } else { Item::Chip(i, c.content_width()) });
        }
        items.push(if editing == Some(Edit::Add) { Item::Editor } else { Item::Add });
        let width = |it: &Item| match it {
            Item::Chip(_, w) => *w,
            Item::Add => ADD_W,
            Item::Editor => EDIT_W,
        };
        // A line breaks only when the habits do not fit; the last habit and the add button move as one, so the
        // button never starts a line alone.
        let last = items.len() - 1;
        let mut placed = vec![(0usize, 0.0f64); items.len()];
        let (mut x, mut line, mut i) = (0.0, 0usize, 0usize);
        while i < items.len() {
            let end = if i + 1 == last { last } else { i };
            let unit: f64 = (i..=end).map(|k| width(&items[k])).sum::<f64>() + ITEM_GAP * (end - i) as f64;
            if x > 0.0 && x + unit > avail {
                line += 1;
                x = 0.0;
            }
            for (k, slot) in placed.iter_mut().enumerate().take(end + 1).skip(i) {
                *slot = (line, x);
                x += width(&items[k]) + ITEM_GAP;
            }
            i = end + 1;
        }
        let lines = line + 1;
        let block = lines as f64 * CHIP_H + (lines - 1) as f64 * LINE_GAP;
        let footer = block.max(BUTTON_H);
        let bottom = PAD_BOTTOM + (footer - block) / 2.0;
        let line_y = |l: usize| bottom + block - CHIP_H * (l + 1) as f64 - LINE_GAP * l as f64;
        self.add.setHidden(editing == Some(Edit::Add));
        self.editor.setHidden(editing.is_none());
        for (it, (l, x)) in items.iter().zip(placed) {
            let (x, y) = (PAD_X + x, line_y(l));
            match it {
                Item::Chip(i, w) => {
                    chips[*i].setHidden(false);
                    chips[*i].setFrame(rect(x - CHIP_MARGIN, y, w + 2.0 * CHIP_MARGIN, CHIP_H));
                }
                Item::Add => self.add.place(rect(x, y, ADD_W, CHIP_H)),
                Item::Editor => self.editor.setFrame(rect(x, y, EDIT_W, CHIP_H)),
            }
        }
        if let Some(Edit::Rename(i)) = editing && let Some(c) = chips.get(i) {
            c.setHidden(true);
        }

        // Right: dismiss, counter, Save, centred on the footer.
        let ry = PAD_BOTTOM + (footer - BUTTON_H) / 2.0;
        self.save.place(rect(W - PAD_X - SAVE_W, ry, SAVE_W, BUTTON_H));
        let cx = W - PAD_X - SAVE_W - RIGHT_GAP - self.count_w;
        self.count.setFrame(rect(cx, ry + 10.0, self.count_w, 20.0));
        self.dismiss.place(rect(cx - RIGHT_GAP + 8.0 - dismiss_w, ry, dismiss_w, BUTTON_H));

        // Hairline, note (with the notice line under it), top row.
        let rule_y = PAD_BOTTOM + footer + FOOTER_GAP;
        self.rule.setFrame(rect(PAD_X, rule_y, INNER, 1.0));
        let note_y = rule_y + 1.0 + RULE_GAP;
        let notice = self.notice.get();
        let lift = if notice == Notice::None { 0.0 } else { NOTICE_H };
        self.scroll.setFrame(rect(PAD_X, note_y + lift, INNER, NOTE_H - lift));
        self.placeholder.setFrame(rect(PAD_X - 2.0, note_y + NOTE_H - 30.0, INNER + 4.0, 30.0));
        self.notice_text.setHidden(notice == Notice::None);
        self.notice_text.setFrame(rect(PAD_X, note_y + 2.0, INNER, 20.0));
        self.notice_icon.setHidden(notice != Notice::Error);
        let text_w = self.notice_text.attributedStringValue().size().width.ceil();
        self.notice_icon.setFrame(rect(W - PAD_X - text_w - 24.0, note_y + 4.0, 16.0, 16.0));
        let top = note_y + NOTE_H + NOTE_GAP;
        self.date.setFrame(rect(PAD_X, top + 4.0, 300.0, 20.0));
        let mut right = W - PAD_X;
        for p in self.days.iter().rev() {
            let w = p.frame().size.width;
            right -= w;
            p.place(rect(right, top, w, TOP_ROW));
            right -= 6.0;
        }

        let h = top + TOP_ROW + PAD_TOP;
        let f = self.window.frame();
        if (f.size.height - h).abs() > 0.5 {
            let y = f.origin.y + f.size.height - h;
            self.window.setFrame_display(rect(f.origin.x, y, W, h), true);
        }
    }

    fn present(&self, mtm: MainThreadMarker) {
        let (frame, visible) = screen_frames(mtm);
        self.backdrop.setFrame_display(frame, false);
        let h = self.window.frame().size.height;
        let x = visible.origin.x + ((visible.size.width - W) / 2.0).max(0.0);
        let y = visible.origin.y + ((visible.size.height - h) / 2.0).max(0.0);
        self.window.setFrame_display(rect(x.round(), y.round(), W, h), false);
        unsafe { self.backdrop.addChildWindow_ordered(&self.window, NSWindowOrderingMode::Above) };
        self.showing.set(true);
        self.ticket.set(self.ticket.get() + 1);
        let was = crate::motion::shown(&self.window);
        self.backdrop.orderFrontRegardless();
        if crate::app::offscreen() {
            self.window.orderFrontRegardless();
        } else {
            self.window.makeKeyAndOrderFront(None);
        }
        if !was {
            crate::motion::show(&self.backdrop, crate::motion::Kind::Fade);
            crate::motion::show(&self.window, crate::motion::Kind::Bloom);
        }
        let note: &NSResponder = &self.note;
        self.window.makeFirstResponder(Some(note));
        let end = NSRange::new(self.note.string().length(), 0);
        self.note.setSelectedRange(end);
        self.note.scrollRangeToVisible(end);
        if self.mode.get() == Mode::Due {
            let every = schedule().nag.num_seconds() as f64;
            let block = RcBlock::new(|_t: NonNull<NSTimer>| bring_front());
            let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(every, true, &block) };
            if let Some(old) = self.nag.replace(Some(timer)) {
                old.invalidate();
            }
        }
    }

    fn dismiss(&self) {
        self.showing.set(false);
        self.ticket.set(self.ticket.get() + 1);
        if let Some(t) = self.nag.take() {
            t.invalidate();
        }
        self.backdrop.removeChildWindow(&self.window);
        crate::motion::hide(&self.window, crate::motion::Kind::Bloom);
        crate::motion::hide(&self.backdrop, crate::motion::Kind::Fade);
    }

    /// The keyboard order follows the page: the rating (on its chosen day, or the first; arrows move inside
    /// it), the note, each habit, add, dismiss, Save.
    fn tab(&self, back: bool) {
        let chips = self.chips.borrow();
        let rating: &NSView = &self.days[self.draft.borrow().day.unwrap_or(0)];
        let mut stops: Vec<&NSView> = vec![rating, &self.note];
        stops.extend(chips.iter().filter(|c| !c.isHidden()).map(|c| -> &NSView { c }));
        if !self.add.isHidden() {
            stops.push(&self.add);
        }
        stops.push(&self.dismiss);
        if self.save.isEnabled() {
            stops.push(&self.save);
        }
        let n = stops.len();
        let focus = self.window.firstResponder();
        let focus: Option<&AnyObject> = focus.as_deref().map(|r| r as &AnyObject);
        let current = focus.and_then(|f| {
            if self.days.iter().any(|p| same(f, p)) {
                return Some(0);
            }
            stops.iter().position(|s| same(f, s))
        });
        let next = match current {
            Some(c) if back => (c + n - 1) % n,
            Some(c) => (c + 1) % n,
            None if back => n - 1,
            None => 0,
        };
        let view: &NSResponder = stops[next];
        self.window.makeFirstResponder(Some(view));
    }

    fn step(&self, p: &Pill, by: isize) {
        let i = p.tag();
        if !(0..self.days.len() as isize).contains(&i) {
            return;
        }
        let j = (i + by).clamp(0, self.days.len() as isize - 1) as usize;
        let next: &NSResponder = &self.days[j];
        self.window.makeFirstResponder(Some(next));
        unsafe { self.days[j].performClick(None) };
    }

    fn key(&self, ev: &NSEvent, mtm: MainThreadMarker) -> Option<Key> {
        let ours = ev.window(mtm).is_some_and(|w| same(&w, &self.window));
        if !self.showing.get() || !ours {
            return None;
        }
        let flags = ev.modifierFlags();
        let cmd = flags.contains(NSEventModifierFlags::Command);
        let shift = flags.contains(NSEventModifierFlags::Shift);
        let other = flags.intersects(NSEventModifierFlags::Control | NSEventModifierFlags::Option);
        let focus = self.window.firstResponder();
        let focus: Option<&AnyObject> = focus.as_deref().map(|r| r as &AnyObject);
        let in_note = focus.is_some_and(|f| same(f, &self.note));
        if in_note && self.note.hasMarkedText() {
            return None; // an accent or IME composition owns Esc, Return and Tab
        }
        // The habit name field owns every key but these two.
        if self.editing.get().is_some() && !in_note {
            return match ev.keyCode() {
                53 => Some(Key::FinishEdit(false)),
                36 | 76 => Some(Key::FinishEdit(true)),
                _ => None,
            };
        }
        let pill = focus.and_then(|f| f.downcast_ref::<Pill>()).map(|p| p.retain());
        let chip = focus.and_then(|f| f.downcast_ref::<HabitChip>()).map(|c| c.ivars().index.get());
        match ev.keyCode() {
            53 => Some(Key::Escape),
            // Return and Enter save; in the note they type a new line unless ⌘ is held.
            36 | 76 if cmd || !in_note => Some(Key::Save),
            48 if !cmd && !other => Some(Key::Tab(shift)),
            49 if !in_note && !cmd && !other => chip.map(Key::Toggle).or(pill.map(Key::Press)),
            123 if !in_note && !cmd => pill.map(|p| Key::Step(p, -1)),
            124 if !in_note && !cmd => pill.map(|p| Key::Step(p, 1)),
            _ => None,
        }
    }
}

/// The screen with the mouse: whole frame for the blur, visible frame to centre the prompt.
fn screen_frames(mtm: MainThreadMarker) -> (NSRect, NSRect) {
    let p = NSEvent::mouseLocation();
    let screens = NSScreen::screens(mtm);
    let screen = screens.iter().find(|s| {
        let f = s.frame();
        p.x >= f.origin.x && p.x <= f.origin.x + f.size.width && p.y >= f.origin.y && p.y <= f.origin.y + f.size.height
    });
    let fallback = rect(0.0, 0.0, W, 400.0);
    let (frame, visible) = screen.or_else(|| NSScreen::mainScreen(mtm)).map(|s| (s.frame(), s.visibleFrame())).unwrap_or((fallback, fallback));
    if crate::app::offscreen() {
        let away = NSRect::new(NSPoint::new(ui::OFFSCREEN, ui::OFFSCREEN), frame.size);
        (away, away)
    } else {
        (frame, visible)
    }
}

fn panel_like(window: &NSWindow) {
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setHasShadow(false);
    window.setLevel(NSFloatingWindowLevel);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary | NSWindowCollectionBehavior::Stationary,
    );
    unsafe { window.setReleasedWhenClosed(false) };
    window.setAppearance(NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }).as_deref());
}

fn text_label(mtm: MainThreadMarker, s: &str, font: &NSFont, color: [u8; 3], parent: &NSView) -> Retained<NSTextField> {
    let t = ui::label(mtm, font.pointSize(), color);
    t.setFont(Some(font));
    t.setStringValue(&NSString::from_str(s));
    parent.addSubview(&t);
    t
}

fn fill_view(mtm: MainThreadMarker, parent: &NSView, color: &NSColor, radius: f64) -> Retained<NSView> {
    let v = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
    v.setWantsLayer(true);
    if let Some(l) = v.layer() {
        l.setBackgroundColor(Some(&color.CGColor()));
        l.setCornerRadius(radius);
    }
    parent.addSubview(&v);
    v
}

fn build(mtm: MainThreadMarker) -> Ui {
    let target = target(mtm);
    let fill = NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable;
    let start = rect(0.0, 0.0, W, 400.0);

    // Full-screen blur behind the prompt; clicks pass through it (Vesper).
    let backdrop = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        start,
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    panel_like(&backdrop);
    backdrop.setIgnoresMouseEvents(true);
    backdrop.setHidesOnDeactivate(false);
    let blur = NSVisualEffectView::new(mtm);
    blur.setMaterial(NSVisualEffectMaterial::HUDWindow);
    blur.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    blur.setState(NSVisualEffectState::Active);
    let wash = NSView::initWithFrame(NSView::alloc(mtm), start);
    wash.setAutoresizingMask(fill);
    wash.setWantsLayer(true);
    if let Some(l) = wash.layer() {
        l.setBackgroundColor(Some(&NSColor::colorWithWhite_alpha(0.0, BACKDROP_WASH).CGColor()));
    }
    backdrop.setContentView(Some(&blur));
    wash.setFrame(blur.bounds());
    blur.addSubview(&wash);

    let this = Prompt::alloc(mtm).set_ivars(());
    let window: Retained<Prompt> = unsafe {
        msg_send![super(this), initWithContentRect: start,
            styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            backing: NSBackingStoreType::Buffered, defer: false]
    };
    panel_like(&window);
    window.setHidesOnDeactivate(false);
    // A see-through window lets clicks fall through to the app behind; take every click (panel.rs).
    window.setIgnoresMouseEvents(false);
    window.setDelegate(Some(ProtocolObject::from_ref(&*target)));
    let glass = Glass::new(mtm, PANEL_RADIUS, config::get().glass.max(0.85));
    window.setContentView(Some(&glass.root));
    let v: &NSView = &glass.content;

    // Styles: 14 regular (date, habits, counter, notices), 13 medium (ratings), 14 medium (buttons), 24 regular (note).
    let small = regular(tokens::text::LABEL.size);

    // Top row: the date; the ratings as small pills, only the chosen one in its colour. `layout` places them.
    let date = text_label(mtm, "", &small, SECONDARY, v);
    let days: Vec<Retained<Pill>> = DAY_LABELS
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let w = styled(label, &medium(13.0), &ui::rgb(TEXT), true).size().width.ceil() + 24.0;
            let p = Pill::new(mtm, v, rect(0.0, 0.0, w, TOP_ROW), TOP_ROW / 2.0, i as isize, sel!(choose:));
            p.ivars().size.set(13.0);
            p
        })
        .collect();

    // The note: borderless, no fill, the placeholder behind the text view (which draws no background), so a
    // click on it still lands in the note.
    let placeholder = text_label(mtm, "What did today teach you?", &regular(NOTE_SIZE), TERTIARY, v);
    let scroll = NSTextView::scrollableTextView(mtm);
    scroll.setDrawsBackground(false);
    scroll.setBorderType(NSBorderType::NoBorder);
    scroll.setHasVerticalScroller(true);
    scroll.setAutohidesScrollers(true);
    v.addSubview(&scroll);
    let note = scroll.documentView().and_then(|d| d.downcast::<NSTextView>().ok()).expect("scrollable text view");
    note.setDrawsBackground(false);
    note.setRichText(false);
    note.setAllowsUndo(true);
    note.setFont(Some(&regular(NOTE_SIZE)));
    note.setTextColor(Some(&ui::rgb(TEXT)));
    note.setInsertionPointColor(Some(&ui::rgb(TEXT)));
    note.setTextContainerInset(NSSize::new(0.0, 0.0));
    if let Some(c) = unsafe { note.textContainer() } {
        c.setLineFragmentPadding(0.0);
    }
    let attrs = note_attributes();
    unsafe { note.setTypingAttributes(&attrs) };
    note.setDefaultParagraphStyle(Some(&note_paragraph()));
    note.setDelegate(Some(ProtocolObject::from_ref(&*target)));
    note.setAccessibilityLabel(Some(ns_string!("Today's note")));

    // The line under the note: what Save still needs, or a failed save with its mark.
    let notice_icon = NSImageView::new(mtm);
    if let Some(img) = ui::symbol("exclamationmark.triangle.fill") {
        notice_icon.setImage(Some(&img));
    }
    notice_icon.setContentTintColor(Some(&ui::rgb(tokens::color::status::ERROR)));
    notice_icon.setHidden(true);
    v.addSubview(&notice_icon);
    let notice_text = text_label(mtm, "", &small, TEXT, v);
    notice_text.setAlignment(NSTextAlignment::Right);
    notice_text.setHidden(true);

    let rule = fill_view(mtm, v, &ui::white(0.08), 0.0);

    // Footer, left: the habits (built per open), the add button, and the name field it opens.
    let add = Pill::new(mtm, v, rect(0.0, 0.0, ADD_W, CHIP_H), CHIP_H / 2.0, ADD_TAG, sel!(addHabit:));
    add.paint("+", Some(&ui::white(0.06)), None, &ui::rgb(SECONDARY));
    add.setAccessibilityLabel(Some(ns_string!("Add a habit")));
    let editor = fill_view(mtm, v, &ui::white(0.06), CHIP_H / 2.0);
    editor.setHidden(true);
    let field = NSTextField::textFieldWithString(ns_string!(""), mtm);
    field.setBezeled(false);
    field.setDrawsBackground(false);
    field.setFocusRingType(NSFocusRingType::None);
    field.setFont(Some(&small));
    field.setTextColor(Some(&ui::rgb(TEXT)));
    field.setPlaceholderAttributedString(Some(&ui::placeholder("New habit", &small)));
    field.setFrame(rect(12.0, 4.0, EDIT_W - 24.0, 20.0));
    unsafe { field.setDelegate(Some(ProtocolObject::from_ref(&*target))) };
    editor.addSubview(&field);

    // Footer, right: the quiet dismiss text (titled per mode in `prepare`), the counter, the Save pill.
    let digits = crate::font::digits(tokens::text::LABEL.size, unsafe { NSFontWeightRegular });
    let count = text_label(mtm, "", &digits, SECONDARY, v);
    count.setAlignment(NSTextAlignment::Right);
    let count_w = styled("000 / 90", &digits, &ui::rgb(TEXT), false).size().width.ceil() + 2.0;
    let dismiss = Pill::new(mtm, v, rect(0.0, 0.0, SAVE_W, BUTTON_H), PILL_RADIUS, DISMISS_TAG, sel!(dismiss:));
    let save = Pill::new(mtm, v, rect(0.0, 0.0, SAVE_W, BUTTON_H), PILL_RADIUS, SAVE_TAG, sel!(save:));

    let handler = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        let ev = unsafe { event.as_ref() };
        if key_down(ev) { std::ptr::null_mut() } else { event.as_ptr() }
    });
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler) };

    Ui {
        backdrop,
        window,
        glass,
        date,
        days,
        scroll,
        note,
        placeholder,
        rule,
        chips: RefCell::new(Vec::new()),
        add,
        editor,
        field,
        count,
        count_w,
        notice_text,
        notice_icon,
        dismiss,
        save,
        mode: Cell::new(Mode::Early),
        draft: RefCell::new(Draft::default()),
        streak_base: RefCell::new(Vec::new()),
        editing: Cell::new(None),
        notice: Cell::new(Notice::None),
        saving: Cell::new(false),
        showing: Cell::new(false),
        ticket: Cell::new(0),
        nag: RefCell::new(None),
        _monitor: monitor,
    }
}

/// Returns true when the prompt used the key.
fn key_down(ev: &NSEvent) -> bool {
    let mtm = MainThreadMarker::new().unwrap();
    let Some(key) = with_ui(|u| u.key(ev, mtm)).flatten() else { return false };
    match key {
        Key::Escape => escape(),
        Key::Save => save(),
        Key::Tab(back) => {
            with_ui(|u| u.tab(back));
        }
        Key::Press(p) => unsafe { p.performClick(None) },
        Key::Step(p, by) => {
            with_ui(|u| u.step(&p, by));
        }
        Key::Toggle(i) => toggle_habit(i),
        Key::FinishEdit(confirm) => {
            with_ui(|u| u.finish_edit(confirm, true));
        }
    }
    true
}

fn schedule() -> Schedule {
    config::get().journal.schedule()
}

/// The journal file: the test file under `--test` or after `show_test`, else the one in Settings.
pub fn file() -> PathBuf {
    if let Some(p) = SCHED.with(|s| s.borrow().test_file.clone()) {
        return p;
    }
    if crate::app::test_mode() {
        return config::support_dir().join(TEST_FILE);
    }
    let f = config::get().journal.file;
    match f.strip_prefix('~') {
        Some(rest) => config::home().join(rest.trim_start_matches('/')),
        None => PathBuf::from(f),
    }
}

/// Whether `day` has an entry; the file is read again only when it changed.
fn done_on(day: NaiveDate) -> bool {
    let path = file();
    SCHED.with(|s| s.borrow_mut().done.get(&path, day))
}

/// Vesper (`com.mateus.vesper`) still running: it owns the evening prompt until the owner retires it.
fn vesper_running() -> bool {
    let me = NSRunningApplication::currentApplication().processIdentifier();
    NSWorkspace::sharedWorkspace().runningApplications().iter().any(|a| {
        let id = a.bundleIdentifier().map(|s| s.to_string().to_lowercase()).unwrap_or_default();
        let name = a.localizedName().map(|s| s.to_string()).unwrap_or_default();
        a.processIdentifier() != me && (id.contains("vesper") || name == "Vesper")
    })
}

fn changed() {
    if let Some(f) = SCHED.with(|s| s.borrow().listener) {
        f();
    }
}

fn showing() -> bool {
    with_ui(|u| u.showing.get()).unwrap_or(false)
}

/// Every 30 s and on wake: open the prompt when it is due.
fn check() {
    let test = crate::app::test_mode();
    // A `--test` copy asks on its own only after `show_test`, and then only into the test file.
    if test && SCHED.with(|s| s.borrow().test_file.is_none()) {
        return;
    }
    if !config::get().journal.enabled || showing() {
        return;
    }
    let (s, now) = (schedule(), core::now());
    if !SCHED.with(|x| x.borrow().tracker.due(&s, now, false)) {
        return;
    }
    if (!crate::app::offscreen() && vesper_running()) || done_on(now.date()) {
        return;
    }
    open(Mode::Due);
}

fn open(mode: Mode) {
    let mtm = MainThreadMarker::new().unwrap();
    if showing() {
        bring_front();
        return;
    }
    if UI.with(|u| u.borrow().is_none()) {
        let built = build(mtm);
        UI.with(|u| *u.borrow_mut() = Some(built));
    }
    with_ui(|u| {
        u.prepare(mode, core::now());
        u.present(mtm);
    });
    changed();
}

fn bring_front() {
    with_ui(|u| {
        if !u.showing.get() {
            return;
        }
        u.backdrop.orderFrontRegardless();
        if crate::app::offscreen() {
            u.window.orderFrontRegardless();
        } else {
            u.window.makeKeyAndOrderFront(None);
        }
    });
}

/// A due prompt that lost the keyboard comes back 25 s later (Vesper).
fn resigned() {
    let Some(ticket) = with_ui(|u| (u.showing.get() && u.mode.get() == Mode::Due).then(|| u.ticket.get())).flatten() else { return };
    let after = dispatch2::DispatchTime::NOW.time(REFRONT_S * 1_000_000_000);
    let _ = dispatch2::DispatchQueue::main().after(after, move || {
        if with_ui(|u| u.showing.get() && u.ticket.get() == ticket && !u.window.isKeyWindow()).unwrap_or(false) {
            bring_front();
        }
    });
}

/// Esc and the dismiss button: snooze when due, close until the hour when early, cancel an edit.
fn escape() {
    let Some(mode) = with_ui(|u| (u.showing.get() && !u.saving.get()).then(|| u.mode.get())).flatten() else { return };
    let (s, now) = (schedule(), core::now());
    SCHED.with(|x| {
        let mut x = x.borrow_mut();
        match mode {
            Mode::Due => x.tracker.snooze(&s, now),
            Mode::Early => x.tracker.close(&s, now),
            Mode::Edit => {}
        }
    });
    with_ui(|u| u.dismiss());
    changed();
}

/// Save on a background thread; on failure the form stays as typed and the day stays pending.
fn save() {
    let keys = keys(&habits());
    let entry = with_ui(|u| (u.showing.get() && !u.saving.get()).then(|| u.entry())).flatten();
    let Some(entry) = entry.filter(|e| e.is_complete(&keys)) else {
        // Say the first thing still missing instead of only beeping.
        with_ui(|u| {
            if let Some(hint) = core::missing(&u.entry(), &keys) {
                u.set_notice(Notice::Hint, Some(hint));
            }
        });
        return;
    };
    let now = core::now();
    let snoozes = SCHED.with(|s| s.borrow().tracker.snoozes(now));
    let path = file();
    with_ui(|u| {
        u.saving.set(true);
        if u.notice.get() != Notice::None {
            u.set_notice(Notice::None, None);
        }
        u.refresh_state();
    });
    std::thread::spawn(move || {
        let result = core::save(&path, &entry, snoozes, now).map_err(|e| format!("{}: {e}", path.display()));
        dispatch2::DispatchQueue::main().exec_async(move || saved(result));
    });
}

fn saved(result: Result<(), String>) {
    match result {
        Ok(()) => {
            SCHED.with(|s| {
                let mut s = s.borrow_mut();
                s.tracker.saved();
                s.done.clear();
            });
            let centre = with_ui(|u| {
                u.saving.set(false);
                u.draft.borrow_mut().forget();
                let f = u.window.frame();
                u.dismiss();
                NSPoint::new(f.origin.x + f.size.width / 2.0, f.origin.y + f.size.height / 2.0)
            });
            if let Some(c) = centre {
                crate::hud::show("Saved", c);
            }
            crate::year::refresh();
            changed();
        }
        Err(e) => {
            eprintln!("livae: journal not saved: {e}");
            with_ui(|u| {
                u.saving.set(false);
                u.set_notice(Notice::Error, Some(&e));
                u.refresh_state();
            });
        }
    }
}

/// Start the schedule: a check every 30 s and on wake, and one now (as Vesper does at launch).
pub fn install(mtm: MainThreadMarker) {
    let target = target(mtm);
    let observer: &AnyObject = &target;
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    unsafe { center.addObserver_selector_name_object(observer, sel!(wake:), Some(NSWorkspaceDidWakeNotification), None) };
    let tick = RcBlock::new(|_t: NonNull<NSTimer>| check());
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(30.0, true, &tick) };
    TIMER.with(|t| *t.borrow_mut() = Some(timer));
    check();
}

/// Open now. Before the hour it can be closed until then; once today is saved it edits today's entry.
pub fn show() {
    let now = core::now();
    open(schedule().mode(now, done_on(now.date())));
}

/// `--journal`: open with every read and write in a file under `config::support_dir()` (the fixture folder
/// under `--test`), never the vault. `fixture_file` seeds it (copied in unless it already lies there);
/// None starts from an empty journal.
pub fn show_test(fixture_file: Option<PathBuf>) {
    let dir = config::support_dir();
    let inside = |f: &Path| match (f.canonicalize(), dir.canonicalize()) {
        (Ok(f), Ok(d)) => f.starts_with(d),
        _ => false,
    };
    let path = match fixture_file {
        Some(f) if inside(&f) => f,
        Some(f) => {
            let to = dir.join(TEST_FILE);
            if let Err(e) = std::fs::copy(&f, &to) {
                eprintln!("livae: journal fixture {} not copied: {e}", f.display());
            }
            to
        }
        None => {
            let to = dir.join(TEST_FILE);
            let _ = std::fs::remove_file(&to);
            to
        }
    };
    SCHED.with(|s| {
        let mut s = s.borrow_mut();
        s.test_file = Some(path);
        s.done.clear();
    });
    show();
}

pub fn state() -> JournalState {
    if !config::get().journal.enabled {
        JournalState::Off
    } else if done_on(core::now().date()) {
        JournalState::Done
    } else {
        JournalState::Pending
    }
}

/// One line for the menu: "Opens at 20:00", "Snoozed until 20:15", "Due now", "Done today", "Vesper is running".
pub fn status_line() -> String {
    if !config::get().journal.enabled {
        return "Journal off".into();
    }
    if vesper_running() {
        return "Vesper is running".into();
    }
    let now = core::now();
    let done = done_on(now.date());
    SCHED.with(|s| s.borrow().tracker.status(&schedule(), now, done))
}

/// Today's note is saved (for Settings, which shows the time in its own row).
pub fn written_today() -> bool {
    done_on(core::now().date())
}

/// Called after the prompt opens, closes or saves, so the menu bar can redraw ✷ / ✦ at once.
pub fn on_change(f: fn()) {
    SCHED.with(|s| s.borrow_mut().listener = Some(f));
}

/// The Settings glass slider, live.
pub fn set_glass(darkness: f64) {
    with_ui(|u| u.glass.set_darkness(darkness.max(0.85)));
}
