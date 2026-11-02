//! System commands run on the UI thread, after the launcher gives the focus back.

use livae_core::commands::{self, Command, System};
use livae_core::settings::Platform;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LUID, WPARAM};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Shutdown::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Shell::{SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHEmptyRecycleBinW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PWSTR, w};

use crate::{app, config};

/// Enable the privilege required for sleep, restart and shutdown.
fn shutdown_privilege() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut luid = LUID::default();
        let mut ok = false;
        if LookupPrivilegeValueW(None, w!("SeShutdownPrivilege"), &mut luid).is_ok() {
            let privileges = TOKEN_PRIVILEGES { PrivilegeCount: 1, Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }] };
            ok = AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None).is_ok()
                && windows::Win32::Foundation::GetLastError() == windows::Win32::Foundation::ERROR_SUCCESS;
        }
        let _ = CloseHandle(token);
        ok
    }
}

pub fn run(action: System, front: HWND) {
    if config::test_mode() {
        if let Some(d) = commands::all(Platform::Windows).find(|d| d.command == Command::System(action)) {
            config::log_action(d.id);
        }
        return;
    }
    unsafe {
        match action {
            System::LockScreen => {
                let _ = LockWorkStation();
            }
            System::Sleep => {
                if shutdown_privilege() {
                    SetSuspendState(false, false, false);
                }
            }
            System::DisplaysOff => {
                let _ = PostMessageW(Some(HWND_BROADCAST), WM_SYSCOMMAND, WPARAM(SC_MONITORPOWER as usize), LPARAM(2));
            }
            System::Restart | System::ShutDown | System::LogOut => {
                if action == System::LogOut || shutdown_privilege() {
                    let flags = match action {
                        System::Restart => EWX_REBOOT,
                        System::ShutDown => EWX_POWEROFF,
                        _ => EWX_LOGOFF,
                    };
                    let _ = ExitWindowsEx(flags, SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_FLAG_PLANNED);
                }
            }
            System::EmptyTrash => {
                let _ = SHEmptyRecycleBinW(None, None, SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND);
            }
            System::DarkMode => dark_mode(),
            System::QuitApp | System::ForceQuitApp => quit_app(front, action == System::ForceQuitApp),
            System::EjectDisks => {}
        }
    }
}

fn dark_mode() {
    unsafe {
        let key = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
        let mut light = 1u32;
        let mut size = size_of::<u32>() as u32;
        let _ = RegGetValueW(HKEY_CURRENT_USER, key, w!("AppsUseLightTheme"), RRF_RT_REG_DWORD, None, Some((&mut light as *mut u32).cast()), Some(&mut size));
        let value = u32::from(light == 0);
        for name in [w!("AppsUseLightTheme"), w!("SystemUsesLightTheme")] {
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, key, name, REG_DWORD.0, Some((&value as *const u32).cast()), size_of::<u32>() as u32);
        }
        let _ =
            SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, WPARAM(0), LPARAM(w!("ImmersiveColorSet").as_ptr() as isize), SMTO_ABORTIFHUNG, 200, None);
    }
}

/// Refuse the shell and ourselves even if a hotkey is pressed over one of our windows.
fn quit_app(front: HWND, force: bool) {
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(front, Some(&mut pid));
        if front.is_invalid() || pid == 0 || app::is_ours(front) {
            return;
        }
        let access = PROCESS_QUERY_LIMITED_INFORMATION | if force { PROCESS_TERMINATE } else { PROCESS_ACCESS_RIGHTS(0) };
        let Ok(process) = OpenProcess(access, false, pid) else { return };
        let mut path = [0u16; 32768];
        let mut len = path.len() as u32;
        let known = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(path.as_mut_ptr()), &mut len).is_ok();
        let path = String::from_utf16_lossy(&path[..len.min(path.len() as u32) as usize]);
        let explorer = path.rsplit('\\').next().is_some_and(|s| s.eq_ignore_ascii_case("explorer.exe"));
        if known && !explorer {
            if force {
                let _ = TerminateProcess(process, 1);
            } else {
                let _ = EnumWindows(Some(close_window), LPARAM(pid as isize));
            }
        }
        let _ = CloseHandle(process);
    }
}

unsafe extern "system" fn close_window(hwnd: HWND, param: LPARAM) -> BOOL {
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == param.0 as u32 && IsWindowVisible(hwnd).as_bool() {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    BOOL(1)
}
