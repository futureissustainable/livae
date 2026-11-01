pub mod breaks;
pub mod calc;
pub mod clip;
pub mod field;
pub mod files;
pub mod history;
pub mod journal;
pub mod search;
pub mod settings;
pub mod shortcut;
pub mod system_settings;
pub mod tokens;
pub mod units;
pub mod usage;

use std::io;
use std::path::Path;

/// Write a copy next to `path`, then swap it in: a crash mid-write never leaves a half file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
