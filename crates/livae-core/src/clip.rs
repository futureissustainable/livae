//! What kind of thing a copied text is: a link, a colour, code, or plain text.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextKind {
    Text,
    Code,
    Link,
    Color,
}

/// Apps whose copies lean towards code.
pub const TERMINALS: [&str; 9] = ["Ghostty", "Terminal", "iTerm2", "Warp", "Code", "Cursor", "Zed", "Xcode", "Visual Studio Code"];

pub fn is_color(t: &str) -> bool {
    let Some(h) = t.strip_prefix('#') else { return false };
    matches!(h.len(), 3 | 6 | 8) && h.chars().all(|c| c.is_ascii_hexdigit())
}

/// sRGB 0..1 of "#rgb", "#rrggbb" or "#rrggbbaa".
pub fn color_of(t: &str) -> Option<(f64, f64, f64)> {
    if !is_color(t) {
        return None;
    }
    let h = &t[1..];
    let h: String = if h.len() == 3 { h.chars().flat_map(|c| [c, c]).collect() } else { h[..6].to_string() };
    let v = u32::from_str_radix(&h, 16).ok()?;
    Some((((v >> 16) & 255) as f64 / 255.0, ((v >> 8) & 255) as f64 / 255.0, (v & 255) as f64 / 255.0))
}

/// Code reads as code: commands, symbols, few sentences. Prose copied from a terminal stays text.
fn looks_like_code(t: &str, app: &str) -> bool {
    // Links are symbol-heavy but are not code: count symbols without them.
    let body: String = t.split_whitespace().filter(|w| !w.contains("://")).collect::<Vec<_>>().join(" ");
    let symbols = body.chars().filter(|c| "{}[]();=<>$|&/\\_:`".contains(*c)).count();
    let words = t.split_whitespace().count().max(1);
    // Sentence ends, not commas (code is full of commas).
    let sentences = t.matches(". ").count() + t.matches("? ").count() + t.matches("! ").count();
    let first = t.split_whitespace().next().unwrap_or("");
    const COMMANDS: [&str; 24] = [
        "cd", "ls", "git", "cargo", "npm", "npx", "pnpm", "brew", "ssh", "scp", "rsync", "python3", "python", "node", "sudo", "curl",
        "docker", "make", "bash", "sh", "echo", "export", "kill", "open",
    ];
    if COMMANDS.contains(&first) && !t.contains(". ") {
        return true;
    }
    let dense = symbols as f64 / words as f64;
    (dense > 0.6 && sentences * 10 <= words) || (TERMINALS.contains(&app) && dense > 0.3 && sentences == 0)
}

pub fn classify(text: &str, app: &str) -> TextKind {
    let t = text.trim();
    if (t.starts_with("https://") || t.starts_with("http://")) && !t.contains(char::is_whitespace) {
        TextKind::Link
    } else if is_color(t) {
        TextKind::Color
    } else if looks_like_code(t, app) {
        TextKind::Code
    } else {
        TextKind::Text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(classify("https://livae.com/a?b=1", ""), TextKind::Link);
        assert_eq!(classify("see https://livae.com", ""), TextKind::Text);
        assert_eq!(classify("#B07F53", ""), TextKind::Color);
        assert_eq!(classify("#fff", ""), TextKind::Color);
        assert_eq!(classify("B07F53", ""), TextKind::Text);
        assert_eq!(classify("cargo build --release -p livae-mac", ""), TextKind::Code);
        assert_eq!(classify("fn main() {\n    println!(\"{}\", x);\n}", ""), TextKind::Code);
        // Prose copied from a terminal is still prose (the 31.10.2026 bug).
        let prose = "**1. Framer, \"That's Framer\"** (38 s, 33M YouTube views). My pick. One continuous camera move flies through the canvas.";
        assert_eq!(classify(prose, "Ghostty"), TextKind::Text);
        assert_eq!(classify("Come in every single day and just try to do a little better.", ""), TextKind::Text);
        assert_eq!(color_of("#B07F53").map(|c| (c.0 * 255.0).round() as u8), Some(176));
    }
}
