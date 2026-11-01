//! File search rules shared by both systems: which folders are never searched, and how file names rank.

/// Folders full of files nobody opens by hand (package managers, build output, app data).
pub const NOISE: [&str; 13] = [
    "node_modules", "site-packages", "__pycache__", "target", "vendor", "Pods", "DerivedData", "bower_components", "venv", ".git",
    "AppData", "$RECYCLE.BIN", "System Volume Information",
];

/// A folder the file search never walks into: hidden by name (".cache", "$WinREAgent") or noise.
pub fn skip_dir(name: &str) -> bool {
    name.starts_with('.') || name.starts_with('$') || NOISE.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// The query's words, lower case. Every word must be in a file name for it to match.
pub fn words(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// How well a lower-case file name fits the words: None when a word is missing. A name that starts with the
/// query beats one where a word starts, which beats a name that only contains it.
pub fn score(words: &[String], name_lower: &str) -> Option<u32> {
    if words.is_empty() {
        return None;
    }
    let mut score = 0;
    for w in words {
        let at = find(name_lower.as_bytes(), w.as_bytes())?;
        let word_start = at == 0 || !name_lower[..at].chars().next_back().is_some_and(char::is_alphanumeric);
        score += if word_start { 100 } else { 10 };
    }
    if name_lower.starts_with(words[0].as_str()) {
        score += 300;
    }
    Some(score)
}

/// Where `needle` first occurs in `hay`, as `str::find` gives it; quicker for a short word in a short name.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let (&first, rest) = needle.split_first()?;
    let last = hay.len().checked_sub(needle.len())?;
    (0..=last).find(|&i| hay[i] == first && &hay[i + 1..i + needle.len()] == rest)
}

/// Indices of the best `limit` entries, best first. Each entry is (lower-case name, folder depth); `boost(i)` (opened
/// before, recently used) is asked only for matches and outranks the name; shallower, shorter names win ties.
pub fn rank<'a>(query: &str, entries: impl Iterator<Item = (&'a str, u16)>, boost: impl Fn(usize) -> u32, limit: usize) -> Vec<usize> {
    let words = words(query);
    let mut hits: Vec<(u32, u16, usize, usize)> = entries
        .enumerate()
        .filter_map(|(i, (name, depth))| Some((score(&words, name)? + boost(i), depth, name.len(), i)))
        .collect();
    // Only the best `limit` are sorted; the index breaks the last ties, as a stable sort of all hits would.
    let order = |a: &(u32, u16, usize, usize), b: &(u32, u16, usize, usize)| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)).then(a.3.cmp(&b.3));
    if limit < hits.len() {
        hits.select_nth_unstable_by(limit, order);
        hits.truncate(limit);
    }
    hits.sort_unstable_by(order);
    hits.into_iter().map(|h| h.3).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_and_hidden_folders_are_skipped() {
        for name in ["node_modules", "AppData", ".git", ".cache", "$RECYCLE.BIN", "Target"] {
            assert!(skip_dir(name), "{name}");
        }
        for name in ["Documents", "Projects", "targets"] {
            assert!(!skip_dir(name), "{name}");
        }
    }

    #[test]
    fn find_matches_str_find() {
        for (hay, needle) in [("résumé été.pdf", "été"), ("aaab", "ab"), ("ab", "abc"), ("x", ""), ("report", "rep"), ("my_report", "port")] {
            assert_eq!(find(hay.as_bytes(), needle.as_bytes()), if needle.is_empty() { None } else { hay.find(needle) }, "{hay} {needle}");
        }
    }

    #[test]
    fn names_rank_by_fit_then_boost_then_depth() {
        let names = ["old report.pdf", "report.pdf", "myreport.txt", "notes.md", "report.pdf"];
        let entries = || names.iter().enumerate().map(|(i, n)| (*n, if i == 4 { 1 } else { 3 }));
        // Starts-with first; the shallower of two equal names wins.
        assert_eq!(rank("rep", entries(), |_| 0, 10), vec![4, 1, 0, 2]);
        assert_eq!(rank("report pdf", entries(), |_| 0, 10), vec![4, 1, 0]);
        assert!(rank("zzz", entries(), |_| 0, 10).is_empty());
        assert!(rank("  ", entries(), |_| 0, 10).is_empty());
        // A file opened before beats a better name.
        assert_eq!(rank("rep", entries(), |i| if i == 2 { 1000 } else { 0 }, 1), vec![2]);
    }
}
