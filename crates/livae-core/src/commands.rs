//! The launcher's commands: system actions (lock, sleep, empty the trash, dark mode, quit the app in front) and window
//! placement. Each has an id, so `[hotkeys]` in the settings can give it a shortcut ("left-half" = "Hyper+Left").

use crate::settings::Platform;
use crate::window::Action as Window;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum System {
    LockScreen,
    Sleep,
    DisplaysOff,
    Restart,
    ShutDown,
    LogOut,
    EmptyTrash,
    DarkMode,
    /// Quit the app that was in front when the launcher opened.
    QuitApp,
    ForceQuitApp,
    /// The Mac only.
    EjectDisks,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    System(System),
    Window(Window),
}

pub struct Def {
    pub command: Command,
    pub id: &'static str,
    pub name: &'static str,
    /// Hidden search words.
    pub keywords: &'static str,
}

const fn sys(s: System, id: &'static str, name: &'static str, keywords: &'static str) -> Def {
    Def { command: Command::System(s), id, name, keywords }
}

const fn win(w: Window, id: &'static str, name: &'static str, keywords: &'static str) -> Def {
    Def { command: Command::Window(w), id, name, keywords }
}

const SYSTEM: &[Def] = &[
    sys(System::LockScreen, "lock-screen", "Lock Screen", "lock away"),
    sys(System::Sleep, "sleep", "Sleep", "suspend"),
    sys(System::DisplaysOff, "displays-off", "Turn Off Displays", "sleep displays screen off monitor"),
    sys(System::Restart, "restart", "Restart", "reboot"),
    sys(System::ShutDown, "shut-down", "Shut Down", "shutdown power off turn off"),
    sys(System::LogOut, "log-out", "Log Out", "sign out logout signout"),
    sys(System::DarkMode, "dark-mode", "Toggle Dark Mode", "dark mode light mode appearance theme"),
    sys(System::QuitApp, "quit-app", "Quit App", "close app quit front"),
    sys(System::ForceQuitApp, "force-quit-app", "Force Quit App", "kill frozen not responding"),
];

const WINDOW: &[Def] = &[
    win(Window::LeftHalf, "left-half", "Left Half", "window tile snap"),
    win(Window::RightHalf, "right-half", "Right Half", "window tile snap"),
    win(Window::TopHalf, "top-half", "Top Half", "window tile snap"),
    win(Window::BottomHalf, "bottom-half", "Bottom Half", "window tile snap"),
    win(Window::Maximize, "maximize", "Maximize", "window fill full zoom"),
    win(Window::AlmostMaximize, "almost-maximize", "Almost Maximize", "window fill"),
    win(Window::Center, "center", "Center", "window centre middle"),
    win(Window::LeftThird, "left-third", "Left Third", "window first third"),
    win(Window::CenterThird, "center-third", "Center Third", "window centre middle third"),
    win(Window::RightThird, "right-third", "Right Third", "window last third"),
    win(Window::LeftTwoThirds, "left-two-thirds", "Left Two Thirds", "window first two thirds"),
    win(Window::RightTwoThirds, "right-two-thirds", "Right Two Thirds", "window last two thirds"),
    win(Window::TopLeft, "top-left", "Top Left Quarter", "window corner"),
    win(Window::TopRight, "top-right", "Top Right Quarter", "window corner"),
    win(Window::BottomLeft, "bottom-left", "Bottom Left Quarter", "window corner"),
    win(Window::BottomRight, "bottom-right", "Bottom Right Quarter", "window corner"),
    win(Window::NextDisplay, "next-display", "Next Display", "window move screen monitor"),
    win(Window::Restore, "restore", "Restore Window", "window undo back"),
];

const MAC: &[Def] = &[
    sys(System::EmptyTrash, "empty-trash", "Empty Trash", "bin delete"),
    sys(System::EjectDisks, "eject", "Eject All Disks", "unmount drives volumes usb"),
];

const WINDOWS: &[Def] = &[sys(System::EmptyTrash, "empty-trash", "Empty Recycle Bin", "trash bin delete")];

/// Every command this system offers, in the order the launcher lists them on a tie.
pub fn all(platform: Platform) -> impl Iterator<Item = &'static Def> {
    let own = match platform {
        Platform::Mac => MAC,
        Platform::Windows => WINDOWS,
    };
    SYSTEM.iter().chain(own).chain(WINDOW)
}

pub fn by_id(id: &str, platform: Platform) -> Option<Command> {
    all(platform).find(|d| d.id.eq_ignore_ascii_case(id.trim())).map(|d| d.command)
}

impl Command {
    /// Asks for a second Enter first: it closes apps, loses work or cannot be undone.
    pub fn confirm(self) -> bool {
        matches!(self, Command::System(System::Restart | System::ShutDown | System::LogOut | System::EmptyTrash | System::ForceQuitApp))
    }

    /// The kind shown beside the name.
    pub fn detail(self) -> &'static str {
        match self {
            Command::System(_) => "Command",
            Command::Window(_) => "Window",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_found_on_each_system() {
        for p in [Platform::Mac, Platform::Windows] {
            let ids: Vec<&str> = all(p).map(|d| d.id).collect();
            let mut unique = ids.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(ids.len(), unique.len(), "{p:?}");
        }
        assert_eq!(by_id("Left-Half", Platform::Windows), Some(Command::Window(Window::LeftHalf)));
        assert_eq!(by_id("eject", Platform::Mac), Some(Command::System(System::EjectDisks)));
        assert_eq!(by_id("eject", Platform::Windows), None);
        assert!(by_id("shut-down", Platform::Mac).unwrap().confirm());
        assert!(!by_id("lock-screen", Platform::Mac).unwrap().confirm());
    }
}
