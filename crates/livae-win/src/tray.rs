//! The tray icon and its menu: Search, Clipboard, the journal and its status, Pause clipboard history, Breaks (what
//! is next, take one now, skip, pause, resume), Settings, Quit. The icon is the Mac's menu-bar star, drawn at the
//! tray's size in the taskbar's theme colour: dimmed while the history is paused, amber while another app holds one
//! of the shortcuts.

use std::cell::Cell;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP};
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapCacheOnLoad};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::glass::{self, Rgba};

pub const WM_TRAY: u32 = WM_APP + 1;

pub const SEARCH: u32 = 1;
pub const CLIPBOARD: u32 = 2;
pub const PAUSE: u32 = 3;
pub const SETTINGS: u32 = 4;
pub const QUIT: u32 = 5;
pub const JOURNAL: u32 = 6;
pub const BREAK_NOW: u32 = 7;
pub const BREAK_SKIP: u32 = 8;
pub const BREAK_PAUSE_HOUR: u32 = 9;
pub const BREAK_PAUSE_MORNING: u32 = 10;
pub const BREAK_RESUME: u32 = 11;
pub const YEAR: u32 = 12;
pub const EQ_TOGGLE: u32 = 13;
pub const EQ_EDIT: u32 = 14;
pub const EQ_COMPARE: u32 = 15;
pub const EQ_APO: u32 = 16;

thread_local! {
    static ICON: Cell<HICON> = const { Cell::new(HICON(std::ptr::null_mut())) };
    /// What the icon last showed: (paused, shortcut taken, light taskbar).
    static SHOWN: Cell<Option<(bool, bool, bool)>> = const { Cell::new(None) };
}

fn light_taskbar() -> bool {
    let mut v = 0u32;
    let mut size = 4u32;
    let key = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let ok = unsafe { RegGetValueW(HKEY_CURRENT_USER, key, w!("SystemUsesLightTheme"), RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as _), Some(&mut size)) };
    ok.is_ok() && v == 1
}

fn data(hwnd: HWND, with_icon: bool) -> NOTIFYICONDATAW {
    let paused = crate::clipboard::paused();
    let taken = crate::hotkeys::taken();
    let mut nid = NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: 1, uFlags: NIF_MESSAGE | NIF_TIP, uCallbackMessage: WM_TRAY, ..Default::default() };
    let tip = if taken {
        "Livae: another app holds one of your shortcuts; retrying"
    } else if paused {
        "Livae: clipboard history paused"
    } else {
        "Livae"
    };
    for (d, s) in nid.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
        *d = s;
    }
    if with_icon {
        let light = light_taskbar();
        let state = (paused, taken, light);
        if SHOWN.with(|s| s.get()) != Some(state) || ICON.with(|i| i.get()).is_invalid() {
            let ink = if taken {
                glass::Rgba(1.0, 0.68, 0.32, 1.0) // status warning #ffad52
            } else if light {
                Rgba(0.08, 0.09, 0.11, 1.0)
            } else {
                Rgba(1.0, 1.0, 1.0, 1.0)
            };
            if let Some(icon) = mark(Rgba(ink.0, ink.1, ink.2, if paused { 0.45 } else { 1.0 })) {
                let old = ICON.with(|i| i.replace(icon));
                if !old.is_invalid() {
                    let _ = unsafe { DestroyIcon(old) };
                }
                SHOWN.with(|s| s.set(Some(state)));
            }
        }
        nid.hIcon = ICON.with(|i| i.get());
        nid.uFlags |= NIF_ICON;
    }
    nid
}

pub fn add(hwnd: HWND) {
    let nid = data(hwnd, true);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

/// Refresh the icon and tooltip (pause or a taken shortcut changed, or the taskbar theme).
pub fn update(hwnd: HWND) {
    let nid = data(hwnd, true);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

pub fn remove(hwnd: HWND) {
    let nid = data(hwnd, false);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

/// Show the menu at the pointer; returns the chosen command (0 for none).
pub fn menu(hwnd: HWND) -> u32 {
    let s = crate::config::get();
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return 0 };
        let add = |flags: MENU_ITEM_FLAGS, id: u32, text: &str| {
            let _ = AppendMenuW(m, flags, id as usize, &HSTRING::from(text));
        };
        // The text after a tab shows right-aligned, where menus show shortcuts.
        add(MF_STRING, SEARCH, &format!("Search\t{}", s.launcher));
        add(MF_STRING, CLIPBOARD, &format!("Clipboard\t{}", s.clipboard));
        add(MF_STRING | if crate::clipboard::paused() { MF_CHECKED } else { MF_UNCHECKED }, PAUSE, "Pause clipboard history");
        if crate::hotkeys::taken() {
            add(MF_STRING | MF_GRAYED, 0, "A shortcut is in use by another app; retrying");
        }
        // The same four groups as the Mac menu: open, today, what runs, the app.
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
        add(MF_STRING, JOURNAL, "Journal\u{2026}");
        add(MF_STRING | MF_GRAYED, 0, &crate::journal::status_line());
        add(MF_STRING, YEAR, "Year View\u{2026}");
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
        // Breaks: what is next, and the few things one does with it.
        if let Ok(breaks) = CreatePopupMenu() {
            let sub = |flags: MENU_ITEM_FLAGS, id: u32, text: &str| {
                let _ = AppendMenuW(breaks, flags, id as usize, &HSTRING::from(text));
            };
            sub(MF_STRING | MF_GRAYED, 0, &crate::breaks::status_line());
            let _ = AppendMenuW(breaks, MF_SEPARATOR, 0, PCWSTR::null());
            sub(MF_STRING, BREAK_NOW, "Take a break now");
            sub(MF_STRING, BREAK_SKIP, "Skip to next break");
            sub(MF_STRING, BREAK_PAUSE_HOUR, "Pause for 1 hour");
            sub(MF_STRING, BREAK_PAUSE_MORNING, "Pause until tomorrow");
            sub(MF_STRING, BREAK_RESUME, "Resume");
            let _ = AppendMenuW(m, MF_POPUP, breaks.0 as usize, w!("Breaks"));
        }
        // The equaliser: what it is doing, on or off, and where its profiles live (settings.toml, [sound]).
        if let Ok(eq) = CreatePopupMenu() {
            let sub = |flags: MENU_ITEM_FLAGS, id: u32, text: &str| {
                let _ = AppendMenuW(eq, flags, id as usize, &HSTRING::from(text));
            };
            sub(MF_STRING | MF_GRAYED, 0, &crate::sound::status());
            if crate::sound::needs_apo() {
                sub(MF_STRING, EQ_APO, "Install Equalizer APO on this output\u{2026}");
            }
            let _ = AppendMenuW(eq, MF_SEPARATOR, 0, PCWSTR::null());
            sub(MF_STRING, EQ_TOGGLE, if s.sound.eq { "Turn off" } else { "Turn on" });
            sub(MF_STRING | if s.sound.bypass { MF_CHECKED } else { MF_UNCHECKED } | if s.sound.eq { MF_ENABLED } else { MF_GRAYED }, EQ_COMPARE, "Filters off (compare)");
            sub(MF_STRING, EQ_EDIT, "Edit profiles\u{2026}");
            let _ = AppendMenuW(m, MF_POPUP, eq.0 as usize, w!("Equaliser"));
        }
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
        add(MF_STRING, SETTINGS, "Settings");
        add(MF_STRING, QUIT, "Quit Livae");
        // A tray menu needs its window in front, or it never closes when you click elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let pt = glass::cursor();
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, None, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(m);
        cmd.0 as u32
    }
}

/// Livae's mark in `ink` at the system's small-icon size: a 3 x 3 grid of days with the middle day lit, the other days faint.
fn mark(ink: Rgba) -> Option<HICON> {
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON).max(16) as u32;
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let bmp = wic.CreateBitmap(size, size, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad).ok()?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            ..Default::default()
        };
        let d2d = &glass::gfx().d2d;
        let rt = d2d.CreateWicBitmapRenderTarget(&bmp, &props).ok()?;
        rt.BeginDraw();
        rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        let s = size as f32;
        let (cell, gap) = (s * 0.26, s * 0.08);
        let o = (s - (3.0 * cell + 2.0 * gap)) / 2.0;
        for r in 0..3 {
            for c in 0..3 {
                let lit = r == 1 && c == 1;
                let brush = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: if lit { 1.0 } else { 0.35 } }, None).ok()?;
                let (x, y) = (o + c as f32 * (cell + gap), o + r as f32 * (cell + gap));
                let rect = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: x, top: y, right: x + cell, bottom: y + cell }, radiusX: cell * 0.2, radiusY: cell * 0.2 };
                rt.FillRoundedRectangle(&rect, &brush);
            }
        }
        rt.EndDraw(None, None).ok()?;
        let mut px = vec![0u8; (size * size * 4) as usize];
        bmp.CopyPixels(std::ptr::null(), size * 4, &mut px).ok()?;
        // Coverage becomes alpha; the colour is the ink (icons take straight alpha).
        for p in px.as_chunks_mut::<4>().0 {
            let a = p[3] as f32 / 255.0 * ink.3;
            p[0] = (ink.2 * 255.0) as u8;
            p[1] = (ink.1 * 255.0) as u8;
            p[2] = (ink.0 * 255.0) as u8;
            p[3] = (a * 255.0).round() as u8;
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: -(size as i32), // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color: HBITMAP = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u8, px.len());
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}
