//! Window placement in physical pixels, measured at the visible DWM frame.

use std::cell::RefCell;
use std::collections::HashMap;

use livae_core::commands::{self, Command};
use livae_core::settings::Platform;
use livae_core::window::{self, Action, Rect};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::BOOL;

use crate::{app, config};

thread_local! {
    static ORIGINALS: RefCell<HashMap<isize, (u32, Rect)>> = RefCell::new(HashMap::new());
}

fn rect(r: RECT) -> Rect {
    Rect::new(r.left as f64, r.top as f64, (r.right - r.left) as f64, (r.bottom - r.top) as f64)
}

fn frame(hwnd: HWND) -> Option<(Rect, Rect)> {
    unsafe {
        let mut outer = RECT::default();
        GetWindowRect(hwnd, &mut outer).ok()?;
        let mut visible = outer;
        let _ = DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, (&mut visible as *mut RECT).cast(), size_of::<RECT>() as u32);
        Some((rect(outer), rect(visible)))
    }
}

unsafe extern "system" fn monitor(mon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    unsafe {
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut info).as_bool() {
            (*(data.0 as *mut Vec<Rect>)).push(rect(info.rcWork));
        }
    }
    BOOL(1)
}

pub fn run(action: Action, target: HWND) {
    if config::test_mode() {
        if let Some(d) = commands::all(Platform::Windows).find(|d| d.command == Command::Window(action)) {
            config::log_action(d.id);
        }
        return;
    }
    unsafe {
        if target.is_invalid()
            || !IsWindow(Some(target)).as_bool()
            || app::is_ours(target)
            || target == GetDesktopWindow()
            || target == GetShellWindow()
            || IsIconic(target).as_bool()
        {
            return;
        }
        let mut class = [0u16; 128];
        let n = GetClassNameW(target, &mut class);
        if matches!(String::from_utf16_lossy(&class[..n as usize]).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") {
            return;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(target, Some(&mut pid));
        let key = target.0 as isize;
        ORIGINALS.with(|cell| {
            let mut saved = cell.borrow_mut();
            saved.retain(|&hwnd, (owner, _)| {
                let hwnd = HWND(hwnd as _);
                let mut current = 0;
                GetWindowThreadProcessId(hwnd, Some(&mut current));
                IsWindow(Some(hwnd)).as_bool() && current == *owner
            });
            let restore = saved.get(&key).map(|(_, r)| *r);
            if action == Action::Restore && restore.is_none() {
                return;
            }
            let mut areas = Vec::new();
            let _ = EnumDisplayMonitors(None, None, Some(monitor), LPARAM((&mut areas as *mut Vec<Rect>) as isize));
            if action != Action::Restore && (areas.is_empty() || (action == Action::NextDisplay && areas.len() < 2)) {
                return;
            }
            let Some((_, original)) = frame(target) else { return };
            if IsZoomed(target).as_bool() {
                let _ = ShowWindow(target, SW_RESTORE);
            }
            let Some((outer, visible)) = frame(target) else { return };
            let next = if action == Action::Restore {
                restore
            } else {
                if action == Action::NextDisplay {
                    window::next_display(visible, &areas)
                } else {
                    window::screen_of(visible, &areas).and_then(|i| window::place(action, areas[i], visible))
                }
            };
            let Some(next) = next else { return };
            let ok = SetWindowPos(
                target,
                None,
                (next.x + outer.x - visible.x).round() as i32,
                (next.y + outer.y - visible.y).round() as i32,
                (next.w + outer.w - visible.w).round() as i32,
                (next.h + outer.h - visible.h).round() as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
            .is_ok();
            if ok {
                if action == Action::Restore {
                    saved.remove(&key);
                } else {
                    saved.entry(key).or_insert((pid, original));
                }
            }
        });
    }
}
