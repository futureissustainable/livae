//! Livae for macOS: launcher and clipboard in Apple's Liquid Glass, as a menu-bar app.

#[cfg(target_os = "macos")]
mod access;
#[cfg(target_os = "macos")]
mod app;
#[cfg(target_os = "macos")]
mod apps;
#[cfg(target_os = "macos")]
mod breaks;
#[cfg(target_os = "macos")]
mod clipboard;
#[cfg(target_os = "macos")]
mod config;
#[cfg(target_os = "macos")]
mod gate;
#[cfg(target_os = "macos")]
mod glass;
#[cfg(target_os = "macos")]
mod hud;
#[cfg(target_os = "macos")] mod journal;
#[cfg(target_os = "macos")]
mod launcher;
#[cfg(target_os = "macos")]
mod list;
#[cfg(target_os = "macos")]
mod panel;
#[cfg(target_os = "macos")]
mod settings;
#[cfg(target_os = "macos")]
mod ui;
#[cfg(target_os = "macos")]
mod welcome;
#[cfg(target_os = "macos")] mod year;

#[cfg(target_os = "macos")]
fn main() {
    // Started by the browser for Zen Gate: answer and exit, no app.
    if gate::is_host_call() {
        gate::serve();
        return;
    }
    app::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("livae-mac runs on macOS only");
}
