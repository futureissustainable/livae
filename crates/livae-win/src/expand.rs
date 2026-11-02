//! Text expansion hooks only collect input. Posted work types after the original key reaches the app.

use std::cell::RefCell;
use std::time::Duration;

use livae_core::settings::Settings;
use livae_core::snippets::{self, Expander, Expansion, Snippet};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::{app, clipboard, config};

pub const WM_EXPAND: u32 = WM_APP + 40;

#[derive(Default)]
struct Hooks {
    keyboard: HHOOK,
    mouse: HHOOK,
    expander: Expander,
    front: isize,
    snippets: Vec<Snippet>,
    pending: Vec<(isize, Expansion)>,
}

thread_local! {
    static HOOKS: RefCell<Hooks> = RefCell::new(Hooks::default());
}

/// Settings changes also remove the hooks when expansion is switched off.
pub fn apply(settings: &Settings) {
    stop();
    if !settings.expansion || config::test_mode() {
        return;
    }
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let Ok(keyboard) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(module.into()), 0) else { return };
        let Ok(mouse) = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(module.into()), 0) else {
            let _ = UnhookWindowsHookEx(keyboard);
            return;
        };
        HOOKS.with(|h| {
            *h.borrow_mut() = Hooks { keyboard, mouse, snippets: settings.snippets.clone(), ..Default::default() };
        });
    }
}

pub fn stop() {
    let hooks = HOOKS.with(|h| std::mem::take(&mut *h.borrow_mut()));
    unsafe {
        for hook in [hooks.keyboard, hooks.mouse] {
            if !hook.is_invalid() {
                let _ = UnhookWindowsHookEx(hook);
            }
        }
    }
}

fn held(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(vk.0 as i32) < 0 }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 && matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
            let event = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            if !event.flags.contains(LLKHF_INJECTED) {
                collect(event);
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

fn collect(event: &KBDLLHOOKSTRUCT) {
    unsafe {
        let front = GetForegroundWindow();
        HOOKS.with(|h| {
            let Ok(mut h) = h.try_borrow_mut() else { return };
            if h.front != front.0 as isize {
                h.expander.reset();
                h.front = front.0 as isize;
            }
            if front.is_invalid() || app::is_ours(front) {
                h.expander.reset();
                return;
            }
            let vk = VIRTUAL_KEY(event.vkCode as u16);
            if vk == VK_BACK {
                h.expander.backspace();
                return;
            }
            if matches!(vk, VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN | VK_HOME | VK_END | VK_PRIOR | VK_NEXT | VK_RETURN | VK_TAB | VK_ESCAPE | VK_DELETE) {
                h.expander.reset();
                return;
            }
            let (ctrl, alt) = (held(VK_CONTROL), held(VK_MENU));
            if ctrl != alt || held(VK_LWIN) || held(VK_RWIN) {
                h.expander.reset();
                return;
            }
            let mut state = [0u8; 256];
            for k in [VK_SHIFT, VK_LSHIFT, VK_RSHIFT, VK_CONTROL, VK_LCONTROL, VK_RCONTROL, VK_MENU, VK_LMENU, VK_RMENU] {
                state[k.0 as usize] = if held(k) { 0x80 } else { 0 };
            }
            state[VK_CAPITAL.0 as usize] = (GetKeyState(VK_CAPITAL.0 as i32) & 1) as u8;
            let layout = GetKeyboardLayout(GetWindowThreadProcessId(front, None));
            let mut text = [0u16; 16];
            let n = ToUnicodeEx(event.vkCode, event.scanCode, &state, &mut text, 0x4, Some(layout));
            if n <= 0 {
                return;
            }
            for c in String::from_utf16_lossy(&text[..(n as usize).min(text.len())]).chars() {
                let Hooks { expander, snippets, pending, .. } = &mut *h;
                if let Some(expansion) = expander.typed(c, snippets, clipboard::get_text) {
                    pending.push((front.0 as isize, expansion));
                    let _ = PostMessageW(Some(app::main_hwnd()), WM_EXPAND, WPARAM(0), LPARAM(0));
                }
            }
        });
    }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 && matches!(wparam.0 as u32, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN) {
            HOOKS.with(|h| {
                if let Ok(mut h) = h.try_borrow_mut() {
                    h.expander.reset();
                }
            });
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

pub fn flush() {
    let pending = HOOKS.with(|h| std::mem::take(&mut h.borrow_mut().pending));
    for (front, expansion) in pending {
        if unsafe { GetForegroundWindow() }.0 as isize == front {
            send(&expansion);
        }
    }
}

/// Launcher insertion waits for the previous app to regain focus.
pub fn insert(snippet: Snippet, front: windows::Win32::Foundation::HWND) {
    if config::test_mode() {
        config::log_action(&format!("snippet {}", snippet.trigger));
        return;
    }
    let front = front.0 as isize;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(120));
        app::on_main(move || {
            if unsafe { GetForegroundWindow() }.0 as isize == front {
                let (text, left) = snippets::render(&snippet.text, livae_core::journal::now(), clipboard::get_text);
                send(&Expansion { erase: 0, text, left });
            }
        });
    });
}

fn input(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } } }
}

fn key(inputs: &mut Vec<INPUT>, vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) {
    inputs.push(input(vk, scan, flags));
    inputs.push(input(vk, scan, flags | KEYEVENTF_KEYUP));
}

/// One input batch keeps Unicode units, line breaks and cursor placement in order.
fn send(expansion: &Expansion) {
    if config::test_mode() {
        return;
    }
    let front = unsafe { GetForegroundWindow() };
    if front.is_invalid() || app::is_ours(front) {
        return;
    }
    let modifiers: Vec<_> = [VK_LCONTROL, VK_RCONTROL, VK_LSHIFT, VK_RSHIFT, VK_LMENU, VK_RMENU, VK_LWIN, VK_RWIN].into_iter().filter(|&vk| held(vk)).collect();
    let inputs = batch(expansion, &modifiers);
    if !inputs.is_empty() {
        unsafe {
            SendInput(&inputs, size_of::<INPUT>() as i32);
        }
    }
}

fn modifier(vk: VIRTUAL_KEY) -> KEYBD_EVENT_FLAGS {
    if matches!(vk, VK_RCONTROL | VK_RMENU | VK_LWIN | VK_RWIN) { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) }
}

fn batch(expansion: &Expansion, modifiers: &[VIRTUAL_KEY]) -> Vec<INPUT> {
    let mut inputs = Vec::new();
    // Shift and AltGr must not turn cursor moves or line breaks into shortcuts.
    for &vk in modifiers {
        inputs.push(input(vk, 0, modifier(vk) | KEYEVENTF_KEYUP));
    }
    for _ in 0..expansion.erase {
        key(&mut inputs, VK_BACK, 0, KEYBD_EVENT_FLAGS(0));
    }
    for unit in expansion.text.encode_utf16() {
        match unit {
            10 => key(&mut inputs, VK_RETURN, 0, KEYBD_EVENT_FLAGS(0)),
            9 => key(&mut inputs, VK_TAB, 0, KEYBD_EVENT_FLAGS(0)),
            _ => key(&mut inputs, VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE),
        }
    }
    for _ in 0..expansion.left {
        key(&mut inputs, VK_LEFT, 0, KEYBD_EVENT_FLAGS(0));
    }
    for &vk in modifiers {
        inputs.push(input(vk, 0, modifier(vk)));
    }
    inputs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_batch_erases_types_unicode_and_controls_then_moves_the_cursor() {
        let inputs = batch(&Expansion { erase: 2, text: "é\n\t😀".into(), left: 1 }, &[VK_LSHIFT, VK_RMENU]);
        let events: Vec<_> = inputs
            .iter()
            .map(|event| {
                assert_eq!(event.r#type, INPUT_KEYBOARD);
                let key = unsafe { event.Anonymous.ki };
                (key.wVk, key.wScan, key.dwFlags)
            })
            .collect();
        assert_eq!(events[0], (VK_LSHIFT, 0, KEYEVENTF_KEYUP));
        assert_eq!(events[1], (VK_RMENU, 0, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP));
        let expected =
            [(VK_BACK, 0), (VK_BACK, 0), (VIRTUAL_KEY(0), 0xE9), (VK_RETURN, 0), (VK_TAB, 0), (VIRTUAL_KEY(0), 0xD83D), (VIRTUAL_KEY(0), 0xDE00), (VK_LEFT, 0)];
        assert_eq!(events.len(), expected.len() * 2 + 4);
        for (pair, (vk, scan)) in events[2..events.len() - 2].chunks_exact(2).zip(expected) {
            let flags = if vk.0 == 0 { KEYEVENTF_UNICODE } else { KEYBD_EVENT_FLAGS(0) };
            assert_eq!(pair, [(vk, scan, flags), (vk, scan, flags | KEYEVENTF_KEYUP)]);
        }
        assert_eq!(events[events.len() - 2], (VK_LSHIFT, 0, KEYBD_EVENT_FLAGS(0)));
        assert_eq!(events[events.len() - 1], (VK_RMENU, 0, KEYEVENTF_EXTENDEDKEY));
    }
}
