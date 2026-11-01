//! The installed apps the launcher can open, and how often each was opened (for ranking).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::support_dir;

pub struct App {
    pub name: String,
    pub path: PathBuf,
}

/// Every `.app` in the usual folders and one level of subfolders (e.g. `/Applications/Adobe Photoshop 2025/`),
/// one per name (the first folder wins), sorted by name.
pub fn scan() -> Vec<App> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let roots = [
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Applications/Utilities"),
        PathBuf::from("/System/Library/CoreServices/Applications"),
        home.join("Applications"),
    ];
    let mut out: Vec<App> = Vec::new();
    let push = |path: PathBuf, out: &mut Vec<App>| {
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else { return };
        if name.starts_with('.') {
            return; // hidden helpers such as .Karabiner-VirtualHIDDevice-Manager
        }
        if !out.iter().any(|a| a.name == name) {
            out.push(App { name: name.to_string(), path });
        }
    };
    let is_app = |p: &Path| p.extension().is_some_and(|e| e == "app");
    for root in roots {
        let Ok(dir) = std::fs::read_dir(&root) else { continue };
        for entry in dir.flatten() {
            let path = entry.path();
            if is_app(&path) {
                push(path, &mut out);
            } else if path.is_dir() && !path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
                if let Ok(sub) = std::fs::read_dir(&path) {
                    for e in sub.flatten() {
                        if is_app(&e.path()) {
                            push(e.path(), &mut out);
                        }
                    }
                }
            }
        }
    }
    push(PathBuf::from("/System/Library/CoreServices/Finder.app"), &mut out);
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// Open counts per item key (app name or file path), kept in `usage.tsv` as "count<TAB>key" lines.
pub struct Usage {
    counts: HashMap<String, u32>,
}

impl Usage {
    pub fn load() -> Self {
        let counts = std::fs::read_to_string(support_dir().join("usage.tsv"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('\t').and_then(|(n, k)| Some((k.to_string(), n.parse().ok()?))))
            .collect();
        Self { counts }
    }

    /// Ranking bonus: frequent items win close matches, without burying a better text match.
    pub fn boost(&self, key: &str) -> u32 {
        self.counts.get(key).map_or(0, |&n| 30 * (n.min(20)))
    }

    pub fn bump(&mut self, key: &str) {
        *self.counts.entry(key.to_string()).or_default() += 1;
        if crate::app::test_mode() {
            return;
        }
        let mut lines: Vec<_> = self.counts.iter().collect();
        lines.sort_by(|a, b| b.1.cmp(a.1));
        let text: String = lines.iter().take(500).map(|(k, n)| format!("{n}\t{k}\n")).collect();
        let _ = std::fs::write(support_dir().join("usage.tsv"), text);
    }
}

/// Seed ranking from macOS's own record of when each app was last opened (Spotlight's kMDItemLastUsedDate),
/// so a fresh install already ranks the apps you use. Blocking: call it off the main thread.
pub fn recent_scores(apps: Vec<(String, PathBuf)>, out: std::sync::Arc<std::sync::Mutex<HashMap<String, u32>>>) {
    {
        let Ok(o) = std::process::Command::new("/usr/bin/mdls").arg("-name").arg("kMDItemLastUsedDate").arg("-raw").args(apps.iter().map(|(_, p)| p)).output() else {
            return;
        };
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        // -raw prints the values separated by NUL; "(null)" when never used.
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let mut scores = HashMap::new();
        for ((name, _), value) in apps.iter().zip(text.split('\0')) {
            let Some(day) = value.get(0..10) else { continue };
            let parts: Vec<i64> = day.split('-').filter_map(|p| p.parse().ok()).collect();
            if parts.len() != 3 {
                continue;
            }
            // Days since 1970 for a calendar date (civil-from-days inverse).
            let (y, m, d) = (parts[0] - if parts[1] <= 2 { 1 } else { 0 }, parts[1], parts[2]);
            let era = y.div_euclid(400);
            let yoe = y - era * 400;
            let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
            let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
            let days = era * 146_097 + doe - 719_468;
            let age = (now / 86_400 - days).max(0);
            let score = match age {
                0..=2 => 160,
                3..=7 => 120,
                8..=30 => 70,
                31..=180 => 25,
                _ => 0,
            };
            if score > 0 {
                scores.insert(name.clone(), score);
            }
        }
        if let Ok(mut r) = out.lock() {
            *r = scores;
        }
    }
}
