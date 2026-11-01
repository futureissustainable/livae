//! Zen Gate (the browser add-on) reads its sites, waits and quotes from Livae through Firefox native messaging.
//! The browser starts this same binary with the host manifest path and the add-on id as arguments; Livae then
//! answers one request on stdin/stdout and exits, without starting the app.

use std::io::{Read, Write};

use serde_json::json;

use crate::config;

const HOST: &str = "com.mateus.livae";
/// The extension's id, and the id of builds from before the public release (still allowed to call).
const ADDONS: [&str; 2] = ["zen-gate@livae", "zen-gate@god.local"];

/// True when the browser started us as the native messaging host.
pub fn is_host_call() -> bool {
    std::env::args().skip(1).any(|a| ADDONS.contains(&a.as_str()) || a.ends_with(&format!("{HOST}.json")))
}

/// What Zen Gate gets: the sites and waits from Settings, the quotes from `quotes.md` (the same ones the breaks show).
fn gate_config() -> serde_json::Value {
    let s = config::get();
    let quotes: Vec<serde_json::Value> = crate::breaks::quotes()
        .into_iter()
        .map(|q| json!({ "text": q.text, "author": q.author.unwrap_or_default() }))
        .collect();
    json!({ "version": 1, "pass_minutes": s.gate.pass_minutes, "sites": s.gate.sites, "quotes": quotes })
}

/// Native messaging: each message is a 4-byte little-endian length, then that much UTF-8 JSON.
pub fn serve() {
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut len = [0u8; 4];
    while stdin.read_exact(&mut len).is_ok() {
        let n = u32::from_le_bytes(len) as usize;
        if n > 1 << 20 {
            return;
        }
        let mut body = vec![0u8; n];
        if stdin.read_exact(&mut body).is_err() {
            return;
        }
        let ask: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        let reply = if ask.get("get").and_then(|g| g.as_str()) == Some("gate") { gate_config() } else { json!({ "error": "unknown request" }) };
        let out = reply.to_string().into_bytes();
        if stdout.write_all(&(out.len() as u32).to_le_bytes()).and_then(|_| stdout.write_all(&out)).and_then(|_| stdout.flush()).is_err() {
            return;
        }
    }
}

/// Register Livae as the host for Zen Gate (Firefox and Zen read the Mozilla folder). Rewritten at every start,
/// so it always points at the installed binary.
pub fn install_manifest() {
    let Ok(exe) = std::env::current_exe() else { return };
    let dir = config::home().join("Library/Application Support/Mozilla/NativeMessagingHosts");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let manifest = json!({
        "name": HOST,
        "description": "Livae: Zen Gate sites, waits and quotes",
        "path": exe,
        "type": "stdio",
        "allowed_extensions": ADDONS,
    });
    let file = dir.join(format!("{HOST}.json"));
    let text = serde_json::to_string_pretty(&manifest).unwrap_or_default();
    if std::fs::read_to_string(&file).ok().as_deref() != Some(text.as_str()) {
        let _ = std::fs::write(file, text);
    }
}
