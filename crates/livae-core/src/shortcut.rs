//! Shortcut text from the settings ("Hyper+Space", "Ctrl+Shift+V") as modifiers and a key, the same words on
//! both systems. Each system says what Hyper means and maps the result to its own hotkey type.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Cmd on the Mac, the Windows key on the PC.
    pub meta: bool,
}

/// Cmd+Ctrl+Opt+Shift: what Caps Lock sends through Karabiner.
pub const HYPER_MAC: Mods = Mods { ctrl: true, alt: true, shift: true, meta: true };
/// Ctrl+Alt+Shift: what Caps Lock sends through a remapping script such as AutoHotkey (no Windows key, which the shell keeps for itself).
pub const HYPER_WINDOWS: Mods = Mods { ctrl: true, alt: true, shift: true, meta: false };

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Space,
    /// Upper case A-Z.
    Letter(char),
    Digit(u8),
    Left,
    Right,
    Up,
    Down,
    Return,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub mods: Mods,
    pub key: Key,
}

/// None for text without a key or without a modifier (a bare letter would swallow typing everywhere).
pub fn parse(text: &str, hyper: Mods) -> Option<Shortcut> {
    let mut m = Mods::default();
    let mut key = None;
    for part in text.split('+').map(|p| p.trim().to_lowercase()) {
        match part.as_str() {
            "hyper" => {
                m.ctrl |= hyper.ctrl;
                m.alt |= hyper.alt;
                m.shift |= hyper.shift;
                m.meta |= hyper.meta;
            }
            "cmd" | "command" | "win" | "super" | "meta" => m.meta = true,
            "ctrl" | "control" => m.ctrl = true,
            "opt" | "option" | "alt" => m.alt = true,
            "shift" => m.shift = true,
            "space" => key = Some(Key::Space),
            "left" => key = Some(Key::Left),
            "right" => key = Some(Key::Right),
            "up" => key = Some(Key::Up),
            "down" => key = Some(Key::Down),
            "return" | "enter" => key = Some(Key::Return),
            k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphabetic()) => key = k.chars().next().map(|c| Key::Letter(c.to_ascii_uppercase())),
            k if k.len() == 1 && k.chars().all(|c| c.is_ascii_digit()) => key = k.parse().ok().map(Key::Digit),
            _ => return None,
        }
    }
    let any = m.ctrl || m.alt || m.shift || m.meta;
    let key = key?;
    any.then_some(Shortcut { mods: m, key })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyper_means_each_systems_modifiers() {
        let s = parse("Hyper+Space", HYPER_WINDOWS).unwrap();
        assert_eq!((s.mods, s.key), (HYPER_WINDOWS, Key::Space));
        assert_eq!(parse("hyper + v", HYPER_MAC).unwrap(), Shortcut { mods: HYPER_MAC, key: Key::Letter('V') });
        let s = parse("Ctrl+Shift+1", HYPER_WINDOWS).unwrap();
        assert_eq!((s.mods.ctrl, s.mods.shift, s.mods.alt, s.key), (true, true, false, Key::Digit(1)));
        assert!(parse("V", HYPER_WINDOWS).is_none());
        assert!(parse("Hyper", HYPER_WINDOWS).is_none());
        assert!(parse("Hyper+F13", HYPER_WINDOWS).is_none());
        assert_eq!(parse("Ctrl+Opt+Left", HYPER_MAC).map(|s| s.key), Some(Key::Left));
        assert!(parse("Left", HYPER_MAC).is_none());
    }
}
