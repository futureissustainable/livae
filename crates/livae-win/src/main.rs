//! Livae for Windows: the launcher, the clipboard history, break reminders and the evening journal, drawn with
//! Direct2D over a blurred copy of the screen, as a tray app. The behaviour follows the Mac version; shared rules live
//! in livae-core.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod breaks;
#[cfg(windows)]
mod clipboard;
#[cfg(windows)]
mod config;
#[cfg(windows)]
mod glass;
#[cfg(windows)]
mod hotkeys;
#[cfg(windows)]
mod journal;
#[cfg(windows)]
mod launcher;
#[cfg(windows)]
mod shell;
#[cfg(windows)]
mod tray;

#[cfg(windows)]
fn main() {
    app::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("livae-win runs on Windows only");
}
