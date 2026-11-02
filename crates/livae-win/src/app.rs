//! App start: one instance per session, the hidden main window (tray icon, clipboard listener, timers, lock, sleep
//! and display power for the breaks, work handed over from other threads), the global shortcuts, the test flags and
//! the message loop.
//!
//! Flags: `--show` opens the launcher, `--clipboard` the clipboard, `--query <text>` fills the open panel's field;
//! sent to an already running Livae they open its panel. `--break mini|long` shows a break at once, `--journal
//! [file]` the journal on `journal-test.md` in the data folder (seeded from `file`). `--test` makes a throwaway copy
//! (see config.rs), `--offscreen` keeps its windows off every screen, `--dump <file>` writes what the open panel
//! shows and quits. `--install-startup` / `--remove-startup` add or remove the Startup-folder shortcut; `--quit`
//! closes Livae. `--year` opens the year board; `--streak` chooses its habit tab and `--hover YYYY-MM-DD` its card.

use std::io::Write;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use windows::core::w;
use windows::Win32::Foundation::{GetLastError, SetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WIN32_ERROR, WPARAM};
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::System::Console::{AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE};
use windows::Win32::System::DataExchange::{AddClipboardFormatListener, RemoveClipboardFormatListener};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{RegisterPowerSettingNotification, UnregisterPowerSettingNotification, HPOWERNOTIFY, POWERBROADCAST_SETTING};
use windows::Win32::System::RemoteDesktop::{WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION};
use windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::{breaks, clipboard, config, expand, gate, glass, hotkeys, journal, launcher, shell, sound, tray, year};

const WM_RUN: u32 = WM_APP + 2;
const WM_HOTKEY_PRESSED: u32 = WM_APP + 3;
/// Sent by a second start: wparam 1 = launcher, 2 = clipboard, 3 = year.
const WM_FORWARD: u32 = WM_APP + 4;

const T_RETRY: usize = 1;
const T_PASTE: usize = 2;
const T_INDEX: usize = 3;
const T_DUMP: usize = 4;
const T_BLINK: usize = 5;
const T_BREAKS: usize = 6;
const T_JOURNAL: usize = 7;

static MAIN: AtomicIsize = AtomicIsize::new(0);
/// The display on/off notification, unregistered when Livae quits.
static DISPLAY_NOTIFY: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

pub fn main_hwnd() -> HWND {
    HWND(MAIN.load(Ordering::SeqCst) as _)
}

/// True for any window of this process (our panels, the tray menu).
pub fn is_ours(hwnd: HWND) -> bool {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid == GetCurrentProcessId()
    }
}

type Job = Box<dyn FnOnce() + Send>;

fn queue() -> &'static Mutex<Vec<Job>> {
    static Q: OnceLock<Mutex<Vec<Job>>> = OnceLock::new();
    Q.get_or_init(Default::default)
}

/// Run `f` on the UI thread soon, outside whatever is running now (background results, deferred UI steps).
pub fn on_main(f: impl FnOnce() + Send + 'static) {
    if let Ok(mut q) = queue().lock() {
        q.push(Box::new(f));
    }
    unsafe {
        let _ = PostMessageW(Some(main_hwnd()), WM_RUN, WPARAM(0), LPARAM(0));
    }
}

pub fn post_hotkey(id: u32) {
    unsafe {
        let _ = PostMessageW(Some(main_hwnd()), WM_HOTKEY_PRESSED, WPARAM(id as usize), LPARAM(0));
    }
}

/// Press Ctrl+V a moment from now, once the app we handed the focus back to is in front.
pub fn paste_soon() {
    unsafe {
        let _ = SetTimer(Some(main_hwnd()), T_PASTE, 70, None);
    }
}

/// Start the caret blink (it stops itself when no panel is open).
pub fn blink_on() {
    unsafe {
        let _ = SetTimer(Some(main_hwnd()), T_BLINK, GetCaretBlinkTime().clamp(200, 1200), None);
    }
}

pub fn open_settings() {
    config::ensure_file();
    shell::edit(&config::settings_path());
}

/// A line for whoever started us from a console (`--install-startup` over SSH): our stdout when redirected,
/// else the parent's console.
fn say(msg: &str) {
    // A GUI program's stdout is often no handle at all, and Rust then drops writes silently: check first.
    let piped = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.is_ok_and(|h| !h.is_invalid() && !h.0.is_null());
    if piped {
        let mut out = std::io::stdout();
        let _ = writeln!(out, "{msg}");
        let _ = out.flush();
        return;
    }
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_ok()
            && let Ok(mut con) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
                let _ = writeln!(con, "{msg}");
            }
    }
}

/// The real Livae's main window class; a `--test` copy uses its own, so it is never taken for the running app.
fn main_class() -> windows::core::PCWSTR {
    if config::test_mode() { w!("LivaeTestMain") } else { w!("LivaeMain") }
}

fn find_running() -> Option<HWND> {
    unsafe { FindWindowW(w!("LivaeMain"), None).ok().filter(|h| !h.is_invalid()) }
}

pub fn run() {
    if gate::is_host_call() {
        gate::serve();
        return;
    }
    gate::install_manifest();
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // COM for the shell, plus OLE for dragging rows out of the search.
        let _ = OleInitialize(None);
    }
    for (flag, install) in [("--install-startup", true), ("--remove-startup", false)] {
        if config::flag(flag) {
            let code = match shell::startup_link(install) {
                Ok(p) => {
                    say(&format!("{} {}", if install { "Created" } else { "Removed" }, p.display()));
                    0
                }
                Err(e) => {
                    say(&format!("Livae {flag} failed: {e}"));
                    1
                }
            };
            std::process::exit(code);
        }
    }
    if config::flag("--quit") {
        if let Some(h) = find_running() {
            unsafe {
                let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        return;
    }
    if !config::test_mode() {
        // One Livae per session. A second start hands its --show / --clipboard to the first and exits.
        // The handle stays open (and the name taken) until the process ends.
        let exists = unsafe {
            SetLastError(WIN32_ERROR(0));
            let _mutex = CreateMutexW(None, true, w!("Local\\Livae.SingleInstance"));
            GetLastError() == ERROR_ALREADY_EXISTS
        };
        if exists {
            let which = if config::flag("--year") { 3 } else if config::flag("--clipboard") { 2 } else if config::flag("--show") { 1 } else { 0 };
            if let (Some(h), true) = (find_running(), which > 0) {
                unsafe {
                    let _ = AllowSetForegroundWindow(windows::Win32::UI::WindowsAndMessaging::ASFW_ANY);
                    let _ = PostMessageW(Some(h), WM_FORWARD, WPARAM(which), LPARAM(0));
                }
            }
            return;
        }
    }

    let hwnd = unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(main_proc),
            hInstance: instance.into(),
            lpszClassName: main_class(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        glass::register();
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::SeqCst);
        CreateWindowExW(WS_EX_TOOLWINDOW, main_class(), w!("Livae"), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance.into()), None).unwrap_or_default()
    };
    MAIN.store(hwnd.0 as isize, Ordering::SeqCst);

    config::ensure_file();
    launcher::install();
    clipboard::install();
    expand::apply(&config::get());
    breaks::install();
    if !config::test_mode() {
        tray::add(hwnd);
        hotkeys::install();
        hotkeys::apply(&config::get());
        tray::update(hwnd);
        unsafe {
            let _ = AddClipboardFormatListener(hwnd);
            // Lock and unlock, and the displays going off: time away for the breaks.
            let _ = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
            if let Ok(h) = RegisterPowerSettingNotification(windows::Win32::Foundation::HANDLE(hwnd.0), &GUID_CONSOLE_DISPLAY_STATE, DEVICE_NOTIFY_WINDOW_HANDLE) {
                DISPLAY_NOTIFY.store(h.0, Ordering::SeqCst);
            }
        }
    }
    unsafe {
        let _ = SetTimer(Some(hwnd), T_RETRY, 5000, None);
        let _ = SetTimer(Some(hwnd), T_INDEX, 10 * 60 * 1000, None);
        let _ = SetTimer(Some(hwnd), T_BREAKS, 1000, None);
        let _ = SetTimer(Some(hwnd), T_JOURNAL, 30_000, None);
    }
    journal::install();
    // A test copy leaves the real devices and Equalizer APO alone unless a check asks for them.
    if !config::test_mode() || std::env::var_os("LIVAE_TEST_SOUND").is_some() {
        sound::install();
    }

    if config::flag("--clipboard") {
        clipboard::show();
    } else if config::flag("--show") {
        launcher::show();
    }
    if let Some(q) = config::value("--query") {
        if config::flag("--clipboard") { clipboard::set_query(&q) } else { launcher::set_query(&q) }
    }
    if let Some(kind) = config::value("--break") {
        breaks::show_test(kind == "long");
    }
    if config::flag("--journal") {
        journal::show_test(config::value("--journal").filter(|v| !v.starts_with("--")).map(std::path::PathBuf::from));
    }
    if config::flag("--year") {
        year::show_test();
    }
    if config::flag("--dump") {
        let _ = START.set(Instant::now());
        unsafe {
            let _ = SetTimer(Some(hwnd), T_DUMP, 250, None);
        }
    }

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

static START: OnceLock<Instant> = OnceLock::new();

/// `--dump`: once the launcher has its apps and file index (or after 15 s), or a break has faded in (or after 5 s),
/// write what the open panel shows.
fn dump_tick() {
    let clip = config::flag("--clipboard");
    let waited = START.get().map_or(0, |s| s.elapsed().as_secs());
    let mut text = if config::flag("--year") {
        year::dump()
    } else if config::flag("--break") {
        if !breaks::settled() && waited < 5 {
            return;
        }
        breaks::dump()
    } else if config::flag("--journal") {
        journal::dump()
    } else {
        if !clip && !launcher::ready() && waited < 15 {
            return;
        }
        if clip { clipboard::dump() } else { launcher::dump() }
    };
    if !clip && !config::flag("--year") && !config::flag("--break") && !config::flag("--journal") && !launcher::ready() {
        text.push_str("# not ready after 15 s\n");
    }
    match config::value("--dump").filter(|v| !v.starts_with("--")) {
        Some(path) => {
            let _ = std::fs::write(path, text);
        }
        None => say(text.trim_end()),
    }
    unsafe { PostQuitMessage(0) };
}

/// Every 5 s: settings edited on disk, shortcuts another app released, the tray state.
fn tick() {
    if let Some((before, after)) = config::reload() {
        if before.launcher != after.launcher || before.clipboard != after.clipboard || before.hotkeys != after.hotkeys {
            hotkeys::apply(&after);
        }
        if before.expansion != after.expansion || before.snippets != after.snippets {
            expand::apply(&after);
        }
        launcher::settings_changed(&before, &after);
        clipboard::settings_changed(&before, &after);
        sound::settings_changed(&before, &after);
    }
    if !config::test_mode() {
        hotkeys::retry();
        tray::update(main_hwnd());
    }
}

fn command(cmd: u32) {
    match cmd {
        tray::SEARCH => launcher::show(),
        tray::CLIPBOARD => clipboard::show(),
        tray::PAUSE => {
            clipboard::toggle_paused();
            tray::update(main_hwnd());
        }
        tray::SETTINGS => open_settings(),
        tray::JOURNAL => journal::show(),
        tray::YEAR => year::show(),
        tray::BREAK_NOW => breaks::start_now(),
        tray::BREAK_SKIP => breaks::skip_to_next_break(),
        tray::BREAK_PAUSE_HOUR => breaks::pause_for(60),
        tray::BREAK_PAUSE_MORNING => breaks::pause_until_morning(),
        tray::BREAK_RESUME => breaks::resume(),
        tray::EQ_TOGGLE => {
            let before = config::get();
            config::update(|s| s.sound.eq = !s.sound.eq);
            sound::settings_changed(&before, &config::get());
        }
        tray::EQ_EDIT => open_settings(),
        tray::QUIT => unsafe {
            let _ = DestroyWindow(main_hwnd());
        },
        _ => {}
    }
}

extern "system" fn main_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            tray::WM_TRAY => {
                let event = (lparam.0 & 0xFFFF) as u32;
                if event == WM_LBUTTONUP || event == WM_RBUTTONUP {
                    command(tray::menu(hwnd));
                }
                LRESULT(0)
            }
            WM_CLIPBOARDUPDATE => {
                clipboard::changed();
                LRESULT(0)
            }
            WM_TIMER => {
                match wparam.0 {
                    T_RETRY => tick(),
                    T_PASTE => {
                        let _ = KillTimer(Some(hwnd), T_PASTE);
                        clipboard::send_paste();
                    }
                    T_INDEX => launcher::refresh_index(),
                    T_DUMP => dump_tick(),
                    T_BLINK => {
                        let a = launcher::blink();
                        let b = clipboard::blink();
                        let c = journal::blink();
                        if !a && !b && !c {
                            let _ = KillTimer(Some(hwnd), T_BLINK);
                        }
                    }
                    T_BREAKS => breaks::tick(),
                    T_JOURNAL => journal::check(),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_RUN => {
                let jobs: Vec<Job> = queue().lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
                for job in jobs {
                    job();
                }
                LRESULT(0)
            }
            WM_HOTKEY_PRESSED => {
                match hotkeys::target(wparam.0 as u32) {
                    Some(hotkeys::Target::Launcher) => launcher::toggle(),
                    Some(hotkeys::Target::Clipboard) => clipboard::toggle(),
                    Some(hotkeys::Target::Command(command)) => run_command(command, GetForegroundWindow()),
                    None => {}
                }
                LRESULT(0)
            }
            expand::WM_EXPAND => {
                expand::flush();
                LRESULT(0)
            }
            WM_FORWARD => {
                match wparam.0 {
                    1 => launcher::show(),
                    2 => clipboard::show(),
                    3 => year::show(),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                if !config::test_mode() {
                    tray::update(hwnd);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_DISPLAYCHANGE => {
                launcher::hide();
                clipboard::hide();
                LRESULT(0)
            }
            WM_WTSSESSION_CHANGE => {
                match wparam.0 as u32 {
                    WTS_SESSION_LOCK => breaks::set_locked(true),
                    WTS_SESSION_UNLOCK => breaks::set_locked(false),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_POWERBROADCAST => {
                match wparam.0 as u32 {
                    PBT_APMSUSPEND => breaks::set_asleep(true),
                    PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => {
                        breaks::set_asleep(false);
                        journal::check();
                    }
                    PBT_POWERSETTINGCHANGE => {
                        // The console display: 0 off, 1 on, 2 dimmed.
                        if let Some(p) = (lparam.0 as *const POWERBROADCAST_SETTING).as_ref()
                            && p.PowerSetting == GUID_CONSOLE_DISPLAY_STATE && p.DataLength >= 1 {
                                breaks::set_displays_off(p.Data[0] == 0);
                            }
                    }
                    _ => {}
                }
                LRESULT(1)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                expand::stop();
                if !config::test_mode() {
                    tray::remove(hwnd);
                    let _ = RemoveClipboardFormatListener(hwnd);
                    let _ = WTSUnRegisterSessionNotification(hwnd);
                    let notify = DISPLAY_NOTIFY.swap(0, Ordering::SeqCst);
                    if notify != 0 {
                        let _ = UnregisterPowerSettingNotification(HPOWERNOTIFY(notify));
                    }
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            m if m != 0 && m == TASKBAR_CREATED.load(Ordering::SeqCst) => {
                // Explorer restarted: the tray icon is gone until we add it again.
                if !config::test_mode() {
                    tray::add(hwnd);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Both launcher rows and global shortcuts use the same main-thread action route.
pub fn run_command(command: livae_core::commands::Command, front: HWND) {
    match command {
        livae_core::commands::Command::System(action) => crate::system::run(action, front),
        livae_core::commands::Command::Window(action) => crate::winmgr::run(action, front),
    }
}
