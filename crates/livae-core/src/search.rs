//! Fuzzy ranking shared by the launcher and the clipboard history on both systems.

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Indices of `items` matching `query`, best first. An empty query keeps the given order.
/// `boost` adds to an item's score (for example recent use), so a frequent app wins a tie.
pub fn rank<S: AsRef<str>>(query: &str, items: &[S], boost: impl Fn(usize) -> u32) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() {
        return (0..items.len()).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::new(query, CaseMatching::Ignore, Normalization::Smart, AtomKind::Fuzzy);
    let mut buf = Vec::new();
    let mut hits: Vec<(u32, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let text = s.as_ref();
            let score = pattern.score(Utf32Str::new(text, &mut buf), &mut matcher)?;
            // A name that starts with the query reads as the obvious pick.
            let prefix = if text.to_lowercase().starts_with(&query.to_lowercase()) { 200 } else { 0 };
            Some((score + prefix + boost(i), i))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(items[a.1].as_ref().len().cmp(&items[b.1].as_ref().len())));
    hits.into_iter().map(|(_, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::rank;

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
    fn boost_breaks_ties() {
        let apps = ["Discord", "Disk Utility"];
        assert_eq!(apps[rank("dis", &apps, |i| if i == 1 { 500 } else { 0 })[0]], "Disk Utility");
    }
}
