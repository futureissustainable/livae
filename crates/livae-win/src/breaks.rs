//! Break reminders, replacing Stretchly Zen, in the Mac's design A. A 1 s tick feeds
//! `livae_core::breaks::Scheduler` with idle time, lock, sleep and display power, Focus assist, and whether
//! Stretchly still runs. A break covers every monitor edge to edge with the blurred screen under a black shade, one
//! quote and a thin line that drains as the break runs; a toast counts down to it. It fades in from the screen as it
//! was and back out to it. Quotes live in `quotes.md` in the data folder.

use std::cell::RefCell;
use std::ffi::c_void;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

use livae_core::breaks::{self, Action, Break, Deck, NaiveDateTime, Quote, Rules, Scheduler};
use windows::core::{s, w, Interface, BOOL};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextFormat, IDWriteTextLayout, DWRITE_FONT_WEIGHT_MEDIUM, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_LINE_METRICS, DWRITE_LINE_SPACING_METHOD_UNIFORM,
    DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS,
};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::glass::{self, gfx, Font, Rgba, Surface, SECONDARY, TEXT};
use crate::shell::{self, Pixels};
use crate::{app, config};

const STRETCHLY: &str = "Stretchly.exe";
/// The quote: 34 regular, line height 1.3, at most 720 wide; its block centred 45% down the screen.
const QUOTE: f32 = 34.0;
const QUOTE_LEADING: f32 = 1.3;
const MEASURE: f32 = 720.0;
const FROM_TOP: f32 = 0.45;
const AUTHOR_GAP: f32 = 20.0;
const AUTHOR_H: f32 = 24.0;
/// The time line: 240 x 2, 72 above the bottom of the screen.
const LINE_W: f32 = 240.0;
const LINE_H: f32 = 2.0;
const LINE_Y: f32 = 72.0;
const TIME_GAP: f32 = 12.0;
const ROW_H: f32 = 20.0;
const CONTROL_GAP: f32 = 24.0;
const HIT: f32 = 44.0;
const FADE_IN_MS: f32 = 600.0;
const FADE_OUT_MS: f32 = 400.0;
/// Redraws: 60 a second while fading, 30 while the line drains.
const FRAME_TIMER: usize = 1;

#[derive(Clone, Copy, Default)]
struct Away {
    locked: bool,
    asleep: bool,
    displays_off: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Fade {
    In(Instant),
    Shown,
    Out(Instant),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Control {
    Postpone,
    Skip,
}

/// One break window, on one monitor.
struct Screen {
    surface: Surface,
    scale: f32,
    /// The monitor as it was when the break began: the break fades in from it and back out to it.
    before: Option<Pixels>,
    words: Option<IDWriteTextLayout>,
    words_h: f32,
}

struct State {
    labels: Labels,
    scheduler: Scheduler,
    deck: Deck,
    ticks: u64,
    stretchly: bool,
    focus: bool,
    away: Away,
    screens: Vec<Screen>,
    quote: Quote,
    skip: bool,
    /// Minutes a long break can be put off by, when it can.
    postpone: Option<u32>,
    fade: Fade,
    /// The break on screen, kept for its fade-out after the scheduler has ended it.
    shown: Option<Break>,
    /// The window in front before the break; it gets the keyboard back as the break fades out.
    prev: HWND,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Runs `f` on the breaks. Re-entrant calls (Windows calling back while we hold it) are skipped.
fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|cell| cell.try_borrow_mut().ok().and_then(|mut slot| slot.as_mut().map(f)))
}

fn rules() -> Rules {
    config::get().breaks.rules()
}

fn quotes_file() -> PathBuf {
    config::data_dir().join("quotes.md")
}

fn unix_now() -> f64 {
    breaks::clock().0
}

/// Starts the scheduler (the app ticks it every second). Writes the built-in quotes to `quotes.md` once.
pub fn install() {
    let path = quotes_file();
    if !path.exists() && !config::test_mode() {
        let _ = livae_core::write_atomic(&path, breaks::DEFAULT_QUOTES.as_bytes());
    }
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
    let st = State {
        labels: Labels::default(),
        scheduler: Scheduler::new(),
        deck: Deck::new(seed),
        ticks: 0,
        stretchly: false,
        focus: false,
        away: Away::default(),
        screens: Vec::new(),
        quote: Quote { text: String::new(), author: None },
        skip: false,
        postpone: None,
        fade: Fade::Shown,
        shown: None,
        prev: HWND::default(),
    };
    STATE.with(|c| *c.borrow_mut() = Some(st));
}

fn idle_seconds() -> f64 {
    let mut lii = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    if !unsafe { GetLastInputInfo(&mut lii) }.as_bool() {
        return 0.0;
    }
    unsafe { GetTickCount() }.wrapping_sub(lii.dwTime) as f64 / 1000.0
}

/// Focus assist (Do not disturb) is on: the shell's quiet-hours profile, read the way Stretchly reads it on
/// Windows (ntdll's NtQueryWnfStateData; profile 0 is off). Unreadable counts as off.
fn focus_assist() -> bool {
    type Query = unsafe extern "system" fn(*const u64, *const c_void, *const c_void, *mut u32, *mut c_void, *mut u32) -> i32;
    static QUERY: OnceLock<Option<Query>> = OnceLock::new();
    const QUIET_HOURS_PROFILE: u64 = 0x0D83_063E_A3BF_1C75;
    let query = QUERY.get_or_init(|| unsafe {
        let ntdll = GetModuleHandleW(w!("ntdll.dll")).ok()?;
        GetProcAddress(ntdll, s!("NtQueryWnfStateData")).map(|f| std::mem::transmute::<unsafe extern "system" fn() -> isize, Query>(f))
    });
    let Some(query) = query else { return false };
    let (mut stamp, mut profile, mut size) = (0u32, 0u32, 4u32);
    let status = unsafe { query(&QUIET_HOURS_PROFILE, std::ptr::null(), std::ptr::null(), &mut stamp, &mut profile as *mut u32 as *mut c_void, &mut size) };
    status >= 0 && profile != 0
}

/// Every second, from the app.
pub fn tick() {
    let r = rules();
    let action = with(|st| {
        st.ticks += 1;
        if st.ticks % 5 == 1 {
            st.stretchly = shell::running(STRETCHLY);
            st.focus = focus_assist();
        }
        let (now, local) = breaks::clock();
        let a = st.away;
        let t = breaks::Tick {
            now,
            local,
            idle: idle_seconds(),
            away: a.locked || a.asleep || a.displays_off,
            focus: st.focus,
            // A test copy may show breaks while the owner's Stretchly still runs.
            other_app: st.stretchly && !config::test_mode(),
        };
        st.scheduler.tick(&r, t)
    });
    if let Some(a) = action {
        apply(a);
    }
}

pub fn set_locked(on: bool) {
    with(|st| st.away.locked = on);
}

pub fn set_asleep(on: bool) {
    with(|st| st.away.asleep = on);
}

pub fn set_displays_off(off: bool) {
    with(|st| st.away.displays_off = off);
}

/// Run `f` on the scheduler with the current settings and time, then do what it returns.
fn act(f: impl FnOnce(&mut Scheduler, &Rules, f64, NaiveDateTime) -> Action) {
    let r = rules();
    let (now, local) = breaks::clock();
    if let Some(a) = with(|st| f(&mut st.scheduler, &r, now, local)) {
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
pub fn show_test(long: bool) {
    act(|s, r, now, _| s.start_now(r, now, Some(long)));
}

/// "Next break in 12 min", "Paused until 06:00", "Stretchly is running", …
pub fn status_line() -> String {
    let r = rules();
    with(|st| st.scheduler.status(&r).line()).unwrap_or_default()
}

/// A break is on screen (fading in or out included).
pub fn on_screen() -> bool {
    with(|st| !st.screens.is_empty()).unwrap_or(false)
}

/// The break is fully in (or none is showing): what `--dump` waits for.
pub fn settled() -> bool {
    with(|st| st.screens.is_empty() || st.fade == Fade::Shown).unwrap_or(true)
}

/// "Break in 10 s" in a toast at the top of the main monitor, refreshed every second until the break.
fn notice(seconds: u32, long: bool) {
    let m = glass::monitor_at(POINT { x: 0, y: 0 });
    let at = POINT { x: (m.work.left + m.work.right) / 2, y: m.work.top + ((16.0 + 22.0) * m.scale).round() as i32 };
    glass::toast(&format!("{} in {seconds} s", if long { "Long break" } else { "Break" }), at);
}
struct Display {
    rect: RECT,
    scale: f32,
    primary: bool,
}

/// Every monitor, whole (the taskbar too); one screen-sized place far outside them for an `--offscreen` copy.
fn displays() -> Vec<Display> {
    unsafe extern "system" fn each(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let out = unsafe { &mut *(data.0 as *mut Vec<Display>) };
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if unsafe { GetMonitorInfoW(m, &mut mi) }.as_bool() {
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = unsafe { GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
            let scale = (dx.max(48) as f32 / 96.0).clamp(0.5, 5.0);
            out.push(Display { rect: mi.rcMonitor, scale, primary: mi.dwFlags & MONITORINFOF_PRIMARY != 0 });
        }
        true.into()
    }
    let mut out: Vec<Display> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut out as *mut Vec<Display> as isize));
    }
    if out.is_empty() {
        out.push(Display { rect: RECT { left: 0, top: 0, right: 1920, bottom: 1080 }, scale: 1.0, primary: true });
    }
    if config::offscreen() {
        let main = out.iter().find(|d| d.primary).unwrap_or(&out[0]);
        let (x, y) = glass::offscreen_origin();
        let (w, h) = (main.rect.right - main.rect.left, main.rect.bottom - main.rect.top);
        return vec![Display { rect: RECT { left: x, top: y, right: x + w, bottom: y + h }, scale: main.scale, primary: true }];
    }
    out
}

/// The quote with the owner's line breaks, centred, lines 1.3 apart (the half-leading on both sides, as CSS
/// does), and its height.
fn quote_layout(text: &str, column: f32) -> Option<(IDWriteTextLayout, f32)> {
    let g = gfx();
    let fmt = g.format(Font::Display, QUOTE, DWRITE_FONT_WEIGHT_NORMAL, true, false);
    let l = g.layout(text, &fmt, column, 10_000.0)?;
    let line = (QUOTE * QUOTE_LEADING).round();
    unsafe {
        let _ = l.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        let _ = l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
        let mut lines = vec![DWRITE_LINE_METRICS::default(); 64];
        let mut n = 0u32;
        let baseline = match l.GetLineMetrics(Some(&mut lines), &mut n) {
            Ok(()) if n > 0 => lines[0].baseline + (line - lines[0].height) / 2.0,
            _ => line * 0.8,
        };
        let _ = l.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line, baseline);
        let mut m = DWRITE_TEXT_METRICS::default();
        let h = l.GetMetrics(&mut m).map_or(line, |_| m.height);
        Some((l, h.ceil()))
    }
}

/// The quote's column: up to 720 wide, 48 from the screen's sides.
fn column(w: f32) -> (f32, f32) {
    let width = (w - 2.0 * 48.0).clamp(256.0, MEASURE);
    (((w - width) / 2.0).round(), width)
}

/// The small texts a break draws every frame (author, time, controls), laid out once per text, style and box
/// instead of 30 to 60 times a second. A handful at most: the time changes once a second.
#[derive(Default)]
struct Labels(Vec<(String, usize, u32, u32, IDWriteTextLayout)>);

impl Labels {
    fn get(&mut self, text: &str, fmt: &IDWriteTextFormat, w: f32, h: f32) -> Option<IDWriteTextLayout> {
        let key = (fmt.as_raw() as usize, w.to_bits(), h.to_bits());
        if let Some(e) = self.0.iter().find(|e| e.0 == text && (e.1, e.2, e.3) == key) {
            return Some(e.4.clone());
        }
        let l = gfx().layout(text, fmt, w, h)?;
        if self.0.len() >= 16 {
            self.0.remove(0);
        }
        self.0.push((text.to_string(), key.0, key.1, key.2, l.clone()));
        Some(l)
    }

    /// As `Gfx::width`.
    fn width(&mut self, text: &str, fmt: &IDWriteTextFormat) -> f32 {
        let Some(l) = self.get(text, fmt, 100_000.0, 100.0) else { return 0.0 };
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe { l.GetMetrics(&mut m) }.map_or(0.0, |_| m.widthIncludingTrailingWhitespace)
    }

    /// As `Canvas::text`.
    #[allow(clippy::too_many_arguments)]
    fn draw(&mut self, c: &glass::Canvas, text: &str, fmt: &IDWriteTextFormat, x: f32, y: f32, w: f32, h: f32, color: Rgba) {
        if let Some(l) = self.get(text, fmt, w, h) {
            c.layout(&l, x, y, color);
        }
    }
}

impl State {
    fn controls(&self) -> Vec<(Control, String)> {
        let mut c = Vec::new();
        if let Some(m) = self.postpone {
            c.push((Control::Postpone, format!("Postpone {m} min")));
        }
        if self.skip {
            c.push((Control::Skip, "Skip".to_string()));
        }
        c
    }

    fn opacity(&self) -> f32 {
        let eased = |since: Instant, ms: f32| {
            let p = (since.elapsed().as_secs_f64() / crate::motion::duration(ms as u64).as_secs_f64()).min(1.0) as f32;
            1.0 - (1.0 - p).powi(3)
        };
        match self.fade {
            Fade::In(t) => eased(t, FADE_IN_MS),
            Fade::Shown => 1.0,
            Fade::Out(t) => 1.0 - eased(t, FADE_OUT_MS),
        }
    }

    /// Where the time line, the time and the controls go on a screen `h` tall: (line top, time top, controls top).
    /// With controls the stack rises so they keep a margin above the bottom edge.
    fn rows(&self, h: f32, long: bool) -> (f32, f32, f32) {
        let has_controls = self.postpone.is_some() || self.skip;
        let below = if long { TIME_GAP + ROW_H } else { 0.0 } + if has_controls { CONTROL_GAP + ROW_H } else { 0.0 };
        let from_bottom = if has_controls { LINE_Y.max(below + 8.0) } else { LINE_Y };
        let line = h - from_bottom - LINE_H;
        let time = line + LINE_H + TIME_GAP;
        let controls = if long { time + ROW_H + CONTROL_GAP } else { line + LINE_H + CONTROL_GAP };
        (line, time, controls)
    }

    /// Each control's hit box on a screen `w` x `h`: 44 high, its text plus 12 on each side, 8 apart, centred.
    fn control_boxes(&mut self, w: f32, h: f32, long: bool) -> Vec<(Control, String, f32, f32, f32)> {
        let fmt = gfx().format(Font::Sans, 14.0, DWRITE_FONT_WEIGHT_MEDIUM, false, false);
        let items = self.controls();
        let widths: Vec<f32> = items.iter().map(|(_, t)| self.labels.width(t, &fmt).ceil() + 24.0).collect();
        let total = widths.iter().sum::<f32>() + 8.0 * items.len().saturating_sub(1) as f32;
        let (_, _, top) = self.rows(h, long);
        let y = top + (ROW_H - HIT) / 2.0;
        let mut x = ((w - total) / 2.0).round();
        items
            .into_iter()
            .zip(widths)
            .map(|((c, t), bw)| {
                let b = (c, t, x, y, bw);
                x += bw + 8.0;
                b
            })
            .collect()
    }

    fn draw(&mut self, i: usize) {
        let t = Instant::now();
        self.draw_screen(i);
        glass::perf("break_frame", t);
    }

    fn draw_screen(&mut self, i: usize) {
        let Some(b) = self.shown else { return };
        let Some(sc) = self.screens.get(i) else { return };
        let (w, h) = window_dips(sc.surface.hwnd, sc.scale);
        let (line_y, time_y, _) = self.rows(h, b.long);
        let controls = self.control_boxes(w, h, b.long);
        let opacity = self.opacity();
        let left = b.left(unix_now());
        let fraction = (left / b.seconds.max(1.0)).clamp(0.0, 1.0) as f32;
        let author = self.quote.author.clone();
        let quiet = gfx().format(Font::Sans, 14.0, DWRITE_FONT_WEIGHT_NORMAL, false, false);
        let label = gfx().format(Font::Sans, 14.0, DWRITE_FONT_WEIGHT_MEDIUM, false, false);
        let body = gfx().format(Font::Sans, 16.0, DWRITE_FONT_WEIGHT_NORMAL, false, false);
        let labels = &mut self.labels;
        let Screen { surface, scale, before, words, words_h } = &mut self.screens[i];
        let words_h = *words_h;
        surface.draw(*scale, |c| {
            c.backdrop();
            // Black over the blur: 0.96 at the top and bottom edges, 0.82 across the middle, so the screen shows
            // faintly behind the quote and the edges stay OLED black.
            let black = |a: f32| Rgba(0.0, 0.0, 0.0, a);
            c.gradient(0.0, 0.0, c.w, c.h, &[(0.0, black(0.96)), (0.3, black(0.82)), (0.7, black(0.82)), (1.0, black(0.96))]);
            let (x, width) = column(c.w);
            let gap = if author.is_some() { AUTHOR_GAP + AUTHOR_H } else { 0.0 };
            let top = (c.h * FROM_TOP - (words_h + gap) / 2.0).round().max(48.0);
            if let Some(l) = words.as_ref() {
                c.layout(l, x, top, TEXT);
            }
            if let Some(name) = &author {
                let tw = labels.width(name, &body);
                labels.draw(c, name, &body, x + ((width - tw) / 2.0).round(), top + words_h + AUTHOR_GAP, tw + 2.0, AUTHOR_H, SECONDARY);
            }
            let lx = ((c.w - LINE_W) / 2.0).round();
            c.round(lx, line_y, LINE_W, LINE_H, LINE_H / 2.0, glass::white(0.12));
            c.round(lx, line_y, LINE_W * fraction, LINE_H, LINE_H / 2.0, TEXT);
            if b.long {
                let t = breaks::minutes_seconds(left);
                let tw = labels.width(&t, &quiet);
                labels.draw(c, &t, &quiet, ((c.w - tw) / 2.0).round(), time_y, tw + 2.0, ROW_H, SECONDARY);
            }
            for (_, t, bx, by, bw) in &controls {
                labels.draw(c, t, &label, bx + 12.0, *by, bw - 24.0 + 2.0, HIT, SECONDARY);
            }
            // Fading: the screen as it was, over the break, at what is left of it.
            if let (true, Some(p)) = (opacity < 1.0, before.as_ref()) {
                c.cover("before", p, 1.0 - opacity);
            }
        });
    }
}

/// A window's client size in DIPs.
fn window_dips(hwnd: HWND, scale: f32) -> (f32, f32) {
    let mut r = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut r) };
    ((r.right - r.left) as f32 / scale, (r.bottom - r.top) as f32 / scale)
}

fn next_quote(st: &mut State) -> Quote {
    let mut quotes = breaks::load_quotes(&quotes_file());
    let i = st.deck.next(quotes.len()).unwrap_or(0);
    quotes.swap_remove(i.min(quotes.len().saturating_sub(1)))
}

fn show(b: Break) {
    close_now();
    let r = rules();
    let (_, local) = breaks::clock();
    let ready = with(|st| {
        st.quote = next_quote(st);
        st.skip = st.scheduler.can_skip(local);
        st.postpone = st.scheduler.can_postpone(&r).then(|| (r.long_postpone / 60.0).round() as u32);
        let fg = unsafe { GetForegroundWindow() };
        st.prev = if fg.is_invalid() || app::is_ours(fg) { HWND::default() } else { fg };
        st.quote.text.clone()
    });
    let Some(text) = ready else { return };

    // Copy every monitor before any break window is on it.
    let offscreen = config::offscreen();
    let shots: Vec<(Display, Option<Pixels>)> = displays()
        .into_iter()
        .map(|d| {
            let r = d.rect;
            let shot = if offscreen { None } else { glass::screen_copy(r.left, r.top, r.right - r.left, r.bottom - r.top) };
            (d, shot)
        })
        .collect();
    let mut primary = None;
    let mut screens = Vec::new();
    for (d, before) in shots {
        let mut surface = Surface::new(glass::BREAK, None, true);
        surface.full_screen();
        surface.backdrop = before.as_ref().map(|p| glass::blurred(p, d.rect.left, d.rect.top, d.scale, 1.0));
        let w_dip = (d.rect.right - d.rect.left) as f32 / d.scale;
        let (_, width) = column(w_dip);
        let (words, words_h) = quote_layout(&text, width).map_or((None, 0.0), |(l, h)| (Some(l), h));
        surface.cloak_until_drawn();
        unsafe {
            let _ = SetWindowPos(
                surface.hwnd,
                Some(HWND_TOPMOST),
                d.rect.left,
                d.rect.top,
                d.rect.right - d.rect.left,
                d.rect.bottom - d.rect.top,
                SWP_SHOWWINDOW | SWP_NOACTIVATE,
            );
        }
        if d.primary {
            primary = Some(surface.hwnd);
        }
        screens.push(Screen { surface, scale: d.scale, before, words, words_h });
    }
    let first = screens.first().map(|s| s.surface.hwnd);
    let fade = if crate::motion::animations() { Fade::In(Instant::now()) } else { Fade::Shown };
    with(|st| {
        st.screens = screens;
        st.shown = Some(b);
        st.fade = fade;
        for i in 0..st.screens.len() {
            st.draw(i);
        }
    });
    // The main monitor's break takes the keyboard (Esc, and typing pauses with the break); an offscreen copy never does.
    if !offscreen && let Some(h) = primary.or(first) {
        glass::bring_to_front(h);
    }
    if let Some(h) = first {
        unsafe {
            let _ = SetTimer(Some(h), FRAME_TIMER, if fade == Fade::Shown { 33 } else { 16 }, None);
        }
    }
}

/// Fade out, handing the keyboard back to the app that had it.
fn hide() {
    let fade = crate::motion::animations();
    let restore = with(|st| {
        if st.screens.is_empty() {
            return None;
        }
        let first = st.screens[0].surface.hwnd;
        if fade {
            st.fade = Fade::Out(Instant::now());
            unsafe {
                let _ = SetTimer(Some(first), FRAME_TIMER, 16, None);
            }
        }
        let fg = unsafe { GetForegroundWindow() };
        let ours = st.screens.iter().any(|s| s.surface.hwnd == fg);
        Some((ours && unsafe { IsWindow(Some(st.prev)) }.as_bool()).then_some(st.prev))
    })
    .flatten();
    if let Some(Some(prev)) = restore {
        unsafe {
            let _ = SetForegroundWindow(prev);
        }
    }
    if !fade {
        close_now();
    }
}

/// Close at once, without the fade.
fn close_now() {
    let screens = with(|st| {
        st.shown = None;
        std::mem::take(&mut st.screens)
    });
    // Destroyed outside the borrow: their windows get messages while they go.
    for s in screens.into_iter().flatten() {
        unsafe {
            let _ = DestroyWindow(s.surface.hwnd);
        }
    }
}

/// The frame timer: fades, the draining line, the countdown.
fn frame() {
    let done = with(|st| {
        if st.screens.is_empty() {
            return true;
        }
        let first = st.screens[0].surface.hwnd;
        match st.fade {
            Fade::In(t) if t.elapsed() >= crate::motion::duration(FADE_IN_MS as u64) => {
                st.fade = Fade::Shown;
                unsafe {
                    let _ = SetTimer(Some(first), FRAME_TIMER, 33, None);
                }
            }
            Fade::Out(t) if t.elapsed() >= crate::motion::duration(FADE_OUT_MS as u64) => return true,
            _ => {}
        }
        for i in 0..st.screens.len() {
            st.draw(i);
        }
        false
    })
    .unwrap_or(false);
    if done {
        close_now();
    }
}

/// Window messages for the break windows.
pub fn message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_PAINT => {
            with(|st| {
                if let Some(i) = st.screens.iter().position(|s| s.surface.hwnd == hwnd) {
                    st.draw(i);
                }
            });
            Some(LRESULT(0))
        }
        WM_TIMER if wparam.0 == FRAME_TIMER => {
            frame();
            Some(LRESULT(0))
        }
        // Esc skips a break that may be skipped; a strict break ignores it. Other keys stop here.
        WM_KEYDOWN => {
            if wparam.0 as u16 == VK_ESCAPE.0 {
                act(|s, _, _, local| s.skip(local));
            }
            Some(LRESULT(0))
        }
        WM_CHAR | WM_SYSCHAR => Some(LRESULT(0)),
        // Alt+F4 closes nothing: a break ends by its time, Skip or Postpone.
        WM_CLOSE => Some(LRESULT(0)),
        WM_LBUTTONUP => {
            let scale = with(|st| st.screens.iter().find(|s| s.surface.hwnd == hwnd).map(|s| s.scale)).flatten()?;
            let (w, h) = window_dips(hwnd, scale);
            let (x, y) = ((lparam.0 & 0xFFFF) as i16 as f32 / scale, ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / scale);
            let long = with(|st| st.scheduler.running().is_some_and(|b| b.long)).unwrap_or(false);
            let hit = with(|st| st.control_boxes(w, h, long).into_iter().find(|(_, _, bx, by, bw)| x >= *bx && x < bx + bw && y >= *by && y < by + HIT).map(|c| c.0)).flatten();
            match hit {
                Some(Control::Postpone) => act(|s, r, _, _| s.postpone(r)),
                Some(Control::Skip) => act(|s, _, _, local| s.skip(local)),
                None => {}
            }
            Some(LRESULT(0))
        }
        _ => None,
    }
}

/// `--dump`: what the break shows: its kind, the quote, the line, the controls, each screen's window.
pub fn dump() -> String {
    let r = rules();
    with(|st| {
        let mut s = String::new();
        let now = unix_now();
        let _ = writeln!(s, "panel break");
        match st.scheduler.running() {
            Some(b) => {
                let _ = writeln!(s, "kind {}", if b.long { "long" } else { "mini" });
                let _ = writeln!(s, "strict {}", if b.strict { "yes" } else { "no" });
                let _ = writeln!(s, "seconds {}", b.seconds);
                let _ = writeln!(s, "line {:.2}", (b.left(now) / b.seconds.max(1.0)).clamp(0.0, 1.0));
                if b.long {
                    let _ = writeln!(s, "time {}", breaks::minutes_seconds(b.left(now)));
                }
            }
            None => {
                let _ = writeln!(s, "kind none");
            }
        }
        let _ = writeln!(s, "opacity {:.2}", st.opacity());
        let _ = writeln!(s, "quote {:?}", st.quote.text);
        let _ = writeln!(s, "author {}", st.quote.author.as_deref().unwrap_or("-"));
        let controls: Vec<String> = st.controls().into_iter().map(|(_, t)| t).collect();
        let _ = writeln!(s, "controls {}", if controls.is_empty() { "-".to_string() } else { controls.join(" | ") });
        for (i, sc) in st.screens.iter().enumerate() {
            let mut r = RECT::default();
            let _ = unsafe { GetWindowRect(sc.surface.hwnd, &mut r) };
            let _ = writeln!(s, "screen {} {} {} {} {} scale {} backdrop {}", i + 1, r.left, r.top, r.right - r.left, r.bottom - r.top, sc.scale, if sc.surface.backdrop.is_some() { "yes" } else { "no" });
        }
        let _ = writeln!(s, "focus-assist {}", if focus_assist() { "on" } else { "off" });
        let _ = writeln!(s, "stretchly {}", if shell::running(STRETCHLY) { "running" } else { "no" });
        let _ = writeln!(s, "status {}", st.scheduler.status(&r).line());
        s
    })
    .unwrap_or_default()
}
