//! A one-line search field: its text, caret and selection, kept apart from how a system draws it.
//! Offsets are bytes on character boundaries; the caret moves by whole graphemes (an emoji is one step).

use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug, Default)]
pub struct Field {
    text: String,
    caret: usize,
    /// The other end of the selection, when there is one.
    anchor: Option<usize>,
}

/// Pasted text stays on one line: line breaks and tabs become spaces, other control characters go.
fn one_line(s: &str) -> String {
    s.replace("\r\n", " ").chars().filter_map(|c| if matches!(c, '\n' | '\r' | '\t') { Some(' ') } else if c.is_control() { None } else { Some(c) }).collect()
}

impl Field {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn set(&mut self, text: &str) {
        self.text = one_line(text);
        self.caret = self.text.len();
        self.anchor = None;
    }

    pub fn clear(&mut self) {
        self.set("");
    }

    /// The selected byte range, start before end.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor.filter(|&a| a != self.caret)?;
        Some((a.min(self.caret), a.max(self.caret)))
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|(a, b)| &self.text[a..b])
    }

    pub fn select_all(&mut self) {
        if !self.text.is_empty() {
            self.anchor = Some(0);
            self.caret = self.text.len();
        }
    }

    fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else { return false };
        self.text.replace_range(a..b, "");
        self.caret = a;
        self.anchor = None;
        true
    }

    /// Typing or pasting: replaces the selection.
    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        let s = one_line(s);
        self.text.insert_str(self.caret, &s);
        self.caret += s.len();
        self.anchor = None;
    }

    pub fn cut(&mut self) -> Option<String> {
        let s = self.selected_text()?.to_string();
        self.delete_selection();
        Some(s)
    }

    pub fn backspace(&mut self, word: bool) {
        if self.delete_selection() || self.caret == 0 {
            return;
        }
        let start = if word { word_start(&self.text, self.caret) } else { prev_boundary(&self.text, self.caret) };
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
    }

    pub fn delete(&mut self, word: bool) {
        if self.delete_selection() || self.caret >= self.text.len() {
            return;
        }
        let end = if word { word_end(&self.text, self.caret) } else { next_boundary(&self.text, self.caret) };
        self.text.replace_range(self.caret..end, "");
    }

    /// Move the caret to `to`; with `extend` the selection grows from where it started.
    fn go(&mut self, to: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.caret);
        } else {
            self.anchor = None;
        }
        self.caret = to;
    }

    pub fn left(&mut self, extend: bool, word: bool) {
        if let (false, Some((a, _))) = (extend, self.selection()) {
            return self.go(a, false); // a selection collapses to its start
        }
        let to = if word { word_start(&self.text, self.caret) } else { prev_boundary(&self.text, self.caret) };
        self.go(to, extend);
    }

    pub fn right(&mut self, extend: bool, word: bool) {
        if let (false, Some((_, b))) = (extend, self.selection()) {
            return self.go(b, false);
        }
        let to = if word { word_end(&self.text, self.caret) } else { next_boundary(&self.text, self.caret) };
        self.go(to, extend);
    }

    pub fn home(&mut self, extend: bool) {
        self.go(0, extend);
    }

    pub fn end(&mut self, extend: bool) {
        self.go(self.text.len(), extend);
    }

    /// A byte offset in UTF-16 units, the unit Windows text layout uses.
    pub fn utf16(&self, byte: usize) -> usize {
        self.text[..byte.min(self.text.len())].encode_utf16().count()
    }
}

fn prev_boundary(text: &str, i: usize) -> usize {
    text[..i].grapheme_indices(true).next_back().map_or(0, |(j, _)| j)
}

fn next_boundary(text: &str, i: usize) -> usize {
    text[i..].graphemes(true).next().map_or(text.len(), |g| i + g.len())
}

/// Back over spaces, then over the word before them.
fn word_start(text: &str, i: usize) -> usize {
    let chars: Vec<(usize, char)> = text[..i].char_indices().collect();
    let mut k = chars.len();
    while k > 0 && chars[k - 1].1.is_whitespace() {
        k -= 1;
    }
    while k > 0 && !chars[k - 1].1.is_whitespace() {
        k -= 1;
    }
    chars.get(k).map_or(i, |c| c.0).min(i)
}

/// Forward over spaces, then over the next word.
fn word_end(text: &str, i: usize) -> usize {
    let mut it = text[i..].char_indices().peekable();
    while it.next_if(|c| c.1.is_whitespace()).is_some() {}
    while it.next_if(|c| !c.1.is_whitespace()).is_some() {}
    it.peek().map_or(text.len(), |c| i + c.0)
}

#[cfg(test)]
mod tests {
    use super::Field;

    #[test]
    fn typing_selection_and_words() {
        let mut f = Field::default();
        f.insert("hello world");
        f.backspace(true);
        assert_eq!(f.text(), "hello ");
        f.insert("wörld");
        f.left(false, false);
        f.left(true, false);
        assert_eq!(f.selected_text(), Some("l"));
        f.insert("L");
        assert_eq!(f.text(), "hello wörLd");
        f.select_all();
        assert_eq!(f.cut().as_deref(), Some("hello wörLd"));
        assert!(f.is_empty());
        // Pasted lines become one line; the caret steps over a whole emoji.
        f.insert("a\r\nb\tc");
        assert_eq!(f.text(), "a b c");
        f.set("x👍🏽");
        f.backspace(false);
        assert_eq!(f.text(), "x");
        f.set("ab cd");
        f.home(false);
        f.delete(true);
        assert_eq!(f.text(), " cd");
        assert_eq!(f.utf16(f.text().len()), 3);
    }
}
