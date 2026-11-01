//! The clipboard history model shared by both systems: what a copied item is, the order it is kept in, and how
//! the list is saved (`clipboard.json`, images as PNG files in `clips/`). The format is the Mac's, field for field.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Text longer than this is not recorded (a pasted log would stall every search and save).
pub const MAX_TEXT: usize = 1 << 20;
/// Images larger than this (PNG bytes) are not recorded.
pub const MAX_IMAGE: usize = 24 << 20;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    #[default]
    Text,
    Code,
    Link,
    Color,
    Image,
    File,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Clip {
    pub id: u64,
    pub kind: Kind,
    /// The text; for files the path (several paths one per line); for images empty.
    pub text: String,
    /// PNG file name in `clips/` for images.
    pub image: Option<String>,
    pub w: u32,
    pub h: u32,
    pub hash: u64,
    /// The app it was copied from.
    pub app: String,
    /// Seconds since 1970.
    pub time: i64,
    pub pinned: bool,
}

/// Text kinds from the shared classifier.
pub fn classify(text: &str, app: &str) -> Kind {
    match crate::clip::classify(text, app) {
        crate::clip::TextKind::Link => Kind::Link,
        crate::clip::TextKind::Color => Kind::Color,
        crate::clip::TextKind::Code => Kind::Code,
        crate::clip::TextKind::Text => Kind::Text,
    }
}

pub fn hash_of(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Short age text for the quiet time column: "Now", "5 min", "2 h", "3 d".
pub fn ago(now: i64, t: i64) -> String {
    let s = (now - t).max(0);
    match s {
        0..=59 => "Now".into(),
        60..=3599 => format!("{} min", s / 60),
        3600..=86_399 => format!("{} h", s / 3600),
        _ => format!("{} d", s / 86_400),
    }
}

#[derive(Debug, Default)]
pub struct History {
    pub clips: Vec<Clip>,
    pub next_id: u64,
    /// The search keys made ready for matching, kept between keystrokes.
    search: std::cell::RefCell<crate::search::Cache>,
}

impl History {
    pub fn new(clips: Vec<Clip>) -> Self {
        let next_id = clips.iter().map(|c| c.id).max().unwrap_or(0) + 1;
        Self { clips, next_id, search: Default::default() }
    }

    /// Re-sort texts with the current rules (an older build could file prose as code).
    pub fn reclassify(&mut self) {
        for c in self.clips.iter_mut().filter(|c| matches!(c.kind, Kind::Text | Kind::Code)) {
            c.kind = classify(&c.text, &c.app);
        }
    }

    /// True when an item with this content is already kept (an image file need not be written again).
    pub fn has(&self, kind: Kind, hash: u64) -> bool {
        self.clips.iter().any(|c| c.kind == kind && c.hash == hash)
    }

    /// The file name a new image is saved under.
    pub fn next_image_name(&self) -> String {
        format!("{}.png", self.next_id)
    }

    /// A new copy goes to the top. The same thing copied again moves up, keeping its id, pin and image file.
    /// Returns an image file that is no longer needed (the new copy's own, when the old one is kept).
    pub fn add(&mut self, mut clip: Clip) -> Option<String> {
        let mut unused = None;
        if let Some(i) = self.clips.iter().position(|c| c.hash == clip.hash && c.kind == clip.kind) {
            let old = self.clips.remove(i);
            clip.pinned = old.pinned;
            clip.id = old.id;
            if old.image.is_some() {
                if clip.image != old.image {
                    unused = clip.image.take();
                }
                clip.image = old.image;
            }
        } else {
            clip.id = self.next_id;
            self.next_id += 1;
        }
        self.clips.insert(0, clip);
        unused
    }

    /// Keep every pinned item and the newest unpinned ones up to `limit` (at least 10). Returns the image files
    /// of the items dropped.
    pub fn prune(&mut self, limit: usize) -> Vec<String> {
        let limit = limit.max(10);
        let mut unpinned = 0;
        let mut dropped = Vec::new();
        self.clips.retain(|c| {
            if c.pinned {
                return true;
            }
            unpinned += 1;
            if unpinned <= limit {
                return true;
            }
            dropped.extend(c.image.clone());
            false
        });
        dropped
    }

    pub fn toggle_pin(&mut self, i: usize) {
        if let Some(c) = self.clips.get_mut(i) {
            c.pinned = !c.pinned;
        }
    }

    pub fn remove(&mut self, i: usize) -> Option<Clip> {
        (i < self.clips.len()).then(|| self.clips.remove(i))
    }

    /// Used again: it moves to the top with a fresh time.
    pub fn touch(&mut self, i: usize, now: i64) {
        if i < self.clips.len() {
            let mut c = self.clips.remove(i);
            c.time = now;
            self.clips.insert(0, c);
        }
    }

    pub fn clear_unpinned(&mut self) -> Vec<String> {
        let dropped = self.clips.iter().filter(|c| !c.pinned).filter_map(|c| c.image.clone()).collect();
        self.clips.retain(|c| c.pinned);
        dropped
    }

    /// Indices to show: with no query pinned items first, then newest; with a query the best fuzzy matches.
    pub fn order(&self, query: &str) -> Vec<usize> {
        let q = query.trim();
        if q.is_empty() {
            let mut v: Vec<usize> = (0..self.clips.len()).collect();
            v.sort_by_key(|&i| !self.clips[i].pinned);
            return v;
        }
        let keys: Vec<String> = self.clips.iter().map(search_key).collect();
        self.search.borrow_mut().rank(q, &keys, |_| 0)
    }
}

/// What the search looks at: the text, a file's name, or "image" and the app for pictures.
fn search_key(c: &Clip) -> String {
    match c.kind {
        Kind::Image => format!("image {}", c.app),
        Kind::File => c.text.lines().map(|p| p.rsplit(['/', '\\']).next().unwrap_or(p)).collect::<Vec<_>>().join(" "),
        _ => c.text.chars().take(500).collect(),
    }
}

/// The saved history. One unreadable item (a kind from a newer build) is skipped, not the whole list; a file this
/// build cannot read at all is kept aside as `clipboard.json.bad`, never overwritten.
pub fn load(path: &Path) -> Vec<Clip> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    // A byte-order mark (a file written by Windows PowerShell) is not part of the JSON.
    match serde_json::from_str::<Vec<serde_json::Value>>(text.trim_start_matches('\u{feff}')) {
        Ok(items) => items.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect(),
        Err(_) => {
            let _ = std::fs::rename(path, path.with_extension("json.bad"));
            Vec::new()
        }
    }
}

pub fn save(path: &Path, clips: &[Clip]) -> io::Result<()> {
    let json = serde_json::to_string(clips).map_err(io::Error::other)?;
    crate::write_atomic(path, json.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &str, time: i64) -> Clip {
        Clip { kind: classify(t, ""), text: t.into(), hash: hash_of(t.as_bytes()), time, ..Default::default() }
    }

    #[test]
    fn copies_dedupe_to_the_top_keeping_pins() {
        let mut h = History { next_id: 1, ..History::default() };
        h.add(text("one", 1));
        h.add(text("two", 2));
        h.toggle_pin(1); // "one"
        h.add(text("one", 3));
        assert_eq!(h.clips.len(), 2);
        assert_eq!((h.clips[0].text.as_str(), h.clips[0].pinned, h.clips[0].id, h.clips[0].time), ("one", true, 1, 3));
        // An image copied again keeps its first file; the duplicate's file is returned for deletion.
        let img = |name: &str| Clip { kind: Kind::Image, hash: 9, image: Some(name.into()), ..Default::default() };
        assert_eq!(h.add(img("3.png")), None);
        assert_eq!(h.add(img("4.png")).as_deref(), Some("4.png"));
        assert_eq!(h.clips[0].image.as_deref(), Some("3.png"));
    }

    #[test]
    fn prune_keeps_pins_and_reports_dropped_images() {
        let mut h = History::default();
        for i in 0..15 {
            let mut c = text(&format!("item {i}"), i);
            if i == 0 {
                c.pinned = true;
            }
            if i == 1 {
                c.kind = Kind::Image;
                c.image = Some("old.png".into());
            }
            h.add(c);
        }
        let dropped = h.prune(10);
        assert_eq!(dropped, vec!["old.png".to_string()]);
        assert_eq!(h.clips.len(), 11);
        assert!(h.clips.iter().any(|c| c.text == "item 0" && c.pinned));
        // Pinned first with no query; a query ranks matches.
        assert_eq!(h.clips[h.order("")[0]].text, "item 0");
        assert_eq!(h.clips[h.order("item 14")[0]].text, "item 14");
    }

    #[test]
    fn load_skips_bad_items_and_keeps_a_broken_file_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clipboard.json");
        std::fs::write(&path, r#"[{"id":1,"kind":"Text","text":"hi"},{"id":2,"kind":"Hologram"},{"id":3,"kind":"File","text":"C:\\a\\b.txt","pinned":true}]"#).unwrap();
        let clips = load(&path);
        assert_eq!(clips.iter().map(|c| c.id).collect::<Vec<_>>(), vec![1, 3]);
        // Round trip through the atomic save.
        save(&path, &clips).unwrap();
        assert_eq!(load(&path), clips);
        assert!(!dir.path().join("clipboard.json.tmp").exists());
        // A file search finds files by name on either system's separators.
        assert_eq!(History::new(clips).order("b.txt"), vec![1]);
        std::fs::write(&path, "\u{feff}[{\"id\":7}]").unwrap();
        assert_eq!(load(&path).len(), 1);
        std::fs::write(&path, "not json").unwrap();
        assert!(load(&path).is_empty());
        assert!(dir.path().join("clipboard.json.bad").exists());
    }

    #[test]
    fn age_text() {
        assert_eq!(ago(100, 100), "Now");
        assert_eq!(ago(100, 200), "Now");
        assert_eq!(ago(400, 100), "5 min");
        assert_eq!(ago(7300, 100), "2 h");
        assert_eq!(ago(3 * 86_400 + 5, 0), "3 d");
    }
}
