//! Quicklinks: a keyword, then what to look for ("gh livae" searches GitHub). A link without `{query}` in its address
//! just opens it ("mail" opens the inbox). They live in `[[quicklinks]]` in the settings.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Quicklink {
    pub name: String,
    pub keyword: String,
    /// The address; `{query}` is replaced by the typed words, encoded for a URL.
    pub url: String,
}

fn link(name: &str, keyword: &str, url: &str) -> Quicklink {
    Quicklink { name: name.into(), keyword: keyword.into(), url: url.into() }
}

pub fn defaults() -> Vec<Quicklink> {
    vec![
        link("Google", "g", "https://www.google.com/search?q={query}"),
        link("GitHub", "gh", "https://github.com/search?q={query}"),
        link("YouTube", "yt", "https://www.youtube.com/results?search_query={query}"),
        link("Wikipedia", "w", "https://en.wikipedia.org/wiki/Special:Search?search={query}"),
        link("Maps", "maps", "https://www.google.com/maps/search/{query}"),
        link("Translate", "tr", "https://translate.google.com/?text={query}"),
    ]
}

/// Percent-encoding for a URL query: letters, digits and `-._~` stay, everything else becomes %XX.
pub fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

impl Quicklink {
    pub fn wants_query(&self) -> bool {
        self.url.contains("{query}")
    }

    pub fn address(&self, query: &str) -> String {
        self.url.replace("{query}", &encode(query.trim()))
    }
}

/// What a launcher row for a quicklink does on Enter.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    /// "gh livae": open this address. The title is "Search GitHub for “livae”".
    Open { index: usize, title: String, url: String },
    /// A search link picked without words: put "gh " in the field so the words can follow.
    Fill { index: usize, title: String, text: String },
}

impl Hit {
    /// The quicklink this row is for.
    pub fn index(&self) -> usize {
        match self {
            Hit::Open { index, .. } | Hit::Fill { index, .. } => *index,
        }
    }
}

/// The row for a query that starts with a keyword and a space ("gh livae"), shown first in the launcher.
pub fn keyword_hit(query: &str, links: &[Quicklink]) -> Option<Hit> {
    let (kw, rest) = query.trim_start().split_once(' ')?;
    let (index, l) = links.iter().enumerate().find(|(_, l)| !l.keyword.is_empty() && l.keyword.eq_ignore_ascii_case(kw))?;
    let rest = rest.trim();
    if !l.wants_query() || rest.is_empty() {
        return None;
    }
    Some(Hit::Open { index, title: format!("Search {} for \u{201c}{rest}\u{201d}", l.name), url: l.address(rest) })
}

/// The row for a quicklink found by its name or keyword ("git" finds GitHub).
pub fn named_hit(index: usize, l: &Quicklink) -> Hit {
    if l.wants_query() {
        Hit::Fill { index, title: format!("Search {}", l.name), text: format!("{} ", l.keyword) }
    } else {
        Hit::Open { index, title: l.name.clone(), url: l.url.clone() }
    }
}

/// `(name, keyword)` pairs for `search::rank_words`, so a quicklink is found by either.
pub fn search_names(links: &[Quicklink]) -> Vec<(&str, &str)> {
    links.iter().map(|l| (l.name.as_str(), l.keyword.as_str())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_then_words_opens_the_search() {
        let links = defaults();
        let Some(Hit::Open { title, url, .. }) = keyword_hit("gh livae app", &links) else { panic!() };
        assert_eq!(title, "Search GitHub for \u{201c}livae app\u{201d}");
        assert_eq!(url, "https://github.com/search?q=livae%20app");
        assert_eq!(keyword_hit("GH  x&y", &links).map(|h| matches!(h, Hit::Open { url, .. } if url.ends_with("x%26y"))), Some(true));
        // A keyword alone, or a word that only starts like one, is a normal search.
        assert_eq!(keyword_hit("gh ", &links), None);
        assert_eq!(keyword_hit("ghost town", &links), None);
    }

    #[test]
    fn named_links_open_or_ask_for_words() {
        let mail = link("Mail", "mail", "https://mail.google.com");
        assert_eq!(named_hit(3, &mail), Hit::Open { index: 3, title: "Mail".into(), url: "https://mail.google.com".into() });
        assert_eq!(named_hit(1, &defaults()[1]), Hit::Fill { index: 1, title: "Search GitHub".into(), text: "gh ".into() });
        assert_eq!(keyword_hit("mail inbox", &[mail]), None);
        assert_eq!(encode("é ok"), "%C3%A9%20ok");
    }
}
