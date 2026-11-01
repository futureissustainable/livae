//! Drawing and the glass windows.
//!
//! A panel is two popups, a search capsule and a results card 10 px below it, each with the Windows 11 acrylic
//! backdrop (DWMSBT_TRANSIENTWINDOW) under a black gradient drawn with Direct2D, so it reads OLED black over any
//! wallpaper. Two windows, not one with a cut-out: a window region switches the system backdrop off, and DWM ignores
//! the backdrop on frameless popups, so each window keeps a thick frame that WM_NCCALCSIZE hides. Corners are DWM's
//! round corners. Windows draws acrylic only on a window it considers active, so the card (which never takes the
//! focus) is told it is active through WM_NCACTIVATE.
//!
//! Sizes are DIPs; each window's render target runs at the monitor's DPI.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_BITMAP_BRUSH_PROPERTIES, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAMMA_2_2,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES, D2D1_DASH_STYLE_CUSTOM, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_TYPE_SOFTWARE,
    ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFactory5, IDWriteFontCollection, IDWriteFontCollection1, IDWriteTextFormat, IDWriteTextLayout,
    DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_HIT_TEST_METRICS, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
    DWRITE_WORD_WRAPPING_WRAP,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAK,
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMSBT_TRANSIENTWINDOW, DWMWA_BORDER_COLOR, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWCP_ROUND,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BitBlt, ClientToScreen, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW, MonitorFromPoint, ReleaseDC,
    SelectObject, ValidateRect, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, MONITORINFO, MONITOR_DEFAULTTONEAREST, SRCCOPY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Matrix3x2, Vector2};

use crate::shell::{self, Pixels};
use livae_core::tokens;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub f32, pub f32, pub f32, pub f32);

const fn token(c: [u8; 3]) -> Rgba {
    Rgba(c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0)
}

pub const TEXT: Rgba = token(tokens::color::dark::TEXT);
pub const SECONDARY: Rgba = token(tokens::color::dark::TEXT_SECONDARY);
pub const TERTIARY: Rgba = token(tokens::color::dark::TEXT_TERTIARY);

pub fn white(a: f32) -> Rgba {
    Rgba(1.0, 1.0, 1.0, a)
}

fn color(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.0, g: c.1, b: c.2, a: c.3 }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

/// Window parts, kept in GWLP_USERDATA so one window procedure can route each message to its owner.
pub const LAUNCHER_FIELD: isize = 1;
pub const LAUNCHER_CARD: isize = 2;
pub const CLIP_FIELD: isize = 3;
pub const CLIP_CARD: isize = 4;
pub const HUD: isize = 5;
pub const BREAK: isize = 6;
pub const JOURNAL: isize = 7;
pub const JOURNAL_VEIL: isize = 8;

/// Space between the capsule and the card.
pub const GAP: f32 = 10.0;
/// Selection band: a surface change, nothing else (white 12%, as on the Mac).
pub const BAND: f32 = 0.12;
/// Band corners: concentric with DWM's 8 px window corners at an 8 px inset (never below 4).
pub const BAND_RADIUS: f32 = 4.0;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Font {
    Sans,
    /// The large-size cut of the system sans (the break quote).
    Display,
    Mono,
    Icons,
}

/// Segoe Fluent Icons code points (Windows 11; Segoe MDL2 Assets on Windows 10 has the same ones).
pub mod glyph {
    pub const SEARCH: char = '\u{E721}';
    pub const CLIPBOARD: char = '\u{E77F}';
    pub const APPS: char = '\u{E71D}';
    pub const FOLDER: char = '\u{E8B7}';
    pub const PIN: char = '\u{E718}';
    pub const WARNING: char = '\u{E7BA}';
}

pub enum Trim {
    Head,
    Middle,
}

/// Direct2D and DirectWrite factories and the text formats, one set for the UI thread.
pub struct Gfx {
    pub d2d: ID2D1Factory,
    pub dw: IDWriteFactory,
    families: HashMap<Font, Vec<u16>>,
    /// Livae Sans, loaded from the exe (never installed); the sans and display text use it when present.
    livae: Option<IDWriteFontCollection>,
    formats: RefCell<HashMap<(Font, u32, i32, bool, bool), IDWriteTextFormat>>,
}

thread_local! {
    static GFX: &'static Gfx = Box::leak(Box::new(Gfx::new()));
}

pub fn gfx() -> &'static Gfx {
    GFX.with(|g| *g)
}

/// Livae Sans (SIL Open Font License, assets/fonts): Regular, Medium, SemiBold and Bold, built into the exe.
pub const LIVAE_SANS: [&[u8]; 4] = [
    include_bytes!("../../../assets/fonts/LivaeSans-Regular.ttf"),
    include_bytes!("../../../assets/fonts/LivaeSans-Medium.ttf"),
    include_bytes!("../../../assets/fonts/LivaeSans-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/LivaeSans-Bold.ttf"),
];

/// The four Livae Sans faces as a private DirectWrite collection read from memory (Windows 10 1703 and later).
fn livae_collection(dw: &IDWriteFactory) -> Option<IDWriteFontCollection> {
    unsafe {
        let f5: IDWriteFactory5 = dw.cast().ok()?;
        let loader = f5.CreateInMemoryFontFileLoader().ok()?;
        f5.RegisterFontFileLoader(&loader).ok()?;
        let builder = f5.CreateFontSetBuilder().ok()?;
        for bytes in LIVAE_SANS {
            let file = loader.CreateInMemoryFontFileReference(&f5, bytes.as_ptr() as *const std::ffi::c_void, bytes.len() as u32, None).ok()?;
            builder.AddFontFile(&file).ok()?;
        }
        let set = builder.CreateFontSet().ok()?;
        let all: IDWriteFontCollection1 = f5.CreateFontCollectionFromFontSet(&set).ok()?;
        all.cast().ok()
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

impl Gfx {
    fn new() -> Self {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).expect("Direct2D");
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).expect("DirectWrite");
            let mut fonts: Option<IDWriteFontCollection> = None;
            let _ = dw.GetSystemFontCollection(&mut fonts, false);
            // The first installed family of each list (Windows 11 names first, Windows 10 after).
            let pick = |names: &[&str]| -> Vec<u16> {
                for name in names {
                    let w = wide(name);
                    let (mut index, mut exists) = (0u32, windows::core::BOOL(0));
                    if let Some(f) = &fonts {
                        if f.FindFamilyName(PCWSTR(w.as_ptr()), &mut index, &mut exists).is_ok() && exists.as_bool() {
                            return w;
                        }
                    }
                }
                wide(names[names.len() - 1])
            };
            let livae = livae_collection(&dw);
            let mut families = HashMap::new();
            if livae.is_some() {
                families.insert(Font::Sans, wide("Livae Sans"));
                families.insert(Font::Display, wide("Livae Sans"));
            } else {
                families.insert(Font::Sans, pick(&["Segoe UI Variable Text", "Segoe UI"]));
                families.insert(Font::Display, pick(&["Segoe UI Variable Display", "Segoe UI"]));
            }
            families.insert(Font::Mono, pick(&["Cascadia Mono", "Consolas"]));
            families.insert(Font::Icons, pick(&["Segoe Fluent Icons", "Segoe MDL2 Assets"]));
            Self { d2d, dw, families, livae, formats: RefCell::new(HashMap::new()) }
        }
    }

    /// One line, vertically centred, ending in "…" when it does not fit; `wrap` allows more lines instead.
    pub fn format(&self, font: Font, size: f32, weight: DWRITE_FONT_WEIGHT, wrap: bool, right: bool) -> IDWriteTextFormat {
        let key = (font, (size * 10.0) as u32, weight.0, wrap, right);
        if let Some(f) = self.formats.borrow().get(&key) {
            return f.clone();
        }
        let f = unsafe {
            let family = PCWSTR(self.families[&font].as_ptr());
            let (style, stretch) = (DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL);
            let f = match (&self.livae, font) {
                (Some(livae), Font::Sans | Font::Display) => self.dw.CreateTextFormat(family, livae, weight, style, stretch, size, w!("en-us")),
                _ => self.dw.CreateTextFormat(family, None, weight, style, stretch, size, w!("en-us")),
            }
            .expect("text format");
            let _ = f.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP });
            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            if right {
                let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
            }
            if let Ok(sign) = self.dw.CreateEllipsisTrimmingSign(&f) {
                let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                let _ = f.SetTrimming(&trim, &sign);
            }
            f
        };
        self.formats.borrow_mut().insert(key, f.clone());
        f
    }

    pub fn layout(&self, text: &str, fmt: &IDWriteTextFormat, w: f32, h: f32) -> Option<IDWriteTextLayout> {
        let t: Vec<u16> = text.encode_utf16().collect();
        unsafe { self.dw.CreateTextLayout(&t, fmt, w.max(1.0), h.max(1.0)).ok() }
    }

    pub fn width(&self, text: &str, fmt: &IDWriteTextFormat) -> f32 {
        let Some(l) = self.layout(text, fmt, 100_000.0, 100.0) else { return 0.0 };
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe { l.GetMetrics(&mut m) }.map_or(0.0, |_| m.widthIncludingTrailingWhitespace)
    }

    /// One long line as two that fit `w`: the first breaks at a space when it can; the second ends in "…" when
    /// drawn (DirectWrite's ellipsis only cuts a line's width, never a paragraph's height).
    pub fn two_lines(&self, text: &str, fmt: &IDWriteTextFormat, w: f32) -> (String, String) {
        if self.width(text, fmt) <= w {
            return (text.to_string(), String::new());
        }
        let chars: Vec<char> = text.chars().collect();
        let (mut lo, mut hi) = (1usize, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.width(&chars[..mid].iter().collect::<String>(), fmt) <= w { lo = mid } else { hi = mid - 1 }
        }
        let cut = match chars[..lo].iter().rposition(|c| c.is_whitespace()) {
            Some(sp) if sp > lo / 2 => sp + 1,
            _ => lo,
        };
        let first: String = chars[..cut].iter().collect();
        let rest: String = chars[cut..].iter().collect();
        (first.trim_end().to_string(), rest.trim_start().to_string())
    }

    /// Shorten `text` to `max_w`, cutting from the start (folders: the end is what tells them apart) or the
    /// middle (file names: start and extension both matter). Tail cuts are DirectWrite's ellipsis.
    pub fn fit(&self, text: &str, fmt: &IDWriteTextFormat, max_w: f32, trim: Trim) -> String {
        if self.width(text, fmt) <= max_w {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        let make = |k: usize| -> String {
            match trim {
                Trim::Head => std::iter::once('…').chain(chars[chars.len() - k..].iter().copied()).collect(),
                _ => {
                    let front = k.div_ceil(2);
                    let back = k / 2;
                    chars[..front].iter().copied().chain(Some('…')).chain(chars[chars.len() - back..].iter().copied()).collect()
                }
            }
        };
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.width(&make(mid), fmt) <= max_w { lo = mid } else { hi = mid - 1 }
        }
        make(lo)
    }
}

/// One glass window and its Direct2D target; bitmaps (icons, thumbnails) belong to the target.
pub struct Surface {
    pub hwnd: HWND,
    /// Hidden from the screen by DWM until its first frame is drawn, so Windows never shows the empty window
    /// (grey fill and a white frame) that appears for a moment between showing and painting.
    cloaked: bool,
    target: Option<ID2D1HwndRenderTarget>,
    bitmaps: HashMap<String, Option<ID2D1Bitmap>>,
    /// This window's own blurred screen copy, in place of the panels' shared one (breaks, the journal).
    pub backdrop: Option<Rc<Backdrop>>,
    /// Drawn on the CPU and handed over with GDI, so native child controls and the drawing share one surface.
    software: bool,
}

impl Surface {
    /// `takes_focus` is false for the card and the toast: they never become the active window.
    pub fn new(part: isize, owner: Option<HWND>, takes_focus: bool) -> Self {
        unsafe {
            let instance = GetModuleHandleW(None).unwrap_or_default();
            let mut ex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
            if !takes_focus || crate::config::offscreen() {
                ex |= WS_EX_NOACTIVATE;
            }
            let hwnd = CreateWindowExW(ex, w!("LivaePanel"), w!("Livae"), WS_POPUP | WS_THICKFRAME, 0, 0, 100, 100, owner, None, Some(instance.into()), None)
                .unwrap_or_default();
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, part);
            let set = |attr, value: u32| {
                let _ = DwmSetWindowAttribute(hwnd, attr, &value as *const u32 as _, 4);
            };
            set(DWMWA_USE_IMMERSIVE_DARK_MODE, 1);
            set(DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_TRANSIENTWINDOW.0 as u32);
            set(DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND.0 as u32);
            // No light frame line around the glass (DWMWA_COLOR_NONE).
            set(DWMWA_BORDER_COLOR, 0xFFFF_FFFE);
            let _ = DwmExtendFrameIntoClientArea(hwnd, &MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 });
            // DWM applies the attributes on the next frame change.
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
            Self { hwnd, cloaked: false, target: None, bitmaps: HashMap::new(), backdrop: None, software: false }
        }
    }

    /// For a window that covers a whole monitor: square corners and no frame (its shadow would fall on the next
    /// monitor). Its drawing is opaque, so it needs no system backdrop either.
    pub fn full_screen(&self) {
        let value = DWMWCP_DONOTROUND.0 as u32;
        unsafe {
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &value as *const u32 as _, 4);
            let style = GetWindowLongPtrW(self.hwnd, GWL_STYLE);
            SetWindowLongPtrW(self.hwnd, GWL_STYLE, style & !(WS_THICKFRAME.0 as isize));
            let _ = SetWindowPos(self.hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        }
    }

    /// For a window with native child controls: GDI draws them opaque only outside DWM's glass, so the window
    /// gives up the glass frame (its own drawing is opaque anyway) and keeps its children out of its drawing. It
    /// draws on the CPU: a GPU target presents through its own DWM surface, which leaves a hole where a child is.
    pub fn host_controls(&mut self) {
        self.software = true;
        self.target = None;
        unsafe {
            let _ = DwmExtendFrameIntoClientArea(self.hwnd, &MARGINS::default());
            let style = GetWindowLongPtrW(self.hwnd, GWL_STYLE);
            SetWindowLongPtrW(self.hwnd, GWL_STYLE, style | WS_CLIPCHILDREN.0 as isize);
        }
    }

    pub fn visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    /// Call right before showing a hidden window; `draw` lifts the cloak once the first frame is on it.
    pub fn cloak_until_drawn(&mut self) {
        if !self.visible() {
            self.set_cloak(true);
        }
    }

    fn set_cloak(&mut self, on: bool) {
        let value: i32 = on.into();
        unsafe {
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_CLOAK, &value as *const i32 as _, 4);
        }
        self.cloaked = on;
    }

    /// Tell DWM to draw this window as active, so its acrylic stays glass instead of a flat fill.
    pub fn keep_lit(&self) {
        unsafe {
            SendMessageW(self.hwnd, WM_NCACTIVATE, Some(WPARAM(1)), Some(LPARAM(0)));
        }
    }

    pub fn draw(&mut self, scale: f32, f: impl FnOnce(&mut Canvas)) {
        if !self.visible() {
            return;
        }
        unsafe {
            let mut r = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut r);
            let size = D2D_SIZE_U { width: (r.right - r.left).max(1) as u32, height: (r.bottom - r.top).max(1) as u32 };
            if self.target.is_none() {
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: if self.software { D2D1_RENDER_TARGET_TYPE_SOFTWARE } else { D2D1_RENDER_TARGET_TYPE_DEFAULT },
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    ..Default::default()
                };
                let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES { hwnd: self.hwnd, pixelSize: size, presentOptions: D2D1_PRESENT_OPTIONS_NONE };
                self.target = gfx().d2d.CreateHwndRenderTarget(&props, &hprops).ok();
                self.bitmaps.clear();
            }
            let Some(t) = self.target.clone() else { return };
            let now = t.GetPixelSize();
            if now.width != size.width || now.height != size.height {
                let _ = t.Resize(&size);
            }
            t.SetDpi(96.0 * scale, 96.0 * scale);
            t.BeginDraw();
            // Transparent clear lets the acrylic through; the owner paints the black gradient and the content.
            t.Clear(Some(&color(Rgba(0.0, 0.0, 0.0, 0.0))));
            let own = self.backdrop.clone();
            let mut canvas = Canvas { t: &t, w: size.width as f32 / scale, h: size.height as f32 / scale, scale, hwnd: self.hwnd, bitmaps: &mut self.bitmaps, own };
            f(&mut canvas);
            if let Err(e) = t.EndDraw(None, None) {
                if e.code() == D2DERR_RECREATE_TARGET {
                    // Device lost (driver update, GPU reset): rebuilt on the next paint.
                    self.target = None;
                    self.bitmaps.clear();
                    let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(self.hwnd), None, false);
                    return;
                }
            }
            let _ = ValidateRect(Some(self.hwnd), None);
        }
        if self.cloaked {
            self.set_cloak(false);
        }
    }
}

pub struct Canvas<'a> {
    pub t: &'a ID2D1HwndRenderTarget,
    pub w: f32,
    pub h: f32,
    pub scale: f32,
    hwnd: HWND,
    bitmaps: &'a mut HashMap<String, Option<ID2D1Bitmap>>,
    own: Option<Rc<Backdrop>>,
}

impl Canvas<'_> {
    fn brush(&self, c: Rgba) -> Option<ID2D1SolidColorBrush> {
        unsafe { self.t.CreateSolidColorBrush(&color(c), None).ok() }
    }

    /// The panel's own blur of what is behind it (when it has one), then the black gradient: 0 clear … 1 black
    /// at the top fading to a dark veil at the bottom (the Mac's 92% to 45%).
    pub fn glass(&mut self, darkness: f32) {
        self.backdrop();
        let d = darkness.clamp(0.0, 1.0);
        unsafe {
            let stops = [
                D2D1_GRADIENT_STOP { position: 0.0, color: color(Rgba(0.0, 0.0, 0.0, 0.92 * d)) },
                D2D1_GRADIENT_STOP { position: 1.0, color: color(Rgba(0.0, 0.0, 0.0, 0.45 * d)) },
            ];
            let Ok(coll) = self.t.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2::new(0.0, 0.0), endPoint: Vector2::new(0.0, self.h) };
            if let Ok(b) = self.t.CreateLinearGradientBrush(&props, None, &coll) {
                self.t.FillRectangle(&rect(0.0, 0.0, self.w, self.h), &b);
            }
        }
    }

    pub fn fill(&self, x: f32, y: f32, w: f32, h: f32, c: Rgba) {
        if let Some(b) = self.brush(c) {
            unsafe { self.t.FillRectangle(&rect(x, y, w, h), &b) };
        }
    }

    pub fn round(&self, x: f32, y: f32, w: f32, h: f32, radius: f32, c: Rgba) {
        if let Some(b) = self.brush(c) {
            let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: radius, radiusY: radius };
            unsafe { self.t.FillRoundedRectangle(&rr, &b) };
        }
    }

    /// A vertical gradient over (x, y, w, h): (position 0..1 from the top, colour) stops.
    pub fn gradient(&self, x: f32, y: f32, w: f32, h: f32, stops: &[(f32, Rgba)]) {
        let stops: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|&(position, c)| D2D1_GRADIENT_STOP { position, color: color(c) }).collect();
        unsafe {
            let Ok(coll) = self.t.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2::new(0.0, y), endPoint: Vector2::new(0.0, y + h) };
            if let Ok(b) = self.t.CreateLinearGradientBrush(&props, None, &coll) {
                self.t.FillRectangle(&rect(x, y, w, h), &b);
            }
        }
    }

    /// A rounded outline `width` thick inside (x, y, w, h); `dash` gives dash and gap lengths in widths.
    pub fn outline(&self, x: f32, y: f32, w: f32, h: f32, radius: f32, width: f32, c: Rgba, dash: Option<&[f32]>) {
        let Some(b) = self.brush(c) else { return };
        let half = width / 2.0;
        let rr = D2D1_ROUNDED_RECT { rect: rect(x + half, y + half, w - width, h - width), radiusX: (radius - half).max(0.0), radiusY: (radius - half).max(0.0) };
        unsafe {
            let style: Option<ID2D1StrokeStyle> = dash.and_then(|d| {
                let props = D2D1_STROKE_STYLE_PROPERTIES { dashStyle: D2D1_DASH_STYLE_CUSTOM, miterLimit: 10.0, ..Default::default() };
                gfx().d2d.CreateStrokeStyle(&props, Some(d)).ok()
            });
            self.t.DrawRoundedRectangle(&rr, &b, width, style.as_ref());
        }
    }

    pub fn layout(&self, l: &IDWriteTextLayout, x: f32, y: f32, c: Rgba) {
        if let Some(b) = self.brush(c) {
            unsafe { self.t.DrawTextLayout(Vector2::new(x, y), l, &b, D2D1_DRAW_TEXT_OPTIONS_CLIP | D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT) };
        }
    }

    /// Text in a box, vertically centred, cut with "…" at the box's right edge.
    pub fn text(&self, s: &str, fmt: &IDWriteTextFormat, x: f32, y: f32, w: f32, h: f32, c: Rgba) {
        if let Some(l) = gfx().layout(s, fmt, w, h) {
            self.layout(&l, x, y, c);
        }
    }

    pub fn glyph(&self, g: char, size: f32, x: f32, y: f32, w: f32, h: f32, c: Rgba) {
        let fmt = gfx().format(Font::Icons, size, windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL, false, false);
        self.text(&g.to_string(), &fmt, x, y, w, h, c);
    }

    /// An icon or thumbnail at (x, y, w, h) DIPs with rounded corners. `key` names the picture at this size; the
    /// shell worker makes it from `source` in the background and repaints this window when it is ready.
    pub fn picture(&mut self, key: &str, source: &str, x: f32, y: f32, w: f32, h: f32, radius: f32) {
        let px = ((w * self.scale).round() as u32, (h * self.scale).round() as u32);
        let full = format!("{key}@{}x{}", px.0, px.1);
        if !self.bitmaps.contains_key(&full) {
            match shell::picture(&full, source, px, self.hwnd) {
                // Not made yet: drawn when the worker posts back.
                None => return,
                Some(p) => {
                    let b = self.bitmap(&p);
                    if self.bitmaps.len() > 400 {
                        self.bitmaps.clear();
                    }
                    self.bitmaps.insert(full.clone(), b);
                }
            }
        }
        let Some(Some(bmp)) = self.bitmaps.get(&full) else { return };
        unsafe {
            let size = bmp.GetSize();
            if radius <= 0.0 {
                self.t.DrawBitmap(bmp, Some(&rect(x, y, w, h)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                return;
            }
            let props = D2D1_BITMAP_BRUSH_PROPERTIES { extendModeX: D2D1_EXTEND_MODE_CLAMP, extendModeY: D2D1_EXTEND_MODE_CLAMP, interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE_LINEAR };
            if let Ok(brush) = self.t.CreateBitmapBrush(bmp, Some(&props), None) {
                let m = Matrix3x2 { M11: w / size.width.max(1.0), M12: 0.0, M21: 0.0, M22: h / size.height.max(1.0), M31: x, M32: y };
                brush.SetTransform(&m);
                let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: radius, radiusY: radius };
                self.t.FillRoundedRectangle(&rr, &brush);
            }
        }
    }

    /// `p` (an opaque picture of this window's size, such as a screen copy) over the whole window at `opacity`,
    /// made into a bitmap once under `key`.
    pub fn cover(&mut self, key: &str, p: &Pixels, opacity: f32) {
        if !self.bitmaps.contains_key(key) {
            let b = self.bitmap_from(p, 96.0);
            self.bitmaps.insert(key.to_string(), b);
        }
        let Some(Some(bmp)) = self.bitmaps.get(key) else { return };
        unsafe { self.t.DrawBitmap(bmp, Some(&rect(0.0, 0.0, self.w, self.h)), opacity.clamp(0.0, 1.0), D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None) };
    }

    /// This window's part of the blurred backdrop (its own, else the panels' shared one), stretched back up from
    /// its quarter size (smooth by itself).
    pub fn backdrop(&mut self) {
        let Some(b) = self.own.clone() else { return };
        let mut at = POINT::default();
        unsafe {
            let _ = ClientToScreen(self.hwnd, &mut at);
        }
        let key = format!("backdrop{}", b.id);
        if !self.bitmaps.contains_key(&key) {
            self.bitmaps.retain(|k, _| !k.starts_with("backdrop"));
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            let size = D2D_SIZE_U { width: b.w as u32, height: b.h as u32 };
            let bmp = unsafe { self.t.CreateBitmap(size, Some(b.px.as_ptr() as _), b.w as u32 * 4, &props).ok() };
            self.bitmaps.insert(key.clone(), bmp);
        }
        let Some(Some(bmp)) = self.bitmaps.get(&key) else { return };
        let k = BLUR_SCALE as f32;
        let (sx, sy) = ((at.x - b.x) as f32 / k, (at.y - b.y) as f32 / k);
        let src = rect(sx, sy, self.w * self.scale / k, self.h * self.scale / k);
        unsafe { self.t.DrawBitmap(bmp, Some(&rect(0.0, 0.0, self.w, self.h)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, Some(&src)) };
    }

    fn bitmap(&self, p: &Option<std::sync::Arc<Pixels>>) -> Option<ID2D1Bitmap> {
        self.bitmap_from(p.as_ref()?, 96.0 * self.scale)
    }

    fn bitmap_from(&self, p: &Pixels, dpi: f32) -> Option<ID2D1Bitmap> {
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: dpi,
            dpiY: dpi,
        };
        unsafe { self.t.CreateBitmap(D2D_SIZE_U { width: p.w, height: p.h }, Some(p.data.as_ptr() as _), p.w * 4, &props).ok() }
    }

    /// A one-line text field the window's height: placeholder when empty, selection, caret; scrolls to keep the
    /// caret in view. Returns where the caret is (x, top, height in DIPs) for the system caret that IMEs place
    /// their window at.
    pub fn field(&self, f: &livae_core::field::Field, fmt: &IDWriteTextFormat, x: f32, w: f32, placeholder: &str, caret: bool) -> (f32, f32, f32) {
        self.field_at(f, fmt, x, 0.0, w, self.h, placeholder, caret)
    }

    /// `field` in the box (x, y, w, h).
    pub fn field_at(&self, f: &livae_core::field::Field, fmt: &IDWriteTextFormat, x: f32, y: f32, w: f32, h: f32, placeholder: &str, caret: bool) -> (f32, f32, f32) {
        let g = gfx();
        let text = if f.is_empty() { placeholder } else { f.text() };
        let Some(l) = g.layout(text, fmt, 100_000.0, h) else { return (x, y, h) };
        let hit = |i: usize| -> (f32, DWRITE_HIT_TEST_METRICS) {
            let (mut px, mut py, mut m) = (0f32, 0f32, DWRITE_HIT_TEST_METRICS::default());
            let _ = unsafe { l.HitTestTextPosition(i as u32, false, &mut px, &mut py, &mut m) };
            (px, m)
        };
        let (cx, m) = if f.is_empty() { (0.0, hit(0).1) } else { hit(f.utf16(f.caret())) };
        let shift = (cx - (w - 2.0)).max(0.0);
        unsafe { self.t.PushAxisAlignedClip(&rect(x, y, w, h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        if let Some((a, b)) = f.selection() {
            let (xa, _) = hit(f.utf16(a));
            let (xb, _) = hit(f.utf16(b));
            self.fill(x + xa - shift, y + m.top, xb - xa, m.height, white(0.24));
        }
        self.layout(&l, x - shift, y, if f.is_empty() { TERTIARY } else { TEXT });
        if caret && f.selection().is_none() {
            self.fill(x + cx - shift, y + m.top + 1.0, 1.5, (m.height - 2.0).max(8.0), TEXT);
        }
        unsafe { self.t.PopAxisAlignedClip() };
        (x + cx - shift, y + m.top, m.height)
    }
}

/// The panel glass: Windows 11 no longer draws its light blur-behind (tested 01.11.2026, the window came out
/// black), and acrylic lifts black to grey and blurs every shape away. So a panel blurs what is behind it once,
/// as it opens: a GDI copy of the screen, shrunk BLUR_SCALE times, box-blurred three times (close to a Gaussian of
/// BLUR_DIP), dimmed to DIM so white text stays readable over white. Black stays black, as on the Mac.
pub struct Backdrop {
    id: u64,
    /// Top-left of the captured area in screen pixels, and the shrunk size.
    x: i32,
    y: i32,
    w: usize,
    h: usize,
    px: Vec<u8>,
}

const BLUR_SCALE: i32 = 4;
const BLUR_DIP: f32 = 18.0;
pub const DIM: f32 = 0.35;

fn capture_backdrop(x: i32, y: i32, w: i32, h: i32, scale: f32) -> Option<Rc<Backdrop>> {
    screen_copy(x, y, w, h).map(|full| blurred(&full, x, y, scale, DIM))
}

/// `full`, a screen copy taken at (x, y), shrunk BLUR_SCALE times, blurred and dimmed to `dim` (1 keeps it as bright).
pub fn blurred(full: &Pixels, x: i32, y: i32, scale: f32, dim: f32) -> Rc<Backdrop> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let (w, h) = (full.w as usize, full.h as usize);
    let k = BLUR_SCALE as usize;
    let (sw, sh) = (w.div_ceil(k), h.div_ceil(k));
    // Shrink: the average of each k x k block.
    let mut small = vec![[0f32; 3]; sw * sh];
    for sy in 0..sh {
        for sx in 0..sw {
            let (mut acc, mut n) = ([0f32; 3], 0f32);
            for yy in sy * k..((sy + 1) * k).min(h) {
                for xx in sx * k..((sx + 1) * k).min(w) {
                    let i = (yy * w + xx) * 4;
                    for c in 0..3 {
                        acc[c] += full.data[i + c] as f32;
                    }
                    n += 1.0;
                }
            }
            small[sy * sw + sx] = acc.map(|v| v / n.max(1.0));
        }
    }
    // Three box blurs approach a Gaussian; sigma of three passes of radius r is about 1.12 r.
    let r = ((BLUR_DIP * scale / BLUR_SCALE as f32) / 1.12).round().max(1.0) as usize;
    for _ in 0..3 {
        small = box_blur(&small, sw, sh, r, true);
        small = box_blur(&small, sw, sh, r, false);
    }
    let px = small.iter().flat_map(|p| [(p[0] * dim) as u8, (p[1] * dim) as u8, (p[2] * dim) as u8, 255]).collect();
    Rc::new(Backdrop { id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed), x, y, w: sw, h: sh, px })
}

impl Backdrop {
    /// The blur back at `w` x `h` pixels (bilinear), under `wash` black, as BGRA with alpha 255.
    pub fn full_size(&self, w: usize, h: usize, wash: f32) -> Vec<u8> {
        let mut out = vec![0u8; w * h * 4];
        if self.w == 0 || self.h == 0 {
            return out;
        }
        let keep = 1.0 - wash.clamp(0.0, 1.0);
        let (fx, fy) = (self.w as f32 / w.max(1) as f32, self.h as f32 / h.max(1) as f32);
        for y in 0..h {
            let sy = ((y as f32 + 0.5) * fy - 0.5).clamp(0.0, (self.h - 1) as f32);
            let (y0, ty) = (sy as usize, sy.fract());
            let y1 = (y0 + 1).min(self.h - 1);
            for x in 0..w {
                let sx = ((x as f32 + 0.5) * fx - 0.5).clamp(0.0, (self.w - 1) as f32);
                let (x0, tx) = (sx as usize, sx.fract());
                let x1 = (x0 + 1).min(self.w - 1);
                let at = |xx: usize, yy: usize, c: usize| self.px[(yy * self.w + xx) * 4 + c] as f32;
                let o = (y * w + x) * 4;
                for c in 0..3 {
                    let top = at(x0, y0, c) * (1.0 - tx) + at(x1, y0, c) * tx;
                    let bottom = at(x0, y1, c) * (1.0 - tx) + at(x1, y1, c) * tx;
                    out[o + c] = ((top * (1.0 - ty) + bottom * ty) * keep) as u8;
                }
                out[o + 3] = 255;
            }
        }
        out
    }
}

/// One pass of a box blur along rows (or columns), edges clamped.
fn box_blur(src: &[[f32; 3]], w: usize, h: usize, r: usize, rows: bool) -> Vec<[f32; 3]> {
    let (len, lines) = if rows { (w, h) } else { (h, w) };
    let at = |line: usize, i: usize| if rows { line * w + i } else { i * w + line };
    let mut out = vec![[0f32; 3]; src.len()];
    let span = (2 * r + 1) as f32;
    for line in 0..lines {
        let mut acc = [0f32; 3];
        for i in 0..=2 * r {
            let p = src[at(line, i.saturating_sub(r).min(len - 1))];
            for c in 0..3 {
                acc[c] += p[c];
            }
        }
        for i in 0..len {
            out[at(line, i)] = acc.map(|v| v / span);
            let add = src[at(line, (i + r + 1).min(len - 1))];
            let sub = src[at(line, i.saturating_sub(r))];
            for c in 0..3 {
                acc[c] += add[c] - sub[c];
            }
        }
    }
    out
}

/// The screen in a rectangle as an opaque picture (BGRA, alpha 255), or None when the copy fails.
pub fn screen_copy(x: i32, y: i32, w: i32, h: i32) -> Option<Pixels> {
    let mut data = screen_pixels(x, y, w, h)?;
    for a in data.iter_mut().skip(3).step_by(4) {
        *a = 255;
    }
    Some(Pixels { w: w as u32, h: h as u32, data })
}

/// The screen's pixels in a rectangle (BGRA, top row first), or None when the copy fails.
fn screen_pixels(x: i32, y: i32, w: i32, h: i32) -> Option<Vec<u8>> {
    if w <= 0 || h <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let out = match CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(dib) if !bits.is_null() => {
                let old = SelectObject(mem, dib.into());
                let ok = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY).is_ok();
                let px = ok.then(|| std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec());
                SelectObject(mem, old);
                let _ = DeleteObject(dib.into());
                px
            }
            _ => None,
        };
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        out
    }
}

/// A capsule and a card, positioned together. The owner (launcher, clipboard) draws into both.
pub struct Panel {
    pub capsule: Surface,
    pub card: Surface,
    pub width: f32,
    pub field_h: f32,
    pub card_h: f32,
    pub scale: f32,
    pub x: i32,
    pub top: i32,
    /// The window that was in front before the panel opened; focus goes back there.
    pub prev: HWND,
}

impl Panel {
    pub fn new(field_part: isize, card_part: isize, width: f32, field_h: f32) -> Self {
        let capsule = Surface::new(field_part, None, true);
        let card = Surface::new(card_part, Some(capsule.hwnd), false);
        Self { capsule, card, width, field_h, card_h: 0.0, scale: 1.0, x: 0, top: 0, prev: HWND::default() }
    }

    pub fn px(&self, dip: f32) -> i32 {
        (dip * self.scale).round() as i32
    }

    pub fn visible(&self) -> bool {
        self.capsule.visible()
    }

    /// Height of capsule, gap and card in pixels for a card of `card_h` DIPs.
    pub fn total_px(&self, card_h: f32) -> i32 {
        self.px(self.field_h + if card_h > 0.0 { GAP + card_h } else { 0.0 })
    }

    /// Remember the app in front. When one of our windows is in front (the other panel is open), take the app
    /// that panel was opened over.
    pub fn remember_front(&mut self, other_prev: HWND) {
        let fg = unsafe { GetForegroundWindow() };
        if !fg.is_invalid() && !crate::app::is_ours(fg) {
            self.prev = fg;
        } else if !other_prev.is_invalid() {
            self.prev = other_prev;
        }
    }

    /// Place the system caret (never shown) where the drawn caret is, so an IME opens its window there.
    pub fn set_caret(&self, at: (f32, f32, f32)) {
        unsafe {
            if GetFocus() == self.capsule.hwnd {
                let _ = SetCaretPos(self.px(at.0), self.px(at.1));
            }
        }
    }

    /// Put both windows at (x, top) and show them; with `activate`, take the keyboard focus.
    pub fn show(&mut self, activate: bool) {
        let w = self.px(self.width);
        if !self.visible() {
            // Room for the tallest card; captured before our windows are on screen, so they are not in it.
            // Each panel owns its copy: opening one panel hides the other, which must not clear the new one's.
            let b = capture_backdrop(self.x, self.top, w, self.px(self.field_h + GAP + 600.0), self.scale);
            self.capsule.backdrop = b.clone();
            self.card.backdrop = b;
        }
        self.capsule.cloak_until_drawn();
        unsafe {
            let flags = SWP_SHOWWINDOW | if activate { SET_WINDOW_POS_FLAGS(0) } else { SWP_NOACTIVATE };
            let _ = SetWindowPos(self.capsule.hwnd, Some(HWND_TOPMOST), self.x, self.top, w, self.px(self.field_h), flags);
        }
        self.place_card();
        if activate {
            bring_to_front(self.capsule.hwnd);
            // An invisible system caret that follows the drawn one: IMEs open their window at it.
            let _ = unsafe { CreateCaret(self.capsule.hwnd, None, 1, self.px(self.field_h * 0.4)) };
        }
    }

    pub fn set_card(&mut self, card_h: f32) {
        self.card_h = card_h;
        if self.visible() {
            self.place_card();
        }
    }

    fn place_card(&mut self) {
        self.card.cloak_until_drawn();
        unsafe {
            if self.card_h <= 0.0 {
                let _ = ShowWindow(self.card.hwnd, SW_HIDE);
                return;
            }
            let y = self.top + self.px(self.field_h + GAP);
            let _ = SetWindowPos(self.card.hwnd, Some(HWND_TOPMOST), self.x, y, self.px(self.width), self.px(self.card_h), SWP_SHOWWINDOW | SWP_NOACTIVATE);
        }
        self.card.keep_lit();
    }

    /// Hide both; with `restore`, first hand the focus back to the app that had it (while Windows still lets us).
    pub fn hide(&mut self, restore: bool) {
        unsafe {
            if GetFocus() == self.capsule.hwnd {
                let _ = DestroyCaret();
            }
            if restore && self.capsule.hwnd == GetForegroundWindow() && IsWindow(Some(self.prev)).as_bool() {
                let _ = SetForegroundWindow(self.prev);
            }
            let _ = ShowWindow(self.card.hwnd, SW_HIDE);
            let _ = ShowWindow(self.capsule.hwnd, SW_HIDE);
        }
        self.capsule.backdrop = None;
        self.card.backdrop = None;
    }

    /// Window rectangles for `--dump`: "x y w h" in pixels, or "hidden".
    pub fn geometry(&self) -> (String, String) {
        let r = |s: &Surface| {
            if !s.visible() {
                return "hidden".to_string();
            }
            let mut r = RECT::default();
            let _ = unsafe { GetWindowRect(s.hwnd, &mut r) };
            format!("{} {} {} {}", r.left, r.top, r.right - r.left, r.bottom - r.top)
        };
        (r(&self.capsule), r(&self.card))
    }
}

/// Bring our window to the front. Windows refuses SetForegroundWindow to a background app unless it just got
/// input (our hotkey counts); if it still refuses, borrow the front app's input queue for the call.
pub fn bring_to_front(hwnd: HWND) {
    unsafe {
        let _ = SetForegroundWindow(hwnd);
        let fg = GetForegroundWindow();
        if fg != hwnd {
            let theirs = GetWindowThreadProcessId(fg, None);
            let ours = GetCurrentThreadId();
            if theirs != 0 && theirs != ours && AttachThreadInput(ours, theirs, true).as_bool() {
                let _ = BringWindowToTop(hwnd);
                let _ = SetForegroundWindow(hwnd);
                let _ = AttachThreadInput(ours, theirs, false);
            }
        }
        let _ = SetFocus(Some(hwnd));
    }
}

pub struct Monitor {
    pub work: RECT,
    /// The whole monitor, taskbar included.
    pub full: RECT,
    pub scale: f32,
}

/// The work area (screen minus taskbar) and DPI scale of the monitor nearest to `pt`.
pub fn monitor_at(pt: POINT) -> Monitor {
    unsafe {
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let mut work = mi.rcWork;
        if work.right <= work.left || work.bottom <= work.top {
            work = RECT { left: 0, top: 0, right: 1280, bottom: 800 };
        }
        let full = if mi.rcMonitor.right > mi.rcMonitor.left { mi.rcMonitor } else { work };
        Monitor { work, full, scale: (dx.max(48) as f32 / 96.0).clamp(0.5, 5.0) }
    }
}

/// Far outside every monitor, for `--offscreen` check copies.
pub fn offscreen_origin() -> (i32, i32) {
    unsafe { (GetSystemMetrics(SM_XVIRTUALSCREEN) - 8000, GetSystemMetrics(SM_YVIRTUALSCREEN) - 8000) }
}

/// `v` kept within lo..=hi; on a work area narrower than the panel, the left edge wins (never a panic).
pub fn fit(v: i32, lo: i32, hi: i32) -> i32 {
    if hi < lo { lo } else { v.clamp(lo, hi) }
}

pub fn cursor() -> POINT {
    let mut pt = POINT::default();
    let _ = unsafe { GetCursorPos(&mut pt) };
    pt
}

/// The window class every glass window uses.
pub fn register() {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(panel_proc),
            hInstance: instance.into(),
            lpszClassName: w!("LivaePanel"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

extern "system" fn panel_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let part = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        let passive = matches!(part, LAUNCHER_CARD | CLIP_CARD | HUD | JOURNAL_VEIL);
        match msg {
            // The frame stays for DWM's backdrop but takes no room: the client area is the whole window.
            WM_NCCALCSIZE if wparam.0 != 0 => return LRESULT(0),
            WM_NCHITTEST => return LRESULT(HTCLIENT as isize),
            WM_ERASEBKGND => return LRESULT(1),
            // No classic frame painting: with no non-client area it could only draw the white border that
            // flashed around the glass on opening and while scrolling.
            WM_NCPAINT => return LRESULT(0),
            WM_GETMINMAXINFO => {
                // A thick-frame window has a minimum size; ours are sized by us alone.
                let mm = lparam.0 as *mut MINMAXINFO;
                if let Some(mm) = mm.as_mut() {
                    mm.ptMinTrackSize = POINT { x: 1, y: 1 };
                }
                return LRESULT(0);
            }
            // lParam -1: update the active state (DWM keeps the acrylic lit) without repainting the frame.
            WM_NCACTIVATE => return DefWindowProcW(hwnd, msg, if passive { WPARAM(1) } else { wparam }, LPARAM(-1)),
            WM_MOUSEACTIVATE if passive => return LRESULT(MA_NOACTIVATE as isize),
            // We size each window for its monitor when it opens.
            WM_DPICHANGED => return LRESULT(0),
            WM_TIMER if part == HUD => {
                let _ = KillTimer(Some(hwnd), 1);
                let _ = ShowWindow(hwnd, SW_HIDE);
                return LRESULT(0);
            }
            WM_PAINT if part == HUD => {
                hud_paint();
                let _ = ValidateRect(Some(hwnd), None);
                return LRESULT(0);
            }
            _ => {}
        }
        let routed = match part {
            LAUNCHER_FIELD | LAUNCHER_CARD => crate::launcher::message(part, msg, wparam, lparam),
            CLIP_FIELD | CLIP_CARD => crate::clipboard::message(part, msg, wparam, lparam),
            BREAK => crate::breaks::message(hwnd, msg, wparam, lparam),
            JOURNAL | JOURNAL_VEIL => crate::journal::message(hwnd, msg, wparam, lparam),
            _ => None,
        };
        if msg == WM_PAINT {
            let _ = ValidateRect(Some(hwnd), None);
            return LRESULT(0);
        }
        routed.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wparam, lparam))
    }
}

// The toast ("Copied 42"): a small glass capsule that never takes the focus and hides itself.

struct Hud {
    surface: Surface,
    text: String,
    scale: f32,
}

thread_local! {
    static HUD_STATE: RefCell<Option<Hud>> = const { RefCell::new(None) };
}

/// Show `text` centred on `at` for a moment.
pub fn toast(text: &str, at: POINT) {
    if crate::config::offscreen() {
        return;
    }
    HUD_STATE.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else { return };
        let hud = slot.get_or_insert_with(|| Hud { surface: Surface::new(HUD, None, false), text: String::new(), scale: 1.0 });
        let m = monitor_at(at);
        hud.scale = m.scale;
        hud.text = text.to_string();
        let fmt = gfx().format(Font::Sans, 16.0, windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL, false, false);
        let w_dip = gfx().width(text, &fmt) + 48.0;
        let (w, h) = ((w_dip * m.scale).round() as i32, (44.0 * m.scale).round() as i32);
        let x = fit(at.x - w / 2, m.work.left + 8, m.work.right - w - 8);
        let y = fit(at.y - h / 2, m.work.top + 8, m.work.bottom - h - 8);
        unsafe {
            let _ = SetWindowPos(hud.surface.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_SHOWWINDOW | SWP_NOACTIVATE);
            hud.surface.keep_lit();
            let _ = SetTimer(Some(hud.surface.hwnd), 1, 1300, None);
        }
        draw_hud(hud);
    });
}

fn draw_hud(hud: &mut Hud) {
    let text = hud.text.clone();
    hud.surface.draw(hud.scale, |c| {
        c.glass(1.0);
        let fmt = gfx().format(Font::Sans, 16.0, windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL, false, false);
        let tw = gfx().width(&text, &fmt);
        c.text(&text, &fmt, (c.w - tw) / 2.0, 0.0, tw + 2.0, c.h, TEXT);
    });
}

fn hud_paint() {
    HUD_STATE.with(|cell| {
        if let Ok(mut slot) = cell.try_borrow_mut() {
            if let Some(h) = slot.as_mut() {
                draw_hud(h);
            }
        }
    });
}
