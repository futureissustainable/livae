//! Mac's year board in Direct2D: Monday-first weeks, Mood/Streak, matching-day focus and a passive hover card.
//! These surfaces subclass the existing glass window locally, keeping the shared drawing code unchanged.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::OnceLock;
use std::time::SystemTime;

use livae_core::journal::{self as core, DAY_COLORS, DAY_LABELS, Datelike, DaySummary, NaiveDate};
use livae_core::settings::Habit;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_MEDIUM, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_LINE_SPACING_METHOD_UNIFORM, DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_METRICS,
    IDWriteTextLayout,
};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ValidateRect};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE, VK_LEFT, VK_RIGHT, VK_TAB};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::glass::{self, Canvas, Font, Rgba, SECONDARY, Surface, TERTIARY, TEXT, gfx};
use crate::{app, config, journal};

const PAD_X: f32 = 34.0;
const PAD_Y: f32 = 28.0;
const CELL: f32 = 12.0;
const PITCH: f32 = 17.0;
const GRID_X: f32 = 26.0;
const GRID_Y: f32 = 26.0;
const GRID_W: f32 = GRID_X + 54.0 * PITCH;
const GRID_H: f32 = GRID_Y + 7.0 * PITCH;
const BOARD_Y: f32 = PAD_Y + 28.0 + 22.0;
const FOOT_Y: f32 = BOARD_Y + GRID_H + 18.0;
const W: f32 = 2.0 * PAD_X + GRID_W;
const CARD_W: f32 = 300.0;
const CARD_X: f32 = 18.0;
const CARD_Y: f32 = 16.0;
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const LETTERS: [&str; 7] = ["M", "T", "W", "T", "F", "S", "S"];
const BOARD: isize = 100;
const CARD: isize = 101;

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Tab {
    #[default]
    Mood,
    Streak,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Group {
    Rating(usize),
    NoEntry,
    Habits(usize),
}
#[derive(Default)]
struct State {
    year: i32,
    today: NaiveDate,
    days: BTreeMap<NaiveDate, DaySummary>,
    habits: Vec<Habit>,
    streaks: Vec<(String, u32)>,
    hover: Option<NaiveDate>,
    legend: Option<Group>,
    tab: Tab,
    stamp: Option<SystemTime>,
}
struct Ui {
    board: Surface,
    card: Surface,
    state: State,
    scale: f32,
    x: i32,
    y: i32,
    work: RECT,
    prev: HWND,
    showing: bool,
    card_day: Option<NaiveDate>,
}
thread_local! { static UI: RefCell<Option<Ui>> = const { RefCell::new(None) }; }
static ORIGINAL: OnceLock<isize> = OnceLock::new();

fn with<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|cell| cell.try_borrow_mut().ok().and_then(|mut slot| slot.as_mut().map(f)))
}
fn format(size: f32, medium: bool) -> windows::Win32::Graphics::DirectWrite::IDWriteTextFormat {
    gfx().format(Font::Sans, size, if medium { DWRITE_FONT_WEIGHT_MEDIUM } else { DWRITE_FONT_WEIGHT_NORMAL }, false, false)
}
fn width(s: &str, size: f32, medium: bool) -> f32 {
    gfx().width(s, &format(size, medium)).ceil()
}
#[allow(clippy::too_many_arguments)] // a thin wrapper over Canvas::text: text, style, then the box and colour
fn text(c: &Canvas, s: &str, size: f32, medium: bool, x: f32, y: f32, w: f32, h: f32, color: Rgba) {
    c.text(s, &format(size, medium), x, y, w, h, color);
}
fn day_color(i: usize) -> Rgba {
    let (r, g, b) = DAY_COLORS[i];
    Rgba(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
}
fn jan1(year: i32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, 1, 1).unwrap_or_default()
}
fn lead(year: i32) -> i64 {
    jan1(year).weekday().num_days_from_monday() as i64
}
fn cell(d: NaiveDate) -> (f32, f32) {
    let n = d.ordinal0() as i64 + lead(d.year());
    (PAD_X + GRID_X + (n / 7) as f32 * PITCH, BOARD_Y + GRID_Y + (n % 7) as f32 * PITCH)
}
fn date_at(s: &State, x: f32, y: f32) -> Option<NaiveDate> {
    let (x, y) = (x - PAD_X - GRID_X, y - BOARD_Y - GRID_Y);
    if x < 0.0 || y < 0.0 || x >= 54.0 * PITCH || y >= 7.0 * PITCH {
        return None;
    }
    let n = (x / PITCH) as i64 * 7 + (y / PITCH) as i64 - lead(s.year);
    u32::try_from(n).ok().and_then(|n| NaiveDate::from_yo_opt(s.year, n + 1))
}
fn done(day: &DaySummary, key: &str) -> bool {
    day.habits.iter().any(|(k, v)| k == key && *v)
}
fn habits_done(s: &State, day: &DaySummary) -> (usize, usize) {
    let answered: Vec<bool> = s.habits.iter().filter_map(|h| day.habits.iter().find(|(k, _)| *k == h.key).map(|(_, v)| *v)).collect();
    (answered.iter().filter(|v| **v).count(), answered.len())
}
fn streak_alpha(done: usize, total: usize) -> f32 {
    match (done, total) {
        (0, _) | (_, 0) => 0.07,
        (_, 1) => 1.0,
        (d, t) => 0.35 + 0.65 * (d.min(t) - 1) as f32 / (t - 1) as f32,
    }
}
fn group(s: &State, d: NaiveDate) -> Option<Group> {
    if d > s.today {
        return None;
    }
    let Some(day) = s.days.get(&d) else {
        return Some(Group::NoEntry);
    };
    Some(match s.tab {
        Tab::Mood => day.rating.filter(|r| *r < DAY_COLORS.len()).map(Group::Rating).unwrap_or(Group::NoEntry),
        Tab::Streak => Group::Habits(habits_done(s, day).0),
    })
}
fn focus(s: &State) -> Option<Group> {
    s.legend.or_else(|| s.hover.and_then(|d| group(s, d)))
}
fn count(s: &State, g: Group) -> u32 {
    jan1(s.year).iter_days().take_while(|d| d.year() == s.year && *d <= s.today).filter(|d| group(s, *d) == Some(g)).count() as u32
}
fn streak_at(s: &State, key: &str, mut d: NaiveDate) -> u32 {
    let mut n = 0;
    while s.days.get(&d).is_some_and(|day| done(day, key)) {
        n += 1;
        let Some(prev) = d.pred_opt() else { break };
        d = prev;
    }
    n
}
fn groups(s: &State) -> Vec<(Group, Rgba, String)> {
    match s.tab {
        Tab::Mood => DAY_LABELS
            .iter()
            .enumerate()
            .map(|(i, l)| (Group::Rating(i), day_color(i), l.to_string()))
            .chain(Some((Group::NoEntry, glass::white(0.07), "No entry".into())))
            .collect(),
        Tab::Streak => (0..=s.habits.len())
            .map(|d| {
                let n = s.habits.len();
                let label = match (d, n) {
                    (0, _) => "None".into(),
                    (1, 1) => "Done".into(),
                    (2, 2) => "Both habits".into(),
                    (d, n) if d == n => "All habits".into(),
                    (1, _) => "1 habit".into(),
                    (d, _) => format!("{d} habits"),
                };
                (Group::Habits(d), glass::white(streak_alpha(d, n)), label)
            })
            .collect(),
    }
}
struct Legend {
    x: f32,
    y: f32,
    w: f32,
    group: Group,
    color: Rgba,
    label: String,
}
fn legend(s: &State) -> Vec<Legend> {
    let (mut x, mut y) = (PAD_X, FOOT_Y);
    groups(s)
        .into_iter()
        .map(|(g, color, label)| {
            let on = focus(s) == Some(g);
            let label = if on {
                let n = count(s, g);
                format!("{label}  {n} {}", if n == 1 { "day" } else { "days" })
            } else {
                label
            };
            let w = CELL + 6.0 + width(&label, 12.0, on);
            if x + w > W - PAD_X {
                x = PAD_X;
                y += 28.0;
            }
            let item = Legend { x, y, w, group: g, color, label };
            x += w + 14.0;
            item
        })
        .collect()
}
/// Share the Mac footer's row while it fits; extra habits wrap instead of covering the legend.
fn habit_layout(s: &State) -> Vec<(f32, f32, f32, &Habit, String)> {
    let legends = legend(s);
    let last = legends.last();
    let (mut x, mut y) = last.map_or((PAD_X, FOOT_Y), |l| (l.x + l.w + 24.0, l.y));
    let mut out = Vec::new();
    for h in &s.habits {
        let n = s.streaks.iter().find(|(k, _)| *k == h.key).map_or(0, |(_, n)| *n);
        let value = format!("{n}-day streak");
        let w = width(&h.label, 14.0, false) + 8.0 + width(&value, 14.0, true);
        if x + w > W - PAD_X {
            x = PAD_X;
            y += 28.0;
        }
        out.push((x, y, w, h, value));
        x += w + 24.0;
    }
    // Right-align a single footer row, as on the Mac.
    if out.iter().all(|(_, y, ..)| *y == FOOT_Y) && !out.is_empty() {
        let shift = (W - PAD_X - (x - 24.0)).max(0.0);
        for (x, ..) in &mut out {
            *x += shift;
        }
    }
    out
}
fn height(s: &State) -> f32 {
    let last_legend = legend(s).last().map_or(FOOT_Y, |l| l.y);
    let last_habit = habit_layout(s).last().map_or(FOOT_Y, |(_, y, ..)| *y);
    last_legend.max(last_habit) + 20.0 + PAD_Y
}
fn note_layout(note: &str) -> Option<(IDWriteTextLayout, f32)> {
    let fmt = gfx().format(Font::Sans, 14.0, DWRITE_FONT_WEIGHT_NORMAL, true, false);
    let layout = gfx().layout(note, &fmt, CARD_W - CARD_X * 2.0, 10_000.0)?;
    let mut metrics = DWRITE_TEXT_METRICS::default();
    unsafe {
        let _ = layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
        let _ = layout.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, 20.0, 15.0);
        layout.GetMetrics(&mut metrics).ok()?;
        let h = metrics.height.ceil().min(80.0);
        let _ = layout.SetMaxHeight(h);
        Some((layout, h))
    }
}
fn card_height(s: &State, d: NaiveDate) -> f32 {
    let Some(day) = s.days.get(&d) else {
        return CARD_Y * 2.0 + 20.0;
    };
    let note = day.note.trim();
    CARD_Y * 2.0
        + 20.0
        + if note.is_empty() { 0.0 } else { 10.0 + note_layout(note).map_or(20.0, |(_, h)| h) }
        + if s.habits.is_empty() { 0.0 } else { 12.0 + s.habits.len() as f32 * 22.0 - 6.0 }
}
fn tabs() -> [(Tab, &'static str, f32, f32); 2] {
    let streak_w = width("Streak", 14.0, true) + 28.0;
    let mood_w = width("Mood", 14.0, true) + 28.0;
    let right = W - PAD_X;
    [(Tab::Mood, "Mood", right - streak_w - 4.0 - mood_w, mood_w), (Tab::Streak, "Streak", right - streak_w, streak_w)]
}
fn draw_board(c: &mut Canvas, s: &State) {
    c.glass(config::get().glass.max(0.85) as f32);
    let year_w = width(&s.year.to_string(), 20.0, true) + 4.0;
    text(c, &s.year.to_string(), 20.0, true, PAD_X, PAD_Y, year_w, 28.0, TEXT);
    for (label, offset, color) in [("‹", 8.0, SECONDARY), ("›", 30.0, if s.year < s.today.year() { SECONDARY } else { TERTIARY })] {
        let tw = width(label, 14.0, true);
        text(c, label, 14.0, true, PAD_X + year_w + offset + (22.0 - tw) / 2.0, PAD_Y, tw, 28.0, color);
    }
    for (tab, label, x, w) in tabs() {
        let on = s.tab == tab;
        if on {
            c.round(x, PAD_Y, w, 28.0, 9.0, glass::white(0.16));
        }
        text(c, label, 14.0, true, x + 14.0, PAD_Y, w - 28.0, 28.0, if on { TEXT } else { SECONDARY });
    }
    for (i, name) in MONTHS.iter().enumerate() {
        if let Some(d) = NaiveDate::from_ymd_opt(s.year, i as u32 + 1, 1) {
            text(c, name, 12.0, false, cell(d).0, BOARD_Y, 40.0, 20.0, TERTIARY);
        }
    }
    for (i, letter) in LETTERS.iter().enumerate() {
        text(c, letter, 12.0, true, PAD_X, BOARD_Y + GRID_Y + i as f32 * PITCH - 4.0, 20.0, 20.0, SECONDARY);
    }
    let focused = focus(s);
    for d in jan1(s.year).iter_days().take_while(|d| d.year() == s.year) {
        let (x, y) = cell(d);
        let mut color = if d > s.today {
            glass::white(0.03)
        } else if let Some(day) = s.days.get(&d) {
            match s.tab {
                Tab::Mood => day.rating.filter(|r| *r < DAY_COLORS.len()).map(day_color).unwrap_or(glass::white(0.07)),
                Tab::Streak => {
                    let (done, total) = habits_done(s, day);
                    glass::white(streak_alpha(done, total))
                }
            }
        } else {
            glass::white(0.07)
        };
        if let Some(g) = focused
            && d <= s.today {
                if group(s, d) != Some(g) {
                    if s.tab == Tab::Mood {
                        color.3 *= 0.22;
                    } else {
                        color = glass::white(0.04);
                    }
                } else if matches!(g, Group::NoEntry | Group::Habits(0)) {
                    color = glass::white(0.22);
                }
            }
        c.round(x, y, CELL, CELL, 2.6, color);
        if d == s.today {
            c.outline(x + 0.5, y + 0.5, CELL - 1.0, CELL - 1.0, 2.4, 1.0, glass::white(0.6), Some(&[2.0, 2.0]));
        }
        if s.hover == Some(d) {
            c.outline(x - 3.5, y - 3.5, CELL + 7.0, CELL + 7.0, 6.0, 1.5, TEXT, None);
        }
    }
    for item in legend(s) {
        let on = focused == Some(item.group);
        c.round(item.x, item.y + 4.0, CELL, CELL, 2.6, item.color);
        text(c, &item.label, 12.0, on, item.x + CELL + 6.0, item.y, item.w - CELL - 6.0, 20.0, if on { TEXT } else { TERTIARY });
    }
    for (x, y, w, habit, value) in habit_layout(s) {
        let vw = width(&value, 14.0, true);
        text(c, &habit.label, 14.0, false, x, y, (w - vw - 8.0).max(1.0), 20.0, SECONDARY);
        text(c, &value, 14.0, true, x + w - vw, y, vw, 20.0, if value.starts_with("0-") { TERTIARY } else { TEXT });
    }
}
fn draw_card(c: &mut Canvas, s: &State, day: Option<NaiveDate>) {
    let Some(d) = day else { return };
    c.glass(config::get().glass.max(0.85) as f32);
    let inner = CARD_W - 2.0 * CARD_X;
    text(c, &d.format("%a %d.%m.%Y").to_string(), 14.0, true, CARD_X, CARD_Y, inner, 20.0, TEXT);
    let Some(day) = s.days.get(&d) else {
        let w = width("No entry", 14.0, false);
        text(c, "No entry", 14.0, false, CARD_W - CARD_X - w, CARD_Y, w, 20.0, TERTIARY);
        return;
    };
    if let Some(r) = day.rating.filter(|r| *r < DAY_LABELS.len()) {
        let label = DAY_LABELS[r];
        let w = width(label, 12.0, true) + 20.0;
        let x = CARD_W - CARD_X - w;
        c.round(x, CARD_Y, w, 20.0, 10.0, day_color(r));
        text(c, label, 12.0, true, x + 10.0, CARD_Y, w - 20.0, 20.0, Rgba(0.0, 0.0, 0.0, 1.0));
    }
    let mut y = CARD_Y + 20.0;
    if !day.note.trim().is_empty()
        && let Some((layout, h)) = note_layout(day.note.trim()) {
            c.layout(&layout, CARD_X, y + 10.0, TEXT);
            y += 10.0 + h;
        }
    y += 12.0;
    for habit in &s.habits {
        let on = done(day, &habit.key);
        if on {
            c.round(CARD_X, y + 2.0, 12.0, 12.0, 3.0, TEXT);
        } else {
            c.outline(CARD_X + 0.75, y + 2.75, 10.5, 10.5, 2.5, 1.5, TERTIARY, None);
        }
        let run = if on { format!("{}-day streak", streak_at(s, &habit.key, d)) } else { String::new() };
        let rw = width(&run, 13.0, false);
        text(c, &habit.label, 13.0, false, CARD_X + 19.0, y - 2.0, (inner - 19.0 - rw - 8.0).max(1.0), 20.0, if on { TEXT } else { TERTIARY });
        if on {
            text(c, &run, 13.0, false, CARD_W - CARD_X - rw, y - 2.0, rw, 20.0, SECONDARY);
        }
        y += 22.0;
    }
}

fn load(s: &mut State, year: i32) {
    let path = journal::file();
    s.year = year;
    s.today = core::now().date();
    s.habits = config::get().journal.habits;
    s.days = core::year(&path, year).into_iter().map(|d| (d.date, d)).collect();
    s.streaks = core::streaks(&path, &s.habits.iter().map(|h| &h.key).collect::<Vec<_>>(), s.today);
    s.stamp = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    s.hover = None;
    s.legend = None;
}
impl Ui {
    fn px(&self, dip: f32) -> i32 {
        (dip * self.scale).round() as i32
    }
    fn paint(&mut self) {
        self.board.draw(self.scale, |c| draw_board(c, &self.state));
        self.card.draw(self.scale, |c| draw_card(c, &self.state, self.card_day));
    }
    fn place(&mut self) {
        unsafe {
            let _ = SetWindowPos(self.board.hwnd, None, 0, 0, self.px(W), self.px(height(&self.state)), SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        let Some(d) = self.state.hover.filter(|_| self.showing) else {
            if let Some(day) = self.card_day {
                self.card.exit((CARD_W / 2.0, card_height(&self.state, day) / 2.0));
            }
            return;
        };
        self.card_day = Some(d);
        let h = self.px(card_height(&self.state, d));
        let x = glass::fit(self.x + self.px(cell(d).0 + CELL / 2.0 - CARD_W / 2.0), self.x, self.x + self.px(W - CARD_W));
        let above = self.y - 10 - h;
        let y = if above < self.work.top && !config::offscreen() { self.y + self.px(height(&self.state)) + 10 } else { above };
        self.card.enter(crate::motion::Family::Bloom, (CARD_W / 2.0, card_height(&self.state, d) / 2.0));
        unsafe {
            let _ = SetWindowPos(self.card.hwnd, Some(HWND_TOPMOST), x, y, self.px(CARD_W), h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
        self.card.keep_lit();
    }
    fn hide(&mut self, restore: bool) {
        self.showing = false;
        unsafe {
            let _ = KillTimer(Some(self.board.hwnd), 1);
            if restore && GetForegroundWindow() == self.board.hwnd && IsWindow(Some(self.prev)).as_bool() {
                let _ = SetForegroundWindow(self.prev);
            }
        }
        if let Some(day) = self.card_day {
            self.card.exit((CARD_W / 2.0, card_height(&self.state, day) / 2.0));
        }
        self.board.exit((W / 2.0, height(&self.state) / 2.0));
    }
    fn step(&mut self, by: i32) {
        let year = (self.state.year + by).min(self.state.today.year());
        if year != self.state.year && NaiveDate::from_ymd_opt(year, 1, 1).is_some() {
            load(&mut self.state, year);
            self.place();
            self.paint();
        }
    }
    fn tab(&mut self, tab: Tab) {
        self.state.tab = tab;
        self.state.legend = None;
        self.place();
        self.paint();
    }
}
fn build() -> Ui {
    let board = Surface::new(BOARD, None, true);
    let card = Surface::new(CARD, Some(board.hwnd), false);
    unsafe {
        let old = SetWindowLongPtrW(board.hwnd, GWLP_WNDPROC, proc as *const () as isize);
        let _ = ORIGINAL.set(old);
        SetWindowLongPtrW(card.hwnd, GWLP_WNDPROC, proc as *const () as isize);
        let style = GetWindowLongPtrW(card.hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(card.hwnd, GWL_EXSTYLE, style | WS_EX_TRANSPARENT.0 as isize);
    }
    Ui { board, card, state: State::default(), scale: 1.0, x: 0, y: 0, work: RECT::default(), prev: HWND::default(), showing: false, card_day: None }
}
pub fn show() {
    let needs_build = UI.with(|u| u.borrow().is_none());
    if needs_build {
        let built = build();
        UI.with(|u| *u.borrow_mut() = Some(built));
    }
    with(|u| {
        if u.board.visible() {
            u.showing = true;
            u.board.enter(crate::motion::Family::Bloom, (W / 2.0, height(&u.state) / 2.0));
            u.place();
            u.paint();
            unsafe { SetTimer(Some(u.board.hwnd), 1, 1000, None); }
            if !config::offscreen() {
                glass::bring_to_front(u.board.hwnd);
            }
            return;
        }
        load(&mut u.state, core::now().date().year());
        let monitor = glass::monitor_at(glass::cursor());
        u.scale = monitor.scale;
        u.work = monitor.work;
        let (w, h) = (u.px(W), u.px(height(&u.state)));
        (u.x, u.y) = if config::offscreen() {
            glass::offscreen_origin()
        } else {
            (
                monitor.work.left + ((monitor.work.right - monitor.work.left - w) / 2).max(0),
                monitor.work.top + ((monitor.work.bottom - monitor.work.top - h) / 2).max(0),
            )
        };
        let front = unsafe { GetForegroundWindow() };
        if !app::is_ours(front) {
            u.prev = front;
        }
        if !config::offscreen() {
            // Capture the board and card area together before either window is visible.
            let (x, y, h) = (u.x, (u.y - u.px(400.0)).max(monitor.full.top), u.px(height(&u.state) + 800.0));
            let backdrop = glass::screen_copy(x, y, w, h).map(|p| glass::blurred(&p, x, y, u.scale, glass::DIM));
            u.board.backdrop = backdrop.clone();
            u.card.backdrop = backdrop;
        }
        u.board.enter(crate::motion::Family::Bloom, (W / 2.0, height(&u.state) / 2.0));
        u.showing = true;
        unsafe {
            let flags = SWP_SHOWWINDOW | if config::offscreen() { SWP_NOACTIVATE } else { SET_WINDOW_POS_FLAGS(0) };
            let _ = SetWindowPos(u.board.hwnd, Some(HWND_TOPMOST), u.x, u.y, w, h, flags);
            let _ = SetTimer(Some(u.board.hwnd), 1, 1000, None);
        }
        u.place();
        u.paint();
        if !config::offscreen() {
            glass::bring_to_front(u.board.hwnd);
        }
    });
}
pub fn show_test() {
    show();
    with(|u| {
        if config::flag("--streak") {
            u.state.tab = Tab::Streak;
        }
        u.state.hover = config::value("--hover").and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()).filter(|d| d.year() == u.state.year);
        if let Some(i) = config::value("--legend").and_then(|s| s.parse::<usize>().ok()) {
            u.state.legend = groups(&u.state).get(i).map(|(g, ..)| *g);
        }
        u.place();
        u.paint();
    });
}
pub fn dump() -> String {
    with(|u| {
        let s = &u.state;
        let mut out = format!(
            "year {}\ntab {}\ndays {}\nentries {}\n",
            s.year,
            if s.tab == Tab::Mood { "Mood" } else { "Streak" },
            jan1(s.year).iter_days().take_while(|d| d.year() == s.year).count(),
            s.days.len()
        );
        for (g, _, label) in groups(s) {
            let _ = writeln!(out, "count {label}: {}", count(s, g));
        }
        if s.tab == Tab::Streak {
            let _ = writeln!(out, "count No entry: {}", count(s, Group::NoEntry));
        }
        if let Some(g) = focus(s) {
            let _ = writeln!(out, "focus {g:?}: {}", count(s, g));
        }
        for (key, run) in &s.streaks {
            let _ = writeln!(out, "streak {key}: {run}");
        }
        if let Some(d) = s.hover {
            let _ = writeln!(out, "hover {d}\n{}", d.format("%a %d.%m.%Y"));
            if let Some(day) = s.days.get(&d) {
                let _ = writeln!(out, "rating {}\nnote {}", day.rating.and_then(|r| DAY_LABELS.get(r)).unwrap_or(&"Unrated"), day.note.trim());
                for habit in &s.habits {
                    let on = done(day, &habit.key);
                    let _ = writeln!(
                        out,
                        "habit {}: {} ({}-day streak)",
                        habit.label,
                        if on { "Yes" } else { "No" },
                        if on { streak_at(s, &habit.key, d) } else { 0 }
                    );
                }
            } else {
                out.push_str("No entry\n");
            }
        }
        out
    })
    .unwrap_or_default()
}
extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let card = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } == CARD;
    if matches!(msg, WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_KEYDOWN) && with(|u| !u.showing).unwrap_or(true) {
        return LRESULT(0);
    }
    match msg {
        WM_PAINT => {
            with(|u| {
                if card {
                    u.card.draw(u.scale, |c| draw_card(c, &u.state, u.card_day));
                } else {
                    u.board.draw(u.scale, |c| draw_board(c, &u.state));
                }
            });
            let _ = unsafe { ValidateRect(Some(hwnd), None) };
            return LRESULT(0);
        }
        WM_NCHITTEST if card => return LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE if card => return LRESULT(MA_NOACTIVATE as isize),
        WM_CLOSE if !card => {
            with(|u| u.hide(true));
            return LRESULT(0);
        }
        WM_ACTIVATE if !card && wp.0 & 0xffff == WA_INACTIVE as usize && !config::test_mode() => {
            with(|u| u.hide(false));
        }
        WM_KEYDOWN if !card => {
            with(|u| match wp.0 as u16 {
                k if k == VK_ESCAPE.0 => u.hide(true),
                k if k == VK_LEFT.0 => u.step(-1),
                k if k == VK_RIGHT.0 => u.step(1),
                k if k == VK_TAB.0 => u.tab(if u.state.tab == Tab::Mood { Tab::Streak } else { Tab::Mood }),
                _ => {}
            });
            return LRESULT(0);
        }
        WM_MOUSEMOVE if !card => {
            let (x, y) = ((lp.0 & 0xffff) as u16 as i16 as f32, ((lp.0 >> 16) & 0xffff) as u16 as i16 as f32);
            with(|u| {
                let (x, y) = (x / u.scale, y / u.scale);
                let on = legend(&u.state).into_iter().find(|l| x >= l.x && x < l.x + l.w && y >= l.y && y < l.y + 20.0).map(|l| l.group);
                let day = if on.is_some() { None } else { date_at(&u.state, x, y) };
                if u.state.hover != day || u.state.legend != on {
                    u.state.hover = day;
                    u.state.legend = on;
                    u.place();
                    u.paint();
                }
                unsafe {
                    let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, ..Default::default() };
                    let _ = TrackMouseEvent(&mut track);
                }
            });
            return LRESULT(0);
        }
        WM_MOUSELEAVE if !card => {
            with(|u| {
                u.state.hover = None;
                u.state.legend = None;
                u.place();
                u.paint();
            });
            return LRESULT(0);
        }
        WM_LBUTTONDOWN if !card => {
            with(|u| {
                let (x, y) = ((lp.0 & 0xffff) as u16 as i16 as f32 / u.scale, ((lp.0 >> 16) & 0xffff) as u16 as i16 as f32 / u.scale);
                if (PAD_Y - 8.0..PAD_Y + 36.0).contains(&y) {
                    let arrow_x = PAD_X + width(&u.state.year.to_string(), 20.0, true) + 4.0 + 8.0;
                    if (arrow_x..arrow_x + 22.0).contains(&x) {
                        u.step(-1);
                    } else if (arrow_x + 22.0..arrow_x + 44.0).contains(&x) {
                        u.step(1);
                    } else if let Some((tab, ..)) = tabs().into_iter().find(|(_, _, tx, tw)| x >= *tx && x < tx + tw) {
                        u.tab(tab);
                    }
                }
            });
            return LRESULT(0);
        }
        WM_TIMER if !card && wp.0 == 1 => {
            with(|u| {
                let path = journal::file();
                let stamp = std::fs::metadata(path).and_then(|m| m.modified()).ok();
                if u.state.stamp != stamp || u.state.today != core::now().date() || u.state.habits != config::get().journal.habits {
                    let (hover, legend, year) = (u.state.hover, u.state.legend, u.state.year);
                    load(&mut u.state, year);
                    u.state.hover = hover;
                    u.state.legend = legend;
                    u.place();
                    u.paint();
                } else {
                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                }
            });
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe {
        match ORIGINAL.get().copied().filter(|p| *p != 0) {
            Some(original) => CallWindowProcW(std::mem::transmute::<isize, WNDPROC>(original), hwnd, msg, wp, lp),
            None => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn year_counts_future_missing_and_new_habits_like_the_mac() {
        let d = |m, d| NaiveDate::from_ymd_opt(2024, m, d).unwrap();
        let mut s = State {
            year: 2024,
            today: d(3, 1),
            habits: vec![Habit { key: "Read".into(), label: "Read".into() }, Habit { key: "Added later".into(), label: "Added later".into() }],
            ..Default::default()
        };
        for date in [d(2, 28), d(2, 29)] {
            s.days.insert(date, DaySummary { date, rating: Some(4), habits: vec![("Read".into(), true)], note: "Leap year".into() });
        }
        assert_eq!(count(&s, Group::Rating(4)), 2);
        assert_eq!(count(&s, Group::NoEntry), 59);
        assert_eq!(group(&s, d(3, 2)), None);
        assert_eq!(streak_at(&s, "Read", d(2, 29)), 2);
        s.tab = Tab::Streak;
        assert_eq!(count(&s, Group::Habits(1)), 2);
        assert_eq!(habits_done(&s, &s.days[&d(2, 29)]), (1, 1));
        assert_eq!(streak_alpha(1, 1), 1.0);
        for date in jan1(2024).iter_days().take_while(|d| d.year() == 2024) {
            let (x, y) = cell(date);
            assert_eq!(date_at(&s, x + 6.0, y + 6.0), Some(date));
        }
        assert_eq!(date_at(&s, PAD_X + GRID_X, BOARD_Y + GRID_Y), Some(d(1, 1)));
    }
}
