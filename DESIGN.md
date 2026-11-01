# Livae design

Livae follows the Mateus Muste design system (v2). Its values are generated into `crates/livae-core/src/tokens.rs`; change them there only through the generator, not by hand. This file lists what Livae takes from the system and what it adds.

## Taken from the system

- **Type:** the system font (SF on the Mac, Segoe UI Variable on Windows) for letters; Zen Gate shows digits and punctuation in Departure Mono (SIL Open Font License). Copied code uses a plain monospace (`--mm-font-code`). Styles: caption 12 (text colour only), label 14, body 16, title 20; break quote h3 28. Secondary text only from 14 px. Caps labels tracking .05em.
- **Colour:** text `#e6e8ea`, secondary `#cdd0d6`, tertiary `#a1a4aa` (placeholders), true black. Surfaces `#111111` / `#191a1b` / `#242423`, lines `#171717` / `#262626`, divider `#5a5a5a`, field underline `#7c7c7c`, focus `#9b9b9b`. Primary action `#f4f3f6` with `#14171c` text. Over glass, surfaces and lines are drawn as their white-over-black equivalents (6.7%, 10%, 14%; 9%, 15%, 40%).
- **Spacing:** 4, 8, 12, 16, 24, 32, 48. Hit target 44.
- **Corners (G2):** control 7.10, menu 11.36, panel 17.04. Circles stay circles.
- **Controls:** ordinary = transparent with the `#262626` edge; primary = `#f4f3f6`; disabled = 45%. Editable fields: surface, quiet edges, the stronger line only on the bottom edge. One emphasis cue per state: selection is a surface change.
- **Shadow:** `0 4px 10px -4px #0003`, floating panels only. **Motion:** 160 / 200 / 240 ms, the system's curves, press scale .97.

## Added for Livae

1. The macOS 27 Spotlight shape: a glass search capsule (radius = half its height) and a separate glass card for results, 10 pt apart, in one `NSGlassEffectContainerView`. Apple's glass untinted (regular style); it adapts to what is behind. Panels are Apple glass (`NSGlassEffectView`); on Windows, a blurred copy of the screen behind the panel plus the same black wash (Windows 11's own light blur no longer draws, and acrylic turns black to grey). Controls inside a panel are fills, never glass on glass. A control floating over the desktop (Skip) may be glass.
2. Launcher and clipboard: field row 56 (16 + 24 + 16), rows 44, inset 8, edge 16, one text column at 52.
3. The clipboard opens at the text cursor, as one list (no split preview), and always sits over the text being written: it uses a 50% black tint so nothing behind it stays readable.
4. Nothing repeats in a view: the return glyph shows only on the selected row. Meaning sits next to its object: the note counter is inside the note box; facts sit under the copied item.
5. Icons are SF Symbols on the Mac and Segoe Fluent Icons on Windows.
