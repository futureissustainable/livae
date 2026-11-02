//! Spring panels, reversible focus pulls and the Windows motion preference.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use livae_core::{motion, tokens};
use windows::core::BOOL;
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};

pub const FRAME_TIMER: usize = 0x4D01;
pub const BAND_TIMER: usize = 0x4D02;

pub fn animations() -> bool {
    let mut on = BOOL(1);
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut BOOL as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
    };
    ok.is_err() || on.as_bool()
}

pub fn slow() -> f32 {
    static FACTOR: OnceLock<f32> = OnceLock::new();
    *FACTOR.get_or_init(|| {
        std::env::var("LIVAE_MOTION_SLOW").ok().and_then(|v| v.parse::<f32>().ok()).filter(|n| n.is_finite() && *n >= 1.0).unwrap_or(1.0)
    })
}

pub fn duration(ms: u64) -> Duration {
    Duration::from_secs_f64((ms as f64 * slow() as f64 / 1000.0).min(Duration::MAX.as_secs_f64() / 2.0))
}

/// Invert the curve's x coordinate before evaluating y.
pub fn ease(p: f32, curve: [f64; 4]) -> f32 {
    let [x1, y1, x2, y2] = curve;
    let cubic = |t: f64, a: f64, b: f64| 3.0 * (1.0 - t).powi(2) * t * a + 3.0 * (1.0 - t) * t * t * b + t.powi(3);
    let p = p.clamp(0.0, 1.0) as f64;
    if p == 0.0 || p == 1.0 {
        return p as f32;
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..20 {
        let t = (lo + hi) / 2.0;
        if cubic(t, x1, x2) < p { lo = t } else { hi = t }
    }
    cubic((lo + hi) / 2.0, y1, y2) as f32
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Family {
    Drop,
    Clipboard,
    Card,
    Bloom,
}

impl Family {
    fn spring(self) -> motion::Spring {
        if self == Self::Bloom { motion::BLOOM } else { motion::DROP }
    }

    fn pose(self, anchor: (f32, f32), spatial: bool, entering: bool) -> Frame {
        let mut frame = Frame { opacity: 0.0, ..Frame::IDENTITY };
        if !spatial {
            return frame;
        }
        frame.blur = if entering { motion::FOCUS_BLUR } else { motion::EXIT_BLUR } as f32;
        match self {
            Self::Drop | Self::Clipboard => {
                frame.y = -if entering {
                    if self == Self::Clipboard { motion::CLIP_DROP_Y } else { motion::DROP_Y }
                } else { motion::EXIT_LIFT } as f32;
                if entering && self == Self::Clipboard {
                    frame.sx = motion::CARET_SCALE as f32;
                    frame.sy = frame.sx;
                }
            }
            Self::Card => {
                frame.sy = if entering { motion::UNFOLD_FROM as f32 } else { 0.85 };
            }
            Self::Bloom => {
                frame.sx = if entering { motion::BLOOM_FROM } else { motion::EXIT_SCALE } as f32;
                frame.sy = frame.sx;
            }
        }
        frame.x += anchor.0 * (1.0 - frame.sx);
        frame.y += anchor.1 * (1.0 - frame.sy);
        frame
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub opacity: f32,
    pub sx: f32,
    pub sy: f32,
    pub x: f32,
    pub y: f32,
    pub blur: f32,
}

impl Frame {
    pub const IDENTITY: Self = Self { opacity: 1.0, sx: 1.0, sy: 1.0, x: 0.0, y: 0.0, blur: 0.0 };

    fn mix(self, to: Self, spatial: f32, focus: f32) -> Self {
        let lerp = |a, b, p| a + (b - a) * p;
        Self {
            opacity: lerp(self.opacity, to.opacity, focus),
            sx: lerp(self.sx, to.sx, spatial), sy: lerp(self.sy, to.sy, spatial),
            x: lerp(self.x, to.x, spatial), y: lerp(self.y, to.y, spatial),
            blur: lerp(self.blur, to.blur, focus),
        }
    }
}

/// Row delay uses the focus duration, with each following row one stagger later.
fn row_delay(index: usize) -> f64 {
    (motion::FOCUS_MS as f64 / 3.0 + index as f64 * motion::STAGGER_MS as f64) / 1000.0
}

fn row_settle() -> f64 {
    static SECONDS: OnceLock<f64> = OnceLock::new();
    *SECONDS.get_or_init(|| motion::ROW.settle())
}

#[derive(Clone, Copy)]
pub struct Progress {
    start: Instant,
    from: Frame,
    family: Family,
    settle: f64,
    anchor: (f32, f32),
    spatial: bool,
    pub showing: bool,
    pending: bool,
    running: bool,
    rows: usize,
    row_time: f64,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            start: Instant::now(), from: Frame::IDENTITY, family: Family::Bloom, settle: 0.0, anchor: (0.0, 0.0),
            spatial: false, showing: true, pending: false, running: false, rows: 0, row_time: 0.0,
        }
    }
}

impl Progress {
    fn elapsed(&self, now: Instant) -> f64 {
        if self.pending { 0.0 } else { now.saturating_duration_since(self.start).as_secs_f64() / slow() as f64 }
    }

    pub fn frame(&self, now: Instant) -> Frame {
        if !self.running {
            return Frame::IDENTITY;
        }
        let t = self.elapsed(now);
        if self.showing {
            let focus = ease((t * 1000.0 / motion::FOCUS_MS as f64) as f32, motion::EASE_FOCUS);
            let spring = self.family.spring();
            let p = if t >= self.settle || !self.spatial { 1.0 } else { spring.at(t) as f32 };
            self.from.mix(Frame::IDENTITY, p, focus)
        } else {
            let p = ease((t * 1000.0 / motion::EXIT_MS as f64) as f32, motion::EASE_EXIT);
            self.from.mix(self.family.pose(self.anchor, self.spatial, false), p, p)
        }
    }

    fn rows_elapsed(&self, now: Instant) -> f64 {
        self.row_time + if self.showing && self.running { self.elapsed(now) } else { 0.0 }
    }

    /// Rows stay in their current pose during an exit and continue on reversal.
    pub fn row(&self, index: usize, now: Instant) -> Frame {
        if !self.running || self.family != Family::Card {
            return Frame::IDENTITY;
        }
        let t = self.rows_elapsed(now) - if self.spatial { row_delay(index) } else { 0.0 };
        let focus = ease((t * 1000.0 / motion::FOCUS_MS as f64) as f32, motion::EASE_FOCUS);
        let y = if !self.spatial || t >= row_settle() { 0.0 } else { -motion::ROW_DROP as f32 * (1.0 - motion::ROW.at(t.max(0.0)) as f32) };
        Frame { opacity: focus, y, ..Frame::IDENTITY }
    }

    pub fn set_rows(&mut self, rows: usize) {
        self.rows = rows;
    }

    pub fn active(&self, now: Instant) -> bool {
        if !self.running {
            return false;
        }
        let end = if self.showing {
            let panel = (motion::FOCUS_MS as f64 / 1000.0).max(if self.spatial { self.settle } else { 0.0 });
            if self.family == Family::Card && self.rows > 0 {
                let rows = if self.spatial { row_delay(self.rows - 1) + row_settle().max(motion::FOCUS_MS as f64 / 1000.0) } else { motion::FOCUS_MS as f64 / 1000.0 };
                panel.max(rows - self.row_time)
            } else { panel }
        } else { motion::EXIT_MS as f64 / 1000.0 };
        self.pending || self.elapsed(now) < end
    }

    /// Start the clock on the first paint, while DWM still cloaks a new window.
    pub fn draw_frame(&mut self, now: Instant) -> Frame {
        if self.pending {
            self.start = now;
            self.pending = false;
        }
        self.frame(now)
    }

    pub fn enter(&mut self, family: Family, anchor: (f32, f32), spatial: bool, visible: bool, now: Instant) {
        if visible && self.showing {
            return;
        }
        let from = if visible { self.frame(now) } else { family.pose(anchor, spatial, true) };
        self.row_time = if visible { self.rows_elapsed(now) } else { 0.0 };
        self.start = now;
        self.from = from;
        self.family = family;
        self.settle = family.spring().settle();
        self.anchor = anchor;
        self.spatial = spatial;
        self.showing = true;
        self.pending = !visible;
        self.running = true;
    }

    pub fn exit(&mut self, anchor: (f32, f32), now: Instant) {
        if !self.showing {
            return;
        }
        self.from = self.frame(now);
        self.row_time = self.rows_elapsed(now);
        self.anchor = anchor;
        self.start = now;
        self.showing = false;
        self.pending = false;
        self.running = true;
    }
}

/// The selected row moves in DIPs; a scroll starts a new coordinate space.
#[derive(Default)]
pub struct Band {
    from: (f32, f32),
    to: (f32, f32),
    start: Option<Instant>,
    first: Option<usize>,
    spatial: bool,
}

impl Band {
    pub fn reset(&mut self) {
        self.first = None;
        self.start = None;
    }

    pub fn at(&mut self, target: (f32, f32), first: usize) -> ((f32, f32), bool) {
        let now = Instant::now();
        if self.first != Some(first) {
            self.from = target;
            self.to = target;
            self.start = None;
            self.first = Some(first);
        } else if self.to != target {
            self.from = self.sample(now).0;
            self.to = target;
            self.start = Some(now);
            self.spatial = animations();
        }
        self.sample(now)
    }

    fn sample(&self, now: Instant) -> ((f32, f32), bool) {
        let Some(start) = self.start.filter(|_| self.spatial) else { return (self.to, false) };
        let p = now.saturating_duration_since(start).as_secs_f64() / duration(tokens::motion::PRESS).as_secs_f64();
        let e = ease(p as f32, tokens::motion::EASE_OUT);
        ((self.from.0 + (self.to.0 - self.from.0) * e, self.from.1 + (self.to.1 - self.from.1) * e), p < 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_uses_the_time_coordinate() {
        assert!((ease(0.2975, motion::EASE_FOCUS) - 0.875).abs() < 0.00001);
        assert!((ease(0.65, motion::EASE_EXIT) - 0.5).abs() < 0.00001);
        assert_eq!(ease(0.0, motion::EASE_FOCUS), 0.0);
        assert_eq!(ease(1.0, motion::EASE_EXIT), 1.0);
    }

    #[test]
    fn reversal_keeps_the_frame_rows_and_final_visibility() {
        let start = Instant::now();
        for family in [Family::Drop, Family::Clipboard, Family::Card, Family::Bloom] {
            let mut p = Progress::default();
            p.set_rows(8);
            p.enter(family, (320.0, 200.0), true, false, start);
            let first = start + Duration::from_secs(1);
            assert_eq!(p.frame(first), p.draw_frame(first));
            let exiting = first + duration(motion::FOCUS_MS) / 2;
            let frame = p.frame(exiting);
            let row = p.row(2, exiting);
            p.exit((320.0, 200.0), exiting);
            assert_eq!(p.frame(exiting), frame);
            assert_eq!(p.row(2, exiting), row);
            let reopening = exiting + duration(motion::EXIT_MS) / 2;
            let frame = p.frame(reopening);
            let row = p.row(2, reopening);
            p.enter(family, (320.0, 200.0), true, true, reopening);
            assert_eq!(p.frame(reopening), frame);
            assert_eq!(p.row(2, reopening), row);
            let end = reopening + Duration::from_secs_f64(slow() as f64 * 2.0);
            assert!(p.showing && !p.active(end));
            assert_eq!(p.frame(end), Frame::IDENTITY);
            p.exit((320.0, 200.0), end);
            p.exit((320.0, 200.0), end + duration(motion::EXIT_MS) / 2);
            let hidden = end + duration(motion::EXIT_MS);
            assert!(!p.showing && !p.active(hidden));
            assert_eq!(p.frame(hidden).opacity, 0.0);
        }
    }

    #[test]
    fn springs_overshoot_and_rows_wait_their_turn() {
        let start = Instant::now();
        let mut p = Progress::default();
        p.set_rows(8);
        for family in [Family::Drop, Family::Clipboard, Family::Card, Family::Bloom] {
            p.enter(family, (0.0, 0.0), true, false, start);
            p.draw_frame(start);
            assert!((0..1000).any(|ms| {
                let f = p.frame(start + duration(ms));
                f.sx > 1.0 || f.sy > 1.0 || f.y > 0.0
            }));
        }
        p.enter(Family::Card, (0.0, 0.0), true, false, start);
        p.draw_frame(start);
        let now = start + duration(motion::FOCUS_MS / 3 + motion::STAGGER_MS);
        assert!(p.row(0, now).opacity > 0.0);
        assert_eq!(p.row(2, now).opacity, 0.0);
        assert!(p.active(start + duration(motion::FOCUS_MS)));
    }

    #[test]
    fn reduced_motion_only_fades_even_on_reversal() {
        let start = Instant::now();
        for family in [Family::Drop, Family::Clipboard, Family::Card, Family::Bloom] {
            let mut p = Progress::default();
            p.enter(family, (320.0, 200.0), false, false, start);
            p.draw_frame(start);
            let halfway = start + duration(motion::FOCUS_MS) / 2;
            let frame = p.frame(halfway);
            assert!(frame.opacity > 0.0 && frame.opacity < 1.0);
            assert_eq!(Frame { opacity: 1.0, ..frame }, Frame::IDENTITY);
            p.exit((320.0, 200.0), halfway);
            let frame = p.frame(halfway + duration(motion::EXIT_MS) / 2);
            assert_eq!(Frame { opacity: 1.0, ..frame }, Frame::IDENTITY);
            assert_eq!(p.row(0, start).y, 0.0);
        }
    }
}
