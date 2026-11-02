//! The year view: every day of a year as a small square, one column per week
//! (Monday first). Two tabs: Mood shows each day in its rating colour; Streak shows the habits
//! done that day as white, from a 35% base for one up to full white for all, so no-habit days stay clearly dim. Pointing at a day shows it in a small glass card.
//! Opened from the menu; Esc, ⌘W or a click elsewhere closes it. Data comes from `livae_core::journal` and is
//! read again whenever the journal is saved.

use std::cell::{OnceCell, RefCell};
use std::collections::BTreeMap;
use std::ptr::NonNull;

use block2::RcBlock;
use livae_core::journal::{self as core, Datelike, DaySummary, NaiveDate, DAY_COLORS, DAY_LABELS};
use livae_core::tokens::color::dark;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, AnyThread, MainThreadOnly};
use objc2_app_kit::{
    NSAccessibility, NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSAttributedStringNSExtendedStringDrawing,
    NSAttributedStringNSStringDrawing, NSAutoresizingMaskOptions, NSBackingStoreType, NSBezierPath, NSButton, NSButtonType, NSColor, NSControl,
    NSEvent, NSEventMask, NSEventModifierFlags, NSFloatingWindowLevel, NSFont, NSFontAttributeName, NSFontWeightMedium,
    NSForegroundColorAttributeName, NSLineBreakMode, NSMutableParagraphStyle, NSPanel, NSParagraphStyleAttributeName, NSResponder,
    NSScreen, NSStringDrawingOptions, NSTextAlignment, NSTextField, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowDelegate, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSDictionary, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use crate::config::{self, Habit};
use crate::glass::Glass;
use crate::ui::{self, SECONDARY, TERTIARY, TEXT};

const PAD_X: f64 = 34.0;
const PAD_Y: f64 = 28.0;
const RADIUS: f64 = 28.0;
const CELL: f64 = 12.0;
/// 12 pt squares, 5 pt apart, so neighbouring habit rings never touch.
const PITCH: f64 = 17.0;
const CELL_RADIUS: f64 = 2.6;
/// The grid starts after the weekday letters and under the month names.
const GRID_X: f64 = 26.0;
const GRID_Y: f64 = 26.0;
const WEEKS: usize = 54;
const GRID_W: f64 = GRID_X + WEEKS as f64 * PITCH;
const GRID_H: f64 = GRID_Y + 7.0 * PITCH;
/// The ring around the day under the pointer: 3.5 pt outside the square.
const RING_RADIUS: f64 = 4.0;
const HOVER_OUT: f64 = 3.5;
const HEADER_H: f64 = 28.0;
const HEADER_GAP: f64 = 22.0;
const FOOTER_GAP: f64 = 18.0;
const FOOTER_H: f64 = 20.0;
const BOARD_H: f64 = GRID_H + FOOTER_GAP + FOOTER_H;
const W: f64 = 2.0 * PAD_X + GRID_W;
const H: f64 = 2.0 * PAD_Y + HEADER_H + HEADER_GAP + BOARD_H;
const CARD_W: f64 = 300.0;
const CARD_X: f64 = 18.0;
const CARD_Y: f64 = 16.0;
const CARD_RADIUS: f64 = 16.0;
/// The note in the card: 14 regular on 20 pt lines, at most four.
const CARD_LINE: f64 = 20.0;
const CARD_LINES: f64 = 4.0;
const LETTERS: [&str; 7] = ["M", "T", "W", "T", "F", "S", "S"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
// Button tags: year back -1 and forward +1.

#[derive(Default)]
struct State {
    year: i32,
    habits: Vec<Habit>,
    days: BTreeMap<NaiveDate, DaySummary>,
    /// Each habit's current streak, by key.
    streaks: Vec<(String, u32)>,
    today: NaiveDate,
    hover: Option<NaiveDate>,
    /// The legend item under the pointer, when it is on the legend instead of a day.
    legend_hover: Option<Group>,
    tab: Tab,
}

/// What a day belongs to in the current tab: its rating, or how many habits it has done. Pointing at a day or a
/// legend item lights every day of the same group and counts them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Rating(usize),
    NoEntry,
    Habits(usize),
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Tab {
    #[default]
    Mood,
    Streak,
}

struct Ui {
    window: Retained<YearPanel>,
    glass: Glass,
    year_label: Retained<NSTextField>,
    prev: Retained<NSButton>,
    next: Retained<NSButton>,
    tabs: [Retained<NSButton>; 2],
    board: Retained<Board>,
    card: Retained<NSPanel>,
    _card_glass: Glass,
    card_view: Retained<CardView>,
    _monitor: Option<Retained<AnyObject>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    /// Built once, then only ever borrowed shared, so AppKit callbacks may re-enter safely.
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static TARGET: OnceCell<Retained<Target>> = const { OnceCell::new() };
}

fn with_ui<R>(f: impl FnOnce(&Ui) -> R) -> Option<R> {
    UI.with(|u| u.borrow().as_ref().map(f))
}

fn with_state<R>(f: impl FnOnce(&State) -> R) -> R {
    STATE.with(|s| f(&s.borrow()))
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeYearTarget"]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl NSWindowDelegate for Target {
        /// A click in another app (or anywhere outside) takes the keyboard away: close, like a popover.
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _n: &NSNotification) {
            // A test copy started in the background loses the keyboard at once; it stays until it is quit.
            if !crate::app::test_mode() {
                close();
            }
        }
    }

    impl Target {
        #[unsafe(method(tab:))]
        fn tab_pressed(&self, sender: Option<&AnyObject>) {
            if let Some(t) = sender.and_then(|s| s.downcast_ref::<NSControl>()).map(|c| c.tag()) {
                set_tab(if t == 1 { Tab::Streak } else { Tab::Mood });
            }
        }

        #[unsafe(method(year:))]
        fn year_pressed(&self, sender: Option<&AnyObject>) {
            if let Some(by) = sender.and_then(|s| s.downcast_ref::<NSControl>()).map(|c| c.tag()) {
                step_year(by as i32);
            }
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
    /// Non-activating like the launcher: it takes the keyboard (Esc, ⌘W, arrows) without pulling Livae forward.
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeYearPanel"]
    struct YearPanel;

    impl YearPanel {
        /// Never in an offscreen test copy: it must not take the owner's typing.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            !crate::app::offscreen()
        }
    }
);

define_class!(
    /// The grid, its labels and the footer, drawn in one view; it answers the pointer for the hover card.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeYearBoard"]
    struct Board;

    impl Board {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _e: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            with_state(draw_board);
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            let on_legend = with_state(|s| legend_layout(s).into_iter().find(|(r, ..)| p.x >= r.origin.x && p.x < r.origin.x + r.size.width && p.y >= r.origin.y && p.y < r.origin.y + r.size.height).map(|(_, g, ..)| g));
            let legend_changed = STATE.with(|s| {
                let mut s = s.borrow_mut();
                let changed = s.legend_hover != on_legend;
                s.legend_hover = on_legend;
                changed
            });
            if legend_changed {
                self.setNeedsDisplay(true);
            }
            hover(if on_legend.is_some() { None } else { with_state(|s| date_at(s, p)) });
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            STATE.with(|s| s.borrow_mut().legend_hover = None);
            self.setNeedsDisplay(true);
            hover(None);
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            for area in self.trackingAreas().iter() {
                self.removeTrackingArea(&area);
            }
            let options = NSTrackingAreaOptions::MouseMoved
                | NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect;
            let area = unsafe { NSTrackingArea::initWithRect_options_owner_userInfo(NSTrackingArea::alloc(), NSRect::ZERO, options, Some(self), None) };
            self.addTrackingArea(&area);
            let _: () = unsafe { msg_send![super(self), updateTrackingAreas] };
        }
    }
);

define_class!(
    /// The hover card's content: date, rating, note, habits.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LivaeYearCard"]
    struct CardView;

    impl CardView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            with_state(draw_card);
        }
    }
);

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn font(size: f64, medium: bool) -> Retained<NSFont> {
    if medium { crate::font::sans(size, unsafe { NSFontWeightMedium }) } else { crate::font::regular(size) }
}

fn styled(text: &str, font: &NSFont, color: &NSColor, para: Option<&NSMutableParagraphStyle>) -> Retained<NSAttributedString> {
    let default = NSMutableParagraphStyle::new();
    let para = para.unwrap_or(&default);
    let keys = unsafe { [NSForegroundColorAttributeName, NSFontAttributeName, NSParagraphStyleAttributeName] };
    let values: [&AnyObject; 3] = [color, font, para];
    let attrs = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &values);
    unsafe { NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &NSString::from_str(text), Some(&attrs)) }
}

fn centred(text: &str, font: &NSFont, color: &NSColor) -> Retained<NSAttributedString> {
    let para = NSMutableParagraphStyle::new();
    para.setAlignment(NSTextAlignment::Center);
    styled(text, font, color, Some(&para))
}

fn day_color(i: usize) -> Retained<NSColor> {
    let (r, g, b) = DAY_COLORS[i];
    NSColor::colorWithSRGBRed_green_blue_alpha(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0, 1.0)
}

fn fill_round(r: NSRect, radius: f64, color: &NSColor) {
    color.setFill();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, radius, radius).fill();
}

fn stroke_round(r: NSRect, radius: f64, width: f64, color: &NSColor) {
    let p = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, radius, radius);
    p.setLineWidth(width);
    color.setStroke();
    p.stroke();
}

fn dashed_round(r: NSRect, radius: f64, color: &NSColor) {
    let p = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, radius, radius);
    p.setLineWidth(1.0);
    let dash = [2.0_f64, 2.0];
    unsafe { p.setLineDash_count_phase(dash.as_ptr(), 2, 0.0) };
    color.setStroke();
    p.stroke();
}

/// Draw `s` with its left edge at `x`, centred on `mid` (flipped view).
fn text_at(s: &NSAttributedString, x: f64, mid: f64) -> f64 {
    let size = s.size();
    s.drawAtPoint(NSPoint::new(x, (mid - size.height / 2.0).round()));
    size.width.ceil()
}

fn jan1(year: i32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, 1, 1).unwrap_or_default()
}

/// Days before Monday in the first week.
fn lead(year: i32) -> i64 {
    jan1(year).weekday().num_days_from_monday() as i64
}

fn cell_origin(d: NaiveDate) -> NSPoint {
    let index = d.ordinal0() as i64 + lead(d.year());
    NSPoint::new(GRID_X + (index / 7) as f64 * PITCH, GRID_Y + (index % 7) as f64 * PITCH)
}

fn date_at(s: &State, p: NSPoint) -> Option<NaiveDate> {
    if p.x < GRID_X || p.y < GRID_Y || p.x >= GRID_W || p.y >= GRID_H {
        return None;
    }
    let (col, row) = (((p.x - GRID_X) / PITCH) as i64, ((p.y - GRID_Y) / PITCH) as i64);
    let index = col * 7 + row - lead(s.year);
    u32::try_from(index).ok().and_then(|i| NaiveDate::from_yo_opt(s.year, i + 1))
}

fn habit_done(day: &DaySummary, key: &str) -> bool {
    day.habits.iter().any(|(k, v)| k == key && *v)
}

/// The day's group in the current tab; days still to come have none.
fn group_of(s: &State, d: NaiveDate) -> Option<Group> {
    if d > s.today {
        return None;
    }
    let Some(day) = s.days.get(&d) else { return Some(Group::NoEntry) };
    Some(match s.tab {
        Tab::Mood => match day.rating.filter(|r| *r < DAY_COLORS.len()) {
            Some(r) => Group::Rating(r),
            None => Group::NoEntry,
        },
        Tab::Streak => Group::Habits(habits_done(s, day).0),
    })
}

/// What is lit: the legend item under the pointer, else the group of the day under it.
fn focus(s: &State) -> Option<Group> {
    s.legend_hover.or_else(|| s.hover.and_then(|d| group_of(s, d)))
}

/// Days of the year so far in group `g`.
fn count(s: &State, g: Group) -> u32 {
    let mut n = 0;
    let mut d = jan1(s.year);
    while d.year() == s.year && d <= s.today {
        if group_of(s, d) == Some(g) {
            n += 1;
        }
        match d.succ_opt() {
            Some(x) => d = x,
            None => break,
        }
    }
    n
}

/// Consecutive days up to and including `d` on which habit `key` was done.
fn streak_at(s: &State, key: &str, d: NaiveDate) -> u32 {
    let mut n = 0;
    let mut day = d;
    while s.days.get(&day).is_some_and(|x| habit_done(x, key)) {
        n += 1;
        match day.pred_opt() {
            Some(p) => day = p,
            None => break,
        }
    }
    n
}

/// The legend items of the current tab: (hit rectangle, group, colour, label). Drawing and pointing share it.
fn legend_layout(s: &State) -> Vec<(NSRect, Group, Retained<NSColor>, String)> {
    let mut items: Vec<(Group, Retained<NSColor>, String)> = Vec::new();
    match s.tab {
        Tab::Mood => {
            for (i, label) in DAY_LABELS.iter().enumerate() {
                items.push((Group::Rating(i), day_color(i), label.to_string()));
            }
            items.push((Group::NoEntry, ui::white(0.07), "No entry".into()));
        }
        Tab::Streak => {
            let n = s.habits.len();
            items.push((Group::Habits(0), ui::white(0.07), "None".into()));
            for done in 1..=n {
                let label = match (done, n) {
                    (1, 1) => "Done".to_string(),
                    (d, t) if d == t && t == 2 => "Both habits".to_string(),
                    (d, t) if d == t => "All habits".to_string(),
                    (1, _) => "1 habit".to_string(),
                    (d, _) => format!("{d} habits"),
                };
                items.push((Group::Habits(done), ui::white(streak_alpha(done, n).unwrap_or(1.0)), label));
            }
        }
    }
    let small = font(12.0, false);
    let bold = font(12.0, true);
    let mid = GRID_H + FOOTER_GAP + FOOTER_H / 2.0;
    let focus = focus(s);
    let mut x = 0.0;
    let mut out = Vec::new();
    for (g, color, label) in items {
        let text = if focus == Some(g) { format!("{label}  {}", days_label(count(s, g))) } else { label.clone() };
        let w = CELL + 6.0 + styled(&text, if focus == Some(g) { &bold } else { &small }, &ui::rgb(TEXT), None).size().width.ceil();
        out.push((rect(x, mid - 10.0, w, 20.0), g, color, label));
        x += w + 14.0;
    }
    out
}

/// Mood: the day's rating colour. Streak: white by habits done (none dim, one of two 35%, all 100%).
/// Days without an entry, and the days still to come, stay dim in both.
fn cell_color(s: &State, d: NaiveDate) -> Retained<NSColor> {
    if d > s.today {
        return ui::white(0.03);
    }
    let Some(day) = s.days.get(&d) else { return ui::white(0.07) };
    match s.tab {
        Tab::Mood => match day.rating.filter(|r| *r < DAY_COLORS.len()) {
            Some(r) => day_color(r),
            None => ui::white(0.07),
        },
        Tab::Streak => {
            let (done, total) = habits_done(s, day);
            ui::white(streak_alpha(done, total).unwrap_or(0.07))
        }
    }
}

/// White for `done` of `total` habits: nothing when none, a 35% base for one, rising to full white for all.
fn streak_alpha(done: usize, total: usize) -> Option<f64> {
    const BASE: f64 = 0.35;
    match (done, total) {
        (0, _) | (_, 0) => None,
        (_, 1) => Some(1.0),
        (d, t) => Some(BASE + (1.0 - BASE) * (d.min(t) - 1) as f64 / (t - 1) as f64),
    }
}

/// Counts only the habits in Settings that the day answered, so a habit added later never changes older days.
fn habits_done(s: &State, day: &DaySummary) -> (usize, usize) {
    let answered: Vec<bool> = s.habits.iter().filter_map(|h| day.habits.iter().find(|(k, _)| *k == h.key).map(|(_, v)| *v)).collect();
    (answered.iter().filter(|v| **v).count(), answered.len())
}

fn outset(r: NSRect, by: f64) -> NSRect {
    rect(r.origin.x - by, r.origin.y - by, r.size.width + 2.0 * by, r.size.height + 2.0 * by)
}

fn days_label(n: u32) -> String {
    if n == 1 { "1 day".into() } else { format!("{n} days") }
}

fn draw_board(s: &State) {
    let small = font(12.0, false);
    let tertiary = ui::rgb(TERTIARY);
    for (m, name) in MONTHS.iter().enumerate() {
        if let Some(first) = NaiveDate::from_ymd_opt(s.year, m as u32 + 1, 1) {
            text_at(&styled(name, &small, &tertiary, None), cell_origin(first).x, 10.0);
        }
    }
    let letter = font(12.0, true);
    let secondary = ui::rgb(SECONDARY);
    for (k, l) in LETTERS.iter().enumerate() {
        text_at(&styled(l, &letter, &secondary, None), 0.0, GRID_Y + k as f64 * PITCH + CELL / 2.0);
    }
    let focus = focus(s);
    let mut d = jan1(s.year);
    while d.year() == s.year {
        let o = cell_origin(d);
        let r = rect(o.x, o.y, CELL, CELL);
        let color = cell_color(s, d);
        // Lit days keep their colour; the rest fade (Mood keeps a hint of its hue, Streak drops to near black,
        // since brightness is its only signal). Lit empty days are raised so they stand out too.
        let color = match focus {
            Some(g) if d <= s.today && group_of(s, d) != Some(g) => match s.tab {
                Tab::Mood => color.colorWithAlphaComponent(color.alphaComponent() * 0.22),
                Tab::Streak => ui::white(0.04),
            },
            Some(Group::NoEntry | Group::Habits(0)) if d <= s.today => ui::white(0.22),
            _ => color,
        };
        fill_round(r, CELL_RADIUS, &color);
        if d == s.today {
            dashed_round(outset(r, -0.5), 2.4, &ui::white(0.6));
        }
        if s.hover == Some(d) {
            stroke_round(outset(r, HOVER_OUT), RING_RADIUS + 2.0, 1.5, &ui::rgb(TEXT));
        }
        match d.succ_opt() {
            Some(n) => d = n,
            None => break,
        }
    }

    // Footer: the legend on the left, each habit's streak on the right.
    let mid = GRID_H + FOOTER_GAP + FOOTER_H / 2.0;
    for (r, g, color, label) in legend_layout(s) {
        let on = focus == Some(g);
        fill_round(rect(r.origin.x, mid - 6.0, CELL, CELL), CELL_RADIUS, &color);
        let text = if on { format!("{label}  {}", days_label(count(s, g))) } else { label };
        let f = if on { font(12.0, true) } else { small.clone() };
        let style = styled(&text, &f, &ui::rgb(if on { TEXT } else { TERTIARY }), None);
        text_at(&style, r.origin.x + CELL + 6.0, mid);
    }
    let shown: Vec<&Habit> = s.habits.iter().collect();
    let (body, strong) = (font(14.0, false), font(14.0, true));
    let mut right = GRID_W;
    for h in shown.iter().rev() {
        let n = s.streaks.iter().find(|(k, _)| *k == h.key).map_or(0, |(_, n)| *n);
        let value = styled(&format!("{n}-day streak"), &strong, &ui::rgb(if n == 0 { TERTIARY } else { TEXT }), None);
        let label = styled(&h.label, &body, &secondary, None);
        let (vw, lw) = (value.size().width.ceil(), label.size().width.ceil());
        text_at(&value, right - vw, mid);
        text_at(&label, right - vw - 8.0 - lw, mid);
        right -= vw + 8.0 + lw + 24.0;
    }
}

/// The note as drawn in the card: wrapped, at most four lines, an ellipsis when cut.
fn card_note(note: &str) -> Retained<NSAttributedString> {
    let para = NSMutableParagraphStyle::new();
    para.setMinimumLineHeight(CARD_LINE);
    para.setMaximumLineHeight(CARD_LINE);
    para.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    styled(note, &font(14.0, false), &ui::rgb(TEXT), Some(&para))
}

fn note_height(note: &NSAttributedString) -> f64 {
    let size = NSSize::new(CARD_W - 2.0 * CARD_X, 10_000.0);
    let h = note.boundingRectWithSize_options_context(size, NSStringDrawingOptions::UsesLineFragmentOrigin, None).size.height;
    h.ceil().min(CARD_LINE * CARD_LINES)
}

/// One check per habit, wrapping inside the card: (line, x, done, label).
fn card_habits(s: &State, day: &DaySummary) -> Vec<(bool, Retained<NSAttributedString>, Option<Retained<NSAttributedString>>)> {
    let label_font = font(13.0, false);
    s.habits
        .iter()
        .map(|h| {
            let done = habit_done(day, &h.key);
            let label = styled(&h.label, &label_font, &ui::rgb(if done { TEXT } else { TERTIARY }), None);
            // A done habit shows its run on that day, right-aligned ("3-day streak").
            let run = done.then(|| styled(&format!("{}-day streak", streak_at(s, &h.key, day.date)), &label_font, &ui::rgb(SECONDARY), None));
            (done, label, run)
        })
        .collect()
}

/// The card for the pointed-at day: just the date and "No entry", or the date, rating, note and habits.
fn card_height(s: &State, d: NaiveDate) -> f64 {
    let Some(day) = s.days.get(&d) else { return 2.0 * CARD_Y + 20.0 };
    let mut h = CARD_Y + 20.0;
    if !day.note.trim().is_empty() {
        h += 10.0 + note_height(&card_note(day.note.trim()));
    }
    let lines = s.habits.len();
    if lines > 0 {
        h += 12.0 + lines as f64 * 16.0 + (lines - 1) as f64 * 6.0;
    }
    h + CARD_Y
}

fn draw_card(s: &State) {
    let Some(d) = s.hover else { return };
    let inner = CARD_W - 2.0 * CARD_X;
    let mid = CARD_Y + 10.0;
    text_at(&styled(&d.format("%a %d.%m.%Y").to_string(), &font(14.0, true), &ui::rgb(TEXT), None), CARD_X, mid);
    let Some(day) = s.days.get(&d) else {
        let none = styled("No entry", &font(14.0, false), &ui::rgb(TERTIARY), None);
        text_at(&none, CARD_X + inner - none.size().width.ceil(), mid);
        return;
    };
    if let Some(r) = day.rating.filter(|r| *r < DAY_LABELS.len()) {
        let label = styled(DAY_LABELS[r], &font(12.0, true), &ui::rgb(dark::CANVAS), None);
        let w = label.size().width.ceil() + 20.0;
        let x = CARD_X + inner - w;
        fill_round(rect(x, mid - 10.0, w, 20.0), 10.0, &day_color(r));
        text_at(&label, x + 10.0, mid);
    }
    let mut y = CARD_Y + 20.0;
    let note = day.note.trim();
    if !note.is_empty() {
        let text = card_note(note);
        let h = note_height(&text);
        let options = NSStringDrawingOptions::UsesLineFragmentOrigin | NSStringDrawingOptions::TruncatesLastVisibleLine;
        text.drawWithRect_options_context(rect(CARD_X, y + 10.0, inner, h), options, None);
        y += 10.0 + h;
    }
    y += 12.0;
    for (line, (done, label, run)) in card_habits(s, day).into_iter().enumerate() {
        let top = y + line as f64 * 22.0;
        let b = rect(CARD_X, top + 2.0, 12.0, 12.0);
        if done {
            fill_round(b, 3.0, &ui::rgb(TEXT));
        } else {
            stroke_round(rect(b.origin.x + 0.75, b.origin.y + 0.75, 10.5, 10.5), 2.5, 1.5, &ui::rgb(TERTIARY));
        }
        text_at(&label, CARD_X + 19.0, top + 8.0);
        if let Some(run) = run {
            text_at(&run, CARD_X + inner - run.size().width.ceil(), top + 8.0);
        }
    }
}

/// Read the year and the streaks from the journal file.
fn load(year: i32) {
    let path = crate::journal::file();
    let habits = config::get().journal.habits;
    let keys: Vec<String> = habits.iter().map(|h| h.key.clone()).collect();
    let today = core::now().date();
    let days = core::year(&path, year).into_iter().map(|d| (d.date, d)).collect();
    let streaks = core::streaks(&path, &keys, today);
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.year = year;
        s.habits = habits;
        s.days = days;
        s.streaks = streaks;
        s.today = today;
        s.hover = None;
    });
}

fn hover(d: Option<NaiveDate>) {
    let changed = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let changed = s.hover != d;
        s.hover = d;
        changed
    });
    if changed {
        with_ui(|u| {
            u.board.setNeedsDisplay(true);
            u.place_card();
        });
    }
}

fn set_tab(tab: Tab) {
    STATE.with(|s| s.borrow_mut().tab = tab);
    with_ui(|u| u.refresh());
}

fn step_year(by: i32) {
    let (year, now) = with_state(|s| (s.year, s.today.year()));
    let next = (year + by).min(now);
    if next != year {
        load(next);
        with_ui(|u| u.refresh());
    }
}


impl Ui {
    /// The year with its arrows, the board and the card, from the current state.
    fn refresh(&self) {
        let (year, now) = with_state(|s| (s.year, s.today.year()));
        let title = styled(&year.to_string(), &font(20.0, true), &ui::rgb(TEXT), None);
        let tw = title.size().width.ceil() + 4.0;
        let header_y = H - PAD_Y - HEADER_H;
        self.year_label.setAttributedStringValue(&title);
        self.year_label.setFrame(rect(PAD_X, header_y + (HEADER_H - 26.0) / 2.0, tw, 26.0));
        let arrow = |b: &NSButton, x: f64, text: &str, enabled: bool| {
            let t = centred(text, &font(14.0, true), &ui::rgb(if enabled { SECONDARY } else { TERTIARY }));
            b.setAttributedTitle(&t);
            b.setAttributedAlternateTitle(&t);
            b.setEnabled(enabled);
            b.setFrame(rect(x, header_y + (HEADER_H - 24.0) / 2.0, 22.0, 24.0));
        };
        arrow(&self.prev, PAD_X + tw + 8.0, "‹", true);
        arrow(&self.next, PAD_X + tw + 30.0, "›", year < now);
        // Mood | Streak, right-aligned: the chosen tab on a light track, the other quiet.
        let tab = with_state(|s| s.tab);
        let mut right = W - PAD_X;
        for (i, b) in self.tabs.iter().enumerate().rev() {
            let on = (i == 1) == (tab == Tab::Streak);
            let label = if i == 0 { "Mood" } else { "Streak" };
            let t = centred(label, &font(14.0, true), &ui::rgb(if on { TEXT } else { SECONDARY }));
            let w = (t.size().width + 28.0).ceil();
            b.setAttributedTitle(&t);
            b.setAttributedAlternateTitle(&t);
            b.setFrame(rect(right - w, header_y + (HEADER_H - 28.0) / 2.0, w, 28.0));
            if let Some(l) = b.layer() {
                l.setBackgroundColor(on.then(|| ui::white(0.16).CGColor()).as_deref());
            }
            b.setAccessibilitySelected(on);
            right -= w + 4.0;
        }
        let what = if tab == Tab::Mood { "each day in its rating colour" } else { "brighter for more habits done" };
        self.board.setAccessibilityLabel(Some(&NSString::from_str(&format!("{year}: {what}"))));
        self.board.setNeedsDisplay(true);
        self.place_card();
    }

    /// Above the window (below it when the screen top is too near), centred on the pointed-at square's
    /// column, so the lit days stay in view; hidden when nothing is pointed at.
    fn place_card(&self) {
        let Some((d, h)) = with_state(|s| s.hover.map(|d| (d, card_height(s, d)))) else {
            self.card.orderOut(None);
            return;
        };
        let o = cell_origin(d);
        let cell = self.board.convertRect_toView(rect(o.x, o.y, CELL, CELL), None);
        let cell = self.window.convertRectToScreen(cell);
        let win = self.window.frame();
        let x = (cell.origin.x + CELL / 2.0 - CARD_W / 2.0).min(win.origin.x + win.size.width - CARD_W).max(win.origin.x);
        let top = self.window.screen().map(|s| s.visibleFrame()).map(|f| f.origin.y + f.size.height);
        let above = win.origin.y + win.size.height + 10.0;
        let y = if top.is_some_and(|t| above + h > t) { win.origin.y - 10.0 - h } else { above };
        self.card.setFrame_display(rect(x.round(), y.round(), CARD_W, h), true);
        self.card_view.setNeedsDisplay(true);
        if self.card.parentWindow().is_none() {
            unsafe { self.window.addChildWindow_ordered(&self.card, NSWindowOrderingMode::Above) };
        }
        self.card.orderFrontRegardless();
    }
}

fn panel_like(window: &NSWindow) {
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setHasShadow(false);
    window.setLevel(NSFloatingWindowLevel);
    window.setCollectionBehavior(NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary);
    unsafe { window.setReleasedWhenClosed(false) };
    window.setAppearance(NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }).as_deref());
}

fn plain_button(mtm: MainThreadMarker, parent: &NSView, tag: isize) -> Retained<NSButton> {
    let b = NSButton::new(mtm);
    b.setButtonType(NSButtonType::MomentaryChange);
    b.setBordered(false);
    b.setTag(tag);
    let t = target(mtm);
    let t: &AnyObject = &t;
    unsafe {
        b.setTarget(Some(t));
        b.setAction(Some(sel!(year:)));
    }
    parent.addSubview(&b);
    b
}

fn build(mtm: MainThreadMarker) -> Ui {
    let target = target(mtm);
    let darkness = config::get().glass.max(0.85);
    let this = YearPanel::alloc(mtm).set_ivars(());
    let window: Retained<YearPanel> = unsafe {
        msg_send![super(this), initWithContentRect: rect(0.0, 0.0, W, H),
            styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            backing: NSBackingStoreType::Buffered, defer: false]
    };
    panel_like(&window);
    window.setHidesOnDeactivate(false);
    // A see-through window lets clicks fall through to the app behind; take every click (panel.rs).
    window.setIgnoresMouseEvents(false);
    window.setDelegate(Some(ProtocolObject::from_ref(&*target)));
    let glass = Glass::new(mtm, RADIUS, darkness);
    window.setContentView(Some(&glass.root));
    let v: &NSView = &glass.content;

    let year_label = ui::label(mtm, 20.0, TEXT);
    v.addSubview(&year_label);
    let prev = plain_button(mtm, v, -1);
    prev.setAccessibilityLabel(Some(&NSString::from_str("Previous year")));
    let next = plain_button(mtm, v, 1);
    next.setAccessibilityLabel(Some(&NSString::from_str("Next year")));
    let tab_button = |tag: isize| {
        let b = plain_button(mtm, v, tag);
        let t: &AnyObject = &target;
        unsafe {
            b.setTarget(Some(t));
            b.setAction(Some(sel!(tab:)));
        }
        b.setWantsLayer(true);
        if let Some(l) = b.layer() {
            l.setCornerRadius(9.0);
        }
        b
    };
    let tabs = [tab_button(0), tab_button(1)];

    let board: Retained<Board> = unsafe { msg_send![super(Board::alloc(mtm).set_ivars(())), initWithFrame: rect(PAD_X, PAD_Y, GRID_W, BOARD_H)] };
    v.addSubview(&board);

    // The hover card: its own small glass window above this one, never taking clicks.
    let card = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        rect(0.0, 0.0, CARD_W, 100.0),
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    panel_like(&card);
    card.setHidesOnDeactivate(false);
    card.setIgnoresMouseEvents(true);
    let card_glass = Glass::new(mtm, CARD_RADIUS, darkness);
    card.setContentView(Some(&card_glass.root));
    let card_view: Retained<CardView> = unsafe { msg_send![super(CardView::alloc(mtm).set_ivars(())), initWithFrame: rect(0.0, 0.0, CARD_W, 100.0)] };
    card_view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    card_view.setFrame(card_glass.content.bounds());
    card_glass.content.addSubview(&card_view);

    // Esc and ⌘W close; ← and → change the year.
    let handler = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        let ev = unsafe { event.as_ref() };
        if key_down(ev) { std::ptr::null_mut() } else { event.as_ptr() }
    });
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler) };

    Ui {
        window,
        glass,
        year_label,
        prev,
        next,
        tabs,
        board,
        card,
        _card_glass: card_glass,
        card_view,
        _monitor: monitor,
    }
}

fn key_down(ev: &NSEvent) -> bool {
    let mtm = MainThreadMarker::new().unwrap();
    let ours = with_ui(|u| {
        let ours: &AnyObject = &u.window;
        u.window.isVisible() && ev.window(mtm).is_some_and(|w| { let w: &AnyObject = &w; std::ptr::eq(w, ours) })
    })
    .unwrap_or(false);
    if !ours {
        return false;
    }
    let flags = ev.modifierFlags();
    let cmd = flags.contains(NSEventModifierFlags::Command);
    let key = ev.charactersIgnoringModifiers().map(|s| s.to_string().to_lowercase()).unwrap_or_default();
    match ev.keyCode() {
        53 => close(),
        _ if cmd && key == "w" => close(),
        123 if !cmd => step_year(-1),
        124 if !cmd => step_year(1),
        48 => set_tab(if with_state(|s| s.tab) == Tab::Mood { Tab::Streak } else { Tab::Mood }),
        _ => return false,
    }
    true
}

fn close() {
    STATE.with(|s| s.borrow_mut().hover = None);
    with_ui(|u| {
        u.card.orderOut(None);
        crate::motion::hide(&u.window, crate::motion::Kind::Bloom);
    });
}

fn visible() -> bool {
    with_ui(|u| crate::motion::shown(&u.window)).unwrap_or(false)
}

/// Open on the current year, centred on the screen with the pointer.
pub fn show() {
    let mtm = MainThreadMarker::new().unwrap();
    load(core::now().date().year());
    if UI.with(|u| u.borrow().is_none()) {
        let built = build(mtm);
        UI.with(|u| *u.borrow_mut() = Some(built));
    }
    with_ui(|u| {
        u.refresh();
        let p = NSEvent::mouseLocation();
        let screens = NSScreen::screens(mtm);
        let screen = screens.iter().find(|s| {
            let f = s.frame();
            p.x >= f.origin.x && p.x <= f.origin.x + f.size.width && p.y >= f.origin.y && p.y <= f.origin.y + f.size.height
        });
        let vf = screen.or_else(|| NSScreen::mainScreen(mtm)).map(|s| s.visibleFrame()).unwrap_or(rect(0.0, 0.0, W, H));
        let x = vf.origin.x + ((vf.size.width - W) / 2.0).max(0.0);
        let y = vf.origin.y + ((vf.size.height - H) / 2.0).max(0.0);
        u.window.setFrame_display(rect(x.round(), y.round(), W, H), true);
        ui::present(&u.window);
        crate::motion::show(&u.window, crate::motion::Kind::Bloom);
    });
    // A menu-bar app must come forward, or the window gives up the keyboard at once and closes.
    if !crate::app::offscreen() {
        #[allow(deprecated)]
        objc2_app_kit::NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    }
}

/// `--year`: the same window; under `--test` it reads the test journal (never the vault), under
/// `--offscreen` it opens outside every screen without the focus. `--streak` opens the Streak tab and
/// `--hover YYYY-MM-DD` points at that day and `--legend N` at the Nth legend item, so the focus view can be checked without the pointer.
pub fn show_test(args: &[String]) {
    show();
    if args.iter().any(|a| a == "--streak") {
        set_tab(Tab::Streak);
    }
    let day = args.iter().position(|a| a == "--hover").and_then(|i| args.get(i + 1));
    if let Some(d) = day.and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()) {
        hover(Some(d));
    }
    let item = args.iter().position(|a| a == "--legend").and_then(|i| args.get(i + 1)?.parse::<usize>().ok());
    if let Some(g) = item.and_then(|i| with_state(|s| legend_layout(s).get(i).map(|l| l.1))) {
        STATE.with(|s| s.borrow_mut().legend_hover = Some(g));
        with_ui(|u| u.board.setNeedsDisplay(true));
    }
}

/// Read the file again while the window is open (the journal calls this after a save).
pub fn refresh() {
    if !visible() {
        return;
    }
    let year = with_state(|s| s.year);
    load(year);
    with_ui(|u| u.refresh());
}

/// The Settings glass slider, live.
pub fn set_glass(darkness: f64) {
    with_ui(|u| {
        u.glass.set_darkness(darkness.max(0.85));
        u._card_glass.set_darkness(darkness.max(0.85));
    });
}
