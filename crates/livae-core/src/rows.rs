//! The launcher's own rows beside apps and files: commands, quicklinks and snippets that match the query, each with
//! its match score, so each system merges them with its app rows in one order.

use crate::commands::{self, Def};
use crate::quicklinks::{self, Hit, Quicklink};
use crate::search::rank_words_scored;
use crate::settings::Platform;
use crate::snippets::Snippet;

pub enum Extra {
    Command(&'static Def),
    Quick(Hit),
    /// An index into the snippets.
    Snippet(usize),
}

/// A few of each kind, scored like `search::rank_words_scored`. `skip_link` is a quicklink already shown as a keyword
/// search ("gh livae"). `used` boosts a command by how often it ran; a typed-out command (3 letters or more) wins a
/// tie with an app or a settings page of the same name ("Lock Screen").
pub fn extras(q: &str, platform: Platform, links: &[Quicklink], snippets: &[Snippet], skip_link: Option<usize>, used: impl Fn(&str) -> u32) -> Vec<(u32, Extra)> {
    let defs: Vec<&'static Def> = commands::all(platform).collect();
    let names: Vec<(&str, &str)> = defs.iter().map(|d| (d.name, d.keywords)).collect();
    let edge = u32::from(q.trim().chars().count() >= 3);
    let mut out: Vec<(u32, Extra)> =
        rank_words_scored(q, &names, |i| edge + used(defs[i].name)).into_iter().take(4).map(|(sc, i)| (sc, Extra::Command(defs[i]))).collect();
    let found = rank_words_scored(q, &quicklinks::search_names(links), |_| 0).into_iter().filter(|&(_, i)| Some(i) != skip_link);
    out.extend(found.take(3).map(|(sc, i)| (sc, Extra::Quick(quicklinks::named_hit(i, &links[i])))));
    let snips: Vec<(&str, &str)> = snippets.iter().map(|s| (s.trigger.as_str(), s.text.as_str())).collect();
    out.extend(rank_words_scored(q, &snips, |_| 0).into_iter().take(3).map(|(sc, i)| (sc, Extra::Snippet(i))));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_links_and_snippets_found_together() {
        let (links, snippets) = (quicklinks::defaults(), crate::snippets::defaults());
        let kinds = |q: &str, skip| -> Vec<&'static str> {
            extras(q, Platform::Mac, &links, &snippets, skip, |_| 0)
                .into_iter()
                .map(|(_, e)| match e {
                    Extra::Command(d) => d.id,
                    Extra::Quick(_) => "link",
                    Extra::Snippet(_) => "snippet",
                })
                .collect()
        };
        assert_eq!(kinds("left half", None).first(), Some(&"left-half"));
        assert!(kinds("github", None).contains(&"link"));
        assert!(!kinds("github", Some(1)).contains(&"link"));
        assert!(kinds("shrug", None).contains(&"snippet"));
        assert!(kinds("left", None).len() <= 10);
    }
}
