//! The evening journal: Vesper's prompt in Livae's glass, laid out as the Mac's notebook (design D): the date and the
//! day's rating small on top, the note large in the middle, the habits and Save in the footer. It asks at the hour in
//! the settings (checked every 30 s and on wake), snoozes, comes back to the front until today is saved, and edits
//! today's entry. Entries are written by `livae_core::journal` in Vesper's format, so the Obsidian file stays one
//! journal. While Vesper still runs, Livae never asks on its own; opening it from the tray still works.
//!
//! The panel is drawn with Direct2D. The note is a native multi-line EDIT (IME, selection, undo) on a black page:
//! GDI controls draw opaque only outside DWM's glass, so this one window gives up the glass frame. Behind the panel,
//! a click-through blur of the monitor (a layered window, so clicks reach the apps under it, as on the Mac).

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};

use livae_core::field::Field;
use livae_core::journal::{self as core, DoneCache, Draft, Entry, Mode, NaiveDate, Schedule, Tracker, DAY_COLORS, DAY_LABELS, MIN_CHARS};
use livae_core::settings::Habit;
use livae_core::tokens::{self, color::dark};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_MEDIUM, DWRITE_FONT_WEIGHT_NORMAL};
use windows::Win32::Graphics::Gdi::{
    AddFontMemResourceEx, CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW, GdiFlush, GetDC, GetStockObject, GetTextFaceW, InvalidateRect,
    ReleaseDC, SelectObject, SetBkColor, SetBkMode, SetTextColor, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    BLACK_BRUSH, BLENDFUNCTION, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateRectRgn, SetWindowRgn, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DT_NOPREFIX,
    DT_WORDBREAK, FW_NORMAL, HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{EM_EMPTYUNDOBUFFER, EM_GETRECT, EM_GETSEL, EM_REPLACESEL, EM_SCROLLCARET, EM_SETLIMITTEXT, EM_SETMARGINS, EM_SETSEL};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::glass::{self, gfx, Font, Rgba, Surface, SECONDARY, TERTIARY, TEXT};
use crate::{app, config, shell};

const W: f32 = 720.0;
const PAD_TOP: f32 = 36.0;
const PAD_X: f32 = 40.0;
const PAD_BOTTOM: f32 = 28.0;
const INNER: f32 = W - 2.0 * PAD_X;
/// Top row (date and rating pills), the note, a hairline, the footer.
const TOP_ROW: f32 = 28.0;
const NOTE_GAP: f32 = 26.0;
const NOTE_H: f32 = 190.0;
const NOTE_SIZE: f32 = 24.0;
const RULE_GAP: f32 = 20.0;
const FOOTER_GAP: f32 = 18.0;
const BUTTON_H: f32 = 40.0;
const SAVE_W: f32 = 96.0;
const PILL_RADIUS: f32 = 12.0;
/// Space between footer items. Habit chips wrap onto more lines (8 apart) when they do not fit beside Save.
const ITEM_GAP: f32 = 16.0;
/// Between the dismiss text, the counter and Save.
const RIGHT_GAP: f32 = 12.0;
const CHIP_H: f32 = 28.0;
const LINE_GAP: f32 = 8.0;
/// Habit chips keep this room around their content for the focus outline.
const CHIP_MARGIN: f32 = 4.0;
const BOX: f32 = 16.0;
const ADD_W: f32 = 28.0;
const EDIT_W: f32 = 180.0;
/// The hint or error line under the note; the note gives up this height while it shows.
const NOTICE_H: f32 = 24.0;
/// The black page the note sits on reaches this far past its text.
const PAGE_PAD: f32 = 12.0;
/// Black over the full-screen blur, so the desktop behind stays quiet on an OLED screen.
const BACKDROP_WASH: f32 = 0.3;
/// A due prompt that lost the keyboard comes back after this long (Vesper: 25 s).
const REFRONT_MS: u32 = 25_000;
/// `--journal` and test copies write here, inside the data folder, never in the vault.
const TEST_FILE: &str = "journal-test.md";
const ERROR_TEXT: &str = "Could not save. Check the file and retry.";
const PLACEHOLDER: &str = "What did today teach you?";
const NOTE_ID: usize = 1;
const NAG_TIMER: usize = 1;
const REFRONT_TIMER: usize = 2;
const RENAME: u32 = 1;
const REMOVE: u32 = 2;

#[derive(Default)]
struct Sched {
    tracker: Tracker,
    /// Set by `show_test`: every read and write goes to this file instead.
    test_file: Option<PathBuf>,
    done: DoneCache,
}

/// The inline habit name field: adding a new habit, or renaming the one at an index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

/// What has the keyboard: the note (the EDIT itself), or one of the drawn controls (the panel window).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Note,
    Rating(usize),
    Habit(usize),
    Add,
    Name,
    Dismiss,
    Save,
}

/// Something the pointer can press.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    Rating(usize),
    Habit(usize),
    Add,
    Name,
    Dismiss,
    Save,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Item {
    Chip(usize),
    Add,
    Editor,
}

/// Where everything goes, in DIPs from the panel's top left.
struct Layout {
    h: f32,
    note_y: f32,
    lift: f32,
    rule_y: f32,
    /// Rating pills: x and width (y is PAD_TOP).
    days: Vec<(f32, f32)>,
    /// Footer items: what, x, y, width (a chip's width without its focus margin).
    items: Vec<(Item, f32, f32, f32)>,
    ry: f32,
    dismiss: (f32, f32),
    count: (f32, f32),
    save_x: f32,
}

struct Ui {
    panel: Surface,
    note: HWND,
    note_frame: Option<shell::Pixels>,
    note_clipped: bool,
    font: HFONT,
    font_scale: f32,
    /// The click-through blur behind the panel while it shows.
    veil: HWND,
    scale: f32,
    mode: Mode,
    date: NaiveDate,
    /// The rating, the habit answers and the entry they edit, until Save.
    draft: Draft,
    /// Each habit's run of Yes days up to yesterday; today's answer is added live.
    streak_base: Vec<(String, u32)>,
    editing: Option<Edit>,
    name: Field,
    caret: bool,
    /// The first half of a character outside the BMP, waiting for its second WM_CHAR.
    high: Option<u16>,
    notice: Notice,
    /// The hint shown, or the failed save's path and reason.
    notice_text: String,
    saving: bool,
    showing: bool,
    focus: Focus,
    note_empty: bool,
    /// The window in front before the prompt; it gets the keyboard back when the prompt closes.
    prev: HWND,
    h: f32,
}

thread_local! {
    static SCHED: RefCell<Sched> = RefCell::new(Sched::default());
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// The EDIT's own window procedure, which ours passes everything else to.
static EDIT_PROC: AtomicIsize = AtomicIsize::new(0);

/// Runs `f` on the prompt. Re-entrant calls (Windows calling back while we hold it) are skipped.
fn with<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|cell| cell.try_borrow_mut().ok().and_then(|mut slot| slot.as_mut().map(f)))
}

fn habits() -> Vec<Habit> {
    config::get().journal.habits
}

fn keys(habits: &[Habit]) -> Vec<String> {
    habits.iter().map(|h| h.key.clone()).collect()
}

fn schedule() -> Schedule {
    config::get().journal.schedule()
}

fn colorref(c: [u8; 3]) -> COLORREF {
    COLORREF(c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16)
}

fn rgb(c: [u8; 3]) -> Rgba {
    Rgba(c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0)
}

fn day_color(i: usize) -> Rgba {
    let (r, g, b) = DAY_COLORS[i];
    rgb([r, g, b])
}

/// The journal file: the test file under `--test` or after `show_test`, else the one in the settings.
pub fn file() -> PathBuf {
    if let Some(p) = SCHED.with(|s| s.borrow().test_file.clone()) {
        return p;
    }
    if config::test_mode() {
        return config::data_dir().join(TEST_FILE);
    }
    livae_core::settings::expand(&config::get().journal.file, &config::home())
}

/// Whether `day` has an entry; the file is read again only when it changed.
fn done_on(day: NaiveDate) -> bool {
    let path = file();
    SCHED.with(|s| s.borrow_mut().done.get(&path, day))
}

/// Vesper still running: it owns the evening prompt until the owner retires it.
fn vesper_running() -> bool {
    shell::running("Vesper.exe")
}

fn showing() -> bool {
    with(|u| u.showing).unwrap_or(false)
}

fn text_w(text: &str, size: f32, medium: bool) -> f32 {
    let weight = if medium { DWRITE_FONT_WEIGHT_MEDIUM } else { DWRITE_FONT_WEIGHT_NORMAL };
    gfx().width(text, &gfx().format(Font::Sans, size, weight, false, false))
}

/// The note's font: the panel's sans at 24, in the monitor's pixels.
fn note_font(scale: f32) -> HFONT {
    let make = |face: &str| unsafe {
        let face = HSTRING::from(face);
        CreateFontW(
            -((NOTE_SIZE * scale).round() as i32),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32,
            &face,
        )
    };
    // Livae Sans (added to this process only, from the exe), else Windows 11's Segoe UI Variable, else Segoe UI.
    static ADDED: std::sync::Once = std::sync::Once::new();
    ADDED.call_once(|| unsafe {
        let regular = crate::glass::LIVAE_SANS[0];
        let count = 0u32;
        let _ = AddFontMemResourceEx(regular.as_ptr() as *const std::ffi::c_void, regular.len() as u32, None, &count);
    });
    for face in ["Livae Sans", "Segoe UI Variable Text"] {
        let font = make(face);
        unsafe {
            let dc = GetDC(None);
            let old = SelectObject(dc, HGDIOBJ(font.0));
            let mut name = [0u16; 64];
            let n = GetTextFaceW(dc, Some(&mut name)).max(0) as usize;
            SelectObject(dc, old);
            ReleaseDC(None, dc);
            if String::from_utf16_lossy(&name[..n.min(64)]).trim_end_matches('\0') == face {
                return font;
            }
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
    }
    make("Segoe UI")
}

/// The black page and the note's text field; Tab, Esc, Ctrl+Enter, Ctrl+A and Ctrl+Backspace are the prompt's.
extern "system" fn note_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let original = EDIT_PROC.load(Ordering::SeqCst);
    let pass = || unsafe { CallWindowProcW(std::mem::transmute::<isize, WNDPROC>(original), hwnd, msg, wparam, lparam) };
    let down = |vk: VIRTUAL_KEY| unsafe { GetKeyState(vk.0 as i32) < 0 };
    match msg {
        WM_KEYDOWN => {
            let ctrl = down(VK_CONTROL) && !down(VK_MENU);
            match VIRTUAL_KEY(wparam.0 as u16) {
                VK_TAB => tab(down(VK_SHIFT)),
                VK_ESCAPE => escape(),
                VK_RETURN if ctrl => save(),
                VK_BACK if ctrl => delete_word_before(hwnd),
                k if ctrl && k.0 == b'A' as u16 => unsafe {
                    SendMessageW(hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
                },
                _ => return pass(),
            }
            LRESULT(0)
        }
        // The characters those keys type (Tab, Esc, Ctrl+Enter, Ctrl+A, Ctrl+Backspace) are not text.
        WM_CHAR if matches!(wparam.0, 0x09 | 0x1B | 0x0A | 0x01 | 0x7F) => LRESULT(0),
        WM_SETFOCUS => {
            with(|u| {
                u.focus = Focus::Note;
                u.paint();
            });
            pass()
        }
        WM_PAINT => {
            let r = pass();
            unsafe {
                if GetWindowTextLengthW(hwnd) == 0 {
                    let dc = GetDC(Some(hwnd));
                    draw_placeholder(hwnd, dc);
                    ReleaseDC(Some(hwnd), dc);
                }
            }
            r
        }
        WM_PRINTCLIENT => {
            let result = pass();
            unsafe {
                if GetWindowTextLengthW(hwnd) == 0 {
                    draw_placeholder(hwnd, HDC(wparam.0 as _));
                }
            }
            result
        }
        _ => pass(),
    }
}

/// "What did today teach you?" where the empty note's text starts.
unsafe fn draw_placeholder(hwnd: HWND, dc: HDC) {
    unsafe {
        let font = SendMessageW(hwnd, WM_GETFONT, None, None);
        let old = SelectObject(dc, HGDIOBJ(font.0 as *mut _));
        SetTextColor(dc, colorref(tokens::color::dark::TEXT_TERTIARY));
        SetBkMode(dc, TRANSPARENT);
        let mut r = RECT::default();
        SendMessageW(hwnd, EM_GETRECT, None, Some(LPARAM(&mut r as *mut RECT as isize)));
        let mut text: Vec<u16> = PLACEHOLDER.encode_utf16().collect();
        let _ = HideCaret(Some(hwnd));
        DrawTextW(dc, &mut text, &mut r, DT_NOPREFIX | DT_WORDBREAK);
        let _ = ShowCaret(Some(hwnd));
        SelectObject(dc, old);
    }
}

/// Ctrl+Backspace: the selection, or the word before the caret (and the spaces after it).
fn delete_word_before(note: HWND) {
    unsafe {
        let (mut start, mut end) = (0u32, 0u32);
        SendMessageW(note, EM_GETSEL, Some(WPARAM(&mut start as *mut u32 as usize)), Some(LPARAM(&mut end as *mut u32 as isize)));
        if start == end {
            let text = edit_text(note);
            let space = |c: u16| matches!(c, 0x20 | 0x09 | 0x0D | 0x0A);
            let mut i = (end as usize).min(text.len());
            while i > 0 && space(text[i - 1]) {
                i -= 1;
            }
            while i > 0 && !space(text[i - 1]) {
                i -= 1;
            }
            SendMessageW(note, EM_SETSEL, Some(WPARAM(i)), Some(LPARAM(end as isize)));
        }
        let empty = [0u16];
        SendMessageW(note, EM_REPLACESEL, Some(WPARAM(1)), Some(LPARAM(empty.as_ptr() as isize)));
    }
}

/// The EDIT's text as UTF-16, line breaks as "\r\n".
fn edit_text(note: HWND) -> Vec<u16> {
    unsafe {
        let n = GetWindowTextLengthW(note).max(0) as usize;
        let mut buf = vec![0u16; n + 1];
        let got = GetWindowTextW(note, &mut buf).max(0) as usize;
        buf.truncate(got);
        buf
    }
}

/// The click-through blur behind the prompt: the monitor's blurred copy under a black wash, as a layered window.
fn show_veil(r: RECT, backdrop: &glass::Backdrop) -> HWND {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
        let Ok(hwnd) = CreateWindowExW(ex, w!("LivaePanel"), w!("Livae"), WS_POPUP, r.left, r.top, w, h, None, None, Some(instance.into()), None) else {
            return HWND::default();
        };
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, glass::JOURNAL_VEIL);
        let px = backdrop.full_size(w as usize, h as usize, BACKDROP_WASH);
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        if let Ok(dib) = CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            if !bits.is_null() {
                std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u8, px.len());
                let old = SelectObject(mem, dib.into());
                let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 0, AlphaFormat: AC_SRC_ALPHA as u8 };
                let (at, size, from) = (POINT { x: r.left, y: r.top }, SIZE { cx: w, cy: h }, POINT { x: 0, y: 0 });
                let _ = UpdateLayeredWindow(hwnd, Some(screen), Some(&at), Some(&size), Some(mem), Some(&from), COLORREF(0), Some(&blend), ULW_ALPHA);
                SelectObject(mem, old);
            }
            let _ = DeleteObject(dib.into());
        }
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        hwnd
    }
}

/// WM_PRINTCLIENT keeps the native note's scroll, selection and font in the moving frame.
fn note_frame(note: HWND) -> Option<shell::Pixels> {
    unsafe {
        let mut r = RECT::default();
        GetClientRect(note, &mut r).ok()?;
        let (w, h) = (r.right.max(1), r.bottom.max(1));
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return None;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let mut result = None;
        if let Ok(dib) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            if !bits.is_null() {
                let len = w as usize * h as usize * 4;
                std::ptr::write_bytes(bits, 0, len);
                let old = SelectObject(dc, dib.into());
                SendMessageW(note, WM_PRINTCLIENT, Some(WPARAM(dc.0 as usize)), Some(LPARAM((PRF_CLIENT | PRF_ERASEBKGND) as isize)));
                let _ = GdiFlush();
                let mut data = std::slice::from_raw_parts(bits as *const u8, len).to_vec();
                for pixel in data.as_chunks_mut::<4>().0 {
                    pixel[3] = 255;
                }
                SelectObject(dc, old);
                result = Some(shell::Pixels { w: w as u32, h: h as u32, data });
            }
            let _ = DeleteObject(dib.into());
        }
        let _ = DeleteDC(dc);
        result
    }
}

fn build() -> Ui {
    let mut panel = Surface::new(glass::JOURNAL, None, true);
    panel.host_controls();
    let note = unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let style = WS_CHILD | WS_VISIBLE | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32);
        let note = CreateWindowExW(WINDOW_EX_STYLE(0), w!("EDIT"), PCWSTR::null(), style, 0, 0, 10, 10, Some(panel.hwnd), Some(HMENU(NOTE_ID as _)), Some(instance.into()), None)
            .unwrap_or_default();
        let original = SetWindowLongPtrW(note, GWLP_WNDPROC, note_proc as *const () as isize);
        EDIT_PROC.store(original, Ordering::SeqCst);
        SendMessageW(note, EM_SETMARGINS, Some(WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize)), Some(LPARAM(0)));
        SendMessageW(note, EM_SETLIMITTEXT, Some(WPARAM(0)), Some(LPARAM(0)));
        note
    };
    Ui {
        panel,
        note,
        note_frame: None,
        note_clipped: false,
        font: HFONT::default(),
        font_scale: 0.0,
        veil: HWND::default(),
        scale: 1.0,
        mode: Mode::Early,
        date: core::now().date(),
        draft: Draft::default(),
        streak_base: Vec::new(),
        editing: None,
        name: Field::default(),
        caret: true,
        high: None,
        notice: Notice::None,
        notice_text: String::new(),
        saving: false,
        showing: false,
        focus: Focus::Note,
        note_empty: true,
        prev: HWND::default(),
        h: 400.0,
    }
}

impl Ui {
    fn freeze_note(&mut self) {
        self.note_frame = note_frame(self.note);
        if !self.note_clipped {
            unsafe {
                let region = CreateRectRgn(0, 0, 0, 0);
                if SetWindowRgn(self.note, Some(region), false) == 0 {
                    let _ = DeleteObject(region.into());
                }
            }
            self.note_clipped = true;
        }
        self.panel.animate_controls();
    }

    fn finish_motion(&mut self) {
        if !self.veil.is_invalid() {
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: (self.panel.frame().opacity * 255.0).round() as u8, AlphaFormat: AC_SRC_ALPHA as u8 };
            unsafe {
                let _ = UpdateLayeredWindow(self.veil, None, None, None, None, None, COLORREF(0), Some(&blend), ULW_ALPHA);
            }
        }
        if !self.panel.moving() && self.note_clipped {
            self.panel.host_controls();
            unsafe {
                SetWindowRgn(self.note, None, false);
                let _ = InvalidateRect(Some(self.note), None, false);
            }
            self.note_clipped = false;
            self.note_frame = None;
            if self.showing {
                self.paint();
            } else {
                unsafe {
                    if !self.veil.is_invalid() {
                        let _ = DestroyWindow(self.veil);
                        self.veil = HWND::default();
                    }
                }
            }
        }
    }

    fn px(&self, dip: f32) -> i32 {
        (dip * self.scale).round() as i32
    }

    fn answer(&self, key: &str) -> Option<bool> {
        self.draft.answer(key)
    }

    fn note_text(&self) -> String {
        String::from_utf16_lossy(&edit_text(self.note)).replace("\r\n", "\n")
    }

    fn entry(&self) -> Entry {
        self.draft.entry(self.note_text())
    }

    fn set_note(&mut self, text: &str) {
        unsafe {
            let _ = SetWindowTextW(self.note, &HSTRING::from(text.replace('\n', "\r\n")));
            SendMessageW(self.note, EM_EMPTYUNDOBUFFER, None, None);
            let _ = InvalidateRect(Some(self.note), None, true);
        }
        self.note_empty = text.is_empty();
    }

    /// Fill the form for `mode`: today's entry when editing, the kept draft after a snooze, else empty.
    fn prepare(&mut self, mode: Mode) {
        let today = core::now().date();
        self.mode = mode;
        self.date = today;
        self.saving = false;
        self.editing = None;
        self.notice = Notice::None;
        self.focus = Focus::Note;
        let keys = keys(&habits());
        let saved = (mode == Mode::Edit).then(|| core::parse(&file(), today).ok().flatten().unwrap_or_default());
        if let Some(note) = self.draft.prepare(saved, today, &keys) {
            self.set_note(&note);
        }
        self.streak_base = core::streaks_before(&file(), &keys, today);
    }

    fn dismiss_title(&self) -> String {
        match self.mode {
            Mode::Due => format!("Snooze {} min", config::get().journal.snooze_min.max(1)),
            Mode::Early => "Close".into(),
            Mode::Edit => "Cancel".into(),
        }
    }

    /// Place everything from the top down. The footer grows by a line when the habits wrap, and the panel with it.
    fn layout(&self) -> Layout {
        let habits = habits();
        let note_y = PAD_TOP + TOP_ROW + NOTE_GAP;
        let lift = if self.notice == Notice::None { 0.0 } else { NOTICE_H };
        let rule_y = note_y + NOTE_H + RULE_GAP;
        let footer_y = rule_y + 1.0 + FOOTER_GAP;

        // Right: the dismiss text, the counter and Save, 12 apart; the habits get everything left of them. The
        // dismiss button's 8 pt sides belong to the gaps around its text.
        let dismiss_w = text_w(&self.dismiss_title(), 14.0, true).ceil() + 16.0;
        let count_w = text_w("000 / 90", 14.0, false).ceil() + 2.0;
        let right_w = (dismiss_w - 16.0) + RIGHT_GAP + count_w + RIGHT_GAP + SAVE_W;
        let avail = INNER - right_w - ITEM_GAP;

        // Habits flow left to right, then the add button (or the name field).
        let mut items: Vec<(Item, f32)> = Vec::new();
        for (i, h) in habits.iter().enumerate() {
            if self.editing == Some(Edit::Rename(i)) {
                items.push((Item::Editor, EDIT_W));
            } else {
                items.push((Item::Chip(i), self.chip_width(h)));
            }
        }
        items.push(if self.editing == Some(Edit::Add) { (Item::Editor, EDIT_W) } else { (Item::Add, ADD_W) });
        // A line breaks only when the habits do not fit; the last habit and the add button move as one, so the
        // button never starts a line alone.
        let last = items.len() - 1;
        let mut placed = vec![(0usize, 0.0f32); items.len()];
        let (mut x, mut line, mut i) = (0.0f32, 0usize, 0usize);
        while i < items.len() {
            let end = if i + 1 == last { last } else { i };
            let unit: f32 = (i..=end).map(|k| items[k].1).sum::<f32>() + ITEM_GAP * (end - i) as f32;
            if x > 0.0 && x + unit > avail {
                line += 1;
                x = 0.0;
            }
            for (k, slot) in placed.iter_mut().enumerate().take(end + 1).skip(i) {
                *slot = (line, x);
                x += items[k].1 + ITEM_GAP;
            }
            i = end + 1;
        }
        let lines = line + 1;
        let block = lines as f32 * CHIP_H + (lines - 1) as f32 * LINE_GAP;
        let footer = block.max(BUTTON_H);
        let block_top = footer_y + (footer - block) / 2.0;
        let items = items.iter().zip(placed).map(|(&(it, w), (l, x))| (it, PAD_X + x, block_top + l as f32 * (CHIP_H + LINE_GAP), w)).collect();

        let ry = footer_y + (footer - BUTTON_H) / 2.0;
        let save_x = W - PAD_X - SAVE_W;
        let count_x = save_x - RIGHT_GAP - count_w;
        let dismiss_x = count_x - RIGHT_GAP + 8.0 - dismiss_w;

        let mut days = vec![(0.0, 0.0); DAY_LABELS.len()];
        let mut right = W - PAD_X;
        for (i, label) in DAY_LABELS.iter().enumerate().rev() {
            let w = text_w(label, 13.0, true).ceil() + 24.0;
            right -= w;
            days[i] = (right, w);
            right -= 6.0;
        }

        Layout { h: footer_y + footer + PAD_BOTTOM, note_y, lift, rule_y, days, items, ry, dismiss: (dismiss_x, dismiss_w), count: (count_x, count_w), save_x }
    }

    /// A habit's current streak: the run up to yesterday, plus today when Yes, ended by No.
    fn streak(&self, h: &Habit) -> u32 {
        let base = self.streak_base.iter().find(|(k, _)| *k == h.key).map_or(0, |(_, n)| *n);
        core::streak_with(base, self.answer(&h.key))
    }

    /// Box, gap, name, and the streak when there is one.
    fn chip_width(&self, h: &Habit) -> f32 {
        let n = self.streak(h);
        BOX + 8.0 + text_w(&h.label, 14.0, false).ceil() + if n >= 2 { 6.0 + text_w(&format!("{n} days"), 14.0, false).ceil() } else { 0.0 }
    }

    /// Size the panel and the note for the layout, then draw.
    fn place(&mut self) {
        let l = self.layout();
        if (l.h - self.h).abs() > 0.5 && self.panel.visible() {
            unsafe {
                let _ = SetWindowPos(self.panel.hwnd, None, 0, 0, self.px(W), self.px(l.h), SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
        self.h = l.h;
        let note_h = NOTE_H - l.lift - PAGE_PAD;
        unsafe {
            let _ = MoveWindow(self.note, self.px(PAD_X), self.px(l.note_y), self.px(INNER), self.px(note_h), true);
        }
        if self.note_clipped {
            self.freeze_note();
        }
        self.paint();
    }

    fn panel_focused(&self) -> bool {
        unsafe { GetFocus() == self.panel.hwnd }
    }

    fn paint(&mut self) {
        let l = self.layout();
        let entry = self.entry();
        let keys = keys(&habits());
        let habits = habits();
        let n = entry.note_len();
        let complete = entry.is_complete(&keys) && !self.saving;
        let ring = self.panel_focused().then_some(self.focus);
        let dark_glass = config::get().glass.max(0.85) as f32;
        let date = core::date_label(self.date);
        let (day, saving, notice, notice_text) = (self.draft.day, self.saving, self.notice, self.notice_text.clone());
        let dismiss = self.dismiss_title();
        let chips: Vec<(String, Option<bool>, u32)> = habits.iter().map(|h| (h.label.clone(), self.answer(&h.key), self.streak(h))).collect();
        let (name, caret) = (self.name.clone(), self.caret && self.panel_focused());
        let g = gfx();
        let small = g.format(Font::Sans, 14.0, DWRITE_FONT_WEIGHT_NORMAL, false, false);
        let small_right = g.format(Font::Sans, 14.0, DWRITE_FONT_WEIGHT_NORMAL, false, true);
        let label = g.format(Font::Sans, 14.0, DWRITE_FONT_WEIGHT_MEDIUM, false, false);
        let pill = g.format(Font::Sans, 13.0, DWRITE_FONT_WEIGHT_MEDIUM, false, false);
        let focus_ring = rgb(dark::FOCUS);
        let mut at = None;
        let note_frame = self.note_frame.as_ref();
        self.panel.draw(self.scale, |c| {
            c.glass(dark_glass);
            let centred = |c: &mut glass::Canvas, text: &str, fmt, x: f32, y: f32, w: f32, h: f32, color| {
                let tw = g.width(text, fmt);
                c.text(text, fmt, x + ((w - tw) / 2.0).round(), y, tw + 2.0, h, color);
            };
            let ring_around = |c: &mut glass::Canvas, x: f32, y: f32, w: f32, h: f32, radius: f32| c.outline(x - 3.0, y - 3.0, w + 6.0, h + 6.0, radius + 3.0, 1.0, focus_ring, None);

            // Top row: the date; the ratings as small pills, only the chosen one in its colour.
            c.text(&date, &small, PAD_X, PAD_TOP, 300.0, TOP_ROW, SECONDARY);
            for (i, &(x, w)) in l.days.iter().enumerate() {
                let on = day == Some(i);
                c.round(x, PAD_TOP, w, TOP_ROW, TOP_ROW / 2.0, if on { day_color(i) } else { glass::white(0.06) });
                centred(c, DAY_LABELS[i], &pill, x, PAD_TOP, w, TOP_ROW, if on { rgb(dark::CANVAS) } else { SECONDARY });
                if ring == Some(Focus::Rating(i)) {
                    ring_around(c, x, PAD_TOP, w, TOP_ROW, TOP_ROW / 2.0);
                }
            }

            // The note's black page (the EDIT draws the text on it).
            let radius = tokens::radius::CONTROL as f32;
            c.round(PAD_X - PAGE_PAD, l.note_y - PAGE_PAD, INNER + 2.0 * PAGE_PAD, NOTE_H - l.lift + PAGE_PAD, radius, Rgba(0.0, 0.0, 0.0, 1.0));
            if let Some(frame) = note_frame {
                let h = NOTE_H - l.lift - PAGE_PAD;
                c.control(frame, PAD_X, l.note_y, INNER, h);
            }

            // The line under the note: what Save still needs, or a failed save with its mark.
            if notice != Notice::None {
                let text = if notice == Notice::Error { ERROR_TEXT } else { notice_text.as_str() };
                let y = l.note_y + NOTE_H - NOTICE_H;
                c.text(text, &small_right, PAD_X, y, INNER, NOTICE_H, TEXT);
                if notice == Notice::Error {
                    let tw = g.width(text, &small);
                    c.glyph(glass::glyph::WARNING, 14.0, W - PAD_X - tw - 24.0, y, 16.0, NOTICE_H, rgb(tokens::color::status::ERROR));
                }
            }

            c.fill(PAD_X, l.rule_y, INNER, 1.0, glass::white(0.08));

            // Footer, left: the habits, then the add button or the name field.
            for &(item, x, y, w) in &l.items {
                match item {
                    Item::Chip(i) => {
                        let Some((text, answer, streak)) = chips.get(i) else { continue };
                        if ring == Some(Focus::Habit(i)) {
                            c.outline(x - CHIP_MARGIN, y, w + 2.0 * CHIP_MARGIN, CHIP_H, 8.0, 1.0, focus_ring, None);
                        }
                        // A check box: dashed while unanswered, filled for Yes, a minus for No.
                        let top = y + ((CHIP_H - BOX) / 2.0).round();
                        match answer {
                            Some(true) => c.round(x, top, BOX, BOX, 5.0, TEXT),
                            Some(false) => {
                                c.outline(x, top, BOX, BOX, 5.0, 1.5, rgb(dark::CONTROL_LINE), None);
                                c.fill(x + 4.5, top + BOX / 2.0 - 0.75, BOX - 9.0, 1.5, SECONDARY);
                            }
                            None => c.outline(x, top, BOX, BOX, 5.0, 1.5, rgb(dark::CONTROL_LINE), Some(&[2.5 / 1.5, 2.0 / 1.5])),
                        }
                        let tx = x + BOX + 8.0;
                        let tw = g.width(text, &small).ceil();
                        c.text(text, &small, tx, y, tw + 2.0, CHIP_H, if *answer == Some(true) { TEXT } else { SECONDARY });
                        if *streak >= 2 {
                            let s = format!("{streak} days");
                            c.text(&s, &small, tx + tw + 6.0, y, g.width(&s, &small) + 2.0, CHIP_H, TERTIARY);
                        }
                    }
                    Item::Add => {
                        c.round(x, y, ADD_W, CHIP_H, CHIP_H / 2.0, glass::white(0.06));
                        centred(c, "+", &label, x, y, ADD_W, CHIP_H, SECONDARY);
                        if ring == Some(Focus::Add) {
                            ring_around(c, x, y, ADD_W, CHIP_H, CHIP_H / 2.0);
                        }
                    }
                    Item::Editor => {
                        c.round(x, y, EDIT_W, CHIP_H, CHIP_H / 2.0, glass::white(0.06));
                        at = Some(c.field_at(&name, &small, x + 12.0, y + 4.0, EDIT_W - 24.0, 20.0, "New habit", caret));
                    }
                }
            }

            // Footer, right: the quiet dismiss text, the counter, Save (an outline until the form is complete, then
            // the light primary button).
            let (dx, dw) = l.dismiss;
            centred(c, &dismiss, &label, dx, l.ry, dw, BUTTON_H, SECONDARY);
            if ring == Some(Focus::Dismiss) {
                ring_around(c, dx, l.ry, dw, BUTTON_H, PILL_RADIUS);
            }
            let (cx, cw) = l.count;
            c.text(&format!("{n} / {MIN_CHARS}"), &small_right, cx, l.ry + 10.0, cw, 20.0, if n >= MIN_CHARS { TEXT } else { SECONDARY });
            let title = if saving { "Saving\u{2026}" } else { "Save" };
            if complete {
                c.round(l.save_x, l.ry, SAVE_W, BUTTON_H, PILL_RADIUS, rgb(dark::ACTION));
                centred(c, title, &label, l.save_x, l.ry, SAVE_W, BUTTON_H, rgb(dark::ACTION_TEXT));
            } else {
                c.outline(l.save_x, l.ry, SAVE_W, BUTTON_H, PILL_RADIUS, 1.0, glass::white(0.16), None);
                centred(c, title, &label, l.save_x, l.ry, SAVE_W, BUTTON_H, SECONDARY);
            }
            if ring == Some(Focus::Save) {
                ring_around(c, l.save_x, l.ry, SAVE_W, BUTTON_H, PILL_RADIUS);
            }
        });
        // The system caret (never shown) follows the name field's, so an IME opens its window there.
        if let (Some((x, y, _)), true) = (at, self.panel_focused()) {
            unsafe {
                let _ = SetCaretPos(self.px(x), self.px(y));
            }
        }
        self.finish_motion();
    }

    /// The counter, the placeholder, the live hint and Save; on every keystroke in the note.
    fn note_changed(&mut self) {
        if self.note_clipped {
            self.freeze_note();
        }
        let empty = unsafe { GetWindowTextLengthW(self.note) } == 0;
        if empty != self.note_empty {
            self.note_empty = empty;
            unsafe {
                let _ = InvalidateRect(Some(self.note), None, true);
            }
        }
        if self.notice == Notice::Hint {
            match core::missing(&self.entry(), &keys(&habits())) {
                Some(hint) => self.notice_text = hint.to_string(),
                None => {
                    self.notice = Notice::None;
                    self.place();
                    return;
                }
            }
        }
        self.paint();
    }

    /// The line under the note: a hint (what Save still needs) or a failed save.
    fn set_notice(&mut self, notice: Notice, text: &str) {
        self.notice = notice;
        self.notice_text = text.to_string();
        self.place();
    }

    fn toggle(&mut self, index: usize) {
        let Some(h) = habits().into_iter().nth(index) else { return };
        self.draft.toggle(&h.key);
        self.after_answer();
    }

    fn choose(&mut self, day: usize) {
        if day < DAY_LABELS.len() {
            self.draft.day = Some(day);
            self.after_answer();
        }
    }

    /// A rating or a habit changed: the hint follows, and chips may change width.
    fn after_answer(&mut self) {
        if self.notice == Notice::Hint {
            match core::missing(&self.entry(), &keys(&habits())) {
                Some(hint) => self.notice_text = hint.to_string(),
                None => self.notice = Notice::None,
            }
        }
        self.place();
    }

    fn set_focus(&mut self, f: Focus) {
        self.focus = f;
        unsafe {
            let _ = SetFocus(Some(if f == Focus::Note { self.note } else { self.panel.hwnd }));
        }
        self.paint();
    }

    /// The keyboard order follows the page: the rating (its chosen day, or the first; arrows move inside it), the
    /// note, each habit, add, dismiss, Save.
    fn tab(&mut self, back: bool) {
        // The name field stands where the add button or the renamed habit stands.
        let from = match self.editing {
            Some(Edit::Add) => Focus::Add,
            Some(Edit::Rename(i)) => Focus::Habit(i),
            None => self.focus,
        };
        if self.editing.is_some() {
            self.finish_edit(true, false);
        }
        let mut stops = vec![Focus::Rating(self.draft.day.unwrap_or(0)), Focus::Note];
        stops.extend((0..habits().len()).map(Focus::Habit));
        stops.push(Focus::Add);
        stops.push(Focus::Dismiss);
        if !self.saving {
            stops.push(Focus::Save);
        }
        let current = match from {
            Focus::Rating(_) => Some(0),
            f => stops.iter().position(|s| *s == f),
        };
        let n = stops.len();
        let next = match current {
            Some(c) if back => (c + n - 1) % n,
            Some(c) => (c + 1) % n,
            None if back => n - 1,
            None => 0,
        };
        self.set_focus(stops[next]);
    }

    fn begin_edit(&mut self, edit: Edit) {
        let text = match edit {
            Edit::Add => String::new(),
            Edit::Rename(i) => match habits().get(i) {
                Some(h) => h.label.clone(),
                None => return,
            },
        };
        self.editing = Some(edit);
        self.name.set(&text);
        self.name.select_all();
        self.caret = true;
        self.focus = Focus::Name;
        unsafe {
            let _ = SetFocus(Some(self.panel.hwnd));
            let _ = CreateCaret(self.panel.hwnd, None, 1, self.px(18.0));
        }
        app::blink_on();
        self.place();
    }

    /// Return confirms (`strict`: an invalid name beeps and stays), Esc cancels, leaving the field keeps a valid
    /// name. Habits change in the settings at once.
    fn finish_edit(&mut self, confirm: bool, strict: bool) {
        let Some(edit) = self.editing else { return };
        let habits = habits();
        let typed = self.name.text().to_string();
        let renaming = match edit {
            Edit::Add => None,
            Edit::Rename(i) => Some(i),
        };
        let name = if confirm { core::habit_name_for(&typed, &habits, renaming) } else { None };
        if confirm && strict && name.is_none() && !typed.trim().is_empty() {
            unsafe {
                let _ = MessageBeep(MB_OK);
            }
            return;
        }
        self.editing = None;
        unsafe {
            let _ = DestroyCaret();
        }
        match (edit, name) {
            (Edit::Add, Some(name)) => {
                config::update(|s| s.journal.habits.push(Habit { label: name.clone(), key: name.clone() }));
                self.draft.add_habit(&name);
                self.streak_base.extend(core::streaks_before(&file(), &[name], core::now().date()));
            }
            (Edit::Rename(i), Some(name)) => config::update(|s| {
                if let Some(h) = s.journal.habits.get_mut(i) {
                    h.label = name;
                }
            }),
            _ => {}
        }
        if strict || self.focus == Focus::Name {
            self.focus = Focus::Note;
        }
        self.place();
        if strict {
            self.set_focus(Focus::Note);
        }
    }

    /// Hidden from new days only: past entries keep their lines, and an entry already holding it keeps it.
    fn remove_habit(&mut self, index: usize) {
        let Some(h) = habits().into_iter().nth(index) else { return };
        config::update(|s| {
            if index < s.journal.habits.len() {
                s.journal.habits.remove(index);
            }
        });
        self.draft.remove_habit(&h.key);
        if matches!(self.focus, Focus::Habit(_)) {
            self.focus = Focus::Add;
        }
        self.place();
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        let l = self.layout();
        let inside = |bx: f32, by: f32, bw: f32, bh: f32| x >= bx && x < bx + bw && y >= by && y < by + bh;
        if let Some(i) = l.days.iter().position(|&(dx, dw)| inside(dx, PAD_TOP, dw, TOP_ROW)) {
            return Some(Hit::Rating(i));
        }
        for &(item, ix, iy, iw) in &l.items {
            match item {
                Item::Chip(i) if inside(ix - CHIP_MARGIN, iy, iw + 2.0 * CHIP_MARGIN, CHIP_H) => return Some(Hit::Habit(i)),
                Item::Add if inside(ix, iy, ADD_W, CHIP_H) => return Some(Hit::Add),
                Item::Editor if inside(ix, iy, EDIT_W, CHIP_H) => return Some(Hit::Name),
                _ => {}
            }
        }
        if inside(l.dismiss.0, l.ry, l.dismiss.1, BUTTON_H) {
            return Some(Hit::Dismiss);
        }
        inside(l.save_x, l.ry, SAVE_W, BUTTON_H).then_some(Hit::Save)
    }

    /// Keys while a drawn control has the keyboard. Returns what to run once the prompt is let go.
    fn key(&mut self, vk: VIRTUAL_KEY) -> Option<fn()> {
        let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) < 0 };
        let (ctrl, shift) = (down(VK_CONTROL) && !down(VK_MENU), down(VK_SHIFT));
        if self.editing.is_some() {
            // The habit name field owns every key but these.
            match vk {
                VK_ESCAPE => self.finish_edit(false, true),
                VK_RETURN => self.finish_edit(true, true),
                VK_TAB => self.tab(shift),
                VK_LEFT => self.name.left(shift, ctrl),
                VK_RIGHT => self.name.right(shift, ctrl),
                VK_HOME => self.name.home(shift),
                VK_END => self.name.end(shift),
                VK_BACK => self.name.backspace(ctrl),
                VK_DELETE => self.name.delete(ctrl),
                k if ctrl && k.0 == b'A' as u16 => self.name.select_all(),
                k if ctrl && k.0 == b'C' as u16 => {
                    if let Some(t) = self.name.selected_text() {
                        crate::clipboard::put_text(t);
                    }
                }
                k if ctrl && k.0 == b'X' as u16 => {
                    if let Some(t) = self.name.cut() {
                        crate::clipboard::put_text(&t);
                    }
                }
                k if ctrl && k.0 == b'V' as u16 => {
                    if let Some(t) = crate::clipboard::get_text() {
                        self.name.insert(&t);
                    }
                }
                _ => {}
            }
            self.caret = true;
            if self.editing.is_some() {
                self.paint();
            }
            return None;
        }
        match vk {
            VK_ESCAPE => return Some(escape),
            VK_RETURN => return Some(save),
            VK_TAB => self.tab(shift),
            VK_SPACE => match self.focus {
                Focus::Rating(i) => self.choose(i),
                Focus::Habit(i) => self.toggle(i),
                Focus::Add => self.begin_edit(Edit::Add),
                Focus::Dismiss => return Some(escape),
                Focus::Save => return Some(save),
                Focus::Note | Focus::Name => {}
            },
            VK_LEFT | VK_RIGHT => {
                if let Focus::Rating(i) = self.focus {
                    let j = if vk == VK_LEFT { i.saturating_sub(1) } else { (i + 1).min(DAY_LABELS.len() - 1) };
                    self.focus = Focus::Rating(j);
                    self.choose(j);
                }
            }
            _ => {}
        }
        None
    }

    /// Show the panel (and the blur behind it) on the monitor with the pointer, the keyboard in the note.
    fn present(&mut self) {
        let offscreen = config::offscreen();
        if self.panel.visible() {
            self.showing = true;
            self.panel.enter(crate::motion::Family::Bloom);
            self.paint();
            if !offscreen {
                glass::bring_to_front(self.panel.hwnd);
                unsafe { let _ = SetFocus(Some(self.note)); }
            }
            if self.mode == Mode::Due {
                let every = schedule().nag.num_milliseconds().clamp(60_000, u32::MAX as i64) as u32;
                unsafe { SetTimer(Some(self.panel.hwnd), NAG_TIMER, every, None); }
            }
            return;
        }
        let m = glass::monitor_at(glass::cursor());
        self.scale = m.scale;
        if self.font_scale != m.scale {
            let old = std::mem::replace(&mut self.font, note_font(m.scale));
            self.font_scale = m.scale;
            unsafe {
                SendMessageW(self.note, WM_SETFONT, Some(WPARAM(self.font.0 as usize)), Some(LPARAM(1)));
                if !old.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ(old.0));
                }
            }
        }
        let l = self.layout();
        self.h = l.h;
        let (w, h) = (self.px(W), self.px(l.h));
        let (x, y) = if offscreen {
            glass::offscreen_origin()
        } else {
            (m.work.left + (m.work.right - m.work.left - w) / 2, m.work.top + ((m.work.bottom - m.work.top - h) / 2).max(0))
        };
        let fg = unsafe { GetForegroundWindow() };
        self.prev = if fg.is_invalid() || app::is_ours(fg) { HWND::default() } else { fg };
        // Blur the monitor before anything of ours is on it; the panel shows its part of the same copy.
        let (bx, by, bw, bh) = if offscreen { (x, y, w, h) } else { (m.full.left, m.full.top, m.full.right - m.full.left, m.full.bottom - m.full.top) };
        let t = std::time::Instant::now();
        let backdrop = glass::screen_copy(bx, by, bw, bh).map(|p| glass::blurred(&p, bx, by, m.scale, glass::DIM));
        glass::perf("journal_backdrop", t);
        if !offscreen && let Some(b) = &backdrop {
            self.veil = show_veil(m.full, b);
        }
        self.panel.backdrop = backdrop;
        self.place();
        unsafe {
            let end = GetWindowTextLengthW(self.note).max(0) as usize;
            SendMessageW(self.note, EM_SETSEL, Some(WPARAM(end)), Some(LPARAM(end as isize)));
            SendMessageW(self.note, EM_SCROLLCARET, None, None);
        }
        self.freeze_note();
        self.panel.enter(crate::motion::Family::Bloom);
        unsafe {
            let flags = SWP_SHOWWINDOW | if offscreen { SWP_NOACTIVATE } else { SET_WINDOW_POS_FLAGS(0) };
            let _ = SetWindowPos(self.panel.hwnd, Some(HWND_TOPMOST), x, y, w, h, flags);
        }
        self.showing = true;
        self.place();
        unsafe {
            // An offscreen check copy never takes the keyboard.
            if !offscreen {
                glass::bring_to_front(self.panel.hwnd);
                let _ = SetFocus(Some(self.note));
            }
            if self.mode == Mode::Due {
                let every = schedule().nag.num_milliseconds().clamp(60_000, u32::MAX as i64) as u32;
                let _ = SetTimer(Some(self.panel.hwnd), NAG_TIMER, every, None);
            }
        }
        self.focus = Focus::Note;
        self.paint();
    }

    fn dismiss(&mut self) {
        if !self.showing {
            return;
        }
        if !self.note_clipped {
            self.freeze_note();
        }
        self.showing = false;
        if self.editing.take().is_some() {
            unsafe {
                let _ = DestroyCaret();
            }
        }
        unsafe {
            let _ = KillTimer(Some(self.panel.hwnd), NAG_TIMER);
            let _ = KillTimer(Some(self.panel.hwnd), REFRONT_TIMER);
            if GetForegroundWindow() == self.panel.hwnd && IsWindow(Some(self.prev)).as_bool() {
                let _ = SetForegroundWindow(self.prev);
            }
        }
        self.panel.exit();
        self.paint();
    }

    fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                self.paint();
                Some(LRESULT(0))
            }
            WM_COMMAND if (wparam.0 >> 16) as u32 == EN_CHANGE => {
                self.note_changed();
                Some(LRESULT(0))
            }
            WM_SETFOCUS => {
                // Activated with the note as the last focus: give it back.
                if self.focus == Focus::Note {
                    let note = self.note.0 as isize;
                    app::on_main(move || unsafe {
                        let _ = SetFocus(Some(HWND(note as _)));
                    });
                }
                self.paint();
                None
            }
            WM_KILLFOCUS => {
                if self.editing.is_some() {
                    self.finish_edit(true, false);
                } else {
                    self.paint();
                }
                None
            }
            WM_ACTIVATE => {
                let active = (wparam.0 & 0xFFFF) as u32 != WA_INACTIVE;
                unsafe {
                    if active {
                        let _ = KillTimer(Some(self.panel.hwnd), REFRONT_TIMER);
                    } else if self.showing && self.mode == Mode::Due {
                        let _ = SetTimer(Some(self.panel.hwnd), REFRONT_TIMER, REFRONT_MS, None);
                    }
                }
                None
            }
            WM_KEYDOWN => {
                if let Some(run) = self.key(VIRTUAL_KEY(wparam.0 as u16)) {
                    app::on_main(run);
                }
                Some(LRESULT(0))
            }
            WM_CHAR => {
                let u = wparam.0 as u16;
                if self.editing.is_some() {
                    let text = if (0xD800..0xDC00).contains(&u) {
                        self.high = Some(u);
                        None
                    } else if (0xDC00..0xE000).contains(&u) {
                        self.high.take().and_then(|h| String::from_utf16(&[h, u]).ok())
                    } else {
                        char::from_u32(u as u32).map(String::from)
                    };
                    if let Some(t) = text.filter(|t| !t.chars().any(char::is_control)) {
                        self.name.insert(&t);
                        self.caret = true;
                        self.paint();
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let (x, y) = ((lparam.0 & 0xFFFF) as i16 as f32 / self.scale, ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / self.scale);
                let hit = self.hit(x, y);
                if self.editing.is_some() && hit != Some(Hit::Name) {
                    self.finish_edit(true, false);
                }
                match hit {
                    Some(Hit::Rating(i)) => self.choose(i),
                    Some(Hit::Habit(i)) => self.toggle(i),
                    Some(Hit::Add) => self.begin_edit(Edit::Add),
                    Some(Hit::Dismiss) => app::on_main(escape),
                    Some(Hit::Save) => app::on_main(save),
                    Some(Hit::Name) | None => {}
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                let (x, y) = ((lparam.0 & 0xFFFF) as i16 as f32 / self.scale, ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / self.scale);
                if let Some(Hit::Habit(i)) = self.hit(x, y) {
                    app::on_main(move || habit_menu(i));
                }
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == NAG_TIMER => {
                app::on_main(bring_front);
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == REFRONT_TIMER => {
                unsafe {
                    let _ = KillTimer(Some(self.panel.hwnd), REFRONT_TIMER);
                }
                if unsafe { GetForegroundWindow() } != self.panel.hwnd {
                    app::on_main(bring_front);
                }
                Some(LRESULT(0))
            }
            // Alt+F4 means what Esc means.
            WM_CLOSE => {
                app::on_main(escape);
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

/// Right-click on a habit: Rename… or Remove.
fn habit_menu(index: usize) {
    let Some(panel) = with(|u| u.showing.then_some(u.panel.hwnd)).flatten() else { return };
    let cmd = unsafe {
        let Ok(m) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(m, MF_STRING, RENAME as usize, w!("Rename\u{2026}"));
        let _ = AppendMenuW(m, MF_STRING, REMOVE as usize, w!("Remove"));
        let pt = glass::cursor();
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, None, panel, None);
        let _ = DestroyMenu(m);
        cmd.0 as u32
    };
    match cmd {
        RENAME => {
            with(|u| u.begin_edit(Edit::Rename(index)));
        }
        REMOVE => {
            with(|u| u.remove_habit(index));
        }
        _ => {}
    }
}

/// Window messages for the prompt, its note and the blur behind it.
pub fn message(_hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    // The note asks for its colours while it paints, which can happen in the middle of our own calls.
    if msg == WM_CTLCOLOREDIT {
        let dc = HDC(wparam.0 as _);
        unsafe {
            SetTextColor(dc, colorref(dark::TEXT));
            SetBkColor(dc, COLORREF(0));
            return Some(LRESULT(GetStockObject(BLACK_BRUSH).0 as isize));
        }
    }
    with(|u| u.handle(msg, wparam, lparam)).flatten()
}

fn tab(back: bool) {
    with(|u| u.tab(back));
}

/// The caret blink in the habit name field; false when it is closed.
pub fn blink() -> bool {
    with(|u| {
        if !u.showing || u.editing.is_none() {
            return false;
        }
        u.caret = !u.caret;
        u.paint();
        true
    })
    .unwrap_or(false)
}

/// Every 30 s and on wake: open the prompt when it is due.
pub fn check() {
    // A `--test` copy asks on its own only after `show_test`, and then only into the test file.
    if config::test_mode() && SCHED.with(|s| s.borrow().test_file.is_none()) {
        return;
    }
    if !config::get().journal.enabled || showing() {
        return;
    }
    let (s, now) = (schedule(), core::now());
    if !SCHED.with(|x| x.borrow().tracker.due(&s, now, false)) {
        return;
    }
    if (!config::offscreen() && vesper_running()) || done_on(now.date()) {
        return;
    }
    // Never over a break: the check after it asks.
    if crate::breaks::on_screen() {
        return;
    }
    open(Mode::Due);
}

fn open(mode: Mode) {
    // Busy (a broadcast arrived in the middle of our own work): the next check asks again.
    let Ok(state) = UI.with(|u| u.try_borrow().map(|u| u.as_ref().map(|u| u.showing))) else { return };
    match state {
        Some(true) => return bring_front(),
        Some(false) => {}
        None => {
            let built = build();
            UI.with(|u| {
                if let Ok(mut slot) = u.try_borrow_mut() {
                    *slot = Some(built);
                }
            });
        }
    }
    with(|u| {
        if !u.panel.visible() {
            u.prepare(mode);
        }
        u.present();
    });
}

fn bring_front() {
    if crate::breaks::on_screen() {
        return;
    }
    let Some((panel, veil, note, focus)) = with(|u| u.showing.then_some((u.panel.hwnd, u.veil, u.note, u.focus))).flatten() else { return };
    unsafe {
        if !veil.is_invalid() {
            let _ = SetWindowPos(veil, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        let _ = SetWindowPos(panel, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        if !config::offscreen() {
            glass::bring_to_front(panel);
            if focus == Focus::Note {
                let _ = SetFocus(Some(note));
            }
        }
    }
}

/// Esc and the dismiss button: snooze when due, close until the hour when early, cancel an edit.
fn escape() {
    let Some(mode) = with(|u| (u.showing && !u.saving).then_some(u.mode)).flatten() else { return };
    let (s, now) = (schedule(), core::now());
    SCHED.with(|x| {
        let mut x = x.borrow_mut();
        match mode {
            Mode::Due => x.tracker.snooze(&s, now),
            Mode::Early => x.tracker.close(&s, now),
            Mode::Edit => {}
        }
    });
    with(|u| u.dismiss());
}

/// Save on a background thread; on failure the form stays as typed and the day stays pending.
fn save() {
    let keys = keys(&habits());
    let entry = with(|u| (u.showing && !u.saving && u.editing.is_none()).then(|| u.entry())).flatten();
    let Some(entry) = entry.filter(|e| e.is_complete(&keys)) else {
        // Say the first thing still missing instead of only refusing.
        with(|u| {
            if let Some(hint) = core::missing(&u.entry(), &keys) {
                u.set_notice(Notice::Hint, hint);
            }
        });
        return;
    };
    let now = core::now();
    let snoozes = SCHED.with(|s| s.borrow().tracker.snoozes(now));
    let path = file();
    with(|u| {
        u.saving = true;
        u.notice = Notice::None;
        u.place();
    });
    std::thread::spawn(move || {
        let result = core::save(&path, &entry, snoozes, now).map_err(|e| format!("{}: {e}", path.display()));
        app::on_main(move || saved(result));
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
            let centre = with(|u| {
                u.saving = false;
                u.draft.forget();
                let mut r = RECT::default();
                let _ = unsafe { GetWindowRect(u.panel.hwnd, &mut r) };
                u.dismiss();
                POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 }
            });
            if let Some(c) = centre {
                glass::toast("Saved", c);
            }
        }
        Err(e) => {
            eprintln!("livae: journal not saved: {e}");
            with(|u| {
                u.saving = false;
                u.set_notice(Notice::Error, &e);
            });
        }
    }
}

/// Start the schedule: the app checks every 30 s and on wake; one check now (as Vesper does at launch).
pub fn install() {
    check();
}

/// Open now. Before the hour it can be closed until then; once today is saved it edits today's entry.
pub fn show() {
    let now = core::now();
    open(schedule().mode(now, done_on(now.date())));
}

/// `--journal [file]`: open with every read and write in `journal-test.md` in the data folder (the fixture folder
/// under `--test`), never the vault. `fixture` seeds it (copied in unless it already lies there); none starts empty.
pub fn show_test(fixture: Option<PathBuf>) {
    let dir = config::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let inside = |f: &Path| match (f.canonicalize(), dir.canonicalize()) {
        (Ok(f), Ok(d)) => f.starts_with(d),
        _ => false,
    };
    let to = dir.join(TEST_FILE);
    let path = match fixture {
        Some(f) if inside(&f) => f,
        Some(f) => {
            if let Err(e) = std::fs::copy(&f, &to) {
                eprintln!("livae: journal fixture {} not copied: {e}", f.display());
            }
            to
        }
        None => {
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

/// `--dump`: what the prompt shows, top to bottom, and where its windows are.
pub fn dump() -> String {
    let path = file();
    let status = status_line();
    with(|u| {
        let mut s = String::new();
        let rect = |h: HWND| {
            if h.is_invalid() || !unsafe { IsWindowVisible(h) }.as_bool() {
                return "hidden".to_string();
            }
            let mut r = RECT::default();
            let _ = unsafe { GetWindowRect(h, &mut r) };
            format!("{} {} {} {}", r.left, r.top, r.right - r.left, r.bottom - r.top)
        };
        let entry = u.entry();
        let _ = writeln!(s, "panel journal");
        let _ = writeln!(s, "file {}", path.display());
        let _ = writeln!(s, "mode {:?}", u.mode);
        let _ = writeln!(s, "window {}", rect(u.panel.hwnd));
        let _ = writeln!(s, "note-box {}", rect(u.note));
        let _ = writeln!(s, "veil {}", rect(u.veil));
        let _ = writeln!(s, "scale {}", u.scale);
        let _ = writeln!(s, "date {}", core::date_label(u.date));
        let ratings: Vec<String> = DAY_LABELS.iter().enumerate().map(|(i, l)| if u.draft.day == Some(i) { format!("[{l}]") } else { l.to_string() }).collect();
        let _ = writeln!(s, "ratings {}", ratings.join(" "));
        if entry.note.is_empty() {
            let _ = writeln!(s, "note (placeholder) {PLACEHOLDER}");
        } else {
            let _ = writeln!(s, "note {:?}", entry.note);
        }
        let _ = writeln!(s, "counter {} / {MIN_CHARS}", entry.note_len());
        for h in habits() {
            let answer = match u.answer(&h.key) {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unanswered",
            };
            let n = u.streak(&h);
            let _ = writeln!(s, "habit {}\t{answer}\t{}", h.label, if n >= 2 { format!("{n} days") } else { String::new() });
        }
        let _ = writeln!(s, "add {}", match u.editing {
            Some(e) => format!("editing {e:?} {:?}", u.name.text()),
            None => "+".into(),
        });
        let _ = writeln!(s, "dismiss {}", u.dismiss_title());
        let state = if u.saving { "saving" } else if entry.is_complete(&keys(&habits())) { "primary" } else { "outline" };
        let _ = writeln!(s, "save {state}");
        let notice = match u.notice {
            Notice::None => "-".to_string(),
            Notice::Hint => u.notice_text.clone(),
            Notice::Error => format!("{ERROR_TEXT} ({})", u.notice_text),
        };
        let _ = writeln!(s, "notice {notice}");
        let _ = writeln!(s, "keyboard {:?}", u.focus);
        let _ = writeln!(s, "status {status}");
        s
    })
    .unwrap_or_default()
}
