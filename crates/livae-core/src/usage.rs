//! How often each launcher item was opened (app name or file path), for ranking. Kept in `usage.tsv` as
//! "count<TAB>key" lines, the Mac's format.

use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Default)]
pub struct Usage {
    counts: HashMap<String, u32>,
}

impl Usage {
    pub fn load(path: &Path) -> Self {
        let counts = std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('\t').and_then(|(n, k)| Some((k.to_string(), n.parse().ok()?))))
            .collect();
        Self { counts }
    }

    /// Ranking bonus: frequent items win close matches, without burying a better text match.
    pub fn boost(&self, key: &str) -> u32 {
        self.counts.get(key).map_or(0, |&n| 30 * n.min(20))
    }

    pub fn count(&self, key: &str) -> u32 {
        self.counts.get(key).copied().unwrap_or(0)
    }

    pub fn bump(&mut self, key: &str) {
        *self.counts.entry(key.to_string()).or_default() += 1;
    }

    /// The 500 most used items.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut lines: Vec<_> = self.counts.iter().collect();
        lines.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let text: String = lines.iter().take(500).map(|(k, n)| format!("{n}\t{k}\n")).collect();
        crate::write_atomic(path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::Usage;

    #[test]
    fn counts_survive_a_save_and_cap_the_boost() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.tsv");
        let mut u = Usage::load(&path);
        for _ in 0..25 {
            u.bump("Notepad");
        }
        u.bump(r"C:\Users\me\Documents\a.txt");
        u.save(&path).unwrap();
        let u = Usage::load(&path);
        assert_eq!(u.count("Notepad"), 25);
        assert_eq!(u.boost("Notepad"), 600);
        assert_eq!(u.boost(r"C:\Users\me\Documents\a.txt"), 30);
        assert_eq!(u.boost("Paint"), 0);
    }
}
