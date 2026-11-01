//! Break reminders, replacing Stretchly Zen. A 1 s timer feeds `livae_core::breaks::Scheduler` with idle time,
//! lock and sleep, Focus, and whether Stretchly still runs. A break covers every screen edge to edge with
//! black glass, one quote and a thin line that drains as the break runs; a notice capsule comes first.
//! Quotes live in `quotes.md` next to the settings.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

use block2::RcBlock;
use livae_core::breaks::{self, Action, Break, Deck, NaiveDateTime, Quote, Rules, Scheduler, Status, Tick};
use livae_core::tokens::{self, text};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{class, define_class, msg_send, sel, AnyThread, MainThreadOnly};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSAutoresizingMaskOptions, NSBackingStoreType, NSButton, NSColor,
    NSEvent, NSFont, NSFontAttributeName, NSFontWeightMedium, NSFontWeightRegular, NSForegroundColorAttributeName, NSLineBreakMode, NSPanel, NSParagraphStyleAttributeName, NSResponder, NSScreen, NSScreenSaverWindowLevel, NSTextAlignment,
    NSTextField, NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace, NSWorkspaceDidWakeNotification,
    NSWorkspaceScreensDidSleepNotification, NSWorkspaceScreensDidWakeNotification, NSWorkspaceWillSleepNotification,
};
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{CGColor, CGEventSource, CGEventSourceStateID, CGEventType};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSArray, NSAttributedString, NSDictionary, NSNotification, NSNumber, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSTimer,
};

use crate::config;
use crate::glass::Glass;
use crate::ui::{self, SECONDARY, TEXT};
use objc2_quartz_core::CAGradientLayer;

const STRETCHLY_ID: &str = "net.hovancik.stretchly";
/// The quote: 34 pt regular, line height 1.3, at most 720 pt wide; its block centred 45% down the screen.
const QUOTE: f64 = 34.0;
const QUOTE_LEADING: f64 = 1.3;
const MEASURE: f64 = 720.0;
const FROM_TOP: f64 = 0.45;
const AUTHOR_GAP: f64 = 20.0;
/// The time line: 2 x 240 pt, 72 pt above the bottom of the screen.
const LINE_W: f64 = 240.0;
const LINE_H: f64 = 2.0;
const LINE_Y: f64 = 72.0;
const TIME_GAP: f64 = 12.0;
const CONTROL_GAP: f64 = 24.0;
const FADE_IN_MS: u64 = 600;
const FADE_OUT_MS: u64 = 400;

/// Focus is read by a helper process off the main thread; these carry its answer back.
static FOCUS: AtomicBool = AtomicBool::new(false);
static FOCUS_BUSY: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
struct Away {
    locked: bool,
    asleep: bool,
    displays_off: bool,
}

struct State {
    scheduler: Scheduler,
    deck: Deck,
    ticks: u64,
    stretchly: bool,
    focus_checked: f64,
    screens: Vec<Screen>,
    /// Redraws the time line at 30 fps while a break is on screen.
    progress: Option<Retained<NSTimer>>,
    _target: Retained<Target>,
    _timer: Retained<NSTimer>,
}

/// One break window, on one display.
struct Screen {
    panel: Retained<BreakPanel>,
    _glass: Glass,
    /// The part of the time line still to go.
    fill: Retained<NSView>,
    /// Minutes and seconds left, long breaks only.
    time: Option<Retained<NSTextField>>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static AWAY: Cell<Away> = const { Cell::new(Away { locked: false, asleep: false, displays_off: false }) };
}

fn set_away(f: impl FnOnce(&mut Away)) {
    AWAY.with(|a| {
        let mut v = a.get();
        f(&mut v);
        a.set(v);
    });
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeBreaksTarget"]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        #[unsafe(method(locked:))]
        fn locked(&self, _n: &NSNotification) {
            set_away(|a| a.locked = true);
        }

        #[unsafe(method(unlocked:))]
        fn unlocked(&self, _n: &NSNotification) {
            set_away(|a| a.locked = false);
        }

        #[unsafe(method(willSleep:))]
        fn will_sleep(&self, _n: &NSNotification) {
            set_away(|a| a.asleep = true);
        }

        #[unsafe(method(didWake:))]
        fn did_wake(&self, _n: &NSNotification) {
            set_away(|a| a.asleep = false);
        }

        #[unsafe(method(displaysSlept:))]
        fn displays_slept(&self, _n: &NSNotification) {
            set_away(|a| a.displays_off = true);
        }

        #[unsafe(method(displaysWoke:))]
        fn displays_woke(&self, _n: &NSNotification) {
            set_away(|a| a.displays_off = false);
        }

        #[unsafe(method(skip:))]
        fn skip(&self, _s: Option<&AnyObject>) {
            act(|s, _, _, local| s.skip(local));
        }

        #[unsafe(method(postpone:))]
        fn postpone(&self, _s: Option<&AnyObject>) {
            act(|s, r, _, _| s.postpone(r));
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeBreakPanel"]
    struct BreakPanel;

    impl BreakPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }

        /// Esc skips a break that may be skipped; a strict break ignores it. Other keys stop here.
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if event.keyCode() == 53 {
                act(|s, _, _, local| s.skip(local));
            }
        }
    }
);

impl BreakPanel {
    fn new(mtm: MainThreadMarker, rect: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        let panel: Retained<Self> = unsafe {
            msg_send![super(this), initWithContentRect: rect,
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered, defer: false]
        };
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        // Over everything, on every Space, full-screen apps included.
        panel.setLevel(NSScreenSaverWindowLevel);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setHidesOnDeactivate(false);
        panel.setIgnoresMouseEvents(false);
        panel.setAppearance(NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }).as_deref());
        panel
    }
}

fn quotes_file() -> PathBuf {
    config::support_dir().join("quotes.md")
}

fn rules() -> Rules {
    config::get().breaks.rules()
}

/// Starts the 1 s timer and listens for lock, sleep and wake. Writes the built-in quotes to `quotes.md` once.
pub fn install(mtm: MainThreadMarker) {
    let path = quotes_file();
    if !path.exists() && !crate::app::test_mode() {
        let _ = livae_core::write_atomic(&path, breaks::DEFAULT_QUOTES.as_bytes());
    }
    let target = Target::new(mtm);
    observe(&target);
    let block = RcBlock::new(|_t: NonNull<NSTimer>| tick());
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(1.0, true, &block) };
    timer.setTolerance(0.1);
    // Common modes: the timer keeps running while a menu is open or a dialog is up.
    unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
    STATE.with(|c| {
        *c.borrow_mut() = Some(State {
            scheduler: Scheduler::new(),
            deck: Deck::new(seed),
            ticks: 0,
            stretchly: false,
            focus_checked: 0.0,
            screens: Vec::new(),
            progress: None,
            _target: target,
            _timer: timer,
        })
    });
}

fn observe(target: &Target) {
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let workspace = unsafe {
        [
            (sel!(willSleep:), NSWorkspaceWillSleepNotification),
            (sel!(didWake:), NSWorkspaceDidWakeNotification),
            (sel!(displaysSlept:), NSWorkspaceScreensDidSleepNotification),
            (sel!(displaysWoke:), NSWorkspaceScreensDidWakeNotification),
        ]
    };
    for (selector, name) in workspace {
        unsafe { center.addObserver_selector_name_object(target, selector, Some(name), None) };
    }
    let distributed: Retained<AnyObject> = unsafe { msg_send![class!(NSDistributedNotificationCenter), defaultCenter] };
    let lock: [(Sel, &NSString); 2] = [(sel!(locked:), ns_string!("com.apple.screenIsLocked")), (sel!(unlocked:), ns_string!("com.apple.screenIsUnlocked"))];
    for (selector, name) in lock {
        let _: () = unsafe { msg_send![&*distributed, addObserver: target, selector: selector, name: name, object: None::<&AnyObject>] };
    }
}

fn idle_seconds() -> f64 {
    // kCGAnyInputEventType is ~0.
    CGEventSource::seconds_since_last_event_type(CGEventSourceStateID::HIDSystemState, CGEventType(u32::MAX))
}

fn stretchly_running() -> bool {
    NSWorkspace::sharedWorkspace().runningApplications().iter().any(|app| {
        app.bundleIdentifier().is_some_and(|id| id.to_string() == STRETCHLY_ID)
            || app.localizedName().is_some_and(|n| n.to_string().starts_with("Stretchly"))
    })
}

/// Stretchly's macOS check: Focus is on while its menu-bar item shows. Runs `defaults` on a helper thread.
fn check_focus() {
    if FOCUS_BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let on = std::process::Command::new("/usr/bin/defaults")
            .args(["read", "com.apple.controlcenter", "NSStatusItem Visible FocusModes"])
            .output()
            .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "1")
            .unwrap_or(false);
        FOCUS.store(on, Ordering::SeqCst);
        FOCUS_BUSY.store(false, Ordering::SeqCst);
    });
}

fn tick() {
    let r = rules();
    let action = STATE.with(|cell| {
        let mut slot = cell.borrow_mut();
        let st = slot.as_mut()?;
        st.ticks += 1;
        if st.ticks % 5 == 1 {
            st.stretchly = stretchly_running();
        }
        let (now, local) = breaks::clock();
        let away = AWAY.with(|a| a.get());
        let t = Tick {
            now,
            local,
            idle: idle_seconds(),
            away: away.locked || away.asleep || away.displays_off,
            focus: FOCUS.load(Ordering::SeqCst),
            // A test copy may show breaks while the owner's Stretchly still runs.
            other_app: st.stretchly && !crate::app::test_mode(),
        };
        let action = st.scheduler.tick(&r, t);
        // Focus matters only near a break, or while it holds one; reading it starts a process.
        let near = match st.scheduler.status(&r) {
            Status::Next { seconds, .. } => seconds <= r.notice + 30.0,
            Status::Focus => true,
            _ => false,
        };
        if near && now - st.focus_checked >= 5.0 {
            st.focus_checked = now;
            check_focus();
        }
        if let Some(b) = st.scheduler.running() {
            let left = NSString::from_str(&breaks::minutes_seconds(b.left(now)));
            for time in st.screens.iter().filter_map(|s| s.time.as_ref()) {
                time.setStringValue(&left);
            }
        }
        Some(action)
    });
    if let Some(a) = action {
        apply(a);
    }
}

/// Run `f` on the scheduler with the current settings and time, then do what it returns.
fn act(f: impl FnOnce(&mut Scheduler, &Rules, f64, NaiveDateTime) -> Action) {
    let r = rules();
    let (now, local) = breaks::clock();
    let action = STATE.with(|c| c.borrow_mut().as_mut().map(|st| f(&mut st.scheduler, &r, now, local)));
    if let Some(a) = action {
        apply(a);
    }
}

fn apply(action: Action) {
    match action {
        Action::None => {}
        Action::Notice { seconds, long } => notice(seconds, long),
        Action::Start(b) => show(b),
        Action::End => hide(),
    }
}

pub fn pause_for(minutes: u32) {
    act(|s, _, _, local| s.pause_for(local, minutes));
}

pub fn pause_until_morning() {
    act(|s, _, _, local| s.pause_until_morning(local));
}

pub fn resume() {
    act(|s, _, _, _| {
        s.resume();
        Action::None
    });
}

/// Ends a break that may be skipped; otherwise the next break comes a full interval from now.
pub fn skip_to_next_break() {
    act(|s, _, _, local| s.skip_to_next(local));
}

/// The coming break, now (long when a long one is due). Nothing while Stretchly Zen runs.
pub fn start_now() {
    act(|s, r, now, _| s.start_now(r, now, None));
}

/// `--break mini|long`: a break at once, of that kind.
pub fn show_test_break(long: bool) {
    act(|s, r, now, _| s.start_now(r, now, Some(long)));
}

/// "Next break in 12 min", "Paused until 06:00", "Stretchly is running", …
pub fn status_line() -> String {
    let r = rules();
    STATE.with(|c| c.borrow().as_ref().map(|st| st.scheduler.status(&r).line())).unwrap_or_default()
}

/// "Break in 10 s" in a glass capsule at the top of the main screen, refreshed every second until the break.
fn notice(seconds: u32, long: bool) {
    let mtm = MainThreadMarker::new().unwrap();
    let at = if crate::app::offscreen() {
        NSPoint::new(ui::OFFSCREEN, ui::OFFSCREEN)
    } else {
        let f = NSScreen::mainScreen(mtm).map(|s| s.visibleFrame()).unwrap_or(NSRect::ZERO);
        NSPoint::new(f.origin.x + f.size.width / 2.0, f.origin.y + f.size.height - tokens::space::S4 - 20.0)
    };
    crate::hud::show(&format!("{} in {seconds} s", if long { "Long break" } else { "Break" }), at);
}

/// The quotes in `quotes.md` (the built-in 13 when the file is missing or empty). Zen Gate gets the same list.
pub fn quotes() -> Vec<Quote> {
    breaks::load_quotes(&quotes_file())
}

fn next_quote() -> Quote {
    let mut quotes = quotes();
    let i = STATE.with(|c| c.borrow_mut().as_mut().and_then(|st| st.deck.next(quotes.len()))).unwrap_or(0);
    quotes.swap_remove(i.min(quotes.len().saturating_sub(1)))
}

/// Centred text with a fixed line height.
fn styled(text: &str, font: &NSFont, color: [u8; 3], leading: f64) -> Retained<NSAttributedString> {
    let para: Retained<AnyObject> = unsafe { msg_send![class!(NSMutableParagraphStyle), new] };
    unsafe {
        let _: () = msg_send![&*para, setAlignment: NSTextAlignment::Center];
        let _: () = msg_send![&*para, setMinimumLineHeight: leading];
        let _: () = msg_send![&*para, setMaximumLineHeight: leading];
    }
    let color = ui::rgb(color);
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName] };
    let attrs = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &[font as &AnyObject, &*color as &AnyObject, &*para]);
    unsafe { NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &NSString::from_str(text), Some(&attrs)) }
}

fn unix_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// A text-only control (Skip, Postpone), quiet so it never competes with the quote.
fn quiet_button(mtm: MainThreadMarker, title: &str, action: Sel, target: &Target) -> Retained<NSButton> {
    let b = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(target as &AnyObject), Some(action), mtm) };
    b.setBordered(false);
    let font = NSFont::systemFontOfSize_weight(text::LABEL.size, unsafe { NSFontWeightMedium });
    b.setAttributedTitle(&styled(title, &font, SECONDARY, text::LABEL.leading));
    b.sizeToFit();
    b
}

/// Black over the glass: 0.96 at the top and bottom edges, 0.82 across the middle, so the blurred desktop
/// shows faintly behind the quote and the edges stay OLED black.
fn shade(mtm: MainThreadMarker, size: NSSize) -> Retained<NSView> {
    let view = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), size));
    view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    view.setWantsLayer(true);
    let layer = CAGradientLayer::new();
    layer.setStartPoint(CGPoint::new(0.5, 1.0));
    layer.setEndPoint(CGPoint::new(0.5, 0.0));
    let stops = [0.96, 0.82, 0.82, 0.96].map(|a| NSColor::colorWithWhite_alpha(0.0, a).CGColor());
    // CGColor is toll-free bridged to an object, which is what the layer's colour array holds.
    let as_object = |c: &CGColor| -> &AnyObject { unsafe { &*(c as *const CGColor as *const AnyObject) } };
    let colors: Vec<&AnyObject> = stops.iter().map(|c| as_object(c)).collect();
    unsafe { layer.setColors(Some(&NSArray::from_slice(&colors))) };
    layer.setLocations(Some(&NSArray::from_retained_slice(&[0.0, 0.3, 0.7, 1.0].map(NSNumber::numberWithDouble))));
    view.setLayer(Some(&layer));
    view
}

/// A layer-backed bar in one colour with round ends.
fn bar(mtm: MainThreadMarker, frame: NSRect, color: &NSColor) -> Retained<NSView> {
    let view = NSView::initWithFrame(NSView::alloc(mtm), frame);
    view.setWantsLayer(true);
    if let Some(layer) = view.layer() {
        layer.setBackgroundColor(Some(&color.CGColor()));
        layer.setCornerRadius(LINE_H / 2.0);
    }
    view
}

fn build(mtm: MainThreadMarker, frame: NSRect, b: &Break, quote: &Quote, skip: bool, postpone: Option<u32>, target: &Target) -> Screen {
    let (w, h) = (frame.size.width, frame.size.height);
    let panel = BreakPanel::new(mtm, frame);
    // Apple's glass edge to edge, its own gradient off; the break's shade does the darkening.
    let glass = Glass::new(mtm, 0.0, 0.0);
    panel.setContentView(Some(&glass.root));
    glass.root.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), frame.size));
    let v = &glass.content;
    v.addSubview(&shade(mtm, frame.size));

    // The quote with the owner's line breaks, and its author.
    let column = ui::fit(w - 2.0 * tokens::space::S12, tokens::space::S32 * 2.0, MEASURE);
    let x = ((w - column) / 2.0).round();
    let font = NSFont::systemFontOfSize_weight(QUOTE, unsafe { NSFontWeightRegular });
    let words = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
    words.setSelectable(false);
    words.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    words.setAttributedStringValue(&styled(&quote.text, &font, TEXT, (QUOTE * QUOTE_LEADING).round()));
    let fit = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(column, 10_000.0));
    let words_h = words.cell().map(|c| c.cellSizeForBounds(fit).height).unwrap_or(QUOTE * QUOTE_LEADING).ceil();
    let (gap, author_h) = if quote.author.is_some() { (AUTHOR_GAP, text::BODY.leading) } else { (0.0, 0.0) };
    let block = words_h + gap + author_h;
    let top = (h * (1.0 - FROM_TOP) + block / 2.0).round().min(h - tokens::space::S12);
    words.setFrame(NSRect::new(NSPoint::new(x, top - words_h), NSSize::new(column, words_h)));
    v.addSubview(&words);
    if let Some(name) = &quote.author {
        let author = ui::label(mtm, text::BODY.size, SECONDARY);
        author.setAlignment(NSTextAlignment::Center);
        author.setStringValue(&NSString::from_str(name));
        author.setFrame(NSRect::new(NSPoint::new(x, top - words_h - gap - author_h), NSSize::new(column, author_h)));
        v.addSubview(&author);
    }

    // The time line; long breaks add minutes and seconds under it, then the controls. When controls show,
    // the stack rises so they keep a margin above the bottom edge.
    let buttons: Vec<Retained<NSButton>> = postpone
        .map(|m| quiet_button(mtm, &format!("Postpone {m} min"), sel!(postpone:), target))
        .into_iter()
        .chain(skip.then(|| quiet_button(mtm, "Skip", sel!(skip:), target)))
        .collect();
    let row_h = text::LABEL.leading;
    let below = if b.long { TIME_GAP + row_h } else { 0.0 } + if buttons.is_empty() { 0.0 } else { CONTROL_GAP + row_h };
    let line_y = if buttons.is_empty() { LINE_Y } else { LINE_Y.max(below + tokens::space::S8) };
    let track = bar(mtm, NSRect::new(NSPoint::new(((w - LINE_W) / 2.0).round(), line_y), NSSize::new(LINE_W, LINE_H)), &ui::white(0.12));
    let fill = bar(mtm, NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(LINE_W, LINE_H)), &ui::rgb(TEXT));
    track.addSubview(&fill);
    v.addSubview(&track);

    let mut row_top = line_y;
    let time = b.long.then(|| {
        let t = ui::label(mtm, text::LABEL.size, SECONDARY);
        t.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(text::LABEL.size, unsafe { NSFontWeightRegular })));
        t.setAlignment(NSTextAlignment::Center);
        t.setStringValue(&NSString::from_str(&breaks::minutes_seconds(b.seconds)));
        row_top = line_y - TIME_GAP - row_h;
        t.setFrame(NSRect::new(NSPoint::new((w - 200.0) / 2.0, row_top), NSSize::new(200.0, row_h)));
        v.addSubview(&t);
        t
    });

    let row_y = row_top - CONTROL_GAP - row_h;
    let widths: Vec<f64> = buttons.iter().map(|b| b.frame().size.width + 2.0 * tokens::space::S3).collect();
    let spacing = tokens::space::S2;
    let mut bx = ((w - widths.iter().sum::<f64>() - spacing * widths.len().saturating_sub(1) as f64) / 2.0).round();
    for (button, bw) in buttons.iter().zip(widths) {
        let hit = tokens::size::HIT;
        button.setFrame(NSRect::new(NSPoint::new(bx, row_y + (row_h - hit) / 2.0), NSSize::new(bw, hit)));
        v.addSubview(button);
        bx += bw + spacing;
    }

    Screen { panel, _glass: glass, fill, time }
}

/// Draws the time line at 30 fps while a break is on screen.
fn start_progress() -> Retained<NSTimer> {
    let step = RcBlock::new(|_t: NonNull<NSTimer>| {
        STATE.with(|c| {
            let slot = c.borrow();
            let Some(st) = slot.as_ref() else { return };
            let Some(b) = st.scheduler.running() else { return };
            let width = LINE_W * (b.left(unix_now()) / b.seconds.max(1.0)).clamp(0.0, 1.0);
            for s in &st.screens {
                s.fill.setFrameSize(NSSize::new(width, LINE_H));
            }
        })
    });
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(1.0 / 30.0, true, &step) };
    unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
    timer
}

fn show(b: Break) {
    let mtm = MainThreadMarker::new().unwrap();
    close_now();
    let quote = next_quote();
    let r = rules();
    let (_, local) = breaks::clock();
    let Some((skip, postpone, target)) = STATE.with(|c| {
        c.borrow().as_ref().map(|st| {
            let postpone = st.scheduler.can_postpone(&r).then(|| (r.long_postpone / 60.0).round() as u32);
            (st.scheduler.can_skip(local), postpone, st._target.clone())
        })
    }) else {
        return;
    };

    let main = NSScreen::mainScreen(mtm).map(|s| s.frame());
    let frames: Vec<NSRect> = if crate::app::offscreen() {
        vec![main.unwrap_or(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1512.0, 982.0)))]
    } else {
        NSScreen::screens(mtm).iter().map(|s| s.frame()).collect()
    };
    let screens: Vec<Screen> = frames.iter().map(|&f| build(mtm, f, &b, &quote, skip, postpone, &target)).collect();
    for (s, frame) in screens.iter().zip(&frames) {
        s.panel.setAlphaValue(0.0);
        // The main screen's break takes the keyboard (Esc, and typing pauses with the break); an offscreen
        // test copy never does.
        if crate::app::offscreen() || Some(*frame) == main {
            ui::present(&s.panel);
        } else {
            s.panel.orderFrontRegardless();
        }
    }
    fade(screens.iter().map(|s| s.panel.clone()).collect(), 0.0, 1.0, FADE_IN_MS, || {});
    let progress = start_progress();
    STATE.with(|c| {
        if let Some(st) = c.borrow_mut().as_mut() {
            st.screens = screens;
            st.progress = Some(progress);
        }
    });
}

fn take_screens() -> Vec<Screen> {
    STATE.with(|c| {
        c.borrow_mut()
            .as_mut()
            .map(|st| {
                if let Some(t) = st.progress.take() {
                    t.invalidate();
                }
                std::mem::take(&mut st.screens)
            })
            .unwrap_or_default()
    })
}

fn close_now() {
    for s in take_screens() {
        s.panel.orderOut(None);
    }
}

fn hide() {
    let screens = take_screens();
    if screens.is_empty() {
        return;
    }
    let panels = screens.iter().map(|s| s.panel.clone()).collect();
    fade(panels, 1.0, 0.0, FADE_OUT_MS, move || {
        for s in &screens {
            s.panel.orderOut(None);
        }
    });
}

/// Fade the windows' opacity with the system's ease-out; instant under Reduce Motion.
fn fade(panels: Vec<Retained<BreakPanel>>, from: f64, to: f64, ms: u64, done: impl FnOnce() + 'static) {
    let reduce: bool = unsafe { msg_send![&*NSWorkspace::sharedWorkspace(), accessibilityDisplayShouldReduceMotion] };
    if reduce || ms == 0 {
        for p in &panels {
            p.setAlphaValue(to);
        }
        done();
        return;
    }
    let start = std::time::Instant::now();
    let done: Cell<Option<Box<dyn FnOnce()>>> = Cell::new(Some(Box::new(done)));
    let step = RcBlock::new(move |timer: NonNull<NSTimer>| {
        let p = (start.elapsed().as_secs_f64() * 1000.0 / ms as f64).min(1.0);
        let eased = 1.0 - (1.0 - p).powi(3);
        for panel in &panels {
            panel.setAlphaValue(from + (to - from) * eased);
        }
        if p >= 1.0 {
            unsafe { timer.as_ref() }.invalidate();
            if let Some(d) = done.take() {
                d();
            }
        }
    });
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(1.0 / 60.0, true, &step) };
    unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
}
