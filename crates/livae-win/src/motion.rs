//! Livae's motion on Windows, with `livae_core::motion` as on the Mac: the search and the clipboard fall onto their
//! place while their rows fall in and everything sharpens; every other window's contents grow into focus inside a
//! glass that stays put. Exits reverse from wherever the motion is. Plus the Windows motion preference.

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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Family {
    /// The search: its panel falls `DROP_Y` onto its place and lifts `EXIT_LIFT` as it leaves.
    Drop,
    /// The clipboard: its panel falls `CLIP_DROP_Y` at the text cursor.
    Clipboard,
    /// A panel's list: it moves with its capsule, and its rows fall in one after another.
    Card,
    /// Every other window: its contents grow from `BLOOM_FROM` inside a glass that stays put.
    Bloom,
}

impl Family {
    fn spring(self) -> motion::Spring {
        if self == Self::Bloom { motion::BLOOM } else { motion::DROP }
    }

    /// Where it comes from (`entering`) or goes: clear and blurred; a panel above its place, a bloom smaller.
    fn pose(self, spatial: bool, entering: bool) -> Frame {
        let mut frame = Frame { opacity: 0.0, ..Frame::IDENTITY };
        if !spatial {
            return frame;
        }
        frame.blur = if entering { motion::FOCUS_BLUR } else { motion::EXIT_BLUR } as f32;
        match (self, entering) {
            (Self::Drop, true) => frame.y = -motion::DROP_Y as f32,
            (Self::Clipboard, true) => frame.y = -motion::CLIP_DROP_Y as f32,
            (Self::Drop | Self::Clipboard, false) => frame.y = -motion::EXIT_LIFT as f32,
            (Self::Bloom, _) => frame.scale = if entering { motion::BLOOM_FROM } else { motion::EXIT_SCALE } as f32,
            (Self::Card, _) => {}
        }
        frame
    }
}

/// One moment of a window's motion: its drawing's opacity and blur, the size of its contents around their centre
/// (the glass stays whole), and for a panel's capsule `y`, how far the panel's windows sit below their place
/// (DIPs, negative above); a row's `y` is its fall inside the list.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub opacity: f32,
    pub scale: f32,
    pub y: f32,
    pub blur: f32,
}

impl Frame {
    pub const IDENTITY: Self = Self { opacity: 1.0, scale: 1.0, y: 0.0, blur: 0.0 };

    fn mix(self, to: Self, spatial: f32, focus: f32) -> Self {
        let lerp = |a, b, p| a + (b - a) * p;
        Self {
            opacity: lerp(self.opacity, to.opacity, focus),
            scale: lerp(self.scale, to.scale, spatial),
            y: lerp(self.y, to.y, spatial),
            blur: lerp(self.blur, to.blur, focus),
        }
    }
}

fn eased(curve: [f64; 4], seconds: f64, ms: u64) -> f32 {
    motion::ease(curve, seconds * 1000.0 / ms as f64) as f32
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
            start: Instant::now(), from: Frame::IDENTITY, family: Family::Bloom, settle: 0.0, spatial: false,
            showing: true, pending: false, running: false, rows: 0, row_time: 0.0,
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
            let focus = eased(motion::EASE_FOCUS, t, motion::FOCUS_MS);
            let p = if t >= self.settle || !self.spatial { 1.0 } else { self.family.spring().at(t) as f32 };
            self.from.mix(Frame::IDENTITY, p, focus)
        } else {
            let p = eased(motion::EASE_EXIT, t, motion::EXIT_MS);
            self.from.mix(self.family.pose(self.spatial, false), p, p)
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
        let t = self.rows_elapsed(now) - if self.spatial { motion::row_delay(index) } else { 0.0 };
        let focus = eased(motion::EASE_FOCUS, t, motion::FOCUS_MS);
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
                let rows = if self.spatial { motion::row_delay(self.rows - 1) + row_settle().max(motion::FOCUS_MS as f64 / 1000.0) } else { motion::FOCUS_MS as f64 / 1000.0 };
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

    pub fn enter(&mut self, family: Family, spatial: bool, visible: bool, now: Instant) {
        if visible && self.showing {
            return;
        }
        let from = if visible { self.frame(now) } else { family.pose(spatial, true) };
        self.row_time = if visible { self.rows_elapsed(now) } else { 0.0 };
        self.start = now;
        self.from = from;
        self.family = family;
        self.settle = family.spring().settle();
        self.spatial = spatial;
        self.showing = true;
        self.pending = !visible;
        self.running = true;
    }

    pub fn exit(&mut self, now: Instant) {
        if !self.showing {
            return;
        }
        self.from = self.frame(now);
        self.row_time = self.rows_elapsed(now);
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
        let e = motion::ease(tokens::motion::EASE_OUT, p) as f32;
        ((self.from.0 + (self.to.0 - self.from.0) * e, self.from.1 + (self.to.1 - self.from.1) * e), p < 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Family; 4] = [Family::Drop, Family::Clipboard, Family::Card, Family::Bloom];

    #[test]
    fn reversal_keeps_the_frame_rows_and_final_visibility() {
        let start = Instant::now();
        for family in ALL {
            let mut p = Progress::default();
            p.set_rows(8);
            p.enter(family, true, false, start);
            let first = start + Duration::from_secs(1);
            assert_eq!(p.frame(first), p.draw_frame(first));
            let exiting = first + duration(motion::FOCUS_MS) / 2;
            let frame = p.frame(exiting);
            let row = p.row(2, exiting);
            p.exit(exiting);
            assert_eq!(p.frame(exiting), frame);
            assert_eq!(p.row(2, exiting), row);
            let reopening = exiting + duration(motion::EXIT_MS) / 2;
            let frame = p.frame(reopening);
            let row = p.row(2, reopening);
            p.enter(family, true, true, reopening);
            assert_eq!(p.frame(reopening), frame);
            assert_eq!(p.row(2, reopening), row);
            let end = reopening + Duration::from_secs_f64(slow() as f64 * 2.0);
            assert!(p.showing && !p.active(end));
            assert_eq!(p.frame(end), Frame::IDENTITY);
            p.exit(end);
            p.exit(end + duration(motion::EXIT_MS) / 2);
            let hidden = end + duration(motion::EXIT_MS);
            assert!(!p.showing && !p.active(hidden));
            assert_eq!(p.frame(hidden).opacity, 0.0);
        }
    }

    #[test]
    fn panels_land_with_a_settle_and_rows_wait_their_turn() {
        let start = Instant::now();
        let mut p = Progress::default();
        p.set_rows(8);
        // The panels fall past their place a little; blooms grow a little past full size.
        for family in [Family::Drop, Family::Clipboard, Family::Bloom] {
            p.enter(family, true, false, start);
            p.draw_frame(start);
            assert!((0..1000).any(|ms| {
                let f = p.frame(start + duration(ms));
                f.scale > 1.0 || f.y > 0.0
            }));
        }
        p.enter(Family::Card, true, false, start);
        p.draw_frame(start);
        let now = start + Duration::from_secs_f64(motion::row_delay(1) * slow() as f64);
        assert!(p.row(0, now).opacity > 0.0);
        assert_eq!(p.row(2, now).opacity, 0.0);
        assert!(p.active(start + duration(motion::FOCUS_MS)));
    }

    #[test]
    fn reduced_motion_only_fades_even_on_reversal() {
        let start = Instant::now();
        for family in ALL {
            let mut p = Progress::default();
            p.enter(family, false, false, start);
            p.draw_frame(start);
            let halfway = start + duration(motion::FOCUS_MS) / 2;
            let frame = p.frame(halfway);
            assert!(frame.opacity > 0.0 && frame.opacity < 1.0);
            assert_eq!(Frame { opacity: 1.0, ..frame }, Frame::IDENTITY);
            p.exit(halfway);
            let frame = p.frame(halfway + duration(motion::EXIT_MS) / 2);
            assert_eq!(Frame { opacity: 1.0, ..frame }, Frame::IDENTITY);
            assert_eq!(p.row(0, start).y, 0.0);
        }
    }
}
