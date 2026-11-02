//! Panel poses, reversible fades and the Windows motion preference.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use livae_core::tokens::motion;
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
pub fn ease(p: f32) -> f32 {
    let [x1, y1, x2, y2] = motion::EASE_OUT;
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

#[derive(Clone, Copy)]
pub enum Pose {
    Drop,
    Clipboard,
    Pop,
    Rise,
    Shrink,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub opacity: f32,
    pub scale: f32,
    pub x: f32,
    pub y: f32,
}

impl Frame {
    pub const IDENTITY: Self = Self { opacity: 1.0, scale: 1.0, x: 0.0, y: 0.0 };

    pub fn pose(pose: Pose, anchor: (f32, f32), spatial: bool) -> Self {
        let (scale, shift) = if spatial {
            match pose {
                Pose::Drop => (0.96, -6.0),
                Pose::Clipboard => (0.94, 0.0),
                Pose::Pop => (0.9, 0.0),
                Pose::Rise => (0.98, 10.0),
                Pose::Shrink => (0.98, 0.0),
            }
        } else {
            (1.0, 0.0)
        };
        Self { opacity: 0.0, scale, x: anchor.0 * (1.0 - scale), y: anchor.1 * (1.0 - scale) + shift }
    }

    fn mix(self, to: Self, p: f32) -> Self {
        let lerp = |a, b| a + (b - a) * p;
        Self { opacity: lerp(self.opacity, to.opacity), scale: lerp(self.scale, to.scale), x: lerp(self.x, to.x), y: lerp(self.y, to.y) }
    }

    /// Convert a shared panel transform to a child window's local coordinates.
    pub fn local(self, origin: (f32, f32)) -> Self {
        Self { x: self.x + (self.scale - 1.0) * origin.0, y: self.y + (self.scale - 1.0) * origin.1, ..self }
    }
}

#[derive(Clone, Copy)]
pub struct Progress {
    start: Instant,
    duration: Duration,
    from: Frame,
    to: Frame,
    pub showing: bool,
    pending: bool,
}

impl Default for Progress {
    fn default() -> Self {
        Self { start: Instant::now(), duration: Duration::ZERO, from: Frame::IDENTITY, to: Frame::IDENTITY, showing: true, pending: false }
    }
}

impl Progress {
    pub fn frame(&self, now: Instant) -> Frame {
        if self.pending {
            return self.from;
        }
        let p = if self.duration.is_zero() { 1.0 } else { now.saturating_duration_since(self.start).as_secs_f64() / self.duration.as_secs_f64() };
        self.from.mix(self.to, ease(p as f32))
    }

    pub fn active(&self, now: Instant) -> bool {
        self.pending || now.saturating_duration_since(self.start) < self.duration
    }

    /// A slow first paint must still present the initial pose under the cloak.
    pub fn draw_frame(&mut self, now: Instant) -> Frame {
        if self.pending {
            self.start = now;
            self.pending = false;
        }
        self.frame(now)
    }

    pub fn enter(&mut self, pose: Frame, ms: u64, visible: bool, now: Instant) {
        if visible && self.showing {
            return;
        }
        let from = if visible { self.frame(now) } else { pose };
        self.begin(from, Frame::IDENTITY, ms, true, now);
        self.pending = !visible;
    }

    pub fn exit(&mut self, pose: Frame, now: Instant) {
        if !self.showing {
            return;
        }
        self.begin(self.frame(now), pose, motion::EXIT, false, now);
    }

    fn begin(&mut self, from: Frame, to: Frame, ms: u64, showing: bool, now: Instant) {
        let duration = duration(ms).mul_f64((to.opacity - from.opacity).abs() as f64);
        *self = Self { start: now, duration, from, to, showing, pending: false };
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
        let p = now.saturating_duration_since(start).as_secs_f64() / duration(motion::PRESS).as_secs_f64();
        let e = ease(p as f32);
        ((self.from.0 + (self.to.0 - self.from.0) * e, self.from.1 + (self.to.1 - self.from.1) * e), p < 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_uses_the_time_coordinate() {
        // At the curve's parameter 0.5, x is 0.33125 and y is 0.875.
        assert!((ease(0.33125) - 0.875).abs() < 0.00001);
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
    }

    #[test]
    fn reversals_keep_the_visible_frame_and_end_in_the_requested_state() {
        let start = Instant::now();
        let pose = Frame::pose(Pose::Clipboard, (320.0, 400.0), true);
        let exit = Frame::pose(Pose::Shrink, (320.0, 200.0), true);
        let mut progress = Progress::default();
        progress.enter(pose, motion::PANEL, false, start);
        let first_paint = start + Duration::from_secs(1);
        assert_eq!(progress.frame(first_paint), pose);
        assert_eq!(progress.draw_frame(first_paint), pose);
        let start = first_paint;
        let hide_at = start + duration(motion::PANEL) / 3;
        let entering = progress.frame(hide_at);
        progress.exit(exit, hide_at);
        assert_eq!(progress.frame(hide_at), entering);
        assert!(!progress.showing);
        let show_at = hide_at + progress.duration / 3;
        let exiting = progress.frame(show_at);
        progress.enter(pose, motion::PANEL, true, show_at);
        assert_eq!(progress.frame(show_at), exiting);
        assert!(progress.showing);
        let end = show_at + duration(motion::PANEL);
        assert_eq!(progress.frame(end), Frame::IDENTITY);
        assert!(!progress.active(end));
        progress.exit(exit, end);
        progress.exit(exit, end + duration(motion::EXIT) / 2);
        let hidden_at = end + duration(motion::EXIT);
        assert_eq!(progress.frame(hidden_at), exit);
        assert!(!progress.active(hidden_at));
        assert!(!progress.showing);
    }

    #[test]
    fn paired_windows_share_the_anchor_and_reduced_motion_only_fades() {
        let frame = Frame::pose(Pose::Drop, (320.0, 0.0), true);
        let card = frame.local((0.0, 66.0));
        assert!((card.y + 66.0 - (frame.scale * 66.0 + frame.y)).abs() < 0.00001);
        for pose in [Pose::Drop, Pose::Clipboard, Pose::Pop, Pose::Rise, Pose::Shrink] {
            let reduced = Frame::pose(pose, (320.0, 200.0), false);
            assert_eq!(reduced, Frame { opacity: 0.0, ..Frame::IDENTITY });
        }
    }
}
