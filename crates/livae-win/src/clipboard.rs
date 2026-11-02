//! Clipboard history on its own shortcut (Hyper+V), one list like the Mac.
//!
//! Watching: the main window listens for clipboard changes (AddClipboardFormatListener). Copies marked private are
//! never kept: "ExcludeClipboardContentFromMonitorProcessing", "Clipboard Viewer Ignore", or
//! "CanIncludeInClipboardHistory" set to 0 (password managers set these). Text, images (PNG, or a DIB converted to
//! PNG with WIC) and files are kept in `%APPDATA%\Livae\clipboard.json`, images in `clips\`, through the shared
//! model in livae-core. Text over 1 MB and images over 24 MB are skipped; our own writes are ignored.
//!
//! Showing: one glass list at the text caret of the app in front (else at the mouse), its text column under the
//! caret. Enter, a click or Ctrl+1..9 puts the item back on the clipboard, hands the focus back to that app and
//! presses Ctrl+V. Ctrl+P pins, Ctrl+Delete deletes, Esc closes.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use livae_core::field::Field;
use livae_core::history::{self, hash_of, Clip, History, Kind, MAX_IMAGE, MAX_TEXT};
use livae_core::settings::Settings;
use windows::core::w;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::HiDpi::{GetWindowDpiAwarenessContext, LogicalToPhysicalPointForPerMonitorDPI, SetThreadDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::glass::{self, glyph, gfx, Font, Panel, Trim, BAND, BAND_RADIUS, SECONDARY, TERTIARY, TEXT};
use crate::shell::{self, Pixels};
use crate::{app, config};

/// The search's size: as wide, the same capsule, and a list never taller than the search's (Results shown).
const WIDTH: f32 = 720.0;
const FIELD_H: f32 = 72.0;
const QUERY: f32 = livae_core::tokens::text::H3.size as f32;
/// The search's row height: the list is at most Results shown rows of it tall, whatever its own rows are.
const SEARCH_ROW_H: f32 = 46.0;
const INSET: f32 = 8.0;
const EDGE: f32 = 16.0;
/// The text column. No kind icons: text starts where the search icon starts.
const TEXT_X: f32 = 24.0;
/// The field's text in its capsule, after the search icon.
const FIELD_X: f32 = 56.0;
/// The most rows on screen; how many show depends on their heights and the list's height.
const SLOTS: usize = livae_core::settings::RESULTS_MAX;
const THUMB_W: f32 = 120.0;
const THUMB_H: f32 = 68.0;
/// The quiet time column at the right.
const TIME_W: f32 = 56.0;
const WM_CHECK_FOCUS: u32 = WM_APP + 31;

static PAUSED: AtomicBool = AtomicBool::new(false);

pub fn paused() -> bool {
    PAUSED.load(Ordering::SeqCst)
}

/// Stop or resume recording copies (until Livae quits).
pub fn toggle_paused() {
    PAUSED.fetch_xor(true, Ordering::SeqCst);
}

struct Formats {
    png: u32,
    exclude: u32,
    ignore: u32,
    history: u32,
    drop_effect: u32,
}

fn formats() -> &'static Formats {
    static F: OnceLock<Formats> = OnceLock::new();
    F.get_or_init(|| unsafe {
        Formats {
            png: RegisterClipboardFormatW(w!("PNG")),
            exclude: RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")),
            ignore: RegisterClipboardFormatW(w!("Clipboard Viewer Ignore")),
            history: RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")),
            drop_effect: RegisterClipboardFormatW(w!("Preferred DropEffect")),
        }
    })
}

fn clips_dir() -> PathBuf {
    config::data_dir().join("clips")
}

fn store_file() -> PathBuf {
    config::data_dir().join("clipboard.json")
}

// Raw clipboard access. Windows lets one window open the clipboard at a time; another app may hold it for a moment.

struct Opened;

impl Drop for Opened {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

fn open() -> Option<Opened> {
    for _ in 0..10 {
        if unsafe { OpenClipboard(Some(app::main_hwnd())) }.is_ok() {
            return Some(Opened);
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    None
}

fn available(fmt: u32) -> bool {
    unsafe { IsClipboardFormatAvailable(fmt) }.is_ok()
}

/// A format's bytes (clipboard open). Larger than `max`: None.
fn read_bytes(fmt: u32, max: usize) -> Option<Vec<u8>> {
    unsafe {
        let h = GetClipboardData(fmt).ok()?;
        let g = HGLOBAL(h.0);
        let size = GlobalSize(g);
        if size == 0 || size > max {
            return None;
        }
        let p = GlobalLock(g) as *const u8;
        if p.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(p, size).to_vec();
        let _ = GlobalUnlock(g);
        Some(v)
    }
}

fn read_text() -> Option<String> {
    let bytes = read_bytes(CF_UNICODETEXT.0 as u32, MAX_TEXT * 4 + 2)?;
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).take_while(|&u| u != 0).collect();
    Some(String::from_utf16_lossy(&units))
}

fn read_files() -> Vec<String> {
    unsafe {
        let Ok(h) = GetClipboardData(CF_HDROP.0 as u32) else { return Vec::new() };
        let drop = HDROP(h.0);
        let n = DragQueryFileW(drop, u32::MAX, None);
        (0..n.min(1000))
            .filter_map(|i| {
                let len = DragQueryFileW(drop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                let got = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
                (got > 0).then(|| String::from_utf16_lossy(&buf[..got]))
            })
            .collect()
    }
}

/// Put `bytes` on the clipboard as `fmt` (clipboard open and emptied). Windows owns the memory afterwards.
fn write_bytes(fmt: u32, bytes: &[u8]) -> bool {
    unsafe {
        let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) else { return false };
        let p = GlobalLock(g) as *mut u8;
        if p.is_null() {
            let _ = GlobalFree(Some(g));
            return false;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(g);
        if SetClipboardData(fmt, Some(HANDLE(g.0))).is_err() {
            let _ = GlobalFree(Some(g));
            return false;
        }
        true
    }
}

fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect()
}

/// CF_HDROP: a DROPFILES header, then the paths, each ending in a NUL, then one more NUL.
fn dropfiles(paths: &[&str]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend(20u32.to_le_bytes()); // offset of the file list
    v.extend([0u8; 12]); // point and "non-client" flag
    v.extend(1u32.to_le_bytes()); // wide characters
    for p in paths {
        v.extend(utf16z(p));
    }
    v.extend([0u8, 0]);
    v
}

/// A clipboard DIB as a BMP file, which WIC decodes. The pixels start after the header, the colour masks of a
/// bitfields bitmap with a basic header, and the palette.
fn dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let u32_at = |i: usize| u32::from_le_bytes([dib[i], dib[i + 1], dib[i + 2], dib[i + 3]]);
    let header = u32_at(0) as usize;
    let bits = u16::from_le_bytes([dib[14], dib[15]]) as u32;
    let compression = u32_at(16);
    let colors = u32_at(32);
    let masks = match (compression, header) {
        (3, 40) => 12, // BI_BITFIELDS
        (6, 40) => 16, // BI_ALPHABITFIELDS
        _ => 0,
    };
    let palette = if colors > 0 { colors.min(256) } else if bits <= 8 { 1 << bits } else { 0 } as usize * 4;
    let offset = 14 + header + masks + palette;
    let mut out = Vec::with_capacity(14 + dib.len());
    out.extend(b"BM");
    out.extend(((14 + dib.len()) as u32).to_le_bytes());
    out.extend([0u8; 4]);
    out.extend((offset as u32).to_le_bytes());
    out.extend(dib);
    Some(out)
}

/// A 32-bit bottom-up DIB for apps that do not read PNG; transparent parts on white.
fn dib_from(p: &Pixels) -> Vec<u8> {
    let mut v = Vec::with_capacity(40 + p.data.len());
    v.extend(40u32.to_le_bytes());
    v.extend((p.w as i32).to_le_bytes());
    v.extend((p.h as i32).to_le_bytes());
    v.extend(1u16.to_le_bytes());
    v.extend(32u16.to_le_bytes());
    v.extend(0u32.to_le_bytes()); // BI_RGB
    v.extend((p.w * p.h * 4).to_le_bytes());
    v.extend([0u8; 16]);
    let row = (p.w * 4) as usize;
    for y in (0..p.h as usize).rev() {
        for px in p.data[y * row..(y + 1) * row].as_chunks::<4>().0 {
            let a = px[3] as u32;
            let over = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
            v.extend([over(px[0]), over(px[1]), over(px[2]), 255]);
        }
    }
    v
}

/// The PNG data up to its end chunk (clipboard memory can be rounded up past it).
fn trim_png(bytes: &[u8]) -> &[u8] {
    match bytes.windows(4).rposition(|w| w == b"IEND") {
        Some(i) if i + 8 <= bytes.len() => &bytes[..i + 8],
        _ => bytes,
    }
}

/// Terminals and editors, so copied commands read as code.
fn app_kind(exe: &str) -> &str {
    match exe.to_lowercase().as_str() {
        "windowsterminal" | "wt" | "cmd" | "powershell" | "pwsh" | "wezterm-gui" | "alacritty" | "mintty" | "conhost" => "Terminal",
        "code" | "code - insiders" => "Code",
        "cursor" => "Cursor",
        "zed" => "Zed",
        _ => exe,
    }
}

/// Put text on the clipboard (from the launcher: a calculator answer, the field's selection). It is recorded like
/// any copy.
pub fn put_text(text: &str) {
    if let Some(_open) = open() && unsafe { EmptyClipboard() }.is_ok() {
        write_bytes(CF_UNICODETEXT.0 as u32, &utf16z(text));
    }
}

pub fn get_text() -> Option<String> {
    let _open = open()?;
    available(CF_UNICODETEXT.0 as u32).then(read_text).flatten()
}

/// What a copy is, read while the clipboard is open; the slow part (images) runs after it is closed.
enum Raw {
    Files(Vec<String>),
    Png(Vec<u8>),
    Dib(Vec<u8>),
    Text(String),
}

/// Where the app in front shows its text caret, in screen pixels. Windows reports it in the app's own client
/// coordinates and DPI; both are converted here. A rectangle outside the app's window is a stale caret: None.
fn caret_rect(front: HWND) -> Option<RECT> {
    unsafe {
        if front.is_invalid() {
            return None;
        }
        let thread = GetWindowThreadProcessId(front, None);
        let mut gti = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        GetGUIThreadInfo(thread, &mut gti).ok()?;
        let hwnd = gti.hwndCaret;
        let r = gti.rcCaret;
        if hwnd.is_invalid() || r.bottom <= r.top {
            return None;
        }
        let mut top = POINT { x: r.left, y: r.top };
        let mut bottom = POINT { x: r.left, y: r.bottom };
        let old = SetThreadDpiAwarenessContext(GetWindowDpiAwarenessContext(hwnd));
        let _ = ClientToScreen(hwnd, &mut top);
        let _ = ClientToScreen(hwnd, &mut bottom);
        if !old.is_invalid() {
            SetThreadDpiAwarenessContext(old);
        }
        let _ = LogicalToPhysicalPointForPerMonitorDPI(Some(hwnd), &mut top);
        let _ = LogicalToPhysicalPointForPerMonitorDPI(Some(hwnd), &mut bottom);
        let mut wr = RECT::default();
        GetWindowRect(GetAncestor(hwnd, GA_ROOT), &mut wr).ok()?;
        let inside = top.x >= wr.left - 8 && top.x <= wr.right + 8 && top.y >= wr.top - 8 && bottom.y <= wr.bottom + 8;
        inside.then_some(RECT { left: top.x, top: top.y, right: top.x + 1, bottom: bottom.y.max(top.y + 1) })
    }
}

pub struct Clipboard {
    panel: Panel,
    band: crate::motion::Band,
    field: Field,
    history: History,
    /// Indices into the history matching the query, in display order.
    shown: Vec<usize>,
    first: usize,
    selected: usize,
    /// (top, height) in DIPs of each row on screen, for the mouse.
    slots: Vec<(f32, f32)>,
    /// The clipboard sequence number after our own last write.
    own_seq: u32,
    caret: bool,
    high: Option<u16>,
    mouse: POINT,
}

thread_local! {
    static CLIPBOARD: RefCell<Option<Clipboard>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Clipboard) -> R) -> Option<R> {
    CLIPBOARD.with(|cell| cell.try_borrow_mut().ok().and_then(|mut slot| slot.as_mut().map(f)))
}

pub fn install() {
    let _ = formats();
    let mut history = History::new(history::load(&store_file()));
    history.reclassify();
    let mut c = Clipboard {
        panel: Panel::new(glass::CLIP_FIELD, glass::CLIP_CARD, WIDTH, FIELD_H),
        band: crate::motion::Band::default(),
        field: Field::default(),
        history,
        shown: Vec::new(),
        first: 0,
        selected: 0,
        slots: Vec::new(),
        own_seq: 0,
        caret: true,
        high: None,
        mouse: POINT::default(),
    };
    c.panel.family = crate::motion::Family::Clipboard;
    CLIPBOARD.with(|cell| *cell.borrow_mut() = Some(c));
    // Whatever is on the clipboard now is the first item.
    changed();
}

/// WM_CLIPBOARDUPDATE.
pub fn changed() {
    with(|c| c.capture());
}

pub fn show() {
    show_for(HWND::default());
}

/// Open for `prev` (the app the launcher was opened over), or for the app in front when that is null.
pub fn show_for(prev: HWND) {
    with(|c| c.show(prev));
    crate::launcher::hide();
}

pub fn hide() {
    with(|c| c.hide(false));
}

pub fn toggle() {
    let open = with(|c| c.panel.showing()).unwrap_or(false);
    if open { with(|c| c.hide(true)); } else { show(); }
}

pub fn set_query(q: &str) {
    with(|c| {
        c.field.set(q);
        c.query_changed();
    });
}

/// The app the clipboard was opened over.
pub fn front() -> HWND {
    with(|c| c.panel.prev).unwrap_or_default()
}

pub fn blink() -> bool {
    with(|c| {
        if !c.panel.visible() {
            return false;
        }
        c.caret = !c.caret;
        c.paint_field();
        true
    })
    .unwrap_or(false)
}

pub fn settings_changed(before: &Settings, after: &Settings) {
    with(|c| {
        if before.history != after.history {
            c.prune();
            c.save();
        }
        if c.panel.visible() {
            c.filter();
            c.paint_field();
        }
    });
}

/// Press Ctrl+V in the app we handed the focus back to.
pub fn send_paste() {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() || app::is_ours(fg) {
            glass::toast("Copied. Press Ctrl+V to paste", glass::cursor());
            return;
        }
        let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, time: 0, dwExtraInfo: 0 } },
        };
        let v = VIRTUAL_KEY(b'V' as u16);
        let inputs = [key(VK_CONTROL, false), key(v, false), key(v, true), key(VK_CONTROL, true)];
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

fn now() -> i64 {
    history::now()
}

fn remove_images(names: impl IntoIterator<Item = String>) {
    let dir = clips_dir();
    for n in names {
        let _ = std::fs::remove_file(dir.join(n));
    }
}

impl Clipboard {
    fn capture(&mut self) {
        if config::test_mode() || paused() {
            return;
        }
        let f = formats();
        let seq = unsafe { GetClipboardSequenceNumber() };
        if seq != 0 && seq == self.own_seq {
            return; // our own write
        }
        // Password managers mark what must never be kept.
        if available(f.exclude) || available(f.ignore) {
            return;
        }
        let app = shell::process_name(unsafe { GetForegroundWindow() });
        let ignored = config::get().ignore_apps.iter().any(|a| a.trim_end_matches(".exe").eq_ignore_ascii_case(&app));
        if ignored {
            return;
        }
        let raw = {
            let Some(_open) = open() else { return };
            if available(f.history) && read_bytes(f.history, 64).is_some_and(|b| b.len() >= 4 && b[..4] == [0, 0, 0, 0]) {
                return;
            }
            let text = available(CF_UNICODETEXT.0 as u32).then(read_text).flatten();
            if available(CF_HDROP.0 as u32) {
                Raw::Files(read_files())
            } else {
                // The format the app put first is its own; the others Windows made from it.
                let mut image = None;
                let mut fmt = 0;
                for _ in 0..200 {
                    fmt = unsafe { EnumClipboardFormats(fmt) };
                    if fmt == 0 {
                        break;
                    }
                    if fmt == f.png || fmt == CF_DIB.0 as u32 || fmt == CF_DIBV5.0 as u32 {
                        image = Some(fmt);
                        break;
                    }
                }
                // "Copy image" in a browser also puts the image's address as text: keep the image then.
                let bare = |t: &str| {
                    let t = t.trim();
                    t.is_empty() || (!t.contains(char::is_whitespace) && t.contains("://"))
                };
                match (image, text) {
                    (Some(fmt), t) if t.as_deref().is_none_or(bare) => {
                        let Some(bytes) = read_bytes(fmt, 256 << 20) else { return };
                        if fmt == f.png { Raw::Png(bytes) } else { Raw::Dib(bytes) }
                    }
                    (_, Some(t)) => Raw::Text(t),
                    _ => return,
                }
            }
        };
        let mut clip = Clip { time: now(), ..Default::default() };
        match raw {
            Raw::Files(paths) if !paths.is_empty() => {
                clip.kind = Kind::File;
                clip.text = paths.join("\n");
                clip.hash = hash_of(clip.text.as_bytes());
            }
            Raw::Text(t) if !t.trim().is_empty() && t.len() <= MAX_TEXT => {
                clip.kind = history::classify(&t, app_kind(&app));
                clip.hash = hash_of(t.as_bytes());
                clip.text = t;
            }
            Raw::Png(bytes) => {
                let bytes = trim_png(&bytes);
                if bytes.len() > MAX_IMAGE || !self.keep_image(&mut clip, bytes, true) {
                    return;
                }
            }
            Raw::Dib(bytes) => {
                let Some(bmp) = dib_to_bmp(&bytes) else { return };
                if !self.keep_image(&mut clip, &bmp, false) {
                    return;
                }
            }
            _ => return,
        }
        clip.app = app;
        if let Some(unused) = self.history.add(clip) {
            remove_images([unused]);
        }
        self.prune();
        self.save();
        // Always: rows on screen and the card hold indices into the history.
        self.filter();
    }

    /// Decode, hash by pixels (the same picture as PNG or DIB is one item) and save a new image as PNG.
    fn keep_image(&mut self, clip: &mut Clip, bytes: &[u8], is_png: bool) -> bool {
        let Some(px) = shell::decode(bytes) else { return false };
        clip.kind = Kind::Image;
        clip.w = px.w;
        clip.h = px.h;
        clip.hash = hash_of(&px.data) ^ ((px.w as u64) << 32 | px.h as u64);
        if self.history.has(Kind::Image, clip.hash) {
            return true; // the history keeps the first file
        }
        let name = self.history.next_image_name();
        let path = clips_dir().join(&name);
        let _ = std::fs::create_dir_all(clips_dir());
        let written = if is_png { std::fs::write(&path, bytes).is_ok() } else { shell::encode_png(&px, &path) };
        if !written || std::fs::metadata(&path).map_or(true, |m| m.len() as usize > MAX_IMAGE) {
            let _ = std::fs::remove_file(&path);
            return false;
        }
        clip.image = Some(name);
        true
    }

    fn prune(&mut self) {
        remove_images(self.history.prune(config::get().history));
    }

    fn save(&self) {
        if !config::test_mode() {
            let _ = history::save(&store_file(), &self.history.clips);
        }
    }

    /// Put a history item back on the clipboard.
    fn put(&mut self, clip: &Clip) -> bool {
        let f = formats();
        let image = match (clip.kind, &clip.image) {
            (Kind::Image, Some(name)) => {
                let Ok(bytes) = std::fs::read(clips_dir().join(name)) else { return false };
                let px = shell::decode(&bytes);
                Some((bytes, px))
            }
            (Kind::Image, None) => return false,
            _ => None,
        };
        {
            let Some(_open) = open() else { return false };
            if unsafe { EmptyClipboard() }.is_err() {
                return false;
            }
            match clip.kind {
                Kind::Image => {
                    if let Some((bytes, px)) = &image {
                        write_bytes(f.png, bytes);
                        if let Some(px) = px {
                            write_bytes(CF_DIB.0 as u32, &dib_from(px));
                        }
                    }
                }
                Kind::File => {
                    let paths: Vec<&str> = clip.text.lines().filter(|l| !l.trim().is_empty()).collect();
                    write_bytes(CF_HDROP.0 as u32, &dropfiles(&paths));
                    write_bytes(f.drop_effect, &1u32.to_le_bytes()); // copy, not move
                    write_bytes(CF_UNICODETEXT.0 as u32, &utf16z(&clip.text));
                }
                _ => {
                    write_bytes(CF_UNICODETEXT.0 as u32, &utf16z(&clip.text));
                }
            }
        }
        self.own_seq = unsafe { GetClipboardSequenceNumber() };
        true
    }

    fn show(&mut self, prev: HWND) {
        if self.panel.visible() && !self.panel.showing() {
            self.panel.show(!config::offscreen());
            self.paint_all();
            return;
        }
        self.band.reset();
        if prev.is_invalid() {
            self.panel.remember_front(crate::launcher::front());
        } else {
            self.panel.prev = prev;
        }
        self.field.clear();
        self.selected = 0;
        self.first = 0;
        self.mouse = glass::cursor();
        self.filter();
        if config::offscreen() {
            (self.panel.x, self.panel.top) = glass::offscreen_origin();
            self.panel.scale = glass::monitor_at(POINT::default()).scale;
        } else {
            self.place();
        }
        self.panel.show(!config::offscreen());
        self.caret = true;
        app::blink_on();
        self.paint_all();
    }

    /// At the text caret when the app reports one, else at the mouse; above the line when there is no room below;
    /// always inside the monitor's work area.
    fn place(&mut self) {
        let mouse = glass::cursor();
        let (anchor_x, below, above, gap) = match caret_rect(self.panel.prev) {
            Some(r) => (r.left, r.bottom, r.top, 8.0),
            None => (mouse.x, mouse.y, mouse.y, 12.0),
        };
        let m = glass::monitor_at(POINT { x: anchor_x, y: below });
        self.panel.scale = m.scale;
        let p = &self.panel;
        let (gap, edge, total) = (p.px(gap), p.px(8.0), p.total_px(p.card_h));
        let x = glass::fit(anchor_x - p.px(TEXT_X), m.work.left + edge, m.work.right - p.px(WIDTH) - edge);
        let mut top = below + gap;
        if top + total > m.work.bottom - edge {
            top = above - gap - total;
        }
        let top = glass::fit(top, m.work.top + edge, m.work.bottom - total - edge);
        self.panel.x = x;
        self.panel.top = top;
    }

    fn hide(&mut self, restore: bool) {
        if self.panel.visible() {
            self.panel.hide(restore);
        }
    }

    fn query_changed(&mut self) {
        self.band.reset();
        self.selected = 0;
        self.first = 0;
        self.caret = true;
        self.filter();
        self.paint_field();
    }

    fn filter(&mut self) {
        self.shown = self.history.order(self.field.text());
        self.selected = self.selected.min(self.shown.len().saturating_sub(1));
        self.first = self.first.min(self.selected);
        self.layout();
    }

    fn row_h(c: &Clip) -> f32 {
        match c.kind {
            Kind::Image => THUMB_H + 12.0,
            // Two lines only for text that has two (as on the Mac); a long single line stays one line.
            Kind::Text | Kind::Code if c.text.trim().contains('\n') => 60.0,
            _ => 44.0,
        }
    }

    /// The list's room for rows: as tall as the search's list at Results shown.
    fn room() -> f32 {
        livae_core::settings::rows_shown(config::get().results) as f32 * SEARCH_ROW_H
    }

    /// How many items fit from `first` (at least one).
    fn fit_from(&self, first: usize) -> usize {
        let (room, mut used, mut n) = (Self::room(), 0.0, 0);
        for &i in self.shown.iter().skip(first).take(SLOTS) {
            used += Self::row_h(&self.history.clips[i]);
            if used > room && n > 0 {
                break;
            }
            n += 1;
        }
        n.max(1)
    }

    /// The first item to show so that `last` is the bottom row.
    fn first_ending_at(&self, last: usize) -> usize {
        let (room, mut first) = (Self::room(), last);
        let mut used = Self::row_h(&self.history.clips[self.shown[last]]);
        while first > 0 && last - first + 1 < SLOTS {
            let h = Self::row_h(&self.history.clips[self.shown[first - 1]]);
            if used + h > room {
                break;
            }
            used += h;
            first -= 1;
        }
        first
    }

    fn layout(&mut self) {
        let visible: Vec<usize> = self.shown.iter().skip(self.first).take(self.fit_from(self.first)).copied().collect();
        self.slots.clear();
        let mut y = INSET;
        if visible.is_empty() {
            self.slots.push((y, 44.0));
            y += 44.0;
        }
        for &i in &visible {
            let h = Self::row_h(&self.history.clips[i]);
            self.slots.push((y, h));
            y += h;
        }
        self.panel.set_card(y + INSET);
        self.paint_card();
    }

    fn move_selection(&mut self, by: isize) {
        if self.shown.is_empty() {
            return;
        }
        let n = self.shown.len() as isize;
        self.selected = (self.selected as isize + by).rem_euclid(n) as usize;
        if self.selected < self.first {
            self.first = self.selected;
        } else if self.selected >= self.first + self.fit_from(self.first) {
            self.first = self.first_ending_at(self.selected);
        }
        self.layout();
    }

    fn scroll(&mut self, rows: isize) {
        let Some(last) = self.shown.len().checked_sub(1) else { return };
        let max_first = self.first_ending_at(last) as isize;
        self.first = (self.first as isize + rows).clamp(0, max_first) as usize;
        let fit = self.fit_from(self.first);
        self.selected = self.selected.clamp(self.first, (self.first + fit - 1).min(last));
        self.layout();
    }

    /// Put the item back on the clipboard, hand the focus back, and paste.
    fn paste(&mut self, display_index: usize) {
        let Some(clip) = self.shown.get(display_index).and_then(|&i| self.history.clips.get(i)).cloned() else { return };
        let i = self.shown[display_index];
        if !self.put(&clip) {
            return;
        }
        // Used again: it moves to the top.
        self.history.touch(i, now());
        self.save();
        let at = glass::cursor();
        self.hide(true);
        if config::get().paste {
            app::paste_soon();
        } else {
            glass::toast("Copied. Press Ctrl+V to paste", at);
        }
    }

    fn pin(&mut self) {
        if let Some(&i) = self.shown.get(self.selected) {
            self.history.toggle_pin(i);
            self.save();
            self.filter();
        }
    }

    fn delete(&mut self) {
        if let Some(&i) = self.shown.get(self.selected) {
            if let Some(c) = self.history.remove(i) {
                remove_images(c.image);
            }
            self.save();
            self.filter();
        }
    }

    fn darkness(&self) -> f32 {
        // It sits over the text being written: dark enough that nothing behind stays readable.
        config::get().glass.max(0.6) as f32
    }

    fn paint_all(&mut self) {
        self.paint_field();
        self.paint_card();
    }

    fn paint_field(&mut self) {
        let (scale, dark, caret) = (self.panel.scale, self.darkness(), self.caret);
        let field = &self.field;
        let mut at = None;
        self.panel.follow();
        self.panel.capsule.draw(scale, |c| {
            c.glass(dark);
            c.glyph(glyph::CLIPBOARD, 18.0, EDGE + 2.0, 0.0, 28.0, c.h, SECONDARY);
            let fmt = gfx().format(Font::Sans, QUERY, DWRITE_FONT_WEIGHT_NORMAL, false, false);
            at = Some(c.field(field, &fmt, FIELD_X, WIDTH - FIELD_X - EDGE, "Search clipboard", caret));
        });
        if let Some(at) = at {
            self.panel.set_caret(at);
        }
    }

    /// A row's (title, detail) as drawn and dumped.
    fn texts(&self, c: &Clip) -> (String, String) {
        match c.kind {
            Kind::Image => ("Image".into(), format!("{} \u{d7} {}", c.w, c.h)),
            Kind::File => {
                let mut paths = c.text.lines().filter(|l| !l.trim().is_empty());
                let first = Path::new(paths.next().unwrap_or(""));
                let more = paths.count();
                let name = first.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| first.to_string_lossy().to_string());
                let name = if more > 0 { format!("{name} +{more}") } else { name };
                (name, first.parent().map(crate::launcher::home_text).unwrap_or_default())
            }
            _ => {
                let t: String = c.text.chars().take(600).collect();
                let lines: Vec<&str> = t.trim().lines().map(str::trim).filter(|l| !l.is_empty()).take(2).collect();
                (lines.first().copied().unwrap_or("").to_string(), lines.get(1).copied().unwrap_or("").to_string())
            }
        }
    }

    fn paint_card(&mut self) {
        let g = gfx();
        let body = g.format(Font::Sans, 16.0, DWRITE_FONT_WEIGHT_NORMAL, false, false);
        let code = g.format(Font::Mono, 14.0, DWRITE_FONT_WEIGHT_NORMAL, false, false);
        let small = g.format(Font::Sans, 14.0, DWRITE_FONT_WEIGHT_NORMAL, false, false);
        let small_right = g.format(Font::Sans, 14.0, DWRITE_FONT_WEIGHT_NORMAL, false, true);
        let text_right = WIDTH - EDGE - TIME_W - 24.0;
        let now = now();
        let dir = clips_dir();
        let empty_msg = if self.history.clips.is_empty() {
            "Nothing copied yet".to_string()
        } else {
            format!("No copied item matches \u{201c}{}\u{201d}", self.field.text().trim())
        };
        let rows: Vec<(f32, f32, Clip)> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(slot, &(y, h))| {
                let i = *self.shown.get(self.first + slot)?;
                let c = self.history.clips.get(i)?;
                let light = Clip { text: c.text.chars().take(600).collect(), image: c.image.clone(), app: String::new(), ..*c };
                Some((y, h, light))
            })
            .collect();
        let texts: Vec<(String, String)> = rows.iter().map(|r| self.texts(&r.2)).collect();
        let (scale, dark) = (self.panel.scale, self.darkness() * 0.85);
        let target = self.slots.get(self.selected.saturating_sub(self.first)).copied().unwrap_or((INSET, 44.0));
        let ((band_y, band_h), moving) = self.band.at(target, self.first);
        self.panel.card.band_timer(moving);
        self.panel.card.rows(rows.len());
        let selected = self.selected.saturating_sub(self.first);
        self.panel.card.draw(scale, |cv| {
            cv.glass(dark);
            if rows.is_empty() {
                cv.text(&empty_msg, &body, TEXT_X, INSET, WIDTH - TEXT_X - EDGE, 44.0, TERTIARY);
                return;
            }
            for (index, ((y, h, clip), (title, detail))) in rows.iter().zip(&texts).enumerate() {
                cv.row(index, |cv| {
                    if index == selected {
                        cv.round(INSET, band_y, WIDTH - 2.0 * INSET, band_h, BAND_RADIUS, glass::white(BAND));
                    }
                    let (y, h) = (*y, *h);
                    let mid = y + h / 2.0;
                    // Age, quiet and right-aligned; the pin sits right before the age text.
                    let age = history::ago(now, clip.time);
                    cv.text(&age, &small_right, WIDTH - EDGE - TIME_W, y, TIME_W, h, TERTIARY);
                    if clip.pinned {
                        let aw = g.width(&age, &small);
                        cv.glyph(glyph::PIN, 12.0, WIDTH - EDGE - aw - 20.0, y, 14.0, h, TERTIARY);
                    }
                    match clip.kind {
                        Kind::Image => {
                            // The picture starts where text starts, at its own shape; only its size is added.
                            let aspect = if clip.h > 0 { clip.w as f32 / clip.h as f32 } else { 1.0 };
                            let (tw, th) = if aspect >= THUMB_W / THUMB_H { (THUMB_W, THUMB_W / aspect) } else { (THUMB_H * aspect, THUMB_H) };
                            let (tw, th) = (tw.max(12.0), th.max(12.0));
                            if let Some(name) = &clip.image {
                                let source = format!("thumb:{}", dir.join(name).display());
                                cv.picture(&format!("thumb:{name}"), &source, TEXT_X, mid - th / 2.0, tw, th, 8.0);
                            }
                            let x = TEXT_X + tw + 14.0;
                            cv.text(detail, &small, x, y, (text_right - x).max(1.0), h, TERTIARY);
                        }
                        Kind::File => {
                            let name = g.fit(title, &body, 260.0, Trim::Middle);
                            let nw = g.width(&name, &body).min(264.0);
                            cv.text(&name, &body, TEXT_X, y, nw + 4.0, h, TEXT);
                            let dx = TEXT_X + nw + 12.0;
                            let folder = g.fit(detail, &small, (text_right - dx).max(0.0), Trim::Head);
                            cv.text(&folder, &small, dx, y, (text_right - dx).max(1.0), h, SECONDARY);
                        }
                        kind => {
                            let mut x = TEXT_X;
                            if kind == Kind::Color {
                                // The swatch is the content, so it stays: in the text column, the code after it.
                                if let Some((r, gr, b)) = livae_core::clip::color_of(clip.text.trim()) {
                                    cv.round(TEXT_X, mid - 9.0, 18.0, 18.0, 9.0, glass::Rgba(r as f32, gr as f32, b as f32, 1.0));
                                }
                                x += 28.0;
                            }
                            let fmt = if kind == Kind::Code { &code } else { &body };
                            let w = text_right - x;
                            let (l1, l2) = if h > 44.0 && detail.is_empty() { g.two_lines(title, fmt, w) } else { (title.clone(), detail.clone()) };
                            if h > 44.0 && !l2.is_empty() {
                                cv.text(&l1, fmt, x, mid - 21.0, w, 21.0, TEXT);
                                cv.text(&l2, fmt, x, mid, w, 21.0, TEXT);
                            } else {
                                cv.text(&l1, fmt, x, y, w, h, TEXT);
                            }
                        }
                    }
                });
            }
        });
    }

    fn slot_at(&self, lparam: LPARAM) -> Option<usize> {
        let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / self.panel.scale;
        let slot = self.slots.iter().position(|&(top, h)| y >= top && y < top + h)?;
        let i = self.first + slot;
        (i < self.shown.len()).then_some(i)
    }

    fn key(&mut self, vk: u16) -> bool {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        let alt = unsafe { GetKeyState(VK_MENU.0 as i32) } < 0;
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        let cmd = ctrl && !alt;
        let vk = VIRTUAL_KEY(vk);
        match vk {
            VK_ESCAPE => self.hide(true),
            VK_RETURN => self.paste(self.selected),
            VK_DOWN => self.move_selection(1),
            VK_UP => self.move_selection(-1),
            VK_NEXT => self.move_selection(self.fit_from(self.first) as isize),
            VK_PRIOR => self.move_selection(-(self.fit_from(self.first) as isize)),
            VK_DELETE if cmd => self.delete(),
            VK_LEFT => self.field.left(shift, cmd),
            VK_RIGHT => self.field.right(shift, cmd),
            VK_HOME => self.field.home(shift),
            VK_END => self.field.end(shift),
            VK_BACK => {
                self.field.backspace(cmd);
                self.query_changed();
            }
            VK_DELETE => {
                self.field.delete(false);
                self.query_changed();
            }
            VK_OEM_COMMA if cmd => {
                self.hide(true);
                app::open_settings();
            }
            _ if cmd && vk.0 == b'P' as u16 => self.pin(),
            _ if cmd && vk.0 == b'A' as u16 => self.field.select_all(),
            _ if cmd && vk.0 == b'C' as u16 => {
                if let Some(t) = self.field.selected_text().map(str::to_string) {
                    put_text(&t);
                }
            }
            _ if cmd && vk.0 == b'X' as u16 => {
                if let Some(t) = self.field.cut() {
                    put_text(&t);
                    self.query_changed();
                }
            }
            _ if cmd && vk.0 == b'V' as u16 => {
                if let Some(t) = get_text() {
                    self.field.insert(&t);
                    self.query_changed();
                }
            }
            _ if cmd && (b'1' as u16..=b'9' as u16).contains(&vk.0) => self.paste(self.first + (vk.0 - b'1' as u16) as usize),
            _ => return false,
        }
        if self.panel.visible() {
            self.caret = true;
            self.paint_field();
        }
        true
    }

    fn handle(&mut self, part: isize, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let card = part == glass::CLIP_CARD;
        match msg {
            WM_PAINT | shell::WM_PICTURE => {
                if card { self.paint_card() } else { self.paint_field() }
                Some(LRESULT(0))
            }
            WM_ACTIVATE if !card && (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
                unsafe {
                    let _ = PostMessageW(Some(self.panel.capsule.hwnd), WM_CHECK_FOCUS, WPARAM(0), LPARAM(0));
                }
                None
            }
            WM_CHECK_FOCUS => {
                if self.panel.visible() && !config::offscreen() && unsafe { GetForegroundWindow() } != self.panel.capsule.hwnd {
                    self.hide(false);
                }
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                self.hide(true);
                Some(LRESULT(0))
            }
            WM_KEYDOWN => self.key(wparam.0 as u16).then_some(LRESULT(0)),
            WM_CHAR => {
                let u = wparam.0 as u16;
                let text = if (0xD800..0xDC00).contains(&u) {
                    self.high = Some(u);
                    None
                } else if (0xDC00..0xE000).contains(&u) {
                    self.high.take().and_then(|h| String::from_utf16(&[h, u]).ok())
                } else {
                    char::from_u32(u as u32).map(String::from)
                };
                if let Some(t) = text.filter(|t| !t.chars().any(char::is_control)) {
                    self.field.insert(&t);
                    self.query_changed();
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE if card => {
                let at = glass::cursor();
                if (at.x, at.y) != (self.mouse.x, self.mouse.y) {
                    self.mouse = at;
                    if let Some(i) = self.slot_at(lparam) && i != self.selected {
                        self.selected = i;
                        self.paint_card();
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP if card => {
                if let Some(i) = self.slot_at(lparam) {
                    self.paste(i);
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as isize;
                self.scroll(-delta.signum() * 3);
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

/// Window messages for the clipboard's capsule and card.
pub fn message(part: isize, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    with(|c| c.handle(part, msg, wparam, lparam)).flatten()
}

/// `--dump`: the panel's geometry and every row on screen: number, kind, title, detail, age, pin.
pub fn dump() -> String {
    with(|c| {
        let mut s = String::new();
        let (cap, card) = c.panel.geometry();
        let _ = writeln!(s, "panel clipboard");
        let _ = writeln!(s, "query {:?}", c.field.text());
        let _ = writeln!(s, "scale {}", c.panel.scale);
        let _ = writeln!(s, "capsule {cap}");
        let _ = writeln!(s, "card {card}");
        let _ = writeln!(s, "items {}", c.history.clips.len());
        let _ = writeln!(s, "selected {}", c.selected + 1);
        let now = now();
        for slot in 0..c.slots.len() {
            let Some(clip) = c.shown.get(c.first + slot).and_then(|&i| c.history.clips.get(i)) else { break };
            let (t, d) = c.texts(clip);
            let pin = if clip.pinned { "pinned" } else { "" };
            let _ = writeln!(s, "{}\t{:?}\t{}\t{}\t{}\t{}", slot + 1, clip.kind, t, d, history::ago(now, clip.time), pin);
        }
        s
    })
    .unwrap_or_default()
}
