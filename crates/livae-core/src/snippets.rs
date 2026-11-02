//! Text expansion, as in Espanso: type a trigger anywhere (":date") and it is replaced by its text. Off until turned
//! on (`expansion = true`); the snippets live in `[[snippets]]` in the settings.
//!
//! Each system watches the keys typed in other apps and feeds them to an `Expander`. When the text before the caret
//! ends with a trigger, it erases the trigger and types the snippet. In a snippet, `{date}` and `{time}` are now
//! (`{date:%A}` takes a chrono format), `{clipboard}` is the copied text and `{cursor}` is where the caret ends.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Snippet {
    pub trigger: String,
    pub text: String,
}

fn snippet(trigger: &str, text: &str) -> Snippet {
    Snippet { trigger: trigger.into(), text: text.into() }
}

pub fn defaults() -> Vec<Snippet> {
    vec![snippet(":date", "{date}"), snippet(":time", "{time}"), snippet(":shrug", "¯\\_(ツ)_/¯")]
}

/// What to do in the app: erase `erase` characters (the trigger), type `text`, then move the caret `left` characters
/// back (for `{cursor}`).
#[derive(Clone, Debug, PartialEq)]
pub struct Expansion {
    pub erase: usize,
    pub text: String,
    pub left: usize,
}

/// The characters typed since the caret last jumped, kept short.
#[derive(Debug, Default)]
pub struct Expander {
    typed: String,
}

const KEEP: usize = 64;

impl Expander {
    /// A character was typed. Returns the expansion when the typed text now ends with a trigger (the longest wins).
    pub fn typed(&mut self, c: char, snippets: &[Snippet], clipboard: impl FnOnce() -> Option<String>) -> Option<Expansion> {
        if c.is_control() {
            self.reset();
            return None;
        }
        self.typed.push(c);
        if self.typed.chars().count() > KEEP {
            let cut = self.typed.char_indices().nth(self.typed.chars().count() - KEEP).map_or(0, |(i, _)| i);
            self.typed.drain(..cut);
        }
        let s = snippets.iter().filter(|s| !s.trigger.is_empty() && self.typed.ends_with(&s.trigger)).max_by_key(|s| s.trigger.len())?;
        let (text, left) = render(&s.text, chrono::Local::now().naive_local(), clipboard);
        self.reset();
        Some(Expansion { erase: s.trigger.chars().count(), text, left })
    }

    pub fn backspace(&mut self) {
        self.typed.pop();
    }

    /// The caret moved or the app changed: what came before no longer leads up to it.
    pub fn reset(&mut self) {
        self.typed.clear();
    }
}

/// The snippet's text with its placeholders filled in, and how far `{cursor}` is from the end (in characters).
pub fn render(text: &str, now: chrono::NaiveDateTime, clipboard: impl FnOnce() -> Option<String>) -> (String, usize) {
    let mut out = String::new();
    let mut cursor = None;
    let mut clipboard = Some(clipboard);
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let (name, format) = after[..close].split_once(':').map_or((&after[..close], None), |(n, f)| (n, Some(f)));
        match name {
            "date" => out.push_str(&now.format(format.unwrap_or("%d.%m.%Y")).to_string()),
            "time" => out.push_str(&now.format(format.unwrap_or("%H:%M")).to_string()),
            "clipboard" => out.push_str(&clipboard.take().and_then(|f| f()).unwrap_or_default()),
            "cursor" => cursor = Some(out.chars().count()),
            // Not ours: keep it as written.
            _ => out.push_str(&rest[open..open + close + 2]),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    let left = cursor.map_or(0, |at| out.chars().count() - at);
    (out, left)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_all(e: &mut Expander, text: &str, snippets: &[Snippet]) -> Option<Expansion> {
        text.chars().fold(None, |_, c| e.typed(c, snippets, || Some("copied".into())))
    }

    #[test]
    fn trigger_at_the_end_expands_and_the_longest_wins() {
        let snippets = vec![snippet(";e", "e@x.com"), snippet(";em", "Kind regards"), snippet(":shrug", "¯\\_(ツ)_/¯")];
        let mut e = Expander::default();
        assert_eq!(type_all(&mut e, "write ;e", &snippets).map(|x| (x.erase, x.text)), Some((2, "e@x.com".into())));
        assert_eq!(type_all(&mut e, ";em", &snippets), None, "';e' fired first, so ';em' starts over");
        e.reset();
        assert_eq!(type_all(&mut e, "a :shrug", &snippets).map(|x| x.erase), Some(6));
        // A typo fixed with backspace still counts; a control key starts over.
        assert!(type_all(&mut e, ":shrx", &snippets).is_none());
        e.backspace();
        assert!(type_all(&mut e, "ug", &snippets).is_some());
        assert!(type_all(&mut e, ":shr", &snippets).is_none());
        assert!(e.typed('\n', &snippets, || None).is_none());
        assert!(type_all(&mut e, "ug", &snippets).is_none());
    }

    #[test]
    fn placeholders() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap().and_hms_opt(9, 5, 0).unwrap();
        assert_eq!(render("{date} {time}", now, || None), ("02.11.2026 09:05".into(), 0));
        assert_eq!(render("{date:%Y-%m-%d}", now, || None).0, "2026-11-02");
        assert_eq!(render("Hi {cursor}, see {clipboard}", now, || Some("this".into())), ("Hi , see this".into(), 10));
        assert_eq!(render("{name} and {", now, || None).0, "{name} and {");
    }

    #[test]
    fn long_typing_stays_short() {
        let mut e = Expander::default();
        let snippets = defaults();
        for _ in 0..500 {
            e.typed('é', &snippets, || None);
        }
        assert_eq!(e.typed.chars().count(), KEEP);
        assert!(type_all(&mut e, ":time", &snippets).is_some());
    }
}
