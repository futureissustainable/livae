//! Livae's motion: one set of values for every surface on every system, so it all moves like one brand.
//!
//! - The search and the clipboard drop into place on a spring and come into focus (C + B): the capsule falls
//!   `DROP_Y` onto its place, the list unfolds from `UNFOLD_FROM` of its height, and each row falls `ROW_DROP`
//!   while it sharpens from `ROW_BLUR`, `STAGGER_MS` after the one above.
//! - Everything else blooms into focus (A + B): windows, the toast, the break warning and Livae for Zen's gate grow
//!   from `BLOOM_FROM` with a soft overshoot while they sharpen from `FOCUS_BLUR`; the gate's words rise `WORD_RISE`.
//! - Every exit takes `EXIT_MS` with `EASE_EXIT`, blurring to `EXIT_BLUR`: the search and the clipboard lift
//!   `EXIT_LIFT`, the rest shrink to `EXIT_SCALE`.
//!
//! Springs are stiffness and damping with mass 1, the model of Core Animation's `CASpringAnimation`, so the Mac,
//! Windows (`spring`) and the gate's CSS (`css_linear`) follow the same curve. Reduce Motion keeps only the fades.

/// A spring: stiffness and damping, mass 1, starting at rest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub stiffness: f64,
    pub damping: f64,
}

/// The search and clipboard capsule landing: quick, with a small settle (about 4% over).
pub const DROP: Spring = Spring { stiffness: 380.0, damping: 28.0 };
/// Windows, the toast and the gate blooming: slower and softer, the same small settle.
pub const BLOOM: Spring = Spring { stiffness: 170.0, damping: 19.0 };
/// Rows falling into place: a touch livelier (about 8% over).
pub const ROW: Spring = Spring { stiffness: 300.0, damping: 22.0 };

/// Points: how far the capsule falls, the clipboard falls at the text cursor, and each row falls.
pub const DROP_Y: f64 = 28.0;
pub const CLIP_DROP_Y: f64 = 10.0;
pub const ROW_DROP: f64 = 10.0;
/// The list unfolds from this share of its height, from the top.
pub const UNFOLD_FROM: f64 = 0.55;
/// The clipboard starts this size at the text cursor.
pub const CARET_SCALE: f64 = 0.9;
/// Windows, the toast and the gate grow from this size.
pub const BLOOM_FROM: f64 = 0.9;
/// The gate's words rise this far, one after another.
pub const WORD_RISE: f64 = 6.0;

/// Blur radii in points: content coming into focus, each row, and an exit.
pub const FOCUS_BLUR: f64 = 12.0;
pub const ROW_BLUR: f64 = 5.0;
pub const EXIT_BLUR: f64 = 8.0;

/// Milliseconds between rows (and the gate's words); blur to sharp; every exit.
pub const STAGGER_MS: u64 = 26;
pub const FOCUS_MS: u64 = 340;
pub const EXIT_MS: u64 = 160;
/// Points the search and the clipboard lift as they leave; the size the rest shrink to.
pub const EXIT_LIFT: f64 = 12.0;
pub const EXIT_SCALE: f64 = 0.94;

/// Cubic Béziers: coming into focus (an expo ease-out) and leaving (an ease-in).
pub const EASE_FOCUS: [f64; 4] = [0.16, 1.0, 0.3, 1.0];
pub const EASE_EXIT: [f64; 4] = [0.4, 0.0, 1.0, 1.0];

/// A cubic Bézier easing (`EASE_FOCUS`, `EASE_EXIT`) at progress `p` (0 to 1): the curve's x is solved for p, its
/// y returned.
pub fn ease(curve: [f64; 4], p: f64) -> f64 {
    let [x1, y1, x2, y2] = curve;
    let at = |t: f64, a: f64, b: f64| 3.0 * (1.0 - t) * (1.0 - t) * t * a + 3.0 * (1.0 - t) * t * t * b + t * t * t;
    let p = p.clamp(0.0, 1.0);
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..30 {
        let t = (lo + hi) / 2.0;
        if at(t, x1, x2) < p { lo = t } else { hi = t }
    }
    at((lo + hi) / 2.0, y1, y2)
}

impl Spring {
    /// Progress at `t` seconds: 0 at rest, 1 in place; above 1 while it overshoots.
    pub fn at(self, t: f64) -> f64 {
        let w0 = self.stiffness.sqrt();
        let zeta = self.damping / (2.0 * w0);
        if zeta < 1.0 {
            let wd = w0 * (1.0 - zeta * zeta).sqrt();
            1.0 - (-zeta * w0 * t).exp() * ((wd * t).cos() + zeta * w0 / wd * (wd * t).sin())
        } else {
            1.0 - (1.0 + w0 * t) * (-w0 * t).exp()
        }
    }

    /// Seconds until it stays within 0.2% of its place.
    pub fn settle(self) -> f64 {
        let mut t = 0.1;
        while t < 3.0 && ((1.0 - self.at(t)).abs() > 0.002 || (1.0 - self.at(t + 0.05)).abs() > 0.002) {
            t += 0.02;
        }
        t
    }

    /// The same curve as a CSS easing, `linear(…)` with 41 points over `settle()`.
    pub fn css_linear(self) -> String {
        let end = self.settle();
        let points: Vec<String> = (0..=40).map(|i| format!("{:.4}", self.at(end * i as f64 / 40.0))).collect();
        format!("linear({})", points.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn springs_settle_with_their_character() {
        let peak = |s: Spring| (0..3000).map(|i| s.at(i as f64 / 1000.0)).fold(0.0, f64::max);
        // Each overshoots a little (4 to 8%), never springy, and comes to rest in place within a second.
        for s in [DROP, BLOOM, ROW] {
            assert!(peak(s) > 1.02 && peak(s) < 1.08, "{s:?} peaks at {}", peak(s));
            assert!(s.settle() < 1.0 && (s.at(s.settle()) - 1.0).abs() < 0.003);
        }
    }

    #[test]
    fn easing_runs_from_0_to_1() {
        for c in [EASE_FOCUS, EASE_EXIT] {
            assert!(ease(c, 0.0).abs() < 1e-6 && (ease(c, 1.0) - 1.0).abs() < 1e-6);
        }
        // Coming into focus is mostly done early; leaving starts slowly.
        assert!(ease(EASE_FOCUS, 0.3) > 0.8 && ease(EASE_EXIT, 0.3) < 0.15);
    }

    /// Livae for Zen's gate draws its motion in CSS: it must use these same values.
    #[test]
    fn the_gate_moves_with_the_same_values() {
        let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../extensions/livae-for-zen/theme.css")).unwrap();
        for (name, value) in [
            ("--livae-bloom", BLOOM.css_linear()),
            ("--livae-bloom-ms", format!("{}ms", (BLOOM.settle() * 1000.0).round())),
            ("--livae-bloom-from", BLOOM_FROM.to_string()),
            ("--livae-focus-blur", format!("{FOCUS_BLUR}px")),
            ("--livae-row-blur", format!("{ROW_BLUR}px")),
            ("--livae-exit-blur", format!("{EXIT_BLUR}px")),
            ("--livae-word-rise", format!("{WORD_RISE}px")),
            ("--livae-stagger", format!("{STAGGER_MS}ms")),
            ("--livae-focus-ms", format!("{FOCUS_MS}ms")),
            ("--livae-exit-ms", format!("{EXIT_MS}ms")),
            ("--livae-exit-scale", EXIT_SCALE.to_string()),
            ("--livae-ease-focus", format!("cubic-bezier({}, {}, {}, {})", EASE_FOCUS[0], EASE_FOCUS[1], EASE_FOCUS[2], EASE_FOCUS[3])),
            ("--livae-ease-exit", format!("cubic-bezier({}, {}, {}, {})", EASE_EXIT[0], EASE_EXIT[1], EASE_EXIT[2], EASE_EXIT[3])),
        ] {
            let line = css.lines().find(|l| l.trim_start().starts_with(&format!("{name}:"))).unwrap_or_else(|| panic!("{name} missing in theme.css"));
            let got = line.split_once(':').unwrap().1.trim().trim_end_matches(';').trim();
            assert_eq!(got, value, "{name}");
        }
    }
}
