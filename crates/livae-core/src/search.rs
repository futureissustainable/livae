//! Fuzzy ranking shared by the launcher and the clipboard history on both systems.

use std::collections::HashMap;

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Indices of `items` matching `query`, best first. An empty query keeps the given order.
/// `boost` adds to an item's score (for example recent use), so a frequent app wins a tie.
pub fn rank<S: AsRef<str>>(query: &str, items: &[S], boost: impl Fn(usize) -> u32) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() {
        return (0..items.len()).collect();
    }
    let (ql, pattern) = (query.to_lowercase(), fuzzy(query));
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let hits = items.iter().enumerate().filter_map(|(i, s)| {
        let text = s.as_ref();
        let score = pattern.score(Utf32Str::new(text, &mut buf), &mut matcher)?;
        Some((score + prefix(&text.to_lowercase(), &ql) + boost(i), i))
    });
    best(hits.collect(), |i| items[i].as_ref().len())
}

/// `rank` for a list searched again on every keystroke (the clipboard history): each text is made ready for matching
/// once and kept while the list still holds it. Same results as `rank`.
#[derive(Debug, Default)]
pub struct Cache {
    ready: HashMap<String, Ready>,
    round: u64,
}

#[derive(Debug)]
struct Ready {
    /// The text as the matcher sees it; None when that is its own bytes.
    chars: Option<Vec<char>>,
    lower: String,
    round: u64,
}

impl Cache {
    pub fn rank<S: AsRef<str>>(&mut self, query: &str, items: &[S], boost: impl Fn(usize) -> u32) -> Vec<usize> {
        let query = query.trim();
        if query.is_empty() {
            return (0..items.len()).collect();
        }
        let (ql, pattern) = (query.to_lowercase(), fuzzy(query));
        let mut matcher = Matcher::new(Config::DEFAULT);
        self.round += 1;
        let mut hits = Vec::new();
        for (i, s) in items.iter().enumerate() {
            let text = s.as_ref();
            if !self.ready.contains_key(text) {
                self.ready.insert(text.to_string(), Ready::new(text));
            }
            let Some(r) = self.ready.get_mut(text) else { continue };
            r.round = self.round;
            // As Utf32Str::new: its own bytes when every grapheme starts with an ASCII character.
            let haystack = r.chars.as_deref().map_or(Utf32Str::Ascii(text.as_bytes()), Utf32Str::Unicode);
            if let Some(score) = pattern.score(haystack, &mut matcher) {
                hits.push((score + prefix(&r.lower, &ql) + boost(i), i));
            }
        }
        let round = self.round;
        self.ready.retain(|_, r| r.round == round);
        best(hits, |i| items[i].as_ref().len())
    }
}

impl Ready {
    fn new(text: &str) -> Ready {
        let chars = (!text.is_ascii()).then(|| nucleo_matcher::chars::graphemes(text).collect::<Vec<char>>()).filter(|c| !c.iter().all(char::is_ascii));
        Ready { chars, lower: text.to_lowercase(), round: 0 }
    }
}

fn fuzzy(query: &str) -> Pattern {
    Pattern::new(query, CaseMatching::Ignore, Normalization::Smart, AtomKind::Fuzzy)
}

/// A name that starts with the query reads as the obvious pick.
fn prefix(lower: &str, query_lower: &str) -> u32 {
    if lower.starts_with(query_lower) { 200 } else { 0 }
}

/// Indices by score, best first; ties go to the shorter text.
fn best(hits: Vec<(u32, usize)>, len: impl Fn(usize) -> usize) -> Vec<usize> {
    sorted(hits, len).into_iter().map(|(_, i)| i).collect()
}

fn sorted(mut hits: Vec<(u32, usize)>, len: impl Fn(usize) -> usize) -> Vec<(u32, usize)> {
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(len(a.1).cmp(&len(b.1))));
    hits
}

/// Like `rank`, for items with hidden search words: `(name, words)`. The name counts first (an exact name beats a
/// name that starts with the query, which beats one that merely contains it); the words only find an item whose name
/// does not match ("wifi" finds Wi-Fi, "dark mode" finds Colors). Ties go to the shorter name.
pub fn rank_words<N: AsRef<str>, W: AsRef<str>>(query: &str, items: &[(N, W)], boost: impl Fn(usize) -> u32) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..items.len()).collect();
    }
    rank_words_scored(query, items, boost).into_iter().map(|(_, i)| i).collect()
}

/// `rank_words` with each item's score, best first, so lists ranked apart (apps, commands, quicklinks) can be
/// merged into one order. An empty query matches nothing.
pub fn rank_words_scored<N: AsRef<str>, W: AsRef<str>>(query: &str, items: &[(N, W)], boost: impl Fn(usize) -> u32) -> Vec<(u32, usize)> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let ql = query.to_lowercase();
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = fuzzy(query);
    // The hidden words must contain every typed word as written: fuzzy matching there let "chrome" find Display.
    let exact = Pattern::new(query, CaseMatching::Ignore, Normalization::Smart, AtomKind::Substring);
    let mut buf = Vec::new();
    let hits: Vec<(u32, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, (name, words))| {
            let (name, words) = (name.as_ref(), words.as_ref());
            let lower = name.to_lowercase();
            let by_name = pattern.score(Utf32Str::new(name, &mut buf), &mut matcher).map(|s| {
                s + if lower == ql { 400 } else if lower.starts_with(&ql) { 200 } else { 0 }
            });
            let score = match by_name {
                Some(s) => s,
                None if words.is_empty() => return None,
                None => exact.score(Utf32Str::new(words, &mut buf), &mut matcher)?,
            };
            Some((score + boost(i), i))
        })
        .collect();
    sorted(hits, |i| items[i].0.as_ref().len())
}

#[cfg(test)]
mod tests {
    use super::{rank, rank_words_scored, Cache};

    #[test]
    fn cache_ranks_as_rank_does() {
        let items = ["Résumé été", "resume", "e\u{301}te", "Project notes", "notes project", "ünïcode Σ", "report.pdf"];
        let mut cache = Cache::default();
        for q in ["r", "re", "res", "e", "été", "pro not", "Σ", "zzz", " "] {
            for _ in 0..2 {
                assert_eq!(cache.rank(q, &items, |i| i as u32), rank(q, &items, |i| i as u32), "{q}");
            }
        }
        cache.rank("r", &items[..2], |_| 0);
        assert_eq!(cache.ready.len(), 2);
    }

    #[test]
    fn prefix_and_fuzzy_matches_rank_first() {
        let apps = ["Activity Monitor", "DaVinci Resolve", "Discord", "Ghostty", "Google Chrome"];
        let r = rank("dav", &apps, |_| 0);
        assert_eq!(apps[r[0]], "DaVinci Resolve");
        let r = rank("gc", &apps, |_| 0);
        assert_eq!(apps[r[0]], "Google Chrome");
        assert!(rank("zzz", &apps, |_| 0).is_empty());
        assert_eq!(rank("  ", &apps, |_| 0).len(), apps.len());
    }

    #[test]
    fn scored_words_merge_across_lists() {
        let apps = [("Battery", "sleep energy")];
        let commands = [("Sleep", "suspend"), ("Turn Off Displays", "sleep displays")];
        let (a, c) = (rank_words_scored("sleep", &apps, |_| 0), rank_words_scored("sleep", &commands, |_| 0));
        // The command named Sleep beats an app that only lists "sleep" among its hidden words.
        assert!(c[0].0 > a[0].0 && c[0].1 == 0);
        assert!(rank_words_scored(" ", &apps, |_| 0).is_empty());
    }

    #[test]
    fn boost_breaks_ties() {
        let apps = ["Discord", "Disk Utility"];
        assert_eq!(apps[rank("dis", &apps, |i| if i == 1 { 500 } else { 0 })[0]], "Disk Utility");
    }
}
